//! canopy as its own PipeWire node, rather than an ALSA client PipeWire
//! translates for: no ALSA ring buffer between the engine and the graph,
//! a name in patchbays, its own latency and rate asked of the graph, and
//! a list of outputs to pin it to.
//!
//! One [`Connection`] per running stream holds everything: a thread loop
//! (PipeWire's main-loop thread, where state changes and the registry are
//! heard), a context and core, a registry listener keeping the graph's
//! nodes, and the stream. The engine renders in the stream's process
//! callback, which with `RT_PROCESS` runs on PipeWire's real-time data
//! thread, straight into the graph's buffer.

mod ffi;
mod pod;

use std::cell::UnsafeCell;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ffi::{Lib, PwStreamEvents, PwTime, SpaHook, SpaInterface};

use crate::device::{AudioSettings, OutputDevice, Readings};
use crate::overload::Overload;
use crate::{Engine, EngineHandle};

/// Interleaved stereo, as the engine renders.
const CHANNELS: usize = 2;
/// How long to wait for the stream to be set up before giving up on
/// PipeWire (and falling back to ALSA).
const SETUP_SECONDS: c_int = 3;

/// A node in the graph, as the registry announced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    pub id: u32,
    pub name: String,
    pub description: String,
    /// `media.class`: "Audio/Sink" for an output, "Stream/Output/Audio"
    /// for a program playing.
    pub class: String,
}

/// What the stream's callbacks reach through their data pointer.
struct StreamState {
    lib: &'static Lib,
    thread_loop: *mut ffi::PwThreadLoop,
    stream: *mut ffi::PwStream,
    rate: u32,
    /// Only the process callback touches it, on the data thread.
    engine: UnsafeCell<Engine>,
    /// How long the data thread has gone without sleeping; the process
    /// callback's, made on its first run (the thread's limit is set when
    /// PipeWire makes it real-time, after the stream is).
    overload: UnsafeCell<Option<Overload>>,
    readings: Arc<Readings>,
    state: AtomicI32,
    problem: Arc<Mutex<Option<String>>>,
}

/// What the registry's callbacks reach.
struct RegistryState {
    nodes: Arc<Mutex<Vec<Node>>>,
}

/// A running PipeWire stream and the connection under it. Dropping it
/// stops the sound and lets go of everything.
pub(crate) struct Connection {
    lib: &'static Lib,
    thread_loop: *mut ffi::PwThreadLoop,
    context: *mut ffi::PwContext,
    core: *mut ffi::PwCore,
    registry: *mut c_void,
    stream: *mut ffi::PwStream,
    // Boxed so PipeWire's pointers to them hold while this moves; dropped
    // only after the objects listening through them are gone.
    registry_hook: Box<SpaHook>,
    stream_hook: Box<SpaHook>,
    registry_state: Box<RegistryState>,
    stream_state: Option<Box<StreamState>>,
    nodes: Arc<Mutex<Vec<Node>>>,
    problem: Arc<Mutex<Option<String>>>,
    pub name: String,
}

static STREAM_EVENTS: PwStreamEvents = PwStreamEvents {
    version: ffi::PW_VERSION_STREAM_EVENTS,
    destroy: None,
    state_changed: Some(on_state_changed),
    control_info: std::ptr::null(),
    io_changed: std::ptr::null(),
    param_changed: std::ptr::null(),
    add_buffer: std::ptr::null(),
    remove_buffer: std::ptr::null(),
    process: Some(on_process),
    drained: std::ptr::null(),
    command: std::ptr::null(),
    trigger_done: std::ptr::null(),
};

static REGISTRY_EVENTS: ffi::PwRegistryEvents = ffi::PwRegistryEvents {
    version: ffi::PW_VERSION_REGISTRY_EVENTS,
    global: Some(on_global),
    global_remove: Some(on_global_remove),
};

/// The program's name, for its node: "canopy", "canopy-learn".
fn program_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "canopy".to_owned())
}

/// Connects to PipeWire and starts an engine playing through a stream of
/// its own, or says why not (no library, no daemon, no stream).
pub(crate) fn start(
    settings: &AudioSettings,
    readings: Arc<Readings>,
) -> Result<(EngineHandle, Connection), String> {
    let lib = ffi::lib()?;
    let nodes = Arc::new(Mutex::new(Vec::new()));
    let problem = Arc::new(Mutex::new(None));
    let mut connection = Connection {
        lib,
        thread_loop: std::ptr::null_mut(),
        context: std::ptr::null_mut(),
        core: std::ptr::null_mut(),
        registry: std::ptr::null_mut(),
        stream: std::ptr::null_mut(),
        registry_hook: Box::new(SpaHook::zeroed()),
        stream_hook: Box::new(SpaHook::zeroed()),
        registry_state: Box::new(RegistryState {
            nodes: Arc::clone(&nodes),
        }),
        stream_state: None,
        nodes,
        problem: Arc::clone(&problem),
        name: program_name(),
    };
    // SAFETY: every call follows PipeWire's documented use: objects are
    // made and used with the thread loop locked once it runs, and each
    // pointer is checked before use. `Connection::drop` undoes what got
    // done if a step fails.
    unsafe {
        connection.thread_loop =
            (lib.thread_loop_new)(c"canopy-pipewire".as_ptr(), std::ptr::null());
        if connection.thread_loop.is_null() {
            return Err("couldn't make a PipeWire thread loop".to_owned());
        }
        let main_loop = (lib.thread_loop_get_loop)(connection.thread_loop);
        connection.context = (lib.context_new)(main_loop, std::ptr::null_mut(), 0);
        if connection.context.is_null() {
            return Err("couldn't make a PipeWire context".to_owned());
        }
        if (lib.thread_loop_start)(connection.thread_loop) != 0 {
            return Err("couldn't start PipeWire's thread".to_owned());
        }
        (lib.thread_loop_lock)(connection.thread_loop);
        let result = connection.set_up(settings, readings);
        (lib.thread_loop_unlock)(connection.thread_loop);
        let handle = result?;
        Ok((handle, connection))
    }
}

impl Connection {
    /// Connects, lists the graph's nodes and starts the stream. Runs with
    /// the thread loop locked.
    unsafe fn set_up(
        &mut self,
        settings: &AudioSettings,
        readings: Arc<Readings>,
    ) -> Result<EngineHandle, String> {
        let lib = self.lib;
        // SAFETY: see `start`; the core and registry are proxies whose
        // first member is their method table (`SpaInterface`), which the
        // C headers' inline wrappers call through the same way.
        unsafe {
            self.core = (lib.context_connect)(self.context, std::ptr::null_mut(), 0);
            if self.core.is_null() {
                return Err("PipeWire isn't running".to_owned());
            }
            let core = &*(self.core as *const SpaInterface);
            let methods = &*(core.cb.funcs as *const ffi::PwCoreMethods);
            if let Some(get_registry) = methods.get_registry {
                self.registry = get_registry(core.cb.data, ffi::PW_VERSION_REGISTRY, 0);
            }
            if !self.registry.is_null() {
                let registry = &*(self.registry as *const SpaInterface);
                let methods = &*(registry.cb.funcs as *const ffi::PwRegistryMethods);
                if let Some(add_listener) = methods.add_listener {
                    let data: *mut RegistryState = &mut *self.registry_state;
                    add_listener(
                        registry.cb.data,
                        &mut *self.registry_hook,
                        &REGISTRY_EVENTS,
                        data.cast(),
                    );
                }
            }

            let rate = settings.sample_rate;
            let (handle, engine) = crate::create(rate as f32);
            let latency = format!("{}/{rate}", settings.buffer_frames);
            let graph_rate = format!("1/{rate}");
            let mut pairs = vec![
                ("media.type", "Audio"),
                ("media.category", "Playback"),
                ("media.role", "Music"),
                ("node.name", self.name.as_str()),
                ("node.description", self.name.as_str()),
                ("application.name", self.name.as_str()),
                ("node.latency", latency.as_str()),
                // Asks the graph to switch to our rate when it can.
                ("node.rate", graph_rate.as_str()),
            ];
            if let Some(output) = &settings.output {
                pairs.push(("target.object", output.as_str()));
            }
            let props = ffi::properties(lib, &pairs);
            let name = CString::new(self.name.as_str()).unwrap_or_else(|_| c"canopy".into());
            self.stream = (lib.stream_new)(self.core, name.as_ptr(), props);
            if self.stream.is_null() {
                return Err("couldn't make a PipeWire stream".to_owned());
            }
            let state = self.stream_state.insert(Box::new(StreamState {
                lib,
                thread_loop: self.thread_loop,
                stream: self.stream,
                rate,
                engine: UnsafeCell::new(engine),
                overload: UnsafeCell::new(None),
                readings,
                state: AtomicI32::new(0),
                problem: Arc::clone(&self.problem),
            }));
            let data: *mut StreamState = &mut **state;
            (lib.stream_add_listener)(
                self.stream,
                &mut *self.stream_hook,
                &STREAM_EVENTS,
                data.cast(),
            );

            let format = pod::stereo_f32_format(rate);
            let mut params = [format.as_ptr()];
            let flags = ffi::PW_STREAM_FLAG_AUTOCONNECT
                | ffi::PW_STREAM_FLAG_MAP_BUFFERS
                | ffi::PW_STREAM_FLAG_RT_PROCESS;
            let res = (lib.stream_connect)(
                self.stream,
                ffi::PW_DIRECTION_OUTPUT,
                ffi::PW_ID_ANY,
                flags,
                params.as_mut_ptr(),
                1,
            );
            if res < 0 {
                return Err(format!("couldn't connect the PipeWire stream ({res})"));
            }
            // State changes signal the loop; wait (unlocked meanwhile) until
            // the stream is set up or has failed.
            let state = &**self.stream_state.as_ref().expect("just set");
            loop {
                match state.state.load(Ordering::Acquire) {
                    ffi::PW_STREAM_STATE_ERROR => {
                        let why = self.problem().unwrap_or_else(|| "stream error".to_owned());
                        return Err(format!("PipeWire: {why}"));
                    }
                    ffi::PW_STREAM_STATE_PAUSED | ffi::PW_STREAM_STATE_STREAMING => break,
                    _ => {}
                }
                if (lib.thread_loop_timed_wait)(self.thread_loop, SETUP_SECONDS) != 0 {
                    return Err("PipeWire didn't set the stream up in time".to_owned());
                }
            }
            Ok(handle)
        }
    }

    /// Every output (`Audio/Sink`) in the graph now, as the registry told.
    pub fn outputs(&self) -> Vec<OutputDevice> {
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        nodes
            .iter()
            .filter(|n| n.class == "Audio/Sink")
            .map(|n| OutputDevice {
                name: n.name.clone(),
                description: n.description.clone(),
            })
            .collect()
    }

    /// Every node the registry announced, for tests.
    #[cfg(test)]
    pub fn nodes(&self) -> Vec<Node> {
        self.nodes.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Why the stream stopped, if it has.
    pub fn problem(&self) -> Option<String> {
        self.problem
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let lib = self.lib;
        // SAFETY: each object is destroyed once, in the reverse of the
        // order it was made, with the loop locked while it runs; the hooks
        // and states they point at are dropped only afterwards.
        unsafe {
            if self.thread_loop.is_null() {
                return;
            }
            (lib.thread_loop_lock)(self.thread_loop);
            if !self.stream.is_null() {
                (lib.stream_destroy)(self.stream);
            }
            if !self.registry.is_null() {
                (lib.proxy_destroy)(self.registry);
            }
            if !self.core.is_null() {
                (lib.core_disconnect)(self.core);
            }
            (lib.thread_loop_unlock)(self.thread_loop);
            (lib.thread_loop_stop)(self.thread_loop);
            if !self.context.is_null() {
                (lib.context_destroy)(self.context);
            }
            (lib.thread_loop_destroy)(self.thread_loop);
        }
    }
}

/// Runs on PipeWire's real-time data thread for every graph cycle: the
/// engine fills the graph's buffer, and the timing is noted. Nothing here
/// allocates, locks or blocks.
unsafe extern "C" fn on_process(data: *mut c_void) {
    // SAFETY: `data` is the connection's `StreamState`, alive while the
    // stream is; the engine is touched only here, on this one thread; the
    // buffer is PipeWire's, mapped (MAP_BUFFERS) and ours until queued.
    unsafe {
        let state = &*(data as *const StreamState);
        let lib = state.lib;
        let buffer = (lib.stream_dequeue_buffer)(state.stream);
        if buffer.is_null() {
            return;
        }
        let spa = (*buffer).buffer;
        if !spa.is_null() && (*spa).n_datas > 0 && !(*(*spa).datas).data.is_null() {
            let d = &mut *(*spa).datas;
            let stride = std::mem::size_of::<f32>() * CHANNELS;
            let mut frames = d.maxsize as usize / stride;
            let requested = (*buffer).requested as usize;
            if requested > 0 {
                frames = frames.min(requested);
            }
            let out = std::slice::from_raw_parts_mut(d.data.cast::<f32>(), frames * CHANNELS);
            let engine = &mut *state.engine.get();
            let overload = (*state.overload.get()).get_or_insert_with(Overload::new);
            assert_no_alloc::assert_no_alloc(|| {
                // Nothing left of the budget renders nothing, so the
                // thread sleeps until the next cycle.
                engine.process_interleaved_within(out, CHANNELS, overload.budget());
                if !d.chunk.is_null() {
                    let chunk = &mut *d.chunk;
                    chunk.offset = 0;
                    chunk.stride = stride as i32;
                    chunk.size = (frames * stride) as u32;
                }
                let mut time = PwTime::default();
                let timed =
                    (lib.stream_get_time_n)(state.stream, &mut time, size_of::<PwTime>()) == 0;
                let (latency, graph_rate) = match timed && time.rate.denom > 0 {
                    true => {
                        let tick = f64::from(time.rate.num.max(1)) / f64::from(time.rate.denom);
                        // Until the graph plays what's queued, then what the
                        // converter holds, then this buffer.
                        let seconds = time.delay.max(0) as f64 * tick
                            + (time.buffered as f64 + frames as f64) / f64::from(state.rate);
                        (
                            Duration::from_secs_f64(seconds),
                            time.rate.denom / time.rate.num.max(1),
                        )
                    }
                    false => (Duration::ZERO, 0),
                };
                state.readings.record(frames, latency, graph_rate);
            });
        }
        (lib.stream_queue_buffer)(state.stream, buffer);
    }
}

/// On the thread loop: notes the stream's state (and why it failed) and
/// wakes `start`, which may be waiting for it.
unsafe extern "C" fn on_state_changed(
    data: *mut c_void,
    _old: c_int,
    new: c_int,
    error: *const c_char,
) {
    // SAFETY: `data` is the connection's `StreamState`; `error` is null or
    // a NUL-terminated string PipeWire owns for the call.
    unsafe {
        let state = &*(data as *const StreamState);
        state.state.store(new, Ordering::Release);
        let mut problem = state.problem.lock().unwrap_or_else(|e| e.into_inner());
        if new == ffi::PW_STREAM_STATE_ERROR {
            let why =
                (!error.is_null()).then(|| CStr::from_ptr(error).to_string_lossy().into_owned());
            *problem = Some(why.unwrap_or_else(|| "stream error".to_owned()));
        } else if new == ffi::PW_STREAM_STATE_STREAMING {
            *problem = None;
        }
        drop(problem);
        (state.lib.thread_loop_signal)(state.thread_loop, false);
    }
}

/// On the thread loop: a node (or anything else) appeared in the graph.
unsafe extern "C" fn on_global(
    data: *mut c_void,
    id: u32,
    _permissions: u32,
    kind: *const c_char,
    _version: u32,
    props: *const ffi::SpaDict,
) {
    // SAFETY: `data` is the connection's `RegistryState`; `kind` and
    // `props` are PipeWire's for the call.
    unsafe {
        if kind.is_null() || props.is_null() || CStr::from_ptr(kind) != c"PipeWire:Interface:Node" {
            return;
        }
        let props = &*props;
        let Some(name) = props.get(c"node.name") else {
            return;
        };
        let description = props
            .get(c"node.description")
            .or_else(|| props.get(c"node.nick"))
            .unwrap_or(name);
        let node = Node {
            id,
            name: name.to_owned(),
            description: description.to_owned(),
            class: props.get(c"media.class").unwrap_or_default().to_owned(),
        };
        let state = &*(data as *const RegistryState);
        state
            .nodes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(node);
    }
}

/// On the thread loop: something left the graph.
unsafe extern "C" fn on_global_remove(data: *mut c_void, id: u32) {
    // SAFETY: `data` is the connection's `RegistryState`.
    let state = unsafe { &*(data as *const RegistryState) };
    state
        .nodes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|n| n.id != id);
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// Against the running daemon, with `CANOPY_TEST_PIPEWIRE=1` (see
    /// `tests/pipewire.rs` for the rest).
    #[test]
    fn the_stream_shows_up_in_the_graph_under_the_programs_name() {
        if std::env::var_os("CANOPY_TEST_PIPEWIRE").is_none() {
            return;
        }
        let readings = Arc::new(Readings::default());
        let (_handle, connection) = start(&AudioSettings::new(48_000), readings).unwrap();
        let begun = Instant::now();
        let ours = |c: &Connection| {
            c.nodes()
                .into_iter()
                .any(|n| n.name == c.name && n.class == "Stream/Output/Audio")
        };
        while !ours(&connection) {
            assert!(
                begun.elapsed() < Duration::from_secs(3),
                "{:?}",
                connection.nodes()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(connection.outputs().iter().all(|o| !o.name.is_empty()));
    }
}
