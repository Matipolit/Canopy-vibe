//! One VST3 plugin on the audio thread.
//!
//! `process` takes COM objects for its parameter changes and events; ours
//! ([`ParamChanges`], [`ParamQueue`], [`EventList`]) are made at activation
//! with all the room they'll get, and a block only clears and refills
//! them. Nothing here allocates, locks or does I/O.

use std::cell::{Cell, UnsafeCell};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use vst3::Steinberg::Vst::Event_::EventTypes_::*;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComWrapper};

use super::instance::Vst3Plugin;
use crate::block::{
    self, Apply, AudioQueues, Change, HeldNotes, Modulation, OutEvent, OutEvents, PortBuffers,
};
use crate::thread::AudioThreadScope;
use crate::{AudioConfig, ParamId, ProcessStatus, Transport};

/// How many parameters one block can change, each way.
const QUEUES: usize = 128;
/// How many points one parameter can have in a block. Past that, the last
/// point takes the newest value, so the parameter still ends up right.
const POINTS: usize = 32;
/// How many events one block can carry, each way.
const EVENTS: usize = 1024;

/// One parameter's changes within a block.
pub(crate) struct ParamQueue {
    id: Cell<ParamID>,
    /// (sample offset, normalized value), sorted by offset.
    points: UnsafeCell<Vec<(i32, f64)>>,
}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl ParamQueue {
    fn new() -> ComWrapper<ParamQueue> {
        ComWrapper::new(ParamQueue {
            id: Cell::new(0),
            points: UnsafeCell::new(Vec::with_capacity(POINTS)),
        })
    }

    #[allow(clippy::mut_from_ref)]
    fn points(&self) -> &mut Vec<(i32, f64)> {
        // SAFETY: only the thread running the processor touches a queue,
        // and never through two references at once: each method below
        // takes and drops its borrow.
        unsafe { &mut *self.points.get() }
    }

    /// Adds a point, keeping offsets sorted; returns its index.
    fn add(&self, offset: i32, value: f64) -> i32 {
        let points = self.points();
        let index = points.partition_point(|p| p.0 < offset);
        if points.get(index).is_some_and(|p| p.0 == offset) {
            points[index].1 = value;
        } else if points.len() < points.capacity() {
            points.insert(index, (offset, value));
        } else {
            let last = points.len() - 1;
            points[last] = (points[last].0.max(offset), value);
            return last as i32;
        }
        index as i32
    }
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }

    unsafe fn getPointCount(&self) -> i32 {
        self.points().len() as i32
    }

    unsafe fn getPoint(&self, index: i32, offset: *mut i32, value: *mut ParamValue) -> tresult {
        let Some(&(o, v)) = usize::try_from(index)
            .ok()
            .and_then(|i| self.points().get(i))
        else {
            return kResultFalse;
        };
        if offset.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null; the plugin gives somewhere to write.
        unsafe {
            *offset = o;
            *value = v;
        }
        kResultTrue
    }

    unsafe fn addPoint(&self, offset: i32, value: ParamValue, index: *mut i32) -> tresult {
        let at = self.add(offset, value);
        if !index.is_null() {
            // SAFETY: checked non-null.
            unsafe { *index = at };
        }
        kResultTrue
    }
}

/// A block's parameter changes, one queue per parameter.
pub(crate) struct ParamChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    used: Cell<usize>,
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl ParamChanges {
    fn new() -> ComWrapper<ParamChanges> {
        ComWrapper::new(ParamChanges {
            queues: (0..QUEUES).map(|_| ParamQueue::new()).collect(),
            used: Cell::new(0),
        })
    }

    fn clear(&self) {
        self.used.set(0);
    }

    /// The queue for `id`, starting one if there's room.
    fn queue(&self, id: ParamID) -> Option<(usize, &ParamQueue)> {
        let used = self.used.get();
        if let Some(i) = self.queues[..used].iter().position(|q| q.id.get() == id) {
            return Some((i, &self.queues[i]));
        }
        let queue = self.queues.get(used)?;
        queue.id.set(id);
        queue.points().clear();
        self.used.set(used + 1);
        Some((used, queue))
    }

    /// Adds a point for `id`; false if the block has no room for another
    /// parameter.
    fn add(&self, id: ParamID, offset: u32, value: f64) -> bool {
        match self.queue(id) {
            Some((_, queue)) => {
                queue.add(offset as i32, value.clamp(0.0, 1.0));
                true
            }
            None => false,
        }
    }

    /// Each changed parameter and its last value.
    fn last_values(&self) -> impl Iterator<Item = (ParamID, f64)> + '_ {
        self.queues[..self.used.get()]
            .iter()
            .filter_map(|q| q.points().last().map(|p| (q.id.get(), p.1)))
    }
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        self.used.get() as i32
    }

    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        usize::try_from(index)
            .ok()
            .filter(|&i| i < self.used.get())
            .and_then(|i| self.queues[i].as_com_ref::<IParamValueQueue>())
            .map_or(std::ptr::null_mut(), |q| q.as_ptr())
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut i32,
    ) -> *mut IParamValueQueue {
        if id.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: checked non-null.
        let Some((i, _)) = self.queue(unsafe { *id }) else {
            return std::ptr::null_mut();
        };
        if !index.is_null() {
            // SAFETY: checked non-null.
            unsafe { *index = i as i32 };
        }
        self.queues[i]
            .as_com_ref::<IParamValueQueue>()
            .map_or(std::ptr::null_mut(), |q| q.as_ptr())
    }
}

/// A block's note events.
pub(crate) struct EventList {
    events: UnsafeCell<Vec<Event>>,
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl EventList {
    fn new() -> ComWrapper<EventList> {
        ComWrapper::new(EventList {
            events: UnsafeCell::new(Vec::with_capacity(EVENTS)),
        })
    }

    #[allow(clippy::mut_from_ref)]
    fn events(&self) -> &mut Vec<Event> {
        // SAFETY: as for ParamQueue::points.
        unsafe { &mut *self.events.get() }
    }

    fn push(&self, event: Event) -> bool {
        let events = self.events();
        if events.len() == events.capacity() {
            return false;
        }
        events.push(event);
        true
    }
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> i32 {
        self.events().len() as i32
    }

    unsafe fn getEvent(&self, index: i32, event: *mut Event) -> tresult {
        let Some(found) = usize::try_from(index)
            .ok()
            .and_then(|i| self.events().get(i))
        else {
            return kResultFalse;
        };
        if event.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null; the plugin gives an Event to fill.
        unsafe { *event = *found };
        kResultOk
    }

    unsafe fn addEvent(&self, event: *mut Event) -> tresult {
        // SAFETY: the plugin passes an Event to copy.
        match unsafe { event.as_ref() } {
            Some(event) if self.push(*event) => kResultOk,
            Some(_) => kOutOfMemory,
            None => kInvalidArgument,
        }
    }
}

/// What a [`Vst3Processor`] is built from at activation.
pub(crate) struct ProcessorSetup {
    pub config: AudioConfig,
    pub input_channels: Vec<u32>,
    pub output_channels: Vec<u32>,
    pub main_input: Option<usize>,
    pub aux_input: Option<usize>,
    pub main_output: Option<usize>,
    pub note_input: bool,
    pub note_output: bool,
    /// The parameter each MIDI controller (0..=127), channel pressure
    /// (128) and pitch bend (129) is mapped to.
    pub midi_map: [Option<ParamID>; 130],
    pub modulation: Modulation,
}

pub(crate) struct Vst3Processor {
    plugin: Arc<Vst3Plugin>,
    config: AudioConfig,
    inputs: PortBuffers,
    outputs: PortBuffers,
    in_buses: Vec<AudioBusBuffers>,
    out_buses: Vec<AudioBusBuffers>,
    main_input: Option<usize>,
    aux_input: Option<usize>,
    main_output: Option<usize>,
    in_params: ComWrapper<ParamChanges>,
    out_params: ComWrapper<ParamChanges>,
    in_events: Option<ComWrapper<EventList>>,
    out_events: Option<ComWrapper<EventList>>,
    midi_map: [Option<ParamID>; 130],
    modulation: Modulation,
    from_main: rtrb::Consumer<Change>,
    to_main: rtrb::Producer<OutEvent>,
    events_out: OutEvents,
    held: HeldNotes,
    context: ProcessContext,
    continuous: i64,
}

// SAFETY: the bus buffers point into our own PortBuffers; the COM objects
// are only touched by whichever thread owns the processor (the plugin may
// keep a reference past `process`, but only refcounts, which are atomic).
unsafe impl Send for Vst3Processor {}

impl Vst3Processor {
    pub fn new(
        plugin: Arc<Vst3Plugin>,
        setup: ProcessorSetup,
        queues: AudioQueues,
    ) -> Vst3Processor {
        let max_block = setup.config.max_block as usize;
        let mut inputs = PortBuffers::new(&setup.input_channels, max_block);
        let mut outputs = PortBuffers::new(&setup.output_channels, max_block);
        let buses = |ports: &mut PortBuffers| -> Vec<AudioBusBuffers> {
            (0..ports.port_count())
                .map(|port| AudioBusBuffers {
                    numChannels: ports.channel_count(port) as i32,
                    silenceFlags: 0,
                    __field0: AudioBusBuffers__type0 {
                        channelBuffers32: ports.pointers(port),
                    },
                })
                .collect()
        };
        let in_buses = buses(&mut inputs);
        let out_buses = buses(&mut outputs);
        Vst3Processor {
            plugin,
            config: setup.config,
            inputs,
            outputs,
            in_buses,
            out_buses,
            main_input: setup.main_input,
            aux_input: setup.aux_input,
            main_output: setup.main_output,
            in_params: ParamChanges::new(),
            out_params: ParamChanges::new(),
            in_events: setup.note_input.then(EventList::new),
            out_events: setup.note_output.then(EventList::new),
            midi_map: setup.midi_map,
            modulation: setup.modulation,
            from_main: queues.from_main,
            to_main: queues.to_main,
            events_out: OutEvents::new(),
            held: HeldNotes::default(),
            // SAFETY: a plain C struct for which all zeroes is valid.
            context: unsafe { std::mem::zeroed() },
            continuous: 0,
        }
    }

    pub fn config(&self) -> AudioConfig {
        self.config
    }

    pub fn buffers(&mut self) -> (&mut PortBuffers, &mut PortBuffers) {
        (&mut self.inputs, &mut self.outputs)
    }

    pub fn port_buffers(&self) -> (&PortBuffers, &PortBuffers) {
        (&self.inputs, &self.outputs)
    }

    /// The main input, the sidechain input and the main output.
    pub fn ports(&self) -> (Option<usize>, Option<usize>, Option<usize>) {
        (self.main_input, self.aux_input, self.main_output)
    }

    pub fn out_events(&mut self) -> &mut OutEvents {
        &mut self.events_out
    }

    pub fn sent_events(&self) -> &OutEvents {
        &self.events_out
    }

    pub fn is_processing(&self) -> bool {
        self.plugin.processing.load(Ordering::Acquire)
    }

    /// Changes the main thread sent that no block took yet (main thread,
    /// once the processor is back).
    pub fn take_queued(&mut self) -> Vec<Change> {
        std::iter::from_fn(|| self.from_main.pop().ok()).collect()
    }

    pub fn start_processing(&mut self) -> bool {
        if !self.plugin.processing.swap(true, Ordering::AcqRel) {
            let _audio = AudioThreadScope::enter();
            // SAFETY: an active component; the SDK allows this call on the
            // audio thread. Plugins that don't implement it still process.
            unsafe { self.plugin.processor.setProcessing(1) };
        }
        true
    }

    pub fn stop_processing(&mut self) {
        if self.plugin.processing.swap(false, Ordering::AcqRel) {
            let _audio = AudioThreadScope::enter();
            // SAFETY: as above.
            unsafe { self.plugin.processor.setProcessing(0) };
        }
    }

    /// Forgets what's ringing: VST3 has no reset call, but turning
    /// processing off and on again is what it documents for clearing
    /// delay lines and reverbs.
    pub fn reset(&mut self) {
        self.held = HeldNotes::default();
        if self.is_processing() {
            self.stop_processing();
            self.start_processing();
        }
    }

    // VST3's enum constants are u32 on Linux and i32 on Windows.
    #[allow(clippy::unnecessary_cast)]
    pub fn run(
        &mut self,
        frames: u32,
        events: &[crate::Event],
        offset: u32,
        transport: &Transport,
    ) -> ProcessStatus {
        if frames == 0 {
            return ProcessStatus::Continue;
        }
        self.start_processing();
        self.in_params.clear();
        self.out_params.clear();
        for list in [&self.in_events, &self.out_events].into_iter().flatten() {
            list.events().clear();
        }
        let mut feed = Feed {
            params: &self.in_params,
            events: self.in_events.as_deref(),
            held: &mut self.held,
            midi_map: &self.midi_map,
            modulation: &mut self.modulation,
        };
        block::for_each_event(&mut self.from_main, events, offset, frames, |event| {
            feed.push(event)
        });
        self.outputs.clear(frames as usize);
        for bus in &mut self.out_buses {
            bus.silenceFlags = 0;
        }
        self.context = process_context(transport, self.config.sample_rate, self.continuous);
        let pointer = |list: &Option<ComWrapper<EventList>>| {
            list.as_ref()
                .and_then(|l| l.as_com_ref::<IEventList>())
                .map_or(std::ptr::null_mut(), |l| l.as_ptr())
        };
        let mode = if self.config.offline {
            ProcessModes_::kOffline
        } else {
            ProcessModes_::kRealtime
        };
        let mut data = ProcessData {
            processMode: mode as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: frames as i32,
            numInputs: self.in_buses.len() as i32,
            numOutputs: self.out_buses.len() as i32,
            inputs: self.in_buses.as_mut_ptr(),
            outputs: self.out_buses.as_mut_ptr(),
            inputParameterChanges: self
                .in_params
                .as_com_ref::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |p| p.as_ptr()),
            outputParameterChanges: self
                .out_params
                .as_com_ref::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |p| p.as_ptr()),
            inputEvents: pointer(&self.in_events),
            outputEvents: pointer(&self.out_events),
            processContext: &mut self.context,
        };
        let result = {
            let _audio = AudioThreadScope::enter();
            // SAFETY: an active, processing component, with buffers of at
            // least `frames` (≤ max_block) samples per channel and lists
            // that live for the call.
            unsafe { self.plugin.processor.process(&mut data) }
        };
        self.continuous += i64::from(frames);
        let mut changed = false;
        for (id, value) in self.out_params.last_values() {
            let id = ParamId(id);
            self.modulation.plugin_set(id, value);
            changed |= self.to_main.push(OutEvent::Param(id, value)).is_ok();
        }
        if changed {
            self.plugin.wake.request();
        }
        if let Some(list) = &self.out_events {
            for event in list.events().iter() {
                if let Some(event) = out_event(event, offset) {
                    self.events_out.push(event);
                }
            }
        }
        if result == kResultOk {
            ProcessStatus::Continue
        } else {
            ProcessStatus::Error
        }
    }
}

impl Drop for Vst3Processor {
    fn drop(&mut self) {
        self.stop_processing();
        if self.plugin.main.is_current() {
            self.plugin.deactivate_now();
        }
    }
}

/// Gives an active plugin's processor `changes` in a block of no samples,
/// on the main thread: how VST3 flushes parameters outside processing.
// VST3's enum constants are u32 on Linux and i32 on Windows.
#[allow(clippy::unnecessary_cast)]
pub(crate) fn flush(plugin: &Vst3Plugin, changes: &[Change]) {
    let params = ParamChanges::new();
    for change in changes {
        params.add(change.id.0, 0, change.value);
    }
    let Some(params) = params.as_com_ref::<IParameterChanges>() else {
        return;
    };
    // SAFETY: a zeroed context is valid; no buses, no samples.
    let mut context: ProcessContext = unsafe { std::mem::zeroed() };
    let mut data = ProcessData {
        processMode: ProcessModes_::kRealtime as i32,
        symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
        numSamples: 0,
        numInputs: 0,
        numOutputs: 0,
        inputs: std::ptr::null_mut(),
        outputs: std::ptr::null_mut(),
        inputParameterChanges: params.as_ptr(),
        outputParameterChanges: std::ptr::null_mut(),
        inputEvents: std::ptr::null_mut(),
        outputEvents: std::ptr::null_mut(),
        processContext: &mut context,
    };
    let _audio = AudioThreadScope::enter();
    // SAFETY: the plugin is active and nothing else processes it (its
    // processor doesn't exist yet), so this thread may act as the audio
    // thread.
    unsafe {
        plugin.processor.setProcessing(1);
        plugin.processor.process(&mut data);
        plugin.processor.setProcessing(0);
    }
}

/// Fills a block's parameter changes and events.
struct Feed<'a> {
    params: &'a ParamChanges,
    events: Option<&'a EventList>,
    held: &'a mut HeldNotes,
    midi_map: &'a [Option<ParamID>; 130],
    modulation: &'a mut Modulation,
}

impl Feed<'_> {
    fn note(&mut self, frame: u32, on: bool, channel: u8, key: u8, velocity: f32) -> bool {
        let Some(events) = self.events else {
            return true;
        };
        let (channel, key) = (channel & 15, key & 127);
        let field = if on {
            self.held.press(channel, key);
            Event__type0 {
                noteOn: NoteOnEvent {
                    channel: i16::from(channel),
                    pitch: i16::from(key),
                    tuning: 0.0,
                    velocity: velocity.clamp(0.0, 1.0),
                    length: 0,
                    noteId: -1,
                },
            }
        } else {
            self.held.release(channel, key);
            Event__type0 {
                noteOff: NoteOffEvent {
                    channel: i16::from(channel),
                    pitch: i16::from(key),
                    velocity: velocity.clamp(0.0, 1.0),
                    noteId: -1,
                    tuning: 0.0,
                },
            }
        };
        events.push(Event {
            busIndex: 0,
            sampleOffset: frame as i32,
            ppqPosition: 0.0,
            flags: Event_::EventFlags_::kIsLive as u16,
            r#type: if on { kNoteOnEvent } else { kNoteOffEvent } as u16,
            __field0: field,
        })
    }

    /// A controller as the parameter the plugin maps it to, if any.
    fn controller(&mut self, frame: u32, number: usize, normalized: f64) -> bool {
        match self.midi_map.get(number).copied().flatten() {
            Some(id) => self.param(frame, ParamId(id), normalized),
            None => true,
        }
    }

    fn param(&mut self, frame: u32, id: ParamId, value: f64) -> bool {
        let value = self.modulation.set(id, value);
        self.params.add(id.0, frame, value)
    }

    fn push(&mut self, event: crate::Event) -> bool {
        use crate::Event as E;
        match event {
            E::Param { frame, id, value } => self.param(frame, id, value),
            E::ParamMod { frame, id, amount } => match self.modulation.modulate(id, amount) {
                Apply::Value(value) => self.params.add(id.0, frame, value),
                // No such parameter: nothing to modulate.
                Apply::Mod(_) => true,
            },
            E::NoteOn {
                frame,
                channel,
                key,
                velocity,
            } => self.note(frame, true, channel, key, velocity),
            E::NoteOff {
                frame,
                channel,
                key,
                velocity,
            } => self.note(frame, false, channel, key, velocity),
            E::AllNotesOff { frame } => {
                let mut held = std::mem::take(self.held);
                let mut room = true;
                held.release_all(|channel, key| room &= self.note(frame, false, channel, key, 0.5));
                room
            }
            E::Midi { frame, data } => {
                if let Some(note) = block::midi_to_note(frame, data) {
                    return self.push(note);
                }
                match data[0] & 0xF0 {
                    0xB0 => self.controller(
                        frame,
                        usize::from(data[1] & 127),
                        f64::from(data[2] & 127) / 127.0,
                    ),
                    0xD0 => self.controller(frame, 128, f64::from(data[1] & 127) / 127.0),
                    0xE0 => {
                        let bend = (u16::from(data[2] & 127) << 7) | u16::from(data[1] & 127);
                        self.controller(frame, 129, f64::from(bend) / 16383.0)
                    }
                    _ => true,
                }
            }
        }
    }
}

/// A note or controller the plugin sent, at its frame in the whole block.
// Event types are u32 here and i32 on Windows.
#[allow(clippy::unnecessary_cast)]
fn out_event(event: &Event, offset: u32) -> Option<crate::Event> {
    let frame = u32::try_from(event.sampleOffset).unwrap_or(0) + offset;
    let byte = |v: i16| u8::try_from(v.clamp(0, 127)).unwrap_or(0);
    // SAFETY: the union member read is the one `type` names.
    unsafe {
        match u32::from(event.r#type) {
            t if t == kNoteOnEvent as u32 => Some(crate::Event::NoteOn {
                frame,
                channel: byte(event.__field0.noteOn.channel) & 15,
                key: byte(event.__field0.noteOn.pitch),
                velocity: event.__field0.noteOn.velocity,
            }),
            t if t == kNoteOffEvent as u32 => Some(crate::Event::NoteOff {
                frame,
                channel: byte(event.__field0.noteOff.channel) & 15,
                key: byte(event.__field0.noteOff.pitch),
                velocity: event.__field0.noteOff.velocity,
            }),
            t if t == kLegacyMIDICCOutEvent as u32 => {
                let cc = event.__field0.midiCCOut;
                let channel = (cc.channel as u8) & 15;
                let (value, value2) = ((cc.value as u8) & 127, (cc.value2 as u8) & 127);
                let data = match cc.controlNumber {
                    n @ 0..=127 => [0xB0 | channel, n, value],
                    128 => [0xD0 | channel, value, 0],
                    129 => [0xE0 | channel, value, value2],
                    _ => return None,
                };
                Some(crate::Event::Midi { frame, data })
            }
            _ => None,
        }
    }
}

// The state flags are u32 here and i32 on Windows.
#[allow(clippy::unnecessary_cast)]
fn process_context(transport: &Transport, sample_rate: f64, continuous: i64) -> ProcessContext {
    use ProcessContext_::StatesAndFlags_::*;
    // SAFETY: a plain C struct for which all zeroes is valid.
    let mut context: ProcessContext = unsafe { std::mem::zeroed() };
    let mut state =
        kTempoValid | kTimeSigValid | kProjectTimeMusicValid | kBarPositionValid | kContTimeValid;
    if transport.playing {
        state |= kPlaying;
    }
    if transport.recording {
        state |= kRecording;
    }
    if let Some((start, end)) = transport.loop_range {
        state |= kCycleActive | kCycleValid;
        context.cycleStartMusic = start;
        context.cycleEndMusic = end;
    }
    context.state = state as u32;
    context.sampleRate = sample_rate;
    context.projectTimeSamples = transport.sample_position;
    context.continousTimeSamples = continuous;
    context.projectTimeMusic = transport.beat;
    context.barPositionMusic = transport.bar_start;
    context.tempo = if transport.tempo > 0.0 {
        transport.tempo
    } else {
        120.0
    };
    context.timeSigNumerator = i32::from(transport.time_signature.0.max(1));
    context.timeSigDenominator = i32::from(transport.time_signature.1.max(1));
    context
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queues_keep_points_sorted_and_bounded() {
        let changes = ParamChanges::new();
        assert!(changes.add(7, 10, 0.5));
        assert!(changes.add(7, 2, 0.25));
        assert!(changes.add(7, 10, 0.75), "same offset replaces");
        let queue = &changes.queues[0];
        assert_eq!(*queue.points(), vec![(2, 0.25), (10, 0.75)]);
        for offset in 0..100 {
            changes.add(9, offset, f64::from(offset) / 100.0);
        }
        assert_eq!(changes.queues[1].points().len(), POINTS);
        let last: Vec<_> = changes.last_values().collect();
        assert_eq!(last, vec![(7, 0.75), (9, 0.99)]);
        for id in 0..QUEUES as u32 {
            changes.add(1000 + id, 0, 0.0);
        }
        assert!(!changes.add(5000, 0, 0.0), "no room for another parameter");
        changes.clear();
        // SAFETY: plain query.
        assert_eq!(unsafe { changes.getParameterCount() }, 0);
    }
}
