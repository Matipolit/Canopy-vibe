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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::fixedpoint::CLAP_BEATTIME_FACTOR;
use clap_sys::id::clap_id;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::*;
use clap_sys::stream::{clap_istream, clap_ostream};
use instruments::{BuiltInInstrumentDescriptor, ParameterKind};

use crate::bridge::{Bridge, Input, Transport};
use crate::state;
use crate::values::{Values, range, to_host, to_plain};

struct Plugin {
    clap: clap_plugin,
    instrument: &'static BuiltInInstrumentDescriptor,
    values: Arc<Values>,
    bridge: Mutex<Bridge>,
    active: AtomicBool,
}

impl Plugin {
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
    instrument: &'static BuiltInInstrumentDescriptor,
    descriptor: &'static clap_plugin_descriptor,
) -> *const clap_plugin {
    let values = Arc::new(Values::new(instrument.params));
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
        instrument,
        bridge: Mutex::new(Bridge::new((instrument.create)(), values.clone())),
        values,
        active: AtomicBool::new(false),
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

unsafe extern "C" fn on_main_thread(_plugin: *const clap_plugin) {}

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
    // SAFETY: the host's two output channels of `frames` samples each,
    // distinct (the plugin has no inputs to share them with).
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
    CLAP_PROCESS_CONTINUE
}

unsafe extern "C" fn get_extension(
    _plugin: *const clap_plugin,
    id: *const c_char,
) -> *const c_void {
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_PARAMS {
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
    let Some(param) = p.instrument.params.get(index as usize) else {
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
    let module = match (p.instrument.groups)(index as usize) {
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
    let Some(param) = p.instrument.params.get(id as usize) else {
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
    let Some(param) = p.instrument.params.get(id as usize) else {
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
    _out: *const clap_output_events,
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
}

static STATE_EXT: clap_plugin_state = clap_plugin_state {
    save: Some(state_save),
    load: Some(state_load),
};

unsafe extern "C" fn state_save(plugin: *const clap_plugin, stream: *const clap_ostream) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    let bytes = state::save(p.instrument, &p.values);
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
    state::load(p.instrument, &p.values, &bytes).is_ok()
}

static AUDIO_PORTS_EXT: clap_plugin_audio_ports = clap_plugin_audio_ports {
    count: Some(audio_ports_count),
    get: Some(audio_ports_get),
};

unsafe extern "C" fn audio_ports_count(_plugin: *const clap_plugin, is_input: bool) -> u32 {
    u32::from(!is_input)
}

unsafe extern "C" fn audio_ports_get(
    _plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    if is_input || index != 0 {
        return false;
    }
    let info = unsafe { &mut *info };
    info.id = 0;
    copy_str(&mut info.name, "Output");
    info.flags = CLAP_AUDIO_PORT_IS_MAIN;
    info.channel_count = 2;
    info.port_type = CLAP_PORT_STEREO.as_ptr();
    info.in_place_pair = u32::MAX;
    true
}

static NOTE_PORTS_EXT: clap_plugin_note_ports = clap_plugin_note_ports {
    count: Some(note_ports_count),
    get: Some(note_ports_get),
};

unsafe extern "C" fn note_ports_count(_plugin: *const clap_plugin, is_input: bool) -> u32 {
    u32::from(is_input)
}

unsafe extern "C" fn note_ports_get(
    _plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_note_port_info,
) -> bool {
    if !is_input || index != 0 {
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
