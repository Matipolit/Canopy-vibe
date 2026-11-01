//! Rendering tracks nobody is changing, so playback reads the renders
//! instead of running their instruments and effects (see `engine::cache`).
//!
//! Every so often each track's render key is worked out; a track whose key
//! has held for [`IDLE`] gets rendered on a thread of its own at idle
//! priority, one track at a time, so the renders only use time nothing
//! else wants and playback never waits for them. A finished render is
//! kept only if playing the track live costs enough to be worth the
//! memory ([`MIN_COST`]), and starts playing at the next stop, so nothing
//! changes under the listener mid-song. An edit to anything a render
//! depends on changes its key: the next snapshot fades the track back to
//! its own chain at once. Renders whose key went stale are kept while
//! there's room, so an undo finds them again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use engine::cache::{self, RenderKey};
use engine::{AudioLookup, TrackCache};
use media::AudioBuffer;
use project::{Project, Track, TrackId};

/// How long a track must stay as it is before it's rendered.
pub const IDLE: Duration = Duration::from_secs(4);

/// The least a track must cost to play live, as a share of a core, for its
/// render to be kept: cheaper tracks save less than their memory is worth.
pub const MIN_COST: f32 = 0.0025;

/// How much memory renders may hold in all.
pub const BUDGET_BYTES: usize = 1 << 30;

/// How often keys are checked when nothing has been edited (loads finish
/// and change them too).
const CHECK_EVERY: Duration = Duration::from_millis(250);

/// How long a stale render stays in the snapshot, for its track to fade
/// from it.
const FADE_KEEP: Duration = Duration::from_secs(2);

/// One render, current or stale.
struct Entry {
    track: TrackId,
    key: RenderKey,
    audio: Arc<AudioBuffer>,
    /// What playing the track live costs, as a share of a core: what
    /// rendering it cost, per second of sound.
    cost: f32,
    /// May play: set while the transport is stopped, cleared when stale.
    applied: bool,
    /// When its key stopped matching its track.
    stale_since: Option<Instant>,
    /// Stale for longer than its fade, and so out of the snapshot.
    released: bool,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.audio.frames() * 2 * std::mem::size_of::<f32>()
    }
}

/// A render in progress.
struct Job {
    track: TrackId,
    key: RenderKey,
    cancel: Arc<AtomicBool>,
    /// The render and the CPU time it took, or `None` if cancelled.
    done: Receiver<Option<(AudioBuffer, Duration)>>,
}

/// What [`AutoCache`] knows about each track's key.
struct Seen {
    hash: Option<u64>,
    since: Instant,
}

/// Which tracks have renders, which are being made, and which to make.
pub struct AutoCache {
    pub enabled: bool,
    /// How long a track must hold still: [`IDLE`].
    pub idle: Duration,
    /// The least a render must save to be kept: [`MIN_COST`].
    pub min_cost: f32,
    rate: u32,
    entries: Vec<Entry>,
    job: Option<Job>,
    seen: HashMap<TrackId, Seen>,
    /// Keys whose render cost too little to keep, so they aren't made
    /// again until the track changes.
    not_worth: HashMap<TrackId, u64>,
    /// Each track as it was when last fingerprinted, and its fingerprint:
    /// comparing a track with its copy is far cheaper than serializing it.
    fingerprints: RefCell<HashMap<TrackId, (Track, u64)>>,
    checked: Option<Instant>,
    /// When a track will have held still long enough to render, or a stale
    /// render will have faded: the app needs a frame then, even if nothing
    /// else asks for one.
    due: Option<Instant>,
    /// Asks for a repaint, from the render thread when it's done.
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl AutoCache {
    pub fn new(enabled: bool, rate: u32, wake: Arc<dyn Fn() + Send + Sync>) -> AutoCache {
        AutoCache {
            enabled,
            idle: IDLE,
            min_cost: MIN_COST,
            rate,
            entries: Vec::new(),
            job: None,
            seen: HashMap::new(),
            not_worth: HashMap::new(),
            fingerprints: RefCell::new(HashMap::new()),
            checked: None,
            due: None,
            wake,
        }
    }

    /// Drops every render and stops the one in progress: for a new sample
    /// rate or project.
    pub fn clear(&mut self, rate: u32) {
        self.rate = rate;
        self.cancel_job();
        self.entries.clear();
        self.seen.clear();
        self.not_worth.clear();
        self.fingerprints.get_mut().clear();
    }

    fn cancel_job(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn key(
        &self,
        project: &Project,
        track: TrackId,
        lookup: &dyn AudioLookup,
    ) -> Option<RenderKey> {
        let mut fingerprints = self.fingerprints.borrow_mut();
        let mut fingerprint = |track: &Track| {
            if let Some((copy, hash)) = fingerprints.get(&track.id)
                && copy == track
            {
                return *hash;
            }
            let hash = cache::track_fingerprint(track);
            fingerprints.insert(track.id, (track.clone(), hash));
            hash
        };
        cache::render_key(project, track, self.rate, lookup, &mut fingerprint).ok()
    }

    /// What the compiler plays for `track`: its render if it has a current
    /// one that may play, or a stale one to fade from. `lookup` is what the
    /// compiler would play without renders. Checks the key itself, so a
    /// snapshot compiled before [`AutoCache::update`] has seen an edit
    /// never plays a render that no longer fits.
    pub fn cache_for(
        &self,
        project: &Project,
        track: &Track,
        lookup: &dyn AudioLookup,
    ) -> Option<TrackCache> {
        let mine = || self.entries.iter().filter(|e| e.track == track.id);
        mine().next()?;
        let key = self.key(project, track.id, lookup);
        let current = mine().find(|e| Some(e.key.hash) == key.as_ref().map(|k| k.hash));
        if let Some(entry) = current.filter(|e| e.applied) {
            return Some(cache::active(entry.audio.clone()));
        }
        // One that was playing until a moment ago, to fade from: gone stale
        // lately, or not yet noticed to have.
        let fading = mine().find(|e| match e.stale_since {
            None => e.applied,
            Some(since) => since.elapsed() < FADE_KEEP,
        })?;
        Some(TrackCache {
            audio: fading.audio.clone(),
            active: false,
        })
    }

    /// Whether `track` plays from a render (or will at the next stop).
    pub fn is_cached(&self, track: TrackId) -> bool {
        self.entries
            .iter()
            .any(|e| e.track == track && e.stale_since.is_none())
    }

    /// Whether `track` is being rendered now.
    pub fn is_rendering(&self, track: TrackId) -> bool {
        self.job.as_ref().is_some_and(|j| j.track == track)
    }

    /// How much memory the renders hold.
    pub fn bytes(&self) -> usize {
        self.entries.iter().map(Entry::bytes).sum()
    }

    /// Once a frame: collects a finished render, notices edits (`changed`:
    /// the project or its audio changed since the last call), lets renders
    /// play while stopped, keeps within the budget and starts the next
    /// render. True if the snapshot must be compiled again.
    pub fn update(
        &mut self,
        project: &Project,
        lookup: &dyn AudioLookup,
        playing: bool,
        changed: bool,
    ) -> bool {
        let mut rebuild = false;
        if !self.enabled {
            self.cancel_job();
            if !self.entries.is_empty() {
                self.entries.clear();
                rebuild = true;
            }
            return rebuild;
        }
        let now = Instant::now();
        let due = changed || self.checked.is_none_or(|t| t.elapsed() >= CHECK_EVERY);
        let mut keys: HashMap<TrackId, RenderKey> = HashMap::new();
        if due {
            self.checked = Some(now);
            let ids = project.track_ids();
            for &id in &ids {
                let key = self.key(project, id, lookup);
                let hash = key.as_ref().map(|k| k.hash);
                let seen = self.seen.entry(id).or_insert(Seen { hash, since: now });
                if seen.hash != hash {
                    *seen = Seen { hash, since: now };
                }
                if let Some(key) = key {
                    keys.insert(id, key);
                }
            }
            self.seen.retain(|id, _| ids.contains(id));
            self.fingerprints.get_mut().retain(|id, _| ids.contains(id));
            for entry in &mut self.entries {
                let current = keys.get(&entry.track).map(|k| k.hash) == Some(entry.key.hash);
                match (current, entry.stale_since) {
                    (false, None) => {
                        entry.stale_since = Some(now);
                        if std::mem::take(&mut entry.applied) {
                            rebuild = true;
                        }
                    }
                    (true, Some(_)) => {
                        entry.stale_since = None;
                        entry.released = false;
                    }
                    _ => {}
                }
            }
            if let Some(job) = &self.job
                && keys.get(&job.track).map(|k| k.hash) != Some(job.key.hash)
            {
                self.cancel_job();
            }
        }

        // A finished render is kept if its track still fits it and costs
        // enough to play live.
        if let Some(job) = &self.job
            && let Ok(result) = job.done.try_recv()
        {
            let job = self.job.take().expect("checked above");
            if let Some((audio, cpu)) = result {
                let seconds = audio.duration_seconds().max(1e-3);
                let cost = (cpu.as_secs_f64() / seconds) as f32;
                let fits = self.seen.get(&job.track).and_then(|s| s.hash) == Some(job.key.hash);
                if fits && cost >= self.min_cost {
                    self.entries.push(Entry {
                        track: job.track,
                        key: job.key,
                        audio: Arc::new(audio),
                        cost,
                        applied: false,
                        stale_since: None,
                        released: false,
                    });
                } else if fits {
                    self.not_worth.insert(job.track, job.key.hash);
                }
            }
        }

        // Renders start playing only while stopped.
        if !playing {
            for entry in &mut self.entries {
                if entry.stale_since.is_none() && !entry.applied {
                    entry.applied = true;
                    rebuild = true;
                }
            }
        }

        // Stale renders past their fade go first when there's too little
        // room, the oldest first; then the current ones that save least.
        let before = self.entries.len();
        while self.bytes() > BUDGET_BYTES {
            let victim = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.stale_since.is_some_and(|t| t.elapsed() >= FADE_KEEP))
                .min_by_key(|(_, e)| e.stale_since)
                .or_else(|| {
                    self.entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| e.stale_since.is_none())
                        .min_by(|a, b| a.1.cost.total_cmp(&b.1.cost))
                })
                .map(|(i, _)| i);
            let Some(victim) = victim else { break };
            let entry = self.entries.remove(victim);
            if entry.stale_since.is_none() {
                self.not_worth.insert(entry.track, entry.key.hash);
            }
        }
        if self.entries.len() != before {
            rebuild = true;
        }
        // A stale render that has faded leaves the snapshot, so its memory
        // goes once the engine hands the snapshot back.
        for entry in &mut self.entries {
            if entry.stale_since.is_some_and(|t| t.elapsed() >= FADE_KEEP)
                && !std::mem::replace(&mut entry.released, true)
            {
                rebuild = true;
            }
        }

        if self.job.is_none() && due {
            self.start_next(project, &keys, now);
            self.due = self.next_due(project, &keys);
        }
        rebuild
    }

    /// About how much one more render of the song takes, with a couple of
    /// seconds of tail.
    fn estimate(&self, project: &Project) -> usize {
        let end = project
            .tempo
            .tick_to_sample(project.end(), self.rate)
            .max(0) as usize;
        (end + 2 * self.rate as usize) * 2 * std::mem::size_of::<f32>()
    }

    /// Whether one more render fits in the budget, once stale renders out
    /// of the snapshot make way.
    fn has_room(&self, project: &Project) -> bool {
        let kept: usize = self
            .entries
            .iter()
            .filter(|e| !e.released)
            .map(Entry::bytes)
            .sum();
        kept + self.estimate(project) <= BUDGET_BYTES
    }

    /// When something will be ready to happen: a track to render, or a
    /// stale render to let go of.
    fn next_due(&self, project: &Project, keys: &HashMap<TrackId, RenderKey>) -> Option<Instant> {
        let room = self.has_room(project);
        let renders = keys.iter().filter(|_| room).filter_map(|(id, key)| {
            let have = self
                .entries
                .iter()
                .any(|e| e.track == *id && e.key.hash == key.hash);
            let worthless = self.not_worth.get(id) == Some(&key.hash);
            (self.job.is_none() && !have && !worthless)
                .then(|| self.seen.get(id).map(|s| s.since + self.idle))
                .flatten()
        });
        let fades = self
            .entries
            .iter()
            .filter(|e| !e.released)
            .filter_map(|e| e.stale_since.map(|t| t + FADE_KEEP));
        renders.chain(fades).min()
    }

    /// How long until [`AutoCache::update`] has something to do that no
    /// event will prompt, for the app to ask for a frame then.
    pub fn next_frame(&self) -> Option<Duration> {
        let due = self.due.filter(|_| self.enabled && self.job.is_none())?;
        Some(due.saturating_duration_since(Instant::now()) + CHECK_EVERY)
    }

    /// Starts rendering the first track that has held still long enough
    /// and has no render, if the budget has room for it.
    fn start_next(&mut self, project: &Project, keys: &HashMap<TrackId, RenderKey>, now: Instant) {
        if !self.has_room(project) {
            return;
        }
        // Stale renders kept for an undo give way to new ones, the oldest
        // first.
        while self.bytes() + self.estimate(project) > BUDGET_BYTES {
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.released)
                .min_by_key(|(_, e)| e.stale_since)
                .map(|(i, _)| i);
            match oldest {
                Some(i) => drop(self.entries.remove(i)),
                None => return,
            }
        }
        let candidate = project.track_ids().into_iter().find_map(|id| {
            let key = keys.get(&id)?;
            let seen = self.seen.get(&id)?;
            let idle = now.duration_since(seen.since) >= self.idle;
            let have = self
                .entries
                .iter()
                .any(|e| e.track == id && e.key.hash == key.hash);
            let worthless = self.not_worth.get(&id) == Some(&key.hash);
            (idle && !have && !worthless).then(|| (id, key.clone()))
        });
        let Some((track, key)) = candidate else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (send, done) = mpsc::channel();
        let (project, inputs, rate) = (project.clone(), key.inputs.clone(), self.rate);
        let (stop, wake) = (cancel.clone(), self.wake.clone());
        let spawned = thread::Builder::new()
            .name("canopy-cache".into())
            .spawn(move || {
                idle_priority();
                let started = thread_cpu_time();
                let audio = cache::render(&project, track, rate, inputs, &stop, |_| {});
                let cpu = thread_cpu_time().saturating_sub(started);
                let _ = send.send(audio.map(|a| (a, cpu)));
                wake();
            });
        if spawned.is_ok() {
            self.job = Some(Job {
                track,
                key,
                cancel,
                done,
            });
        }
    }
}

impl Drop for AutoCache {
    fn drop(&mut self) {
        self.cancel_job();
    }
}

/// Makes the calling thread run only on time no other thread wants.
fn idle_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: a system call about the calling thread only.
    unsafe {
        let param = libc::sched_param { sched_priority: 0 };
        libc::sched_setscheduler(0, libc::SCHED_IDLE, &param);
    }
    #[cfg(windows)]
    // SAFETY: as above.
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_IDLE,
        };
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_IDLE);
    }
}

/// The CPU time the calling thread has used, where the system says;
/// otherwise the time since the first call, which counts waiting too.
fn thread_cpu_time() -> Duration {
    #[cfg(target_os = "linux")]
    {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: writes the calling thread's clock into `time`.
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) } == 0 {
            return Duration::new(time.tv_sec as u64, time.tv_nsec as u32);
        }
    }
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}
