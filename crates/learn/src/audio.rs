//! Sound through the DAW's engine: a two-track project, one track for what
//! the player plays (the keyboard's live target) and one for what the app
//! plays (other hand, Listen mode), both with the chosen instrument. The
//! transport only runs for the metronome.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use engine::device::AudioOutput;
use engine::{AudioLookup, Command, Compiler, EngineHandle, LiveSender};
use instruments::builtin::sampler::{self, Decoded, SfzBank};
use instruments::{DataKind, InstrumentData, InstrumentRef};
use midi::{NoteEventKind, Pitch, Velocity};
use project::{AudioClip, NewTrack, Project, SourceId, Track, TrackId, TrackKind, TrackPosition};

use crate::song::Output;

/// The rate to ask the device for.
pub const SAMPLE_RATE: u32 = 48_000;

struct Running {
    handle: EngineHandle,
    /// `None` when a test drives the engine.
    output: Option<AudioOutput>,
    compiler: Compiler,
    project: Project,
    player: TrackId,
    app: TrackId,
}

/// An .sfz for the Sampler, loading or loaded.
enum Bank {
    None,
    Loading(PathBuf, Receiver<Result<SfzBank, String>>),
    Ready(PathBuf, Arc<SfzBank>),
    Failed(PathBuf, String),
}

pub struct Audio {
    engine: Result<Running, String>,
    instrument: String,
    volume_db: f32,
    bank: Bank,
    /// The metronome is clicking, at this tempo.
    metronome: Option<f64>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

/// Finds the Sampler's bank for the compiler; nothing else needs audio.
/// (The Wavetable can take user tables too, but this app never loads any.)
struct Lookup<'a>(Option<&'a Arc<SfzBank>>);

impl AudioLookup for Lookup<'_> {
    fn source(&self, _: SourceId) -> Option<Arc<media::AudioBuffer>> {
        None
    }

    fn rendered(&self, _: &AudioClip) -> Option<Arc<media::AudioBuffer>> {
        None
    }

    fn instrument_data(&self, track: &Track) -> Option<InstrumentData> {
        let TrackKind::Midi { instrument, .. } = &track.kind else {
            return None;
        };
        let sfz = instrument.descriptor()?.data == DataKind::Sfz;
        self.0
            .filter(|_| sfz)
            .map(|bank| bank.clone() as InstrumentData)
    }
}

impl Audio {
    /// Opens the default output device.
    pub fn start(wake: Arc<dyn Fn() + Send + Sync>) -> Audio {
        let engine = engine::device::start(SAMPLE_RATE)
            .map(|(handle, output)| Running::new(handle, Some(output)))
            .map_err(|e| e.to_string());
        Audio::with(engine, wake)
    }

    /// Plays through an engine the caller runs: for tests.
    pub fn with_engine(handle: EngineHandle) -> Audio {
        Audio::with(Ok(Running::new(handle, None)), Arc::new(|| {}))
    }

    fn with(engine: Result<Running, String>, wake: Arc<dyn Fn() + Send + Sync>) -> Audio {
        let mut audio = Audio {
            engine,
            instrument: instruments::DEFAULT_INSTRUMENT.to_owned(),
            volume_db: 0.0,
            bank: Bank::None,
            metronome: None,
            wake,
        };
        audio.rebuild();
        if let Ok(running) = &mut audio.engine {
            let target = Some(running.player);
            send(&mut running.handle, Command::SetLiveTarget(target));
        }
        audio
    }

    pub fn live_sender(&self) -> Option<LiveSender> {
        self.engine.as_ref().ok().map(|r| r.handle.live_sender())
    }

    /// What the status bar says about the device, or why there's no sound.
    pub fn status(&self) -> Result<String, String> {
        match &self.engine {
            Err(err) => Err(err.clone()),
            Ok(Running { output: None, .. }) => Ok("engine running without a device".to_owned()),
            Ok(Running {
                output: Some(output),
                ..
            }) => {
                let buffer = match output.buffer_frames {
                    Some(frames) => {
                        let ms = frames as f32 / output.sample_rate as f32 * 1000.0;
                        format!(" · {frames} frames ({ms:.1} ms)")
                    }
                    None => String::new(),
                };
                Ok(format!(
                    "{} · {} · {} Hz{buffer}",
                    output.host_name, output.device_name, output.sample_rate
                ))
            }
        }
    }

    /// How long after it's computed a sound is heard, roughly: one buffer.
    pub fn latency(&self) -> f64 {
        match &self.engine {
            Ok(Running {
                output: Some(output),
                ..
            }) => output
                .buffer_frames
                .map_or(0.01, |f| f64::from(f) / f64::from(output.sample_rate)),
            _ => 0.0,
        }
    }

    pub fn instrument(&self) -> &str {
        &self.instrument
    }

    pub fn set_instrument(&mut self, id: &str) {
        if id == self.instrument || instruments::find_builtin(id).is_none() {
            return;
        }
        self.instrument = id.to_owned();
        self.rebuild();
    }

    pub fn volume_db(&self) -> f32 {
        self.volume_db
    }

    pub fn set_volume(&mut self, db: f32) {
        self.volume_db = db.clamp(project::SILENCE_DB, project::MAX_VOLUME_DB);
        if let Ok(running) = &mut self.engine {
            running.project.master.volume_db = self.volume_db;
            send(
                &mut running.handle,
                Command::SetMasterVolume(self.volume_db),
            );
        }
    }

    /// True if the chosen instrument plays from a file (the Sampler).
    pub fn needs_file(&self) -> bool {
        instruments::find_builtin(&self.instrument).is_some_and(|d| d.data == DataKind::Sfz)
    }

    /// The Sampler's .sfz: its name, and whether it's ready.
    pub fn bank_state(&self) -> Option<(String, Result<bool, String>)> {
        let name = |p: &Path| {
            p.file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
        };
        match &self.bank {
            Bank::None => None,
            Bank::Loading(p, _) => Some((name(p), Ok(false))),
            Bank::Ready(p, _) => Some((name(p), Ok(true))),
            Bank::Failed(p, err) => Some((name(p), Err(err.clone()))),
        }
    }

    pub fn bank_path(&self) -> Option<&Path> {
        match &self.bank {
            Bank::None => None,
            Bank::Loading(p, _) | Bank::Ready(p, _) | Bank::Failed(p, _) => Some(p),
        }
    }

    /// Loads an .sfz (and its samples) on a worker thread for the Sampler.
    pub fn load_sfz(&mut self, path: PathBuf) {
        let (send, receive) = channel();
        let (file, wake) = (path.clone(), self.wake.clone());
        std::thread::Builder::new()
            .name("sfz loader".into())
            .spawn(move || {
                let decode = |file: &Path| {
                    let (audio, _) = media::decode(file).map_err(|e| e.to_string())?;
                    Ok(Decoded::stereo(
                        audio.left().to_vec(),
                        audio.right().to_vec(),
                        audio.sample_rate(),
                    ))
                };
                let _ = send.send(sampler::load(&file, &decode).map_err(|e| e.to_string()));
                wake();
            })
            .ok();
        self.bank = Bank::Loading(path, receive);
    }

    /// Call every frame: drops old snapshots and picks up a loaded bank.
    pub fn update(&mut self) {
        if let Ok(running) = &mut self.engine {
            running.handle.collect_garbage();
        }
        let loaded = match &self.bank {
            Bank::Loading(path, receive) => match receive.try_recv() {
                Ok(Ok(bank)) => Some(Bank::Ready(path.clone(), Arc::new(bank))),
                Ok(Err(err)) => Some(Bank::Failed(path.clone(), err)),
                Err(_) => None,
            },
            _ => None,
        };
        if let Some(bank) = loaded {
            self.bank = bank;
            self.rebuild();
        }
    }

    /// A note the app plays (other hand, Listen mode).
    pub fn play(&mut self, output: Output) {
        let event = match output {
            Output::On { pitch, velocity } => Pitch::new(pitch).map(|pitch| NoteEventKind::On {
                pitch,
                velocity: Velocity::from_midi1(velocity),
            }),
            Output::Off { pitch } => Pitch::new(pitch).map(|pitch| NoteEventKind::Off {
                pitch,
                velocity: Velocity::CENTER,
            }),
        };
        if let (Ok(running), Some(event)) = (&mut self.engine, event) {
            let track = running.app;
            send(&mut running.handle, Command::PreviewNote { track, event });
        }
    }

    /// Stops everything the app is playing.
    pub fn silence_app(&mut self) {
        if let Ok(running) = &mut self.engine {
            let track = running.app;
            send(
                &mut running.handle,
                Command::PreviewNote {
                    track,
                    event: NoteEventKind::AllNotesOff,
                },
            );
        }
    }

    /// Clicks at `bpm` from now on, or stops.
    pub fn set_metronome(&mut self, bpm: Option<f64>) {
        if bpm == self.metronome {
            return;
        }
        self.metronome = bpm;
        if let Some(bpm) = bpm
            && let Ok(running) = &mut self.engine
        {
            running.project.set_tempo_bpm(bpm);
        }
        self.rebuild();
        let Ok(running) = &mut self.engine else {
            return;
        };
        let handle = &mut running.handle;
        send(handle, Command::SetPlaying(false));
        if bpm.is_some() {
            send(handle, Command::Seek(0));
            send(handle, Command::SetMetronome(true));
            send(handle, Command::SetPlaying(true));
        } else {
            send(handle, Command::SetMetronome(false));
        }
    }

    pub fn metronome(&self) -> Option<f64> {
        self.metronome
    }

    /// Where the transport is, in seconds; 0 until audio has run.
    pub fn transport_seconds(&self) -> Option<f64> {
        let running = self.engine.as_ref().ok()?;
        Some(running.handle.playhead_seconds())
    }

    /// Sends the project with the current instrument and volume.
    fn rebuild(&mut self) {
        let Ok(running) = &mut self.engine else {
            return;
        };
        let reference = InstrumentRef::builtin(&self.instrument);
        let file = match &self.bank {
            Bank::Ready(path, _) | Bank::Loading(path, _) | Bank::Failed(path, _) => {
                Some(path.clone())
            }
            Bank::None => None,
        };
        for track in [running.player, running.app] {
            let _ = running.project.set_instrument(track, reference.clone());
            let _ = running.project.set_instrument_file(track, file.clone());
        }
        running.project.master.volume_db = self.volume_db;
        let bank = match &self.bank {
            Bank::Ready(_, bank) => Some(bank),
            _ => None,
        };
        let arrangement = running.compiler.compile(&running.project, Lookup(bank));
        if running
            .handle
            .send(Command::Arrangement(arrangement))
            .is_err()
        {
            // Audio has stalled: next time, send every track's processors.
            running.compiler.invalidate();
        }
    }
}

impl Running {
    fn new(handle: EngineHandle, output: Option<AudioOutput>) -> Running {
        let rate = handle.sample_rate() as u32;
        let mut project = Project::new();
        let player = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .expect("adding at the end always works");
        let app = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .expect("adding at the end always works");
        Running {
            handle,
            output,
            compiler: Compiler::new(rate),
            project,
            player,
            app,
        }
    }
}

fn send(handle: &mut EngineHandle, command: Command) {
    if let Err(command) = handle.send(command) {
        eprintln!("engine command queue full, dropped {command:?}");
    }
}
