//! One instrument as a CLAP plugin: the `clap_plugin` and its extensions
//! (params, state, audio ports, note ports) on the raw C ABI.
//!
//! Threads, as CLAP has them: the host creates, activates and asks about
//! parameters and state on its main thread, and processes on its audio
//! thread while active. Parameter values live in [`Values`], readable
//! from both. The [`Bridge`] (the instrument) sits behind a mutex that
//! only one side wants at a time: the audio thread while active, the main
//! thread while not, so taking it never waits.

use std::ffi::{CStr, c_char, c_void};
use std::ptr::null;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::device::{Device, Display};
use crate::editor::Edits;
use crate::gui::{HostHandle, NativeGui};
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::gui::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::fixedpoint::CLAP_BEATTIME_FACTOR;
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::*;
use clap_sys::stream::{clap_istream, clap_ostream};
use instruments::ParameterKind;
use std::cell::{Cell, RefCell};

use crate::bridge::{Bridge, Input, Transport};
use crate::state;
use crate::values::{Values, range, to_host, to_plain};

struct Plugin {
    clap: clap_plugin,
    device: Device,
    values: Arc<Values>,
    bridge: Mutex<Bridge>,
    active: AtomicBool,
    tail_samples: AtomicU32,
    display: Arc<Display>,
    edits: Arc<Edits>,
    host: HostHandle,
    gui: RefCell<Option<NativeGui>>,
    gui_destroy_requested: Cell<bool>,
}

impl Plugin {
    fn close_gui_if_requested(&self) {
        if self.gui_destroy_requested.get()
            && let Ok(mut gui) = self.gui.try_borrow_mut()
        {
            let window = gui.take();
            self.gui_destroy_requested.set(false);
            drop(gui);
            drop(window);
        }
    }
    /// # Safety
    /// `plugin` must be one [`create`] made and not yet destroyed.
    unsafe fn from<'a>(plugin: *const clap_plugin) -> &'a Plugin {
        unsafe { &*((*plugin).plugin_data as *const Plugin) }
    }

    fn bridge(&self) -> MutexGuard<'_, Bridge> {
        // A panic can't poison it across the C boundary (it would abort),
        // but if it ever is, the bridge is still whole.
        self.bridge
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A new plugin playing `instrument`, described to the host by
/// `descriptor`. Main thread.
pub fn create(
    device: Device,
    descriptor: &'static clap_plugin_descriptor,
    host: *const clap_host,
) -> *const clap_plugin {
    let values = Arc::new(Values::new(device.params()));
    let display = Arc::new(Display::default());
    let edits = Arc::new(Edits::new(values.len()));
    let plugin = Box::into_raw(Box::new(Plugin {
        clap: clap_plugin {
            desc: descriptor,
            plugin_data: std::ptr::null_mut(),
            init: Some(init),
            destroy: Some(destroy),
            activate: Some(activate),
            deactivate: Some(deactivate),
            start_processing: Some(start_processing),
            stop_processing: Some(stop_processing),
            reset: Some(reset),
            process: Some(process),
            get_extension: Some(get_extension),
            on_main_thread: Some(on_main_thread),
        },
        device,
        bridge: Mutex::new(Bridge::for_device(device, values.clone(), display.clone())),
        display,
        edits,
        host: HostHandle(host as usize),
        gui: RefCell::new(None),
        gui_destroy_requested: Cell::new(false),
        values,
        active: AtomicBool::new(false),
        tail_samples: AtomicU32::new(384_000),
    }));
    // SAFETY: just made; the host gets the pointer to its `clap` field and
    // hands it back to `destroy`.
    unsafe {
        (*plugin).clap.plugin_data = plugin as *mut c_void;
        &(*plugin).clap
    }
}

unsafe extern "C" fn init(_plugin: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn destroy(plugin: *const clap_plugin) {
    // SAFETY: made by `create` with `Box::into_raw`; the host destroys it
    // once.
    unsafe { drop(Box::from_raw((*plugin).plugin_data as *mut Plugin)) };
}

unsafe extern "C" fn activate(
    plugin: *const clap_plugin,
    sample_rate: f64,
    _min_frames: u32,
    _max_frames: u32,
) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return false;
    }
    p.tail_samples.store(
        (sample_rate * f64::from(effects::shaper::MAX_TIME_SECONDS)).ceil() as u32,
        Ordering::Relaxed,
    );
    p.bridge().prepare(sample_rate);
    p.active.store(true, Ordering::Release);
    true
}

unsafe extern "C" fn deactivate(plugin: *const clap_plugin) {
    unsafe { Plugin::from(plugin) }
        .active
        .store(false, Ordering::Release);
}

unsafe extern "C" fn start_processing(_plugin: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn stop_processing(_plugin: *const clap_plugin) {}

unsafe extern "C" fn reset(plugin: *const clap_plugin) {
    unsafe { Plugin::from(plugin) }.bridge().reset();
}

unsafe extern "C" fn on_main_thread(plugin: *const clap_plugin) {
    let p = unsafe { Plugin::from(plugin) };
    if let Ok(gui) = p.gui.try_borrow()
        && let Some(gui) = gui.as_ref()
    {
        gui.pump();
    }
    p.close_gui_if_requested();
    p.host.flush(&p.edits);
}

/// The host's events, as the bridge's. Events of other spaces, and kinds
/// an instrument has no use for (note expressions but tuning, SysEx, MIDI
/// 2), are skipped.
///
/// # Safety
/// `events` must be the host's valid input event list.
unsafe fn inputs(events: *const clap_input_events) -> impl Iterator<Item = Input> {
    let count = unsafe {
        events
            .as_ref()
            .and_then(|e| e.size.map(|size| size(events)))
            .unwrap_or(0)
    };
    (0..count).filter_map(move |index| {
        // SAFETY: the list is valid for the call, and `index < size`.
        unsafe {
            let event = (*events).get?(events, index);
            input(event.as_ref()?)
        }
    })
}

/// # Safety
/// `header` must head an event of the size and type it says.
unsafe fn input(header: &clap_event_header) -> Option<Input> {
    if header.space_id != CLAP_CORE_EVENT_SPACE_ID {
        return None;
    }
    let frame = header.time;
    let event = header as *const clap_event_header;
    let key = |key: i16| u8::try_from(key).ok();
    // SAFETY: each type is the struct CLAP defines for it.
    unsafe {
        Some(match header.type_ {
            CLAP_EVENT_NOTE_ON => {
                let e = &*(event as *const clap_event_note);
                Input::NoteOn {
                    frame,
                    key: key(e.key)?,
                    velocity: e.velocity,
                }
            }
            CLAP_EVENT_NOTE_OFF => {
                let e = &*(event as *const clap_event_note);
                Input::NoteOff {
                    frame,
                    key: key(e.key),
                    velocity: e.velocity,
                }
            }
            CLAP_EVENT_NOTE_CHOKE => {
                let e = &*(event as *const clap_event_note);
                Input::Choke {
                    frame,
                    key: key(e.key),
                }
            }
            CLAP_EVENT_NOTE_EXPRESSION => {
                let e = &*(event as *const clap_event_note_expression);
                if e.expression_id != CLAP_NOTE_EXPRESSION_TUNING {
                    return None;
                }
                Input::Tuning {
                    frame,
                    key: key(e.key),
                    semitones: e.value,
                }
            }
            CLAP_EVENT_MIDI => {
                let e = &*(event as *const clap_event_midi);
                Input::Midi {
                    frame,
                    data: e.data,
                }
            }
            CLAP_EVENT_PARAM_VALUE => {
                let e = &*(event as *const clap_event_param_value);
                Input::Param {
                    frame,
                    index: e.param_id as usize,
                    value: e.value,
                }
            }
            CLAP_EVENT_PARAM_MOD => {
                let e = &*(event as *const clap_event_param_mod);
                Input::ParamMod {
                    frame,
                    index: e.param_id as usize,
                    amount: e.amount,
                }
            }
            _ => return None,
        })
    }
}

fn transport(t: &clap_event_transport) -> Transport {
    let has = |flag| t.flags & flag != 0;
    Transport {
        tempo: has(CLAP_TRANSPORT_HAS_TEMPO).then_some(t.tempo),
        beat: has(CLAP_TRANSPORT_HAS_BEATS_TIMELINE)
            .then(|| t.song_pos_beats as f64 / CLAP_BEATTIME_FACTOR as f64),
        playing: has(CLAP_TRANSPORT_IS_PLAYING),
    }
}

unsafe extern "C" fn process(
    plugin: *const clap_plugin,
    process: *const clap_process,
) -> clap_process_status {
    let p = unsafe { Plugin::from(plugin) };
    let process = unsafe { &*process };
    let frames = process.frames_count as usize;
    // One stereo output, as `audio_ports` says.
    let output = (process.audio_outputs_count > 0)
        .then(|| unsafe { &*process.audio_outputs })
        .filter(|o| o.channel_count >= 2 && !o.data32.is_null());
    let Some(output) = output else {
        return CLAP_PROCESS_ERROR;
    };
    // Copy inputs before making mutable output slices. Hosts may process the
    // stereo effect in place, so immutable input slices would alias them.
    if p.device.is_effect() {
        let Some(input) = (process.audio_inputs_count > 0)
            .then(|| unsafe { &*process.audio_inputs })
            .filter(|input| input.channel_count >= 2 && !input.data32.is_null())
        else {
            return CLAP_PROCESS_ERROR;
        };
        let (il, ir, ol, or) = unsafe {
            (
                *input.data32,
                *input.data32.add(1),
                *output.data32,
                *output.data32.add(1),
            )
        };
        if il.is_null()
            || ir.is_null()
            || ol.is_null()
            || or.is_null()
            || ol == or
            || (il == or && il != ol)
            || (ir == ol && ir != or)
        {
            return CLAP_PROCESS_ERROR;
        }
        unsafe {
            std::ptr::copy(il, ol, frames);
            std::ptr::copy(ir, or, frames);
        }
    }
    // SAFETY: the host's two distinct output channels of `frames` samples.
    let (left, right) = unsafe {
        let (left, right) = (*output.data32, *output.data32.add(1));
        if left.is_null() || right.is_null() || left == right {
            return CLAP_PROCESS_ERROR;
        }
        (
            std::slice::from_raw_parts_mut(left, frames),
            std::slice::from_raw_parts_mut(right, frames),
        )
    };
    let transport = unsafe { process.transport.as_ref() }.map(transport);
    let events = unsafe { inputs(process.in_events) };
    p.bridge().process(events, transport, left, right);
    unsafe {
        p.edits.emit(&p.values, process.out_events);
        (*process.audio_outputs).constant_mask = 0;
    };
    CLAP_PROCESS_CONTINUE
}

static GUI_EXT: clap_plugin_gui = clap_plugin_gui {
    is_api_supported: Some(gui_supported),
    get_preferred_api: Some(gui_preferred),
    create: Some(gui_create),
    destroy: Some(gui_destroy),
    set_scale: Some(gui_scale),
    get_size: Some(gui_size),
    can_resize: Some(gui_can_resize),
    get_resize_hints: Some(gui_hints),
    adjust_size: Some(gui_adjust),
    set_size: Some(gui_set_size),
    set_parent: Some(gui_parent),
    set_transient: Some(gui_transient),
    suggest_title: Some(gui_title),
    show: Some(gui_show),
    hide: Some(gui_hide),
};

unsafe extern "C" fn gui_supported(
    plugin: *const clap_plugin,
    api: *const c_char,
    floating: bool,
) -> bool {
    !floating
        && !api.is_null()
        && unsafe { Plugin::from(plugin) }.device.id() == "canopy.shaper"
        && unsafe { CStr::from_ptr(api) } == crate::gui::native_api()
}
unsafe extern "C" fn gui_preferred(
    _plugin: *const clap_plugin,
    api: *mut *const c_char,
    floating: *mut bool,
) -> bool {
    if api.is_null() || floating.is_null() {
        return false;
    }
    unsafe {
        *api = crate::gui::native_api().as_ptr();
        *floating = false;
    }
    true
}
unsafe extern "C" fn gui_create(
    plugin: *const clap_plugin,
    api: *const c_char,
    floating: bool,
) -> bool {
    if !unsafe { gui_supported(plugin, api, floating) } {
        return false;
    }
    let p = unsafe { Plugin::from(plugin) };
    p.close_gui_if_requested();
    let Ok(mut gui) = p.gui.try_borrow_mut() else {
        return false;
    };
    if gui.is_some() {
        return false;
    }
    *gui = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        NativeGui::create(p.host, p.values.clone(), p.display.clone(), p.edits.clone())
    }))
    .ok()
    .flatten();
    gui.is_some()
}
unsafe extern "C" fn gui_destroy(plugin: *const clap_plugin) {
    let p = unsafe { Plugin::from(plugin) };
    // A host may destroy its editor reentrantly from baseview's closed callback.
    p.gui_destroy_requested.set(true);
    p.close_gui_if_requested();
    p.host.request_main();
}
unsafe extern "C" fn gui_scale(_plugin: *const clap_plugin, scale: f64) -> bool {
    // Native DPI scaling is handled by baseview. Additional host zoom is not advertised.
    scale == 1.0
}
unsafe extern "C" fn gui_size(
    plugin: *const clap_plugin,
    width: *mut u32,
    height: *mut u32,
) -> bool {
    if width.is_null() || height.is_null() {
        return false;
    }
    let p = unsafe { Plugin::from(plugin) };
    let size = p
        .gui
        .try_borrow()
        .ok()
        .and_then(|g| g.as_ref().map(NativeGui::size))
        .unwrap_or((crate::gui::WIDTH, crate::gui::HEIGHT));
    unsafe {
        *width = size.0;
        *height = size.1;
    }
    true
}
unsafe extern "C" fn gui_can_resize(_plugin: *const clap_plugin) -> bool {
    true
}
unsafe extern "C" fn gui_hints(
    _plugin: *const clap_plugin,
    hints: *mut clap_gui_resize_hints,
) -> bool {
    let Some(hints) = (unsafe { hints.as_mut() }) else {
        return false;
    };
    *hints = clap_gui_resize_hints {
        can_resize_horizontally: true,
        can_resize_vertically: true,
        preserve_aspect_ratio: false,
        aspect_ratio_width: 0,
        aspect_ratio_height: 0,
    };
    true
}
unsafe extern "C" fn gui_adjust(
    plugin: *const clap_plugin,
    width: *mut u32,
    height: *mut u32,
) -> bool {
    if width.is_null() || height.is_null() {
        return false;
    }
    let p = unsafe { Plugin::from(plugin) };
    let Ok(gui) = p.gui.try_borrow() else {
        return false;
    };
    let Some(gui) = gui.as_ref() else {
        return false;
    };
    let adjusted = gui.adjust_size(unsafe { *width }, unsafe { *height });
    unsafe {
        *width = adjusted.0;
        *height = adjusted.1;
    }
    true
}
unsafe extern "C" fn gui_set_size(plugin: *const clap_plugin, width: u32, height: u32) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    p.gui.try_borrow().ok().is_some_and(|gui| {
        gui.as_ref().is_some_and(|g| {
            g.adjust_size(width, height) == (width, height) && g.resize(width, height)
        })
    })
}
unsafe extern "C" fn gui_parent(plugin: *const clap_plugin, parent: *const clap_window) -> bool {
    let Some(parent) = (unsafe { parent.as_ref() }) else {
        return false;
    };
    let p = unsafe { Plugin::from(plugin) };
    p.gui
        .try_borrow()
        .ok()
        .is_some_and(|gui| gui.as_ref().is_some_and(|g| unsafe { g.parent(parent) }))
}
unsafe extern "C" fn gui_transient(
    _plugin: *const clap_plugin,
    _window: *const clap_window,
) -> bool {
    false
}
unsafe extern "C" fn gui_title(_plugin: *const clap_plugin, _title: *const c_char) {}
unsafe extern "C" fn gui_show(plugin: *const clap_plugin) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    p.gui
        .try_borrow()
        .ok()
        .is_some_and(|gui| gui.as_ref().is_some_and(NativeGui::show))
}
unsafe extern "C" fn gui_hide(plugin: *const clap_plugin) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    p.gui
        .try_borrow()
        .ok()
        .is_some_and(|gui| gui.as_ref().is_some_and(NativeGui::hide))
}

unsafe extern "C" fn get_extension(plugin: *const clap_plugin, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return null();
    }
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_GUI && unsafe { Plugin::from(plugin) }.device.id() == "canopy.shaper" {
        &GUI_EXT as *const _ as *const c_void
    } else if id == clap_sys::ext::tail::CLAP_EXT_TAIL
        && unsafe { Plugin::from(plugin) }.device.id() == "canopy.shaper"
    {
        &TAIL_EXT as *const _ as *const c_void
    } else if id == CLAP_EXT_PARAMS {
        &PARAMS_EXT as *const _ as *const c_void
    } else if id == CLAP_EXT_STATE {
        &STATE_EXT as *const _ as *const c_void
    } else if id == CLAP_EXT_AUDIO_PORTS {
        &AUDIO_PORTS_EXT as *const _ as *const c_void
    } else if id == CLAP_EXT_NOTE_PORTS {
        &NOTE_PORTS_EXT as *const _ as *const c_void
    } else {
        null()
    }
}

/// Copies `src` into a C string buffer, cut short to fit (on a character
/// boundary) and terminated.
fn copy_str(dst: &mut [c_char], src: &str) {
    let Some(room) = dst.len().checked_sub(1) else {
        return;
    };
    let mut end = src.len().min(room);
    while !src.is_char_boundary(end) {
        end -= 1;
    }
    for (d, s) in dst.iter_mut().zip(src[..end].bytes().chain([0])) {
        *d = s as c_char;
    }
}

static PARAMS_EXT: clap_plugin_params = clap_plugin_params {
    count: Some(params_count),
    get_info: Some(params_get_info),
    get_value: Some(params_get_value),
    value_to_text: Some(params_value_to_text),
    text_to_value: Some(params_text_to_value),
    flush: Some(params_flush),
};

unsafe extern "C" fn params_count(plugin: *const clap_plugin) -> u32 {
    unsafe { Plugin::from(plugin) }.values.len() as u32
}

/// Parameter ids are indices into the instrument's parameters, which
/// only ever grow at the end, so a host's saved automation stays put.
unsafe extern "C" fn params_get_info(
    plugin: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    let Some(param) = p.device.params().get(index as usize) else {
        return false;
    };
    let info = unsafe { &mut *info };
    let (min, max) = range(param);
    info.id = index;
    info.flags = CLAP_PARAM_IS_AUTOMATABLE
        | match param.kind {
            ParameterKind::Float { .. } => CLAP_PARAM_IS_MODULATABLE,
            ParameterKind::Enum { .. } => CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_ENUM,
        };
    info.cookie = std::ptr::null_mut();
    copy_str(&mut info.name, param.name);
    let module = match p.device.groups(index as usize) {
        ["", _] => String::new(),
        [outer, ""] => outer.to_owned(),
        [outer, inner] => format!("{outer}/{inner}"),
    };
    copy_str(&mut info.module, &module);
    info.min_value = min;
    info.max_value = max;
    info.default_value = to_host(param, param.default_value());
    true
}

unsafe extern "C" fn params_get_value(
    plugin: *const clap_plugin,
    id: clap_id,
    value: *mut f64,
) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    match p.values.get(id as usize) {
        Some(v) => {
            unsafe { *value = v };
            true
        }
        None => false,
    }
}

unsafe extern "C" fn params_value_to_text(
    plugin: *const clap_plugin,
    id: clap_id,
    value: f64,
    buffer: *mut c_char,
    capacity: u32,
) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    let Some(param) = p.device.params().get(id as usize) else {
        return false;
    };
    if buffer.is_null() {
        return false;
    }
    let buffer = unsafe { std::slice::from_raw_parts_mut(buffer, capacity as usize) };
    copy_str(buffer, &param.format(to_plain(param, value)));
    true
}

unsafe extern "C" fn params_text_to_value(
    plugin: *const clap_plugin,
    id: clap_id,
    text: *const c_char,
    value: *mut f64,
) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    let Some(param) = p.device.params().get(id as usize) else {
        return false;
    };
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    match param.parse(&text) {
        Some(plain) => {
            unsafe { *value = to_host(param, plain) };
            true
        }
        None => false,
    }
}

/// Inactive, on the main thread: values are kept for the next activation.
/// Active, on the audio thread between blocks: the instrument takes them
/// at once.
unsafe extern "C" fn params_flush(
    plugin: *const clap_plugin,
    in_events: *const clap_input_events,
    out: *const clap_output_events,
) {
    let p = unsafe { Plugin::from(plugin) };
    let events = unsafe { inputs(in_events) };
    if p.active.load(Ordering::Acquire) {
        p.bridge().flush(events);
    } else {
        for input in events {
            if let Input::Param { index, value, .. } = input {
                p.values.set(index, value);
            }
        }
    }
    unsafe { p.edits.emit(&p.values, out) };
}

static STATE_EXT: clap_plugin_state = clap_plugin_state {
    save: Some(state_save),
    load: Some(state_load),
};

unsafe extern "C" fn state_save(plugin: *const clap_plugin, stream: *const clap_ostream) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    let bytes = state::save_device(
        p.device.id(),
        p.device.version(),
        p.device.params(),
        &p.values,
    );
    let mut written = 0;
    while written < bytes.len() {
        let rest = &bytes[written..];
        let n = unsafe {
            match (*stream).write {
                Some(write) => write(stream, rest.as_ptr() as *const c_void, rest.len() as u64),
                None => return false,
            }
        };
        if n <= 0 {
            return false;
        }
        written += n as usize;
    }
    true
}

/// Values loaded while active reach the instrument at its next block.
unsafe extern "C" fn state_load(plugin: *const clap_plugin, stream: *const clap_istream) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = unsafe {
            match (*stream).read {
                Some(read) => read(
                    stream,
                    chunk.as_mut_ptr() as *mut c_void,
                    chunk.len() as u64,
                ),
                None => return false,
            }
        };
        match n {
            0 => break,
            n if n < 0 => return false,
            n => bytes.extend_from_slice(&chunk[..n as usize]),
        }
    }
    state::load_device(p.device.id(), p.device.params(), &p.values, &bytes).is_ok()
}

static AUDIO_PORTS_EXT: clap_plugin_audio_ports = clap_plugin_audio_ports {
    count: Some(audio_ports_count),
    get: Some(audio_ports_get),
};

unsafe extern "C" fn audio_ports_count(plugin: *const clap_plugin, is_input: bool) -> u32 {
    u32::from(!is_input || unsafe { Plugin::from(plugin) }.device.is_effect())
}

unsafe extern "C" fn audio_ports_get(
    plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    let effect = unsafe { Plugin::from(plugin) }.device.is_effect();
    if (is_input && !effect) || index != 0 {
        return false;
    }
    let info = unsafe { &mut *info };
    info.id = 0;
    copy_str(&mut info.name, if is_input { "Input" } else { "Output" });
    info.flags = CLAP_AUDIO_PORT_IS_MAIN;
    info.channel_count = 2;
    info.port_type = CLAP_PORT_STEREO.as_ptr();
    info.in_place_pair = if effect { 0 } else { u32::MAX };
    true
}

static NOTE_PORTS_EXT: clap_plugin_note_ports = clap_plugin_note_ports {
    count: Some(note_ports_count),
    get: Some(note_ports_get),
};

unsafe extern "C" fn note_ports_count(plugin: *const clap_plugin, is_input: bool) -> u32 {
    u32::from(is_input && !unsafe { Plugin::from(plugin) }.device.is_effect())
}

unsafe extern "C" fn note_ports_get(
    plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_note_port_info,
) -> bool {
    if !is_input || index != 0 || unsafe { Plugin::from(plugin) }.device.is_effect() {
        return false;
    }
    let info = unsafe { &mut *info };
    info.id = 0;
    info.supported_dialects = CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI;
    info.preferred_dialect = CLAP_NOTE_DIALECT_CLAP;
    copy_str(&mut info.name, "Notes");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "effects-only")]
    fn shaper_accepts_in_place_stereo_and_reports_its_ports_and_tail() {
        use clap_sys::audio_buffer::clap_audio_buffer;
        unsafe {
            let descriptor = &crate::entries()[0].descriptor;
            let plugin = create(
                Device::Effect(&effects::shaper::DESCRIPTOR),
                descriptor,
                null(),
            );
            assert_eq!(audio_ports_count(plugin, true), 1);
            assert_eq!(audio_ports_count(plugin, false), 1);
            assert_eq!(note_ports_count(plugin, true), 0);
            let p = Plugin::from(plugin);
            for module in 0..effects::shaper::MODULES {
                p.values
                    .send_plain(effects::shaper::param(module, effects::shaper::ON), 0.0);
            }
            assert!(activate(plugin, 96_000.0, 1, 127));
            assert_eq!(tail_get(plugin), 768_000);
            let mut left = [0.3; 127];
            let mut right = [-0.7; 127];
            let mut channels = [left.as_mut_ptr(), right.as_mut_ptr()];
            let mut buffer = clap_audio_buffer {
                data32: channels.as_mut_ptr(),
                data64: std::ptr::null_mut(),
                channel_count: 2,
                latency: 0,
                constant_mask: 3,
            };
            let block = clap_process {
                steady_time: 0,
                frames_count: 127,
                transport: null(),
                audio_inputs: &buffer,
                audio_outputs: &mut buffer,
                audio_inputs_count: 1,
                audio_outputs_count: 1,
                in_events: null(),
                out_events: null(),
            };
            assert_eq!(process(plugin, &block), CLAP_PROCESS_CONTINUE);
            assert!(left.iter().all(|s| (*s - 0.3).abs() < 1e-6));
            assert!(right.iter().all(|s| (*s + 0.7).abs() < 1e-6));
            assert_eq!(buffer.constant_mask, 0);
            deactivate(plugin);
            destroy(plugin);
        }
    }

    #[test]
    fn copied_strings_are_cut_on_a_character_boundary_and_terminated() {
        let mut buffer = [1 as c_char; 5];
        copy_str(&mut buffer, "ab\u{e9}cd");
        let text = unsafe { CStr::from_ptr(buffer.as_ptr()) };
        assert_eq!(text.to_str(), Ok("ab\u{e9}"));
        let mut buffer = [1 as c_char; 4];
        copy_str(&mut buffer, "ab\u{e9}");
        assert_eq!(
            unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_str(),
            Ok("ab")
        );
    }
}

// A fixed conservative tail keeps Time-module history audible in offline
// renders even when its range/offset are automated. No audio-thread callbacks
// or GUI-driven changes to the declared tail are needed.
static TAIL_EXT: clap_sys::ext::tail::clap_plugin_tail = clap_sys::ext::tail::clap_plugin_tail {
    get: Some(tail_get),
};
unsafe extern "C" fn tail_get(plugin: *const clap_plugin) -> u32 {
    unsafe { Plugin::from(plugin) }
        .tail_samples
        .load(Ordering::Relaxed)
}
