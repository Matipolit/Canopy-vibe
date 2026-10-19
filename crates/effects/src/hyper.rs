//! Hyper/Dimension: Serum's unison-in-a-box. Hyper makes up to seven
//! detuned copies of the sound, spread across the stereo field, as a
//! synth's unison does (it can stand in for it, for a fraction of the
//! CPU); Dimension then widens the result with short delays that cancel in
//! mono, after Roland's Dimension D.
//!
//! **Hyper**: each voice is the sound through its own short delay (a few
//! ms, different for each) that a triangle LFO sweeps up and down. While
//! the delay shortens the voice plays sharp, while it lengthens flat, by
//! the same amount all the way, so each voice is detuned rather than
//! warbled; their LFOs run at slightly different rates and start spread
//! through the cycle, so at any moment some are sharp and some flat, as a
//! supersaw's voices are. Detune sets how far (up to [`MAX_CENTS`]), Rate
//! how often each turns around. The triangle's corners are rounded into
//! short parabolas ([`CORNER`] of the cycle each), so a voice's pitch glides
//! through the turn rather than flipping. The voices are
//! panned evenly from left to right, each reading the side it sits on,
//! and summed at equal power; Mix blends them with the dry sound. Changing
//! the number of voices fades them in and out one after another.
//!
//! **Dimension**: the mid of the sound through four short delays
//! (scaled by Size), each with its level drifting slowly, alternately
//! added and taken away, and the sum added to the left and taken from the
//! right: a side signal of the sound's own echoes, which widens it without
//! moving its centre and cancels out completely in mono. Its knob is how
//! much.
//!
//! **Modulation** (see `lib.rs`): every float glides, at the knob's pace or
//! within a block for a modulator, and no voice's delay ever moves faster
//! than [`MAX_TAP_SLOPE`] samples a sample, whatever moves the detune, rate
//! or size, so the pitch bends but never runs backwards.

use std::f32::consts::{FRAC_PI_2, TAU};

use instruments::ParamDescriptor;

use crate::line::{Ring, slew};
use crate::meter::PeakMeter;
use crate::{Category, Context, Effect, EffectDescriptor, MAX_METERS, Values, choice, float};
use dsp::{Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.hyper",
    name: "Hyper/Dimension",
    description: "Unison-style detuned voices spread wide, and a Dimension widener",
    category: Category::Modulation,
    params: PARAMS,
    meters: &[
        "Output peak (linear, 1 = 0 dBFS)",
        "Voices heard (fractional while fading)",
        "Voice 1 pitch now (cents)",
        "Voice 2 pitch now (cents)",
        "Voice 3 pitch now (cents)",
        "Voice 4 pitch now (cents)",
        "Voice 5 pitch now (cents)",
        "Voice 6 pitch now (cents)",
        "Voice 7 pitch now (cents)",
    ],
    create: || Box::new(Hyper::new()),
    ..EffectDescriptor::BASE
};

/// The most voices.
pub const MAX_VOICES: usize = 7;

const VOICE_LABELS: &[&str] = &["1", "2", "3", "4", "5", "6", "7"];

const PARAMS: &[ParamDescriptor] = &[
    choice("voices", "Voices", VOICE_LABELS, 4),
    float("detune", "Detune", 0.0, 100.0, 35.0, "%", false),
    float("rate", "Rate", 0.05, 10.0, 0.6, "Hz", true),
    float("hyper_mix", "Hyper mix", 0.0, 100.0, 50.0, "%", false),
    float("size", "Size", 0.0, 100.0, 50.0, "%", false),
    float("dimension", "Dimension", 0.0, 100.0, 30.0, "%", false),
    float("output", "Output", -24.0, 12.0, 0.0, "dB", false),
];

const COUNT: usize = 7;

/// 1 to 7 voices, as labels 0 to 6.
pub const VOICES: usize = 0;
/// 0 to 100%: how far each voice is detuned, up to [`MAX_CENTS`].
pub const DETUNE: usize = 1;
/// How often each voice turns from sharp to flat and back, in Hz.
pub const RATE: usize = 2;
pub const HYPER_MIX: usize = 3;
/// 0 to 100%: Dimension's delays from [`SIZE_SCALE`]'s first to its last
/// times their lengths.
pub const SIZE: usize = 4;
/// How much of Dimension's side signal is added.
pub const DIMENSION: usize = 5;
pub const OUTPUT: usize = 6;

/// Meter slots, in [`DESCRIPTOR`]'s `meters` order. The output peak comes
/// first so that silence and a reset read 0, as [`Effect::meter`] expects.
pub const METER_OUTPUT: usize = 0;
pub const METER_VOICES: usize = 1;
/// Voice `n`'s pitch is at `METER_PITCH + n`.
pub const METER_PITCH: usize = 2;

/// How far each voice is detuned at full Detune, in cents either way.
pub const MAX_CENTS: f32 = 60.0;
/// Each voice's shortest delay, in ms, and its LFO's rate as a share of
/// Rate: spread so they rarely line up.
const VOICE_DELAY_MS: [f32; MAX_VOICES] = [5.0, 7.3, 6.1, 8.9, 5.6, 9.7, 6.8];
const VOICE_RATES: [f32; MAX_VOICES] = [1.0, 1.21, 0.83, 1.43, 0.71, 1.13, 0.91];
/// The most an LFO swings a voice's delay, in ms: at slow rates and wide
/// detune it's held to this, and the detune is a little less.
const MAX_SWING_MS: f32 = 40.0;
/// The share of the LFO's cycle each rounded corner takes, either side of
/// the peak.
pub const CORNER: f32 = 0.04;
/// Dimension's delays at full scale, in ms, their signs, and how fast and
/// far their levels drift.
const DIMENSION_MS: [f32; 4] = [7.1, 11.3, 13.7, 17.9];
const DIMENSION_SIGNS: [f32; 4] = [1.0, -1.0, 1.0, -1.0];
const DRIFT_HZ: [f32; 4] = [0.13, 0.19, 0.23, 0.29];
const DRIFT_DEPTH: f32 = 0.3;
/// The drifts move once every this many samples.
const DRIFT_CHUNK: usize = 32;
/// Dimension's delays scale from the first to the second of these as Size
/// turns.
pub const SIZE_SCALE: (f32, f32) = (0.3, 1.6);
/// The fastest any delay moves, in samples per sample.
pub const MAX_TAP_SLOPE: f32 = 0.5;
/// How long the floats take to reach a new value, and the voices to fade.
const SMOOTH_MS: f32 = 20.0;
const RATE_GLIDE_MS: f32 = 50.0;
const VOICE_FADE_MS: f32 = 60.0;
/// The shortest delay read, in samples: the cubic needs one either side.
const MIN_DELAY: f32 = 2.0;

/// The fractional part of `x`, which is at least 0.
#[inline]
fn wrap(x: f32) -> f32 {
    x - x as i32 as f32
}

/// The voices' LFO at `phase` (0 to 1): a triangle rising through 0 at the
/// start, rising and falling by 4 a cycle, its peak (at a quarter) and
/// trough (at three quarters) rounded into parabolas [`CORNER`] wide that
/// meet the ramps without a kink.
#[inline]
pub fn lfo(phase: f32) -> f32 {
    // How far from the peak, -0.5 to 0.5, and from the trough.
    let from_peak = wrap(phase + 0.25) - 0.5;
    let from_trough = 0.5 - from_peak.abs();
    let corner = |d: f32| 1.0 - 2.0 * CORNER - 2.0 * d * d / CORNER;
    if from_peak.abs() < CORNER {
        corner(from_peak)
    } else if from_trough < CORNER {
        -corner(from_trough)
    } else {
        1.0 - 4.0 * from_peak.abs()
    }
}

/// Where voice `voice` sits, from -1 (left) to 1 (right), when `voices`
/// are heard (fractional while fading, so the voices slide apart as one
/// fades in rather than jump).
pub fn voice_pan(voice: usize, voices: f32) -> f32 {
    let pan = if voices <= 1.0 {
        0.0
    } else if voices < 2.0 {
        // From one voice in the middle to two at the edges.
        (voices - 1.0) * (2.0 * voice as f32 - 1.0)
    } else {
        2.0 * voice as f32 / (voices - 1.0) - 1.0
    };
    pan.clamp(-1.0, 1.0)
}

/// Detune (0 to 1) as cents: gentle at first, for the subtle settings
/// most sounds want.
#[inline]
pub fn detune_cents(detune: f32) -> f32 {
    MAX_CENTS * detune.clamp(0.0, 1.0).powf(1.5)
}

/// Dimension's scale at Size `size` (0 to 1).
#[inline]
fn size_scale(size: f32) -> f32 {
    SIZE_SCALE.0 * (SIZE_SCALE.1 / SIZE_SCALE.0).powf(size.clamp(0.0, 1.0))
}

pub struct Hyper {
    values: Values<COUNT>,
    sample_rate: f32,
    /// Left and right lines of what came in, and the mid's for Dimension.
    lines: [Ring; 2],
    mid: Ring,
    /// The longest delay the lines can be read at, in samples.
    longest: f32,
    /// Each voice's LFO phase (0 to 1), and Dimension's drifts', their
    /// levels now, and the samples until they next move.
    phases: [f32; MAX_VOICES],
    drifts: [f32; 4],
    drift_levels: [f32; 4],
    drift_countdown: usize,
    /// Where each voice and each Dimension delay read last sample, and
    /// whether they've been placed since a reset.
    taps: [f32; MAX_VOICES],
    dimension_taps: [f32; 4],
    placed: bool,
    /// Each voice's delay change last sample, for its pitch meter.
    moved: [f32; MAX_VOICES],
    /// Each voice's swing (samples), and its gains into the left and
    /// right and how much of each line it reads: worked out again only
    /// while the voices, detune or rate move.
    swings: [f32; MAX_VOICES],
    voice_gains: [[f32; 3]; MAX_VOICES],
    tuned: bool,
    /// How many voices (1 to 7), fractional while fading.
    voices: Smoothed,
    detune: Smoothed,
    rate: Smoothed,
    hyper_mix: Smoothed,
    size: Smoothed,
    dimension: Smoothed,
    output: Smoothed,
    output_peak: PeakMeter,
}

impl Hyper {
    pub fn new() -> Hyper {
        let smoothed = |value: f32| Smoothed::new(value, SMOOTH_MS);
        let mut hyper = Hyper {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            lines: [Ring::default(), Ring::default()],
            mid: Ring::default(),
            longest: MIN_DELAY,
            phases: [0.0; MAX_VOICES],
            drifts: [0.0; 4],
            drift_levels: [1.0; 4],
            drift_countdown: 0,
            taps: [0.0; MAX_VOICES],
            dimension_taps: [0.0; 4],
            placed: false,
            moved: [0.0; MAX_VOICES],
            swings: [0.0; MAX_VOICES],
            voice_gains: [[0.0; 3]; MAX_VOICES],
            tuned: false,
            voices: Smoothed::new(1.0, VOICE_FADE_MS),
            detune: smoothed(0.0),
            rate: Smoothed::new(1.0, RATE_GLIDE_MS),
            hyper_mix: smoothed(0.0),
            size: smoothed(0.5),
            dimension: smoothed(0.0),
            output: smoothed(1.0),
            output_peak: PeakMeter::default(),
        };
        hyper.prepare(48_000.0, 0);
        hyper
    }

    fn smoothers(&mut self) -> [&mut Smoothed; 7] {
        [
            &mut self.voices,
            &mut self.detune,
            &mut self.rate,
            &mut self.hyper_mix,
            &mut self.size,
            &mut self.dimension,
            &mut self.output,
        ]
    }

    /// The voices' and drifts' starting phases: spread through the cycle,
    /// so half the voices start sharp and half flat.
    fn start_phases(&mut self) {
        self.phases = std::array::from_fn(|i| wrap(i as f32 * 0.618_034 + 0.1));
        self.drifts = std::array::from_fn(|i| i as f32 * 0.25);
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`; the voice count always fades at its own pace.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            VOICES => self.voices.set_target(value + 1.0),
            DETUNE => self.detune.move_to(value / 100.0, pace),
            RATE => self.rate.move_to(value, pace),
            HYPER_MIX => self.hyper_mix.move_to(value / 100.0, pace),
            SIZE => self.size.move_to(value / 100.0, pace),
            DIMENSION => self.dimension.move_to(value / 100.0, pace),
            OUTPUT => self.output.move_to(dsp::db_to_gain(value), pace),
            _ => {}
        }
    }
}

impl Default for Hyper {
    fn default() -> Self {
        Hyper::new()
    }
}

impl Effect for Hyper {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        let most = |values: &[f32]| values.iter().fold(0.0f32, |a, &b| a.max(b));
        let voice_ms = most(&VOICE_DELAY_MS) + 2.0 * MAX_SWING_MS;
        let dimension_ms = most(&DIMENSION_MS) * SIZE_SCALE.1;
        let len = (voice_ms.max(dimension_ms) * 0.001 * sample_rate).ceil() as usize + 4;
        for line in self.lines.iter_mut().chain([&mut self.mid]) {
            line.allocate(len);
        }
        self.longest = len as f32 - 3.0;
        for smoothed in self.smoothers() {
            smoothed.prepare(sample_rate);
        }
        self.output_peak.prepare(sample_rate);
        for index in 0..COUNT {
            self.apply(index, Pace::Knob);
        }
        self.reset();
    }

    fn reset(&mut self) {
        for line in self.lines.iter_mut().chain([&mut self.mid]) {
            line.clear();
        }
        for smoothed in self.smoothers() {
            smoothed.settle();
        }
        self.start_phases();
        self.drift_countdown = 0;
        self.placed = false;
        self.tuned = false;
        self.moved = [0.0; MAX_VOICES];
        self.output_peak.reset();
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.values.played();
        if self.lines[0].len() <= 1 {
            // Not prepared: pass the sound through.
            return;
        }
        let sample_rate = self.sample_rate;
        let per_ms = sample_rate * 0.001;
        let longest = self.longest;
        let len = left.len().min(right.len());
        let mut output_peak = 0.0f32;
        for n in 0..len {
            let retune = !self.tuned
                || self.voices.is_smoothing()
                || self.detune.is_smoothing()
                || self.rate.is_smoothing();
            let voices = self.voices.tick();
            let cents = detune_cents(self.detune.tick());
            let rate = self.rate.tick();
            if retune {
                // A triangle moves the delay by 4·swing a cycle, which
                // bends the pitch by that slope.
                let slope = 1.0 - (-cents / 1200.0).exp2();
                let each = self
                    .swings
                    .iter_mut()
                    .zip(&mut self.voice_gains)
                    .zip(VOICE_RATES);
                for (voice, ((swing, gains), ratio)) in each.enumerate() {
                    let voice_rate = (rate * ratio).max(1e-3);
                    *swing = (slope * sample_rate / (4.0 * voice_rate))
                        .min(MAX_SWING_MS * 0.001 * sample_rate);
                    // Where it sits, as the share of the right line it
                    // reads and its equal-power gains.
                    let side = 0.5 * (voice_pan(voice, voices) + 1.0);
                    let angle = side * FRAC_PI_2;
                    *gains = [angle.cos(), angle.sin(), side];
                }
                self.tuned = true;
            }
            let hyper_mix = self.hyper_mix.tick();
            let scale = size_scale(self.size.tick());
            let dimension = self.dimension.tick();
            let output = self.output.tick();
            let clean = |x: f32| if x.is_finite() { x } else { 0.0 };
            let dry = [clean(left[n]), clean(right[n])];
            let [line_l, line_r] = &mut self.lines;
            line_l.push(dry[0]);
            line_r.push(dry[1]);
            let placed = self.placed;

            // Hyper: every voice that's heard, panned and summed at equal
            // power.
            let count = voices.ceil() as usize;
            let mut wet = [0.0f32; 2];
            for voice in 0..MAX_VOICES {
                let voice_rate = rate * VOICE_RATES[voice];
                self.phases[voice] = wrap(self.phases[voice] + voice_rate / sample_rate);
                // Voices fade in and out one after another.
                let level = (voices - voice as f32).clamp(0.0, 1.0);
                if level <= 0.0 && voice >= count {
                    continue;
                }
                let wanted = (VOICE_DELAY_MS[voice] * per_ms
                    + self.swings[voice] * (1.0 + lfo(self.phases[voice])))
                .clamp(MIN_DELAY, longest);
                let before = self.taps[voice];
                let at = slew(&mut self.taps[voice], wanted, placed, MAX_TAP_SLOPE);
                self.moved[voice] = if placed { at - before } else { 0.0 };
                let [to_left, to_right, side] = self.voice_gains[voice];
                let x = self.lines[0].delayed(at) * (1.0 - side) + self.lines[1].delayed(at) * side;
                wet[0] += level * to_left * x;
                wet[1] += level * to_right * x;
            }
            // Equal power for the voices heard, and a centred voice at
            // unity on both sides.
            let norm = std::f32::consts::SQRT_2 / voices.max(1.0).sqrt();
            let mut out = [0.0f32; 2];
            for side in 0..2 {
                out[side] = dry[side] + hyper_mix * (norm * wet[side] - dry[side]);
            }

            // Dimension: the mid's drifting echoes, as a side signal.
            let mid = 0.5 * (out[0] + out[1]);
            self.mid.push(mid);
            // The drifts are slow: they move once every DRIFT_CHUNK
            // samples.
            if self.drift_countdown == 0 {
                self.drift_countdown = DRIFT_CHUNK;
                let step = DRIFT_CHUNK as f32 / sample_rate;
                for ((phase, level), hz) in self
                    .drifts
                    .iter_mut()
                    .zip(&mut self.drift_levels)
                    .zip(DRIFT_HZ)
                {
                    *phase = wrap(*phase + hz * step);
                    *level = 1.0 + DRIFT_DEPTH * (TAU * *phase).sin();
                }
            }
            self.drift_countdown -= 1;
            if dimension > 0.0 {
                let mut side = 0.0;
                let each = self
                    .dimension_taps
                    .iter_mut()
                    .zip(DIMENSION_MS)
                    .zip(DIMENSION_SIGNS.iter().zip(&self.drift_levels));
                for ((tap, ms), (sign, drift)) in each {
                    let at = slew(
                        tap,
                        (ms * scale * per_ms).clamp(MIN_DELAY, longest),
                        placed,
                        MAX_TAP_SLOPE,
                    );
                    side += sign * drift * self.mid.delayed(at);
                }
                side *= 0.5 * dimension;
                out[0] += side;
                out[1] -= side;
            } else {
                for (tap, ms) in self.dimension_taps.iter_mut().zip(DIMENSION_MS) {
                    slew(
                        tap,
                        (ms * scale * per_ms).clamp(MIN_DELAY, longest),
                        placed,
                        MAX_TAP_SLOPE,
                    );
                }
            }
            self.placed = true;
            left[n] = output * out[0];
            right[n] = output * out[1];
            output_peak = output_peak.max(left[n].abs()).max(right[n].abs());
        }
        self.output_peak.update(output_peak, len);
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn receive(&mut self, index: usize, value: f32, pace: Pace) {
        if self.values.set(index, value) {
            self.apply(index, pace);
            if self.values.is_fresh() {
                // Cheap: the lines clear in constant time.
                self.reset();
            }
        }
    }

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out[METER_OUTPUT] = self.output_peak.level();
        out[METER_VOICES] = self.voices.current();
        for (voice, moved) in self.moved.iter().enumerate() {
            // Reading a delay that shortens by `moved` a sample plays
            // `1 - moved` as fast.
            out[METER_PITCH + voice] = 1200.0 * (1.0 - moved).max(0.01).log2();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, db, noise, rms, run, set, sine};

    fn hyper(settings: &[(&str, f32)]) -> Hyper {
        let mut hyper = Hyper::new();
        hyper.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut hyper, id, value);
        }
        hyper.reset();
        hyper
    }

    fn stereo(hyper: &mut Hyper, left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (left.to_vec(), right.to_vec());
        run(hyper, &mut l, &mut r, 64, &Context::DEFAULT);
        (l, r)
    }

    #[test]
    fn parameters_are_laid_out_as_documented() {
        assert_eq!(PARAMS.len(), COUNT);
        let ids = [
            (VOICES, "voices"),
            (DETUNE, "detune"),
            (super::RATE, "rate"),
            (HYPER_MIX, "hyper_mix"),
            (SIZE, "size"),
            (DIMENSION, "dimension"),
            (OUTPUT, "output"),
        ];
        for (index, id) in ids {
            assert_eq!(PARAMS[index].id, id);
        }
        assert_eq!(VOICE_LABELS.len(), MAX_VOICES);
    }

    #[test]
    fn the_lfo_ramps_steadily_and_turns_smoothly() {
        let step = 1e-4;
        let mut previous = lfo(0.0);
        let mut slopes = Vec::new();
        for i in 1..=10_000 {
            let value = lfo(i as f32 * step);
            slopes.push((value - previous) / step);
            assert!(value.abs() <= 1.0);
            previous = value;
        }
        // Up or down by 4 a cycle away from the corners, never steeper,
        // and no jump in the slope anywhere.
        assert!(slopes.iter().all(|s| s.abs() <= 4.01));
        assert!((slopes[1000] - 4.0).abs() < 1e-2 && (slopes[5000] + 4.0).abs() < 1e-2);
        assert!(slopes.windows(2).all(|w| (w[1] - w[0]).abs() < 0.05));
        assert!(lfo(0.0).abs() < 1e-6 && (lfo(0.25) - (1.0 - 2.0 * CORNER)).abs() < 1e-6);
    }

    #[test]
    fn a_voice_is_detuned_both_ways_by_the_cents_set() {
        // One voice, all wet: a 1 kHz sine comes out a steady few cents
        // sharp for half the LFO's cycle and as many flat for the other.
        let mut one = hyper(&[
            ("voices", 0.0),
            ("detune", 50.0),
            ("rate", 1.0),
            ("hyper_mix", 100.0),
            ("dimension", 0.0),
        ]);
        let input = sine(1000.0, 0.5, 96_000, RATE);
        let (left, _) = stereo(&mut one, &input, &input);
        let crossings: Vec<f64> = (4800..left.len())
            .filter(|&n| left[n - 1] <= 0.0 && left[n] > 0.0)
            .map(|n| {
                let (a, b) = (f64::from(left[n - 1]), f64::from(left[n]));
                n as f64 - 1.0 + a / (a - b)
            })
            .collect();
        let cents: Vec<f64> = crossings
            .windows(2)
            .map(|w| 1200.0 * (48.0 / (w[1] - w[0])).log2())
            .collect();
        let sharpest = cents.iter().copied().fold(f64::MIN, f64::max);
        let flattest = cents.iter().copied().fold(f64::MAX, f64::min);
        let expected = f64::from(detune_cents(0.5));
        println!("detune {expected:.1} cents: measured {flattest:.1} to {sharpest:.1}");
        assert!((sharpest - expected).abs() < 1.0 && (flattest + expected).abs() < 1.0);
        // Most of the time, it's at one or the other.
        let steady = cents.iter().filter(|c| c.abs() > expected - 1.0).count();
        assert!(
            steady as f64 > 0.8 * cents.len() as f64,
            "{steady} of {}",
            cents.len()
        );
    }

    #[test]
    fn hyper_widens_and_the_mix_blends() {
        let input = noise(48_000, 3);
        let width = |settings: &[(&str, f32)]| {
            let (l, r) = stereo(&mut hyper(settings), &input, &input);
            let side: Vec<f32> = l.iter().zip(&r).map(|(l, r)| 0.5 * (l - r)).collect();
            db(rms(&side[9600..]) / rms(&input[9600..]))
        };
        let seven = width(&[("voices", 6.0), ("hyper_mix", 100.0), ("dimension", 0.0)]);
        let one = width(&[("voices", 0.0), ("hyper_mix", 100.0), ("dimension", 0.0)]);
        println!("side level: 7 voices {seven:.1} dB, 1 voice {one:.1} dB");
        assert!(seven > -9.0 && one < -60.0);
        // No mix and no dimension: the sound as it came, bit for bit.
        let mut dry = hyper(&[("hyper_mix", 0.0), ("dimension", 0.0)]);
        let right: Vec<f32> = input.iter().map(|s| -0.3 * s).collect();
        assert_eq!(stereo(&mut dry, &input, &right), (input.clone(), right));
    }

    #[test]
    fn dimension_widens_and_cancels_in_mono() {
        let input = noise(48_000, 5);
        let mut wide = hyper(&[("hyper_mix", 0.0), ("dimension", 100.0)]);
        let (l, r) = stereo(&mut wide, &input, &input);
        let side: Vec<f32> = l.iter().zip(&r).map(|(l, r)| 0.5 * (l - r)).collect();
        let side_db = db(rms(&side[9600..]) / rms(&input[9600..]));
        println!("dimension at 100%: side {side_db:.1} dB");
        assert!(side_db > -12.0);
        // The mid is the input's, untouched.
        for n in 0..input.len() {
            assert!((0.5 * (l[n] + r[n]) - input[n]).abs() < 1e-6);
        }
    }

    #[test]
    fn changing_settings_does_not_click() {
        let input = sine(110.0, 0.4, 96_000, RATE);
        let largest_step = |hyper: &mut Hyper, change: Option<(usize, f32)>| {
            let (mut l, mut r) = (input.clone(), input.clone());
            for (block, (l, r)) in l.chunks_mut(64).zip(r.chunks_mut(64)).enumerate() {
                if block == 750
                    && let Some((index, value)) = change
                {
                    hyper.set_param(index, value);
                }
                hyper.process(l, r, &Context::DEFAULT);
            }
            l[4800..]
                .windows(2)
                .chain(r[4800..].windows(2))
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max)
        };
        let settings = [("hyper_mix", 80.0), ("dimension", 50.0), ("voices", 2.0)];
        let steady = largest_step(&mut hyper(&settings), None);
        for (index, value) in [(VOICES, 6.0), (VOICES, 0.0), (DETUNE, 100.0), (SIZE, 0.0)] {
            let changed = largest_step(&mut hyper(&settings), Some((index, value)));
            println!("{}: {changed:.4} against {steady:.4}", PARAMS[index].id);
            assert!(changed < 1.3 * steady, "{} {value}", PARAMS[index].id);
        }
    }

    #[test]
    fn reset_silences_and_meters_show_the_voices() {
        let mut hyper = hyper(&[("voices", 6.0), ("detune", 60.0), ("hyper_mix", 100.0)]);
        let input = noise(9600, 7);
        stereo(&mut hyper, &input, &input);
        let mut meters = [0.0; MAX_METERS];
        hyper.meters(&mut meters);
        assert_eq!(meters[METER_VOICES], 7.0);
        let cents = detune_cents(0.6);
        let pitches = &meters[METER_PITCH..METER_PITCH + MAX_VOICES];
        println!("voice pitches {pitches:.1?} (± {cents:.1})");
        assert!(pitches.iter().all(|p| p.abs() <= cents + 0.5));
        assert!(
            pitches.iter().any(|&p| p > 0.5 * cents) && pitches.iter().any(|&p| p < -0.5 * cents)
        );
        hyper.reset();
        assert_eq!(hyper.meter(), 0.0);
        let (l, r) = stereo(&mut hyper, &[0.0; 4800], &[0.0; 4800]);
        assert!(l.iter().chain(&r).all(|&s| s == 0.0));
    }
}
