//! A tiny VST3 plugin for the `plugins` crate's tests, on the `vst3`
//! crate's COM bindings: one module with two plugins, each a component and
//! a separate controller that talk through connection points.
//!
//! - Canopy Test Gain (`C0FFEE01CA0E4A11B0B0000000000001`): stereo in
//!   (plus a stereo sidechain, added to the input) and out, with a note
//!   input. Output is input delayed by "Latency" samples and scaled by
//!   "Gain", changing at the exact sample of each parameter point. The
//!   controller asks for a restart (`kLatencyChanged`) when Latency
//!   changes, and maps MIDI CC 1 to Gain (`IMidiMapping`).
//! - Canopy Test Sine (`...0002`): a sine per held key through Gain.
//!
//! Parameters (normalized, as VST3 has them): 0 Gain (-60 to +12 dB), 1
//! Latency (0 to 1024 samples, 1024 steps), 2 Mod seen (unused here), 3
//! Tempo seen (tempo / 1000, read-only: the processor reports the
//! transport's tempo as an output parameter change), 4 Reset (a switch:
//! setting it makes the controller `performEdit` Gain back to 0 dB, as an
//! editor button would), 6 Messages seen (read-only: how many messages
//! the component sent the controller; it sends one on each activation).
//!
//! On Linux both have a small X11 editor (`gui`), run from the host's
//! `IRunLoop`; parameters 7 to 10 report what it saw, as raw numbers.
//!
//! The component's state is Gain and Latency; the controller's is a
//! marker byte.

#![allow(non_snake_case, clippy::missing_safety_doc)]
// VST3's enum constants are u32 on Linux and i32 on Windows, so casts that
// are no-ops here aren't there.
#![allow(clippy::unnecessary_cast)]

#[cfg(target_os = "linux")]
mod gui;

use std::ffi::{CStr, c_char, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface, uid};

const GAIN: ParamID = 0;
const LATENCY: ParamID = 1;
const MOD_SEEN: ParamID = 2;
const TEMPO_SEEN: ParamID = 3;
const RESET: ParamID = 4;
const MESSAGES: ParamID = 6;
const GUI_TICKS: ParamID = 7;
const GUI_FD_EVENTS: ParamID = 8;
const GUI_PARENT_WIDTH: ParamID = 9;
const GUI_OPEN: ParamID = 10;
const PARAMS: [ParamID; 10] = [
    GAIN,
    LATENCY,
    MOD_SEEN,
    TEMPO_SEEN,
    RESET,
    MESSAGES,
    GUI_TICKS,
    GUI_FD_EVENTS,
    GUI_PARENT_WIDTH,
    GUI_OPEN,
];

const MAX_LATENCY: usize = 1024;
const STATE_MAGIC: &[u8; 8] = b"CNPYVST3";
/// Gain's normalized value at 0 dB.
const GAIN_DEFAULT: f64 = 60.0 / 72.0;

const GAIN_CID: TUID = uid(0xC0FFEE01, 0xCA0E4A11, 0xB0B00000, 0x00000001);
const SINE_CID: TUID = uid(0xC0FFEE01, 0xCA0E4A11, 0xB0B00000, 0x00000002);
const CONTROLLER_CID: TUID = uid(0xC0FFEE01, 0xCA0E4A11, 0xB0B00000, 0x00000003);

fn gain_linear(normalized: f64) -> f32 {
    10f64.powf((-60.0 + 72.0 * normalized.clamp(0.0, 1.0)) / 20.0) as f32
}

fn latency_samples(normalized: f64) -> usize {
    (normalized.clamp(0.0, 1.0) * MAX_LATENCY as f64).round() as usize
}

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    for (d, s) in dst.iter_mut().zip(src.bytes().chain(Some(0))) {
        *d = s as c_char;
    }
    if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (d, s) in dst.iter_mut().zip(src.encode_utf16()) {
        *d = s;
        len += 1;
    }
    let end = len.min(dst.len() - 1);
    dst[end] = 0;
}

/// Reads all of a stream.
unsafe fn read_all(stream: *mut IBStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let Some(stream) = (unsafe { ComRef::from_raw(stream) }) else {
        return bytes;
    };
    let mut chunk = [0u8; 256];
    loop {
        let mut read = 0;
        let result = unsafe {
            stream.read(
                chunk.as_mut_ptr() as *mut c_void,
                chunk.len() as i32,
                &mut read,
            )
        };
        if result != kResultOk || read <= 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read as usize]);
    }
    bytes
}

unsafe fn write_all(stream: *mut IBStream, bytes: &[u8]) -> tresult {
    let Some(stream) = (unsafe { ComRef::from_raw(stream) }) else {
        return kInvalidArgument;
    };
    let mut written = 0;
    unsafe {
        stream.write(
            bytes.as_ptr() as *mut c_void,
            bytes.len() as i32,
            &mut written,
        )
    }
}

/// The component's state: Gain and Latency, normalized.
fn parse_state(bytes: &[u8]) -> Option<(f64, f64)> {
    let rest = bytes.strip_prefix(STATE_MAGIC.as_slice())?;
    let gain = f64::from_le_bytes(rest.get(..8)?.try_into().ok()?);
    let latency = f64::from_le_bytes(rest.get(8..16)?.try_into().ok()?);
    Some((gain, latency))
}

struct Audio {
    delay: Vec<[f32; 2]>,
    write: usize,
    voices: [Option<(f64, f32)>; 128],
    sample_rate: f64,
    max_block: usize,
    last_tempo: f64,
}

struct Processor {
    instrument: bool,
    gain: AtomicU64,
    latency: AtomicU64,
    /// The latency this activation runs with.
    active_latency: AtomicU64,
    audio: Mutex<Audio>,
    host: Mutex<Option<ComPtr<IHostApplication>>>,
    peer: Mutex<Option<ComPtr<IConnectionPoint>>>,
}

impl Processor {
    fn new(instrument: bool) -> Processor {
        Processor {
            instrument,
            gain: AtomicU64::new(GAIN_DEFAULT.to_bits()),
            latency: AtomicU64::new(0f64.to_bits()),
            active_latency: AtomicU64::new(0),
            audio: Mutex::new(Audio {
                delay: Vec::new(),
                write: 0,
                voices: [None; 128],
                sample_rate: 48_000.0,
                max_block: 1024,
                last_tempo: 0.0,
            }),
            host: Mutex::new(None),
            peer: Mutex::new(None),
        }
    }

    fn get(value: &AtomicU64) -> f64 {
        f64::from_bits(value.load(Ordering::Relaxed))
    }

    fn set(&self, id: ParamID, value: f64) {
        match id {
            GAIN => self.gain.store(value.to_bits(), Ordering::Relaxed),
            LATENCY => self.latency.store(value.to_bits(), Ordering::Relaxed),
            _ => {}
        }
    }

    /// Tells the controller it was activated, through a host message.
    unsafe fn notify_controller(&self) {
        let host = self.host.lock().unwrap().clone();
        let peer = self.peer.lock().unwrap().clone();
        let (Some(host), Some(peer)) = (host, peer) else {
            return;
        };
        let mut cid = IMessage_iid;
        let mut iid = IMessage_iid;
        let mut object: *mut c_void = std::ptr::null_mut();
        unsafe {
            if host.createInstance(&mut cid, &mut iid, &mut object) != kResultOk {
                return;
            }
            let Some(message) = ComPtr::<IMessage>::from_raw(object as *mut IMessage) else {
                return;
            };
            message.setMessageID(c"activated".as_ptr());
            if let Some(attributes) = ComRef::from_raw(message.getAttributes()) {
                attributes.setInt(
                    c"latency".as_ptr(),
                    self.active_latency.load(Ordering::Relaxed) as i64,
                );
            }
            peer.notify(message.as_ptr());
        }
    }
}

impl Class for Processor {
    type Interfaces = (
        IComponent,
        IAudioProcessor,
        IProcessContextRequirements,
        IConnectionPoint,
    );
}

impl IPluginBaseTrait for Processor {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        let host = unsafe { ComRef::from_raw(context) }.and_then(|c| c.cast::<IHostApplication>());
        *self.host.lock().unwrap() = host;
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        *self.host.lock().unwrap() = None;
        kResultOk
    }
}

impl IComponentTrait for Processor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        unsafe { *class_id = CONTROLLER_CID };
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media: MediaType, dir: BusDirection) -> i32 {
        let audio = media == MediaTypes_::kAudio as MediaType;
        let input = dir == BusDirections_::kInput as BusDirection;
        match (audio, input) {
            (true, true) if self.instrument => 0,
            (true, true) => 2,
            (true, false) => 1,
            (false, true) => 1,
            (false, false) => 0,
        }
    }

    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if index < 0 || index >= unsafe { self.getBusCount(media, dir) } {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        let audio = media == MediaTypes_::kAudio as MediaType;
        let input = dir == BusDirections_::kInput as BusDirection;
        bus.mediaType = media;
        bus.direction = dir;
        bus.channelCount = if audio { 2 } else { 16 };
        let name = match (audio, input, index) {
            (false, _, _) => "Notes",
            (true, true, 0) => "Input",
            (true, true, _) => "Sidechain",
            _ => "Output",
        };
        copy_wstring(name, &mut bus.name);
        bus.busType = if index == 0 {
            BusTypes_::kMain
        } else {
            BusTypes_::kAux
        } as BusType;
        bus.flags = if index == 0 {
            BusInfo_::BusFlags_::kDefaultActive as u32
        } else {
            0
        };
        kResultOk
    }

    unsafe fn getRoutingInfo(&self, _in: *mut RoutingInfo, _out: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        _media: MediaType,
        _dir: BusDirection,
        _index: i32,
        _state: TBool,
    ) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, state: TBool) -> tresult {
        if state != 0 {
            let latency = latency_samples(Self::get(&self.latency));
            self.active_latency.store(latency as u64, Ordering::Relaxed);
            {
                let mut audio = self.audio.lock().unwrap();
                audio.delay = vec![[0.0; 2]; MAX_LATENCY + audio.max_block + 1];
                audio.write = 0;
                audio.voices = [None; 128];
            }
            unsafe { self.notify_controller() };
        }
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        match parse_state(&unsafe { read_all(state) }) {
            Some((gain, latency)) => {
                self.set(GAIN, gain);
                self.set(LATENCY, latency);
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let mut bytes = STATE_MAGIC.to_vec();
        bytes.extend_from_slice(&Self::get(&self.gain).to_le_bytes());
        bytes.extend_from_slice(&Self::get(&self.latency).to_le_bytes());
        unsafe { write_all(state, &bytes) }
    }
}

impl IAudioProcessorTrait for Processor {
    unsafe fn setBusArrangements(
        &self,
        _inputs: *mut SpeakerArrangement,
        _num_ins: i32,
        _outputs: *mut SpeakerArrangement,
        _num_outs: i32,
    ) -> tresult {
        kResultFalse
    }

    unsafe fn getBusArrangement(
        &self,
        _dir: BusDirection,
        _index: i32,
        arrangement: *mut SpeakerArrangement,
    ) -> tresult {
        unsafe { *arrangement = SpeakerArr::kStereo };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        self.active_latency.load(Ordering::Relaxed) as u32
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        let setup = unsafe { &*setup };
        let mut audio = self.audio.lock().unwrap();
        audio.sample_rate = setup.sampleRate;
        audio.max_block = setup.maxSamplesPerBlock.max(1) as usize;
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let data = unsafe { &mut *data };
        let mut audio = self.audio.lock().unwrap();
        let frames = data.numSamples.max(0) as usize;

        // Report the tempo back, as a processor-side parameter change.
        if let Some(context) = unsafe { data.processContext.as_ref() }
            && context.state & ProcessContext_::StatesAndFlags_::kTempoValid as u32 != 0
            && context.tempo != audio.last_tempo
        {
            audio.last_tempo = context.tempo;
            if let Some(out) = unsafe { ComRef::from_raw(data.outputParameterChanges) } {
                let mut index = 0;
                let queue = unsafe { out.addParameterData(&TEMPO_SEEN, &mut index) };
                if let Some(queue) = unsafe { ComRef::from_raw(queue) } {
                    unsafe {
                        queue.addPoint(0, (context.tempo / 1000.0).clamp(0.0, 1.0), &mut index)
                    };
                }
            }
        }

        // Every change and note, by sample offset.
        let mut changes: Vec<(i32, u8, u32, f64)> = Vec::new();
        if let Some(params) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            for i in 0..unsafe { params.getParameterCount() } {
                let Some(queue) = (unsafe { ComRef::from_raw(params.getParameterData(i)) }) else {
                    continue;
                };
                let id = unsafe { queue.getParameterId() };
                for p in 0..unsafe { queue.getPointCount() } {
                    let (mut offset, mut value) = (0, 0.0);
                    if unsafe { queue.getPoint(p, &mut offset, &mut value) } == kResultTrue {
                        changes.push((offset, 0, id, value));
                    }
                }
            }
        }
        if let Some(events) = unsafe { ComRef::from_raw(data.inputEvents) } {
            for i in 0..unsafe { events.getEventCount() } {
                let mut event: Event = unsafe { std::mem::zeroed() };
                if unsafe { events.getEvent(i, &mut event) } != kResultOk {
                    continue;
                }
                let kind = event.r#type as u32;
                let (on, pitch, velocity) = unsafe {
                    if kind == Event_::EventTypes_::kNoteOnEvent as u32 {
                        (
                            1,
                            event.__field0.noteOn.pitch,
                            event.__field0.noteOn.velocity,
                        )
                    } else if kind == Event_::EventTypes_::kNoteOffEvent as u32 {
                        (2, event.__field0.noteOff.pitch, 0.0)
                    } else {
                        continue;
                    }
                };
                changes.push((event.sampleOffset, on, pitch as u32, f64::from(velocity)));
            }
        }
        changes.sort_by_key(|c| c.0);

        let channel =
            |buses: *mut AudioBusBuffers, count: i32, bus: usize, ch: usize| -> *mut f32 {
                if bus >= count as usize || buses.is_null() {
                    return std::ptr::null_mut();
                }
                unsafe {
                    let b = &*buses.add(bus);
                    if ch >= b.numChannels as usize || b.__field0.channelBuffers32.is_null() {
                        return std::ptr::null_mut();
                    }
                    *b.__field0.channelBuffers32.add(ch)
                }
            };
        let latency = self.active_latency.load(Ordering::Relaxed) as usize;
        let len = audio.delay.len().max(1);
        let mut next = 0;
        for i in 0..frames {
            while next < changes.len() && changes[next].0 as usize <= i {
                let (_, kind, id, value) = changes[next];
                match kind {
                    0 => self.set(id, value),
                    1 => audio.voices[id as usize & 127] = Some((0.0, value as f32)),
                    _ => audio.voices[id as usize & 127] = None,
                }
                next += 1;
            }
            let gain = gain_linear(Self::get(&self.gain));
            let mut frame = [0.0f32; 2];
            if self.instrument {
                let rate = audio.sample_rate;
                for (key, voice) in audio.voices.iter_mut().enumerate() {
                    if let Some((phase, velocity)) = voice {
                        let hz = 440.0 * ((key as f64 - 69.0) / 12.0).exp2();
                        let s = (*phase * std::f64::consts::TAU).sin() as f32 * *velocity;
                        frame[0] += s;
                        frame[1] += s;
                        *phase = (*phase + hz / rate).fract();
                    }
                }
            } else if !audio.delay.is_empty() {
                for (ch, sample) in frame.iter_mut().enumerate() {
                    for bus in 0..2 {
                        let ptr = channel(data.inputs, data.numInputs, bus, ch);
                        if !ptr.is_null() {
                            *sample += unsafe { *ptr.add(i) };
                        }
                    }
                }
                let write = audio.write;
                audio.delay[write] = frame;
                frame = audio.delay[(write + len - latency.min(len - 1)) % len];
                audio.write = (write + 1) % len;
            }
            for (ch, sample) in frame.iter().enumerate() {
                let ptr = channel(data.outputs, data.numOutputs, 0, ch);
                if !ptr.is_null() {
                    unsafe { *ptr.add(i) = sample * gain };
                }
            }
        }
        while next < changes.len() {
            if changes[next].1 == 0 {
                self.set(changes[next].2, changes[next].3);
            }
            next += 1;
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        if self.instrument { kInfiniteTail } else { 0 }
    }
}

impl IProcessContextRequirementsTrait for Processor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

impl IConnectionPointTrait for Processor {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        *self.peer.lock().unwrap() = unsafe { ComRef::from_raw(other) }.map(|o| o.to_com_ptr());
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        *self.peer.lock().unwrap() = None;
        kResultOk
    }

    unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
        kResultOk
    }
}

struct Controller {
    values: Mutex<[f64; 7]>,
    handler: Mutex<Option<ComPtr<IComponentHandler>>>,
    peer: Mutex<Option<ComPtr<IConnectionPoint>>>,
    messages: AtomicU64,
    #[cfg(target_os = "linux")]
    gui: std::sync::Arc<gui::Counters>,
}

impl Controller {
    fn new() -> Controller {
        let mut values = [0.0; 7];
        values[GAIN as usize] = GAIN_DEFAULT;
        Controller {
            values: Mutex::new(values),
            handler: Mutex::new(None),
            peer: Mutex::new(None),
            messages: AtomicU64::new(0),
            #[cfg(target_os = "linux")]
            gui: Default::default(),
        }
    }
}

impl Class for Controller {
    type Interfaces = (IEditController, IMidiMapping, IConnectionPoint);
}

impl IPluginBaseTrait for Controller {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        *self.handler.lock().unwrap() = None;
        kResultOk
    }
}

impl IEditControllerTrait for Controller {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        match parse_state(&unsafe { read_all(state) }) {
            Some((gain, latency)) => {
                let mut values = self.values.lock().unwrap();
                values[GAIN as usize] = gain;
                values[LATENCY as usize] = latency;
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        if unsafe { read_all(state) } == b"C" {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        unsafe { write_all(state, b"C") }
    }

    unsafe fn getParameterCount(&self) -> i32 {
        PARAMS.len() as i32
    }

    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        let Some(&id) = PARAMS.get(index as usize) else {
            return kInvalidArgument;
        };
        use ParameterInfo_::ParameterFlags_::*;
        let info = unsafe { &mut *info };
        let (title, units, steps, default, flags) = match id {
            GAIN => ("Gain", "dB", 0, GAIN_DEFAULT, kCanAutomate),
            LATENCY => ("Latency", "samples", MAX_LATENCY as i32, 0.0, kCanAutomate),
            MOD_SEEN => ("Mod seen", "", 0, 0.0, kIsReadOnly),
            TEMPO_SEEN => ("Tempo seen", "BPM", 0, 0.0, kIsReadOnly),
            RESET => ("Reset", "", 1, 0.0, kCanAutomate | kIsList),
            MESSAGES => ("Messages seen", "", 0, 0.0, kIsReadOnly | kIsHidden),
            GUI_TICKS => ("GUI ticks", "", 0, 0.0, kIsReadOnly | kIsHidden),
            GUI_FD_EVENTS => ("GUI events", "", 0, 0.0, kIsReadOnly | kIsHidden),
            GUI_PARENT_WIDTH => ("GUI parent width", "", 0, 0.0, kIsReadOnly | kIsHidden),
            _ => ("GUI open", "", 1, 0.0, kIsReadOnly | kIsHidden),
        };
        info.id = id;
        copy_wstring(title, &mut info.title);
        copy_wstring(title, &mut info.shortTitle);
        copy_wstring(units, &mut info.units);
        info.stepCount = steps;
        info.defaultNormalizedValue = default;
        info.unitId = kRootUnitId;
        info.flags = flags as i32;
        kResultOk
    }

    unsafe fn getParamStringByValue(
        &self,
        id: ParamID,
        value: ParamValue,
        string: *mut String128,
    ) -> tresult {
        let text = match id {
            GAIN => format!("{:.1} dB", -60.0 + 72.0 * value),
            LATENCY => format!("{} samples", latency_samples(value)),
            TEMPO_SEEN => format!("{:.1} BPM", value * 1000.0),
            RESET => if value >= 0.5 { "Reset" } else { "-" }.to_owned(),
            _ => format!("{value}"),
        };
        copy_wstring(&text, unsafe { &mut *string });
        kResultOk
    }

    unsafe fn getParamValueByString(
        &self,
        id: ParamID,
        string: *mut TChar,
        value: *mut ParamValue,
    ) -> tresult {
        let mut len = 0;
        while unsafe { *string.add(len) } != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(string, len) });
        let Some(number) = text
            .split_whitespace()
            .next()
            .and_then(|t| t.parse::<f64>().ok())
        else {
            return kResultFalse;
        };
        let normalized = match id {
            GAIN => (number + 60.0) / 72.0,
            LATENCY => number / MAX_LATENCY as f64,
            _ => number,
        };
        unsafe { *value = normalized.clamp(0.0, 1.0) };
        kResultOk
    }

    unsafe fn normalizedParamToPlain(&self, id: ParamID, value: ParamValue) -> ParamValue {
        match id {
            GAIN => -60.0 + 72.0 * value,
            LATENCY => latency_samples(value) as f64,
            _ => value,
        }
    }

    unsafe fn plainParamToNormalized(&self, id: ParamID, plain: ParamValue) -> ParamValue {
        match id {
            GAIN => (plain + 60.0) / 72.0,
            LATENCY => plain / MAX_LATENCY as f64,
            _ => plain,
        }
    }

    unsafe fn getParamNormalized(&self, id: ParamID) -> ParamValue {
        if id == MESSAGES {
            return self.messages.load(Ordering::Relaxed) as f64 / 1000.0;
        }
        #[cfg(target_os = "linux")]
        {
            let counters = &self.gui;
            match id {
                GUI_TICKS => return counters.ticks.load(Ordering::Relaxed) as f64,
                GUI_FD_EVENTS => return counters.fd_events.load(Ordering::Relaxed) as f64,
                GUI_PARENT_WIDTH => return counters.parent_width.load(Ordering::Relaxed) as f64,
                GUI_OPEN => return f64::from(u8::from(counters.open.load(Ordering::Relaxed))),
                _ => {}
            }
        }
        self.values
            .lock()
            .unwrap()
            .get(id as usize)
            .copied()
            .unwrap_or(0.0)
    }

    unsafe fn setParamNormalized(&self, id: ParamID, value: ParamValue) -> tresult {
        let old = {
            let mut values = self.values.lock().unwrap();
            let Some(slot) = values.get_mut(id as usize) else {
                return kInvalidArgument;
            };
            std::mem::replace(slot, value)
        };
        let handler = self.handler.lock().unwrap().clone();
        if let Some(handler) = handler {
            if id == LATENCY && latency_samples(old) != latency_samples(value) {
                unsafe { handler.restartComponent(RestartFlags_::kLatencyChanged as i32) };
            }
            if id == RESET && value >= 0.5 {
                // What an editor's "reset" button does: an edit of its own.
                self.values.lock().unwrap()[GAIN as usize] = GAIN_DEFAULT;
                self.values.lock().unwrap()[RESET as usize] = 0.0;
                unsafe {
                    handler.beginEdit(GAIN);
                    handler.performEdit(GAIN, GAIN_DEFAULT);
                    handler.endEdit(GAIN);
                }
            }
        }
        kResultOk
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        *self.handler.lock().unwrap() =
            unsafe { ComRef::from_raw(handler) }.map(|h| h.to_com_ptr());
        kResultOk
    }

    unsafe fn createView(&self, name: FIDString) -> *mut IPlugView {
        #[cfg(target_os = "linux")]
        if !name.is_null() && unsafe { CStr::from_ptr(name) } == c"editor" {
            return gui::create(self.gui.clone());
        }
        let _ = name;
        std::ptr::null_mut()
    }
}

impl IMidiMappingTrait for Controller {
    unsafe fn getMidiControllerAssignment(
        &self,
        bus: i32,
        _channel: i16,
        number: CtrlNumber,
        id: *mut ParamID,
    ) -> tresult {
        if bus == 0 && number == 1 {
            unsafe { *id = GAIN };
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

impl IConnectionPointTrait for Controller {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        *self.peer.lock().unwrap() = unsafe { ComRef::from_raw(other) }.map(|o| o.to_com_ptr());
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        *self.peer.lock().unwrap() = None;
        kResultOk
    }

    unsafe fn notify(&self, message: *mut IMessage) -> tresult {
        let Some(message) = (unsafe { ComRef::from_raw(message) }) else {
            return kInvalidArgument;
        };
        let id = unsafe { message.getMessageID() };
        if !id.is_null() && unsafe { CStr::from_ptr(id) } == c"activated" {
            let mut latency = -1;
            if let Some(attributes) = unsafe { ComRef::from_raw(message.getAttributes()) } {
                unsafe { attributes.getInt(c"latency".as_ptr(), &mut latency) };
            }
            if latency >= 0 {
                self.messages.fetch_add(1, Ordering::Relaxed);
            }
        }
        kResultOk
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

const CLASSES: [(TUID, &str, &str, &str); 3] = [
    (
        GAIN_CID,
        "Audio Module Class",
        "Canopy Test Gain",
        "Fx|Dynamics",
    ),
    (
        SINE_CID,
        "Audio Module Class",
        "Canopy Test Sine",
        "Instrument|Synth",
    ),
    (
        CONTROLLER_CID,
        "Component Controller Class",
        "Canopy Test Controller",
        "",
    ),
];

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = unsafe { &mut *info };
        copy_cstring("canopy", &mut info.vendor);
        copy_cstring("", &mut info.url);
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as i32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        CLASSES.len() as i32
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let Some((cid, category, name, _)) = CLASSES.get(index as usize) else {
            return kInvalidArgument;
        };
        let info = unsafe { &mut *info };
        info.cid = *cid;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_cstring(category, &mut info.category);
        copy_cstring(name, &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        object: *mut *mut c_void,
    ) -> tresult {
        let cid = unsafe { *(cid as *const TUID) };
        let instance = if cid == GAIN_CID || cid == SINE_CID {
            ComWrapper::new(Processor::new(cid == SINE_CID)).to_com_ptr::<FUnknown>()
        } else if cid == CONTROLLER_CID {
            ComWrapper::new(Controller::new()).to_com_ptr::<FUnknown>()
        } else {
            None
        };
        match instance {
            Some(instance) => unsafe {
                let ptr = instance.as_ptr();
                ((*(*ptr).vtbl).queryInterface)(ptr, iid as *const TUID, object)
            },
            None => kInvalidArgument,
        }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        let Some((cid, category, name, sub)) = CLASSES.get(index as usize) else {
            return kInvalidArgument;
        };
        let info = unsafe { &mut *info };
        info.cid = *cid;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_cstring(category, &mut info.category);
        copy_cstring(name, &mut info.name);
        info.classFlags = 0;
        copy_cstring(sub, &mut info.subCategories);
        copy_cstring("canopy", &mut info.vendor);
        copy_cstring("1.0.0", &mut info.version);
        copy_cstring("VST 3.8.0", &mut info.sdkVersion);
        kResultOk
    }
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn InitDll() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn ExitDll() -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "C" fn ModuleEntry(_handle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "C" fn ModuleExit() -> bool {
    true
}

#[unsafe(no_mangle)]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .map_or(std::ptr::null_mut(), |f| f.into_raw())
}

/// Keeps `Interface` in use for the IID constants above.
#[allow(dead_code)]
fn _iid() -> [u8; 16] {
    IMessage::IID
}
