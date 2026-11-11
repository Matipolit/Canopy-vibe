//! Keeping a real-time audio thread alive through an overload.
//!
//! Linux lets a real-time thread run only so long without sleeping
//! (`RLIMIT_RTTIME`; PipeWire and rtkit set 200 ms), and kills the whole
//! process, with no word in any log, when one runs longer. A project too
//! heavy for the machine does exactly that: every callback takes longer
//! than its period, the next is due before it ends, and the audio thread
//! never sleeps. So the callback asks an [`Overload`] how long it may
//! render: once the thread has been awake for half the limit it renders
//! nothing for a period (silence, which takes no time), sleeps until the
//! next, and carries on. An overloaded project then stutters, as it
//! already did, instead of taking the program down.
//!
//! Whether the thread slept between two callbacks is read off two clocks:
//! the time that passed, and the time the thread spent running. Where they
//! agree it never left the processor.

use std::time::{Duration, Instant};

/// The share of the kernel's limit the thread may stay awake for. The
/// kernel counts in scheduler ticks, and the thread runs a little either
/// side of the callback, so not all of it.
const SHARE: u32 = 2;

/// Time off the processor between two callbacks that counts as having
/// slept. Less is interrupts and the clocks' own disagreement.
const SLEPT: Duration = Duration::from_micros(100);

/// How long the audio thread has been awake, and how long it may be.
pub(crate) struct Overload {
    /// How long the thread may stay awake: `None` where nothing limits it
    /// (no real-time scheduling, or another system).
    limit: Option<Duration>,
    /// How long it has run since it was last seen to sleep.
    awake: Duration,
    /// The time and the thread's running time at the last callback.
    last: Option<(Instant, Duration)>,
}

impl Overload {
    /// For the calling process, as its limits stand now. Call it on the
    /// audio thread once it runs: PipeWire sets the limit when it makes
    /// the thread real-time.
    pub fn new() -> Overload {
        Overload::with_limit(kernel_limit().map(|limit| limit / SHARE))
    }

    pub fn with_limit(limit: Option<Duration>) -> Overload {
        Overload {
            limit,
            awake: Duration::ZERO,
            last: None,
        }
    }

    /// At the start of a callback: how long it may render (`None` for as
    /// long as it takes). Nothing left means render nothing this time.
    pub fn budget(&mut self) -> Option<Duration> {
        self.limit?;
        let now = (Instant::now(), thread_time());
        let since = self
            .last
            .replace(now)
            .map(|(then, ran)| (now.0 - then, now.1.saturating_sub(ran)));
        self.after(since)
    }

    /// The budget, given how long ago the last callback started and how
    /// much of that the thread ran (`None` for the first callback).
    fn after(&mut self, since: Option<(Duration, Duration)>) -> Option<Duration> {
        let limit = self.limit?;
        match since {
            Some((passed, ran)) if passed.saturating_sub(ran) < SLEPT => self.awake += passed,
            _ => self.awake = Duration::ZERO,
        }
        Some(limit.saturating_sub(self.awake))
    }
}

/// How long the calling thread has run.
fn thread_time() -> Duration {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a plain system call writing the timespec it is handed. It
    // neither blocks nor allocates.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
    Duration::new(time.tv_sec.max(0) as u64, time.tv_nsec.max(0) as u32)
}

/// How long the kernel lets a real-time thread of this process run
/// without sleeping, if it limits that.
fn kernel_limit() -> Option<Duration> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a plain system call writing the struct it is handed.
    let read = unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut limit) } == 0;
    let micros = limit.rlim_cur.min(limit.rlim_max);
    (read && micros != libc::RLIM_INFINITY).then(|| Duration::from_micros(micros))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_millis(100);
    /// 512 frames at 48 kHz.
    const PERIOD: Duration = Duration::from_micros(10_667);

    /// A callback `passed` after the last, the thread having run `ran`.
    fn callback(overload: &mut Overload, passed: Duration, ran: Duration) -> Duration {
        overload.after(Some((passed, ran))).unwrap()
    }

    #[test]
    fn a_thread_that_sleeps_between_callbacks_keeps_its_whole_budget() {
        let mut overload = Overload::with_limit(Some(LIMIT));
        assert_eq!(overload.after(None), Some(LIMIT));
        for _ in 0..1000 {
            let budget = callback(&mut overload, PERIOD, PERIOD.mul_f32(0.9));
            assert_eq!(budget, LIMIT);
        }
    }

    #[test]
    fn callbacks_back_to_back_use_the_budget_up_and_a_sleep_gives_it_back() {
        let mut overload = Overload::with_limit(Some(LIMIT));
        overload.after(None);
        // Every callback takes longer than its period: no sleep.
        let late = PERIOD.mul_f32(1.2);
        let mut callbacks = 0;
        while callback(&mut overload, late, late) > Duration::ZERO {
            callbacks += 1;
        }
        assert_eq!(callbacks, 7, "under 100 ms of 12.8 ms callbacks");
        // The one with nothing left rendered nothing, so the thread slept
        // until the next was due.
        assert_eq!(callback(&mut overload, PERIOD, Duration::from_micros(50)), LIMIT);
    }

    #[test]
    fn interrupts_are_not_taken_for_sleep() {
        let mut overload = Overload::with_limit(Some(LIMIT));
        overload.after(None);
        let ran = PERIOD - Duration::from_micros(40);
        assert_eq!(callback(&mut overload, PERIOD, ran), LIMIT - PERIOD);
    }

    #[test]
    fn nothing_is_limited_where_the_kernel_sets_no_limit() {
        let mut overload = Overload::with_limit(None);
        assert_eq!(overload.budget(), None);
        assert_eq!(overload.after(Some((PERIOD, PERIOD))), None);
    }

    #[test]
    fn the_thread_clock_counts_running_and_not_sleeping() {
        let before = thread_time();
        std::thread::sleep(Duration::from_millis(30));
        let slept = thread_time() - before;
        assert!(slept < Duration::from_millis(10), "{slept:?}");
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(30) {
            std::hint::spin_loop();
        }
        let ran = thread_time() - before - slept;
        assert!(ran > Duration::from_millis(20), "{ran:?}");
    }
}
