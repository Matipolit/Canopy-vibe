//! Which thread is which. Both formats let plugins call the host from any
//! thread, and some of those calls are only allowed on the main thread
//! (registering a timer) or must not block the audio thread, so the host
//! needs to tell its threads apart cheaply and without allocating.
//!
//! The main thread is the one a [`crate::Host`] was made on. A thread is
//! the audio thread while it is inside a [`crate::Processor`] call (as
//! CLAP's thread-check sees it). Thread identity uses the OS thread id,
//! since `std::thread::current()` may allocate the first time it's called
//! on a thread std didn't start (an audio callback's).

use std::cell::Cell;

/// The OS's id for the calling thread.
#[cfg(unix)]
pub(crate) fn current_thread() -> u64 {
    // SAFETY: pthread_self has no preconditions and only reads a register.
    unsafe { libc::pthread_self() as u64 }
}

#[cfg(windows)]
pub(crate) fn current_thread() -> u64 {
    // SAFETY: GetCurrentThreadId has no preconditions.
    u64::from(unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() })
}

/// A host's main thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MainThread(u64);

impl MainThread {
    pub fn current() -> MainThread {
        MainThread(current_thread())
    }

    pub fn is_current(self) -> bool {
        current_thread() == self.0
    }
}

thread_local! {
    static IN_AUDIO_CALL: Cell<bool> = const { Cell::new(false) };
}

/// Marks the calling thread as the audio thread until dropped.
pub(crate) struct AudioThreadScope {
    was: bool,
}

impl AudioThreadScope {
    pub fn enter() -> AudioThreadScope {
        AudioThreadScope {
            was: IN_AUDIO_CALL.replace(true),
        }
    }
}

impl Drop for AudioThreadScope {
    fn drop(&mut self) {
        IN_AUDIO_CALL.set(self.was);
    }
}

pub(crate) fn is_audio_thread() -> bool {
    IN_AUDIO_CALL.get()
}

/// A value only one thread may touch, inside something shared between
/// threads (what a plugin's callbacks reach). Every access checks the
/// thread, so a plugin calling a main-thread-only function from elsewhere
/// gets refused rather than racing the main thread.
pub(crate) struct MainThreadCell<T> {
    thread: MainThread,
    value: T,
}

// SAFETY: only `thread` can get a reference to `value` (see `get`), so
// sharing the cell never shares `value`. Moving the cell moves `value`,
// which is fine for `T: Send`; dropping it elsewhere drops `value` there,
// which is also what `Send` allows.
unsafe impl<T: Send> Send for MainThreadCell<T> {}
unsafe impl<T: Send> Sync for MainThreadCell<T> {}

impl<T> MainThreadCell<T> {
    pub fn new(thread: MainThread, value: T) -> MainThreadCell<T> {
        MainThreadCell { thread, value }
    }

    /// The value, if called on its thread.
    pub fn get(&self) -> Option<&T> {
        self.thread.is_current().then_some(&self.value)
    }

    /// The value, from any thread: holding the cell exclusively means no
    /// other thread can be using it, and `T: Send` lets it be used here.
    pub fn get_mut(&mut self) -> &mut T
    where
        T: Send,
    {
        &mut self.value
    }
}
