//! Renders for the analysis tools: the project played offline, off the UI
//! thread, the way an export plays it, and kept a while for the next
//! question about the same sound.
//!
//! A request is turned into a project of its own on the UI thread (one
//! target track soloed as an export solos it; faders at unity for
//! `PreFader`; effects bypassed for `Dry`), with the audio it plays. A
//! worker compiles it (on the UI thread instead when it has plugins, whose
//! instances only the UI thread may make), renders it with
//! [`Bounce::with_stems`] and sends back the mix and the targets' stems.
//! At most [`WORKERS`] render at once; the rest wait in order. Each checks
//! its cancel flag every block, so a cancelled call stops its render at
//! once.
//!
//! Results are cached by the project's revision, targets, range and
//! variant, up to [`CACHE_BYTES`] of audio, the least recently used going
//! first. A render that finishes after the project changed is of a version
//! no longer shown: it is answered `stale` and never cached.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use ::mcp::host::{self as job, Poll, RenderRequest, RenderTarget, Ticket, Variant};
use engine::Bounce;
use hosting::Offline;
use media::AudioBuffer;
use midi::Tick;
use project::{AutomationTarget, ClipId, Project, TrackId};

use crate::export::{self, Audio, Compiled};
use crate::session::Session;

/// Renders running at once, at most.
pub const WORKERS: usize = 2;
/// The longest render, in seconds of audio.
pub const MAX_SECONDS: u64 = 10 * 60;
/// Audio the cache keeps, at most, in bytes.
pub const CACHE_BYTES: usize = 512 << 20;
/// Audio one render may hold, its stems included, at most, in bytes: a
/// song's worth of a few dozen tracks.
pub const MAX_BYTES: u64 = 2 << 30;

/// Set in every render's ticket, so they never meet imports' (see
/// [`Renders::owns`]).
const TICKET_BIT: u64 = 1 << 62;

const CANCELLED: &str = "the render was cancelled";

/// What a render is of: the project's revision, the targets, the range and
/// the variant.
type Key = (u64, Vec<RenderTarget>, Option<(Tick, Tick)>, Variant);

/// The renders asked for, running and finished.
pub struct Renders {
    /// Asks the UI for a frame (a render finished, or wants compiling).
    wake: Arc<dyn Fn() + Send + Sync>,
    next: u64,
    /// Waiting for a worker, oldest first.
    queue: VecDeque<Job>,
    running: Vec<Running>,
    /// Answers not yet collected by [`Renders::poll`].
    done: HashMap<Ticket, Result<Arc<job::Renders>, String>>,
    cache: Vec<Cached>,
    /// Counts uses, to tell the least recently used cache entry.
    clock: u64,
    /// Renders started and renders that stopped on being cancelled, ever.
    started: u64,
    stopped: u64,
}

/// A render waiting for a worker: everything it needs, made on the UI
/// thread when it was asked for.
struct Job {
    key: Key,
    /// The calls waiting for it (several asking for the same share one).
    tickets: Vec<Ticket>,
    version: Project,
    audio: Arc<Audio>,
    rate: u32,
    /// In samples.
    range: Range<u64>,
    start: Tick,
    targets: Vec<RenderTarget>,
}

struct Running {
    key: Key,
    tickets: Vec<Ticket>,
    start: Tick,
    targets: Vec<RenderTarget>,
    /// `0..=1`, as `f32` bits.
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    result: Receiver<Result<Bounced, String>>,
    /// For a project with plugins: the worker's request for its snapshot.
    compiles: Option<Compiles>,
    /// The plugins its snapshot plays, dropped on the UI thread once done.
    plugins: Option<Offline>,
}

/// The UI thread's end of a worker's request for a snapshot with plugins.
struct Compiles {
    audio: Arc<Audio>,
    rate: u32,
    requests: Receiver<Request>,
    replies: Sender<Compiled>,
}

/// A project to compile with plugins, and renders of its clips that had
/// none, made on the worker.
struct Request {
    version: Project,
    renders: HashMap<ClipId, Arc<AudioBuffer>>,
}

/// What a worker sends back: the mix, and each stem asked for.
type Bounced = (AudioBuffer, Vec<(TrackId, AudioBuffer)>);

struct Cached {
    key: Key,
    renders: Arc<job::Renders>,
    bytes: usize,
    used: u64,
}

impl Renders {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Renders {
        Renders {
            wake,
            next: 0,
            queue: VecDeque::new(),
            running: Vec::new(),
            done: HashMap::new(),
            cache: Vec::new(),
            clock: 0,
            started: 0,
            stopped: 0,
        }
    }

    /// Whether `ticket` names a render (rather than an import).
    pub fn owns(ticket: Ticket) -> bool {
        ticket.0 & TICKET_BIT != 0
    }

    /// How many renders have started, ever (a cached answer starts none).
    #[cfg(test)]
    pub fn started(&self) -> u64 {
        self.started
    }

    /// How many renders stopped because they were cancelled, ever.
    #[cfg(test)]
    pub fn stopped(&self) -> u64 {
        self.stopped
    }

    /// How many renders are running or waiting for a worker.
    #[cfg(test)]
    pub fn busy(&self) -> usize {
        self.running.len() + self.queue.len()
    }

    /// Asks for `request` to be rendered from the project as it is now:
    /// answered from the cache, joined to the same render under way, or
    /// queued. An error says why it can't be rendered (a missing track,
    /// too long, audio still loading).
    pub fn request(&mut self, session: &Session, request: RenderRequest) -> Result<Ticket, String> {
        let project = &session.project;
        let rate = session.sample_rate();
        let RenderRequest {
            targets,
            range,
            variant,
        } = request;
        if targets.is_empty() {
            return Err("nothing to render: no targets".to_owned());
        }
        let mut stems = Vec::new();
        for target in &targets {
            if let RenderTarget::Track(id) = *target {
                if project.track(id).is_none() {
                    return Err(format!(
                        "track {id} doesn't exist; get_project lists the tracks"
                    ));
                }
                if !stems.contains(&id) {
                    stems.push(id);
                }
            }
        }
        let (from, to) = range.unwrap_or((Tick::ZERO, project.end()));
        if from < Tick::ZERO || to <= from {
            return Err(if range.is_some() {
                "the range is empty: `to` must be after `from`".to_owned()
            } else {
                "the song is empty: there's nothing to render".to_owned()
            });
        }
        let sample = |tick| u64::try_from(project.tempo.tick_to_sample(tick, rate)).unwrap_or(0);
        let samples = sample(from)..sample(to);
        let frames = samples.end.saturating_sub(samples.start);
        if frames > MAX_SECONDS * u64::from(rate) {
            let seconds = frames / u64::from(rate.max(1));
            return Err(format!(
                "that's {}:{:02} of audio; a render is at most {} minutes: pass a shorter `range`",
                seconds / 60,
                seconds % 60,
                MAX_SECONDS / 60
            ));
        }
        // The mix and every stem, both channels, as f32.
        let bytes = frames * 8 * (stems.len() as u64 + 1);
        if bytes > MAX_BYTES {
            return Err(format!(
                "that's too much audio for one render ({} tracks over {} seconds); \
                 pass a shorter `range` or fewer tracks",
                stems.len(),
                frames / u64::from(rate.max(1))
            ));
        }

        self.next += 1;
        let ticket = Ticket(TICKET_BIT | self.next);
        let key: Key = (session.revision(), targets.clone(), range, variant);
        if let Some(cached) = self.cache.iter_mut().find(|c| c.key == key) {
            self.clock += 1;
            cached.used = self.clock;
            self.done.insert(ticket, Ok(cached.renders.clone()));
            return Ok(ticket);
        }
        if let Some(job) = self.queue.iter_mut().find(|j| j.key == key) {
            job.tickets.push(ticket);
            return Ok(ticket);
        }
        if let Some(running) = self.running.iter_mut().find(|r| r.key == key) {
            running.tickets.push(ticket);
            return Ok(ticket);
        }

        let audio = session
            .offline_audio()
            .ok_or("audio files are still loading; try again in a moment")?;
        self.queue.push_back(Job {
            key,
            tickets: vec![ticket],
            version: version_for(project, &targets, variant),
            audio: Arc::new(audio),
            rate,
            range: samples,
            start: from,
            targets,
        });
        self.start_queued();
        Ok(ticket)
    }

    /// Once a frame on the UI thread: compiles what workers ask for,
    /// collects finished renders (caching those of the project as it still
    /// is), forgets cached renders of older versions, and starts queued
    /// renders as workers come free.
    pub fn update(&mut self, session: &mut Session) {
        let current = session.revision();
        self.cache.retain(|c| c.key.0 == current);

        let mut finished = Vec::new();
        for (index, running) in self.running.iter_mut().enumerate() {
            if let Some(compiles) = &running.compiles {
                while let Ok(request) = compiles.requests.try_recv() {
                    running.plugins = None;
                    let plugins = session.plugins.manager.offline(&request.version);
                    let compiled = export::compile(
                        &request.version,
                        &compiles.audio,
                        &request.renders,
                        compiles.rate,
                        Some(&plugins),
                    );
                    let _ = compiles.replies.send(compiled);
                    running.plugins = Some(plugins);
                }
            }
            match running.result.try_recv() {
                Ok(result) => finished.push((index, result)),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    finished.push((index, Err("canopy hit a bug rendering this".to_owned())));
                }
            }
        }
        for (index, result) in finished.into_iter().rev() {
            let running = self.running.remove(index);
            self.finish(running, result, current);
        }
        self.start_queued();
    }

    /// Hands a finished render to the calls waiting for it.
    fn finish(&mut self, running: Running, result: Result<Bounced, String>, current: u64) {
        // Its plugins go here, on the UI thread.
        drop(running.plugins);
        let result = match result {
            Err(error) if error == CANCELLED => {
                self.stopped += 1;
                Err(error)
            }
            Err(error) => Err(error),
            Ok((mix, stems)) => {
                let mix = Arc::new(mix);
                let stems: HashMap<TrackId, Arc<AudioBuffer>> =
                    stems.into_iter().map(|(t, a)| (t, Arc::new(a))).collect();
                let audio: Option<Vec<_>> = running
                    .targets
                    .iter()
                    .map(|&target| match target {
                        RenderTarget::Master => Some((target, mix.clone())),
                        RenderTarget::Track(id) => stems.get(&id).map(|s| (target, s.clone())),
                    })
                    .collect();
                audio
                    .map(|audio| {
                        Arc::new(job::Renders {
                            version: running.key.0,
                            stale: running.key.0 != current,
                            start: running.start,
                            audio,
                        })
                    })
                    .ok_or_else(|| "a track to render wasn't in the song's mix".to_owned())
            }
        };
        if let Ok(renders) = &result
            && !renders.stale
        {
            self.keep(running.key.clone(), renders.clone());
        }
        for ticket in running.tickets {
            self.done.insert(ticket, result.clone());
        }
    }

    /// Caches `renders`, dropping the least recently used to stay within
    /// [`CACHE_BYTES`]. Renders bigger than the whole cache aren't kept.
    fn keep(&mut self, key: Key, renders: Arc<job::Renders>) {
        let mut seen = Vec::new();
        let bytes = renders
            .audio
            .iter()
            .filter(|(_, audio)| {
                // A buffer shared by two targets counts once.
                let unseen = !seen.iter().any(|a| Arc::ptr_eq(a, audio));
                seen.push((*audio).clone());
                unseen
            })
            .map(|(_, audio)| audio.frames() * 8)
            .sum();
        if bytes > CACHE_BYTES {
            return;
        }
        while self.cache.iter().map(|c| c.bytes).sum::<usize>() + bytes > CACHE_BYTES {
            let Some(oldest) = self
                .cache
                .iter()
                .enumerate()
                .min_by_key(|(_, c)| c.used)
                .map(|(i, _)| i)
            else {
                break;
            };
            self.cache.remove(oldest);
        }
        self.clock += 1;
        self.cache.push(Cached {
            key,
            renders,
            bytes,
            used: self.clock,
        });
    }

    /// Starts queued renders while workers are free.
    fn start_queued(&mut self) {
        while self.running.len() < WORKERS {
            let Some(job) = self.queue.pop_front() else {
                return;
            };
            self.started += 1;
            self.running.push(start(job, self.wake.clone()));
        }
    }

    /// How render `ticket` is doing. A finished render is answered once,
    /// marked stale if the project has changed since it was asked for
    /// (`current` is the session's revision now).
    pub fn poll(&mut self, ticket: Ticket, current: u64) -> Poll {
        if let Some(result) = self.done.remove(&ticket) {
            return Poll::Rendered(result.map(|renders| {
                if renders.version != current && !renders.stale {
                    Arc::new(job::Renders {
                        stale: true,
                        ..(*renders).clone()
                    })
                } else {
                    renders
                }
            }));
        }
        match self.progress(ticket) {
            Some(progress) => Poll::Waiting { progress },
            None => Poll::Rendered(Err("canopy has no such render".to_owned())),
        }
    }

    /// How far along render `ticket` is, `0..=1`; `None` if it isn't a
    /// render waiting to be collected.
    pub fn progress(&self, ticket: Ticket) -> Option<f32> {
        if self.done.contains_key(&ticket) {
            return Some(1.0);
        }
        if self.queue.iter().any(|j| j.tickets.contains(&ticket)) {
            return Some(0.0);
        }
        self.running
            .iter()
            .find(|r| r.tickets.contains(&ticket))
            .map(|r| f32::from_bits(r.progress.load(Ordering::Relaxed)))
    }

    /// Forgets `ticket`. A render no call waits for any more is dropped
    /// from the queue, or told to stop.
    pub fn cancel(&mut self, ticket: Ticket) {
        self.done.remove(&ticket);
        self.queue.retain_mut(|job| {
            job.tickets.retain(|&t| t != ticket);
            !job.tickets.is_empty()
        });
        for running in &mut self.running {
            running.tickets.retain(|&t| t != ticket);
            if running.tickets.is_empty() {
                running.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for Renders {
    fn drop(&mut self) {
        for running in &self.running {
            running.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// The project `targets` are rendered from: one track soloed (as an
/// export solos it), the master and several targets as the song plays;
/// `PreFader` puts the targets' faders (and their automation) at unity and
/// centre, `Dry` bypasses the targets' own effects. Playback's loop has no
/// place in a render.
fn version_for(project: &Project, targets: &[RenderTarget], variant: Variant) -> Project {
    let mut version = match targets {
        [RenderTarget::Track(track)] => export::soloed(project, *track),
        _ => project.clone(),
    };
    version.loop_region = None;
    for &target in targets {
        match (target, variant) {
            (_, Variant::Mix) => {}
            (RenderTarget::Track(id), _) => {
                let Some(track) = version.track_mut(id) else {
                    continue;
                };
                if variant == Variant::PreFader {
                    track.mixer.volume_db = 0.0;
                    track.mixer.pan = 0.0;
                    track.automation.retain(|lane| {
                        !matches!(
                            lane.target,
                            AutomationTarget::Volume | AutomationTarget::Pan
                        )
                    });
                } else {
                    for device in &mut track.devices {
                        device.bypassed = true;
                    }
                }
            }
            (RenderTarget::Master, Variant::PreFader) => {
                version.master.volume_db = 0.0;
                version.master.pan = 0.0;
            }
            (RenderTarget::Master, Variant::Dry) => {
                for device in &mut version.master_devices {
                    device.bypassed = true;
                }
            }
        }
    }
    version
}

/// Starts `job` on a worker thread of its own.
fn start(job: Job, wake: Arc<dyn Fn() + Send + Sync>) -> Running {
    let progress = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, result) = mpsc::channel();
    let (compiles, asker) = if hosting::uses_plugins(&job.version) {
        let (ask, requests) = mpsc::channel();
        let (replies, answers) = mpsc::channel();
        let compiles = Compiles {
            audio: job.audio.clone(),
            rate: job.rate,
            requests,
            replies,
        };
        (Some(compiles), Some((ask, answers)))
    } else {
        (None, None)
    };
    let running = Running {
        key: job.key.clone(),
        tickets: job.tickets.clone(),
        start: job.start,
        targets: job.targets.clone(),
        progress: progress.clone(),
        cancel: cancel.clone(),
        result,
        compiles,
        plugins: None,
    };
    let spawned = thread::Builder::new().name("mcp render".into()).spawn({
        let wake = wake.clone();
        move || {
            let outcome = render(job, asker, &progress, &cancel, &*wake);
            let _ = sender.send(outcome);
            wake();
        }
    });
    if spawned.is_err() {
        // `result`'s sender is gone with the closure: the next update
        // answers the calls with an error.
        wake();
    }
    running
}

/// A worker's whole render: compile, then play it through.
fn render(
    job: Job,
    asker: Option<(Sender<Request>, Receiver<Compiled>)>,
    progress: &AtomicU32,
    cancel: &AtomicBool,
    wake: &(dyn Fn() + Send + Sync),
) -> Result<Bounced, String> {
    let Job {
        version,
        audio,
        rate,
        range,
        targets,
        ..
    } = job;
    let (arrangement, latency) = match asker {
        Some((ask, answers)) => {
            let renders = export::render_missing(&version, &audio, rate);
            ask.send(Request { version, renders })
                .map_err(|_| CANCELLED.to_owned())?;
            wake();
            answers.recv().map_err(|_| CANCELLED.to_owned())?
        }
        None => export::compile(&version, &audio, &HashMap::new(), rate, None),
    };
    let stems: Vec<TrackId> = targets
        .iter()
        .filter_map(|t| match t {
            RenderTarget::Track(id) => Some(*id),
            RenderTarget::Master => None,
        })
        .collect();
    let mut bounce = Bounce::range(arrangement, rate, range, false, latency).with_stems(&stems);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.to_owned());
        }
        if bounce.step(engine::MAX_BLOCK) {
            break;
        }
        progress.store(bounce.progress().to_bits(), Ordering::Relaxed);
    }
    progress.store(1.0f32.to_bits(), Ordering::Relaxed);
    Ok(bounce.finish_with_stems())
}

#[cfg(test)]
mod tests {
    use super::*;
    use project::{EffectRef, NewTrack, TrackPosition};

    #[test]
    fn variants_change_only_their_targets() {
        let mut project = Project::new();
        let a = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let b = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        for track in [a, b] {
            project
                .add_device(Some(track), EffectRef::builtin("canopy.eq"), None)
                .unwrap();
            let t = project.track_mut(track).unwrap();
            t.mixer.volume_db = -6.0;
            t.mixer.pan = 0.5;
        }
        project.master.volume_db = -3.0;
        let targets = [RenderTarget::Track(a), RenderTarget::Master];

        let pre = version_for(&project, &targets, Variant::PreFader);
        assert_eq!(pre.track(a).unwrap().mixer.volume_db, 0.0);
        assert_eq!(pre.track(a).unwrap().mixer.pan, 0.0);
        assert_eq!(pre.track(b).unwrap().mixer.volume_db, -6.0);
        assert_eq!(pre.master.volume_db, 0.0);

        let dry = version_for(&project, &targets, Variant::Dry);
        assert!(dry.track(a).unwrap().devices[0].bypassed);
        assert!(!dry.track(b).unwrap().devices[0].bypassed);
        assert_eq!(dry.track(a).unwrap().mixer.volume_db, -6.0);

        // One track alone is soloed, as an export solos it.
        let solo = version_for(&project, &[RenderTarget::Track(b)], Variant::Mix);
        assert!(solo.track(b).unwrap().mixer.solo);
        assert!(!solo.track(a).unwrap().mixer.solo);
        assert_eq!(solo.master.volume_db, 0.0);
    }
}
