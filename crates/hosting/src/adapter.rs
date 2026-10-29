//! Plugins as canopy's devices: [`PluginEffect`] is an [`Effect`] and
//! [`PluginInstrument`] an [`Instrument`], each around an active plugin's
//! [`Processor`]. They live on the audio thread (or an export's), where
//! everything they do is real-time safe: parameter changes, notes and
//! MIDI go into a buffer allocated up front and reach the plugin with the
//! next block.
//!
//! Parameters arrive by descriptor index in canopy's units ([`ParamMap`]
//! converts them). A knob, automation or a controller sets a value
//! ([`Event::Param`]); a modulator offsets it from there
//! ([`Event::ParamMod`]), which CLAP plugins hear without their own knob
//! moving and other plugins as a plain value change.
//!
//! Debug builds abort when the audio thread allocates, to catch canopy's
//! own mistakes. What a plugin does inside its own calls is its business
//! (plenty of plugins allocate while processing), so those calls are let
//! through ([`foreign`]); everything around them is still checked.

use std::sync::Arc;

use effects::{Context, Effect, Pace};
use instruments::{Instrument, ParamDescriptor};
use midi::{NoteEvent, NoteEventKind};
use plugins::{Event, Processor, Transport};

use crate::describe::ParamMap;

/// Room for this many notes and MIDI messages per block, besides one
/// change per parameter; more than a block ever holds in practice. Any
/// beyond are dropped rather than allocated for.
const NOTE_ROOM: usize = 512;

/// Runs a call into the plugin's own code, where allocating is the
/// plugin's choice rather than canopy's mistake.
fn foreign<T>(call: impl FnOnce() -> T) -> T {
    assert_no_alloc::permit_alloc(call)
}

/// What both adapters share: the plugin's processor and the next block's
/// events.
struct Control {
    processor: Processor,
    map: Arc<[ParamMap]>,
    params: &'static [ParamDescriptor],
    /// Each parameter's value as last set, in canopy's units, so that
    /// only changes are sent and modulation knows what it offsets.
    base: Box<[f32]>,
    /// Each parameter's modulation as last sent, in the plugin's units.
    offset: Box<[f64]>,
    events: Vec<Event>,
    tempo: f64,
    beat: f64,
    playing: bool,
}

impl Control {
    fn new(
        processor: Processor,
        map: Arc<[ParamMap]>,
        params: &'static [ParamDescriptor],
        values: &[f32],
    ) -> Control {
        let mut base = vec![f32::NAN; map.len()].into_boxed_slice();
        for (slot, value) in base.iter_mut().zip(values) {
            *slot = *value;
        }
        Control {
            processor,
            offset: vec![0.0; map.len()].into_boxed_slice(),
            events: Vec::with_capacity(map.len() * 2 + NOTE_ROOM),
            map,
            params,
            base,
            tempo: 120.0,
            beat: 0.0,
            playing: false,
        }
    }

    fn receive(&mut self, index: usize, value: f32, pace: Pace) {
        let (Some(map), Some(descriptor)) = (self.map.get(index), self.params.get(index)) else {
            return;
        };
        if value.is_nan() {
            return;
        }
        let value = descriptor.clamp(value);
        match pace {
            Pace::Knob => {
                if self.base[index] == value {
                    return;
                }
                self.base[index] = value;
                self.push(Event::Param {
                    frame: 0,
                    id: map.id,
                    value: map.to_plugin(value),
                });
            }
            Pace::Modulation => {
                let base = self.base[index];
                let from = if base.is_nan() {
                    descriptor.default_value()
                } else {
                    base
                };
                let amount = map.to_plugin(value) - map.to_plugin(from);
                if self.offset[index] == amount {
                    return;
                }
                self.offset[index] = amount;
                self.push(Event::ParamMod {
                    frame: 0,
                    id: map.id,
                    amount,
                });
            }
        }
    }

    fn push(&mut self, event: Event) {
        if self.events.len() < self.events.capacity() {
            self.events.push(event);
        }
    }

    fn transport(&self) -> Transport {
        let rate = self.processor.config().sample_rate;
        let tempo = if self.tempo > 0.0 { self.tempo } else { 120.0 };
        // The engine's blocks carry no time signature: bars count as 4/4.
        let bar = (self.beat / 4.0).floor();
        Transport {
            playing: self.playing,
            recording: false,
            tempo,
            time_signature: (4, 4),
            sample_position: (self.beat * 60.0 / tempo * rate) as i64,
            beat: self.beat,
            bar_start: bar * 4.0,
            bar_number: bar as i32,
            loop_range: None,
        }
    }
}

/// A plugin effect in a chain.
pub struct PluginEffect {
    control: Control,
}

impl PluginEffect {
    /// Wraps an active plugin's processor. `values` are its parameters'
    /// current values, by descriptor index in canopy's units: what the
    /// plugin already has, so they aren't sent again.
    pub fn new(
        processor: Processor,
        map: Arc<[ParamMap]>,
        params: &'static [ParamDescriptor],
        values: &[f32],
    ) -> PluginEffect {
        PluginEffect {
            control: Control::new(processor, map, params, values),
        }
    }
}

impl Effect for PluginEffect {
    /// Nothing to do: the plugin was activated for the engine's rate and
    /// block size before it got here.
    fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}

    fn reset(&mut self) {
        let processor = &mut self.control.processor;
        foreign(|| processor.reset());
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: &Context) {
        let control = &mut self.control;
        control.tempo = context.bpm;
        control.beat = context.beat;
        control.playing = context.playing;
        let transport = control.transport();
        let (processor, events) = (&mut control.processor, &control.events);
        foreign(|| processor.process_stereo(left, right, context.sidechain, events, &transport));
        control.events.clear();
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        self.control.params
    }

    fn receive(&mut self, index: usize, value: f32, pace: Pace) {
        self.control.receive(index, value, pace);
    }

    fn retire(&mut self) {
        let processor = &mut self.control.processor;
        foreign(|| processor.stop_processing());
    }
}

/// A plugin instrument playing a track's notes.
pub struct PluginInstrument {
    control: Control,
}

impl PluginInstrument {
    /// Like [`PluginEffect::new`].
    pub fn new(
        processor: Processor,
        map: Arc<[ParamMap]>,
        params: &'static [ParamDescriptor],
        values: &[f32],
    ) -> PluginInstrument {
        PluginInstrument {
            control: Control::new(processor, map, params, values),
        }
    }
}

impl Instrument for PluginInstrument {
    fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}

    fn reset(&mut self) {
        let processor = &mut self.control.processor;
        foreign(|| processor.reset());
    }

    fn process(&mut self, events: &[NoteEvent], left: &mut [f32], right: &mut [f32]) {
        let control = &mut self.control;
        // Parameter changes are all at frame 0, so the notes after them
        // keep the events sorted.
        for event in events {
            if let Some(event) = note_event(event) {
                control.push(event);
            }
        }
        let transport = control.transport();
        left.fill(0.0);
        right.fill(0.0);
        let (processor, events) = (&mut control.processor, &control.events);
        foreign(|| processor.process_stereo(left, right, None, events, &transport));
        control.events.clear();
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        self.control.params
    }

    fn set_param(&mut self, index: usize, value: f32) {
        self.control.receive(index, value, Pace::Knob);
    }

    fn modulate(&mut self, index: usize, value: f32) {
        self.control.receive(index, value, Pace::Modulation);
    }

    fn set_tempo(&mut self, bpm: f64) {
        self.control.tempo = bpm;
    }

    fn set_position(&mut self, beat: f64, playing: bool) {
        self.control.beat = beat;
        self.control.playing = playing;
    }

    fn retire(&mut self) {
        let processor = &mut self.control.processor;
        foreign(|| processor.stop_processing());
    }
}

/// A canopy note event as the plugin hears it, on MIDI channel 1.
fn note_event(event: &NoteEvent) -> Option<Event> {
    let frame = event.frame;
    Some(match event.kind {
        NoteEventKind::On { pitch, velocity } => Event::NoteOn {
            frame,
            channel: 0,
            key: pitch.get(),
            velocity: velocity.as_f32(),
        },
        NoteEventKind::Off { pitch, velocity } => Event::NoteOff {
            frame,
            channel: 0,
            key: pitch.get(),
            velocity: velocity.as_f32(),
        },
        NoteEventKind::AllNotesOff => Event::AllNotesOff { frame },
        NoteEventKind::NoteBend { pitch, cents } => Event::NoteTuning {
            frame,
            channel: 0,
            key: pitch.get(),
            semitones: f64::from(cents) / 100.0,
        },
        NoteEventKind::PitchBend(value) => {
            let wheel = (i32::from(value).clamp(-8192, 8191) + 8192) as u16;
            Event::Midi {
                frame,
                data: [0xE0, (wheel & 0x7F) as u8, (wheel >> 7) as u8],
            }
        }
        NoteEventKind::Control { controller, value } => Event::Midi {
            frame,
            data: [0xB0, controller.min(127), value.min(127)],
        },
    })
}

/// What a plugin effect's descriptor creates if anything asks it directly
/// (the engine asks the host instead): sound passes through.
pub(crate) struct Passthrough;

impl Effect for Passthrough {
    fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, _left: &mut [f32], _right: &mut [f32], _context: &Context) {}
    fn params(&self) -> &'static [ParamDescriptor] {
        &[]
    }
    fn receive(&mut self, _index: usize, _value: f32, _pace: Pace) {}
}

/// Like [`Passthrough`], for a plugin instrument's descriptor: silence.
pub(crate) struct Silence;

impl Instrument for Silence {
    fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, _events: &[NoteEvent], left: &mut [f32], right: &mut [f32]) {
        left.fill(0.0);
        right.fill(0.0);
    }
    fn params(&self) -> &'static [ParamDescriptor] {
        &[]
    }
    fn set_param(&mut self, _index: usize, _value: f32) {}
}

#[cfg(test)]
mod tests {
    use midi::{Pitch, Velocity};

    use super::*;

    #[test]
    fn notes_and_wheels_become_midi_on_channel_one() {
        let on = NoteEvent {
            frame: 3,
            kind: NoteEventKind::On {
                pitch: Pitch::MIDDLE_C,
                velocity: Velocity::MAX,
            },
        };
        assert_eq!(
            note_event(&on),
            Some(Event::NoteOn {
                frame: 3,
                channel: 0,
                key: 60,
                velocity: 1.0
            })
        );
        let centered = NoteEvent {
            frame: 0,
            kind: NoteEventKind::PitchBend(0),
        };
        assert_eq!(
            note_event(&centered),
            Some(Event::Midi {
                frame: 0,
                data: [0xE0, 0, 64]
            })
        );
        let top = NoteEvent {
            frame: 0,
            kind: NoteEventKind::PitchBend(8191),
        };
        assert_eq!(
            note_event(&top),
            Some(Event::Midi {
                frame: 0,
                data: [0xE0, 127, 127]
            })
        );
    }

    #[test]
    fn a_notes_bend_becomes_its_tuning_in_semitones() {
        let bend = NoteEvent {
            frame: 5,
            kind: NoteEventKind::NoteBend {
                pitch: Pitch::MIDDLE_C,
                cents: -250,
            },
        };
        assert_eq!(
            note_event(&bend),
            Some(Event::NoteTuning {
                frame: 5,
                channel: 0,
                key: 60,
                semitones: -2.5
            })
        );
    }
}
