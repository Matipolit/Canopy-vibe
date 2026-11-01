//! The part of libpipewire-0.3's C API canopy uses, declared by hand and
//! loaded when first needed, so building needs no PipeWire headers and a
//! machine without PipeWire still runs (on ALSA).
//!
//! Layouts follow PipeWire's headers (`pipewire/stream.h`, `core.h`,
//! `spa/buffer/buffer.h`, `spa/utils/hook.h`...), whose ABI is stable
//! across 0.3 and 1.x: structs only grow at the end, and the event tables
//! carry a version saying how much of them the caller filled in.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

use libloading::Library;

/// The library's soname: every PipeWire since 0.3 installs it.
const LIBRARY: &str = "libpipewire-0.3.so.0";

pub const PW_ID_ANY: u32 = 0xffff_ffff;
pub const PW_DIRECTION_OUTPUT: c_int = 1;

pub const PW_STREAM_FLAG_AUTOCONNECT: u32 = 1 << 0;
pub const PW_STREAM_FLAG_MAP_BUFFERS: u32 = 1 << 2;
pub const PW_STREAM_FLAG_RT_PROCESS: u32 = 1 << 4;

pub const PW_STREAM_STATE_ERROR: c_int = -1;
pub const PW_STREAM_STATE_PAUSED: c_int = 2;
pub const PW_STREAM_STATE_STREAMING: c_int = 3;

/// The versions of the tables below this file fills in.
pub const PW_VERSION_STREAM_EVENTS: u32 = 2;
pub const PW_VERSION_REGISTRY: u32 = 3;
pub const PW_VERSION_REGISTRY_EVENTS: u32 = 0;

#[repr(C)]
pub struct SpaDictItem {
    pub key: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub struct SpaDict {
    pub flags: u32,
    pub n_items: u32,
    pub items: *const SpaDictItem,
}

impl SpaDict {
    /// The value of `key`, if it's there and text.
    ///
    /// # Safety
    /// `self` must be a dictionary PipeWire handed over, still alive.
    pub unsafe fn get(&self, key: &CStr) -> Option<&str> {
        if self.items.is_null() {
            return None;
        }
        // SAFETY: PipeWire's dictionaries hold `n_items` items of
        // NUL-terminated strings (values may be null).
        let items = unsafe { std::slice::from_raw_parts(self.items, self.n_items as usize) };
        items.iter().find_map(|item| {
            if item.key.is_null() || item.value.is_null() {
                return None;
            }
            // SAFETY: as above.
            let (k, v) = unsafe { (CStr::from_ptr(item.key), CStr::from_ptr(item.value)) };
            (k == key).then(|| v.to_str().ok()).flatten()
        })
    }
}

#[repr(C)]
pub struct SpaList {
    pub next: *mut SpaList,
    pub prev: *mut SpaList,
}

#[repr(C)]
pub struct SpaCallbacks {
    pub funcs: *const c_void,
    pub data: *mut c_void,
}

/// A listener's place in an object's list. Zeroed until added; it must
/// stay put (and alive) until the object it listens to is gone.
#[repr(C)]
pub struct SpaHook {
    pub link: SpaList,
    pub cb: SpaCallbacks,
    pub removed: Option<unsafe extern "C" fn(*mut SpaHook)>,
    pub private: *mut c_void,
}

impl SpaHook {
    pub fn zeroed() -> SpaHook {
        SpaHook {
            link: SpaList {
                next: std::ptr::null_mut(),
                prev: std::ptr::null_mut(),
            },
            cb: SpaCallbacks {
                funcs: std::ptr::null(),
                data: std::ptr::null_mut(),
            },
            removed: None,
            private: std::ptr::null_mut(),
        }
    }
}

/// What every PipeWire proxy (a core, a registry) starts with: its method
/// table, which the headers' inline wrappers call through.
#[repr(C)]
pub struct SpaInterface {
    pub kind: *const c_char,
    pub version: u32,
    pub cb: SpaCallbacks,
}

#[repr(C)]
pub struct PwCoreMethods {
    pub version: u32,
    pub add_listener: *const c_void,
    pub hello: *const c_void,
    pub sync: *const c_void,
    pub pong: *const c_void,
    pub error: *const c_void,
    pub get_registry: Option<
        unsafe extern "C" fn(
            object: *mut c_void,
            version: u32,
            user_data_size: usize,
        ) -> *mut c_void,
    >,
}

#[repr(C)]
pub struct PwRegistryMethods {
    pub version: u32,
    pub add_listener: Option<
        unsafe extern "C" fn(
            object: *mut c_void,
            listener: *mut SpaHook,
            events: *const PwRegistryEvents,
            data: *mut c_void,
        ) -> c_int,
    >,
}

#[repr(C)]
pub struct PwRegistryEvents {
    pub version: u32,
    pub global: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            id: u32,
            permissions: u32,
            kind: *const c_char,
            version: u32,
            props: *const SpaDict,
        ),
    >,
    pub global_remove: Option<unsafe extern "C" fn(data: *mut c_void, id: u32)>,
}

#[repr(C)]
pub struct PwStreamEvents {
    pub version: u32,
    pub destroy: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub state_changed: Option<
        unsafe extern "C" fn(data: *mut c_void, old: c_int, state: c_int, error: *const c_char),
    >,
    pub control_info: *const c_void,
    pub io_changed: *const c_void,
    pub param_changed: *const c_void,
    pub add_buffer: *const c_void,
    pub remove_buffer: *const c_void,
    pub process: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub drained: *const c_void,
    pub command: *const c_void,
    pub trigger_done: *const c_void,
}

// SAFETY: the tables are immutable function pointers (and nulls), shared
// with PipeWire's threads only to be read.
unsafe impl Sync for PwStreamEvents {}
unsafe impl Sync for PwRegistryEvents {}

#[repr(C)]
pub struct SpaChunk {
    pub offset: u32,
    pub size: u32,
    pub stride: i32,
    pub flags: i32,
}

#[repr(C)]
pub struct SpaData {
    pub kind: u32,
    pub flags: u32,
    pub fd: i64,
    pub mapoffset: u32,
    pub maxsize: u32,
    pub data: *mut c_void,
    pub chunk: *mut SpaChunk,
}

#[repr(C)]
pub struct SpaBuffer {
    pub n_metas: u32,
    pub n_datas: u32,
    pub metas: *mut c_void,
    pub datas: *mut SpaData,
}

#[repr(C)]
pub struct PwBuffer {
    pub buffer: *mut SpaBuffer,
    pub user_data: *mut c_void,
    pub size: u64,
    /// For playback, how many frames the graph would like (0 if it
    /// doesn't say; PipeWire before 0.3.49).
    pub requested: u64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct SpaFraction {
    pub num: u32,
    pub denom: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct PwTime {
    pub now: i64,
    /// The graph's clock: a tick is `num / denom` seconds.
    pub rate: SpaFraction,
    pub ticks: u64,
    /// Ticks until the next sample handed over is heard.
    pub delay: i64,
    pub queued: u64,
    /// Frames the stream's converter holds.
    pub buffered: u64,
    pub queued_buffers: u32,
    pub avail_buffers: u32,
    /// The graph's buffer size, in ticks.
    pub size: u64,
}

// Opaque handles.
pub enum PwThreadLoop {}
pub enum PwLoop {}
pub enum PwContext {}
pub enum PwCore {}
pub enum PwStream {}
pub enum PwProperties {}

/// The functions, looked up once.
pub struct Lib {
    _library: Library,
    pub init: unsafe extern "C" fn(argc: *mut c_int, argv: *mut *mut *mut c_char),
    pub thread_loop_new:
        unsafe extern "C" fn(name: *const c_char, props: *const SpaDict) -> *mut PwThreadLoop,
    pub thread_loop_get_loop: unsafe extern "C" fn(*mut PwThreadLoop) -> *mut PwLoop,
    pub thread_loop_start: unsafe extern "C" fn(*mut PwThreadLoop) -> c_int,
    pub thread_loop_stop: unsafe extern "C" fn(*mut PwThreadLoop),
    pub thread_loop_destroy: unsafe extern "C" fn(*mut PwThreadLoop),
    pub thread_loop_lock: unsafe extern "C" fn(*mut PwThreadLoop),
    pub thread_loop_unlock: unsafe extern "C" fn(*mut PwThreadLoop),
    pub thread_loop_signal: unsafe extern "C" fn(*mut PwThreadLoop, wait_for_accept: bool),
    pub thread_loop_timed_wait: unsafe extern "C" fn(*mut PwThreadLoop, seconds: c_int) -> c_int,
    pub properties_new_dict: unsafe extern "C" fn(*const SpaDict) -> *mut PwProperties,
    pub context_new: unsafe extern "C" fn(
        main_loop: *mut PwLoop,
        props: *mut PwProperties,
        user_data_size: usize,
    ) -> *mut PwContext,
    pub context_connect: unsafe extern "C" fn(
        context: *mut PwContext,
        props: *mut PwProperties,
        user_data_size: usize,
    ) -> *mut PwCore,
    pub context_destroy: unsafe extern "C" fn(*mut PwContext),
    pub core_disconnect: unsafe extern "C" fn(*mut PwCore) -> c_int,
    pub proxy_destroy: unsafe extern "C" fn(*mut c_void),
    pub stream_new: unsafe extern "C" fn(
        *mut PwCore,
        name: *const c_char,
        props: *mut PwProperties,
    ) -> *mut PwStream,
    pub stream_add_listener: unsafe extern "C" fn(
        *mut PwStream,
        listener: *mut SpaHook,
        events: *const PwStreamEvents,
        data: *mut c_void,
    ),
    pub stream_connect: unsafe extern "C" fn(
        *mut PwStream,
        direction: c_int,
        target_id: u32,
        flags: u32,
        params: *mut *const c_void,
        n_params: u32,
    ) -> c_int,
    pub stream_destroy: unsafe extern "C" fn(*mut PwStream),
    pub stream_dequeue_buffer: unsafe extern "C" fn(*mut PwStream) -> *mut PwBuffer,
    pub stream_queue_buffer: unsafe extern "C" fn(*mut PwStream, *mut PwBuffer) -> c_int,
    pub stream_get_time_n: unsafe extern "C" fn(*mut PwStream, *mut PwTime, usize) -> c_int,
}

static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();

/// libpipewire, loaded and initialized on first use; an error if it isn't
/// installed. Never unloaded: PipeWire keeps threads and state for the
/// life of the process.
pub fn lib() -> Result<&'static Lib, String> {
    LIB.get_or_init(|| load(LIBRARY))
        .as_ref()
        .map_err(Clone::clone)
}

/// Loads `name` and looks up every function, failing whole if one's
/// missing.
pub fn load(name: &str) -> Result<Lib, String> {
    // SAFETY: loading PipeWire runs only its own initializers.
    let library = unsafe { Library::new(name) }.map_err(|e| e.to_string())?;
    macro_rules! get {
        ($symbol:literal) => {
            // SAFETY: the field's type is the function's C signature.
            *unsafe { library.get(concat!($symbol, "\0").as_bytes()) }
                .map_err(|e| format!("{}: {e}", $symbol))?
        };
    }
    let lib = Lib {
        init: get!("pw_init"),
        thread_loop_new: get!("pw_thread_loop_new"),
        thread_loop_get_loop: get!("pw_thread_loop_get_loop"),
        thread_loop_start: get!("pw_thread_loop_start"),
        thread_loop_stop: get!("pw_thread_loop_stop"),
        thread_loop_destroy: get!("pw_thread_loop_destroy"),
        thread_loop_lock: get!("pw_thread_loop_lock"),
        thread_loop_unlock: get!("pw_thread_loop_unlock"),
        thread_loop_signal: get!("pw_thread_loop_signal"),
        thread_loop_timed_wait: get!("pw_thread_loop_timed_wait"),
        properties_new_dict: get!("pw_properties_new_dict"),
        context_new: get!("pw_context_new"),
        context_connect: get!("pw_context_connect"),
        context_destroy: get!("pw_context_destroy"),
        core_disconnect: get!("pw_core_disconnect"),
        proxy_destroy: get!("pw_proxy_destroy"),
        stream_new: get!("pw_stream_new"),
        stream_add_listener: get!("pw_stream_add_listener"),
        stream_connect: get!("pw_stream_connect"),
        stream_destroy: get!("pw_stream_destroy"),
        stream_dequeue_buffer: get!("pw_stream_dequeue_buffer"),
        stream_queue_buffer: get!("pw_stream_queue_buffer"),
        stream_get_time_n: get!("pw_stream_get_time_n"),
        _library: library,
    };
    // SAFETY: pw_init accepts no arguments and may be called more than once.
    unsafe { (lib.init)(std::ptr::null_mut(), std::ptr::null_mut()) };
    Ok(lib)
}

/// Builds a `pw_properties` from pairs (PipeWire copies them).
pub fn properties(lib: &Lib, pairs: &[(&str, &str)]) -> *mut PwProperties {
    let owned: Vec<(std::ffi::CString, std::ffi::CString)> = pairs
        .iter()
        .filter_map(|(k, v)| {
            Some((
                std::ffi::CString::new(*k).ok()?,
                std::ffi::CString::new(*v).ok()?,
            ))
        })
        .collect();
    let items: Vec<SpaDictItem> = owned
        .iter()
        .map(|(k, v)| SpaDictItem {
            key: k.as_ptr(),
            value: v.as_ptr(),
        })
        .collect();
    let dict = SpaDict {
        flags: 0,
        n_items: items.len() as u32,
        items: items.as_ptr(),
    };
    // SAFETY: the dictionary and its strings outlive the call, which copies them.
    unsafe { (lib.properties_new_dict)(&dict) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_pipewires_on_64_bit() {
        use std::mem::size_of;
        assert_eq!(size_of::<SpaHook>(), 48);
        assert_eq!(size_of::<SpaData>(), 40);
        assert_eq!(size_of::<SpaChunk>(), 16);
        assert_eq!(size_of::<SpaBuffer>(), 24);
        assert_eq!(size_of::<PwTime>(), 64);
        assert_eq!(size_of::<PwStreamEvents>(), 8 + 11 * 8);
        assert_eq!(size_of::<SpaDict>(), 16);
    }

    #[test]
    fn a_missing_library_is_an_error_not_a_crash() {
        let err = load("libpipewire-does-not-exist.so.0").err().unwrap();
        assert!(err.contains("libpipewire-does-not-exist"), "{err}");
    }

    #[test]
    fn dictionaries_find_their_values() {
        let (k1, v1) = (c"media.class", c"Audio/Sink");
        let (k2, v2) = (c"node.name", c"speakers");
        let items = [
            SpaDictItem {
                key: k1.as_ptr(),
                value: v1.as_ptr(),
            },
            SpaDictItem {
                key: k2.as_ptr(),
                value: v2.as_ptr(),
            },
        ];
        let dict = SpaDict {
            flags: 0,
            n_items: 2,
            items: items.as_ptr(),
        };
        // SAFETY: the dictionary is ours and alive.
        unsafe {
            assert_eq!(dict.get(c"node.name"), Some("speakers"));
            assert_eq!(dict.get(c"node.description"), None);
        }
    }
}
