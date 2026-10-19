//! Amp: a guitar amp and speaker cabinet. Clean, crunch and lead voicings;
//! gain; the bass/middle/treble tone stack of a Fender Bassman or a Marshall
//! JCM800; presence and sag in the power amp; three cabinets and where the
//! microphone sits.
//!
//! The sound is the Guitar's own amp ([`AmpSim`], in
//! `instruments::builtin::guitar::amp`, which documents the model and its
//! sources), with every control brought out. It is mono, as an amp is:
//! left and right are summed into it, and both carry its output. Its
//! preamp runs at four times the sample rate, which delays it by
//! [`LATENCY`] samples; the engine lines the other tracks up with it.
//!
//! Changing the voicing or the cabinet crossfades from the old sound to
//! the new over 15 ms (two amps run while it does); knobs glide, at a
//! knob's pace through [`Effect::set_param`] and within a block through
//! [`Effect::modulate`] (see "Modulation" in `lib.rs`).
//!
//! The UI draws [`AmpSim::response_db`]: the tone stack, presence and
//! cabinet together, without the distortion.

use instruments::ParamDescriptor;
pub use instruments::builtin::guitar::amp::{
    AmpSettings, AmpSim, CABINETS, LATENCY, SwitchingAmp, VOICINGS,
};

use crate::{Category, Context, Effect, EffectDescriptor, MAX_METERS, Pace, Values, choice, float};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.amp",
    name: "Amp",
    description: "Guitar amp and cabinet: clean, crunch and lead voicings, tone stack, \
                  presence, sag, three cabinets and a microphone",
    category: Category::Distortion,
    params: PARAMS,
    meters: &[
        "Input peak (linear, 1 = 0 dBFS)",
        "Output peak (linear, 1 = 0 dBFS)",
        "Sag (dB of power-amp gain held back)",
    ],
    latency: |_, _| LATENCY,
    create: || Box::new(Amp::new()),
    ..EffectDescriptor::BASE
};

/// Slots in [`Effect::meters`].
pub const METER_INPUT: usize = 0;
pub const METER_OUTPUT: usize = 1;
pub const METER_SAG: usize = 2;

/// The knobs run 0 to 10, as an amp's do.
const PARAMS: &[ParamDescriptor] = &[
    choice("voicing", "Voicing", VOICINGS, 0),
    float("gain", "Gain", 0.0, 10.0, 4.0, "", false),
    float("bass", "Bass", 0.0, 10.0, 5.0, "", false),
    float("mid", "Middle", 0.0, 10.0, 5.0, "", false),
    float("treble", "Treble", 0.0, 10.0, 5.0, "", false),
    float("presence", "Presence", 0.0, 10.0, 4.0, "", false),
    float("sag", "Sag", 0.0, 10.0, 3.0, "", false),
    choice("cabinet", "Cabinet", CABINETS, 2),
    // 0 on the cone's axis (bright), 100% well off it (dark).
    float("mic", "Mic", 0.0, 100.0, 30.0, "%", false),
    float("output", "Output", -24.0, 12.0, 0.0, "dB", false),
];

const COUNT: usize = 10;

const VOICING: usize = 0;
const GAIN: usize = 1;
const BASS: usize = 2;
const MID: usize = 3;
const TREBLE: usize = 4;
const PRESENCE: usize = 5;
const SAG: usize = 6;
const CABINET: usize = 7;
const MIC: usize = 8;
const OUTPUT: usize = 9;

/// The settings the parameter values in `values` (in [`PARAMS`] order)
/// describe.
pub fn settings(values: &[f32]) -> AmpSettings {
    let value = |i: usize| values.get(i).copied().unwrap_or(PARAMS[i].default_value());
    AmpSettings {
        voicing: value(VOICING).round().max(0.0) as usize,
        gain: value(GAIN) / 10.0,
        bass: value(BASS) / 10.0,
        mid: value(MID) / 10.0,
        treble: value(TREBLE) / 10.0,
        presence: value(PRESENCE) / 10.0,
        sag: value(SAG) / 10.0,
        cabinet: value(CABINET).round().max(0.0) as usize,
        mic: value(MIC) / 100.0,
        output_db: value(OUTPUT),
    }
}

pub struct Amp {
    amp: SwitchingAmp,
    /// The amp keeps track of being fresh itself (`SwitchingAmp`).
    values: Values<COUNT>,
    meters: [f32; 3],
}

impl Default for Amp {
    fn default() -> Self {
        Self::new()
    }
}

impl Amp {
    pub fn new() -> Self {
        let values = Values::new(PARAMS);
        let mut amp = SwitchingAmp::new();
        amp.set(&settings(&values), false);
        amp.reset();
        Self {
            amp,
            values,
            meters: [0.0; 3],
        }
    }
}

impl Effect for Amp {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.amp.prepare(sample_rate);
        self.amp.set(&settings(&self.values), false);
        self.amp.reset();
    }

    fn reset(&mut self) {
        self.amp.reset();
        self.meters = [0.0; 3];
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.amp.process(left, right);
        self.meters = self.amp.take_meters();
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn receive(&mut self, index: usize, value: f32, pace: Pace) {
        if self.values.set(index, value) {
            self.amp
                .set(&settings(&self.values), pace == Pace::Modulation);
        }
    }

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out[METER_INPUT] = self.meters[0];
        out[METER_OUTPUT] = self.meters[1];
        out[METER_SAG] = self.meters[2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, db, peak, rms, run, set, sine};

    fn amp() -> Amp {
        let mut amp = Amp::new();
        amp.prepare(RATE, 256);
        amp
    }

    #[test]
    fn plays_at_a_sensible_level_in_every_voicing_and_cabinet() {
        for voicing in 0..VOICINGS.len() {
            for cabinet in 0..CABINETS.len() {
                let mut amp = amp();
                set(&mut amp, "voicing", voicing as f32);
                set(&mut amp, "cabinet", cabinet as f32);
                amp.reset();
                let input = sine(330.0, 0.25, RATE as usize, RATE);
                let (mut left, mut right) = (input.clone(), input.clone());
                run(&mut amp, &mut left, &mut right, 256, &Context::DEFAULT);
                let level = db(rms(&left[RATE as usize / 2..]));
                assert!(
                    (-30.0..3.0).contains(&level),
                    "voicing {voicing}, cabinet {cabinet}: {level} dB"
                );
                assert_eq!(left, right, "mono out");
                assert!(peak(&left) < 2.0);
            }
        }
    }

    #[test]
    fn the_descriptor_states_the_latency() {
        assert_eq!((DESCRIPTOR.latency)(&[], RATE), LATENCY);
        let mut amp = amp();
        let mut meters = [0.0; MAX_METERS];
        let (mut left, mut right) = (sine(220.0, 0.3, 4096, RATE), sine(220.0, 0.3, 4096, RATE));
        run(&mut amp, &mut left, &mut right, 256, &Context::DEFAULT);
        amp.meters(&mut meters);
        assert!(meters[METER_INPUT] > 0.29 && meters[METER_OUTPUT] > 0.01);
    }
}
