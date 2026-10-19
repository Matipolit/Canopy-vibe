//! Telling the app when plugins need the main thread.
//!
//! Plugins ask for main-thread work from any thread (a CLAP plugin's
//! `request_callback`, a VST3 `restartComponent`, parameter changes coming
//! out of the audio thread), and their editors need [`crate::Host::idle`]
//! to run when their file descriptors become readable or their timers are
//! due. An app that only redraws on input wouldn't idle in time.
//!
//! [`Wake`] collects all of that. Requests made off the audio thread call
//! the app's waker at once; the audio thread only sets a flag (an atomic
//! store; calling the waker could lock). With a waker set, a watcher
//! thread waits on the registered descriptors until the next timer, checks
//! the flag at least every [`WATCH_INTERVAL`], and calls the waker when any
//! of them fires. It wakes the app once per idle, so a descriptor that
//! stays readable until the plugin handles it doesn't spin.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::thread;

/// How often the watcher looks at requests from the audio thread.
pub const WATCH_INTERVAL: Duration = Duration::from_millis(20);

type Waker = Arc<dyn Fn() + Send + Sync>;

/// What the watcher waits on, as of the last idle.
#[derive(Default)]
struct Watch {
    fds: Vec<i32>,
    deadline: Option<Instant>,
    /// How many times the host has idled.
    idles: u64,
}

#[derive(Default)]
pub(crate) struct Wake {
    pending: AtomicBool,
    waker: Mutex<Option<Waker>>,
    watch: Mutex<Watch>,
    changed: Condvar,
    stop: AtomicBool,
}

impl Wake {
    /// A plugin asked for main-thread work. Real-time safe on the audio
    /// thread.
    pub fn request(&self) {
        self.pending.store(true, Ordering::Release);
        if !thread::is_audio_thread() {
            self.call();
        }
    }

    fn call(&self) {
        let waker = self.waker.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(waker) = waker {
            waker();
        }
    }

    /// The host idled: what to watch until the next idle.
    pub fn idled(&self, fds: Vec<i32>, deadline: Option<Instant>) {
        self.pending.store(false, Ordering::Release);
        let mut watch = self.watch.lock().unwrap_or_else(|e| e.into_inner());
        watch.fds = fds;
        watch.deadline = deadline;
        watch.idles += 1;
        self.changed.notify_all();
    }

    /// Sets the waker and starts the watcher (once).
    pub fn set_waker(self: &Arc<Self>, waker: Waker) -> Option<JoinHandle<()>> {
        let first = self
            .waker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(waker)
            .is_none();
        if !first {
            return None;
        }
        let wake = self.clone();
        std::thread::Builder::new()
            .name("plugin-watch".into())
            .spawn(move || wake.watch())
            .ok()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.changed.notify_all();
    }

    fn watch(&self) {
        let mut woke_at = None;
        while !self.stop.load(Ordering::Acquire) {
            let (fds, deadline, idles) = {
                let mut watch = self.watch.lock().unwrap_or_else(|e| e.into_inner());
                if woke_at == Some(watch.idles) {
                    // Woken for this idle already: wait for the next.
                    watch = self
                        .changed
                        .wait_timeout(watch, Duration::from_millis(200))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                    if woke_at == Some(watch.idles) {
                        continue;
                    }
                }
                (watch.fds.clone(), watch.deadline, watch.idles)
            };
            let now = Instant::now();
            let timeout = deadline
                .map_or(WATCH_INTERVAL, |d| d.saturating_duration_since(now))
                .min(WATCH_INTERVAL);
            let ready = wait_readable(&fds, timeout);
            let due = deadline.is_some_and(|d| d <= Instant::now());
            if ready || due || self.pending.swap(false, Ordering::AcqRel) {
                self.call();
                woke_at = Some(idles);
            }
        }
    }
}

/// Waits up to `timeout` for any of `fds` to be readable.
#[cfg(unix)]
fn wait_readable(fds: &[i32], timeout: Duration) -> bool {
    let mut polls: Vec<libc::pollfd> = fds
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: a valid array of pollfd structs and its length.
    let ready = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, ms) };
    ready > 0
}

#[cfg(not(unix))]
fn wait_readable(_fds: &[i32], timeout: Duration) -> bool {
    std::thread::sleep(timeout);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn wakes_on_requests_and_timers_once_per_idle() {
        let wake = Arc::new(Wake::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let watcher = wake
            .set_waker(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }))
            .unwrap();
        wake.request();
        assert!(
            calls.load(Ordering::SeqCst) >= 1,
            "off the audio thread: at once"
        );

        wake.idled(Vec::new(), Some(Instant::now() + Duration::from_millis(30)));
        std::thread::sleep(Duration::from_millis(150));
        let after_timer = calls.load(Ordering::SeqCst);
        assert!(after_timer >= 2, "the due timer woke the app");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            after_timer,
            "once until the next idle"
        );

        wake.idled(Vec::new(), None);
        {
            let _audio = thread::AudioThreadScope::enter();
            wake.request();
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            calls.load(Ordering::SeqCst) > after_timer,
            "audio-thread requests are seen too"
        );
        wake.stop();
        watcher.join().unwrap();
    }
}
