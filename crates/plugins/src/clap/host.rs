//! The `clap_host` each plugin gets, and the host extensions behind it.
//!
//! A plugin may call the host from any thread. Requests that just need
//! doing later (restart, a main-thread callback, a parameter flush, "my
//! latency changed") set atomic flags that the instance picks up when the
//! host idles. Timers and file descriptors are main-thread-only by CLAP's
//! rules; they live in a [`MainThreadCell`], so a call from another thread
//! is refused instead of racing.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_void};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use clap_sys::ext::audio_ports::{CLAP_EXT_AUDIO_PORTS, clap_host_audio_ports};
use clap_sys::ext::gui::{CLAP_EXT_GUI, clap_host_gui};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_host_latency};
use clap_sys::ext::log::*;
use clap_sys::ext::note_ports::{
    CLAP_EXT_NOTE_PORTS, CLAP_NOTE_DIALECT_CLAP, CLAP_NOTE_DIALECT_MIDI, clap_host_note_ports,
    clap_note_dialect,
};
use clap_sys::ext::params::{
    CLAP_EXT_PARAMS, clap_host_params, clap_param_clear_flags, clap_param_rescan_flags,
};
use clap_sys::ext::posix_fd_support::*;
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_host_state};
use clap_sys::ext::tail::{CLAP_EXT_TAIL, clap_host_tail};
use clap_sys::ext::thread_check::{CLAP_EXT_THREAD_CHECK, clap_host_thread_check};
use clap_sys::ext::timer_support::{CLAP_EXT_TIMER_SUPPORT, clap_host_timer_support};
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::version::CLAP_VERSION;

use crate::runloop::{FdFlags, FdWatches, Timers};
use crate::thread::{self, MainThread, MainThreadCell};
use crate::wake::Wake;

/// What the plugin asked for, for the instance to act on when it idles.
pub(crate) struct Requests {
    pub restart: AtomicBool,
    pub callback: AtomicBool,
    pub flush: AtomicBool,
    pub latency_changed: AtomicBool,
    pub tail_changed: AtomicBool,
    pub dirty: AtomicBool,
    /// `CLAP_PARAM_RESCAN_*` flags, or-ed.
    pub rescan: AtomicU32,
    /// The size the plugin asked its editor window to be.
    pub resize: Mutex<Option<(u32, u32)>>,
    /// The plugin asked for its editor to be shown (true) or hidden.
    pub show: Mutex<Option<bool>>,
}

/// Main-thread-only host state: what the plugin registered.
pub(crate) struct Registrations {
    pub timers: Timers<clap_id>,
    pub fds: FdWatches<()>,
}

/// Everything behind one plugin's `clap_host`.
pub(crate) struct HostData {
    pub main: MainThread,
    /// For log messages.
    pub plugin_name: String,
    pub requests: Requests,
    /// Tells the app the host has main-thread work.
    pub wake: Arc<Wake>,
    pub registrations: MainThreadCell<RefCell<Registrations>>,
}

/// A `clap_host` and its data, at a fixed address for the plugin's life.
pub(crate) struct HostBox {
    clap: clap_host,
    data: HostData,
}

impl HostBox {
    pub fn new(main: MainThread, plugin_name: &str, wake: Arc<Wake>) -> Pin<Box<HostBox>> {
        let mut host = Box::new(HostBox {
            clap: clap_host {
                clap_version: CLAP_VERSION,
                host_data: std::ptr::null_mut(),
                name: c"canopy".as_ptr(),
                vendor: c"canopy".as_ptr(),
                url: c"".as_ptr(),
                version: c"0.1.0".as_ptr(),
                get_extension: Some(get_extension),
                request_restart: Some(request_restart),
                request_process: Some(request_process),
                request_callback: Some(request_callback),
            },
            data: HostData {
                main,
                plugin_name: plugin_name.to_owned(),
                requests: Requests {
                    restart: AtomicBool::new(false),
                    callback: AtomicBool::new(false),
                    flush: AtomicBool::new(false),
                    latency_changed: AtomicBool::new(false),
                    tail_changed: AtomicBool::new(false),
                    dirty: AtomicBool::new(false),
                    rescan: AtomicU32::new(0),
                    resize: Mutex::new(None),
                    show: Mutex::new(None),
                },
                wake,
                registrations: MainThreadCell::new(
                    main,
                    RefCell::new(Registrations {
                        timers: Timers::new(),
                        fds: FdWatches::new(),
                    }),
                ),
            },
        });
        host.clap.host_data = (&host.data as *const HostData).cast_mut().cast();
        Box::into_pin(host)
    }

    /// The pointer the plugin gets.
    pub fn as_ptr(&self) -> *const clap_host {
        &self.clap
    }

    pub fn data(&self) -> &HostData {
        &self.data
    }
}

/// The data behind a `clap_host` we made.
///
/// # Safety
///
/// `host` must be null or one of ours, alive.
unsafe fn data<'a>(host: *const clap_host) -> Option<&'a HostData> {
    // SAFETY: by the caller; `host_data` points at the HostBox's data.
    unsafe {
        host.as_ref()?
            .host_data
            .cast::<HostData>()
            .cast_const()
            .as_ref()
    }
}

static THREAD_CHECK: clap_host_thread_check = clap_host_thread_check {
    is_main_thread: Some(is_main_thread),
    is_audio_thread: Some(is_audio_thread),
};
static LOG: clap_host_log = clap_host_log { log: Some(log) };
static PARAMS: clap_host_params = clap_host_params {
    rescan: Some(params_rescan),
    clear: Some(params_clear),
    request_flush: Some(params_request_flush),
};
static STATE: clap_host_state = clap_host_state {
    mark_dirty: Some(state_mark_dirty),
};
static LATENCY: clap_host_latency = clap_host_latency {
    changed: Some(latency_changed),
};
static TAIL: clap_host_tail = clap_host_tail {
    changed: Some(tail_changed),
};
static GUI: clap_host_gui = clap_host_gui {
    resize_hints_changed: Some(gui_resize_hints_changed),
    request_resize: Some(gui_request_resize),
    request_show: Some(gui_request_show),
    request_hide: Some(gui_request_hide),
    closed: Some(gui_closed),
};
static TIMER_SUPPORT: clap_host_timer_support = clap_host_timer_support {
    register_timer: Some(register_timer),
    unregister_timer: Some(unregister_timer),
};
static POSIX_FD_SUPPORT: clap_host_posix_fd_support = clap_host_posix_fd_support {
    register_fd: Some(register_fd),
    modify_fd: Some(modify_fd),
    unregister_fd: Some(unregister_fd),
};
static AUDIO_PORTS: clap_host_audio_ports = clap_host_audio_ports {
    is_rescan_flag_supported: Some(audio_ports_is_rescan_flag_supported),
    rescan: Some(audio_ports_rescan),
};
static NOTE_PORTS: clap_host_note_ports = clap_host_note_ports {
    supported_dialects: Some(note_ports_supported_dialects),
    rescan: Some(note_ports_rescan),
};

unsafe extern "C" fn get_extension(_host: *const clap_host, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return std::ptr::null();
    }
    // SAFETY: plugins pass NUL-terminated extension ids.
    let id = unsafe { CStr::from_ptr(id) };
    let extension: *const c_void = if id == CLAP_EXT_THREAD_CHECK {
        (&THREAD_CHECK as *const clap_host_thread_check).cast()
    } else if id == CLAP_EXT_LOG {
        (&LOG as *const clap_host_log).cast()
    } else if id == CLAP_EXT_PARAMS {
        (&PARAMS as *const clap_host_params).cast()
    } else if id == CLAP_EXT_STATE {
        (&STATE as *const clap_host_state).cast()
    } else if id == CLAP_EXT_LATENCY {
        (&LATENCY as *const clap_host_latency).cast()
    } else if id == CLAP_EXT_TAIL {
        (&TAIL as *const clap_host_tail).cast()
    } else if id == CLAP_EXT_GUI {
        (&GUI as *const clap_host_gui).cast()
    } else if id == CLAP_EXT_TIMER_SUPPORT {
        (&TIMER_SUPPORT as *const clap_host_timer_support).cast()
    } else if id == CLAP_EXT_POSIX_FD_SUPPORT && cfg!(unix) {
        (&POSIX_FD_SUPPORT as *const clap_host_posix_fd_support).cast()
    } else if id == CLAP_EXT_AUDIO_PORTS {
        (&AUDIO_PORTS as *const clap_host_audio_ports).cast()
    } else if id == CLAP_EXT_NOTE_PORTS {
        (&NOTE_PORTS as *const clap_host_note_ports).cast()
    } else {
        std::ptr::null()
    };
    extension
}

/// Sets a request flag, from any thread (the audio thread included: an
/// atomic store doesn't block).
fn request(host: *const clap_host, flag: impl FnOnce(&Requests) -> &AtomicBool) {
    // SAFETY: plugins call us with the host pointer we gave them.
    if let Some(data) = unsafe { data(host) } {
        flag(&data.requests).store(true, Ordering::Release);
        data.wake.request();
    }
}

unsafe extern "C" fn request_restart(host: *const clap_host) {
    request(host, |r| &r.restart);
}

unsafe extern "C" fn request_process(_host: *const clap_host) {
    // Processors are called every block whether or not the plugin asks.
}

unsafe extern "C" fn request_callback(host: *const clap_host) {
    request(host, |r| &r.callback);
}

unsafe extern "C" fn is_main_thread(host: *const clap_host) -> bool {
    // SAFETY: as in `request`.
    unsafe { data(host) }.is_some_and(|d| d.main.is_current())
}

unsafe extern "C" fn is_audio_thread(_host: *const clap_host) -> bool {
    thread::is_audio_thread()
}

unsafe extern "C" fn log(
    host: *const clap_host,
    severity: clap_log_severity,
    message: *const c_char,
) {
    // Printing locks stderr and may allocate: never on the audio thread.
    if thread::is_audio_thread() || severity < CLAP_LOG_WARNING || message.is_null() {
        return;
    }
    // SAFETY: as in `request`; the message is NUL-terminated.
    let (data, message) = unsafe { (data(host), CStr::from_ptr(message)) };
    let name = data.map_or("?", |d| d.plugin_name.as_str());
    let kind = match severity {
        CLAP_LOG_WARNING => "warning",
        CLAP_LOG_ERROR | CLAP_LOG_FATAL => "error",
        CLAP_LOG_HOST_MISBEHAVING => "host misbehaving",
        CLAP_LOG_PLUGIN_MISBEHAVING => "plugin misbehaving",
        _ => "info",
    };
    eprintln!("[CLAP {name}] {kind}: {}", message.to_string_lossy());
}

unsafe extern "C" fn params_rescan(host: *const clap_host, flags: clap_param_rescan_flags) {
    // SAFETY: as in `request`.
    if let Some(data) = unsafe { data(host) } {
        data.requests.rescan.fetch_or(flags, Ordering::AcqRel);
        data.wake.request();
    }
}

unsafe extern "C" fn params_clear(
    _host: *const clap_host,
    _id: clap_id,
    _flags: clap_param_clear_flags,
) {
    // We keep no automation or modulation for the plugin here; the
    // project does, and drops what refers to parameters that are gone.
}

unsafe extern "C" fn params_request_flush(host: *const clap_host) {
    request(host, |r| &r.flush);
}

unsafe extern "C" fn state_mark_dirty(host: *const clap_host) {
    request(host, |r| &r.dirty);
}

unsafe extern "C" fn latency_changed(host: *const clap_host) {
    request(host, |r| &r.latency_changed);
}

unsafe extern "C" fn tail_changed(host: *const clap_host) {
    request(host, |r| &r.tail_changed);
}

unsafe extern "C" fn gui_resize_hints_changed(_host: *const clap_host) {
    // Hints are read again whenever the window is resized.
}

unsafe extern "C" fn gui_request_resize(host: *const clap_host, width: u32, height: u32) -> bool {
    // SAFETY: as in `request`.
    let Some(data) = (unsafe { data(host) }) else {
        return false;
    };
    *data
        .requests
        .resize
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some((width, height));
    data.wake.request();
    true
}

unsafe extern "C" fn gui_request_show(host: *const clap_host) -> bool {
    show(host, true)
}

unsafe extern "C" fn gui_request_hide(host: *const clap_host) -> bool {
    show(host, false)
}

fn show(host: *const clap_host, visible: bool) -> bool {
    // SAFETY: as in `request`.
    let Some(data) = (unsafe { data(host) }) else {
        return false;
    };
    *data.requests.show.lock().unwrap_or_else(|e| e.into_inner()) = Some(visible);
    data.wake.request();
    true
}

unsafe extern "C" fn gui_closed(_host: *const clap_host, _was_destroyed: bool) {
    // Only floating editors close themselves; ours are embedded in windows
    // we own.
}

/// Runs `f` on the registrations if called on the main thread.
fn registrations<R>(host: *const clap_host, f: impl FnOnce(&mut Registrations) -> R) -> Option<R> {
    // SAFETY: as in `request`.
    let data = unsafe { data(host) }?;
    let cell = data.registrations.get()?;
    let mut registrations = cell.try_borrow_mut().ok()?;
    Some(f(&mut registrations))
}

unsafe extern "C" fn register_timer(
    host: *const clap_host,
    period_ms: u32,
    timer_id: *mut clap_id,
) -> bool {
    if timer_id.is_null() {
        return false;
    }
    registrations(host, |r| r.timers.add_with(u64::from(period_ms), |id| id))
        // SAFETY: checked non-null; the plugin gave us somewhere to write.
        .map(|id| unsafe { *timer_id = id })
        .is_some()
}

unsafe extern "C" fn unregister_timer(host: *const clap_host, timer_id: clap_id) -> bool {
    registrations(host, |r| r.timers.remove(timer_id)).unwrap_or(false)
}

fn fd_flags(flags: clap_posix_fd_flags) -> FdFlags {
    FdFlags {
        read: flags & CLAP_POSIX_FD_READ != 0,
        write: flags & CLAP_POSIX_FD_WRITE != 0,
        error: flags & CLAP_POSIX_FD_ERROR != 0,
    }
}

pub(crate) fn clap_fd_flags(flags: FdFlags) -> clap_posix_fd_flags {
    (if flags.read { CLAP_POSIX_FD_READ } else { 0 })
        | (if flags.write { CLAP_POSIX_FD_WRITE } else { 0 })
        | (if flags.error { CLAP_POSIX_FD_ERROR } else { 0 })
}

unsafe extern "C" fn register_fd(
    host: *const clap_host,
    fd: i32,
    flags: clap_posix_fd_flags,
) -> bool {
    registrations(host, |r| {
        if r.fds.contains(fd) {
            return false;
        }
        r.fds.set(fd, fd_flags(flags), ());
        true
    })
    .unwrap_or(false)
}

unsafe extern "C" fn modify_fd(
    host: *const clap_host,
    fd: i32,
    flags: clap_posix_fd_flags,
) -> bool {
    registrations(host, |r| {
        if !r.fds.contains(fd) {
            return false;
        }
        r.fds.set(fd, fd_flags(flags), ());
        true
    })
    .unwrap_or(false)
}

unsafe extern "C" fn unregister_fd(host: *const clap_host, fd: i32) -> bool {
    registrations(host, |r| r.fds.remove(fd)).unwrap_or(false)
}

unsafe extern "C" fn audio_ports_is_rescan_flag_supported(
    _host: *const clap_host,
    _flag: u32,
) -> bool {
    // Ports are read again on every activation, so any change is fine.
    true
}

unsafe extern "C" fn audio_ports_rescan(_host: *const clap_host, _flags: u32) {
    // Only allowed while deactivated, and the next activation reads them.
}

unsafe extern "C" fn note_ports_supported_dialects(_host: *const clap_host) -> clap_note_dialect {
    CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI
}

unsafe extern "C" fn note_ports_rescan(_host: *const clap_host, _flags: u32) {}
