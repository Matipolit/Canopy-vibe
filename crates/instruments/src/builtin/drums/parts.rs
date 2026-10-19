//! What the drum layers are built from: noise, sweeps, a fast sine, the
//! drive curves, and the parameter constructors.

use dsp::rational_tanh;
use std::f32::consts::TAU;

use crate::ParamDescriptor;

/// A layer ends once its envelope falls below this, −80 dB: inaudible, and
/// stopping there is no click.
pub(super) const SILENCE: f32 = 1e-4;

/// Envelopes snap to zero below this (−120 dB) instead of shrinking into
/// denormal numbers, which most CPUs process a hundred times slower.
pub(super) const FLOOR: f32 = 1e-6;

/// Drive's input gain at its maximum, +24 dB into [`saturate`].
const DRIVE_GAIN: f32 = 15.0;

/// `ln(1000)`: an exponential falls 60 dB over this many time constants.
const LN_1000: f32 = 6.907_755;

/// How loud a hit at `velocity` (`0..=1`) plays: `velocity^(2 × sensitivity)`.
/// Sensitivity 0 ignores velocity, 0.5 is linear in amplitude (half velocity
/// is −6 dB), and 1 is quadratic (−12 dB), which leaves room for ghost notes.
pub(super) fn velocity_gain(velocity: f32, sensitivity: f32) -> f32 {
    velocity.clamp(0.0, 1.0).powf(2.0 * sensitivity)
}

/// The pads' and the kit's Drive: blends in [`saturate`] driven harder as
/// the amount rises, scaled so that a peak of 1 stays 1. Turning it up
/// thickens a sound rather than making its peaks louder.
#[derive(Debug, Clone, Copy)]
pub(super) struct Shaper {
    amount: f32,
    gain: f32,
    /// `1 / rational_tanh(gain)`.
    norm: f32,
}

impl Shaper {
    pub fn new() -> Self {
        let mut shaper = Self {
            amount: -1.0,
            gain: 1.0,
            norm: 1.0,
        };
        shaper.set(0.0);
        shaper
    }

    /// `amount` is `0..=1`. Cheap when unchanged.
    #[inline]
    pub fn set(&mut self, amount: f32) {
        if amount != self.amount {
            self.amount = amount;
            self.gain = 1.0 + DRIVE_GAIN * amount;
            self.norm = 1.0 / rational_tanh(self.gain);
        }
    }

    #[inline]
    pub fn process(&self, x: f32) -> f32 {
        x + self.amount * (rational_tanh(self.gain * x) * self.norm - x)
    }
}

/// A layer's Drive, on its source before the filter: like [`Shaper`], but
/// fully saturated from a tenth of the way up, so that it can square off a
/// sine (the TR-909 kick's body is a sine at 0.1) as well as crush it.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Crunch {
    wet: f32,
    gain: f32,
    norm: f32,
}

impl Crunch {
    pub fn new(amount: f32) -> Self {
        let gain = 1.0 + DRIVE_GAIN * amount;
        Self {
            wet: (amount * 10.0).min(1.0),
            gain,
            norm: 1.0 / rational_tanh(gain),
        }
    }

    pub fn is_on(&self) -> bool {
        self.wet > 0.0
    }

    #[inline]
    pub fn process(&self, x: f32) -> f32 {
        x + self.wet * (rational_tanh(self.gain * x) * self.norm - x)
    }
}

/// Xorshift white noise, uniform in `-1..1`.
#[derive(Debug, Clone, Copy)]
pub(super) struct Noise(u32);

impl Default for Noise {
    fn default() -> Self {
        Self::new(1)
    }
}

impl Noise {
    pub fn new(seed: u32) -> Self {
        // Xorshift never leaves zero.
        Self(seed.max(1))
    }

    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        dsp::xorshift32(&mut self.0)
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        self.next_u32() as i32 as f32 * (1.0 / 2_147_483_648.0)
    }

    /// A value in `0..1`.
    #[inline]
    pub fn next_phase(&mut self) -> f32 {
        dsp::noise::unit(self.next_u32())
    }
}

/// Per-sample multiplier for an exponential that falls 60 dB in `ms`.
pub(super) fn t60_mult(ms: f32, sample_rate: f32) -> f32 {
    (-LN_1000 / (ms * 0.001 * sample_rate).max(1.0)).exp()
}

/// A sweep: a frequency multiplier that starts some semitones away (up, or
/// down for negative amounts) and returns exponentially in octaves to 1, the
/// way an analog control voltage sweeps an exponential oscillator or filter.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Sweep {
    octaves: f32,
    mult: f32,
}

impl Sweep {
    /// Starts `semitones` away and covers 95% of the way back in `ms`,
    /// ticking at `rate` times a second.
    pub fn start(&mut self, semitones: f32, ms: f32, rate: f32) {
        self.octaves = semitones / 12.0;
        // e^-3 is 5%.
        self.mult = (-3.0 / (ms * 0.001 * rate).max(1.0)).exp();
    }

    pub fn is_moving(&self) -> bool {
        self.octaves != 0.0
    }

    /// The multiplier now, then one tick on.
    #[inline]
    pub fn next(&mut self) -> f32 {
        // Once within a hundredth of a cent, skip the exp2.
        if self.octaves.abs() < 1e-5 {
            self.octaves = 0.0;
            return 1.0;
        }
        let ratio = self.octaves.exp2();
        self.octaves *= self.mult;
        ratio
    }
}

/// `sin(2π · phase)`, for any phase in `-2..2` (a cycle is 1): an odd
/// polynomial on the quarter cycle, within 4e-6 of the real thing (−108 dB),
/// several times faster than `f32::sin`, which drums call a lot of.
#[inline]
pub(super) fn sin_cycles(phase: f32) -> f32 {
    // Into -0.5..=0.5, then folded onto -0.25..=0.25, where sin is odd
    // and nearly straight.
    let mut x = phase - phase as i32 as f32;
    if x > 0.5 {
        x -= 1.0;
    } else if x < -0.5 {
        x += 1.0;
    }
    if x > 0.25 {
        x = 0.5 - x;
    } else if x < -0.25 {
        x = -0.5 - x;
    }
    let t = x * TAU;
    let t2 = t * t;
    // Taylor to the 9th power: the next term is 3.6e-6 at a quarter cycle.
    t * (1.0
        + t2 * (-1.0 / 6.0 + t2 * (1.0 / 120.0 + t2 * (-1.0 / 5040.0 + t2 * (1.0 / 362_880.0)))))
}

/// `phase` moved back into `0..1` after a step of less than a cycle either way.
#[inline]
pub(super) fn wrap(phase: f32) -> f32 {
    if phase >= 1.0 {
        phase - 1.0
    } else if phase < 0.0 {
        phase + 1.0
    } else {
        phase
    }
}

/// The fractional part of a non-negative `x`.
#[inline]
pub(super) fn fract(x: f32) -> f32 {
    x - x as i32 as f32
}

/// What scales a `dsp::Svf` bandpass to unity at its center. Its gain there
/// is Q, which `dsp` takes from 0.5 at resonance 0 to 20 at 1, linear in
/// damping (`1 / Q`).
pub(super) fn bandpass_norm(resonance: f32) -> f32 {
    2.0 - 1.95 * resonance
}

/// `2^(semitones / 12)`.
pub(super) fn semitones(semitones: f32) -> f32 {
    (semitones / 12.0).exp2()
}

/// [`crate::float`], with the id and name together, as the kit's tables
/// make them.
pub(super) const fn float(
    (id, name): (&'static str, &'static str),
    min: f32,
    max: f32,
    default: f32,
    unit: &'static str,
    logarithmic: bool,
) -> ParamDescriptor {
    crate::float(id, name, min, max, default, unit, logarithmic)
}

/// [`crate::choice`], with the id and name together.
pub(super) const fn choice(
    (id, name): (&'static str, &'static str),
    labels: &'static [&'static str],
    default: usize,
) -> ParamDescriptor {
    crate::choice(id, name, labels, default)
}

/// A pitch in Hz, from sub-bass to the top of hearing.
pub(super) const fn hz(names: (&'static str, &'static str), default: f32) -> ParamDescriptor {
    float(names, 10.0, 20_000.0, default, "Hz", true)
}

/// A pitch amount in semitones: a drop, a transposition.
pub(super) const fn semis(
    names: (&'static str, &'static str),
    min: f32,
    max: f32,
    default: f32,
) -> ParamDescriptor {
    float(names, min, max, default, "st", false)
}

/// A time on a log scale, so short times get as much of the knob as long ones.
pub(super) const fn ms(
    names: (&'static str, &'static str),
    min: f32,
    max: f32,
    default: f32,
) -> ParamDescriptor {
    float(names, min, max, default, "ms", true)
}

pub(super) const fn level(names: (&'static str, &'static str), default: f32) -> ParamDescriptor {
    float(names, -60.0, 6.0, default, "dB", false)
}

/// `-1` is hard left, `1` hard right.
pub(super) const fn pan(names: (&'static str, &'static str), default: f32) -> ParamDescriptor {
    float(names, -1.0, 1.0, default, "", false)
}

/// A `0..=1` control: drive, velocity sensitivity, shape...
pub(super) const fn amount(names: (&'static str, &'static str), default: f32) -> ParamDescriptor {
    float(names, 0.0, 1.0, default, "", false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    #[test]
    fn sweep_covers_95_percent_in_its_time_either_way() {
        for semis in [24.0f32, -12.0] {
            let mut sweep = Sweep::default();
            sweep.start(semis, 50.0, RATE);
            assert_eq!(sweep.next(), semitones(semis));
            for _ in 1..2400 {
                sweep.next();
            }
            let now = 12.0 * sweep.next().log2();
            let expected = semis * (-3.0f32).exp();
            assert!((now - expected).abs() < 0.05, "{semis}: {now}");
        }
    }

    #[test]
    fn sin_cycles_matches_sin() {
        let mut worst = 0.0f32;
        for i in -40_000..=40_000 {
            let phase = i as f32 / 20_000.0;
            let error = (sin_cycles(phase) - (phase * TAU).sin()).abs();
            worst = worst.max(error);
        }
        assert!(worst < 5e-6, "{worst}");
        assert_eq!(sin_cycles(0.0), 0.0);
    }

    #[test]
    fn shaper_keeps_unit_peaks_and_is_clean_at_zero() {
        let mut shaper = Shaper::new();
        for x in [-1.0, -0.3, 0.0, 0.2, 1.0] {
            assert_eq!(shaper.process(x), x);
        }
        for amount in [0.1, 0.5, 1.0] {
            shaper.set(amount);
            assert!((shaper.process(1.0) - 1.0).abs() < 1e-6);
            assert!(shaper.process(0.3) > 0.3, "thickens below the peak");
        }
    }

    #[test]
    fn crunch_is_clean_at_zero_and_squares_off_a_sine() {
        let clean = Crunch::new(0.0);
        assert!(!clean.is_on());
        assert_eq!(clean.process(0.4), 0.4);
        let crunch = Crunch::new(1.0);
        assert!((crunch.process(1.0) - 1.0).abs() < 1e-6);
        assert!(crunch.process(0.2) > 0.9, "nearly square");
    }

    #[test]
    fn velocity_curves() {
        assert_eq!(velocity_gain(0.5, 0.0), 1.0);
        assert!((velocity_gain(0.5, 0.5) - 0.5).abs() < 1e-6);
        assert!((velocity_gain(0.5, 1.0) - 0.25).abs() < 1e-6);
        assert_eq!(velocity_gain(1.0, 1.0), 1.0);
    }
}
