//! Rendering a block's tracks on several cores.
//!
//! A snapshot's nodes form a dependency graph: a group waits for its
//! children (it sums their output), and an effect listening to another
//! track waits for that track, where that doesn't close a loop. Everything
//! else is independent, so the audio thread and a pool of worker threads
//! take nodes from a ready queue as their dependencies finish, and the
//! block is done when the last node is. The [`Schedule`] (the dependency
//! counts and the queue) is compiled on the UI thread with the snapshot;
//! the per-block work here allocates nothing and takes no lock.
//!
//! What's shared between the threads during a block is reached through raw
//! pointers in a [`Job`]: every node index is taken by exactly one thread
//! per block (the queue hands each out once), a node reads another's output
//! only after that node has finished (its dependency count said so, with
//! acquire/release ordering through the queue), and a sidechain send has
//! two buffers so a reader that can't wait for the writer reads the other
//! one. The audio thread waits for every worker to leave the job before it
//! touches the snapshot again.
//!
//! Workers spin for the next block while a callback lasts (its blocks come
//! back to back) and park as soon as it ends. Waking a parked thread takes
//! long enough (a hundred microseconds and more on a laptop whose cores
//! have gone cold) that the first block of every callback would run
//! without them, so the pool keeps an estimate of the callback period and
//! a parked worker sleeps only until just before the next callback is due,
//! then spins to meet it; a callback that comes early wakes it as before,
//! and after a late one it parks for good until woken.
//!
//! That waiting still costs each worker a tenth of a core or so (measured
//! November 2026: 300 µs of spinning past every callback had made it half
//! a core at 64 frames), so the workers take part only while callbacks
//! need them: once one takes [`JOIN_LOAD`] of the period, until callbacks
//! stay under [`LEAVE_LOAD`] for [`CALM_FOR`]. A project one thread plays
//! with room to spare costs one thread. A worker that panics takes the
//! process down, as a panic in the audio callback would: the alternative
//! is an audio thread waiting forever for a node that will never finish.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

use crate::arrangement::{Arrangement, Node, Send};
use crate::audio::{Block, render_node};
use crate::shared::TrackMeters;

/// Workers beyond this gain little: a block has a few dozen nodes, and
/// every worker woken is a system call.
const MAX_WORKERS: usize = 15;

/// How long a worker spins for the next block before parking, unless the
/// callback ends first: longer than the gap between one callback's blocks
/// (the audio thread's master chain and bookkeeping), and than the jitter
/// of a callback it woke up early for.
const SPIN_BEFORE_PARKING: Duration = Duration::from_micros(300);

/// How long before the next callback is due a parked worker wakes, to be
/// spinning and warm when it comes: the timer's slack and the wake-up
/// itself. With the spinning after it, about 4% of a core per worker at
/// 256-frame callbacks while it takes part.
const PREWAKE_MARGIN: Duration = Duration::from_micros(250);

/// How long a worker woken ahead of a callback spins for it before
/// parking again: the margin, and the jitter of the callback itself.
const SPIN_AFTER_PREWAKE: Duration = Duration::from_micros(450);

/// Callbacks further apart than this (an offline bounce has none; a
/// stalled device) aren't predicted.
const LONGEST_PREDICTED_PERIOD: Duration = Duration::from_millis(100);

/// The share of the callback period a callback must take for the
/// workers to join in: below it one thread keeps up with room to spare,
/// and every worker costs a core's time waiting between callbacks.
const JOIN_LOAD: f32 = 0.4;

/// The share below which, for [`CALM_FOR`], the workers leave again.
/// Measured with them in, so well under what one thread would take alone
/// (a worker makes a callback ~1.6 times faster): no coming and going.
const LEAVE_LOAD: f32 = 0.15;
const CALM_FOR: Duration = Duration::from_secs(2);

/// How many worker threads to use besides the audio thread: one for each
/// physical core but the audio thread's, capped, unless
/// `CANOPY_AUDIO_THREADS` says otherwise (0 renders everything on the
/// audio thread).
///
/// Physical cores, not hardware threads: two threads on one core share
/// its execution units, and DSP keeps those busy, so the second gains
/// little and costs the first. On a CPU with performance and efficient
/// cores only the performance ones count: measured on an i7-1365U (two
/// performance cores, eight efficient), one worker made a block 1.8 times
/// faster, and every worker beyond that made it slower, as the efficient
/// cores took three times as long per track and the whole package clocked
/// down under the load.
pub fn default_workers() -> usize {
    if let Some(n) = std::env::var("CANOPY_AUDIO_THREADS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
    {
        return n.min(MAX_WORKERS);
    }
    physical_cores().saturating_sub(1).min(MAX_WORKERS)
}

/// Physical cores the process may run on: the performance cores on a
/// hybrid CPU. On Linux from sysfs; elsewhere a guess from the hardware
/// thread count (two threads per core when there are enough for that to
/// be likely).
fn physical_cores() -> usize {
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    #[cfg(target_os = "linux")]
    if let Some(cores) = linux_physical_cores(threads) {
        return cores;
    }
    if threads >= 4 { threads / 2 } else { threads }
}

/// Counts distinct core ids among the CPUs this process may use, limited
/// to the performance cores where the kernel tells them apart
/// (`/sys/devices/cpu_core/cpus`, on hybrid Intel CPUs).
#[cfg(target_os = "linux")]
fn linux_physical_cores(threads: usize) -> Option<usize> {
    use std::collections::HashSet;
    /// `0-3,8,10-11` as a set of CPU numbers.
    fn cpu_list(text: &str) -> HashSet<usize> {
        let mut cpus = HashSet::new();
        for part in text.trim().split(',') {
            let mut ends = part.splitn(2, '-');
            let first: usize = match ends.next().and_then(|s| s.trim().parse().ok()) {
                Some(n) => n,
                None => continue,
            };
            let last = ends
                .next()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(first);
            cpus.extend(first..=last);
        }
        cpus
    }
    let allowed = {
        // The affinity mask: what `available_parallelism` already honours.
        let mut set = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
                    .map(cpu_list)
            })
            .unwrap_or_default();
        if set.is_empty() {
            set.extend(0..threads);
        }
        set
    };
    let performance = std::fs::read_to_string("/sys/devices/cpu_core/cpus")
        .ok()
        .map(|text| cpu_list(&text));
    let mut cores = HashSet::new();
    for &cpu in &allowed {
        if performance.as_ref().is_some_and(|p| !p.contains(&cpu)) {
            continue;
        }
        let path = format!("/sys/devices/system/cpu/cpu{cpu}/topology/core_id");
        let id: usize = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
        cores.insert(id);
    }
    (!cores.is_empty()).then_some(cores.len())
}

/// An atomic on a cache line of its own, so threads working on
/// neighbouring counters, or spinning on one while another is written,
/// don't slow each other down bouncing the line between cores.
#[repr(align(64))]
#[derive(Default)]
struct Padded<T>(T);

/// Which nodes wait for which, and the queue that runs them: built with
/// the snapshot, reset at the start of every block.
pub(crate) struct Schedule {
    /// How many nodes each node waits for.
    deps: Box<[u32]>,
    /// The nodes waiting for each node.
    dependents: Box<[Box<[u32]>]>,
    /// The nodes with nothing to wait for, which start every block: the
    /// dearest first, so the block doesn't end waiting for a big node
    /// that started last.
    roots: Box<[u32]>,
    /// How long each node took last block, in nanoseconds, to order the
    /// roots by.
    cost: Box<[Padded<AtomicU32>]>,
    /// How many of its dependencies each node is still waiting for this
    /// block.
    pending: Box<[Padded<AtomicU32>]>,
    /// The ready queue: node index + 1 (0 is a slot not yet written), one
    /// slot per node since each is pushed once a block.
    ready: Box<[Padded<AtomicU32>]>,
    head: Padded<AtomicUsize>,
    tail: Padded<AtomicUsize>,
    /// Nodes finished this block.
    done: Padded<AtomicUsize>,
}

impl Schedule {
    /// A schedule for `deps.len()` nodes, where `deps[i]` are the nodes
    /// node `i` waits for. The graph must be acyclic.
    pub fn new(deps: &[Vec<u32>]) -> Schedule {
        let n = deps.len();
        let mut dependents = vec![Vec::new(); n];
        for (i, waits_for) in deps.iter().enumerate() {
            for &d in waits_for {
                dependents[d as usize].push(i as u32);
            }
        }
        Schedule {
            roots: deps
                .iter()
                .enumerate()
                .filter(|(_, d)| d.is_empty())
                .map(|(i, _)| i as u32)
                .collect(),
            deps: deps.iter().map(|d| d.len() as u32).collect(),
            dependents: dependents.into_iter().map(Vec::into_boxed_slice).collect(),
            cost: (0..n).map(|_| Padded::default()).collect(),
            pending: (0..n).map(|_| Padded::default()).collect(),
            ready: (0..n).map(|_| Padded::default()).collect(),
            head: Padded::default(),
            tail: Padded::default(),
            done: Padded::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.deps.len()
    }

    /// Starts a block: every node waits for all its dependencies again,
    /// and the ones with none are ready, dearest first.
    fn reset(&mut self) {
        for slot in &self.ready {
            slot.0.store(0, Ordering::Relaxed);
        }
        self.head.0.store(0, Ordering::Relaxed);
        self.tail.0.store(0, Ordering::Relaxed);
        self.done.0.store(0, Ordering::Relaxed);
        for (pending, deps) in self.pending.iter().zip(&self.deps) {
            pending.0.store(*deps, Ordering::Relaxed);
        }
        let cost = &self.cost;
        self.roots.sort_unstable_by_key(|&i| {
            std::cmp::Reverse(cost[i as usize].0.load(Ordering::Relaxed))
        });
        for &root in &self.roots {
            self.push(root as usize);
        }
    }

    fn push(&self, node: usize) {
        let slot = self.tail.0.fetch_add(1, Ordering::Relaxed);
        self.ready[slot].0.store(node as u32 + 1, Ordering::Release);
    }

    fn pop(&self) -> Option<usize> {
        loop {
            let head = self.head.0.load(Ordering::Acquire);
            if head >= self.tail.0.load(Ordering::Acquire) {
                return None;
            }
            let value = self.ready[head].0.load(Ordering::Acquire);
            if value == 0 {
                // Reserved, not written yet: a moment.
                std::hint::spin_loop();
                continue;
            }
            if self
                .head
                .0
                .compare_exchange_weak(head, head + 1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return Some(value as usize - 1);
            }
        }
    }

    /// Node `node` has finished, having taken `nanos`: whatever was only
    /// waiting for it is ready.
    fn complete(&self, node: usize, nanos: u32) {
        self.cost[node].0.store(nanos, Ordering::Relaxed);
        for &dependent in &self.dependents[node] {
            if self.pending[dependent as usize]
                .0
                .fetch_sub(1, Ordering::AcqRel)
                == 1
            {
                self.push(dependent as usize);
            }
        }
        self.done.0.fetch_add(1, Ordering::AcqRel);
    }

    fn is_done(&self) -> bool {
        self.done.0.load(Ordering::Acquire) >= self.len()
    }
}

/// Whether the workers take part, from how long callbacks take (see
/// [`JOIN_LOAD`]).
#[derive(Debug)]
struct TakingPart {
    engaged: bool,
    /// Callbacks in a row calm enough for the workers to leave.
    calm: u32,
}

impl TakingPart {
    fn new() -> TakingPart {
        // In from the start: a heavy project's first callbacks shouldn't
        // wait for the measure to say so.
        TakingPart {
            engaged: true,
            calm: 0,
        }
    }

    /// A callback took `load` of the `period`. True when that brings the
    /// workers in.
    fn after(&mut self, load: f32, period: Duration) -> bool {
        if !self.engaged {
            self.engaged = load > JOIN_LOAD;
            self.calm = 0;
            return self.engaged;
        }
        if load < LEAVE_LOAD {
            self.calm += 1;
            if period * self.calm >= CALM_FOR {
                self.engaged = false;
            }
        } else {
            self.calm = 0;
        }
        false
    }
}

/// One block's work, as every thread sees it. Valid from the audio
/// thread's publishing it until the block is done and every worker has
/// left; see the module docs for why the pointers are sound.
pub(crate) struct Job {
    nodes: *mut Node,
    len: usize,
    sends: *const Send,
    sends_len: usize,
    schedule: *const Schedule,
    meters: *const TrackMeters,
    block: Block,
}

// Nothing the job points at is touched by two threads at once (module docs).
unsafe impl std::marker::Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    fn empty() -> Job {
        Job {
            nodes: std::ptr::null_mut(),
            len: 0,
            sends: std::ptr::null(),
            sends_len: 0,
            schedule: std::ptr::null(),
            meters: std::ptr::null(),
            block: Block::default(),
        }
    }

    /// Takes nodes from the queue until the block is done.
    fn work(&self) {
        // SAFETY: the audio thread keeps the snapshot alive and untouched
        // while this job is open (`Pool::run`).
        let schedule = unsafe { &*self.schedule };
        let mut idle = 0u32;
        loop {
            if schedule.is_done() {
                return;
            }
            match schedule.pop() {
                Some(index) => {
                    idle = 0;
                    // SAFETY: the queue hands each index out once per
                    // block, so this is the only reference to the node.
                    let node = unsafe { &mut *self.nodes.add(index) };
                    let others = Others { job: self };
                    let started = std::time::Instant::now();
                    render_node(node, index, &others, &self.block);
                    let nanos = started.elapsed().as_nanos().min(u128::from(u32::MAX)) as u32;
                    schedule.complete(index, nanos);
                }
                None => {
                    // Nothing ready yet: wait with a lighter and lighter
                    // touch, so the threads still working aren't slowed
                    // by this one's reads of what they write.
                    idle = idle.saturating_add(1);
                    for _ in 0..idle.min(64) {
                        std::hint::spin_loop();
                    }
                }
            }
        }
    }
}

/// What a node being rendered can see of the rest of the snapshot: the
/// output of nodes it waited for, the sends and the meters.
pub(crate) struct Others<'a> {
    job: &'a Job,
}

impl Others<'_> {
    /// Node `index`'s output. Only sound for a node this one waits for,
    /// which has therefore finished.
    pub fn output(&self, index: usize) -> &Node {
        debug_assert!(index < self.job.len);
        // SAFETY: the node finished before this one was queued (its
        // dependency count reached zero through an acquire/release pair),
        // and nothing writes a finished node until the block is over.
        unsafe { &*self.job.nodes.add(index) }
    }

    pub fn sends(&self) -> Sends<'_> {
        Sends {
            // SAFETY: the sends live as long as the snapshot, which the
            // audio thread holds for the block.
            sends: unsafe { std::slice::from_raw_parts(self.job.sends, self.job.sends_len) },
            parity: self.job.block.parity,
        }
    }

    pub fn meters(&self) -> &TrackMeters {
        // SAFETY: as for the sends.
        unsafe { &*self.job.meters }
    }
}

/// The sidechain sends as a block reads them: a send's buffer for this
/// block, or the one from the block before for a listener that couldn't
/// wait for its source.
#[derive(Clone, Copy)]
pub(crate) struct Sends<'a> {
    sends: &'a [Send],
    parity: usize,
}

impl<'a> Sends<'a> {
    pub fn new(sends: &'a [Send], parity: usize) -> Sends<'a> {
        Sends { sends, parity }
    }

    /// Send `index` as written this block, or last block if `late`.
    pub fn read(&self, index: usize, late: bool, frames: usize) -> Option<(&'a [f32], &'a [f32])> {
        let send = self.sends.get(index)?;
        let bus = send.read(self.parity ^ usize::from(late));
        Some((&bus.left[..frames], &bus.right[..frames]))
    }

    /// Where send `index` is written this block.
    ///
    /// Only sound for the node that owns the send (one writer per send per
    /// block), whose listeners either waited for it or read the other
    /// buffer.
    pub fn write(&self, index: usize) -> Option<&'a mut crate::arrangement::Bus> {
        let send = self.sends.get(index)?;
        // SAFETY: see above.
        Some(unsafe { &mut *send.write(self.parity) })
    }
}

/// What the audio thread and the workers share.
struct State {
    /// Bumped for every block.
    epoch: AtomicU64,
    /// Whether `job` may be worked on. Cleared once the block is done, so
    /// a worker arriving late does nothing.
    open: AtomicBool,
    /// Workers inside the job.
    active: AtomicUsize,
    job: UnsafeCell<Job>,
    quit: AtomicBool,
    parked: Box<[Padded<AtomicBool>]>,
    /// When the next callback is expected, in nanoseconds after `origin`;
    /// 0 for no idea. Workers that would park sleep until just before it.
    next_due: AtomicU64,
    origin: Instant,
    /// Set when a callback ends, cleared when the next starts: a worker
    /// waiting then parks at once instead of spinning on.
    callback_over: AtomicBool,
}

// `job` is written by the audio thread only while no worker is active and
// the job is closed (`Pool::run`).
unsafe impl Sync for State {}

impl State {
    fn nanos(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.origin).as_nanos() as u64
    }
}

/// The worker threads, and how the audio thread runs a block on them.
pub(crate) struct Pool {
    state: Arc<State>,
    workers: Vec<JoinHandle<()>>,
    threads: Box<[Thread]>,
    /// When the current callback started, and the running estimate of
    /// the time between callbacks, for waking workers ahead of the next.
    callback_started: Option<Instant>,
    period: Option<Duration>,
    /// Whether the last block had work for the workers. If not, there's
    /// no waking them ahead of the next callback.
    shared_work: bool,
    /// Whether the workers take part.
    taking_part: TakingPart,
}

impl Pool {
    /// Starts `workers` threads. Called off the audio thread.
    pub fn new(workers: usize) -> Pool {
        let state = Arc::new(State {
            epoch: AtomicU64::new(0),
            open: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            job: UnsafeCell::new(Job::empty()),
            quit: AtomicBool::new(false),
            parked: (0..workers).map(|_| Padded::default()).collect(),
            next_due: AtomicU64::new(0),
            origin: Instant::now(),
            callback_over: AtomicBool::new(false),
        });
        let handles: Vec<JoinHandle<()>> = (0..workers)
            .map(|i| {
                let state = Arc::clone(&state);
                thread::Builder::new()
                    .name(format!("canopy audio worker {i}"))
                    .spawn(move || worker(&state, i))
                    .expect("spawn an audio worker")
            })
            .collect();
        let threads = handles.iter().map(|h| h.thread().clone()).collect();
        Pool {
            state,
            workers: handles,
            threads,
            callback_started: None,
            period: None,
            shared_work: false,
            taking_part: TakingPart::new(),
        }
    }

    pub fn workers(&self) -> usize {
        self.workers.len()
    }

    /// The audio callback has ended after `took`: workers waiting for
    /// another block park now, and whether they take part in the next
    /// callbacks follows from how long this one took.
    pub fn callback_finished(&mut self, took: Duration) {
        let state = &*self.state;
        state.callback_over.store(true, Ordering::SeqCst);
        let (Some(period), Some(started)) = (self.period, self.callback_started) else {
            return;
        };
        if self.workers.is_empty() {
            return;
        }
        let load = took.as_secs_f32() / period.as_secs_f32();
        if self.taking_part.after(load, period) {
            // Parked for good while out: set them to wake ahead of the
            // next callback, as in-between ones do.
            state
                .next_due
                .store(state.nanos(started + period), Ordering::Relaxed);
            for thread in &self.threads {
                thread.unpark();
            }
        }
    }

    /// The audio callback has begun: notes when, and from the time since
    /// the last one, when the next is likely. Parked workers sleep until
    /// just before then.
    pub fn callback_started(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.callback_started.replace(now) {
            let interval = now.duration_since(last);
            if interval <= LONGEST_PREDICTED_PERIOD {
                // Mostly the last few: a device's period is steady, and
                // this follows a change within a few callbacks.
                self.period = Some(match self.period {
                    Some(period) => (period * 3 + interval) / 4,
                    None => interval,
                });
            } else {
                self.period = None;
            }
        }
        let state = &*self.state;
        state.callback_over.store(false, Ordering::SeqCst);
        let due = match self.period {
            Some(period) if self.shared_work && self.taking_part.engaged => {
                state.nanos(now + period)
            }
            _ => 0,
        };
        state.next_due.store(due, Ordering::Relaxed);
    }

    /// Renders every node of `arrangement` for `block`, on this thread and
    /// the workers, and returns once all are done and every worker has let
    /// go of the snapshot.
    pub fn run(&mut self, arrangement: &mut Arrangement, block: Block) {
        let n = arrangement.nodes.len();
        self.shared_work = n >= 2 && !self.workers.is_empty();
        if n == 0 {
            return;
        }
        let state = &*self.state;
        debug_assert!(!state.open.load(Ordering::SeqCst));
        arrangement.schedule.reset();
        // SAFETY: the job is closed, and a worker reads it only after
        // seeing it open (a worker arriving late from the last block may
        // still be counted active, but it finds the job closed and leaves).
        unsafe {
            *state.job.get() = Job {
                nodes: arrangement.nodes.as_mut_ptr(),
                len: n,
                sends: arrangement.sends.as_ptr(),
                sends_len: arrangement.sends.len(),
                schedule: &arrangement.schedule,
                meters: Arc::as_ptr(&arrangement.meters),
                block,
            };
        }
        // One node gains nothing from waking anyone.
        let use_workers = n >= 2 && !self.workers.is_empty() && self.taking_part.engaged;
        if use_workers {
            state.open.store(true, Ordering::SeqCst);
            state.epoch.fetch_add(1, Ordering::SeqCst);
            for (i, thread) in self.threads.iter().enumerate() {
                if state.parked[i].0.load(Ordering::SeqCst) {
                    thread.unpark();
                }
            }
        }
        // SAFETY: the job was just written for this snapshot.
        let job = unsafe { &*state.job.get() };
        job.work();
        if use_workers {
            // Every node is done; now every worker must be out of the job
            // before the snapshot changes under it.
            state.open.store(false, Ordering::SeqCst);
            while state.active.load(Ordering::SeqCst) != 0 {
                std::hint::spin_loop();
            }
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.state.quit.store(true, Ordering::SeqCst);
        for thread in &self.threads {
            thread.unpark();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn worker(state: &State, me: usize) {
    raise_priority();
    // As on the audio thread: subnormal floats count as zero.
    dsp::denormals::FlushToZero::forever();
    let mut seen = state.epoch.load(Ordering::SeqCst);
    loop {
        // Wait for a block: spin, then park until the next callback is
        // nearly due (or for good, if there's no telling when that is).
        let mut spinning_since = Instant::now();
        let mut spin_for = SPIN_BEFORE_PARKING;
        // Woken ahead of a callback: the last one's being over is no
        // reason to park again.
        let mut prewoken = false;
        loop {
            if state.quit.load(Ordering::SeqCst) {
                return;
            }
            let epoch = state.epoch.load(Ordering::SeqCst);
            if epoch != seen {
                seen = epoch;
                break;
            }
            let over = !prewoken && state.callback_over.load(Ordering::SeqCst);
            if !over && spinning_since.elapsed() < spin_for {
                std::hint::spin_loop();
                continue;
            }
            state.parked[me].0.store(true, Ordering::SeqCst);
            let mut woke_early = false;
            if state.epoch.load(Ordering::SeqCst) == seen && !state.quit.load(Ordering::SeqCst) {
                // A wake that came first is kept as the park token, so no
                // block is missed.
                match sleep_until_due(state) {
                    Some(nap) => {
                        thread::park_timeout(nap);
                        woke_early = true;
                    }
                    None => thread::park(),
                }
            }
            state.parked[me].0.store(false, Ordering::SeqCst);
            prewoken = woke_early;
            spinning_since = Instant::now();
            spin_for = if woke_early {
                SPIN_AFTER_PREWAKE
            } else {
                SPIN_BEFORE_PARKING
            };
        }
        state.active.fetch_add(1, Ordering::SeqCst);
        if state.open.load(Ordering::SeqCst) {
            // SAFETY: the job is open, so the audio thread holds the
            // snapshot still until `active` drops back to zero.
            let job = unsafe { &*state.job.get() };
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // Aborts in debug builds if a node allocates, as the
                // audio callback does; a no-op in release.
                assert_no_alloc::assert_no_alloc(|| job.work());
            }));
            if outcome.is_err() {
                // The audio thread would wait forever for this node.
                std::process::abort();
            }
        }
        state.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// How long a worker about to park should sleep to wake just before the
/// next callback is due: `None` to sleep until woken, when there's no
/// prediction or it's already past (the callback is late, and will wake
/// the worker itself).
fn sleep_until_due(state: &State) -> Option<Duration> {
    let due = state.next_due.load(Ordering::Relaxed);
    if due == 0 {
        return None;
    }
    let now = state.nanos(Instant::now());
    let wake = due.checked_sub(PREWAKE_MARGIN.as_nanos() as u64)?;
    let nap = wake.checked_sub(now)?;
    (nap > 0).then_some(Duration::from_nanos(nap))
}

/// The audio thread waits for the workers, so they should be scheduled
/// like it. On Windows cpal runs the audio thread under MMCSS; the
/// workers get the highest ordinary priority. On Linux the audio thread
/// has no special priority either (see `engine`'s Cargo.toml), so the
/// workers match it as they are.
fn raise_priority() {
    #[cfg(windows)]
    // SAFETY: plain Win32 calls on the current thread.
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
        };
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 256 frames at 48 kHz.
    const PERIOD: Duration = Duration::from_micros(5333);

    /// Feeds `count` callbacks of `load`; whether the workers are in after.
    fn feed(taking_part: &mut TakingPart, load: f32, count: u32) -> bool {
        for _ in 0..count {
            taking_part.after(load, PERIOD);
        }
        taking_part.engaged
    }

    #[test]
    fn workers_leave_after_two_calm_seconds_and_come_back_for_one_heavy_callback() {
        let mut taking_part = TakingPart::new();
        assert!(taking_part.engaged, "in from the start");
        assert!(feed(&mut taking_part, 0.1, 370), "not yet two seconds");
        assert!(!feed(&mut taking_part, 0.1, 10), "two calm seconds: out");
        assert!(!feed(&mut taking_part, 0.39, 1000), "one thread keeps up");
        assert!(taking_part.after(0.5, PERIOD), "a heavy callback: back in");
        assert!(!taking_part.after(0.5, PERIOD), "already in");
    }

    #[test]
    fn a_load_between_the_thresholds_neither_brings_workers_in_nor_sends_them_out() {
        let mut taking_part = TakingPart::new();
        assert!(feed(&mut taking_part, 0.25, 10_000), "in stays in");
        // One busy callback among calm ones starts the count again.
        feed(&mut taking_part, 0.1, 300);
        feed(&mut taking_part, 0.2, 1);
        assert!(feed(&mut taking_part, 0.1, 300), "the count started over");
        assert!(!feed(&mut taking_part, 0.1, 100), "then out");
        assert!(!feed(&mut taking_part, 0.25, 10_000), "out stays out");
    }
}
