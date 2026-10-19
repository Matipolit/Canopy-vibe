//! The instrument on the audio thread, the way canopy's engine runs it:
//! the host's blocks cut into blocks of at most [`MAX_BLOCK`] frames,
//! parameter changes applied at the start of the block they fall in (as
//! the engine applies automation, and `set_param` glides anyway), and the
//! sustain pedal held back from the instrument, which never sees it in
//! canopy either. So a block size of no consequence to the host costs
//! nothing extra: dense automation doesn't cut the instrument's blocks
//! shorter, and the instrument sounds as it does in a canopy song.
//!
//! Nothing here touches the C ABI; `plugin.rs` turns CLAP's events and
//! buffers into [`Input`]s and slices.

use std::sync::Arc;

use dsp::MAX_BLOCK;
use instruments::Instrument;
use midi::{MidiMessage, NoteEvent, NoteEventKind, Pitch, SUSTAIN_PEDAL, Velocity};

use crate::values::{Values, to_host, to_plain};

/// How many note events one engine block can take; past that, a block's
/// events are dropped (64 frames holding over a thousand notes is no
/// performance anyone plays).
pub const MAX_EVENTS: usize = 1024;

/// The tempo a song is at until the host says (canopy's default too).
const DEFAULT_TEMPO: f64 = 120.0;

/// MIDI controllers that end every note: All Sound Off and All Notes Off.
const ALL_SOUND_OFF: u8 = 120;
const ALL_NOTES_OFF: u8 = 123;

/// One of the host's events, at `frame` in its block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    /// `velocity` is `0..=1`.
    NoteOn { frame: u32, key: u8, velocity: f64 },
    /// `key` of `None` is every key (CLAP's wildcard).
    NoteOff {
        frame: u32,
        key: Option<u8>,
        velocity: f64,
    },
    /// Ends a note at once, whatever the pedal says.
    Choke { frame: u32, key: Option<u8> },
    /// A MIDI 1.0 channel message.
    Midi { frame: u32, data: [u8; 3] },
    /// Sets parameter `index` to `value`, in the host's units.
    Param {
        frame: u32,
        index: usize,
        value: f64,
    },
    /// Moves parameter `index` by `amount` (host units) from its value
    /// without changing it; 0 ends the modulation.
    ParamMod {
        frame: u32,
        index: usize,
        amount: f64,
    },
}

impl Input {
    pub fn frame(&self) -> u32 {
        match *self {
            Input::NoteOn { frame, .. }
            | Input::NoteOff { frame, .. }
            | Input::Choke { frame, .. }
            | Input::Midi { frame, .. }
            | Input::Param { frame, .. }
            | Input::ParamMod { frame, .. } => frame,
        }
    }
}

/// What the host says of the song at a block's start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transport {
    pub tempo: Option<f64>,
    /// Quarter notes from the song's start.
    pub beat: Option<f64>,
    pub playing: bool,
}

/// A velocity from the host's `0..=1`.
pub fn velocity(unit: f64) -> Velocity {
    let unit = if unit.is_nan() {
        0.0
    } else {
        unit.clamp(0.0, 1.0)
    };
    Velocity::new((unit * f64::from(u16::MAX)).round() as u16)
}

/// The sustain pedal: while it's down, keys let go keep sounding until
/// it comes up.
struct Sustain {
    down: bool,
    held: [bool; 128],
}

impl Sustain {
    const UP: Sustain = Sustain {
        down: false,
        held: [false; 128],
    };
}

pub struct Bridge {
    instrument: Box<dyn Instrument>,
    values: Arc<Values>,
    /// Each parameter's modulation, in the host's units.
    modulation: Box<[f64]>,
    /// Parameters to hand the instrument at the next block's start, each
    /// once (`dirty` says which are listed).
    changed: Vec<usize>,
    dirty: Box<[bool]>,
    events: Vec<NoteEvent>,
    sustain: Sustain,
    sample_rate: f64,
    tempo: f64,
}

impl Bridge {
    /// Wraps `instrument`, whose parameters `values` holds. Call
    /// [`Self::prepare`] before processing.
    pub fn new(instrument: Box<dyn Instrument>, values: Arc<Values>) -> Bridge {
        let count = values.len();
        Bridge {
            instrument,
            values,
            modulation: vec![0.0; count].into(),
            changed: Vec::with_capacity(count),
            dirty: vec![false; count].into(),
            events: Vec::with_capacity(MAX_EVENTS),
            sustain: Sustain::UP,
            sample_rate: 48_000.0,
            tempo: DEFAULT_TEMPO,
        }
    }

    /// Readies the instrument for `sample_rate` at its current values, as
    /// the engine readies a new one: settled there rather than gliding,
    /// silent, unmodulated, the pedal up. Not on the audio thread.
    pub fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.instrument.prepare(sample_rate as f32, MAX_BLOCK);
        self.instrument.set_tempo(self.tempo);
        self.values.take_pending(|_| {});
        self.changed.clear();
        self.dirty.fill(false);
        self.modulation.fill(0.0);
        for index in 0..self.values.len() {
            let value = self.values.plain(index).unwrap_or_default();
            self.instrument.set_param(index, value);
        }
        self.instrument.reset();
        self.sustain = Sustain::UP;
    }

    /// Silences every voice, and lifts the pedal. Real-time safe.
    pub fn reset(&mut self) {
        self.instrument.reset();
        self.sustain = Sustain::UP;
    }

    /// Renders one host block into `left` and `right` (the same length),
    /// with its `events` in order of frame. Real-time safe.
    pub fn process(
        &mut self,
        events: impl IntoIterator<Item = Input>,
        transport: Option<Transport>,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let values = Arc::clone(&self.values);
        values.take_pending(|index| self.mark(index));
        if let Some(tempo) = transport.and_then(|t| t.tempo)
            && tempo > 0.0
            && tempo != self.tempo
        {
            self.tempo = tempo;
            self.instrument.set_tempo(tempo);
        }
        let len = left.len().min(right.len());
        let mut events = events.into_iter().peekable();
        let mut start = 0;
        while start < len {
            let end = (start + MAX_BLOCK).min(len);
            self.events.clear();
            // Events at or past the block's end (there shouldn't be any)
            // go in its last frame.
            while let Some(input) = events.next_if(|e| (e.frame() as usize) < end || end == len) {
                let frame = (input.frame() as usize).clamp(start, end - 1) - start;
                self.take(input, frame as u32);
            }
            self.apply_changes();
            if let Some(Transport {
                beat: Some(beat),
                playing,
                ..
            }) = transport
            {
                let seconds = start as f64 / self.sample_rate;
                self.instrument
                    .set_position(beat + seconds * self.tempo / 60.0, playing);
            }
            self.instrument
                .process(&self.events, &mut left[start..end], &mut right[start..end]);
            start = end;
        }
        // A block without frames still takes its parameter changes.
        self.events.clear();
        for input in events {
            self.take(input, 0);
        }
        self.apply_changes();
    }

    /// Applies parameter changes and modulation outside a block (CLAP's
    /// `params.flush`). Real-time safe.
    pub fn flush(&mut self, events: impl IntoIterator<Item = Input>) {
        let values = Arc::clone(&self.values);
        values.take_pending(|index| self.mark(index));
        self.events.clear();
        for input in events {
            if matches!(input, Input::Param { .. } | Input::ParamMod { .. }) {
                self.take(input, 0);
            }
        }
        self.apply_changes();
    }

    /// Notes go into this block's events; parameter changes are marked.
    fn take(&mut self, input: Input, frame: u32) {
        match input {
            Input::NoteOn { key, velocity, .. } => {
                if let Some(pitch) = Pitch::new(key) {
                    self.note_on(frame, pitch, self::velocity(velocity));
                }
            }
            Input::NoteOff { key, velocity, .. } => match key.and_then(Pitch::new) {
                Some(pitch) => self.note_off(frame, pitch, self::velocity(velocity)),
                None if !self.sustain.down => self.push(frame, NoteEventKind::AllNotesOff),
                None => self.sustain.held = [true; 128],
            },
            Input::Choke { key, .. } => match key.and_then(Pitch::new) {
                Some(pitch) => {
                    self.sustain.held[usize::from(pitch.get())] = false;
                    let velocity = Velocity::CENTER;
                    self.push(frame, NoteEventKind::Off { pitch, velocity });
                }
                None => self.all_notes_off(frame),
            },
            Input::Midi { data, .. } => self.midi(frame, data),
            Input::Param { index, value, .. } => {
                if self.values.set(index, value) {
                    self.mark(index);
                }
            }
            Input::ParamMod { index, amount, .. } => {
                if let Some(modulation) = self.modulation.get_mut(index) {
                    *modulation = if amount.is_nan() { 0.0 } else { amount };
                    self.mark(index);
                }
            }
        }
    }

    fn midi(&mut self, frame: u32, data: [u8; 3]) {
        match MidiMessage::parse(&data) {
            Some(MidiMessage::Note { kind, .. }) => match kind {
                NoteEventKind::On { pitch, velocity } => self.note_on(frame, pitch, velocity),
                NoteEventKind::Off { pitch, velocity } => self.note_off(frame, pitch, velocity),
                kind => self.push(frame, kind),
            },
            Some(MidiMessage::ControlChange {
                controller, value, ..
            }) => match controller {
                SUSTAIN_PEDAL => self.pedal(frame, value >= 64),
                ALL_SOUND_OFF | ALL_NOTES_OFF => self.all_notes_off(frame),
                controller => self.push(frame, NoteEventKind::Control { controller, value }),
            },
            None => {}
        }
    }

    fn note_on(&mut self, frame: u32, pitch: Pitch, velocity: Velocity) {
        self.sustain.held[usize::from(pitch.get())] = false;
        self.push(frame, NoteEventKind::On { pitch, velocity });
    }

    fn note_off(&mut self, frame: u32, pitch: Pitch, velocity: Velocity) {
        if self.sustain.down {
            self.sustain.held[usize::from(pitch.get())] = true;
        } else {
            self.push(frame, NoteEventKind::Off { pitch, velocity });
        }
    }

    fn pedal(&mut self, frame: u32, down: bool) {
        self.sustain.down = down;
        if down {
            return;
        }
        for key in 0..128u8 {
            if std::mem::take(&mut self.sustain.held[usize::from(key)])
                && let Some(pitch) = Pitch::new(key)
            {
                let velocity = Velocity::CENTER;
                self.push(frame, NoteEventKind::Off { pitch, velocity });
            }
        }
    }

    fn all_notes_off(&mut self, frame: u32) {
        self.sustain.held = [false; 128];
        self.push(frame, NoteEventKind::AllNotesOff);
    }

    fn push(&mut self, frame: u32, kind: NoteEventKind) {
        if self.events.len() < MAX_EVENTS {
            self.events.push(NoteEvent { frame, kind });
        }
    }

    fn mark(&mut self, index: usize) {
        if let Some(dirty) = self.dirty.get_mut(index)
            && !*dirty
        {
            *dirty = true;
            self.changed.push(index);
        }
    }

    /// Hands the marked parameters to the instrument: a modulated one
    /// through `modulate`, which reaches it within the block, as the
    /// engine's modulation does; others through `set_param`.
    fn apply_changes(&mut self) {
        let params = self.values.params();
        for index in self.changed.drain(..) {
            self.dirty[index] = false;
            let Some(value) = self.values.plain(index) else {
                continue;
            };
            let modulation = self.modulation[index];
            if modulation != 0.0 {
                let param = &params[index];
                let value = to_plain(param, to_host(param, value) + modulation);
                self.instrument.modulate(index, value);
            } else {
                self.instrument.set_param(index, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use instruments::{ParamDescriptor, choice, float};

    use super::*;

    static PARAMS: &[ParamDescriptor] = &[
        float("level", "Level", 0.0, 10.0, 5.0, "", false),
        choice("shape", "Shape", &["Sine", "Saw"], 0),
    ];

    /// What the instrument was told, in order.
    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Process(usize, Vec<NoteEvent>),
        Set(usize, f32),
        Modulate(usize, f32),
        Tempo(f64),
        Position(f64, bool),
        Reset,
    }

    struct Recorder(Arc<Mutex<Vec<Call>>>);

    impl Instrument for Recorder {
        fn prepare(&mut self, _: f32, _: usize) {}
        fn reset(&mut self) {
            self.0.lock().unwrap().push(Call::Reset);
        }
        fn process(&mut self, events: &[NoteEvent], left: &mut [f32], _: &mut [f32]) {
            let call = Call::Process(left.len(), events.to_vec());
            self.0.lock().unwrap().push(call);
        }
        fn params(&self) -> &'static [ParamDescriptor] {
            PARAMS
        }
        fn set_param(&mut self, index: usize, value: f32) {
            self.0.lock().unwrap().push(Call::Set(index, value));
        }
        fn modulate(&mut self, index: usize, value: f32) {
            self.0.lock().unwrap().push(Call::Modulate(index, value));
        }
        fn set_tempo(&mut self, bpm: f64) {
            self.0.lock().unwrap().push(Call::Tempo(bpm));
        }
        fn set_position(&mut self, beat: f64, playing: bool) {
            self.0.lock().unwrap().push(Call::Position(beat, playing));
        }
    }

    /// A prepared bridge around a [`Recorder`], with what it recorded
    /// while preparing cleared.
    fn rig() -> (Bridge, Arc<Values>, Arc<Mutex<Vec<Call>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let values = Arc::new(Values::new(PARAMS));
        let mut bridge = Bridge::new(Box::new(Recorder(calls.clone())), values.clone());
        bridge.prepare(48_000.0);
        calls.lock().unwrap().clear();
        (bridge, values, calls)
    }

    fn run(bridge: &mut Bridge, frames: usize, events: &[Input]) {
        let (mut left, mut right) = (vec![0.0; frames], vec![0.0; frames]);
        bridge.process(events.iter().copied(), None, &mut left, &mut right);
    }

    fn take(calls: &Mutex<Vec<Call>>) -> Vec<Call> {
        std::mem::take(&mut *calls.lock().unwrap())
    }

    fn on(frame: u32, key: u8) -> Input {
        Input::NoteOn {
            frame,
            key,
            velocity: 1.0,
        }
    }

    fn off(frame: u32, key: u8) -> Input {
        Input::NoteOff {
            frame,
            key: Some(key),
            velocity: 0.5,
        }
    }

    fn event(frame: u32, kind: NoteEventKind) -> NoteEvent {
        NoteEvent { frame, kind }
    }

    fn note_on(frame: u32, key: u8) -> NoteEvent {
        let (pitch, velocity) = (Pitch::new(key).unwrap(), Velocity::MAX);
        event(frame, NoteEventKind::On { pitch, velocity })
    }

    fn note_off(frame: u32, key: u8, velocity: Velocity) -> NoteEvent {
        let pitch = Pitch::new(key).unwrap();
        event(frame, NoteEventKind::Off { pitch, velocity })
    }

    #[test]
    fn preparing_settles_the_instrument_at_its_values() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let values = Arc::new(Values::new(PARAMS));
        values.send(0, 0.2);
        let mut bridge = Bridge::new(Box::new(Recorder(calls.clone())), values);
        bridge.prepare(44_100.0);
        assert_eq!(
            take(&calls),
            [
                Call::Tempo(120.0),
                Call::Set(0, 2.0),
                Call::Set(1, 0.0),
                Call::Reset
            ]
        );
        // What preparing applied isn't applied again.
        run(&mut bridge, 8, &[]);
        assert_eq!(take(&calls), [Call::Process(8, vec![])]);
    }

    #[test]
    fn host_blocks_are_cut_into_engine_blocks_with_their_notes() {
        let (mut bridge, _, calls) = rig();
        run(&mut bridge, 150, &[on(10, 60), off(70, 60), on(149, 62)]);
        assert_eq!(
            take(&calls),
            [
                Call::Process(64, vec![note_on(10, 60)]),
                Call::Process(64, vec![note_off(6, 60, velocity(0.5))]),
                Call::Process(22, vec![note_on(21, 62)]),
            ]
        );
    }

    #[test]
    fn parameter_changes_apply_at_the_start_of_their_engine_block() {
        let (mut bridge, values, calls) = rig();
        let param = |frame, value| Input::Param {
            frame,
            index: 0,
            value,
        };
        // Two changes in one block: the instrument gets only the last.
        run(
            &mut bridge,
            128,
            &[param(5, 0.1), param(30, 0.3), param(64, 1.0)],
        );
        assert_eq!(
            take(&calls),
            [
                Call::Set(0, 3.0),
                Call::Process(64, vec![]),
                Call::Set(0, 10.0),
                Call::Process(64, vec![]),
            ]
        );
        assert_eq!(values.get(0), Some(1.0));
    }

    #[test]
    fn modulation_moves_the_value_without_changing_it() {
        let (mut bridge, values, calls) = rig();
        let modulate = |amount| Input::ParamMod {
            frame: 0,
            index: 0,
            amount,
        };
        run(&mut bridge, 64, &[modulate(0.25)]);
        assert_eq!(
            take(&calls),
            [Call::Modulate(0, 7.5), Call::Process(64, vec![])]
        );
        assert_eq!(values.get(0), Some(0.5));
        // A change under modulation is modulated too; ending it sets the
        // value plainly.
        let set = Input::Param {
            frame: 0,
            index: 0,
            value: 0.1,
        };
        run(&mut bridge, 64, &[set]);
        run(&mut bridge, 64, &[modulate(0.0)]);
        assert_eq!(
            take(&calls),
            [
                Call::Modulate(0, 3.5),
                Call::Process(64, vec![]),
                Call::Set(0, 1.0),
                Call::Process(64, vec![]),
            ]
        );
    }

    #[test]
    fn values_from_the_main_thread_reach_the_next_block() {
        let (mut bridge, values, calls) = rig();
        values.send(1, 1.0);
        run(&mut bridge, 32, &[]);
        assert_eq!(take(&calls), [Call::Set(1, 1.0), Call::Process(32, vec![])]);
    }

    #[test]
    fn the_sustain_pedal_holds_released_keys_until_it_comes_up() {
        let (mut bridge, _, calls) = rig();
        let pedal = |frame, value| Input::Midi {
            frame,
            data: [0xB0, SUSTAIN_PEDAL, value],
        };
        run(
            &mut bridge,
            64,
            &[
                pedal(0, 127),
                on(1, 60),
                on(2, 64),
                off(3, 60),
                off(4, 64),
                // Played again under the pedal: no longer held by it.
                on(5, 64),
                pedal(20, 0),
            ],
        );
        assert_eq!(
            take(&calls),
            [Call::Process(
                64,
                vec![
                    note_on(1, 60),
                    note_on(2, 64),
                    note_on(5, 64),
                    note_off(20, 60, Velocity::CENTER),
                ]
            )]
        );
    }

    #[test]
    fn midi_becomes_canopy_events() {
        let (mut bridge, _, calls) = rig();
        let midi = |data| Input::Midi { frame: 0, data };
        run(
            &mut bridge,
            16,
            &[
                midi([0x90, 60, 100]),
                midi([0x90, 60, 0]),
                midi([0xE0, 0, 0x40]),
                midi([0xB0, 1, 99]),
                midi([0xB0, ALL_NOTES_OFF, 0]),
                midi([0xF8, 0, 0]),
            ],
        );
        let pitch = Pitch::new(60).unwrap();
        assert_eq!(
            take(&calls),
            [Call::Process(
                16,
                vec![
                    event(
                        0,
                        NoteEventKind::On {
                            pitch,
                            velocity: Velocity::from_midi1(100)
                        }
                    ),
                    note_off(0, 60, Velocity::CENTER),
                    event(0, NoteEventKind::PitchBend(0)),
                    event(
                        0,
                        NoteEventKind::Control {
                            controller: 1,
                            value: 99
                        }
                    ),
                    event(0, NoteEventKind::AllNotesOff),
                ]
            )]
        );
    }

    #[test]
    fn the_transport_sets_tempo_and_each_blocks_position() {
        let (mut bridge, _, calls) = rig();
        let transport = Transport {
            tempo: Some(90.0),
            beat: Some(4.0),
            playing: true,
        };
        let (mut left, mut right) = (vec![0.0; 128], vec![0.0; 128]);
        bridge.process([], Some(transport), &mut left, &mut right);
        let beat = 4.0 + 64.0 / 48_000.0 * 90.0 / 60.0;
        assert_eq!(
            take(&calls),
            [
                Call::Tempo(90.0),
                Call::Position(4.0, true),
                Call::Process(64, vec![]),
                Call::Position(beat, true),
                Call::Process(64, vec![]),
            ]
        );
    }
}
