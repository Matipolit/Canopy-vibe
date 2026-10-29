use dsp::{Adsr, Gain, Oscillator, Processor, Waveform};
use midi::{NoteEvent, NoteEventKind, Pitch};

use crate::envelope::{self, EnvelopeParams};
use crate::{BuiltInInstrumentDescriptor, Instrument, ParamDescriptor, ParameterKind};

pub const DESCRIPTOR: BuiltInInstrumentDescriptor = BuiltInInstrumentDescriptor {
    id: "canopy.simple_osc",
    name: "Simple Oscillator",
    description: "Monophonic band-limited oscillator",
    version: "1.0",
    author: "canopy",
    params: PARAMS,
    envelopes: &[ENVELOPE],
    create: || Box::new(SimpleOsc::new()),
    data: crate::DataKind::None,
    groups: crate::no_groups,
};

const WAVEFORM: usize = 0;
const GAIN_DB: usize = 1;
const ENVELOPE: EnvelopeParams = EnvelopeParams::starting_at("Envelope", 2);
const GLIDE_MS: usize = 9;

const WAVEFORMS: [Waveform; 3] = [Waveform::Sine, Waveform::Saw, Waveform::Square];

const PARAMS: &[ParamDescriptor] = &[
    ParamDescriptor {
        id: "waveform",
        name: "Waveform",
        kind: ParameterKind::Enum {
            labels: &["Sine", "Saw", "Square"],
            default: 0,
        },
    },
    ParamDescriptor {
        id: "gain",
        name: "Gain",
        kind: ParameterKind::Float {
            min: -60.0,
            max: 6.0,
            default: 0.0,
            unit: "dB",
            logarithmic: false,
        },
    },
    envelope::attack("attack", 5.0),
    envelope::decay("decay", 200.0),
    envelope::sustain("sustain", 0.7),
    envelope::release("release", 300.0),
    envelope::curve("attack_curve", "Attack Curve", -0.5),
    envelope::curve("decay_curve", "Decay Curve", -0.7),
    envelope::curve("release_curve", "Release Curve", -0.7),
    ParamDescriptor {
        id: "glide",
        name: "Glide",
        kind: ParameterKind::Float {
            min: 0.0,
            max: 1000.0,
            default: 0.0,
            unit: "ms",
            logarithmic: false,
        },
    },
];

/// One oscillator through an envelope. A new note takes over from the one
/// playing (last-note priority); with Glide above zero it slides there from
/// the previous pitch while that one is still sounding.
pub struct SimpleOsc {
    osc: Oscillator,
    env: Adsr,
    gain: Gain,
    current_note: Option<Pitch>,
    velocity: f32,
    glide_ms: f32,
    /// The pitch wheel's frequency ratio.
    wheel: f32,
    /// The playing note's own bend, as a frequency ratio.
    bend: f32,
    /// A note started since the last render: a bend arriving now is where
    /// it starts, not a move to glide through.
    fresh: bool,
}

impl SimpleOsc {
    pub fn new() -> Self {
        let desc = ENVELOPE.desc(|i| PARAMS[i].default_value());
        Self {
            osc: Oscillator::new(WAVEFORMS[0], 440.0),
            env: Adsr::new(desc, 44_100.0),
            gain: Gain::new(PARAMS[GAIN_DB].default_value()),
            current_note: None,
            velocity: 0.0,
            glide_ms: 0.0,
            wheel: 1.0,
            bend: 1.0,
            fresh: false,
        }
    }

    fn handle(&mut self, kind: NoteEventKind) {
        match kind {
            NoteEventKind::NoteBend { pitch, cents } => {
                if self.current_note == Some(pitch) {
                    self.bend = NoteEventKind::note_bend_ratio(cents);
                    let ratio = self.wheel * self.bend;
                    if self.fresh {
                        self.osc.set_pitch_ratio_immediate(ratio);
                    } else {
                        self.osc.modulate_pitch_ratio(ratio);
                    }
                }
            }
            NoteEventKind::On { pitch, velocity } => {
                // A new note starts unbent. One gliding over from another
                // leaves the last one's bend within a block; otherwise the
                // pitch jumps to the note, and the bend goes with it.
                if self.bend != 1.0 {
                    self.bend = 1.0;
                    if self.env.is_active() && self.glide_ms > 0.0 && !self.fresh {
                        self.osc.modulate_pitch_ratio(self.wheel);
                    } else {
                        self.osc.set_pitch_ratio_immediate(self.wheel);
                    }
                }
                self.fresh = true;
                if self.env.is_active() && self.glide_ms > 0.0 {
                    self.osc.set_frequency(pitch.frequency());
                } else if self.env.is_active() {
                    // Legato without glide: jump, keeping the phase so the
                    // waveform doesn't click.
                    self.osc.set_frequency_immediate(pitch.frequency());
                } else {
                    self.osc.reset();
                    self.osc.set_frequency_immediate(pitch.frequency());
                }
                self.current_note = Some(pitch);
                self.velocity = velocity.as_f32();
                self.env.start_attack();
            }
            NoteEventKind::Off { pitch, .. } => {
                if self.current_note == Some(pitch) {
                    self.current_note = None;
                    self.env.start_release();
                }
            }
            NoteEventKind::AllNotesOff => {
                self.current_note = None;
                self.env.start_release();
            }
            NoteEventKind::PitchBend(value) => {
                self.wheel = NoteEventKind::bend_ratio(value);
                self.osc.set_pitch_ratio(self.wheel * self.bend);
            }
            NoteEventKind::Control { .. } => {}
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !left.is_empty() {
            self.fresh = false;
        }
        if !self.env.is_active() {
            left.fill(0.0);
            right.fill(0.0);
            return;
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let sample = self.osc.next_sample() * self.env.next_sample() * self.velocity;
            *l = sample;
            *r = sample;
        }
    }
}

impl Default for SimpleOsc {
    fn default() -> Self {
        Self::new()
    }
}

impl Instrument for SimpleOsc {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.osc.prepare(sample_rate);
        self.osc.set_glide(self.glide_ms.max(1.0));
        self.env = Adsr::new(*self.env.desc(), sample_rate);
        self.gain.prepare(sample_rate);
        self.current_note = None;
    }

    fn reset(&mut self) {
        self.env.reset();
        self.osc.reset();
        self.osc.set_pitch_ratio_immediate(1.0);
        self.wheel = 1.0;
        self.bend = 1.0;
        self.fresh = false;
        self.gain.reset();
        self.current_note = None;
    }

    fn process(&mut self, events: &[NoteEvent], left: &mut [f32], right: &mut [f32]) {
        for (span, event) in crate::segments(events, left.len()) {
            self.render(&mut left[span.clone()], &mut right[span]);
            if let Some(kind) = event {
                self.handle(kind);
            }
        }
        self.gain.process(left, right);
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(value) = crate::checked_value(PARAMS, index, value) else {
            return;
        };
        match index {
            WAVEFORM => self.osc.set_waveform(WAVEFORMS[value as usize]),
            GAIN_DB => self.gain.set_db(value),
            GLIDE_MS => {
                self.glide_ms = value;
                // Never fully instant: a few ms keeps a slide from clicking.
                self.osc.set_glide(value.max(1.0));
            }
            _ => {
                ENVELOPE.apply(index, value, self.env.desc_mut());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use midi::Velocity;

    use super::*;

    fn note_on(frame: u32, pitch: Pitch) -> NoteEvent {
        NoteEvent {
            frame,
            kind: NoteEventKind::On {
                pitch,
                velocity: Velocity::MAX,
            },
        }
    }

    fn peak(buffer: &[f32]) -> f32 {
        buffer.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn plays_from_the_event_frame_and_releases_on_all_notes_off() {
        let mut osc = SimpleOsc::new();
        osc.prepare(48_000.0, 512);
        osc.set_param(WAVEFORM, 1.0);
        let (mut left, mut right) = ([0.0; 512], [0.0; 512]);
        osc.process(&[note_on(100, Pitch::A4)], &mut left, &mut right);
        assert_eq!(peak(&left[..100]), 0.0, "silent before the note");
        assert!(peak(&left[100..]) > 0.1);
        assert_eq!(left, right);

        let all_off = NoteEvent {
            frame: 0,
            kind: NoteEventKind::AllNotesOff,
        };
        osc.process(&[all_off], &mut left, &mut right);
        // The default release is 300 ms; a second later it's silent.
        for _ in 0..100 {
            osc.process(&[], &mut left, &mut right);
        }
        assert_eq!(peak(&left), 0.0);
    }

    /// Frequency of the second half of a buffer, by counting upward zero
    /// crossings of a sine.
    fn frequency_after_second_note(glide_ms: f32) -> f32 {
        let mut osc = SimpleOsc::new();
        osc.prepare(48_000.0, 4800);
        osc.set_param(GLIDE_MS, glide_ms);
        let (mut left, mut right) = (vec![0.0; 4800], vec![0.0; 4800]);
        osc.process(&[note_on(0, Pitch::A4)], &mut left, &mut right);
        // A second note while the first still sounds: legato.
        let a5 = Pitch::new(81).unwrap();
        osc.process(&[note_on(0, a5)], &mut left, &mut right);
        // The first 2 ms (where a short safety ramp may run) are skipped.
        let settled = &left[96..1056];
        let crossings = settled
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        crossings as f32 / (settled.len() as f32 / 48_000.0)
    }

    #[test]
    fn legato_notes_jump_unless_glide_is_set() {
        let jumped = frequency_after_second_note(0.0);
        assert!(
            (jumped - 880.0).abs() < 60.0,
            "no glide: already at A5 ({jumped} Hz)"
        );
        let gliding = frequency_after_second_note(300.0);
        assert!(
            gliding < 700.0,
            "with glide it's still on its way ({gliding} Hz)"
        );
    }

    #[test]
    fn note_off_for_another_pitch_is_ignored() {
        let mut osc = SimpleOsc::new();
        osc.prepare(48_000.0, 512);
        let (mut left, mut right) = ([0.0; 512], [0.0; 512]);
        let off_other = NoteEvent {
            frame: 10,
            kind: NoteEventKind::Off {
                pitch: Pitch::MIDDLE_C,
                velocity: Velocity::CENTER,
            },
        };
        osc.process(&[note_on(0, Pitch::A4), off_other], &mut left, &mut right);
        for _ in 0..100 {
            osc.process(&[], &mut left, &mut right);
        }
        assert!(peak(&left) > 0.1, "A4 is still held");
    }
}
