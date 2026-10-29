//! One VST2 plugin on the audio thread.
//!
//! VST2 changes parameters with immediate `setParameter` calls, so a block
//! is processed in parts split at its parameter changes: each part gets its
//! MIDI (`effProcessEvents`, offsets counted from the part's start) and its
//! own `processReplacing`, and parameters change at their exact frames.
//! Hosts' automation comes in blocks of a few dozen frames, so the parts
//! stay few.
//!
//! Everything a block needs is built at activation: the channel pointers,
//! the event list and its MIDI events, and the [`AudioSink`] the host
//! callback writes to while the plugin runs. Nothing here allocates, locks
//! or does I/O.

use std::cell::UnsafeCell;
use std::sync::Arc;

use super::abi::*;
use super::host::AudioSink;
use super::instance::Vst2Plugin;
use crate::block::{
    self, Apply, AudioQueues, Change, EVENT_CAPACITY, HeldNotes, Modulation, OutEvents, PortBuffers,
};
use crate::thread::AudioThreadScope;
use crate::{AudioConfig, Event, ProcessStatus, Transport};

/// What a [`Vst2Processor`] is built from at activation.
pub(crate) struct ProcessorSetup {
    pub config: AudioConfig,
    pub input_channels: Vec<u32>,
    pub output_channels: Vec<u32>,
    pub main_input: Option<usize>,
    pub aux_input: Option<usize>,
    pub main_output: Option<usize>,
    pub modulation: Modulation,
}

/// A part's MIDI as `effProcessEvents` takes it: a `VstEvents` with room
/// for every event (the format declares two pointers; the list is as long
/// as it needs to be).
#[repr(C)]
struct EventList {
    num_events: i32,
    reserved: isize,
    events: [*mut VstEvent; EVENT_CAPACITY],
}

pub(crate) struct Vst2Processor {
    plugin: Arc<Vst2Plugin>,
    config: AudioConfig,
    inputs: PortBuffers,
    outputs: PortBuffers,
    main_input: Option<usize>,
    aux_input: Option<usize>,
    main_output: Option<usize>,
    /// Each channel's buffer in the plugin's channel order (at least one
    /// per side), and the pointers for the part being processed.
    input_starts: Vec<*mut f32>,
    output_starts: Vec<*mut f32>,
    input_ptrs: Vec<*mut f32>,
    output_ptrs: Vec<*mut f32>,
    /// The channel counts the buffers were made for.
    channel_counts: (i32, i32),
    /// Stands in for a side with no channels: some plugins touch the first
    /// input or output whether they have one or not.
    spare: Vec<f32>,
    from_main: rtrb::Consumer<Change>,
    /// The plugin's own edits from other threads, as bases for emulated
    /// modulation.
    bases: rtrb::Consumer<Change>,
    /// The block's events, rebased to the part of the block being run.
    pending: Vec<Event>,
    list: Box<EventList>,
    midi: Box<[VstMidiEvent]>,
    sink: Box<UnsafeCell<AudioSink>>,
    held: HeldNotes,
    reset_requested: bool,
    was_playing: Option<bool>,
}

// SAFETY: the raw pointers point into our own buffers (the PortBuffers,
// `spare`, `midi`), which move with us; the plugin is Send + Sync.
unsafe impl Send for Vst2Processor {}

/// Every channel's first sample, port by port; the spare if there are
/// none.
fn channel_starts(ports: &mut PortBuffers, spare: *mut f32) -> Vec<*mut f32> {
    let mut starts = Vec::new();
    for port in 0..ports.port_count() {
        for channel in 0..ports.channel_count(port) {
            if let Some(samples) = ports.channel_mut(port, channel) {
                starts.push(samples.as_mut_ptr());
            }
        }
    }
    if starts.is_empty() {
        starts.push(spare);
    }
    starts
}

impl Vst2Processor {
    pub fn new(
        plugin: Arc<Vst2Plugin>,
        setup: ProcessorSetup,
        queues: AudioQueues,
        bases: rtrb::Consumer<Change>,
    ) -> Vst2Processor {
        let max_block = setup.config.max_block as usize;
        let mut inputs = PortBuffers::new(&setup.input_channels, max_block);
        let mut outputs = PortBuffers::new(&setup.output_channels, max_block);
        let mut spare = vec![0.0f32; 2 * max_block];
        let spare_input = spare.as_mut_ptr();
        // SAFETY: the second half of `spare`.
        let spare_output = unsafe { spare_input.add(max_block) };
        let input_starts = channel_starts(&mut inputs, spare_input);
        let output_starts = channel_starts(&mut outputs, spare_output);
        let count = |channels: &[u32]| channels.iter().sum::<u32>() as i32;
        Vst2Processor {
            config: setup.config,
            main_input: setup.main_input,
            aux_input: setup.aux_input,
            main_output: setup.main_output,
            channel_counts: (count(&setup.input_channels), count(&setup.output_channels)),
            input_ptrs: input_starts.clone(),
            output_ptrs: output_starts.clone(),
            input_starts,
            output_starts,
            inputs,
            outputs,
            spare,
            from_main: queues.from_main,
            bases,
            pending: Vec::with_capacity(EVENT_CAPACITY),
            list: Box::new(EventList {
                num_events: 0,
                reserved: 0,
                events: [std::ptr::null_mut(); EVENT_CAPACITY],
            }),
            midi: vec![midi_event(0, [0; 3]); EVENT_CAPACITY].into_boxed_slice(),
            sink: Box::new(UnsafeCell::new(AudioSink {
                to_main: queues.to_main,
                events: OutEvents::new(),
                modulation: setup.modulation,
                time: VstTimeInfo::default(),
                frame: 0,
            })),
            held: HeldNotes::default(),
            reset_requested: false,
            was_playing: None,
            plugin,
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
        // SAFETY: the sink is only touched by this processor's thread, and
        // by the plugin only during calls made from `run`.
        unsafe { &mut (*self.sink.get()).events }
    }

    pub fn sent_events(&self) -> &OutEvents {
        // SAFETY: as above; `&self` means no `run` is going on.
        unsafe { &(*self.sink.get()).events }
    }

    /// Changes the main thread sent that no block took yet (main thread,
    /// once the processor is back).
    pub fn take_queued(&mut self) -> Vec<Change> {
        std::iter::from_fn(|| self.from_main.pop().ok()).collect()
    }

    /// Nothing to do: VST2's start and stop calls go with activation, on
    /// the main thread (see the module docs of `vst2`).
    pub fn start_processing(&mut self) -> bool {
        true
    }

    pub fn stop_processing(&mut self) {}

    /// VST2 has no real-time reset (suspending and resuming isn't one:
    /// plugins allocate there), so the next block starts by ending every
    /// held note and sending All Sound Off and All Notes Off on every
    /// channel. Voices stop; an effect's delay lines and reverb tails ring
    /// on.
    pub fn reset(&mut self) {
        self.reset_requested = true;
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
        let Some(process) = self.plugin.process_replacing() else {
            return ProcessStatus::Error;
        };
        let (inputs, outputs) = self.plugin.channels();
        if inputs > self.channel_counts.0 || outputs > self.channel_counts.1 {
            // Its channels grew since activation: it waits for the restart
            // it asked for.
            return ProcessStatus::Error;
        }
        let _audio = AudioThreadScope::enter();
        let sink = self.sink.get();
        // SAFETY (each `(*sink)` below): the sink is ours, and the plugin
        // only reaches it during the calls made further down, when no
        // reference to it is held here.
        while let Ok(change) = self.bases.pop() {
            unsafe { (*sink).modulation.plugin_set(change.id, change.value) };
        }
        self.pending.clear();
        if std::mem::take(&mut self.reset_requested) {
            self.pending.push(Event::AllNotesOff { frame: 0 });
            for channel in 0..16 {
                self.pending.push(Event::Midi {
                    frame: 0,
                    data: [0xB0 | channel, 120, 0],
                });
            }
        }
        let pending = &mut self.pending;
        block::for_each_event(&mut self.from_main, events, offset, frames, |event| {
            if pending.len() == pending.capacity() {
                return false;
            }
            pending.push(event);
            true
        });
        self.outputs.clear(frames as usize);
        self.spare.fill(0.0);
        let changed = self.was_playing != Some(transport.playing);
        self.was_playing = Some(transport.playing);
        let effect = self.plugin.effect();
        let _published = self.plugin.shared.publish(sink);

        let mut start = 0;
        let mut next = 0;
        while start < frames {
            // The part runs until the next parameter change.
            let end = self.pending[next..]
                .iter()
                .filter(|e| matches!(e, Event::Param { .. } | Event::ParamMod { .. }))
                .map(Event::frame)
                .find(|&frame| frame > start)
                .unwrap_or(frames)
                .min(frames);
            let mut midi = Midi {
                events: &mut self.midi,
                count: 0,
                start,
            };
            while let Some(&event) = self.pending.get(next)
                && event.frame() < end
            {
                next += 1;
                match event {
                    Event::Param { id, value, .. } => {
                        let value = unsafe { (*sink).modulation.set(id, value) };
                        self.plugin.set_parameter(id.0, value as f32);
                    }
                    Event::ParamMod { id, amount, .. } => {
                        if let Apply::Value(value) =
                            unsafe { (*sink).modulation.modulate(id, amount) }
                        {
                            self.plugin.set_parameter(id.0, value as f32);
                        }
                    }
                    _ => {
                        feed(&mut midi, &mut self.held, event);
                    }
                }
            }
            let count = midi.count;
            if count > 0 {
                let first = self.midi.as_mut_ptr();
                for (i, slot) in self.list.events[..count].iter_mut().enumerate() {
                    // SAFETY: `i < count <= EVENT_CAPACITY`, within `midi`.
                    *slot = unsafe { first.add(i) }.cast();
                }
                self.list.num_events = count as i32;
                // SAFETY: an audio-thread call with a list of `count` MIDI
                // events that stay put until the next part.
                unsafe {
                    self.plugin.dispatch(
                        effProcessEvents,
                        0,
                        0,
                        (&mut *self.list as *mut EventList).cast(),
                        0.0,
                    )
                };
            }
            unsafe {
                (*sink).frame = offset + start;
                (*sink).time = time_info(
                    transport,
                    self.config.sample_rate,
                    changed && start == 0,
                    offset + start,
                );
            }
            let at = start as usize;
            for (ptr, first) in self.input_ptrs.iter_mut().zip(&self.input_starts) {
                // SAFETY: `at < frames <= max_block`, within each buffer.
                *ptr = unsafe { first.add(at) };
            }
            for (ptr, first) in self.output_ptrs.iter_mut().zip(&self.output_starts) {
                // SAFETY: as above.
                *ptr = unsafe { first.add(at) };
            }
            // SAFETY: an audio-thread call on a resumed plugin, with a
            // pointer for every channel it has (as many as at activation,
            // checked above), each with at least `end - start` samples from
            // there, and its MIDI given just before.
            unsafe {
                process(
                    effect,
                    self.input_ptrs.as_mut_ptr(),
                    self.output_ptrs.as_mut_ptr(),
                    (end - start) as i32,
                )
            };
            start = end;
        }
        ProcessStatus::Continue
    }
}

impl Drop for Vst2Processor {
    fn drop(&mut self) {
        if self.plugin.main.is_current() {
            self.plugin.deactivate_now();
        }
    }
}

/// Fills a part's MIDI events.
struct Midi<'a> {
    events: &'a mut [VstMidiEvent],
    count: usize,
    /// The part's first frame: offsets count from it.
    start: u32,
}

impl Midi<'_> {
    /// Adds a message at `frame`; false if the part is full.
    fn add(&mut self, frame: u32, data: [u8; 3]) -> bool {
        let Some(slot) = self.events.get_mut(self.count) else {
            return false;
        };
        *slot = midi_event(frame.saturating_sub(self.start), data);
        self.count += 1;
        true
    }
}

fn midi_event(delta: u32, data: [u8; 3]) -> VstMidiEvent {
    let note_off = data[0] & 0xF0 == 0x80;
    VstMidiEvent {
        type_: kVstMidiType,
        byte_size: size_of::<VstMidiEvent>() as i32,
        delta_frames: delta.min(i32::MAX as u32) as i32,
        flags: 0,
        note_length: 0,
        note_offset: 0,
        midi_data: [data[0], data[1] & 127, data[2] & 127, 0],
        detune: 0,
        note_off_velocity: if note_off { data[2] & 127 } else { 0 },
        reserved1: 0,
        reserved2: 0,
    }
}

/// Adds a block event's MIDI to the part, keeping `held` up to date; false
/// when the part is full. All notes off ends the held notes and sends All
/// Notes Off (CC 123) on every channel, for notes the plugin holds on its
/// own (a sustain, an arpeggio).
fn feed(midi: &mut Midi, held: &mut HeldNotes, event: Event) -> bool {
    match event {
        Event::NoteOn {
            frame,
            channel,
            key,
            ..
        } => {
            held.press(channel, key);
            block::note_to_midi(event).is_none_or(|data| midi.add(frame, data))
        }
        Event::NoteOff {
            frame,
            channel,
            key,
            ..
        } => {
            held.release(channel, key);
            block::note_to_midi(event).is_none_or(|data| midi.add(frame, data))
        }
        Event::Midi { frame, data } => match block::midi_to_note(frame, data) {
            Some(note) => feed(midi, held, note),
            // Channel messages only.
            None if (0x80..0xF0).contains(&data[0]) => midi.add(frame, data),
            None => true,
        },
        Event::AllNotesOff { frame } => {
            let mut room = true;
            std::mem::take(held).release_all(|channel, key| {
                room &= midi.add(frame, [0x80 | channel, key, 64]);
            });
            for channel in 0..16 {
                room &= midi.add(frame, [0xB0 | channel, 123, 0]);
            }
            room
        }
        // VST2 has no per-note pitch.
        Event::Param { .. } | Event::ParamMod { .. } | Event::NoteTuning { .. } => true,
    }
}

/// The transport `frames_in` frames into the block (where the part being
/// processed starts), as `audioMasterGetTime` gives it.
fn time_info(
    transport: &Transport,
    sample_rate: f64,
    changed: bool,
    frames_in: u32,
) -> VstTimeInfo {
    let tempo = if transport.tempo > 0.0 {
        transport.tempo
    } else {
        120.0
    };
    let advance = if transport.playing {
        f64::from(frames_in)
    } else {
        0.0
    };
    let mut flags = kVstTempoValid | kVstTimeSigValid | kVstPpqPosValid | kVstBarsValid;
    if changed {
        flags |= kVstTransportChanged;
    }
    if transport.playing {
        flags |= kVstTransportPlaying;
    }
    if transport.recording {
        flags |= kVstTransportRecording;
    }
    let (cycle_start, cycle_end) = transport.loop_range.unwrap_or((0.0, 0.0));
    if transport.loop_range.is_some() {
        flags |= kVstTransportCycleActive | kVstCyclePosValid;
    }
    VstTimeInfo {
        sample_pos: transport.sample_position as f64 + advance,
        sample_rate,
        nano_seconds: 0.0,
        ppq_pos: transport.beat + advance / sample_rate.max(1.0) * tempo / 60.0,
        tempo,
        bar_start_pos: transport.bar_start,
        cycle_start_pos: cycle_start,
        cycle_end_pos: cycle_end,
        time_sig_numerator: i32::from(transport.time_signature.0.max(1)),
        time_sig_denominator: i32::from(transport.time_signature.1.max(1)),
        smpte_offset: 0,
        smpte_frame_rate: 0,
        samples_to_next_clock: 0,
        flags,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_notes_off_ends_held_notes_and_tells_every_channel() {
        let mut storage = vec![midi_event(0, [0; 3]); 64];
        let mut midi = Midi {
            events: &mut storage,
            count: 0,
            start: 10,
        };
        let mut held = HeldNotes::default();
        let on = Event::NoteOn {
            frame: 12,
            channel: 2,
            key: 60,
            velocity: 1.0,
        };
        assert!(feed(&mut midi, &mut held, on));
        assert!(feed(
            &mut midi,
            &mut held,
            Event::Midi {
                frame: 12,
                data: [0xF8, 0, 0]
            }
        ));
        assert!(feed(&mut midi, &mut held, Event::AllNotesOff { frame: 20 }));
        assert_eq!(midi.count, 1 + 1 + 16, "realtime bytes are left out");
        assert_eq!(storage[0].delta_frames, 2);
        assert_eq!(storage[0].midi_data, [0x92, 60, 127, 0]);
        assert_eq!(storage[1].midi_data, [0x82, 60, 64, 0]);
        assert_eq!(storage[1].note_off_velocity, 64);
        assert_eq!(storage[17].midi_data, [0xBF, 123, 0, 0]);
        held.release_all(|_, _| panic!("nothing is held"));
    }

    #[test]
    fn the_transport_moves_on_within_a_block_only_while_playing() {
        let transport = Transport {
            playing: true,
            tempo: 120.0,
            sample_position: 1000,
            beat: 4.0,
            loop_range: Some((0.0, 8.0)),
            ..Transport::STOPPED
        };
        let time = time_info(&transport, 48_000.0, true, 24_000);
        assert_eq!(time.sample_pos, 25_000.0);
        assert!(
            (time.ppq_pos - 5.0).abs() < 1e-12,
            "half a second at 120 BPM"
        );
        assert_ne!(time.flags & kVstTransportPlaying, 0);
        assert_ne!(time.flags & kVstTransportChanged, 0);
        assert_eq!(time.cycle_end_pos, 8.0);
        let stopped = time_info(&Transport::STOPPED, 48_000.0, false, 100);
        assert_eq!((stopped.sample_pos, stopped.ppq_pos), (0.0, 0.0));
        assert_eq!(stopped.flags & kVstTransportPlaying, 0);
    }
}
