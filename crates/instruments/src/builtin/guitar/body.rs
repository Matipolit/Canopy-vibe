//! An acoustic guitar's body: the strings push on the bridge, and the top,
//! the back and the air inside resonate and radiate. Modelled as a bank of
//! two-pole resonators driven by the bridge force, one per body mode, heard
//! by two microphones (left and right) that pick up each mode differently.
//!
//! The main low modes are placed from measurements (see
//! [`super::models::BodySpec`]): the air mode breathing through the sound
//! hole, the top's and back's first modes and their coupled partners. Above
//! them a body's modes crowd together until they overlap; where each one
//! falls differs from guitar to guitar, and what is heard is their density,
//! damping and average level. Those are generated from the model's numbers
//! with a fixed seed, so a body always sounds the same. A little of the
//! bridge force is heard straight through as well, smoothed: what the top
//! radiates between its modes.
//!
//! A modal bank rather than a measured impulse response (commuted synthesis,
//! Smith 1993; Karjalainen, Välimäki & Jánosy 1993): sixty-four resonators,
//! run eight at a time, cost less than convolving with a response long
//! enough to hold the low modes, their frequencies can be moved while
//! playing (the body knob), and the bridge force they are driven by is also
//! what couples the strings to each other, which a body folded into each
//! string's excitation would not give.

use std::f32::consts::TAU;

use dsp::Smoothed;

use super::CONTROL_INTERVAL;
use super::models::BodySpec;

/// Resonators in the bank, a multiple of [`LANES`].
pub(super) const MAX_MODES: usize = 64;

/// Resonators run side by side, so the compiler can vectorize them.
const LANES: usize = 8;

/// The body knob's setting at which a body sounds as measured; below it
/// less of the body is heard, above it the body grows larger and boomier.
pub(super) const NATURAL: f32 = 0.6;

/// How far the modes move from the natural size at the knob's ends.
const SIZE_OCTAVES: f32 = 0.35;

/// Extra resonance at the largest body.
const BOOM: f32 = 1.2;

/// With no body at all the output is only gently smoothed.
const OPEN_TREBLE_HZ: f32 = 18_000.0;

/// Ramp for body changes, so knob moves don't click.
const RAMP_MS: f32 = 30.0;

/// A hash of `seed` and `index` as a number in `0..1`.
pub(super) fn unit_hash(seed: u32, index: u32) -> f32 {
    let mut x = seed ^ index.wrapping_mul(0x9E37_79B9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    (x >> 8) as f32 / (1 << 24) as f32
}

pub(super) struct Body {
    sample_rate: f32,
    spec: Option<&'static BodySpec>,
    /// 0 is no body at all (the bridge force heard straight, as an
    /// under-saddle pickup would), [`NATURAL`] the body as measured, 1 a
    /// larger, boomier one.
    character: Smoothed,
    countdown: usize,
    /// Modes in use, rounded up to [`LANES`].
    count: usize,
    /// Each mode at the natural size: Hz, Q, left and right levels.
    base_hz: [f32; MAX_MODES],
    base_q: [f32; MAX_MODES],
    base_left: [f32; MAX_MODES],
    base_right: [f32; MAX_MODES],
    b0: [f32; MAX_MODES],
    a1: [f32; MAX_MODES],
    a2: [f32; MAX_MODES],
    gain_left: [f32; MAX_MODES],
    gain_right: [f32; MAX_MODES],
    y1: [f32; MAX_MODES],
    y2: [f32; MAX_MODES],
    x1: f32,
    x2: f32,
    /// Level of the bridge force heard straight through.
    direct: f32,
    resonant: bool,
    treble: f32,
    treble_state: [f32; 2],
}

impl Body {
    pub fn new(character: f32) -> Self {
        let mut body = Self {
            sample_rate: 48_000.0,
            spec: None,
            character: Smoothed::new(character, RAMP_MS),
            countdown: 0,
            count: 0,
            base_hz: [1000.0; MAX_MODES],
            base_q: [10.0; MAX_MODES],
            base_left: [0.0; MAX_MODES],
            base_right: [0.0; MAX_MODES],
            b0: [0.0; MAX_MODES],
            a1: [0.0; MAX_MODES],
            a2: [0.0; MAX_MODES],
            gain_left: [0.0; MAX_MODES],
            gain_right: [0.0; MAX_MODES],
            y1: [0.0; MAX_MODES],
            y2: [0.0; MAX_MODES],
            x1: 0.0,
            x2: 0.0,
            direct: 1.0,
            resonant: false,
            treble: 0.0,
            treble_state: [0.0; 2],
        };
        body.update();
        body
    }

    pub fn prepare(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.character
            .prepare(sample_rate / CONTROL_INTERVAL as f32);
        self.reset();
    }

    /// Clears the resonators and jumps to the current setting.
    pub fn reset(&mut self) {
        self.character.settle();
        self.y1 = [0.0; MAX_MODES];
        self.y2 = [0.0; MAX_MODES];
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.treble_state = [0.0; 2];
        self.countdown = 0;
        self.update();
    }

    pub fn set_character(&mut self, character: f32) {
        self.character.set_target(character);
    }

    /// Switches to another guitar's body (`None` for none), clearing the
    /// resonators. Done while the output is faded out.
    pub fn set_spec(&mut self, spec: Option<&'static BodySpec>) {
        self.spec = spec;
        self.count = 0;
        if let Some(spec) = spec {
            let main = spec.modes.len().min(MAX_MODES);
            for (k, &(hz, q, level, pan)) in spec.modes[..main].iter().enumerate() {
                self.base_hz[k] = hz;
                self.base_q[k] = q;
                self.base_left[k] = level * (1.0 - 0.5 * pan);
                self.base_right[k] = level * (1.0 + 0.5 * pan);
            }
            let dense = spec.dense_count.min(MAX_MODES - main);
            let span = spec.dense_to_hz / spec.dense_from_hz;
            for i in 0..dense {
                let k = main + i;
                let t = (i as f32 + 0.5) / dense as f32;
                // Evenly spread on a log scale, each nudged within its slot.
                let jitter = unit_hash(spec.seed, 4 * i as u32) - 0.5;
                let t = t + 0.8 * jitter / dense as f32;
                self.base_hz[k] = spec.dense_from_hz * span.powf(t);
                self.base_q[k] = spec.dense_q[0] * (spec.dense_q[1] / spec.dense_q[0]).powf(t);
                let level =
                    spec.dense_level[0] * (spec.dense_level[1] / spec.dense_level[0]).powf(t);
                // Each microphone hears each mode with its own strength and
                // sign, which decorrelates the two channels as a real pair does.
                let side = |n: u32| {
                    let h = unit_hash(spec.seed, 4 * i as u32 + n);
                    let sign = if h < 0.5 { -1.0 } else { 1.0 };
                    sign * (0.4 + 1.2 * (2.0 * h - 1.0).abs())
                };
                self.base_left[k] = level * side(1);
                self.base_right[k] = level * side(2);
            }
            self.count = (main + dense).div_ceil(LANES) * LANES;
            for k in main + dense..self.count {
                self.base_left[k] = 0.0;
                self.base_right[k] = 0.0;
            }
        }
        self.y1 = [0.0; MAX_MODES];
        self.y2 = [0.0; MAX_MODES];
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.treble_state = [0.0; 2];
        self.update();
    }

    /// How much the modes are moved up (above 1) or down from the natural size.
    fn size(&self) -> f32 {
        ((NATURAL - self.character.current()) * SIZE_OCTAVES / (1.0 - NATURAL)).exp2()
    }

    fn update(&mut self) {
        let character = self.character.current();
        let mix = (character / NATURAL).clamp(0.0, 1.0);
        let boom = 1.0 + BOOM * ((character - NATURAL) / (1.0 - NATURAL)).max(0.0);
        let size = self.size();
        let nyquist_guard = 0.45 * self.sample_rate;
        let amount = if self.spec.is_some() { mix * boom } else { 0.0 };
        for k in 0..self.count {
            // Constant-peak bandpass (RBJ): gain 1 at the mode's frequency.
            let w = TAU * (self.base_hz[k] * size).min(nyquist_guard) / self.sample_rate;
            let alpha = w.sin() / (2.0 * self.base_q[k]);
            let norm = 1.0 / (1.0 + alpha);
            self.b0[k] = alpha * norm;
            self.a1[k] = -2.0 * w.cos() * norm;
            self.a2[k] = (1.0 - alpha) * norm;
            self.gain_left[k] = amount * self.base_left[k];
            self.gain_right[k] = amount * self.base_right[k];
        }
        let (direct, treble_hz) = match self.spec {
            Some(spec) => (
                1.0 - mix + mix * spec.direct,
                OPEN_TREBLE_HZ + (spec.treble_hz - OPEN_TREBLE_HZ) * mix,
            ),
            None => (1.0, OPEN_TREBLE_HZ),
        };
        self.direct = direct;
        self.resonant = amount > 0.0;
        if !self.resonant {
            self.y1 = [0.0; MAX_MODES];
            self.y2 = [0.0; MAX_MODES];
        }
        self.treble = (-TAU * treble_hz.min(nyquist_guard) / self.sample_rate).exp();
    }

    /// How much a fundamental at `hz` has its decay time shortened by the
    /// body taking its energy, `0..=1`: notes on the main body modes are
    /// the guitar's dead spots.
    pub fn dead_spot(&self, hz: f32) -> f32 {
        let Some(spec) = self.spec else {
            return 1.0;
        };
        let size = self.size();
        let mut drain = 0.0;
        for &(mode_hz, q, level, _) in spec.modes.iter().take(3) {
            let f = mode_hz * size;
            let detune = q * (hz / f - f / hz);
            drain += level / (1.0 + detune * detune);
        }
        let mix = (self.character.current() / NATURAL).clamp(0.0, 1.0);
        1.0 / (1.0 + spec.dead_spots * mix * drain)
    }

    /// The body's sound in `left` and `right`, driven by `bridge`.
    pub fn process(&mut self, bridge: &[f32], left: &mut [f32], right: &mut [f32]) {
        let mut start = 0;
        while start < left.len() {
            let end = if self.character.is_smoothing() {
                if self.countdown == 0 {
                    self.character.tick();
                    self.update();
                    self.countdown = CONTROL_INTERVAL;
                }
                (start + self.countdown).min(left.len())
            } else {
                left.len()
            };
            self.countdown = self.countdown.saturating_sub(end - start);
            self.process_span(
                &bridge[start..end],
                &mut left[start..end],
                &mut right[start..end],
            );
            start = end;
        }
    }

    fn process_span(&mut self, bridge: &[f32], left: &mut [f32], right: &mut [f32]) {
        let direct = self.direct;
        let treble = self.treble;
        let [mut treble_left, mut treble_right] = self.treble_state;
        let count = self.count;
        let resonant = self.resonant;
        for ((&x, l), r) in bridge.iter().zip(left.iter_mut()).zip(right.iter_mut()) {
            let (mut sum_left, mut sum_right) = (x * direct, x * direct);
            if resonant {
                let drive = x - self.x2;
                let mut acc_left = [0.0f32; LANES];
                let mut acc_right = [0.0f32; LANES];
                for base in (0..count).step_by(LANES) {
                    for j in 0..LANES {
                        let k = base + j;
                        let y =
                            self.b0[k] * drive - self.a1[k] * self.y1[k] - self.a2[k] * self.y2[k];
                        self.y2[k] = self.y1[k];
                        self.y1[k] = y;
                        acc_left[j] += self.gain_left[k] * y;
                        acc_right[j] += self.gain_right[k] * y;
                    }
                }
                sum_left += acc_left.iter().sum::<f32>();
                sum_right += acc_right.iter().sum::<f32>();
                self.x2 = self.x1;
                self.x1 = x;
            }
            treble_left = sum_left + (treble_left - sum_left) * treble;
            treble_right = sum_right + (treble_right - sum_right) * treble;
            *l = treble_left;
            *r = treble_right;
        }
        self.treble_state = [treble_left, treble_right];
        // Denormals in the decaying tails would slow the bank to a crawl.
        for k in 0..count {
            if self.y1[k].abs() < 1e-20 && self.y2[k].abs() < 1e-20 {
                self.y1[k] = 0.0;
                self.y2[k] = 0.0;
            }
        }
    }

    /// The level of the body's response to a sine at `hz` (both channels'
    /// average, linear), worked out from the filters rather than by playing
    /// one: for the panel's picture and the tests.
    pub fn response(&self, hz: f32) -> f32 {
        let w = TAU * hz / self.sample_rate;
        let (c1, s1) = (w.cos(), -w.sin());
        let (c2, s2) = ((2.0 * w).cos(), -(2.0 * w).sin());
        let (mut left_re, mut left_im) = (self.direct, 0.0);
        let (mut right_re, mut right_im) = (self.direct, 0.0);
        if self.resonant {
            for k in 0..self.count {
                // H = b0 (1 - z⁻²) / (1 + a1 z⁻¹ + a2 z⁻²)
                let (num_re, num_im) = (self.b0[k] * (1.0 - c2), -self.b0[k] * s2);
                let (den_re, den_im) = (
                    1.0 + self.a1[k] * c1 + self.a2[k] * c2,
                    self.a1[k] * s1 + self.a2[k] * s2,
                );
                let den = den_re * den_re + den_im * den_im;
                let re = (num_re * den_re + num_im * den_im) / den;
                let im = (num_im * den_re - num_re * den_im) / den;
                left_re += self.gain_left[k] * re;
                left_im += self.gain_left[k] * im;
                right_re += self.gain_right[k] * re;
                right_im += self.gain_right[k] * im;
            }
        }
        let treble = {
            let a = self.treble;
            let (den_re, den_im) = (1.0 - a * c1, -a * s1);
            (1.0 - a) / (den_re * den_re + den_im * den_im).sqrt()
        };
        0.5 * treble
            * ((left_re * left_re + left_im * left_im).sqrt()
                + (right_re * right_re + right_im * right_im).sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::ModelKind;
    use super::*;

    fn body(model: ModelKind, character: f32) -> Body {
        let mut body = Body::new(character);
        body.prepare(48_000.0);
        body.set_spec(model.spec().body);
        body
    }

    /// Level of the body's answer to a sine at `hz`, played through it.
    fn played(body: &mut Body, hz: f32) -> f32 {
        let rate = 48_000.0;
        let n = 24_000;
        let bridge: Vec<f32> = (0..n).map(|i| (TAU * hz * i as f32 / rate).sin()).collect();
        let (mut left, mut right) = (vec![0.0; n], vec![0.0; n]);
        body.process(&bridge, &mut left, &mut right);
        left[n / 2..]
            .iter()
            .chain(&right[n / 2..])
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn the_air_and_top_modes_stand_out_where_measured() {
        for model in [ModelKind::Steel, ModelKind::Nylon] {
            let body = body(model, NATURAL);
            let spec = model.spec().body.unwrap();
            for &(hz, _, _, _) in &spec.modes[..2] {
                let peak = body.response(hz);
                // Between the modes, a little to either side.
                let around = body.response(hz * 0.85).max(body.response(hz * 1.1));
                assert!(
                    peak > 1.5 * around,
                    "{model:?} mode at {hz} Hz: {peak} vs {around}"
                );
            }
        }
    }

    #[test]
    fn the_response_worked_out_matches_the_one_played() {
        let mut body = body(ModelKind::Steel, NATURAL);
        for hz in [80.0, 101.0, 186.0, 440.0, 1500.0] {
            let worked = body.response(hz);
            let heard = played(&mut body, hz);
            assert!(
                (heard / worked - 1.0).abs() < 0.35,
                "{hz} Hz: {heard} vs {worked}"
            );
        }
    }

    #[test]
    fn bigger_bodies_resonate_lower_and_no_body_passes_the_strings_through() {
        let small = body(ModelKind::Steel, 0.3);
        let big = body(ModelKind::Steel, 1.0);
        let air = 101.0 * (-SIZE_OCTAVES).exp2();
        assert!(big.response(air) > 2.0 * small.response(air));
        let mut none = body(ModelKind::Steel, 0.0);
        let level = played(&mut none, 440.0);
        assert!((level - 1.0).abs() < 0.02, "{level}");
        let mut electric = body(ModelKind::Electric, NATURAL);
        assert!((played(&mut electric, 440.0) - 1.0).abs() < 0.02);
    }

    #[test]
    fn notes_on_the_main_modes_are_dead_spots() {
        let body = body(ModelKind::Steel, NATURAL);
        let on_mode = body.dead_spot(186.0);
        let between = body.dead_spot(140.0);
        assert!(on_mode < 0.85 && between > 0.95, "{on_mode} {between}");
    }
}
