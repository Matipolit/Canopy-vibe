//! The Slicer's loops: each slicer track's audio file decoded on a worker
//! thread, matched to the project's tempo (time-stretched on another, in
//! Stretch mode) and cut where the track's slices say, as the
//! [`SliceData`] the engine hands its instrument.
//!
//! Files are kept by path, decoded at the session rate. A track's data is
//! rebuilt only when what it's made of changes (the audio, the slices, the
//! speed), so the engine keeps its instrument otherwise. A Stretch render
//! is asked for whenever the speed it needs changes, and the last one (or
//! the file as it is) plays until the new one is ready; renders are
//! stamped with the request they answer, so one overtaken by a newer
//! request never replaces it ([`StretchRenders`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use instruments::DataKind;
use instruments::builtin::slicer::SliceData;
use media::{AudioBuffer, ClipProcess, Peaks, Renderer, Source};
use project::slicer::Mode;
use project::{Project, Track, TrackId, TrackKind};

/// A slicer's file, loading or loaded.
pub enum FileState {
    Loading,
    Ready(Arc<Source>),
    Failed(String),
}

/// Stretch renders of one track's loop: the newest one finished, and the
/// one asked for, if it isn't that.
#[derive(Default)]
pub struct StretchRenders {
    /// The request waiting for its render: its stamp and speed.
    pending: Option<(u64, f64)>,
    /// The latest render to arrive in order: its speed, audio and peaks.
    current: Option<(f64, Arc<AudioBuffer>, Arc<Peaks>)>,
}

impl StretchRenders {
    /// Asks for the loop at `speed`. Returns the stamp a render must be
    /// requested with (`next` is what the caller stamps requests with), or
    /// `None` if it's here or on its way already.
    pub fn want(&mut self, speed: f64, next: &mut u64) -> Option<u64> {
        if self.current.as_ref().is_some_and(|(s, ..)| *s == speed) {
            self.pending = None;
            return None;
        }
        if self.pending.is_some_and(|(_, s)| s == speed) {
            return None;
        }
        *next += 1;
        self.pending = Some((*next, speed));
        Some(*next)
    }

    /// A render arrived, stamped `stamp`: it's taken only if it answers the
    /// request waiting now, so an older one finishing late can't replace a
    /// newer one. True if it was taken.
    pub fn arrive(&mut self, stamp: u64, audio: Arc<AudioBuffer>) -> bool {
        let Some((waiting, speed)) = self.pending else {
            return false;
        };
        if waiting != stamp {
            return false;
        }
        self.pending = None;
        let peaks = Arc::new(Peaks::new(&audio));
        self.current = Some((speed, audio, peaks));
        true
    }

    /// The newest render: its speed, audio and peaks.
    pub fn current(&self) -> Option<(f64, &Arc<AudioBuffer>, &Arc<Peaks>)> {
        self.current.as_ref().map(|(s, a, p)| (*s, a, p))
    }

    pub fn is_rendering(&self) -> bool {
        self.pending.is_some()
    }
}

/// What a track's data is made of: when it changes, the data is rebuilt.
#[derive(Debug, Clone, PartialEq)]
struct Made {
    audio: usize,
    starts: Vec<usize>,
    speed: u64,
}

/// Left and right, as [`SliceData`] holds them.
type Channels = (Arc<[f32]>, Arc<[f32]>);

/// One slicer track.
struct Entry {
    path: PathBuf,
    stretch: StretchRenders,
    /// The audio the data plays, as given to it (kept so its address names
    /// it), with the copies the instrument reads.
    audio: Option<(Arc<AudioBuffer>, Channels)>,
    data: Option<(Made, Arc<SliceData>)>,
    /// The playing Stretch render's peaks, for drawing.
    peaks: Option<Arc<Peaks>>,
}

impl Entry {
    fn new(path: &Path) -> Entry {
        Entry {
            path: path.to_owned(),
            stretch: StretchRenders::default(),
            audio: None,
            data: None,
            peaks: None,
        }
    }
}

/// A load of a file asked for by the user, to apply once it's decoded.
#[derive(Debug, Clone)]
pub struct Load {
    pub path: PathBuf,
    /// The track to load it on; a new one if `None`.
    pub onto: Option<TrackId>,
    pub position: project::TrackPosition,
}

/// What the panel shows of a track's loop.
pub struct View {
    /// The file as it is.
    pub source: Arc<Source>,
    /// The Stretch render's peaks, when that's what plays.
    rendered: Option<Arc<Peaks>>,
    /// How fast the loop plays to match the tempo, as the track is set now.
    pub speed: f64,
    /// A Stretch render for a new speed is on its way.
    pub rendering: bool,
}

impl View {
    /// The matched audio's peaks, for drawing: across its whole length,
    /// they line up with the file's.
    pub fn peaks(&self) -> &Peaks {
        self.rendered.as_deref().unwrap_or(&self.source.peaks)
    }
}

pub struct Slicers {
    files: HashMap<PathBuf, FileState>,
    tracks: HashMap<TrackId, Entry>,
    loads: Vec<Load>,
    /// The rate files were decoded at.
    rate: u32,
    renderer: Renderer,
    /// Stamps for Stretch requests; never reused.
    next_stamp: u64,
    /// Bumped when the project is replaced, so renders asked for by an old
    /// project's tracks are dropped when they arrive.
    epoch: u32,
    done: Receiver<(PathBuf, u32, Result<Source, String>)>,
    send: Sender<(PathBuf, u32, Result<Source, String>)>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

/// Whether `track` plays the Slicer.
pub fn is_slicer(track: &Track) -> bool {
    matches!(&track.kind, TrackKind::Midi { instrument, .. }
        if instrument.descriptor().is_some_and(|d| d.data == DataKind::Slices))
}

impl Slicers {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Slicers {
        let (send, done) = channel();
        let wake_renders = wake.clone();
        Slicers {
            files: HashMap::new(),
            tracks: HashMap::new(),
            loads: Vec::new(),
            rate: 0,
            renderer: Renderer::new(move || wake_renders()),
            next_stamp: 0,
            epoch: 0,
            done,
            send,
            wake,
        }
    }

    /// Forgets the tracks, for a new project. Files stay: they don't
    /// depend on the project.
    pub fn clear(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.tracks.clear();
        self.loads.clear();
    }

    fn slot(&self, track: TrackId) -> u64 {
        (u64::from(self.epoch) << 32) | u64::from(track.0)
    }

    /// Starts decoding `path` unless it's loaded or loading already.
    fn request(&mut self, path: &Path) {
        if self.files.contains_key(path) {
            return;
        }
        self.files.insert(path.to_owned(), FileState::Loading);
        let (path, rate) = (path.to_owned(), self.rate);
        let (send, wake) = (self.send.clone(), self.wake.clone());
        std::thread::Builder::new()
            .name("slicer loader".into())
            .spawn(move || {
                let result = media::load(&path, rate).map_err(|e| e.to_string());
                let _ = send.send((path, rate, result));
                wake();
            })
            .ok();
    }

    /// Loads `load.path` (from the next [`Self::update`]) to apply as the
    /// user asked once it's decoded (see [`Self::take_loaded`]).
    pub fn load(&mut self, load: Load) {
        if let Some(FileState::Failed(_)) = self.files.get(&load.path) {
            // Asked again: maybe it's there now.
            self.files.remove(&load.path);
        }
        self.loads.push(load);
    }

    /// Loads asked for with [`Self::load`] whose files have been decoded,
    /// or failed to be.
    pub fn take_loaded(&mut self) -> Vec<(Load, Result<Arc<Source>, String>)> {
        let mut finished = Vec::new();
        let files = &self.files;
        self.loads.retain(|load| match files.get(&load.path) {
            Some(FileState::Ready(source)) => {
                finished.push((load.clone(), Ok(source.clone())));
                false
            }
            Some(FileState::Failed(err)) => {
                finished.push((load.clone(), Err(err.clone())));
                false
            }
            _ => true,
        });
        finished
    }

    /// Reads a file again, picking up changes to it.
    pub fn reload(&mut self, path: &Path) {
        self.files.remove(path);
        self.tracks.retain(|_, entry| entry.path != path);
    }

    pub fn file(&self, path: &Path) -> Option<&FileState> {
        self.files.get(path)
    }

    /// Takes in decoded files and finished renders, then brings every
    /// slicer track's data in step with the project at `rate`: loading its
    /// file, asking for its Stretch render, rebuilding its data. True if
    /// any track's data changed.
    pub fn update(&mut self, project: &Project, rate: u32) -> bool {
        if rate != self.rate {
            // Decoded for a rate the engine no longer runs at.
            self.rate = rate;
            self.files.clear();
            self.tracks.clear();
        }
        while let Ok((path, at, result)) = self.done.try_recv() {
            if at == self.rate
                && let Some(state) = self.files.get_mut(&path)
                && matches!(state, FileState::Loading)
            {
                *state = match result {
                    Ok(source) => FileState::Ready(Arc::new(source)),
                    Err(err) => FileState::Failed(err),
                };
            }
        }
        while let Some(rendered) = self.renderer.try_recv() {
            let track = TrackId(rendered.slot as u32);
            if rendered.slot != self.slot(track) {
                continue;
            }
            if let Some(entry) = self.tracks.get_mut(&track) {
                entry.stretch.arrive(rendered.tag, rendered.audio);
            }
        }

        let mut changed = false;
        let mut seen = Vec::new();
        let mut wanted_files: Vec<PathBuf> = self.loads.iter().map(|l| l.path.clone()).collect();
        project.visit(|track, _| {
            if !is_slicer(track) {
                return;
            }
            let Some(path) = track.instrument_file() else {
                return;
            };
            seen.push(track.id);
            wanted_files.push(path.to_owned());
            self.request(path);
            changed |= self.update_track(project, track, path);
        });
        for path in &wanted_files {
            self.request(path);
        }
        let before = self.tracks.len();
        self.tracks.retain(|id, _| seen.contains(id));
        changed |= self.tracks.len() != before;
        self.files.retain(|path, _| wanted_files.contains(path));
        changed
    }

    fn update_track(&mut self, project: &Project, track: &Track, path: &Path) -> bool {
        let slot = self.slot(track.id);
        let entry = self
            .tracks
            .entry(track.id)
            .or_insert_with(|| Entry::new(path));
        if entry.path != path {
            *entry = Entry::new(path);
        }
        let Some(FileState::Ready(source)) = self.files.get(path) else {
            return entry.data.take().is_some();
        };
        let mode = Mode::from_index(track.instrument_param("mode").unwrap_or(0.0));
        let speed = project::slicer::speed(track, &project.tempo);
        // The audio to play: in Stretch mode the render at this speed, or
        // the newest one while it's on its way, or else the file.
        let (audio, stretched, peaks) = if mode == Mode::Stretch && speed != 1.0 {
            if let Some(stamp) = entry.stretch.want(speed, &mut self.next_stamp) {
                let frames = source.audio.frames() as u64;
                let spec = ClipProcess {
                    offset: 0,
                    len: frames,
                    reversed: false,
                    looped: false,
                    phase: 0,
                    play_len: frames,
                    mode: media::StretchMode::Stretch,
                    speed,
                    transpose: 0.0,
                    out_len: (frames as f64 / speed).ceil() as u64,
                };
                self.renderer
                    .request(slot, stamp, source.audio.clone(), spec);
            }
            match entry.stretch.current() {
                Some((at, audio, peaks)) => (audio.clone(), at, Some(peaks.clone())),
                None => (source.audio.clone(), 1.0, None),
            }
        } else {
            (source.audio.clone(), 1.0, None)
        };
        let play_speed = if mode == Mode::Resample { speed } else { 1.0 };
        // Slices in the matched audio's frames: the file's frames at the
        // session rate, shortened by the stretch.
        let ratio = f64::from(self.rate) / f64::from(source.native_rate.max(1)) / stretched;
        let frames = audio.frames();
        let starts: Vec<usize> = match track.slices() {
            [] => vec![0],
            slices => slices
                .iter()
                .map(|&s| ((s as f64 * ratio).round() as usize).min(frames))
                .collect(),
        };
        let made = Made {
            audio: Arc::as_ptr(&audio) as usize,
            starts,
            speed: play_speed.to_bits(),
        };
        entry.peaks = peaks;
        if entry.data.as_ref().is_some_and(|(m, _)| *m == made) {
            return false;
        }
        let copies = match &entry.audio {
            Some((kept, channels)) if Arc::ptr_eq(kept, &audio) => channels.clone(),
            _ => {
                let channels: Channels = (audio.left().into(), audio.right().into());
                entry.audio = Some((audio.clone(), channels.clone()));
                channels
            }
        };
        let data = SliceData {
            left: copies.0,
            right: copies.1,
            sample_rate: audio.sample_rate() as f32,
            starts: made.starts.clone().into(),
            speed: play_speed,
        };
        entry.data = Some((made, Arc::new(data)));
        true
    }

    /// The data the engine hands `track`'s instrument, once its file is
    /// loaded.
    pub fn data(&self, track: TrackId) -> Option<Arc<SliceData>> {
        self.tracks
            .get(&track)?
            .data
            .as_ref()
            .map(|(_, d)| d.clone())
    }

    /// What the panel shows of `track`'s loop, once its file is loaded.
    pub fn view(&self, project: &Project, track: &Track) -> Option<View> {
        let entry = self.tracks.get(&track.id)?;
        let Some(FileState::Ready(source)) = self.files.get(&entry.path) else {
            return None;
        };
        Some(View {
            source: source.clone(),
            rendered: entry.peaks.clone(),
            speed: project::slicer::speed(track, &project.tempo),
            rendering: entry.stretch.is_rendering(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use project::{NewTrack, TrackPosition};

    /// `seconds` of a sine at `hz` as a 32-bit float mono WAV at 44.1 kHz.
    fn write_sine(path: &Path, seconds: f32, hz: f32) {
        let rate = 44_100u32;
        let samples: Vec<f32> = (0..(seconds * rate as f32) as u32)
            .map(|i| (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin() * 0.5)
            .collect();
        let data = samples.len() as u32 * 4;
        let mut bytes = b"RIFF".to_vec();
        bytes.extend((36 + data).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(3u16.to_le_bytes()); // IEEE float
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * 4).to_le_bytes());
        bytes.extend(4u16.to_le_bytes());
        bytes.extend(32u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(data.to_le_bytes());
        for s in samples {
            bytes.extend(s.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// Frequency from rising zero crossings.
    fn pitch(signal: &[f32], rate: f32) -> f32 {
        let crossings: Vec<f32> = signal
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
            .map(|(i, w)| i as f32 + w[0] / (w[0] - w[1]))
            .collect();
        let span = crossings.last().unwrap() - crossings[0];
        (crossings.len() - 1) as f32 * rate / span
    }

    /// Runs `update` until `done` says the data is as expected.
    fn wait(
        slicers: &mut Slicers,
        project: &Project,
        track: TrackId,
        done: impl Fn(&SliceData) -> bool,
    ) -> Arc<SliceData> {
        let start = Instant::now();
        loop {
            slicers.update(project, 48_000);
            if let Some(data) = slicers.data(track)
                && done(&data)
            {
                return data;
            }
            assert!(start.elapsed() < Duration::from_secs(30), "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn resample_speeds_the_loop_up_and_stretch_keeps_its_pitch() {
        let dir = std::env::temp_dir().join(format!("canopy-slicers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        write_sine(&path, 2.0, 300.0);
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        project
            .set_instrument(track, instruments::InstrumentRef::builtin("canopy.slicer"))
            .unwrap();
        project
            .set_instrument_file(track, Some(path.clone()))
            .unwrap();
        project.set_slices(track, vec![0, 44_100]).unwrap();
        // The loop is at 60 BPM and the project at 120: twice as fast.
        project.set_tempo_bpm(120.0);
        project
            .set_instrument_param(track, "original_tempo", 60.0)
            .unwrap();
        let mut slicers = Slicers::new(Arc::new(|| {}));

        // Resample: the file as it is, played twice as fast; the slices at
        // the session rate.
        let data = wait(&mut slicers, &project, track, |_| true);
        assert_eq!(data.speed, 2.0);
        assert_eq!(data.frames(), 96_000);
        assert_eq!(&*data.starts, [0, 48_000]);

        // Stretch: half as long, played as it is, at the same pitch.
        project.set_instrument_param(track, "mode", 1.0).unwrap();
        let data = wait(&mut slicers, &project, track, |d| d.frames() < 90_000);
        assert_eq!(data.speed, 1.0);
        assert_eq!(data.frames(), 48_000);
        assert_eq!(&*data.starts, [0, 24_000]);
        let heard = pitch(&data.left[4_000..40_000], 48_000.0);
        assert!((heard - 300.0).abs() < 3.0, "{heard} Hz");
        // Unchanged, it's the same data: the engine keeps its instrument.
        slicers.update(&project, 48_000);
        assert!(Arc::ptr_eq(&data, &slicers.data(track).unwrap()));

        // Off plays the file as it is.
        project.set_instrument_param(track, "mode", 2.0).unwrap();
        let data = wait(&mut slicers, &project, track, |d| d.frames() == 96_000);
        assert_eq!(data.speed, 1.0);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn audio(frames: usize) -> Arc<AudioBuffer> {
        Arc::new(AudioBuffer::new(
            48_000,
            vec![0.1; frames],
            vec![0.1; frames],
        ))
    }

    #[test]
    fn a_stale_stretch_render_never_replaces_a_newer_one() {
        let mut renders = StretchRenders::default();
        let mut next = 0;
        // The tempo changes while the first render is on its way.
        let first = renders.want(1.5, &mut next).unwrap();
        let second = renders.want(1.25, &mut next).unwrap();
        assert_ne!(first, second);
        assert!(renders.is_rendering());
        // The first finishes after the second was asked for: dropped, and
        // nothing plays but the file.
        assert!(!renders.arrive(first, audio(100)));
        assert!(renders.current().is_none());
        // The second replaces it.
        assert!(renders.arrive(second, audio(200)));
        let (speed, current, _) = renders.current().unwrap();
        assert_eq!((speed, current.frames()), (1.25, 200));
        assert!(!renders.is_rendering());
        // Asked again for what it has, nothing is rendered.
        assert_eq!(renders.want(1.25, &mut next), None);
        // A new speed: the old render plays until the new one is here, and
        // a late answer to an old request still can't replace it.
        let third = renders.want(2.0, &mut next).unwrap();
        assert!(!renders.arrive(second, audio(300)));
        assert_eq!(renders.current().unwrap().1.frames(), 200);
        assert!(renders.arrive(third, audio(400)));
        assert_eq!(renders.current().unwrap().0, 2.0);
        // Back to the first speed while a render for another is waiting:
        // nothing new is asked for twice.
        let fourth = renders.want(1.5, &mut next).unwrap();
        assert_eq!(renders.want(1.5, &mut next), None);
        assert!(renders.arrive(fourth, audio(500)));
    }
}
