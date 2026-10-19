//! The voice filter: state-variable low/high/band/notch at 12 and 24 dB, a
//! morphing state-variable filter, a peak and two shelves, combs, a
//! flanger, a phaser, a vowel formant filter, a drivable transistor ladder,
//! an acid (three-pole, highpassed feedback) ladder, and a dirty, screaming
//! low-pass whose resonance saturates.
//!
//! Every type is a zero-delay-feedback (topology-preserving) design or a
//! plain feedback delay, so all stay stable while the cutoff moves every
//! sample. Coefficients are computed once per control tick and ramped
//! linearly to the next tick's, so fast modulation doesn't step.
//!
//! Besides cutoff and resonance each type may read the blend (what Serum
//! calls Var and Vital Blend): the peak's and shelves' gain, where Morph
//! sits between low-, band- and high-pass, how far apart the phaser's
//! notches are, the flanger's polarity.

use dsp::one_pole::tpt_lowpass;
use dsp::rational_tanh;
use std::f32::consts::PI;

pub const LP12: usize = 0;
pub const LP24: usize = 1;
pub const HP12: usize = 2;
pub const HP24: usize = 3;
pub const BP12: usize = 4;
pub const NOTCH: usize = 5;
pub const COMB_POS: usize = 6;
pub const COMB_NEG: usize = 7;
pub const FORMANT: usize = 8;
pub const LADDER: usize = 9;
/// Two band-passes in series: a narrower, steeper band.
pub const BP24: usize = 10;
/// A bell: blend sets its gain, ±24 dB (0.5 is flat), resonance its width.
pub const PEAK: usize = 11;
/// Lows below the cutoff up or down by the blend's ±24 dB.
pub const LOW_SHELF: usize = 12;
/// Highs above the cutoff up or down by the blend's ±24 dB.
pub const HIGH_SHELF: usize = 13;
/// A 12 dB state-variable filter morphing from low-pass (blend 0) through
/// band-pass (0.5) to high-pass (1), as Vital's.
pub const MORPH: usize = 14;
/// Three ladder stages with the feedback high-passed, a diode ladder's
/// character: squelchy resonance that keeps its bass (Serum's Acid Ladder).
pub const ACID: usize = 15;
/// A 12 dB low-pass whose resonance saturates instead of running away: it
/// screams at high resonance and growls under drive (Vital's Dirty).
pub const DIRTY: usize = 16;
/// Eight all-pass stages around the cutoff mixed with the dry sound: four
/// notches, spread apart by the blend, deepened by resonance (feedback).
pub const PHASER: usize = 17;
/// A short delay at the cutoff's period added to the sound: a row of
/// notches (blend below 0.5) or peaks (above), resonance feeding it back.
pub const FLANGER: usize = 18;
pub const FILTER_TYPE_COUNT: usize = 19;

pub const MIN_CUTOFF: f32 = 16.0;
/// The prewarped gain `tan(π f / fs)` goes to infinity at Nyquist.
const MAX_CUTOFF_RATIO: f32 = 0.49;
/// Q at resonance 0 for the 12 dB types: Butterworth, flat up to the cutoff.
const Q_FLAT: f32 = std::f32::consts::FRAC_1_SQRT_2;
/// Q at full resonance: a long, whistling ring short of self-oscillation.
const Q_MAX: f32 = 25.0;
/// The Dirty filter's Q at full resonance: its saturation keeps it in check.
const Q_DIRTY: f32 = 60.0;
/// The two sections of a 24 dB Butterworth: a damped one and a peaky one,
/// whose Q the resonance raises.
const K_24_FIRST: f32 = 1.847_759;
const Q_24_SECOND: f32 = 1.306_563;
/// Lowest comb pitch: the delay line holds one period of this.
const MIN_COMB_HZ: f32 = 20.0;
/// Feedback at full resonance for the combs.
const MAX_COMB_FEEDBACK: f32 = 0.97;
/// The flanger's feedback at full resonance.
const MAX_FLANGER_FEEDBACK: f32 = 0.9;
/// One-pole damping in the comb's loop: each pass loses a little top end,
/// like a plucked string, which keeps high feedback from sounding harsh.
const COMB_DAMPING: f32 = 0.65;
/// Ladder feedback at full resonance; 4 is where it starts to self-oscillate.
const LADDER_MAX_FEEDBACK: f32 = 4.0;
/// Share of the ladder's resonance loss of bass that is made up.
const LADDER_COMPENSATION: f32 = 0.5;
/// The acid ladder's feedback at full resonance: three stages oscillate
/// at 8, and the high-pass in the loop takes a little away.
const ACID_MAX_FEEDBACK: f32 = 8.4;
/// The acid ladder's feedback high-pass, as a share of the cutoff.
const ACID_HIGHPASS: f32 = 0.25;
/// The peak's and shelves' gain at blend 0 and 1, in dB.
const SHELF_RANGE_DB: f32 = 24.0;
/// The phaser's feedback at full resonance.
const PHASER_MAX_FEEDBACK: f32 = 0.85;
/// The phaser's all-pass stages.
const PHASER_STAGES: usize = 8;
/// Formants of the vowels A, E, I, O, U, swept by the cutoff knob.
const VOWELS: [[f32; 3]; 5] = [
    [730.0, 1090.0, 2440.0],
    [530.0, 1840.0, 2480.0],
    [270.0, 2290.0, 3010.0],
    [570.0, 840.0, 2410.0],
    [300.0, 870.0, 2240.0],
];
const FORMANT_GAINS: [f32; 3] = [1.0, 0.6, 0.35];
/// Most drive, as a gain into the saturator (24 dB).
const MAX_DRIVE_GAIN: f32 = 15.0;
/// Per-channel state slots: enough for the phaser's eight all-passes and
/// its feedback, three formant SVFs (two each) or a four-stage ladder;
/// slot 5 is the comb's damping.
const STATE: usize = 10;

/// Resonance `0..=1` to the damping `k = 1/Q` of a section that is flat
/// (Q = `q0`) at 0 and reaches `q_max` at 1. Exponential in Q, so the knob
/// is even to the ear.
#[inline]
fn damping_to(q0: f32, q_max: f32, resonance: f32) -> f32 {
    1.0 / (q0 * (q_max / q0).powf(resonance.clamp(0.0, 1.0)))
}

#[inline]
fn damping(q0: f32, resonance: f32) -> f32 {
    damping_to(q0, Q_MAX, resonance)
}

/// `tan(π f / fs)`, the prewarped integrator gain for a cutoff of `hz`.
#[inline]
fn gain(hz: f32, sample_rate: f32) -> f32 {
    let hz = hz.clamp(MIN_CUTOFF, sample_rate * MAX_CUTOFF_RATIO);
    (PI * hz / sample_rate).tan()
}

/// Vowel position `0..=4` (A to U) for a cutoff: the formant filter's cutoff
/// knob sweeps the vowels from 20 Hz to 20 kHz.
fn vowel_formants(cutoff: f32) -> [f32; 3] {
    let x = ((cutoff.max(20.0) / 20.0).log2() / 1000f32.log2()).clamp(0.0, 1.0) * 4.0;
    let i = (x as usize).min(3);
    let t = x - i as f32;
    std::array::from_fn(|n| VOWELS[i][n] + (VOWELS[i + 1][n] - VOWELS[i][n]) * t)
}

/// Comb feedback for a resonance, signed for the comb's polarity.
fn comb_feedback(kind: usize, resonance: f32) -> f32 {
    let fb = MAX_COMB_FEEDBACK * resonance.clamp(0.0, 1.0);
    if kind == COMB_NEG { -fb } else { fb }
}

/// The peak's and shelves' amplitude `A = 10^(dB/40)` for a blend.
fn shelf_amplitude(blend: f32) -> f32 {
    let db = (blend.clamp(0.0, 1.0) - 0.5) * 2.0 * SHELF_RANGE_DB;
    10f32.powf(db / 40.0)
}

/// How far apart neighbouring phaser stages are, as a frequency ratio.
fn phaser_spread(blend: f32) -> f32 {
    1.0 + 0.6 * blend.clamp(0.0, 1.0)
}

/// The phaser's lowest stage's integrator gain, from the center's: the
/// stages sit `spread` apart around the cutoff, each the last times
/// `spread`, so the render loop needs no powers.
fn phaser_lowest(center: f32, spread: f32) -> f32 {
    center * spread.powf(-(PHASER_STAGES as f32 - 1.0) / 2.0)
}

/// A phaser stage's integrator gain, kept where `tan` is meaningful.
#[inline(always)]
fn phaser_stage_gain(g: f32) -> f32 {
    g.clamp(1e-4, 40.0)
}

/// What a filter type needs, per control tick: up to four coefficients
/// whose meaning depends on the type.
pub fn coefficients(
    kind: usize,
    cutoff: f32,
    resonance: f32,
    blend: f32,
    sample_rate: f32,
) -> [f32; 4] {
    let cutoff = if cutoff.is_nan() { MIN_CUTOFF } else { cutoff };
    let resonance = if resonance.is_nan() {
        0.0
    } else {
        resonance.clamp(0.0, 1.0)
    };
    let blend = if blend.is_nan() {
        0.5
    } else {
        blend.clamp(0.0, 1.0)
    };
    match kind {
        LP24 | HP24 => [
            gain(cutoff, sample_rate),
            damping(Q_24_SECOND, resonance),
            0.0,
            0.0,
        ],
        COMB_POS | COMB_NEG => {
            let hz = cutoff.clamp(MIN_COMB_HZ, sample_rate * MAX_CUTOFF_RATIO);
            [sample_rate / hz, comb_feedback(kind, resonance), 0.0, 0.0]
        }
        FLANGER => {
            let hz = cutoff.clamp(MIN_COMB_HZ, sample_rate * MAX_CUTOFF_RATIO);
            let polarity = 2.0 * blend - 1.0;
            let feedback = MAX_FLANGER_FEEDBACK * resonance * polarity.signum();
            [sample_rate / hz, feedback, polarity, 0.0]
        }
        FORMANT => {
            let [f1, f2, f3] = vowel_formants(cutoff);
            let q = 4.0 + 16.0 * resonance;
            [
                gain(f1, sample_rate),
                gain(f2, sample_rate),
                gain(f3, sample_rate),
                1.0 / q,
            ]
        }
        LADDER => [
            gain(cutoff, sample_rate),
            LADDER_MAX_FEEDBACK * resonance,
            0.0,
            0.0,
        ],
        ACID => [
            gain(cutoff, sample_rate),
            ACID_MAX_FEEDBACK * resonance,
            gain(cutoff * ACID_HIGHPASS, sample_rate),
            0.0,
        ],
        PEAK => {
            let a = shelf_amplitude(blend);
            let q = 0.5 * (16.0f32).powf(resonance);
            let k = 1.0 / (q * a);
            [gain(cutoff, sample_rate), k, k * (a * a - 1.0), 0.0]
        }
        LOW_SHELF | HIGH_SHELF => {
            let a = shelf_amplitude(blend);
            let q = Q_FLAT * (4.0f32).powf(resonance);
            let k = 1.0 / q;
            let g = gain(cutoff, sample_rate);
            if kind == LOW_SHELF {
                [g / a.sqrt(), k, k * (a - 1.0), a * a - 1.0]
            } else {
                [g * a.sqrt(), k, k * (1.0 - a) * a, 1.0 - a * a]
            }
        }
        MORPH => [
            gain(cutoff, sample_rate),
            damping(Q_FLAT, resonance),
            blend,
            0.0,
        ],
        DIRTY => [
            gain(cutoff, sample_rate),
            damping_to(Q_FLAT, Q_DIRTY, resonance),
            0.0,
            0.0,
        ],
        PHASER => {
            let spread = phaser_spread(blend);
            [
                phaser_lowest(gain(cutoff, sample_rate), spread),
                PHASER_MAX_FEEDBACK * resonance,
                spread,
                0.0,
            ]
        }
        _ => [
            gain(cutoff, sample_rate),
            damping(Q_FLAT, resonance),
            0.0,
            0.0,
        ],
    }
}

/// Drive `0..=1` as the gain into the saturator.
#[inline]
pub fn drive_gain(drive: f32) -> f32 {
    1.0 + MAX_DRIVE_GAIN * drive.clamp(0.0, 1.0).powi(2)
}

/// One trapezoidal state-variable filter step. Returns (low, band, high);
/// band is not normalised (its peak is Q).
#[inline(always)]
fn svf(g: f32, k: f32, a1: f32, s: &mut [f32], x: f32) -> (f32, f32, f32) {
    let a2 = g * a1;
    let a3 = g * a2;
    let v3 = x - s[1];
    let v1 = a1 * s[0] + a2 * v3;
    let v2 = s[1] + a2 * s[0] + a3 * v3;
    s[0] = 2.0 * v1 - s[0];
    s[1] = 2.0 * v2 - s[1];
    (v2, v1, x - k * v1 - v2)
}

/// The weights Morph gives low-, band- and high-pass at `blend`.
#[inline(always)]
fn morph_weights(blend: f32) -> [f32; 3] {
    let b = blend.clamp(0.0, 1.0) * 2.0;
    if b < 1.0 {
        [1.0 - b, b, 0.0]
    } else {
        [0.0, 2.0 - b, b - 1.0]
    }
}

/// Settings ramped per sample, from their value at one control tick to the next.
#[derive(Debug, Clone, Copy, Default)]
struct Ramp {
    coef: [f32; 4],
    coef_step: [f32; 4],
    drive: f32,
    drive_step: f32,
    mix: f32,
    mix_step: f32,
}

impl Ramp {
    #[inline(always)]
    fn tick(&mut self) {
        for (c, s) in self.coef.iter_mut().zip(self.coef_step) {
            *c += s;
        }
        self.drive += self.drive_step;
        self.mix += self.mix_step;
    }
}

/// A voice's filter, two channels.
pub struct VoiceFilter {
    kind: usize,
    ramp: Ramp,
    state: [[f32; STATE]; 2],
    comb: [Vec<f32>; 2],
    comb_mask: usize,
    comb_pos: usize,
}

impl Default for VoiceFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceFilter {
    pub fn new() -> Self {
        Self {
            kind: LP24,
            ramp: Ramp::default(),
            state: [[0.0; STATE]; 2],
            comb: [Vec::new(), Vec::new()],
            comb_mask: 0,
            comb_pos: 0,
        }
    }

    /// Sizes the comb delay lines for `sample_rate`. Allocates.
    pub fn prepare(&mut self, sample_rate: f32) {
        let len = ((sample_rate / MIN_COMB_HZ) as usize + 4).next_power_of_two();
        for line in &mut self.comb {
            line.clear();
            line.resize(len, 0.0);
        }
        self.comb_mask = len - 1;
        self.reset();
    }

    pub fn reset(&mut self) {
        self.state = [[0.0; STATE]; 2];
        for line in &mut self.comb {
            line.fill(0.0);
        }
        self.comb_pos = 0;
    }

    /// Switching type clears the state, which was shaped by another topology.
    pub fn set_kind(&mut self, kind: usize) {
        if kind != self.kind {
            self.kind = kind;
            self.reset();
        }
    }

    /// The settings to reach in `steps` samples, or at once if `immediate`.
    pub fn set_targets(
        &mut self,
        coef: [f32; 4],
        drive: f32,
        mix: f32,
        steps: usize,
        immediate: bool,
    ) {
        let ramp = &mut self.ramp;
        if immediate || steps == 0 {
            *ramp = Ramp {
                coef,
                drive,
                mix,
                ..Ramp::default()
            };
            return;
        }
        let inv = 1.0 / steps as f32;
        for ((step, target), current) in ramp.coef_step.iter_mut().zip(coef).zip(ramp.coef) {
            *step = (target - current) * inv;
        }
        ramp.drive_step = (drive - ramp.drive) * inv;
        ramp.mix_step = (mix - ramp.mix) * inv;
    }

    /// Filters `left` and `right` in place.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let drive = self
            .ramp
            .drive
            .max(self.ramp.drive + self.ramp.drive_step * left.len() as f32);
        if drive > 0.0 {
            self.run::<true>(left, right);
        } else {
            self.run::<false>(left, right);
        }
        // Decaying states would otherwise sink into slow denormals.
        for v in self.state.iter_mut().flatten() {
            if v.abs() < 1e-20 {
                *v = 0.0;
            }
        }
    }

    fn run<const DRIVE: bool>(&mut self, left: &mut [f32], right: &mut [f32]) {
        match self.kind {
            LP24 | HP24 => {
                let high = self.kind == HP24;
                self.each::<DRIVE>(left, right, |c, s, x| {
                    let a1 = 1.0 / (1.0 + c[0] * (c[0] + K_24_FIRST));
                    let a1b = 1.0 / (1.0 + c[0] * (c[0] + c[1]));
                    let (s1, s2) = s.split_at_mut(2);
                    let (lp, _, hp) = svf(c[0], K_24_FIRST, a1, s1, x);
                    let (lp2, _, hp2) = svf(c[0], c[1], a1b, s2, if high { hp } else { lp });
                    if high { hp2 } else { lp2 }
                });
            }
            BP24 => {
                self.each::<DRIVE>(left, right, |c, s, x| {
                    let (g, k) = (c[0], c[1]);
                    let a1 = 1.0 / (1.0 + g * (g + k));
                    let (s1, s2) = s.split_at_mut(2);
                    let (_, band, _) = svf(g, k, a1, s1, x);
                    let (_, band2, _) = svf(g, k, a1, s2, k * band);
                    k * band2
                });
            }
            COMB_POS | COMB_NEG => self.comb::<DRIVE>(left, right),
            FLANGER => self.flanger::<DRIVE>(left, right),
            FORMANT => {
                self.each::<DRIVE>(left, right, |c, s, x| {
                    let k = c[3];
                    let mut out = 0.0;
                    for (n, gain) in FORMANT_GAINS.iter().enumerate() {
                        let g = c[n];
                        let a1 = 1.0 / (1.0 + g * (g + k));
                        let (_, band, _) = svf(g, k, a1, &mut s[2 * n..2 * n + 2], x);
                        out += gain * k * band;
                    }
                    out * 1.5
                });
            }
            LADDER => {
                self.each::<DRIVE>(left, right, |c, s, x| {
                    let (g, k) = (c[0], c[1]);
                    let big = g / (1.0 + g);
                    let b = 1.0 / (1.0 + g);
                    // Each stage is y = G·in + s/(1+g); solve the feedback
                    // loop linearly, then saturate where the loop sums.
                    let sum = big * big * big * b * s[0]
                        + big * big * b * s[1]
                        + big * b * s[2]
                        + b * s[3];
                    let g4 = big * big * big * big;
                    let input = x * (1.0 + LADDER_COMPENSATION * k);
                    let mut u = rational_tanh((input - k * sum) / (1.0 + k * g4));
                    for stage in s.iter_mut().take(4) {
                        let v = (u - *stage) * big;
                        let y = v + *stage;
                        *stage = y + v;
                        u = y;
                    }
                    u
                });
            }
            ACID => {
                self.each::<DRIVE>(left, right, |c, s, x| {
                    let (g, k, gh) = (c[0], c[1], c[2]);
                    let big = g / (1.0 + g);
                    let b = 1.0 / (1.0 + g);
                    let big_h = gh / (1.0 + gh);
                    // Three stages: y3 = G³·u + S, with S what the states
                    // add. The loop reads y3 through a one-pole high-pass,
                    // hp = (1 - Gh)·(y3 - sh). Solved for u, then saturated.
                    let sum = big * big * b * s[0] + big * b * s[1] + b * s[2];
                    let g3 = big * big * big;
                    let c_h = 1.0 - big_h;
                    // u = x - k·c_h·(g3·u + sum - s_h)
                    let u_linear = (x - k * c_h * (sum - s[3])) / (1.0 + k * c_h * g3);
                    // Saturated where the loop sums, as the ladder is.
                    let mut u = rational_tanh(u_linear);
                    for stage in s.iter_mut().take(3) {
                        let v = (u - *stage) * big;
                        let y = v + *stage;
                        *stage = y + v;
                        u = y;
                    }
                    // The feedback high-pass's own state.
                    tpt_lowpass(&mut s[3], big_h, u);
                    u * (1.0 + 0.25 * k / ACID_MAX_FEEDBACK)
                });
            }
            PEAK => {
                self.each::<DRIVE>(left, right, |c, s, x| {
                    let (g, k) = (c[0], c[1]);
                    let a1 = 1.0 / (1.0 + g * (g + k));
                    let (_, band, _) = svf(g, k, a1, s, x);
                    x + c[2] * band
                });
            }
            LOW_SHELF | HIGH_SHELF => {
                let high = self.kind == HIGH_SHELF;
                self.each::<DRIVE>(left, right, |c, s, x| {
                    let (g, k) = (c[0], c[1]);
                    let a1 = 1.0 / (1.0 + g * (g + k));
                    let (low, band, _) = svf(g, k, a1, s, x);
                    let dry = if high { 1.0 - c[3] } else { 1.0 };
                    dry * x + c[2] * band + c[3] * low
                });
            }
            MORPH => {
                self.each::<DRIVE>(left, right, |c, s, x| {
                    let (g, k) = (c[0], c[1]);
                    let a1 = 1.0 / (1.0 + g * (g + k));
                    let (low, band, high) = svf(g, k, a1, s, x);
                    let [wl, wb, wh] = morph_weights(c[2]);
                    wl * low + wb * k * band + wh * high
                });
            }
            DIRTY => {
                self.each::<DRIVE>(left, right, |c, s, x| {
                    let (g, k) = (c[0], c[1]);
                    let a1 = 1.0 / (1.0 + g * (g + k));
                    let (low, _, _) = svf(g, k, a1, s, x);
                    // The band integrator saturates: resonance grows until
                    // it clips, rather than running away.
                    s[0] = 2.0 * rational_tanh(0.5 * s[0]);
                    s[1] = 3.0 * rational_tanh(s[1] / 3.0);
                    low
                });
            }
            PHASER => {
                self.each::<DRIVE>(left, right, |c, s, x| {
                    let (mut g, feedback, spread) = (c[0], c[1], c[2]);
                    let mut u = x + feedback * s[PHASER_STAGES];
                    for state in s.iter_mut().take(PHASER_STAGES) {
                        let stage = phaser_stage_gain(g);
                        let low = tpt_lowpass(state, stage / (1.0 + stage), u);
                        u = 2.0 * low - u;
                        g *= spread;
                    }
                    s[PHASER_STAGES] = u;
                    0.5 * (x + u)
                });
            }
            kind => {
                self.each::<DRIVE>(left, right, |c, s, x| {
                    let (g, k) = (c[0], c[1]);
                    let a1 = 1.0 / (1.0 + g * (g + k));
                    let (lp, band, hp) = svf(g, k, a1, s, x);
                    match kind {
                        HP12 => hp,
                        BP12 => k * band,
                        NOTCH => x - k * band,
                        _ => lp,
                    }
                });
            }
        }
    }

    /// Runs `step` over both channels sample by sample, with the drive
    /// stage in front and the dry/wet mix after.
    #[inline(always)]
    fn each<const DRIVE: bool>(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        mut step: impl FnMut(&[f32; 4], &mut [f32; STATE], f32) -> f32,
    ) {
        let [state_l, state_r] = &mut self.state;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            self.ramp.tick();
            let ramp = &self.ramp;
            let (dry_l, dry_r) = (*l, *r);
            let (in_l, in_r) = if DRIVE {
                (drive(dry_l, ramp.drive), drive(dry_r, ramp.drive))
            } else {
                (dry_l, dry_r)
            };
            let wet_l = step(&ramp.coef, state_l, in_l);
            let wet_r = step(&ramp.coef, state_r, in_r);
            *l = dry_l + (wet_l - dry_l) * ramp.mix;
            *r = dry_r + (wet_r - dry_r) * ramp.mix;
        }
    }

    fn comb<const DRIVE: bool>(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.comb[0].is_empty() {
            return;
        }
        let mask = self.comb_mask;
        let max_delay = (mask - 2) as f32;
        let [line_l, line_r] = &mut self.comb;
        let [state_l, state_r] = &mut self.state;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            self.ramp.tick();
            let ramp = &self.ramp;
            let delay = ramp.coef[0].clamp(2.0, max_delay);
            let feedback = ramp.coef[1];
            // Feedback near 1 piles up energy at the comb's peaks; scaling
            // by sqrt(1 - fb²) keeps the overall level about even.
            let scale = (1.0 - feedback * feedback).sqrt();
            let read = self.comb_pos as f32 + (mask + 1) as f32 - delay;
            let i = read as usize;
            let t = read - i as f32;
            for (sample, line, state) in [
                (&mut *l, &mut *line_l, &mut *state_l),
                (&mut *r, &mut *line_r, &mut *state_r),
            ] {
                let dry = *sample;
                let input = if DRIVE { drive(dry, ramp.drive) } else { dry };
                let a = line[i & mask];
                let b = line[(i + 1) & mask];
                let delayed = a + (b - a) * t;
                state[5] += (delayed - state[5]) * COMB_DAMPING;
                let y = input + feedback * state[5];
                line[self.comb_pos] = y;
                let wet = y * scale;
                *sample = dry + (wet - dry) * ramp.mix;
            }
            self.comb_pos = (self.comb_pos + 1) & mask;
        }
    }

    /// The flanger: the delay line holds the input plus fed-back delayed
    /// sound, and the output adds the delayed sound to the input, with the
    /// blend's polarity.
    fn flanger<const DRIVE: bool>(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.comb[0].is_empty() {
            return;
        }
        let mask = self.comb_mask;
        let max_delay = (mask - 2) as f32;
        let [line_l, line_r] = &mut self.comb;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            self.ramp.tick();
            let ramp = &self.ramp;
            let delay = ramp.coef[0].clamp(2.0, max_delay);
            let (feedback, polarity) = (ramp.coef[1], ramp.coef[2]);
            let read = self.comb_pos as f32 + (mask + 1) as f32 - delay;
            let i = read as usize;
            let t = read - i as f32;
            for (sample, line) in [(&mut *l, &mut *line_l), (&mut *r, &mut *line_r)] {
                let dry = *sample;
                let input = if DRIVE { drive(dry, ramp.drive) } else { dry };
                let a = line[i & mask];
                let b = line[(i + 1) & mask];
                let delayed = a + (b - a) * t;
                line[self.comb_pos] = input + feedback * delayed;
                let wet = 0.5 * (input + polarity * delayed);
                *sample = dry + (wet - dry) * ramp.mix;
            }
            self.comb_pos = (self.comb_pos + 1) & mask;
        }
    }
}

/// The saturator in front of the filter: `tanh(g·x)/√g`, faded in over the
/// first tenth of the drive range so drive 0 is exactly linear.
#[inline(always)]
fn drive(x: f32, drive: f32) -> f32 {
    let g = drive_gain(drive);
    let saturated = rational_tanh(g * x) / g.sqrt();
    x + (saturated - x) * (drive * 10.0).clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Copy)]
struct Complex {
    re: f32,
    im: f32,
}

impl Complex {
    const ONE: Complex = Complex { re: 1.0, im: 0.0 };

    fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }
    fn add(self, o: Complex) -> Complex {
        Complex::new(self.re + o.re, self.im + o.im)
    }
    fn sub(self, o: Complex) -> Complex {
        Complex::new(self.re - o.re, self.im - o.im)
    }
    fn mul(self, o: Complex) -> Complex {
        Complex::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
    fn scale(self, s: f32) -> Complex {
        Complex::new(self.re * s, self.im * s)
    }
    fn div(self, o: Complex) -> Complex {
        let d = o.re * o.re + o.im * o.im;
        Complex::new(
            (self.re * o.re + self.im * o.im) / d,
            (self.im * o.re - self.re * o.im) / d,
        )
    }
    fn norm(self) -> f32 {
        (self.re * self.re + self.im * self.im).sqrt()
    }
    /// `e^{-iωd}`: a delay of `d` samples at `ω` radians per sample.
    fn delay(w: f32, d: f32) -> Complex {
        Complex::new((w * d).cos(), -(w * d).sin())
    }
}

/// The analog prototype's `s` at `freq` for a section with prewarped gain
/// `g`: bilinear-transformed filters match it exactly at the warped frequency.
fn s_at(freq: f32, g: f32, sample_rate: f32) -> Complex {
    let w = (PI * freq.clamp(0.0, sample_rate * 0.4999) / sample_rate).tan();
    Complex::new(0.0, w / g)
}

/// Second-order section responses at `s`, damping `k`: (low, band, high).
fn section(s: Complex, k: f32) -> (Complex, Complex, Complex) {
    let den = s.mul(s).add(s.scale(k)).add(Complex::ONE);
    (Complex::ONE.div(den), s.div(den), s.mul(s).div(den))
}

/// A one-pole section at `s`: its low-pass response.
fn pole(s: Complex) -> Complex {
    Complex::ONE.div(Complex::ONE.add(s))
}

/// The filter's linear response (no saturation) at `freq` Hz, as a complex
/// gain, for type `kind` with the given coefficients.
fn response(kind: usize, coef: [f32; 4], freq: f32, sample_rate: f32) -> Complex {
    let w = std::f32::consts::TAU * freq / sample_rate;
    match kind {
        LP24 | HP24 => {
            let s = s_at(freq, coef[0], sample_rate);
            let (lp1, _, hp1) = section(s, K_24_FIRST);
            let (lp2, _, hp2) = section(s, coef[1]);
            if kind == HP24 {
                hp1.mul(hp2)
            } else {
                lp1.mul(lp2)
            }
        }
        BP24 => {
            let (_, band, _) = section(s_at(freq, coef[0], sample_rate), coef[1]);
            let band = band.scale(coef[1]);
            band.mul(band)
        }
        COMB_POS | COMB_NEG => {
            let (delay, feedback) = (coef[0], coef[1]);
            let z1 = Complex::delay(w, 1.0);
            // The damping one-pole: a / (1 - (1 - a) z⁻¹).
            let damp =
                Complex::new(COMB_DAMPING, 0.0).div(Complex::ONE.sub(z1.scale(1.0 - COMB_DAMPING)));
            let loop_gain = damp.mul(Complex::delay(w, delay)).scale(feedback);
            Complex::new((1.0 - feedback * feedback).sqrt(), 0.0).div(Complex::ONE.sub(loop_gain))
        }
        FLANGER => {
            let (delay, feedback, polarity) = (coef[0], coef[1], coef[2]);
            let zd = Complex::delay(w, delay);
            let delayed = zd.div(Complex::ONE.sub(zd.scale(feedback)));
            Complex::ONE.add(delayed.scale(polarity)).scale(0.5)
        }
        FORMANT => {
            let k = coef[3];
            let mut sum = Complex::new(0.0, 0.0);
            for (n, gain) in FORMANT_GAINS.iter().enumerate() {
                let (_, band, _) = section(s_at(freq, coef[n], sample_rate), k);
                sum = sum.add(band.scale(gain * k));
            }
            sum.scale(1.5)
        }
        LADDER => {
            let stage = pole(s_at(freq, coef[0], sample_rate));
            let g4 = stage.mul(stage).mul(stage).mul(stage);
            let k = coef[1];
            g4.scale(1.0 + LADDER_COMPENSATION * k)
                .div(Complex::ONE.add(g4.scale(k)))
        }
        ACID => {
            let stage = pole(s_at(freq, coef[0], sample_rate));
            let g3 = stage.mul(stage).mul(stage);
            let s_h = s_at(freq, coef[2], sample_rate);
            let high = s_h.div(Complex::ONE.add(s_h));
            let k = coef[1];
            g3.scale(1.0 + 0.25 * k / ACID_MAX_FEEDBACK)
                .div(Complex::ONE.add(g3.mul(high).scale(k)))
        }
        PEAK => {
            let (_, band, _) = section(s_at(freq, coef[0], sample_rate), coef[1]);
            Complex::ONE.add(band.scale(coef[2]))
        }
        LOW_SHELF | HIGH_SHELF => {
            let (low, band, _) = section(s_at(freq, coef[0], sample_rate), coef[1]);
            let dry = if kind == HIGH_SHELF {
                1.0 - coef[3]
            } else {
                1.0
            };
            Complex::new(dry, 0.0)
                .add(band.scale(coef[2]))
                .add(low.scale(coef[3]))
        }
        MORPH => {
            let (low, band, high) = section(s_at(freq, coef[0], sample_rate), coef[1]);
            let [wl, wb, wh] = morph_weights(coef[2]);
            low.scale(wl)
                .add(band.scale(wb * coef[1]))
                .add(high.scale(wh))
        }
        PHASER => {
            let (mut g, feedback, spread) = (coef[0], coef[1], coef[2]);
            let mut all = Complex::ONE;
            for _ in 0..PHASER_STAGES {
                let s = s_at(freq, phaser_stage_gain(g), sample_rate);
                all = all.mul(Complex::ONE.sub(s).div(Complex::ONE.add(s)));
                g *= spread;
            }
            // The feedback is a sample late: u = x + fb·z⁻¹·out.
            let wet = all.div(Complex::ONE.sub(all.mul(Complex::delay(w, 1.0)).scale(feedback)));
            Complex::ONE.add(wet).scale(0.5)
        }
        _ => {
            let (lp, band, hp) = section(s_at(freq, coef[0], sample_rate), coef[1]);
            match kind {
                HP12 => hp,
                BP12 => band.scale(coef[1]),
                NOTCH => Complex::ONE.sub(band.scale(coef[1])),
                _ => lp,
            }
        }
    }
}

/// The filter's gain in dB at `freq` for a type, cutoff, resonance, blend
/// and mix, ignoring drive, saturation and modulation.
pub fn response_db(
    kind: usize,
    cutoff: f32,
    resonance: f32,
    blend: f32,
    mix: f32,
    freq: f32,
    sample_rate: f32,
) -> f32 {
    let coef = coefficients(kind, cutoff, resonance, blend, sample_rate);
    let wet = response(kind, coef, freq, sample_rate);
    let mix = mix.clamp(0.0, 1.0);
    let total = Complex::new(1.0 - mix, 0.0).add(wet.scale(mix));
    let db = 20.0 * total.norm().max(1e-9).log10();
    if db.is_finite() { db } else { -180.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    /// Measured RMS gain of `kind` for a sine at `hz`.
    fn measured(kind: usize, cutoff: f32, resonance: f32, blend: f32, hz: f32) -> f32 {
        let mut filter = VoiceFilter::new();
        filter.prepare(RATE);
        filter.set_kind(kind);
        filter.set_targets(
            coefficients(kind, cutoff, resonance, blend, RATE),
            0.0,
            1.0,
            0,
            true,
        );
        let n = RATE as usize / 2;
        let (mut input, mut output) = (0.0f64, 0.0f64);
        let mut block_l = [0.0f32; 32];
        let mut block_r = [0.0f32; 32];
        for start in (0..n).step_by(32) {
            for (i, (l, r)) in block_l.iter_mut().zip(block_r.iter_mut()).enumerate() {
                let x = (std::f32::consts::TAU * hz * (start + i) as f32 / RATE).sin() * 0.05;
                *l = x;
                *r = x;
            }
            let dry = block_l;
            filter.process(&mut block_l, &mut block_r);
            if start > n / 2 {
                input += dry.iter().map(|x| (x * x) as f64).sum::<f64>();
                output += block_l.iter().map(|x| (x * x) as f64).sum::<f64>();
            }
        }
        (output / input).sqrt() as f32
    }

    #[test]
    fn measured_gains_match_the_drawn_response() {
        for kind in 0..FILTER_TYPE_COUNT {
            for blend in [0.2, 0.8] {
                for hz in [100.0, 700.0, 1000.0, 3000.0] {
                    let gain = measured(kind, 1000.0, 0.3, blend, hz);
                    let drawn =
                        10f32.powf(response_db(kind, 1000.0, 0.3, blend, 1.0, hz, RATE) / 20.0);
                    if drawn < 0.03 {
                        // Near the bottom of a notch: both just need to be deep.
                        assert!(gain < 0.06, "type {kind} at {hz} Hz: {gain} vs {drawn}");
                        continue;
                    }
                    let db = 20.0 * (gain / drawn).log10();
                    // The saturating types are nearly linear for this quiet input.
                    assert!(
                        db.abs() < 1.0,
                        "type {kind}, blend {blend} at {hz} Hz: {gain} vs {drawn}"
                    );
                }
            }
        }
    }

    #[test]
    fn low_pass_slopes() {
        let pass = measured(LP12, 1000.0, 0.0, 0.5, 100.0);
        let stop12 = measured(LP12, 1000.0, 0.0, 0.5, 8000.0);
        let stop24 = measured(LP24, 1000.0, 0.0, 0.5, 8000.0);
        assert!(pass > 0.95, "{pass}");
        assert!(stop12 < 0.02, "{stop12}");
        assert!(stop24 < stop12 * 0.05, "{stop24} vs {stop12}");
    }

    #[test]
    fn the_blend_sets_the_shelves_and_the_peak() {
        let db = |kind, blend, hz| response_db(kind, 1000.0, 0.0, blend, 1.0, hz, RATE);
        for kind in [PEAK, LOW_SHELF, HIGH_SHELF] {
            for hz in [50.0, 1000.0, 15_000.0] {
                assert!(db(kind, 0.5, hz).abs() < 0.01, "type {kind} is flat at 0.5");
            }
        }
        assert!((db(PEAK, 1.0, 1000.0) - 24.0).abs() < 0.5);
        assert!((db(PEAK, 0.0, 1000.0) + 24.0).abs() < 0.5);
        assert!((db(LOW_SHELF, 1.0, 30.0) - 24.0).abs() < 0.5);
        assert!(db(LOW_SHELF, 1.0, 15_000.0).abs() < 0.5);
        assert!((db(HIGH_SHELF, 0.0, 18_000.0) + 24.0).abs() < 1.0);
        assert!(db(HIGH_SHELF, 0.0, 30.0).abs() < 0.5);
        // Morph runs low-pass, band-pass, high-pass.
        assert!(db(MORPH, 0.0, 100.0).abs() < 0.5 && db(MORPH, 0.0, 10_000.0) < -30.0);
        assert!(db(MORPH, 1.0, 10_000.0).abs() < 0.5 && db(MORPH, 1.0, 100.0) < -30.0);
        assert!(db(MORPH, 0.5, 1000.0).abs() < 0.5 && db(MORPH, 0.5, 100.0) < -15.0);
    }

    #[test]
    fn the_phaser_and_flanger_cut_notches() {
        // A deep notch somewhere in the phaser's range, and none at the ends.
        let deepest = (0..400)
            .map(|i| 100.0 * 1.01f32.powi(i))
            .map(|hz| response_db(PHASER, 1000.0, 0.0, 0.5, 1.0, hz, RATE))
            .fold(0.0f32, f32::min);
        assert!(deepest < -20.0, "{deepest}");
        assert!(response_db(PHASER, 1000.0, 0.0, 0.5, 1.0, 20.0, RATE).abs() < 1.0);
        // Negative flanging notches the fundamental's odd multiples' halves:
        // at a 1 kHz delay, 0 Hz is cancelled and 500 Hz passes doubled.
        let low = response_db(FLANGER, 1000.0, 0.0, 0.0, 1.0, 1000.0, RATE);
        let between = response_db(FLANGER, 1000.0, 0.0, 0.0, 1.0, 500.0, RATE);
        assert!(low < -30.0 && between.abs() < 0.5, "{low} {between}");
        let positive = response_db(FLANGER, 1000.0, 0.0, 1.0, 1.0, 500.0, RATE);
        assert!(positive < -30.0, "{positive}");
    }
}
