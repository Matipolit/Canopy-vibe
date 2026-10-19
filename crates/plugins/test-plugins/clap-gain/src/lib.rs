//! A tiny CLAP plugin for the `plugins` crate's tests, on the raw C ABI
//! (`clap-sys`): one file with two plugins.
//!
//! - `org.canopy.test.gain`: stereo in (plus a stereo sidechain, added to
//!   the input) and out, with a note input. Its output is its input
//!   delayed by "Latency" samples and scaled by "Gain" (plus any
//!   `PARAM_MOD` on it), changing at the exact sample of each event. It
//!   reports "Latency" as its latency and asks for a restart when it
//!   changes while active. MIDI CC 7 makes it set its own Gain the way an
//!   editor would (gesture begin, value, gesture end as output events).
//!   Read-only parameters echo what it saw: the last `PARAM_MOD` amount,
//!   the transport's tempo, and how many calls came on the wrong thread
//!   (as the host's thread-check extension tells it).
//! - `org.canopy.test.sine`: a sine per held key, velocity-scaled, through
//!   the same Gain. It sends every note it gets back out on its note
//!   output an octave up, as a note effect would.
//!
//! On Linux both have a small X11 editor (`gui`), run from the host's
//! timers and file-descriptor watching.
//!
//! State is the parameters' values. `CANOPY_TEST_CLAP_CRASH` set in the
//! environment makes the entry crash, for testing scanners.

#![allow(clippy::missing_safety_doc)]

#[cfg(target_os = "linux")]
mod gui;

use std::ffi::{CStr, c_char, c_void};
use std::ptr::null;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_host_latency, clap_plugin_latency};
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::ext::thread_check::{CLAP_EXT_THREAD_CHECK, clap_host_thread_check};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::*;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;

const GAIN: clap_id = 0;
const LATENCY: clap_id = 1;
const MOD_SEEN: clap_id = 2;
const TEMPO_SEEN: clap_id = 3;
const THREAD_ERRORS: clap_id = 5;
const GUI_TICKS: clap_id = 7;
const GUI_FD_EVENTS: clap_id = 8;
const GUI_PARENT_WIDTH: clap_id = 9;
const GUI_OPEN: clap_id = 10;
const PARAMS: [clap_id; 9] = [
    GAIN,
    LATENCY,
    MOD_SEEN,
    TEMPO_SEEN,
    THREAD_ERRORS,
    GUI_TICKS,
    GUI_FD_EVENTS,
    GUI_PARENT_WIDTH,
    GUI_OPEN,
];

const MAX_LATENCY: usize = 1024;
const STATE_MAGIC: &[u8; 8] = b"CNPYCLAP";

struct Descriptor(clap_plugin_descriptor);
// SAFETY: the descriptors only point at static strings.
unsafe impl Sync for Descriptor {}

struct Features([*const c_char; 3]);
// SAFETY: as above.
unsafe impl Sync for Features {}

static GAIN_FEATURES: Features = Features([c"audio-effect".as_ptr(), c"stereo".as_ptr(), null()]);
static SINE_FEATURES: Features =
    Features([c"instrument".as_ptr(), c"synthesizer".as_ptr(), null()]);

const fn descriptor(
    id: &'static CStr,
    name: &'static CStr,
    features: &'static Features,
) -> Descriptor {
    Descriptor(clap_plugin_descriptor {
        clap_version: CLAP_VERSION,
        id: id.as_ptr(),
        name: name.as_ptr(),
        vendor: c"canopy".as_ptr(),
        url: c"".as_ptr(),
        manual_url: c"".as_ptr(),
        support_url: c"".as_ptr(),
        version: c"1.0.0".as_ptr(),
        description: c"A plugin for canopy's tests".as_ptr(),
        features: features.0.as_ptr(),
    })
}

static GAIN_DESCRIPTOR: Descriptor =
    descriptor(c"org.canopy.test.gain", c"Canopy Test Gain", &GAIN_FEATURES);
static SINE_DESCRIPTOR: Descriptor =
    descriptor(c"org.canopy.test.sine", c"Canopy Test Sine", &SINE_FEATURES);

/// A parameter's value, readable from any thread.
struct Value(AtomicU64);

impl Value {
    fn new(v: f64) -> Value {
        Value(AtomicU64::new(v.to_bits()))
    }
    fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
    fn set(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
}

/// What the audio thread keeps between blocks.
struct Audio {
    delay: Vec<[f32; 2]>,
    write: usize,
    /// Each key's phase (0 to 1) and amplitude, if held.
    voices: [Option<(f64, f64)>; 128],
    sample_rate: f64,
    gain_mod: f64,
    last_tempo: f64,
}

struct Plugin {
    clap: clap_plugin,
    host: *const clap_host,
    instrument: bool,
    gain: Value,
    latency: Value,
    mod_seen: Value,
    tempo_seen: Value,
    thread_errors: AtomicU64,
    /// The latency this activation runs with.
    active_latency: AtomicU64,
    active: AtomicBool,
    audio: Mutex<Audio>,
    #[cfg(target_os = "linux")]
    gui: Mutex<Option<gui::Gui>>,
    gui_ticks: AtomicU64,
    gui_fd_events: AtomicU64,
    gui_parent_width: AtomicU64,
    gui_open: AtomicBool,
}

impl Plugin {
    /// # Safety
    /// `plugin` must be one of ours.
    unsafe fn from<'a>(plugin: *const clap_plugin) -> &'a Plugin {
        unsafe { &*((*plugin).plugin_data as *const Plugin) }
    }

    fn host_ext<T>(&self, id: &CStr) -> Option<&T> {
        // SAFETY: the host pointer lives as long as we do.
        unsafe {
            let get = (*self.host).get_extension?;
            (get(self.host, id.as_ptr()) as *const T).as_ref()
        }
    }

    /// Counts a call made on the wrong thread.
    fn expect_thread(&self, audio: bool) {
        let Some(check) = self.host_ext::<clap_host_thread_check>(CLAP_EXT_THREAD_CHECK) else {
            return;
        };
        // SAFETY: thread-check functions may be called from any thread.
        let right = unsafe {
            if audio {
                check.is_audio_thread.is_some_and(|f| f(self.host))
            } else {
                check.is_main_thread.is_some_and(|f| f(self.host))
            }
        };
        if !right {
            self.thread_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn value(&self, id: clap_id) -> Option<&Value> {
        match id {
            GAIN => Some(&self.gain),
            LATENCY => Some(&self.latency),
            MOD_SEEN => Some(&self.mod_seen),
            TEMPO_SEEN => Some(&self.tempo_seen),
            _ => None,
        }
    }

    fn set_param(&self, id: clap_id, value: f64) {
        match id {
            GAIN => self.gain.set(value.clamp(-60.0, 12.0)),
            LATENCY => {
                let value = value.round().clamp(0.0, MAX_LATENCY as f64);
                let changed = value != self.latency.get();
                self.latency.set(value);
                if changed && self.active.load(Ordering::Relaxed) {
                    // SAFETY: request_restart is thread-safe.
                    unsafe {
                        if let Some(restart) = (*self.host).request_restart {
                            restart(self.host);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn db(gain: f64) -> f32 {
    10f64.powf(gain / 20.0) as f32
}

unsafe extern "C" fn init(plugin: *const clap_plugin) -> bool {
    unsafe { Plugin::from(plugin) }.expect_thread(false);
    true
}

unsafe extern "C" fn destroy(plugin: *const clap_plugin) {
    unsafe {
        Plugin::from(plugin).expect_thread(false);
        drop(Box::from_raw((*plugin).plugin_data as *mut Plugin));
    }
}

unsafe extern "C" fn activate(
    plugin: *const clap_plugin,
    sample_rate: f64,
    _min: u32,
    max: u32,
) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    let latency = p.latency.get() as u64;
    if latency != p.active_latency.swap(latency, Ordering::Relaxed)
        && let Some(host_latency) = p.host_ext::<clap_host_latency>(CLAP_EXT_LATENCY)
        && let Some(changed) = host_latency.changed
    {
        // SAFETY: latency.changed is allowed while activating.
        unsafe { changed(p.host) };
    }
    let mut audio = p.audio.lock().unwrap();
    audio.delay = vec![[0.0; 2]; MAX_LATENCY + max as usize + 1];
    audio.write = 0;
    audio.sample_rate = sample_rate;
    audio.voices = [None; 128];
    p.active.store(true, Ordering::Relaxed);
    true
}

unsafe extern "C" fn deactivate(plugin: *const clap_plugin) {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    p.active.store(false, Ordering::Relaxed);
}

unsafe extern "C" fn start_processing(plugin: *const clap_plugin) -> bool {
    unsafe { Plugin::from(plugin) }.expect_thread(true);
    true
}

unsafe extern "C" fn stop_processing(plugin: *const clap_plugin) {
    unsafe { Plugin::from(plugin) }.expect_thread(true);
}

unsafe extern "C" fn reset(plugin: *const clap_plugin) {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(true);
    let mut audio = p.audio.lock().unwrap();
    audio.delay.fill([0.0; 2]);
    audio.voices = [None; 128];
}

/// Pushes an output event (ours are all small structs with a header).
unsafe fn push<T>(out: *const clap_output_events, event: &T) {
    unsafe {
        if let Some(try_push) = (*out).try_push {
            try_push(out, event as *const T as *const clap_event_header);
        }
    }
}

fn param_out(id: clap_id, value: f64, time: u32) -> clap_event_param_value {
    clap_event_param_value {
        header: header(
            CLAP_EVENT_PARAM_VALUE,
            time,
            size_of::<clap_event_param_value>(),
        ),
        param_id: id,
        cookie: std::ptr::null_mut(),
        note_id: -1,
        port_index: -1,
        channel: -1,
        key: -1,
        value,
    }
}

fn gesture(type_: u16, id: clap_id, time: u32) -> clap_event_param_gesture {
    clap_event_param_gesture {
        header: header(type_, time, size_of::<clap_event_param_gesture>()),
        param_id: id,
    }
}

fn header(type_: u16, time: u32, size: usize) -> clap_event_header {
    clap_event_header {
        size: size as u32,
        time,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    }
}

/// Applies one input event; `out` gets what the plugin says back.
unsafe fn apply(
    p: &Plugin,
    audio: &mut Audio,
    event: *const clap_event_header,
    out: *const clap_output_events,
) {
    unsafe {
        let header = &*event;
        if header.space_id != CLAP_CORE_EVENT_SPACE_ID {
            return;
        }
        match header.type_ {
            CLAP_EVENT_PARAM_VALUE => {
                let e = &*(event as *const clap_event_param_value);
                p.set_param(e.param_id, e.value);
            }
            CLAP_EVENT_PARAM_MOD => {
                let e = &*(event as *const clap_event_param_mod);
                if e.param_id == GAIN {
                    audio.gain_mod = e.amount;
                    p.mod_seen.set(e.amount);
                    if !out.is_null() {
                        push(out, &param_out(MOD_SEEN, e.amount, header.time));
                    }
                }
            }
            CLAP_EVENT_NOTE_ON | CLAP_EVENT_NOTE_OFF => {
                let e = &*(event as *const clap_event_note);
                if let Some(voice) = audio.voices.get_mut(e.key as usize) {
                    *voice = (header.type_ == CLAP_EVENT_NOTE_ON).then_some((0.0, e.velocity));
                }
                // The instrument also plays each note an octave up on its
                // note output, like a note effect would.
                if p.instrument && !out.is_null() && e.key + 12 < 128 {
                    let echo = clap_event_note {
                        key: e.key + 12,
                        port_index: 0,
                        ..*e
                    };
                    push(out, &echo);
                }
            }
            CLAP_EVENT_MIDI => {
                let e = &*(event as *const clap_event_midi);
                let [status, a, b] = e.data;
                match status & 0xF0 {
                    0x90 if b > 0 => {
                        audio.voices[a as usize & 127] = Some((0.0, f64::from(b) / 127.0))
                    }
                    0x80 | 0x90 => audio.voices[a as usize & 127] = None,
                    // CC 7: the plugin sets its own gain, as its editor would.
                    0xB0 if a == 7 && !out.is_null() => {
                        let gain = -60.0 + 72.0 * f64::from(b) / 127.0;
                        p.gain.set(gain);
                        push(
                            out,
                            &gesture(CLAP_EVENT_PARAM_GESTURE_BEGIN, GAIN, header.time),
                        );
                        push(out, &param_out(GAIN, gain, header.time));
                        push(
                            out,
                            &gesture(CLAP_EVENT_PARAM_GESTURE_END, GAIN, header.time),
                        );
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

unsafe extern "C" fn process(
    plugin: *const clap_plugin,
    process: *const clap_process,
) -> clap_process_status {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(true);
    let process = unsafe { &*process };
    let mut audio = p.audio.lock().unwrap();
    let frames = process.frames_count as usize;
    if let Some(transport) = unsafe { process.transport.as_ref() }
        && transport.flags & CLAP_TRANSPORT_HAS_TEMPO != 0
        && transport.tempo != audio.last_tempo
    {
        audio.last_tempo = transport.tempo;
        p.tempo_seen.set(transport.tempo);
        unsafe {
            push(
                process.out_events,
                &param_out(TEMPO_SEEN, transport.tempo, 0),
            )
        };
    }
    let channel =
        |buffers: *const clap_audio_buffer, count: u32, port: usize, ch: usize| -> *mut f32 {
            if port >= count as usize {
                return std::ptr::null_mut();
            }
            // SAFETY: the host's buffers for `count` ports.
            unsafe {
                let buffer = &*buffers.add(port);
                if ch >= buffer.channel_count as usize || buffer.data32.is_null() {
                    return std::ptr::null_mut();
                }
                *buffer.data32.add(ch)
            }
        };
    let input = |port, ch| channel(process.audio_inputs, process.audio_inputs_count, port, ch);
    let output = |ch| channel(process.audio_outputs, process.audio_outputs_count, 0, ch);
    let in_events = unsafe { &*process.in_events };
    let count = unsafe { in_events.size.map_or(0, |size| size(in_events)) };
    let mut next = 0;
    let latency = p.active_latency.load(Ordering::Relaxed) as usize;
    let len = audio.delay.len();
    for i in 0..frames {
        while next < count {
            let event = unsafe { in_events.get.map_or(null(), |get| get(in_events, next)) };
            if event.is_null() || unsafe { (*event).time } as usize > i {
                break;
            }
            unsafe { apply(p, &mut audio, event, process.out_events) };
            next += 1;
        }
        let gain = db((p.gain.get() + audio.gain_mod).clamp(-60.0, 12.0));
        let mut frame = [0.0f32; 2];
        if p.instrument {
            let rate = audio.sample_rate;
            for (key, voice) in audio.voices.iter_mut().enumerate() {
                if let Some((phase, velocity)) = voice {
                    let hz = 440.0 * ((key as f64 - 69.0) / 12.0).exp2();
                    let s = (*phase * std::f64::consts::TAU).sin() * *velocity;
                    frame[0] += s as f32;
                    frame[1] += s as f32;
                    *phase = (*phase + hz / rate).fract();
                }
            }
        } else {
            for (ch, sample) in frame.iter_mut().enumerate() {
                for port in 0..2 {
                    let ptr = input(port, ch);
                    if !ptr.is_null() {
                        // SAFETY: `frames` samples per channel.
                        *sample += unsafe { *ptr.add(i) };
                    }
                }
            }
            let write = audio.write;
            audio.delay[write] = frame;
            frame = audio.delay[(write + len - latency) % len];
            audio.write = (write + 1) % len;
        }
        for (ch, sample) in frame.iter().enumerate() {
            let ptr = output(ch);
            if !ptr.is_null() {
                // SAFETY: as above.
                unsafe { *ptr.add(i) = sample * gain };
            }
        }
    }
    // Events at or after the end (there shouldn't be any).
    while next < count {
        let event = unsafe { in_events.get.map_or(null(), |get| get(in_events, next)) };
        if !event.is_null() {
            unsafe { apply(p, &mut audio, event, process.out_events) };
        }
        next += 1;
    }
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
    } else if id == CLAP_EXT_LATENCY {
        &LATENCY_EXT as *const _ as *const c_void
    } else if id == CLAP_EXT_AUDIO_PORTS {
        &AUDIO_PORTS_EXT as *const _ as *const c_void
    } else if id == CLAP_EXT_NOTE_PORTS {
        &NOTE_PORTS_EXT as *const _ as *const c_void
    } else {
        gui_extension(id)
    }
}

unsafe extern "C" fn on_main_thread(plugin: *const clap_plugin) {
    unsafe { Plugin::from(plugin) }.expect_thread(false);
}

#[cfg(target_os = "linux")]
fn gui_extension(id: &CStr) -> *const c_void {
    use clap_sys::ext::gui::CLAP_EXT_GUI;
    use clap_sys::ext::posix_fd_support::CLAP_EXT_POSIX_FD_SUPPORT;
    use clap_sys::ext::timer_support::CLAP_EXT_TIMER_SUPPORT;
    if id == CLAP_EXT_GUI {
        &gui::GUI_EXT as *const _ as *const c_void
    } else if id == CLAP_EXT_TIMER_SUPPORT {
        &gui::TIMER_EXT as *const _ as *const c_void
    } else if id == CLAP_EXT_POSIX_FD_SUPPORT {
        &gui::FD_EXT as *const _ as *const c_void
    } else {
        null()
    }
}

#[cfg(not(target_os = "linux"))]
fn gui_extension(_id: &CStr) -> *const c_void {
    null()
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
    unsafe { Plugin::from(plugin) }.expect_thread(false);
    PARAMS.len() as u32
}

fn copy_name(dst: &mut [c_char], src: &str) {
    for (d, s) in dst.iter_mut().zip(src.bytes().chain(Some(0))) {
        *d = s as c_char;
    }
}

unsafe extern "C" fn params_get_info(
    plugin: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> bool {
    unsafe { Plugin::from(plugin) }.expect_thread(false);
    let Some(&id) = PARAMS.get(index as usize) else {
        return false;
    };
    let info = unsafe { &mut *info };
    let (name, min, max, default, flags) = match id {
        GAIN => (
            "Gain",
            -60.0,
            12.0,
            0.0,
            CLAP_PARAM_IS_AUTOMATABLE | CLAP_PARAM_IS_MODULATABLE,
        ),
        LATENCY => (
            "Latency",
            0.0,
            MAX_LATENCY as f64,
            0.0,
            CLAP_PARAM_IS_AUTOMATABLE | CLAP_PARAM_IS_STEPPED,
        ),
        MOD_SEEN => ("Mod seen", -100.0, 100.0, 0.0, CLAP_PARAM_IS_READONLY),
        TEMPO_SEEN => ("Tempo seen", 0.0, 1000.0, 0.0, CLAP_PARAM_IS_READONLY),
        GUI_TICKS => (
            "GUI ticks",
            0.0,
            1e9,
            0.0,
            CLAP_PARAM_IS_READONLY | CLAP_PARAM_IS_HIDDEN,
        ),
        GUI_FD_EVENTS => (
            "GUI events",
            0.0,
            1e9,
            0.0,
            CLAP_PARAM_IS_READONLY | CLAP_PARAM_IS_HIDDEN,
        ),
        GUI_PARENT_WIDTH => (
            "GUI parent width",
            0.0,
            1e9,
            0.0,
            CLAP_PARAM_IS_READONLY | CLAP_PARAM_IS_HIDDEN,
        ),
        GUI_OPEN => (
            "GUI open",
            0.0,
            1.0,
            0.0,
            CLAP_PARAM_IS_READONLY | CLAP_PARAM_IS_HIDDEN,
        ),
        _ => (
            "Thread errors",
            0.0,
            1e9,
            0.0,
            CLAP_PARAM_IS_READONLY | CLAP_PARAM_IS_HIDDEN,
        ),
    };
    info.id = id;
    info.flags = flags;
    info.cookie = std::ptr::null_mut();
    copy_name(&mut info.name, name);
    copy_name(&mut info.module, if id == GAIN { "Main" } else { "" });
    info.min_value = min;
    info.max_value = max;
    info.default_value = default;
    true
}

unsafe extern "C" fn params_get_value(
    plugin: *const clap_plugin,
    id: clap_id,
    value: *mut f64,
) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    let v = match id {
        THREAD_ERRORS => p.thread_errors.load(Ordering::Relaxed) as f64,
        GUI_TICKS => p.gui_ticks.load(Ordering::Relaxed) as f64,
        GUI_FD_EVENTS => p.gui_fd_events.load(Ordering::Relaxed) as f64,
        GUI_PARENT_WIDTH => p.gui_parent_width.load(Ordering::Relaxed) as f64,
        GUI_OPEN => f64::from(u8::from(p.gui_open.load(Ordering::Relaxed))),
        _ => match p.value(id) {
            Some(v) => v.get(),
            None => return false,
        },
    };
    unsafe { *value = v };
    true
}

unsafe extern "C" fn params_value_to_text(
    plugin: *const clap_plugin,
    id: clap_id,
    value: f64,
    buffer: *mut c_char,
    capacity: u32,
) -> bool {
    unsafe { Plugin::from(plugin) }.expect_thread(false);
    let text = match id {
        GAIN => format!("{value:.1} dB"),
        LATENCY => format!("{value:.0} samples"),
        _ => format!("{value}"),
    };
    let buffer = unsafe { std::slice::from_raw_parts_mut(buffer, capacity as usize) };
    let n = text.len().min(buffer.len().saturating_sub(1));
    copy_name(&mut buffer[..=n], &text[..n]);
    true
}

unsafe extern "C" fn params_text_to_value(
    plugin: *const clap_plugin,
    _id: clap_id,
    text: *const c_char,
    value: *mut f64,
) -> bool {
    unsafe { Plugin::from(plugin) }.expect_thread(false);
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    match text.split_whitespace().next().and_then(|t| t.parse().ok()) {
        Some(v) => {
            unsafe { *value = v };
            true
        }
        None => false,
    }
}

unsafe extern "C" fn params_flush(
    plugin: *const clap_plugin,
    in_events: *const clap_input_events,
    out: *const clap_output_events,
) {
    let p = unsafe { Plugin::from(plugin) };
    let active = p.active.load(Ordering::Relaxed);
    p.expect_thread(active);
    let mut audio = p.audio.lock().unwrap();
    let in_events = unsafe { &*in_events };
    let count = unsafe { in_events.size.map_or(0, |size| size(in_events)) };
    for i in 0..count {
        let event = unsafe { in_events.get.map_or(null(), |get| get(in_events, i)) };
        if !event.is_null() {
            unsafe { apply(p, &mut audio, event, out) };
        }
    }
}

static STATE_EXT: clap_plugin_state = clap_plugin_state {
    save: Some(state_save),
    load: Some(state_load),
};

unsafe extern "C" fn state_save(plugin: *const clap_plugin, stream: *const clap_ostream) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    let mut bytes = STATE_MAGIC.to_vec();
    bytes.extend_from_slice(&p.gain.get().to_le_bytes());
    bytes.extend_from_slice(&p.latency.get().to_le_bytes());
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

unsafe extern "C" fn state_load(plugin: *const clap_plugin, stream: *const clap_istream) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    let mut bytes = [0u8; 24];
    let mut read = 0;
    while read < bytes.len() {
        let rest = &mut bytes[read..];
        let n = unsafe {
            match (*stream).read {
                Some(f) => f(stream, rest.as_mut_ptr() as *mut c_void, rest.len() as u64),
                None => return false,
            }
        };
        if n <= 0 {
            return false;
        }
        read += n as usize;
    }
    if &bytes[..8] != STATE_MAGIC {
        return false;
    }
    p.gain
        .set(f64::from_le_bytes(bytes[8..16].try_into().unwrap()));
    p.set_param(
        LATENCY,
        f64::from_le_bytes(bytes[16..24].try_into().unwrap()),
    );
    true
}

static LATENCY_EXT: clap_plugin_latency = clap_plugin_latency {
    get: Some(latency_get),
};

unsafe extern "C" fn latency_get(plugin: *const clap_plugin) -> u32 {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    p.active_latency.load(Ordering::Relaxed) as u32
}

static AUDIO_PORTS_EXT: clap_plugin_audio_ports = clap_plugin_audio_ports {
    count: Some(audio_ports_count),
    get: Some(audio_ports_get),
};

unsafe extern "C" fn audio_ports_count(plugin: *const clap_plugin, is_input: bool) -> u32 {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    match (p.instrument, is_input) {
        (true, true) => 0,
        (false, true) => 2,
        (_, false) => 1,
    }
}

unsafe extern "C" fn audio_ports_get(
    plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    if index >= unsafe { audio_ports_count(plugin, is_input) } {
        return false;
    }
    let info = unsafe { &mut *info };
    info.id = index;
    let name = match (is_input, index) {
        (true, 0) => "Input",
        (true, _) => "Sidechain",
        _ => "Output",
    };
    copy_name(&mut info.name, name);
    info.flags = if index == 0 {
        CLAP_AUDIO_PORT_IS_MAIN
    } else {
        0
    };
    info.channel_count = 2;
    info.port_type = CLAP_PORT_STEREO.as_ptr();
    info.in_place_pair = u32::MAX;
    true
}

static NOTE_PORTS_EXT: clap_plugin_note_ports = clap_plugin_note_ports {
    count: Some(note_ports_count),
    get: Some(note_ports_get),
};

unsafe extern "C" fn note_ports_count(plugin: *const clap_plugin, is_input: bool) -> u32 {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    u32::from(is_input || p.instrument)
}

unsafe extern "C" fn note_ports_get(
    plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_note_port_info,
) -> bool {
    if index >= unsafe { note_ports_count(plugin, is_input) } {
        return false;
    }
    let info = unsafe { &mut *info };
    info.id = 0;
    info.supported_dialects = CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI;
    info.preferred_dialect = CLAP_NOTE_DIALECT_CLAP;
    copy_name(&mut info.name, if is_input { "Notes" } else { "Echo" });
    true
}

unsafe extern "C" fn factory_count(_factory: *const clap_plugin_factory) -> u32 {
    2
}

unsafe extern "C" fn factory_descriptor(
    _factory: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    match index {
        0 => &GAIN_DESCRIPTOR.0,
        1 => &SINE_DESCRIPTOR.0,
        _ => null(),
    }
}

unsafe extern "C" fn factory_create(
    _factory: *const clap_plugin_factory,
    host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    let id = unsafe { CStr::from_ptr(id) };
    let descriptor = if id == unsafe { CStr::from_ptr(GAIN_DESCRIPTOR.0.id) } {
        &GAIN_DESCRIPTOR.0
    } else if id == unsafe { CStr::from_ptr(SINE_DESCRIPTOR.0.id) } {
        &SINE_DESCRIPTOR.0
    } else {
        return null();
    };
    let plugin = Box::new(Plugin {
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
        host,
        instrument: std::ptr::eq(descriptor, &SINE_DESCRIPTOR.0),
        gain: Value::new(0.0),
        latency: Value::new(0.0),
        mod_seen: Value::new(0.0),
        tempo_seen: Value::new(0.0),
        thread_errors: AtomicU64::new(0),
        #[cfg(target_os = "linux")]
        gui: Mutex::new(None),
        gui_ticks: AtomicU64::new(0),
        gui_fd_events: AtomicU64::new(0),
        gui_parent_width: AtomicU64::new(0),
        gui_open: AtomicBool::new(false),
        active_latency: AtomicU64::new(0),
        active: AtomicBool::new(false),
        audio: Mutex::new(Audio {
            delay: Vec::new(),
            write: 0,
            voices: [None; 128],
            sample_rate: 48_000.0,
            gain_mod: 0.0,
            last_tempo: 0.0,
        }),
    });
    let plugin = Box::into_raw(plugin);
    unsafe {
        (*plugin).clap.plugin_data = plugin as *mut c_void;
        &(*plugin).clap
    }
}

struct Factory(clap_plugin_factory);
// SAFETY: only function pointers.
unsafe impl Sync for Factory {}

static FACTORY: Factory = Factory(clap_plugin_factory {
    get_plugin_count: Some(factory_count),
    get_plugin_descriptor: Some(factory_descriptor),
    create_plugin: Some(factory_create),
});

unsafe extern "C" fn entry_init(_path: *const c_char) -> bool {
    if std::env::var_os("CANOPY_TEST_CLAP_CRASH").is_some() {
        std::process::abort();
    }
    true
}

unsafe extern "C" fn entry_deinit() {}

unsafe extern "C" fn entry_get_factory(id: *const c_char) -> *const c_void {
    if unsafe { CStr::from_ptr(id) } == CLAP_PLUGIN_FACTORY_ID {
        &FACTORY.0 as *const _ as *const c_void
    } else {
        null()
    }
}

#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static clap_entry: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(entry_get_factory),
};
