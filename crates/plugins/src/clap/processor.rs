//! One CLAP plugin on the audio thread.
//!
//! Everything a block needs is built at activation: audio buffers and the
//! `clap_audio_buffer`s pointing at them, room for the block's events, the
//! parameter cookies. A block then only fills those in and calls
//! `process`; nothing here allocates, locks or does I/O.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::*;
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::process::*;

use super::instance::ClapPlugin;
use crate::block::{
    self, Apply, AudioQueues, Change, EVENT_CAPACITY, HeldNotes, Modulation, OutEvent, OutEvents,
    PortBuffers,
};
use crate::thread::AudioThreadScope;
use crate::wake::Wake;
use crate::{AudioConfig, Event, ParamId, ProcessStatus, Transport};

/// The note formats a plugin's first note input understands.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NoteDialect {
    pub clap: bool,
    pub midi: bool,
}

/// Room for any event we send, so a block's events are one flat array.
#[repr(C)]
#[derive(Clone, Copy)]
union InEvent {
    header: clap_event_header,
    note: clap_event_note,
    expression: clap_event_note_expression,
    param: clap_event_param_value,
    param_mod: clap_event_param_mod,
    midi: clap_event_midi,
}

/// What `out_push` writes to while the plugin processes.
struct OutState {
    to_main: rtrb::Producer<OutEvent>,
    wake: Arc<Wake>,
    events: OutEvents,
    modulation: Modulation,
    /// Where the part of the block being processed starts, for frames.
    offset: u32,
}

/// What a [`ClapProcessor`] is built from at activation.
pub(crate) struct ProcessorSetup {
    pub config: AudioConfig,
    pub input_channels: Vec<u32>,
    pub output_channels: Vec<u32>,
    pub main_input: Option<usize>,
    pub aux_input: Option<usize>,
    pub main_output: Option<usize>,
    pub dialect: Option<NoteDialect>,
    /// Parameter ids and their cookies (as addresses), sorted by id.
    pub cookies: Vec<(u32, usize)>,
    pub modulation: Modulation,
}

pub(crate) struct ClapProcessor {
    plugin: Arc<ClapPlugin>,
    config: AudioConfig,
    inputs: PortBuffers,
    outputs: PortBuffers,
    clap_inputs: Vec<clap_audio_buffer>,
    clap_outputs: Vec<clap_audio_buffer>,
    main_input: Option<usize>,
    aux_input: Option<usize>,
    main_output: Option<usize>,
    dialect: Option<NoteDialect>,
    cookies: Vec<(u32, usize)>,
    from_main: rtrb::Consumer<Change>,
    events: Vec<InEvent>,
    out: OutState,
    held: HeldNotes,
    steady_time: i64,
}

// SAFETY: the raw pointers in the clap buffers point into our own
// `PortBuffers`, which move with us; the plugin is Send + Sync.
unsafe impl Send for ClapProcessor {}

impl ClapProcessor {
    pub fn new(
        plugin: Arc<ClapPlugin>,
        setup: ProcessorSetup,
        queues: AudioQueues,
    ) -> ClapProcessor {
        let max_block = setup.config.max_block as usize;
        let mut inputs = PortBuffers::new(&setup.input_channels, max_block);
        let mut outputs = PortBuffers::new(&setup.output_channels, max_block);
        let buffers = |ports: &mut PortBuffers| -> Vec<clap_audio_buffer> {
            (0..ports.port_count())
                .map(|port| clap_audio_buffer {
                    data32: ports.pointers(port),
                    data64: std::ptr::null_mut(),
                    channel_count: ports.channel_count(port) as u32,
                    latency: 0,
                    constant_mask: 0,
                })
                .collect()
        };
        let clap_inputs = buffers(&mut inputs);
        let clap_outputs = buffers(&mut outputs);
        let wake = plugin.host.data().wake.clone();
        ClapProcessor {
            plugin,
            config: setup.config,
            inputs,
            outputs,
            clap_inputs,
            clap_outputs,
            main_input: setup.main_input,
            aux_input: setup.aux_input,
            main_output: setup.main_output,
            dialect: setup.dialect,
            cookies: setup.cookies,
            from_main: queues.from_main,
            events: Vec::with_capacity(EVENT_CAPACITY),
            out: OutState {
                to_main: queues.to_main,
                wake,
                events: OutEvents::new(),
                modulation: setup.modulation,
                offset: 0,
            },
            held: HeldNotes::default(),
            steady_time: 0,
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
        &mut self.out.events
    }

    pub fn sent_events(&self) -> &OutEvents {
        &self.out.events
    }

    pub fn is_processing(&self) -> bool {
        self.plugin.processing.load(Ordering::Acquire)
    }

    /// Changes the main thread sent that no block took yet (main thread,
    /// once the processor is back).
    pub fn take_queued(&mut self) -> Vec<Change> {
        std::iter::from_fn(|| self.from_main.pop().ok()).collect()
    }

    /// `start_processing`, if it isn't yet; false if the plugin refused.
    pub fn start_processing(&mut self) -> bool {
        if self.is_processing() {
            return true;
        }
        let _audio = AudioThreadScope::enter();
        // SAFETY: audio-thread call on an active plugin that isn't processing.
        let started = unsafe {
            self.plugin
                .vt()
                .start_processing
                .is_none_or(|start| start(self.plugin.plugin))
        };
        self.plugin.processing.store(started, Ordering::Release);
        started
    }

    pub fn stop_processing(&mut self) {
        if !self.plugin.processing.swap(false, Ordering::AcqRel) {
            return;
        }
        let _audio = AudioThreadScope::enter();
        // SAFETY: audio-thread call on a processing plugin.
        unsafe {
            if let Some(stop) = self.plugin.vt().stop_processing {
                stop(self.plugin.plugin);
            }
        }
    }

    pub fn reset(&mut self) {
        self.held = HeldNotes::default();
        let _audio = AudioThreadScope::enter();
        // SAFETY: audio-thread call on an active plugin.
        unsafe {
            if let Some(reset) = self.plugin.vt().reset {
                reset(self.plugin.plugin);
            }
        }
    }

    /// Processes `frames` frames of the processor's buffers, with the
    /// events of `events` that fall in `offset..offset + frames`.
    pub fn run(
        &mut self,
        frames: u32,
        events: &[Event],
        offset: u32,
        transport: &Transport,
    ) -> ProcessStatus {
        if frames == 0 {
            return ProcessStatus::Continue;
        }
        if !self.start_processing() {
            return ProcessStatus::Error;
        }
        self.events.clear();
        let mut list = EventList {
            events: &mut self.events,
            held: &mut self.held,
            dialect: self.dialect,
            cookies: &self.cookies,
            modulation: &mut self.out.modulation,
        };
        block::for_each_event(&mut self.from_main, events, offset, frames, |event| {
            list.push(event)
        });
        self.outputs.clear(frames as usize);
        self.out.offset = offset;
        let transport = transport_event(transport, self.config.sample_rate);
        let in_events = clap_input_events {
            ctx: (&self.events as *const Vec<InEvent>).cast_mut().cast(),
            size: Some(in_size),
            get: Some(in_get),
        };
        let out_events = clap_output_events {
            ctx: (&mut self.out as *mut OutState).cast(),
            try_push: Some(out_push),
        };
        let process = clap_process {
            steady_time: self.steady_time,
            frames_count: frames,
            transport: &transport,
            audio_inputs: self.clap_inputs.as_ptr(),
            audio_outputs: self.clap_outputs.as_mut_ptr(),
            audio_inputs_count: self.clap_inputs.len() as u32,
            audio_outputs_count: self.clap_outputs.len() as u32,
            in_events: &in_events,
            out_events: &out_events,
        };
        let _audio = AudioThreadScope::enter();
        // SAFETY: audio-thread call on a processing plugin, with buffers of
        // at least `frames` (≤ max_block) samples and lists that live for
        // the call.
        let status = unsafe {
            self.plugin
                .vt()
                .process
                .map_or(CLAP_PROCESS_ERROR, |process_fn| {
                    process_fn(self.plugin.plugin, &process)
                })
        };
        self.steady_time += i64::from(frames);
        match status {
            CLAP_PROCESS_CONTINUE => ProcessStatus::Continue,
            CLAP_PROCESS_CONTINUE_IF_NOT_QUIET => ProcessStatus::ContinueIfNotQuiet,
            CLAP_PROCESS_TAIL => ProcessStatus::Tail,
            CLAP_PROCESS_SLEEP => ProcessStatus::Sleep,
            _ => ProcessStatus::Error,
        }
    }
}

impl Drop for ClapProcessor {
    fn drop(&mut self) {
        self.stop_processing();
        if self.plugin.host.data().main.is_current() {
            self.plugin.deactivate_now();
        }
    }
}

fn header(type_: u16, frame: u32, size: usize) -> clap_event_header {
    clap_event_header {
        size: size as u32,
        time: frame,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    }
}

/// One block's events as the plugin gets them.
struct EventList<'a> {
    events: &'a mut Vec<InEvent>,
    held: &'a mut HeldNotes,
    dialect: Option<NoteDialect>,
    cookies: &'a [(u32, usize)],
    modulation: &'a mut Modulation,
}

impl EventList<'_> {
    fn add(&mut self, event: InEvent) -> bool {
        if self.events.len() == self.events.capacity() {
            return false;
        }
        self.events.push(event);
        true
    }

    fn cookie(&self, id: ParamId) -> *mut c_void {
        let cookies = self.cookies;
        cookies
            .binary_search_by_key(&id.0, |c| c.0)
            .map_or(0, |i| cookies[i].1) as *mut c_void
    }

    fn param(&mut self, frame: u32, id: ParamId, value: f64) -> bool {
        let cookie = self.cookie(id);
        self.add(InEvent {
            param: clap_event_param_value {
                header: header(
                    CLAP_EVENT_PARAM_VALUE,
                    frame,
                    size_of::<clap_event_param_value>(),
                ),
                param_id: id.0,
                cookie,
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value,
            },
        })
    }

    fn midi(&mut self, frame: u32, data: [u8; 3]) -> bool {
        if !self.dialect.is_some_and(|d| d.midi) {
            return true;
        }
        self.add(InEvent {
            midi: clap_event_midi {
                header: header(CLAP_EVENT_MIDI, frame, size_of::<clap_event_midi>()),
                port_index: 0,
                data,
            },
        })
    }

    fn note(&mut self, event: Event) -> bool {
        let Some(dialect) = self.dialect else {
            return true;
        };
        let (type_, frame, channel, key, velocity) = match event {
            Event::NoteOn {
                frame,
                channel,
                key,
                velocity,
            } => {
                self.held.press(channel, key);
                (CLAP_EVENT_NOTE_ON, frame, channel, key, velocity)
            }
            Event::NoteOff {
                frame,
                channel,
                key,
                velocity,
            } => {
                self.held.release(channel, key);
                (CLAP_EVENT_NOTE_OFF, frame, channel, key, velocity)
            }
            _ => return true,
        };
        if !dialect.clap {
            return block::note_to_midi(event).is_none_or(|data| self.midi(frame, data));
        }
        self.add(InEvent {
            note: clap_event_note {
                header: header(type_, frame, size_of::<clap_event_note>()),
                note_id: -1,
                port_index: 0,
                channel: i16::from(channel & 15),
                key: i16::from(key & 127),
                velocity: f64::from(velocity.clamp(0.0, 1.0)),
            },
        })
    }

    /// A note's tuning, if the plugin takes CLAP notes: MIDI 1.0 has no
    /// per-note pitch.
    fn tuning(&mut self, frame: u32, channel: u8, key: u8, semitones: f64) -> bool {
        if !self.dialect.is_some_and(|d| d.clap) {
            return true;
        }
        self.add(InEvent {
            expression: tuning_event(frame, channel, key, semitones),
        })
    }

    /// Adds one block event; false when the list is full.
    fn push(&mut self, event: Event) -> bool {
        match event {
            Event::Param { frame, id, value } => {
                let value = self.modulation.set(id, value);
                self.param(frame, id, value)
            }
            Event::ParamMod { frame, id, amount } => match self.modulation.modulate(id, amount) {
                Apply::Value(value) => self.param(frame, id, value),
                Apply::Mod(amount) => {
                    let cookie = self.cookie(id);
                    self.add(InEvent {
                        param_mod: clap_event_param_mod {
                            header: header(
                                CLAP_EVENT_PARAM_MOD,
                                frame,
                                size_of::<clap_event_param_mod>(),
                            ),
                            param_id: id.0,
                            cookie,
                            note_id: -1,
                            port_index: -1,
                            channel: -1,
                            key: -1,
                            amount,
                        },
                    })
                }
            },
            Event::NoteOn { .. } | Event::NoteOff { .. } => self.note(event),
            Event::NoteTuning {
                frame,
                channel,
                key,
                semitones,
            } => self.tuning(frame, channel, key, semitones),
            Event::Midi { frame, data } => match block::midi_to_note(frame, data) {
                Some(note) => self.note(note),
                None => self.midi(frame, data),
            },
            Event::AllNotesOff { frame } => {
                let mut held = std::mem::take(self.held);
                let mut room = true;
                held.release_all(|channel, key| {
                    room &= self.note(Event::NoteOff {
                        frame,
                        channel,
                        key,
                        velocity: 0.5,
                    });
                });
                room
            }
        }
    }
}

/// A tuning note expression: the note on `channel` and `key` (any note
/// id, as the host sends notes without one) sounds `semitones` from its
/// own pitch.
fn tuning_event(frame: u32, channel: u8, key: u8, semitones: f64) -> clap_event_note_expression {
    clap_event_note_expression {
        header: header(
            CLAP_EVENT_NOTE_EXPRESSION,
            frame,
            size_of::<clap_event_note_expression>(),
        ),
        expression_id: CLAP_NOTE_EXPRESSION_TUNING,
        note_id: -1,
        port_index: 0,
        channel: i16::from(channel & 15),
        key: i16::from(key & 127),
        value: semitones,
    }
}

fn transport_event(transport: &Transport, sample_rate: f64) -> clap_event_transport {
    let beats = |b: f64| (b * CLAP_BEATTIME_FACTOR as f64).round() as i64;
    let tempo = if transport.tempo > 0.0 {
        transport.tempo
    } else {
        120.0
    };
    // Seconds for beats at the current tempo: the loop's ends.
    let beat_seconds = |b: f64| (b * 60.0 / tempo * CLAP_SECTIME_FACTOR as f64).round() as i64;
    let seconds = transport.sample_position as f64 / sample_rate.max(1.0);
    let mut flags = CLAP_TRANSPORT_HAS_TEMPO
        | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
        | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE
        | CLAP_TRANSPORT_HAS_TIME_SIGNATURE;
    if transport.playing {
        flags |= CLAP_TRANSPORT_IS_PLAYING;
    }
    if transport.recording {
        flags |= CLAP_TRANSPORT_IS_RECORDING;
    }
    let (loop_start, loop_end) = transport.loop_range.unwrap_or((0.0, 0.0));
    if transport.loop_range.is_some() {
        flags |= CLAP_TRANSPORT_IS_LOOP_ACTIVE;
    }
    clap_event_transport {
        header: header(CLAP_EVENT_TRANSPORT, 0, size_of::<clap_event_transport>()),
        flags,
        song_pos_beats: beats(transport.beat),
        song_pos_seconds: (seconds * CLAP_SECTIME_FACTOR as f64).round() as i64,
        tempo,
        tempo_inc: 0.0,
        loop_start_beats: beats(loop_start),
        loop_end_beats: beats(loop_end),
        loop_start_seconds: beat_seconds(loop_start),
        loop_end_seconds: beat_seconds(loop_end),
        bar_start: beats(transport.bar_start),
        bar_number: transport.bar_number,
        tsig_num: transport.time_signature.0.max(1),
        tsig_denom: transport.time_signature.1.max(1),
    }
}

unsafe extern "C" fn in_size(list: *const clap_input_events) -> u32 {
    // SAFETY: `ctx` is the processor's event Vec, alive for the call.
    unsafe { (*(*list).ctx.cast::<Vec<InEvent>>()).len() as u32 }
}

unsafe extern "C" fn in_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    // SAFETY: as above; every InEvent starts with its header.
    unsafe {
        let events = &*(*list).ctx.cast::<Vec<InEvent>>();
        events
            .get(index as usize)
            .map_or(std::ptr::null(), |e| &e.header as *const clap_event_header)
    }
}

/// What a plugin's output event means to the main thread, if anything.
///
/// # Safety
///
/// `event` must be null or a CLAP event whose header's size covers it.
pub(crate) unsafe fn out_event(event: *const clap_event_header) -> Option<OutEvent> {
    // SAFETY: by the caller; each cast is checked against the header's size.
    unsafe {
        let header = event.as_ref()?;
        if header.space_id != CLAP_CORE_EVENT_SPACE_ID {
            return None;
        }
        let fits = |size: usize| header.size as usize >= size;
        match header.type_ {
            CLAP_EVENT_PARAM_VALUE if fits(size_of::<clap_event_param_value>()) => {
                let e = &*event.cast::<clap_event_param_value>();
                Some(OutEvent::Param(ParamId(e.param_id), e.value))
            }
            CLAP_EVENT_PARAM_GESTURE_BEGIN | CLAP_EVENT_PARAM_GESTURE_END
                if fits(size_of::<clap_event_param_gesture>()) =>
            {
                let e = &*event.cast::<clap_event_param_gesture>();
                let begin = header.type_ == CLAP_EVENT_PARAM_GESTURE_BEGIN;
                Some(OutEvent::Gesture(ParamId(e.param_id), begin))
            }
            _ => None,
        }
    }
}

/// A note or MIDI event the plugin sent out, at its own frame.
///
/// # Safety
///
/// As for [`out_event`].
unsafe fn out_note(event: *const clap_event_header) -> Option<Event> {
    // SAFETY: by the caller; each cast is checked against the header's size.
    unsafe {
        let header = event.as_ref()?;
        if header.space_id != CLAP_CORE_EVENT_SPACE_ID {
            return None;
        }
        let fits = |size: usize| header.size as usize >= size;
        let frame = header.time;
        match header.type_ {
            CLAP_EVENT_NOTE_ON | CLAP_EVENT_NOTE_OFF if fits(size_of::<clap_event_note>()) => {
                let e = &*event.cast::<clap_event_note>();
                let channel = u8::try_from(e.channel).unwrap_or(0) & 15;
                let key = u8::try_from(e.key).ok()? & 127;
                let velocity = e.velocity.clamp(0.0, 1.0) as f32;
                Some(if header.type_ == CLAP_EVENT_NOTE_ON {
                    Event::NoteOn {
                        frame,
                        channel,
                        key,
                        velocity,
                    }
                } else {
                    Event::NoteOff {
                        frame,
                        channel,
                        key,
                        velocity,
                    }
                })
            }
            CLAP_EVENT_MIDI if fits(size_of::<clap_event_midi>()) => {
                let data = (*event.cast::<clap_event_midi>()).data;
                Some(block::midi_to_note(frame, data).unwrap_or(Event::Midi { frame, data }))
            }
            _ => None,
        }
    }
}

unsafe extern "C" fn out_push(
    list: *const clap_output_events,
    event: *const clap_event_header,
) -> bool {
    // SAFETY: `ctx` is the processor's OutState, alive and not otherwise
    // borrowed during `process`; events are checked as above.
    unsafe {
        let out = &mut *(*list).ctx.cast::<OutState>();
        if let Some(event) = out_event(event) {
            if let OutEvent::Param(id, value) = event {
                out.modulation.plugin_set(id, value);
            }
            let pushed = out.to_main.push(event).is_ok();
            out.wake.request();
            return pushed;
        }
        if let Some(note) = out_note(event) {
            let frame = note.frame().saturating_add(out.offset);
            return out.events.push(note.at(frame));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a plugin taking notes as `dialect` gets for `events`.
    fn fed(dialect: NoteDialect, events: &[Event]) -> Vec<InEvent> {
        let mut list = Vec::with_capacity(8);
        let mut held = HeldNotes::default();
        let mut modulation = Modulation::new([]);
        let mut feed = EventList {
            events: &mut list,
            held: &mut held,
            dialect: Some(dialect),
            cookies: &[],
            modulation: &mut modulation,
        };
        for &event in events {
            assert!(feed.push(event));
        }
        list
    }

    const BENT: [Event; 2] = [
        Event::NoteOn {
            frame: 0,
            channel: 2,
            key: 60,
            velocity: 1.0,
        },
        Event::NoteTuning {
            frame: 32,
            channel: 2,
            key: 60,
            semitones: -1.5,
        },
    ];

    #[test]
    fn a_notes_tuning_is_a_tuning_note_expression_for_its_key() {
        let both = NoteDialect {
            clap: true,
            midi: true,
        };
        let events = fed(both, &BENT);
        assert_eq!(events.len(), 2);
        // SAFETY: each union member read is the one its header names.
        unsafe {
            let e = events[1].expression;
            assert_eq!(e.header.type_, CLAP_EVENT_NOTE_EXPRESSION);
            assert_eq!(
                e.header.size as usize,
                size_of::<clap_event_note_expression>()
            );
            assert_eq!(e.header.time, 32);
            assert_eq!(e.expression_id, CLAP_NOTE_EXPRESSION_TUNING);
            assert_eq!(e.value, -1.5);
            // The note it tunes, matched as the host sent it.
            let on = events[0].note;
            assert_eq!(on.header.type_, CLAP_EVENT_NOTE_ON);
            assert_eq!((e.note_id, e.port_index), (on.note_id, on.port_index));
            assert_eq!((e.channel, e.key), (on.channel, on.key));
        }
    }

    #[test]
    fn a_plugin_taking_only_midi_hears_no_tuning() {
        let midi = NoteDialect {
            clap: false,
            midi: true,
        };
        let events = fed(midi, &BENT);
        assert_eq!(events.len(), 1, "the note on, as MIDI");
        // SAFETY: the member its header names.
        assert_eq!(unsafe { events[0].header.type_ }, CLAP_EVENT_MIDI);
    }
}
