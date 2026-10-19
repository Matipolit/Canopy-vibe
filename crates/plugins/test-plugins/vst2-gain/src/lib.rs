//! A tiny VST2 plugin for the `plugins` crate's tests, written against the
//! host's own description of the VST2 ABI (`src/vst2/abi.rs` in the
//! `plugins` crate, included here), so it needs no SDK.
//!
//! A VST2 file holds one plugin, so this one library is installed under
//! several names and becomes the plugin its file name says:
//!
//! - Canopy Test Gain (unique id `'CnyG'`, any other name): four inputs
//!   (stereo, plus a stereo sidechain added to it) and two outputs. Output
//!   is input delayed by "Latency" samples and scaled by "Gain". Latency is
//!   reported as `initialDelay` when resumed; changing it while resumed
//!   calls `audioMasterIOChanged`. State is a chunk (Gain and Latency).
//! - Canopy Test Sine (`'CnyS'`, a name containing "sine"): a sine per
//!   held key, velocity-scaled, through Gain. It sends every note it gets
//!   back out an octave up (`audioMasterProcessEvents`). Two programs, and
//!   no chunk: the host saves its parameters and program.
//! - A name containing "shell" makes the gain call itself a shell plugin,
//!   which hosts here refuse.
//!
//! Parameters (normalized): 0 Gain (-60 to +12 dB), 1 Latency (0 to 1024
//! samples), 2 Mod seen (unused), 3 Tempo seen (tempo / 1000: the
//! transport's tempo, reported with `audioMasterAutomate` from the audio
//! thread), 4 Reset (a switch: setting it makes the plugin set Gain back to
//! 0 dB the way its editor would, with begin and end edit), 5 Thread
//! errors, 6 Program; 7 to 10 report what the editor saw; 11 counts the
//! plugins the library made since it was loaded (so a test can tell it was
//! unloaded in between). 5 to 11 are raw numbers. MIDI CC 7 makes it set its own Gain as its editor would; a
//! program change switches programs; CC 120 and 123 silence it.
//!
//! "Thread errors" counts calls made on the wrong kind of thread, as the
//! host's `audioMasterGetCurrentProcessLevel` answers: processing and
//! events on the audio thread, everything else on the main thread.
//!
//! On Linux it has a small X11 editor run from `effEditIdle`: on its first
//! idle it asks the host for 400 × 250 (`audioMasterSizeWindow`), and it
//! watches the host's window to see that happen.

#![allow(clippy::missing_safety_doc, non_upper_case_globals)]

#[path = "../../../src/vst2/abi.rs"]
mod abi;
#[cfg(target_os = "linux")]
mod gui;

use std::cell::UnsafeCell;
use std::ffi::{CStr, c_char, c_void};
use std::ptr::null_mut;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

use abi::*;

const GAIN: i32 = 0;
const LATENCY: i32 = 1;
const MOD_SEEN: i32 = 2;
const TEMPO_SEEN: i32 = 3;
const RESET: i32 = 4;
const THREAD_ERRORS: i32 = 5;
const PROGRAM: i32 = 6;
const GUI_TICKS: i32 = 7;
const GUI_FD_EVENTS: i32 = 8;
const GUI_PARENT_WIDTH: i32 = 9;
const GUI_OPEN: i32 = 10;
const MADE: i32 = 11;
const PARAMS: i32 = 12;

const MAX_LATENCY: usize = 1024;
/// Gain's value at 0 dB.
const GAIN_DEFAULT: f32 = 60.0 / 72.0;
const CHUNK_MAGIC: &[u8; 8] = b"CNPYV2CK";

/// Plugins made since the library was loaded.
static MADE_COUNT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Gain,
    Sine,
    Shell,
}

impl Kind {
    /// The plugin the library's file name asks for.
    fn from_file_name(path: &str) -> Kind {
        let name = path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(path)
            .to_lowercase();
        if name.contains("sine") {
            Kind::Sine
        } else if name.contains("shell") {
            Kind::Shell
        } else {
            Kind::Gain
        }
    }
}

/// The file this library was loaded from.
#[cfg(unix)]
fn library_name() -> String {
    // SAFETY: dladdr fills `info` for an address inside a loaded object;
    // the name it points at lives as long as the object.
    unsafe {
        let mut info: libc::Dl_info = std::mem::zeroed();
        let address = VSTPluginMain as *const c_void;
        if libc::dladdr(address, &mut info) == 0 || info.dli_fname.is_null() {
            return String::new();
        }
        CStr::from_ptr(info.dli_fname)
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(windows)]
fn library_name() -> String {
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW,
    };
    // SAFETY: looks up the module containing this function, and its path
    // into a buffer of the size given.
    unsafe {
        let mut module = null_mut();
        let flags =
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
        if GetModuleHandleExW(flags, VSTPluginMain as *const u16, &mut module) == 0 {
            return String::new();
        }
        let mut buf = [0u16; 1024];
        let n = GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) as usize;
        String::from_utf16_lossy(&buf[..n.min(buf.len())])
    }
}

fn gain_linear(normalized: f32) -> f32 {
    10f32.powf((-60.0 + 72.0 * normalized.clamp(0.0, 1.0)) / 20.0)
}

fn latency_samples(normalized: f32) -> usize {
    (normalized.clamp(0.0, 1.0) * MAX_LATENCY as f32).round() as usize
}

/// Copies `text` into a host buffer of `len` bytes, NUL-terminated.
unsafe fn write(ptr: *mut c_void, len: usize, text: &str) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let n = text.len().min(len - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), ptr as *mut u8, n);
        *(ptr as *mut u8).add(n) = 0;
    }
}

/// What the audio thread keeps between blocks.
struct Audio {
    delay: Vec<[f32; 2]>,
    write: usize,
    latency: usize,
    /// Each key's phase (0 to 1) and amplitude, if held.
    voices: [Option<(f64, f32)>; 128],
    sample_rate: f64,
    max_block: usize,
    last_tempo: f64,
    /// MIDI for the next process call: offset and bytes.
    events: Vec<(i32, [u8; 3])>,
}

pub struct Plugin {
    effect: UnsafeCell<AEffect>,
    host: HostCallback,
    kind: Kind,
    /// Gain, Latency, Mod seen, Tempo seen, Reset, as f32 bits.
    values: [AtomicU32; 5],
    thread_errors: AtomicU64,
    program: AtomicI32,
    resumed: AtomicBool,
    audio: Mutex<Audio>,
    chunk: Mutex<Vec<u8>>,
    /// The editor's size, as `effEditGetRect` hands it out.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    rect: UnsafeCell<ERect>,
    #[cfg(target_os = "linux")]
    gui: Mutex<Option<gui::Gui>>,
    gui_ticks: AtomicU64,
    gui_fd_events: AtomicU64,
    gui_parent_width: AtomicU64,
    gui_open: AtomicBool,
}

impl Plugin {
    unsafe fn from<'a>(effect: *mut AEffect) -> &'a Plugin {
        unsafe { &*((*effect).object as *const Plugin) }
    }

    /// Calls the host.
    fn call(&self, opcode: i32, index: i32, value: isize, ptr: *mut c_void, opt: f32) -> isize {
        unsafe { (self.host)(self.effect.get(), opcode, index, value, ptr, opt) }
    }

    /// Counts a call made on the wrong kind of thread.
    fn expect_thread(&self, audio: bool) {
        let level = self.call(audioMasterGetCurrentProcessLevel, 0, 0, null_mut(), 0.0);
        let right = if audio {
            level == kVstProcessLevelRealtime || level == kVstProcessLevelOffline
        } else {
            level == kVstProcessLevelUser
        };
        if !right {
            self.thread_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn get(&self, index: i32) -> f32 {
        f32::from_bits(self.values[index as usize].load(Ordering::Relaxed))
    }

    fn store(&self, index: i32, value: f32) {
        self.values[index as usize].store(value.to_bits(), Ordering::Relaxed);
    }

    fn programs(&self) -> i32 {
        if self.kind == Kind::Sine { 2 } else { 1 }
    }

    /// Sets Gain the way an editor would: begin, value, end.
    fn edit_gain(&self, value: f32) {
        self.call(audioMasterBeginEdit, GAIN, 0, null_mut(), 0.0);
        self.store(GAIN, value);
        self.call(audioMasterAutomate, GAIN, 0, null_mut(), value);
        self.call(audioMasterEndEdit, GAIN, 0, null_mut(), 0.0);
    }

    fn set(&self, index: i32, value: f32) {
        match index {
            GAIN => self.store(GAIN, value),
            LATENCY => {
                let old = self.get(LATENCY);
                self.store(LATENCY, value);
                if self.resumed.load(Ordering::Relaxed)
                    && latency_samples(old) != latency_samples(value)
                {
                    self.call(audioMasterIOChanged, 0, 0, null_mut(), 0.0);
                }
            }
            RESET if value >= 0.5 => self.edit_gain(GAIN_DEFAULT),
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        match self.kind {
            Kind::Sine => "Canopy Test Sine",
            _ => "Canopy Test Gain",
        }
    }

    /// Sends one MIDI message out, during processing.
    fn send(&self, delta: i32, data: [u8; 3]) {
        #[repr(C)]
        struct One {
            num_events: i32,
            reserved: isize,
            events: [*mut VstEvent; 1],
        }
        let mut event = VstMidiEvent {
            type_: kVstMidiType,
            byte_size: size_of::<VstMidiEvent>() as i32,
            delta_frames: delta,
            flags: 0,
            note_length: 0,
            note_offset: 0,
            midi_data: [data[0], data[1], data[2], 0],
            detune: 0,
            note_off_velocity: 0,
            reserved1: 0,
            reserved2: 0,
        };
        let mut list = One {
            num_events: 1,
            reserved: 0,
            events: [&mut event as *mut VstMidiEvent as *mut VstEvent],
        };
        self.call(
            audioMasterProcessEvents,
            0,
            0,
            &mut list as *mut One as *mut c_void,
            0.0,
        );
    }

    /// Applies one MIDI message at offset `delta`.
    fn apply(&self, audio: &mut Audio, delta: i32, data: [u8; 3]) {
        let [status, a, b] = data;
        let key = a as usize & 127;
        match status & 0xF0 {
            0x90 | 0x80 => {
                let on = status & 0xF0 == 0x90 && b > 0;
                audio.voices[key] = on.then_some((0.0, f32::from(b) / 127.0));
                if self.kind == Kind::Sine && key + 12 < 128 {
                    self.send(delta, [status, a + 12, b]);
                }
            }
            0xB0 if a == 7 => self.edit_gain(f32::from(b) / 127.0),
            0xB0 if a == 120 || a == 123 => audio.voices = [None; 128],
            0xC0 if i32::from(a) < self.programs() => {
                self.program.store(i32::from(a), Ordering::Relaxed);
            }
            _ => {}
        }
    }

    fn chunk(&self) -> Vec<u8> {
        let mut bytes = CHUNK_MAGIC.to_vec();
        bytes.extend_from_slice(&self.get(GAIN).to_le_bytes());
        bytes.extend_from_slice(&self.get(LATENCY).to_le_bytes());
        bytes
    }

    fn load_chunk(&self, bytes: &[u8]) -> bool {
        let Some(rest) = bytes.strip_prefix(CHUNK_MAGIC.as_slice()) else {
            return false;
        };
        let (Some(gain), Some(latency)) = (rest.get(..4), rest.get(4..8)) else {
            return false;
        };
        self.set(GAIN, f32::from_le_bytes(gain.try_into().unwrap()));
        self.set(LATENCY, f32::from_le_bytes(latency.try_into().unwrap()));
        true
    }
}

unsafe extern "C" fn dispatcher(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    ptr: *mut c_void,
    opt: f32,
) -> isize {
    let p = unsafe { Plugin::from(effect) };
    match opcode {
        effOpen => {
            p.expect_thread(false);
            0
        }
        effClose => {
            p.expect_thread(false);
            #[cfg(target_os = "linux")]
            gui::close(p);
            // SAFETY: made by Box::into_raw in VSTPluginMain; nothing uses
            // it after this.
            unsafe { drop(Box::from_raw(p as *const Plugin as *mut Plugin)) };
            1
        }
        effSetProgram => {
            if (0..p.programs() as isize).contains(&value) {
                p.program.store(value as i32, Ordering::Relaxed);
            }
            0
        }
        effGetProgram => p.program.load(Ordering::Relaxed) as isize,
        effGetProgramName => {
            let name = format!("Program {}", p.program.load(Ordering::Relaxed) + 1);
            unsafe { write(ptr, kVstMaxProgNameLen, &name) };
            1
        }
        effGetParamLabel => {
            let label = match index {
                GAIN => "dB",
                LATENCY => "samples",
                TEMPO_SEEN => "BPM",
                _ => "",
            };
            unsafe { write(ptr, kVstMaxParamStrLen, label) };
            0
        }
        effGetParamDisplay => {
            let value = unsafe { get_parameter(effect, index) };
            let text = match index {
                GAIN => format!("{:.1}", -60.0 + 72.0 * value),
                LATENCY => latency_samples(value).to_string(),
                TEMPO_SEEN => format!("{:.1}", value * 1000.0),
                RESET => if value >= 0.5 { "On" } else { "Off" }.to_owned(),
                _ => value.to_string(),
            };
            unsafe { write(ptr, kVstMaxParamStrLen, &text) };
            0
        }
        effGetParamName => {
            let name = match index {
                GAIN => "Gain",
                LATENCY => "Latency",
                MOD_SEEN => "Mod seen",
                TEMPO_SEEN => "Tempo seen",
                RESET => "Reset",
                THREAD_ERRORS => "Thread errors",
                PROGRAM => "Program",
                GUI_TICKS => "GUI ticks",
                GUI_FD_EVENTS => "GUI events",
                GUI_PARENT_WIDTH => "GUI parent width",
                GUI_OPEN => "GUI open",
                _ => "Made since loaded",
            };
            // Longer than the format's 8 bytes, as plugins often are.
            unsafe { write(ptr, 32, name) };
            0
        }
        effSetSampleRate => {
            p.audio.lock().unwrap().sample_rate = f64::from(opt);
            0
        }
        effSetBlockSize => {
            p.audio.lock().unwrap().max_block = value.max(1) as usize;
            0
        }
        effMainsChanged => {
            p.expect_thread(false);
            if value != 0 {
                let latency = latency_samples(p.get(LATENCY));
                let mut audio = p.audio.lock().unwrap();
                audio.latency = latency;
                audio.delay = vec![[0.0; 2]; MAX_LATENCY + audio.max_block + 1];
                audio.write = 0;
                audio.voices = [None; 128];
                unsafe { (*p.effect.get()).initial_delay = latency as i32 };
            }
            p.resumed.store(value != 0, Ordering::Relaxed);
            0
        }
        effStartProcess | effStopProcess => {
            p.expect_thread(false);
            0
        }
        effGetChunk => {
            p.expect_thread(false);
            let mut chunk = p.chunk.lock().unwrap();
            *chunk = p.chunk();
            unsafe { *(ptr as *mut *mut c_void) = chunk.as_mut_ptr() as *mut c_void };
            chunk.len() as isize
        }
        effSetChunk => {
            p.expect_thread(false);
            let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, value as usize) };
            isize::from(p.load_chunk(bytes))
        }
        effProcessEvents => {
            p.expect_thread(true);
            let list = ptr as *const VstEvents;
            let mut audio = p.audio.lock().unwrap();
            unsafe {
                let events = std::ptr::addr_of!((*list).events) as *const *const VstEvent;
                for i in 0..(*list).num_events.max(0) as usize {
                    let event = *events.add(i);
                    if (*event).type_ == kVstMidiType {
                        let midi = &*(event as *const VstMidiEvent);
                        let [a, b, c, _] = midi.midi_data;
                        audio.events.push((midi.delta_frames, [a, b, c]));
                    }
                }
            }
            1
        }
        effCanBeAutomated => 1,
        effGetPlugCategory => match p.kind {
            Kind::Gain => kPlugCategEffect,
            Kind::Sine => kPlugCategSynth,
            Kind::Shell => kPlugCategShell,
        },
        effGetEffectName => {
            unsafe { write(ptr, kVstMaxEffectNameLen, p.name()) };
            1
        }
        effGetVendorString => {
            unsafe { write(ptr, kVstMaxVendorStrLen, "canopy") };
            1
        }
        effGetProductString => {
            unsafe { write(ptr, kVstMaxProductStrLen, p.name()) };
            1
        }
        effGetVendorVersion => 0x01_00_00,
        effCanDo => {
            let asked = unsafe { CStr::from_ptr(ptr as *const c_char) }.to_bytes();
            match asked {
                b"receiveVstEvents" | b"receiveVstMidiEvent" => 1,
                b"sendVstEvents" | b"sendVstMidiEvent" if p.kind == Kind::Sine => 1,
                _ => -1,
            }
        }
        // The gain says it has no tail; the sine doesn't say.
        effGetTailSize => isize::from(p.kind == Kind::Gain),
        effGetParameterProperties => {
            let properties = unsafe { &mut *(ptr as *mut VstParameterProperties) };
            match index {
                GAIN => {
                    properties.flags = kVstParameterSupportsDisplayCategory;
                    properties.category = 1;
                    properties.num_parameters_in_category = 1;
                    let label = b"Main\0";
                    properties.category_label[..label.len()].copy_from_slice(label);
                    1
                }
                LATENCY => {
                    properties.flags = kVstParameterUsesIntegerMinMax;
                    properties.min_integer = 0;
                    properties.max_integer = MAX_LATENCY as i32;
                    1
                }
                RESET => {
                    properties.flags = kVstParameterIsSwitch;
                    1
                }
                _ => 0,
            }
        }
        effGetVstVersion => 2400,
        effSetProcessPrecision => isize::from(value == kVstProcessPrecision32),
        effBeginSetProgram | effEndSetProgram => 1,
        #[cfg(target_os = "linux")]
        effEditGetRect => {
            p.expect_thread(false);
            unsafe { *(ptr as *mut *mut ERect) = p.rect.get() };
            1
        }
        #[cfg(target_os = "linux")]
        effEditOpen => {
            p.expect_thread(false);
            isize::from(gui::open(p, ptr as usize as u32))
        }
        #[cfg(target_os = "linux")]
        effEditIdle => {
            p.expect_thread(false);
            gui::idle(p);
            1
        }
        #[cfg(target_os = "linux")]
        effEditClose => {
            p.expect_thread(false);
            gui::close(p);
            1
        }
        _ => 0,
    }
}

unsafe extern "C" fn set_parameter(effect: *mut AEffect, index: i32, value: f32) {
    unsafe { Plugin::from(effect) }.set(index, value);
}

unsafe extern "C" fn get_parameter(effect: *mut AEffect, index: i32) -> f32 {
    let p = unsafe { Plugin::from(effect) };
    match index {
        GAIN | LATENCY | MOD_SEEN | TEMPO_SEEN | RESET => p.get(index),
        THREAD_ERRORS => p.thread_errors.load(Ordering::Relaxed) as f32,
        PROGRAM => p.program.load(Ordering::Relaxed) as f32,
        GUI_TICKS => p.gui_ticks.load(Ordering::Relaxed) as f32,
        GUI_FD_EVENTS => p.gui_fd_events.load(Ordering::Relaxed) as f32,
        GUI_PARENT_WIDTH => p.gui_parent_width.load(Ordering::Relaxed) as f32,
        GUI_OPEN => f32::from(u8::from(p.gui_open.load(Ordering::Relaxed))),
        MADE => MADE_COUNT.load(Ordering::Relaxed) as f32,
        _ => 0.0,
    }
}

unsafe extern "C" fn process_replacing(
    effect: *mut AEffect,
    inputs: *mut *mut f32,
    outputs: *mut *mut f32,
    frames: i32,
) {
    let p = unsafe { Plugin::from(effect) };
    p.expect_thread(true);
    let mut audio = p.audio.lock().unwrap();

    // Report the tempo back, as a change of its own.
    let time = p.call(
        audioMasterGetTime,
        0,
        kVstTempoValid as isize,
        null_mut(),
        0.0,
    ) as *const VstTimeInfo;
    if let Some(time) = unsafe { time.as_ref() }
        && time.flags & kVstTempoValid != 0
        && time.tempo != audio.last_tempo
    {
        audio.last_tempo = time.tempo;
        let value = (time.tempo / 1000.0) as f32;
        p.store(TEMPO_SEEN, value);
        p.call(audioMasterAutomate, TEMPO_SEEN, 0, null_mut(), value);
    }

    let mut events = std::mem::take(&mut audio.events);
    events.sort_by_key(|e| e.0);
    let (num_inputs, num_outputs) = unsafe { ((*effect).num_inputs, (*effect).num_outputs) };
    let channel = |list: *mut *mut f32, count: i32, index: i32| -> *mut f32 {
        if list.is_null() || index >= count {
            null_mut()
        } else {
            unsafe { *list.add(index as usize) }
        }
    };
    let latency = audio.latency;
    let len = audio.delay.len().max(1);
    let mut next = 0;
    for i in 0..frames.max(0) {
        while next < events.len() && events[next].0 <= i {
            p.apply(&mut audio, i, events[next].1);
            next += 1;
        }
        let gain = gain_linear(p.get(GAIN));
        let mut frame = [0.0f32; 2];
        if p.kind == Kind::Sine {
            let rate = audio.sample_rate;
            for (key, voice) in audio.voices.iter_mut().enumerate() {
                if let Some((phase, velocity)) = voice {
                    let hz = 440.0 * ((key as f64 - 69.0) / 12.0).exp2();
                    frame[0] += (*phase * std::f64::consts::TAU).sin() as f32 * *velocity;
                    *phase = (*phase + hz / rate).fract();
                }
            }
            frame[1] = frame[0];
        } else if !audio.delay.is_empty() {
            for (ch, sample) in frame.iter_mut().enumerate() {
                for input in [ch as i32, 2 + ch as i32] {
                    let ptr = channel(inputs, num_inputs, input);
                    if !ptr.is_null() {
                        *sample += unsafe { *ptr.add(i as usize) };
                    }
                }
            }
            let write = audio.write;
            audio.delay[write] = frame;
            frame = audio.delay[(write + len - latency.min(len - 1)) % len];
            audio.write = (write + 1) % len;
        }
        for (ch, sample) in frame.iter().enumerate() {
            let ptr = channel(outputs, num_outputs, ch as i32);
            if !ptr.is_null() {
                unsafe { *ptr.add(i as usize) = sample * gain };
            }
        }
    }
    while next < events.len() {
        p.apply(&mut audio, frames - 1, events[next].1);
        next += 1;
    }
    events.clear();
    audio.events = events;
}

/// Makes the plugin this file is (see the crate docs).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn VSTPluginMain(host: HostCallback) -> *mut AEffect {
    // The effect is null here: hosts must cope.
    if unsafe { host(null_mut(), audioMasterVersion, 0, 0, null_mut(), 0.0) } < kVstVersion {
        return null_mut();
    }
    let kind = Kind::from_file_name(&library_name());
    let mut flags = effFlagsCanReplacing;
    if kind == Kind::Sine {
        flags |= effFlagsIsSynth;
    } else {
        flags |= effFlagsProgramChunks;
    }
    if cfg!(target_os = "linux") {
        flags |= effFlagsHasEditor;
    }
    let plugin = Box::new(Plugin {
        effect: UnsafeCell::new(AEffect {
            magic: kEffectMagic,
            dispatcher: Some(dispatcher),
            process: None,
            set_parameter: Some(set_parameter),
            get_parameter: Some(get_parameter),
            num_programs: if kind == Kind::Sine { 2 } else { 1 },
            num_params: PARAMS,
            num_inputs: if kind == Kind::Sine { 0 } else { 4 },
            num_outputs: 2,
            flags,
            resvd1: 0,
            resvd2: 0,
            initial_delay: 0,
            real_qualities: 0,
            off_qualities: 0,
            io_ratio: 1.0,
            object: null_mut(),
            user: null_mut(),
            unique_id: i32::from_be_bytes(if kind == Kind::Sine {
                *b"CnyS"
            } else {
                *b"CnyG"
            }),
            version: 0x01_00_00,
            process_replacing: Some(process_replacing),
            process_double_replacing: None,
            future: [0; 56],
        }),
        host,
        kind,
        values: [
            AtomicU32::new(GAIN_DEFAULT.to_bits()),
            AtomicU32::new(0),
            AtomicU32::new(0),
            AtomicU32::new(0),
            AtomicU32::new(0),
        ],
        thread_errors: AtomicU64::new(0),
        program: AtomicI32::new(0),
        resumed: AtomicBool::new(false),
        audio: Mutex::new(Audio {
            delay: Vec::new(),
            write: 0,
            latency: 0,
            voices: [None; 128],
            sample_rate: 48_000.0,
            max_block: 1024,
            last_tempo: 0.0,
            events: Vec::with_capacity(4096),
        }),
        chunk: Mutex::new(Vec::new()),
        rect: UnsafeCell::new(ERect {
            top: 0,
            left: 0,
            bottom: 200,
            right: 300,
        }),
        #[cfg(target_os = "linux")]
        gui: Mutex::new(None),
        gui_ticks: AtomicU64::new(0),
        gui_fd_events: AtomicU64::new(0),
        gui_parent_width: AtomicU64::new(0),
        gui_open: AtomicBool::new(false),
    });
    MADE_COUNT.fetch_add(1, Ordering::Relaxed);
    let plugin = Box::into_raw(plugin);
    unsafe {
        (*(*plugin).effect.get()).object = plugin as *mut c_void;
        (*plugin).effect.get()
    }
}
