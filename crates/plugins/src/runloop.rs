//! Timers and file-descriptor watches plugins register with the host, run
//! from [`crate::Host::idle`] on the main thread.
//!
//! On Linux a plugin's editor has no event loop of its own: it registers
//! its X11 connection's file descriptor and a redraw timer with the host
//! (CLAP's timer-support and posix-fd-support, VST3's `IRunLoop`), and the
//! host calls it back when the descriptor is readable or the timer is due.
//! CLAP plugins also use timers without an editor.

use std::time::{Duration, Instant};

/// Timers, each calling back a handler `H` (a CLAP timer id, a VST3
/// `ITimerHandler`) every `period`.
pub(crate) struct Timers<H> {
    next_id: u32,
    timers: Vec<Timer<H>>,
}

struct Timer<H> {
    id: u32,
    period: Duration,
    due: Instant,
    handler: H,
}

impl<H: Clone> Timers<H> {
    pub fn new() -> Timers<H> {
        Timers {
            next_id: 0,
            timers: Vec::new(),
        }
    }

    /// Adds a timer and returns its id. Periods under a millisecond are
    /// taken as one: the host can't run timers faster than it idles anyway.
    pub fn add(&mut self, period_ms: u64, handler: H) -> u32 {
        self.add_with(period_ms, |_| handler)
    }

    /// Adds a timer whose handler is made from its id.
    pub fn add_with(&mut self, period_ms: u64, handler: impl FnOnce(u32) -> H) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let handler = handler(id);
        let period = Duration::from_millis(period_ms.max(1));
        self.timers.push(Timer {
            id,
            period,
            due: Instant::now() + period,
            handler,
        });
        id
    }

    /// Removes timer `id`; false if there's none.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.timers.len();
        self.timers.retain(|t| t.id != id);
        self.timers.len() != before
    }

    /// Removes every timer whose handler `is`.
    pub fn remove_where(&mut self, is: impl Fn(&H) -> bool) -> bool {
        let before = self.timers.len();
        self.timers.retain(|t| !is(&t.handler));
        self.timers.len() != before
    }

    /// The handlers of timers that are due at `now`, each once however late
    /// it is, and schedules their next calls.
    pub fn due(&mut self, now: Instant) -> Vec<H> {
        let mut due = Vec::new();
        for timer in &mut self.timers {
            if timer.due <= now {
                due.push(timer.handler.clone());
                timer.due += timer.period;
                if timer.due <= now {
                    timer.due = now + timer.period;
                }
            }
        }
        due
    }

    /// When the next timer is due.
    pub fn next_due(&self) -> Option<Instant> {
        self.timers.iter().map(|t| t.due).min()
    }
}

/// What to watch a file descriptor for, and what it's ready for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FdFlags {
    pub read: bool,
    pub write: bool,
    pub error: bool,
}

impl FdFlags {
    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn any(self) -> bool {
        self.read || self.write || self.error
    }
}

/// File descriptors to watch, each with a handler `H`.
pub(crate) struct FdWatches<H> {
    watches: Vec<(i32, FdFlags, H)>,
}

impl<H: Clone> FdWatches<H> {
    pub fn new() -> FdWatches<H> {
        FdWatches {
            watches: Vec::new(),
        }
    }

    /// Watches `fd`, or changes what it's watched for.
    pub fn set(&mut self, fd: i32, flags: FdFlags, handler: H) {
        if let Some(watch) = self.watches.iter_mut().find(|w| w.0 == fd) {
            watch.1 = flags;
            watch.2 = handler;
        } else {
            self.watches.push((fd, flags, handler));
        }
    }

    pub fn contains(&self, fd: i32) -> bool {
        self.watches.iter().any(|w| w.0 == fd)
    }

    pub fn remove(&mut self, fd: i32) -> bool {
        let before = self.watches.len();
        self.watches.retain(|w| w.0 != fd);
        self.watches.len() != before
    }

    pub fn remove_where(&mut self, is: impl Fn(&H) -> bool) -> bool {
        let before = self.watches.len();
        self.watches.retain(|w| !is(&w.2));
        self.watches.len() != before
    }

    /// Every watched descriptor, appended to `out`.
    pub fn fds(&self, out: &mut Vec<i32>) {
        out.extend(self.watches.iter().map(|w| w.0));
    }

    /// The watches whose descriptors are ready now, without waiting.
    pub fn ready(&self) -> Vec<(i32, FdFlags, H)> {
        if self.watches.is_empty() {
            return Vec::new();
        }
        poll_now(&self.watches)
    }
}

#[cfg(unix)]
fn poll_now<H: Clone>(watches: &[(i32, FdFlags, H)]) -> Vec<(i32, FdFlags, H)> {
    let mut fds: Vec<libc::pollfd> = watches
        .iter()
        .map(|(fd, flags, _)| libc::pollfd {
            fd: *fd,
            events: if flags.read { libc::POLLIN } else { 0 }
                | if flags.write { libc::POLLOUT } else { 0 },
            revents: 0,
        })
        .collect();
    // SAFETY: `fds` is a valid array of `fds.len()` pollfd structs, and a
    // zero timeout makes poll return at once.
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 0) };
    if ready <= 0 {
        return Vec::new();
    }
    fds.iter()
        .zip(watches)
        .filter_map(|(pollfd, (fd, wanted, handler))| {
            let flags = FdFlags {
                read: pollfd.revents & libc::POLLIN != 0,
                write: pollfd.revents & libc::POLLOUT != 0,
                error: pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0,
            };
            // Errors are reported to anyone watching; the rest as asked.
            let flags = FdFlags {
                read: flags.read && wanted.read,
                write: flags.write && wanted.write,
                error: flags.error,
            };
            flags.any().then(|| (*fd, flags, handler.clone()))
        })
        .collect()
}

#[cfg(not(unix))]
fn poll_now<H: Clone>(_watches: &[(i32, FdFlags, H)]) -> Vec<(i32, FdFlags, H)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timers_fire_when_due_and_reschedule() {
        let mut timers = Timers::new();
        let a = timers.add(10, 'a');
        let b = timers.add(1000, 'b');
        assert_ne!(a, b);
        let start = Instant::now();
        assert!(timers.due(start).is_empty());
        let later = start + Duration::from_millis(50);
        assert_eq!(timers.due(later), vec!['a'], "late timers fire once");
        assert!(timers.due(later).is_empty());
        assert!(timers.next_due().unwrap() > later);
        assert!(timers.remove(a));
        assert!(!timers.remove(a));
        assert!(timers.remove_where(|h| *h == 'b'));
        assert!(timers.next_due().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn readable_descriptors_are_reported() {
        let mut fds = [0; 2];
        // SAFETY: `fds` has room for the two descriptors pipe writes.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let mut watches = FdWatches::new();
        let read = FdFlags {
            read: true,
            ..FdFlags::default()
        };
        watches.set(fds[0], read, 7);
        assert!(watches.ready().is_empty());
        // SAFETY: writing one byte from a valid buffer to our own pipe.
        assert_eq!(unsafe { libc::write(fds[1], b"x".as_ptr().cast(), 1) }, 1);
        let ready = watches.ready();
        assert_eq!(ready.len(), 1);
        assert_eq!((ready[0].0, ready[0].1.read, ready[0].2), (fds[0], true, 7));
        assert!(watches.remove(fds[0]));
        // SAFETY: closing the descriptors we opened.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
