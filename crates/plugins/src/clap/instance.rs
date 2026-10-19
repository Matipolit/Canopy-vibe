//! One CLAP plugin on the main thread.
//!
//! [`ClapPlugin`] owns the `clap_plugin` and is shared (in an `Arc`) by the
//! main-thread [`ClapInstance`] and, while active, the audio-thread
//! [`ClapProcessor`]; whichever goes last destroys the plugin, on the main
//! thread (`crate::Processor` makes sure of that).

use std::cell::{Cell, RefCell};
use std::ffi::{CString, c_char, c_void};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::gui::*;
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::posix_fd_support::{CLAP_EXT_POSIX_FD_SUPPORT, clap_plugin_posix_fd_support};
use clap_sys::ext::render::*;
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::ext::tail::{CLAP_EXT_TAIL, clap_plugin_tail};
use clap_sys::ext::timer_support::{CLAP_EXT_TIMER_SUPPORT, clap_plugin_timer_support};
use clap_sys::plugin::clap_plugin;
use clap_sys::stream::{clap_istream, clap_ostream};

use super::host::{HostBox, clap_fd_flags};
use super::processor::{ClapProcessor, NoteDialect, ProcessorSetup};
use super::{Module, text_buf};
use crate::block::{self, Change, Modulation, OutEvent, Queues};
use crate::editor::{self, EditorOptions, Window, WindowEvent};
use crate::thread::{AudioThreadScope, MainThread};
use crate::wake::Wake;
use crate::{
    AudioConfig, AudioPortInfo, Error, NotePortInfo, ParamFlags, ParamId, ParamInfo, PluginEvent,
    PluginInfo, Tail,
};

/// A plugin's extension tables, null where it has none. Valid as long as
/// the plugin is.
pub(crate) struct Extensions {
    pub params: *const clap_plugin_params,
    pub state: *const clap_plugin_state,
    pub latency: *const clap_plugin_latency,
    pub tail: *const clap_plugin_tail,
    pub gui: *const clap_plugin_gui,
    pub timer_support: *const clap_plugin_timer_support,
    pub posix_fd: *const clap_plugin_posix_fd_support,
    pub audio_ports: *const clap_plugin_audio_ports,
    pub note_ports: *const clap_plugin_note_ports,
    pub render: *const clap_plugin_render,
}

/// A created and initialized `clap_plugin`.
pub(crate) struct ClapPlugin {
    pub plugin: *const clap_plugin,
    pub ext: Extensions,
    pub active: AtomicBool,
    pub processing: AtomicBool,
    /// Its `clap_host`; outlives the plugin (fields drop after `drop`).
    pub host: Pin<Box<HostBox>>,
    _module: Arc<Module>,
}

// SAFETY: the plugin is only called as CLAP's thread rules allow: main
// thread functions from the main thread (ClapInstance is !Send and checks
// nothing else is needed), audio-thread functions from whoever owns the
// ClapProcessor, which there is at most one of.
unsafe impl Send for ClapPlugin {}
unsafe impl Sync for ClapPlugin {}

impl ClapPlugin {
    fn new(
        module: Arc<Module>,
        id: &str,
        name: &str,
        main: MainThread,
        wake: Arc<Wake>,
    ) -> Result<ClapPlugin, Error> {
        let host = HostBox::new(main, name, wake);
        let factory = module.factory();
        let c_id = CString::new(id).map_err(|_| Error::NotFound(id.to_owned()))?;
        let plugin = factory
            .create_plugin
            // SAFETY: a valid host and plugin id; creation is main-thread.
            .map(|create| unsafe { create(module.factory, host.as_ptr(), c_id.as_ptr()) })
            .unwrap_or(std::ptr::null());
        if plugin.is_null() {
            return Err(Error::NotFound(id.to_owned()));
        }
        // SAFETY: the factory returned a live plugin, not yet initialized.
        let initialized = unsafe { (*plugin).init.is_some_and(|init| init(plugin)) };
        if !initialized {
            // SAFETY: destroy is allowed after a failed init.
            unsafe {
                if let Some(destroy) = (*plugin).destroy {
                    destroy(plugin);
                }
            }
            return Err(Error::Plugin(format!("{name}: init failed")));
        }
        let get = |id: &std::ffi::CStr| -> *const c_void {
            // SAFETY: get_extension is callable once the plugin is initialized.
            unsafe {
                (*plugin)
                    .get_extension
                    .map_or(std::ptr::null(), |get| get(plugin, id.as_ptr()))
            }
        };
        let ext = Extensions {
            params: get(CLAP_EXT_PARAMS).cast(),
            state: get(CLAP_EXT_STATE).cast(),
            latency: get(CLAP_EXT_LATENCY).cast(),
            tail: get(CLAP_EXT_TAIL).cast(),
            gui: get(CLAP_EXT_GUI).cast(),
            timer_support: get(CLAP_EXT_TIMER_SUPPORT).cast(),
            posix_fd: get(CLAP_EXT_POSIX_FD_SUPPORT).cast(),
            audio_ports: get(CLAP_EXT_AUDIO_PORTS).cast(),
            note_ports: get(CLAP_EXT_NOTE_PORTS).cast(),
            render: get(CLAP_EXT_RENDER).cast(),
        };
        Ok(ClapPlugin {
            plugin,
            ext,
            active: AtomicBool::new(false),
            processing: AtomicBool::new(false),
            host,
            _module: module,
        })
    }

    /// The plugin's own vtable.
    pub fn vt(&self) -> &clap_plugin {
        // SAFETY: valid from creation until `drop` destroys it.
        unsafe { &*self.plugin }
    }

    /// Calls `deactivate`; main thread, once processing has stopped.
    pub fn deactivate_now(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            // SAFETY: active and not processing; main thread by our callers.
            unsafe {
                if let Some(deactivate) = self.vt().deactivate {
                    deactivate(self.plugin);
                }
            }
        }
    }
}

/// An extension table, if the plugin has it.
///
/// # Safety
///
/// `ptr` must be null or a table valid for `'a`.
pub(crate) unsafe fn ext<'a, T>(ptr: *const T) -> Option<&'a T> {
    // SAFETY: by the caller.
    unsafe { ptr.as_ref() }
}

impl Drop for ClapPlugin {
    fn drop(&mut self) {
        if self.processing.swap(false, Ordering::AcqRel) {
            let _audio = AudioThreadScope::enter();
            // SAFETY: nothing else can be processing: we're the last owner.
            unsafe {
                if let Some(stop) = self.vt().stop_processing {
                    stop(self.plugin);
                }
            }
        }
        self.deactivate_now();
        // SAFETY: inactive, and nothing holds the plugin any more.
        unsafe {
            if let Some(destroy) = self.vt().destroy {
                destroy(self.plugin);
            }
        }
    }
}

/// An open editor: our window with the plugin's GUI inside.
struct Editor {
    window: Window,
    size: (u32, u32),
    resizable: bool,
}

/// A CLAP plugin on the main thread.
pub(crate) struct ClapInstance {
    plugin: Arc<ClapPlugin>,
    name: String,
    queues: RefCell<Option<Queues>>,
    latency: Cell<u32>,
    editor: RefCell<Option<Editor>>,
}

impl ClapInstance {
    pub fn new(
        module: Arc<Module>,
        info: &PluginInfo,
        main: MainThread,
        wake: Arc<Wake>,
    ) -> Result<ClapInstance, Error> {
        let plugin = ClapPlugin::new(module, &info.id, &info.name, main, wake)?;
        Ok(ClapInstance {
            plugin: Arc::new(plugin),
            name: info.name.clone(),
            queues: RefCell::new(None),
            latency: Cell::new(0),
            editor: RefCell::new(None),
        })
    }

    fn raw(&self) -> *const clap_plugin {
        self.plugin.plugin
    }

    pub fn audio_ports(&self) -> (Vec<AudioPortInfo>, Vec<AudioPortInfo>) {
        let ports = |input: bool| -> Vec<AudioPortInfo> {
            // SAFETY: the table lives as long as the plugin.
            let Some(table) = (unsafe { ext(self.plugin.ext.audio_ports) }) else {
                return Vec::new();
            };
            let (Some(count), Some(get)) = (table.count, table.get) else {
                return Vec::new();
            };
            // SAFETY: main-thread port queries with a buffer to fill.
            let count = unsafe { count(self.raw(), input) };
            (0..count)
                .filter_map(|i| {
                    let mut info: clap_audio_port_info = unsafe { std::mem::zeroed() };
                    unsafe { get(self.raw(), i, input, &mut info) }.then(|| AudioPortInfo {
                        name: text_buf(&info.name),
                        channels: info.channel_count,
                        main: info.flags & CLAP_AUDIO_PORT_IS_MAIN != 0,
                    })
                })
                .collect()
        };
        (ports(true), ports(false))
    }

    pub fn note_ports(&self) -> (Vec<NotePortInfo>, Vec<NotePortInfo>) {
        let (inputs, outputs) = (self.note_port_infos(true), self.note_port_infos(false));
        let names = |ports: Vec<clap_note_port_info>| {
            ports
                .iter()
                .map(|p| NotePortInfo {
                    name: text_buf(&p.name),
                })
                .collect()
        };
        (names(inputs), names(outputs))
    }

    fn note_port_infos(&self, input: bool) -> Vec<clap_note_port_info> {
        // SAFETY: as in `audio_ports`.
        let Some(table) = (unsafe { ext(self.plugin.ext.note_ports) }) else {
            return Vec::new();
        };
        let (Some(count), Some(get)) = (table.count, table.get) else {
            return Vec::new();
        };
        let count = unsafe { count(self.raw(), input) };
        (0..count)
            .filter_map(|i| {
                let mut info: clap_note_port_info = unsafe { std::mem::zeroed() };
                unsafe { get(self.raw(), i, input, &mut info) }.then_some(info)
            })
            .collect()
    }

    fn param_infos(&self) -> Vec<clap_param_info> {
        // SAFETY: main-thread parameter queries.
        let Some(table) = (unsafe { ext(self.plugin.ext.params) }) else {
            return Vec::new();
        };
        let (Some(count), Some(get)) = (table.count, table.get_info) else {
            return Vec::new();
        };
        let count = unsafe { count(self.raw()) };
        (0..count)
            .filter_map(|i| {
                let mut info: clap_param_info = unsafe { std::mem::zeroed() };
                unsafe { get(self.raw(), i, &mut info) }.then_some(info)
            })
            .collect()
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        self.param_infos()
            .iter()
            .map(|p| {
                let has = |flag| p.flags & flag != 0;
                let stepped = has(CLAP_PARAM_IS_STEPPED);
                ParamInfo {
                    id: ParamId(p.id),
                    name: text_buf(&p.name),
                    module: text_buf(&p.module),
                    unit: String::new(),
                    min: p.min_value,
                    max: p.max_value,
                    default: p.default_value,
                    step_count: if stepped {
                        (p.max_value - p.min_value)
                            .round()
                            .clamp(0.0, f64::from(u32::MAX)) as u32
                    } else {
                        0
                    },
                    flags: ParamFlags {
                        stepped,
                        enumerated: has(CLAP_PARAM_IS_ENUM),
                        automatable: has(CLAP_PARAM_IS_AUTOMATABLE),
                        modulatable: has(CLAP_PARAM_IS_MODULATABLE),
                        periodic: has(CLAP_PARAM_IS_PERIODIC),
                        read_only: has(CLAP_PARAM_IS_READONLY),
                        hidden: has(CLAP_PARAM_IS_HIDDEN),
                        bypass: has(CLAP_PARAM_IS_BYPASS),
                    },
                }
            })
            .collect()
    }

    pub fn param_value(&self, id: ParamId) -> Option<f64> {
        // SAFETY: main-thread call with somewhere to write.
        let table = unsafe { ext(self.plugin.ext.params) }?;
        let get = table.get_value?;
        let mut value = 0.0;
        unsafe { get(self.raw(), id.0, &mut value) }.then_some(value)
    }

    pub fn param_text(&self, id: ParamId, value: f64) -> Option<String> {
        // SAFETY: main-thread call with a buffer of the size we pass.
        let table = unsafe { ext(self.plugin.ext.params) }?;
        let to_text = table.value_to_text?;
        let mut buf = [0 as c_char; 256];
        unsafe { to_text(self.raw(), id.0, value, buf.as_mut_ptr(), buf.len() as u32) }
            .then(|| text_buf(&buf))
    }

    pub fn param_from_text(&self, id: ParamId, text: &str) -> Option<f64> {
        let table = unsafe { ext(self.plugin.ext.params) }?;
        let from_text = table.text_to_value?;
        let text = CString::new(text).ok()?;
        let mut value = 0.0;
        // SAFETY: main-thread call with a C string and somewhere to write.
        unsafe { from_text(self.raw(), id.0, text.as_ptr(), &mut value) }.then_some(value)
    }

    /// Sets a parameter from the host: through the audio thread while
    /// active, directly with `params.flush` otherwise.
    pub fn set_param(&self, id: ParamId, value: f64) {
        if self.plugin.active.load(Ordering::Acquire) {
            if let Some(queues) = self.queues.borrow_mut().as_mut() {
                let _ = queues.to_audio.push(Change { id, value });
            }
            return;
        }
        let cookie = self
            .param_infos()
            .iter()
            .find(|p| p.id == id.0)
            .map_or(std::ptr::null_mut(), |p| p.cookie);
        let event = clap_event_param_value {
            header: clap_event_header {
                size: size_of::<clap_event_param_value>() as u32,
                time: 0,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_PARAM_VALUE,
                flags: 0,
            },
            param_id: id.0,
            cookie,
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value,
        };
        let mut out = Vec::new();
        self.flush(&[&event.header], &mut out);
    }

    /// `params.flush` with `events` in; what comes out goes to `out`.
    fn flush(&self, events: &[&clap_event_header], out: &mut Vec<OutEvent>) {
        // SAFETY: flush is main-thread while the plugin is inactive, which
        // our callers check; the lists live for the call.
        let Some(flush) = (unsafe { ext(self.plugin.ext.params) }).and_then(|t| t.flush) else {
            return;
        };
        let input = clap_input_events {
            ctx: (&events as *const &[&clap_event_header]).cast_mut().cast(),
            size: Some(flush_in_size),
            get: Some(flush_in_get),
        };
        let output = clap_output_events {
            ctx: (out as *mut Vec<OutEvent>).cast(),
            try_push: Some(flush_out_push),
        };
        unsafe { flush(self.raw(), &input, &output) };
    }

    pub fn save_state(&self) -> Result<Vec<u8>, Error> {
        let name = self.name();
        let table = unsafe { ext(self.plugin.ext.state) }
            .ok_or_else(|| Error::State(format!("{name} can't save its state")))?;
        let save = table
            .save
            .ok_or_else(|| Error::State(format!("{name} can't save its state")))?;
        let mut bytes = Vec::new();
        let stream = clap_ostream {
            ctx: (&mut bytes as *mut Vec<u8>).cast(),
            write: Some(stream_write),
        };
        // SAFETY: main-thread call; the stream writes into `bytes`.
        if unsafe { save(self.raw(), &stream) } {
            Ok(bytes)
        } else {
            Err(Error::State(format!("{name} failed to save its state")))
        }
    }

    pub fn load_state(&self, state: &[u8]) -> Result<(), Error> {
        let name = self.name();
        let table = unsafe { ext(self.plugin.ext.state) }
            .ok_or_else(|| Error::State(format!("{name} can't load a state")))?;
        let load = table
            .load
            .ok_or_else(|| Error::State(format!("{name} can't load a state")))?;
        let mut reader = Reader {
            bytes: state,
            pos: 0,
        };
        let stream = clap_istream {
            ctx: (&mut reader as *mut Reader).cast(),
            read: Some(stream_read),
        };
        // SAFETY: main-thread call; the stream reads from `state`.
        if unsafe { load(self.raw(), &stream) } {
            Ok(())
        } else {
            Err(Error::State(format!("{name} failed to load its state")))
        }
    }

    fn name(&self) -> &str {
        &self.name
    }

    pub fn is_active(&self) -> bool {
        self.plugin.active.load(Ordering::Acquire)
    }

    pub fn activate(&self, config: AudioConfig) -> Result<ClapProcessor, Error> {
        if self.is_active() {
            return Err(Error::Plugin(format!("{} is already active", self.name())));
        }
        // SAFETY: main-thread calls on an inactive plugin.
        unsafe {
            if let Some(set) = ext(self.plugin.ext.render).and_then(|r| r.set) {
                let mode = if config.offline {
                    CLAP_RENDER_OFFLINE
                } else {
                    CLAP_RENDER_REALTIME
                };
                set(self.raw(), mode);
            }
        }
        let requests = &self.plugin.host.data().requests;
        requests.latency_changed.store(false, Ordering::Release);
        let max_block = config.max_block.max(1);
        let min_block = config.min_block.clamp(1, max_block);
        // SAFETY: as above.
        let activated = unsafe {
            self.plugin.vt().activate.is_some_and(|activate| {
                activate(self.raw(), config.sample_rate, min_block, max_block)
            })
        };
        if !activated {
            return Err(Error::Plugin(format!("{} failed to activate", self.name())));
        }
        self.plugin.active.store(true, Ordering::Release);
        // Whatever it asked before it was active is answered by activating.
        requests.restart.store(false, Ordering::Release);
        requests.latency_changed.store(false, Ordering::Release);
        // SAFETY: latency.get is main-thread once active.
        let latency = unsafe { ext(self.plugin.ext.latency) }
            .and_then(|l| l.get)
            .map_or(0, |get| unsafe { get(self.raw()) });
        self.latency.set(latency);

        let (inputs, outputs) = self.audio_ports();
        let channels =
            |ports: &[AudioPortInfo]| ports.iter().map(|p| p.channels).collect::<Vec<_>>();
        let dialect = self.note_port_infos(true).first().map(|port| NoteDialect {
            clap: port.supported_dialects & CLAP_NOTE_DIALECT_CLAP != 0,
            midi: port.supported_dialects & (CLAP_NOTE_DIALECT_MIDI | CLAP_NOTE_DIALECT_MIDI_MPE)
                != 0,
        });
        let infos = self.param_infos();
        let mut cookies: Vec<(u32, usize)> =
            infos.iter().map(|p| (p.id, p.cookie as usize)).collect();
        cookies.sort_unstable();
        let modulation = Modulation::new(infos.iter().map(|p| {
            let value = self.param_value(ParamId(p.id)).unwrap_or(p.default_value);
            let native = p.flags & CLAP_PARAM_IS_MODULATABLE != 0;
            (p.id, value, p.min_value, p.max_value, native)
        }));
        let (queues, audio_queues) = block::queues();
        *self.queues.borrow_mut() = Some(queues);
        let setup = ProcessorSetup {
            config: AudioConfig {
                min_block,
                max_block,
                ..config
            },
            input_channels: channels(&inputs),
            output_channels: channels(&outputs),
            main_input: crate::info::main_port_index(&inputs),
            aux_input: crate::info::aux_port_index(&inputs),
            main_output: crate::info::main_port_index(&outputs),
            dialect,
            cookies,
            modulation,
        };
        Ok(ClapProcessor::new(self.plugin.clone(), setup, audio_queues))
    }

    /// Deactivates the plugin, its processor back from the audio thread.
    pub fn deactivate(&self, mut processor: ClapProcessor) {
        let unsent = processor.take_queued();
        drop(processor);
        // Dropped on the main thread, the processor stopped processing and
        // deactivated already; this covers a processor dropped elsewhere.
        self.plugin.deactivate_now();
        self.queues.borrow_mut().take();
        // Inactive now, the plugin takes them at once.
        for change in unsent {
            self.set_param(change.id, change.value);
        }
    }

    pub fn latency(&self) -> u32 {
        self.latency.get()
    }

    pub fn tail(&self) -> Tail {
        // SAFETY: tail.get is main-thread or audio-thread, active or not.
        let tail = unsafe { ext(self.plugin.ext.tail) }
            .and_then(|t| t.get)
            .map_or(0, |get| unsafe { get(self.raw()) });
        if tail == u32::MAX {
            Tail::Infinite
        } else {
            Tail::Samples(tail)
        }
    }

    /// Runs what the plugin asked for on the main thread and reports what
    /// it did. Returns when it next needs to run (its next timer).
    pub fn idle(&self, events: &mut Vec<PluginEvent>) -> Option<Instant> {
        let data = self.plugin.host.data();
        let requests = &data.requests;
        if requests.callback.swap(false, Ordering::AcqRel) {
            // SAFETY: on_main_thread is what request_callback asks for.
            unsafe {
                if let Some(on_main_thread) = self.plugin.vt().on_main_thread {
                    on_main_thread(self.raw());
                }
            }
        }
        let registrations = data.registrations.get().expect("idle on the host's thread");
        let due = registrations.borrow_mut().timers.due(Instant::now());
        if let Some(on_timer) =
            unsafe { ext(self.plugin.ext.timer_support) }.and_then(|t| t.on_timer)
        {
            for id in due {
                // SAFETY: main-thread timer callback for a timer it registered.
                unsafe { on_timer(self.raw(), id) };
            }
        }
        let ready = registrations.borrow().fds.ready();
        if let Some(on_fd) = unsafe { ext(self.plugin.ext.posix_fd) }.and_then(|t| t.on_fd) {
            for (fd, flags, ()) in ready {
                // SAFETY: main-thread callback for a descriptor it registered.
                unsafe { on_fd(self.raw(), fd, clap_fd_flags(flags)) };
            }
        }
        let mut out = Vec::new();
        if requests.flush.swap(false, Ordering::AcqRel) && !self.is_active() {
            self.flush(&[], &mut out);
        }
        if let Some(queues) = self.queues.borrow_mut().as_mut() {
            while let Ok(event) = queues.from_audio.pop() {
                out.push(event);
            }
        }
        events.extend(out.into_iter().map(|e| match e {
            OutEvent::Param(id, value) => PluginEvent::ParamValue { id, value },
            OutEvent::Gesture(id, begin) => PluginEvent::ParamGesture { id, begin },
        }));
        let rescan = requests.rescan.swap(0, Ordering::AcqRel);
        if rescan != 0 {
            let infos = rescan
                & (CLAP_PARAM_RESCAN_INFO | CLAP_PARAM_RESCAN_ALL | CLAP_PARAM_RESCAN_TEXT)
                != 0;
            events.push(PluginEvent::ParamsRescanned { infos });
        }
        let restart = requests.restart.swap(false, Ordering::AcqRel);
        // Latency may only change while activating; a plugin that says so
        // while active wants a restart to apply it.
        let latency = requests.latency_changed.swap(false, Ordering::AcqRel);
        if (restart || latency) && self.is_active() {
            events.push(PluginEvent::RestartRequested);
        }
        if requests.tail_changed.swap(false, Ordering::AcqRel) {
            events.push(PluginEvent::TailChanged);
        }
        if requests.dirty.swap(false, Ordering::AcqRel) {
            events.push(PluginEvent::StateDirty);
        }
        self.idle_editor(events);
        registrations.borrow().timers.next_due()
    }

    fn gui(&self) -> Option<&clap_plugin_gui> {
        // SAFETY: the table lives as long as the plugin.
        unsafe { ext(self.plugin.ext.gui) }
    }

    pub fn has_editor(&self) -> bool {
        let Some(api) = editor::CLAP_API else {
            return false;
        };
        // SAFETY: main-thread query with a static C string.
        self.gui()
            .and_then(|g| g.is_api_supported)
            .is_some_and(|supported| unsafe { supported(self.raw(), api.as_ptr(), false) })
    }

    pub fn editor_is_open(&self) -> bool {
        self.editor.borrow().is_some()
    }

    pub fn open_editor(&self, options: &EditorOptions) -> Result<(), Error> {
        if self.editor_is_open() {
            return Ok(());
        }
        let no_editor = || Error::Editor(format!("{} has no editor we can show", self.name()));
        let api = editor::CLAP_API.ok_or_else(no_editor)?;
        if !self.has_editor() {
            return Err(no_editor());
        }
        let gui = self.gui().ok_or_else(no_editor)?;
        let raw = self.raw();
        // SAFETY: main-thread gui calls in the order the gui extension
        // documents: create, size, set_parent, show; destroy on failure.
        unsafe {
            if !gui
                .create
                .is_some_and(|create| create(raw, api.as_ptr(), false))
            {
                return Err(Error::Editor(format!(
                    "{} failed to create its editor",
                    self.name()
                )));
            }
            let (mut width, mut height) = (0, 0);
            if !gui
                .get_size
                .is_some_and(|get| get(raw, &mut width, &mut height))
            {
                (width, height) = (640, 480);
            }
            let resizable = gui.can_resize.is_some_and(|can| can(raw));
            let window = match Window::open(options, width.max(1), height.max(1), resizable) {
                Ok(window) => window,
                Err(e) => {
                    if let Some(destroy) = gui.destroy {
                        destroy(raw);
                    }
                    return Err(e);
                }
            };
            let parent = clap_window {
                api: api.as_ptr(),
                specific: clap_handle(&window),
            };
            if !gui.set_parent.is_some_and(|set| set(raw, &parent)) {
                if let Some(destroy) = gui.destroy {
                    destroy(raw);
                }
                return Err(Error::Editor(format!("{} refused our window", self.name())));
            }
            if let Some(suggest) = gui.suggest_title
                && let Ok(title) = CString::new(options.title.as_str())
            {
                suggest(raw, title.as_ptr());
            }
            if let Some(show) = gui.show {
                show(raw);
            }
            *self.editor.borrow_mut() = Some(Editor {
                window,
                size: (width, height),
                resizable,
            });
        }
        Ok(())
    }

    pub fn set_editor_keep_on_top(&self, on_top: bool) {
        if let Some(editor) = self.editor.borrow().as_ref() {
            editor.window.set_keep_on_top(on_top);
        }
    }

    /// The registered file descriptors, for a thread that waits on them.
    pub fn watched_fds(&self, out: &mut Vec<i32>) {
        if let Some(registrations) = self.plugin.host.data().registrations.get() {
            registrations.borrow().fds.fds(out);
        }
        if let Some(editor) = self.editor.borrow().as_ref() {
            out.extend(editor.window.fd());
        }
    }

    pub fn close_editor(&self) {
        let Some(_editor) = self.editor.borrow_mut().take() else {
            return;
        };
        if let Some(gui) = self.gui() {
            // SAFETY: main-thread calls on the editor we created.
            unsafe {
                if let Some(hide) = gui.hide {
                    hide(self.raw());
                }
                if let Some(destroy) = gui.destroy {
                    destroy(self.raw());
                }
            }
        }
        // The window closes as `_editor` goes, after the plugin let go of it.
    }

    fn idle_editor(&self, events: &mut Vec<PluginEvent>) {
        let requests = &self.plugin.host.data().requests;
        let resize = requests
            .resize
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let show = requests
            .show
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let window_events = match self.editor.borrow_mut().as_mut() {
            Some(editor) => {
                if let Some((width, height)) = resize {
                    editor.size = (width, height);
                    editor.window.set_size(width, height, editor.resizable);
                }
                if let Some(visible) = show {
                    editor.window.show(visible);
                }
                editor.window.poll()
            }
            None => return,
        };
        for event in window_events {
            match event {
                WindowEvent::CloseRequested => {
                    self.close_editor();
                    events.push(PluginEvent::EditorClosed);
                    return;
                }
                WindowEvent::Resized(width, height) => self.window_resized(width, height),
            }
        }
    }

    /// The user resized the window: let the plugin pick the nearest size
    /// it can be, and fit the window to that.
    fn window_resized(&self, width: u32, height: u32) {
        let Some(gui) = self.gui() else { return };
        let mut slot = self.editor.borrow_mut();
        let Some(editor) = slot.as_mut() else { return };
        if (width, height) == editor.size {
            return;
        }
        if !editor.resizable {
            editor.window.set_size(editor.size.0, editor.size.1, false);
            return;
        }
        let (mut w, mut h) = (width, height);
        // SAFETY: main-thread gui calls on an open editor.
        unsafe {
            if let Some(adjust) = gui.adjust_size {
                adjust(self.raw(), &mut w, &mut h);
            }
            if gui.set_size.is_some_and(|set| set(self.raw(), w, h)) {
                editor.size = (w, h);
            }
        }
        if editor.size != (width, height) {
            editor.window.set_size(editor.size.0, editor.size.1, true);
        }
    }
}

impl Drop for ClapInstance {
    fn drop(&mut self) {
        self.close_editor();
    }
}

/// Our window as CLAP's union holds it.
fn clap_handle(window: &Window) -> clap_window_handle {
    match window.parent() {
        #[cfg(unix)]
        editor::Parent::X11(id) => clap_window_handle { x11: id.into() },
        #[cfg(windows)]
        editor::Parent::Win32(hwnd) => clap_window_handle { win32: hwnd },
        #[allow(unreachable_patterns)]
        _ => clap_window_handle {
            ptr: std::ptr::null_mut(),
        },
    }
}

/// Reads a state back from a byte slice.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

unsafe extern "C" fn stream_write(
    stream: *const clap_ostream,
    buffer: *const c_void,
    size: u64,
) -> i64 {
    // SAFETY: `ctx` is the Vec `save_state` passed; the plugin gives a
    // buffer of `size` bytes.
    unsafe {
        let bytes = &mut *(*stream).ctx.cast::<Vec<u8>>();
        let size = size.min(i64::MAX as u64) as usize;
        if size > 0 {
            bytes.extend_from_slice(std::slice::from_raw_parts(buffer.cast::<u8>(), size));
        }
        size as i64
    }
}

unsafe extern "C" fn stream_read(
    stream: *const clap_istream,
    buffer: *mut c_void,
    size: u64,
) -> i64 {
    // SAFETY: `ctx` is the Reader `load_state` passed; the plugin gives
    // room for `size` bytes.
    unsafe {
        let reader = &mut *(*stream).ctx.cast::<Reader>();
        let rest = &reader.bytes[reader.pos..];
        let n = rest.len().min(size.min(i64::MAX as u64) as usize);
        std::ptr::copy_nonoverlapping(rest.as_ptr(), buffer.cast::<u8>(), n);
        reader.pos += n;
        n as i64
    }
}

unsafe extern "C" fn flush_in_size(list: *const clap_input_events) -> u32 {
    // SAFETY: `ctx` points at the slice `flush` passed.
    let events: &[&clap_event_header] = unsafe { *(*list).ctx.cast::<&[&clap_event_header]>() };
    events.len() as u32
}

unsafe extern "C" fn flush_in_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    // SAFETY: as above.
    unsafe {
        let events = *(*list).ctx.cast::<&[&clap_event_header]>();
        events
            .get(index as usize)
            .map_or(std::ptr::null(), |e| *e as *const clap_event_header)
    }
}

unsafe extern "C" fn flush_out_push(
    list: *const clap_output_events,
    event: *const clap_event_header,
) -> bool {
    // SAFETY: `ctx` is the Vec `flush` passed; events are as CLAP lays them
    // out, their type given by the header.
    unsafe {
        let out = &mut *(*list).ctx.cast::<Vec<OutEvent>>();
        if let Some(event) = super::processor::out_event(event) {
            out.push(event);
        }
    }
    true
}
