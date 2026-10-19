//! Reverb: three algorithms behind one set of knobs, chosen by Type, with
//! pre-delay, damping of the highs as the tail decays, a low cut, a slow
//! modulation of the tail, stereo width and a dry/wet mix.
//!
//! **Hall** (the default, and what every project from before the types
//! has) is a feedback delay network. The input goes through the pre-delay
//! and, per channel, four allpass diffusers that smear each sound into a
//! dense burst. That feeds sixteen delay lines of mutually unrelated
//! lengths, whose outputs are mixed by a 16×16 Hadamard matrix
//! (orthogonal, so it neither adds nor loses energy, and every line reaches
//! every other at once) and fed back. Half the lines' read positions drift
//! slowly on their own LFOs, which keeps the modes from ringing
//! metallically; reads are cubic-interpolated, which is exact at the other
//! lines' whole-sample lengths, so their highs pass untouched.
//!
//! Each line loses exactly the share of its level that, over its own
//! length, makes the network fall by 60 dB in `decay` seconds (Jot's
//! method). That loss is a second-order shelf (an SVF, as everywhere here)
//! with its corner at `damping`: below it the decay is as set, above it the
//! tail dies four times as fast. Size scales every length; the left and
//! right outputs tap the lines with orthogonal sign patterns, so they are
//! uncorrelated, and width blends them towards their middle.
//!
//! **Plate** is Jon Dattorro's plate reverb ("Effect Design, Part 1:
//! Reverberator and Other Filters", JAES 1997), after the Lexicon and EMT
//! plates people love on vocals and drums: the input through four
//! diffusers into a tank of two cross-coupled halves, each a modulated
//! all-pass, a delay, a damping low-pass, a second all-pass and a second
//! delay, the output a sum of taps from inside the tank. Bright and dense
//! from the first moment, with no early echoes to hear. Its lengths are the
//! paper's (at 29761 Hz), scaled by the sample rate and by Size; its decay
//! is set so the tank loses 60 dB in `decay` seconds; Damping is the tank's
//! low-pass.
//!
//! **Room** is a small room's sound: early reflections, a dozen taps of the
//! pre-delayed input spread across the stereo field and growing quieter
//! (Size spreads them from a booth's 15 ms to a hall's 50), then a network
//! like the hall's at [`ROOM_SCALE`] of its size for the tail.
//!
//! **Modulation** (Serum's Spin, Vital's chorus) sets how far the drifting
//! read positions move, from still to four times the hall's original
//! 0.3 ms (25% is the original), and how fast (its rate scales every
//! line's own). Switching the type crossfades over [`TYPE_FADE_MS`]; the
//! type faded out finishes ringing silently and is cleared when it's
//! silent. Every float glides, at the knob's pace or within a block for a
//! modulator (see "Modulation" in `lib.rs`); lengths never change faster
//! than [`MAX_READ_SLOPE`] samples a sample, so a fast size bends the
//! tail's pitch a little but never wildly.

use std::f32::consts::TAU;

use instruments::ParamDescriptor;

use crate::line::{Ring, Tap};
use crate::meter::PeakMeter;
use crate::{Context, Effect, EffectDescriptor, MAX_METERS, Values, choice, float};
use dsp::flush;
use dsp::svf::{BUTTERWORTH_Q, Coefs, Shape, SvfState, prewarp};
use dsp::{MAX_BLOCK, Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.reverb",
    name: "Reverb",
    description: "Algorithmic reverb: hall, plate or room",
    category: crate::Category::Space,
    params: PARAMS,
    meters: &[
        "Wet peak (linear, 1 = 0 dBFS)",
        "Hall heard (0 to 1)",
        "Plate heard (0 to 1)",
        "Room heard (0 to 1)",
    ],
    create: || Box::new(Reverb::new()),
    ..EffectDescriptor::BASE
};

/// The types, in the order of the `type` parameter's labels.
pub const HALL: usize = 0;
pub const PLATE: usize = 1;
pub const ROOM: usize = 2;
const TYPES: usize = 3;
pub const TYPE_LABELS: &[&str] = &["Hall", "Plate", "Room"];

const PARAMS: &[ParamDescriptor] = &[
    float("size", "Size", 0.0, 100.0, 60.0, "%", false),
    float("decay", "Decay", 0.1, 20.0, 2.0, "s", true),
    float("predelay", "Pre-delay", 0.0, 200.0, 10.0, "ms", false),
    float("damping", "Damping", 500.0, 20_000.0, 6000.0, "Hz", true),
    float("low_cut", "Low cut", 20.0, 1000.0, 100.0, "Hz", true),
    float("width", "Width", 0.0, 100.0, 100.0, "%", false),
    float("mix", "Mix", 0.0, 100.0, 25.0, "%", false),
    choice("type", "Type", TYPE_LABELS, HALL),
    float(
        "mod_depth",
        "Modulation",
        0.0,
        100.0,
        MOD_DEPTH_DEFAULT,
        "%",
        false,
    ),
    float(
        "mod_rate",
        "Modulation rate",
        0.05,
        5.0,
        MOD_RATE_DEFAULT,
        "Hz",
        true,
    ),
];

const COUNT: usize = 10;

const SIZE: usize = 0;
const DECAY: usize = 1;
const PREDELAY: usize = 2;
const DAMPING: usize = 3;
const LOW_CUT: usize = 4;
const WIDTH: usize = 5;
const MIX: usize = 6;
pub const TYPE: usize = 7;
const MOD_DEPTH: usize = 8;
const MOD_RATE: usize = 9;

/// Meter slots, in [`DESCRIPTOR`]'s `meters` order.
pub const METER_WET: usize = 0;
pub const METER_TYPES: usize = 1;

/// Modulation's defaults: what the hall always had. The depth scales every
/// line's swing from [`MOD_DEPTH_MS`], the rate every line's rate from
/// [`MOD_RATES_HZ`], both as a share of these.
const MOD_DEPTH_DEFAULT: f32 = 25.0;
const MOD_RATE_DEFAULT: f32 = 0.35;
/// The most the depth swings the lines, as a multiple of the default's.
const MOD_DEPTH_MOST: f32 = 100.0 / MOD_DEPTH_DEFAULT;

/// Delay lines in each network.
const LINES: usize = 16;

/// Line lengths at full size, in ms: spread over two octaves, in no simple
/// ratio to each other so their echoes rarely coincide.
const LENGTHS_MS: [f32; LINES] = [
    23.3, 59.3, 31.3, 77.3, 41.3, 25.9, 64.7, 34.1, 93.1, 45.1, 28.7, 70.1, 49.7, 37.9, 84.7, 53.9,
];

/// At size 0 the lines are this fraction of their full length; in between
/// they scale exponentially, so each step of the knob sounds alike.
const SMALLEST: f32 = 0.12;
/// The room's network is this share of the hall's at the same size.
pub const ROOM_SCALE: f32 = 0.3;

/// How far each LFO moves its line's read position, either way, in ms, at
/// the default modulation; zero for the lines that stay still.
const MOD_DEPTH_MS: [f32; LINES] = [
    0.3, 0.0, 0.3, 0.0, 0.3, 0.0, 0.3, 0.0, 0.3, 0.0, 0.3, 0.0, 0.3, 0.0, 0.3, 0.0,
];
const MOD_RATES_HZ: [f32; LINES] = [
    0.31, 0.0, 0.17, 0.0, 0.43, 0.0, 0.23, 0.0, 0.37, 0.0, 0.13, 0.0, 0.47, 0.0, 0.83, 0.0,
];

/// Above the damping frequency the tail takes this fraction of `decay` to
/// fall by 60 dB.
const HIGH_DECAY_RATIO: f32 = 0.25;

/// The input diffusers' lengths in ms, per channel, and their gains.
const DIFFUSER_MS: [[f32; 4]; 2] = [[4.771, 3.595, 12.73, 9.307], [4.513, 3.793, 11.79, 8.931]];
const DIFFUSER_GAINS: [f32; 4] = [0.75, 0.75, 0.625, 0.625];

/// Signs with which the left input feeds each line.
const FEED_L: [f32; LINES] = [
    1.0, -1.0, 1.0, 1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, -1.0, -1.0, -1.0, 1.0,
];
/// Signs with which each line feeds the left output.
const OUT_L: [f32; LINES] = [
    1.0, 1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0, -1.0, 1.0, -1.0, -1.0,
];

/// `FEED_L` with every other sign flipped: orthogonal to it.
const fn feed_r() -> [f32; LINES] {
    let mut signs = FEED_L;
    let mut i = 1;
    while i < LINES {
        signs[i] = -signs[i];
        i += 2;
    }
    signs
}
const FEED_R: [f32; LINES] = feed_r();

/// `OUT_L` with the second half's signs flipped: orthogonal to it.
const fn out_r() -> [f32; LINES] {
    let mut signs = OUT_L;
    let mut i = LINES / 2;
    while i < LINES {
        signs[i] = -signs[i];
        i += 1;
    }
    signs
}
const OUT_R: [f32; LINES] = out_r();

/// How strongly the input feeds the lines.
const FEED_GAIN: f32 = 0.25;
/// Wet output level at full size; smaller sizes are turned down by the
/// square root of their scale, which keeps the loudness about the same.
const WET_LEVEL: f32 = 1.2;

/// Dattorro's plate, in samples at the rate it was designed for: the input
/// diffusers and their gains; per half of the tank, the modulated
/// all-pass, the first delay, the second all-pass and the second delay;
/// the two all-passes' gains (the first's sign as in the paper's figure);
/// how far the modulation swings the first (at the default modulation)
/// and how fast; and the output taps, (half, element, position, sign),
/// for the left side and the right.
const PLATE_RATE: f32 = 29_761.0;
const PLATE_DIFFUSERS: [f32; 4] = [142.0, 107.0, 379.0, 277.0];
const PLATE_DIFFUSION: [f32; 4] = [0.75, 0.75, 0.625, 0.625];
const PLATE_TANK: [[f32; 4]; 2] = [
    [672.0, 4453.0, 1800.0, 3720.0],
    [908.0, 4217.0, 2656.0, 3163.0],
];
const PLATE_DECAY_DIFFUSION: [f32; 2] = [-0.7, 0.5];
const PLATE_EXCURSION: f32 = 16.0;
const PLATE_LFO_HZ: f32 = 1.0;
const PLATE_TAPS: [[(usize, usize, f32, f32); 7]; 2] = [
    [
        (1, 1, 266.0, 1.0),
        (1, 1, 2974.0, 1.0),
        (1, 2, 1913.0, -1.0),
        (1, 3, 1996.0, 1.0),
        (0, 1, 1990.0, -1.0),
        (0, 2, 187.0, -1.0),
        (0, 3, 1066.0, -1.0),
    ],
    [
        (0, 1, 353.0, 1.0),
        (0, 1, 3627.0, 1.0),
        (0, 2, 1228.0, -1.0),
        (0, 3, 2673.0, 1.0),
        (1, 1, 2111.0, -1.0),
        (1, 2, 335.0, -1.0),
        (1, 3, 121.0, -1.0),
    ],
];
/// The plate's scale at size 0 (1 at full size, the paper's), and its
/// output level, about the hall's loudness.
const PLATE_SMALLEST: f32 = 0.3;
const PLATE_LEVEL: f32 = 0.3;

/// The room's early reflections at full size, in ms, where each sits
/// (-1 left to 1 right), how much quieter each is than the last, and how
/// loud they are against the tail; at size 0 they're [`ROOM_SMALLEST`]
/// of these times.
const EARLY_MS: [f32; 12] = [
    4.3, 7.1, 11.2, 13.9, 17.5, 21.0, 24.7, 29.3, 33.1, 38.6, 43.2, 49.8,
];
const EARLY_PANS: [f32; 12] = [
    -0.7, 0.6, -0.3, 0.9, -0.9, 0.2, 0.5, -0.5, 0.8, -0.1, -0.6, 0.4,
];
const EARLY_FALL: f32 = 0.88;
const EARLY_LEVEL: f32 = 0.5;
const ROOM_SMALLEST: f32 = 0.3;

const MAX_PREDELAY_MS: f32 = 200.0;
/// Parameters that reshape the network follow their glide every this many
/// samples; read positions move smoothly in between.
const CHUNK: usize = 32;
/// Size glides slowly: changing it bends the pitch of the tail.
const SIZE_GLIDE_MS: f32 = 250.0;
const DECAY_GLIDE_MS: f32 = 100.0;
const FILTER_GLIDE_MS: f32 = 30.0;
/// Width, mix and modulation.
const SMOOTH_MS: f32 = 20.0;
const PREDELAY_FADE_MS: f32 = 20.0;
/// How long switching the type crossfades.
pub const TYPE_FADE_MS: f32 = 40.0;
/// The fastest a line's read position, or a modulated pre-delay, moves:
/// in samples per sample, the share by which the tail's pitch bends while
/// the size glides.
const MAX_READ_SLOPE: f32 = 0.25;

/// Schroeder allpass: `w = x + g·w[n-D]`, `y = w[n-D] - g·w`.
#[derive(Debug, Clone, Default)]
struct Allpass {
    buffer: Vec<f32>,
    position: usize,
}

impl Allpass {
    fn allocate(&mut self, len: usize) {
        self.buffer = vec![0.0; len.max(1)];
        self.position = 0;
    }

    #[inline]
    fn tick(&mut self, x: f32, gain: f32) -> f32 {
        let delayed = self.buffer[self.position];
        let w = x + gain * delayed;
        self.buffer[self.position] = flush(w);
        self.position += 1;
        if self.position == self.buffer.len() {
            self.position = 0;
        }
        delayed - gain * w
    }
}

/// The same allpass on a [`Ring`], `delay` samples long (between samples
/// if need be), for the plate, whose lengths move.
#[inline]
fn ring_allpass(ring: &mut Ring, x: f32, gain: f32, delay: f32) -> f32 {
    let delayed = ring.delayed(delay);
    let w = x + gain * delayed;
    ring.push(flush(w));
    delayed - gain * w
}

/// `x` into a delay `delay` samples long on `ring`; what comes out.
#[inline]
fn ring_delay(ring: &mut Ring, x: f32, delay: f32) -> f32 {
    let out = ring.delayed(delay);
    ring.push(flush(x));
    out
}

/// In-place fast Walsh-Hadamard transform: the 16×16 Hadamard matrix times
/// `x`, unnormalised (each output is 4 times too loud).
#[inline]
fn hadamard(x: &mut [f32; LINES]) {
    let mut half = 1;
    while half < LINES {
        let mut start = 0;
        while start < LINES {
            for i in start..start + half {
                let (a, b) = (x[i], x[i + half]);
                x[i] = a + b;
                x[i + half] = a - b;
            }
            start += 2 * half;
        }
        half *= 2;
    }
}

/// The scale of the hall's line lengths at `size` percent.
fn scale(size: f32) -> f32 {
    SMALLEST.powf(1.0 - size / 100.0)
}

/// What the networks are tuned to, worked out once a chunk.
#[derive(Debug, Clone, Copy)]
struct Tuning {
    sample_rate: f32,
    /// The hall's scale at the size now (the room's is less).
    scale: f32,
    decay: f32,
    damping: f32,
    /// Modulation depth and rate, as multiples of the default's.
    depth: f32,
    rate: f32,
}

/// One feedback delay network: the hall's, or the room's tail.
#[derive(Debug, Clone, Default)]
struct Network {
    /// The lines one after another, each `line_len` samples (a power of two).
    lines: Vec<f32>,
    line_len: usize,
    /// Where every line writes next.
    write: usize,
    /// Each line's length in samples, without modulation.
    length: [f32; LINES],
    /// Where each line is read, in samples ago, and its change per sample.
    delay: [f32; LINES],
    delay_step: [f32; LINES],
    lfo_phase: [f32; LINES],
    /// Each line's loss: a shelving SVF whose tuning (`absorb_a*`) all lines
    /// share and whose output mix (`absorb_m*`, including the Hadamard
    /// matrix's normalisation) is the line's own; and its state.
    absorb_a1: f32,
    absorb_a2: f32,
    absorb_a3: f32,
    absorb_m0: [f32; LINES],
    absorb_m1: [f32; LINES],
    absorb_m2: [f32; LINES],
    absorb_s1: [f32; LINES],
    absorb_s2: [f32; LINES],
    wet_gain: f32,
}

impl Network {
    /// Room for the longest line, its modulation and the interpolator.
    fn allocate(&mut self, sample_rate: f32) {
        let most = |values: &[f32]| values.iter().fold(0.0f32, |a, &b| a.max(b));
        let longest = most(&LENGTHS_MS) + MOD_DEPTH_MOST * most(&MOD_DEPTH_MS);
        let needed = (longest * 0.001 * sample_rate).ceil() as usize + 4;
        self.line_len = needed.next_power_of_two();
        self.lines = vec![0.0; LINES * self.line_len];
        self.write = 0;
    }

    /// Silences it. Not cheap (it fills every line): for a reset, or once
    /// when it falls silent unheard.
    fn clear(&mut self) {
        self.lines.fill(0.0);
        self.absorb_s1 = [0.0; LINES];
        self.absorb_s2 = [0.0; LINES];
        // From the same LFO phases every time, so renders repeat exactly.
        self.lfo_phase = std::array::from_fn(|i| (i as f32 * 0.618_034).fract());
    }

    /// Works out the lines' lengths and losses, at `scale` of their full
    /// lengths.
    fn update(&mut self, scale: f32, t: &Tuning) {
        let samples_per_ms = 0.001 * t.sample_rate;
        // 10^(-3·length/(decay·rate)) loses 60 dB per `decay` seconds.
        let per_sample = -3.0 * std::f32::consts::LOG2_10 / (t.decay * t.sample_rate);

        // A high shelf from 1 down to `A²`, its corner (poles) at the damping
        // frequency and Butterworth damping: |H|² = (1 + A⁴w⁴) / (1 + w⁴).
        // It falls monotonically, so the loop never gains, and steeply, so
        // two octaves below the corner the decay is within about 1% of the
        // setting.
        let g = prewarp(t.damping, t.sample_rate);
        let k = std::f32::consts::SQRT_2;
        self.absorb_a1 = 1.0 / (1.0 + g * (g + k));
        self.absorb_a2 = g * self.absorb_a1;
        self.absorb_a3 = g * self.absorb_a2;
        for line in 0..LINES {
            // Whole samples, so the still lines read without interpolating;
            // and far enough back for the interpolator, LFO included.
            let shortest = 4.0 + MOD_DEPTH_MOST * MOD_DEPTH_MS[line] * samples_per_ms;
            let length = (LENGTHS_MS[line] * scale * samples_per_ms)
                .round()
                .max(shortest.ceil());
            self.length[line] = length;
            let low = (per_sample * length).exp2();
            let high = (per_sample * length / HIGH_DECAY_RATIO).exp2();
            let a = (high / low).sqrt();
            // The Hadamard matrix's normalisation, folded in.
            let gain = 0.25 * low;
            self.absorb_m0[line] = gain * a * a;
            self.absorb_m1[line] = gain * k * (1.0 - a) * a;
            self.absorb_m2[line] = gain * (1.0 - a * a);
        }
        self.wet_gain = WET_LEVEL * scale.sqrt();
    }

    /// Where `line` should be read now, in samples ago.
    fn read_position(&self, line: usize, t: &Tuning) -> f32 {
        let depth = t.depth * MOD_DEPTH_MS[line] * 0.001 * t.sample_rate;
        self.length[line] + depth * (TAU * self.lfo_phase[line]).sin()
    }

    /// Advances the LFOs by a chunk and sets each read position gliding to
    /// where it should be at the end of it.
    fn update_read_positions(&mut self, t: &Tuning) {
        let chunk_seconds = CHUNK as f32 / t.sample_rate;
        for (line, rate) in MOD_RATES_HZ.iter().enumerate() {
            self.lfo_phase[line] = (self.lfo_phase[line] + rate * t.rate * chunk_seconds).fract();
            let target = self.read_position(line, t);
            let distance = target - self.delay[line];
            if distance.abs() < 1e-3 {
                // Arrived: settle exactly, so a still line reads whole samples.
                self.delay[line] = target;
                self.delay_step[line] = 0.0;
            } else {
                self.delay_step[line] =
                    (distance / CHUNK as f32).clamp(-MAX_READ_SLOPE, MAX_READ_SLOPE);
            }
        }
    }

    /// Ends a chunk: the loss filters' states end a decay in exact zeros
    /// rather than denormals (and never carry a NaN on).
    fn flush_states(&mut self) {
        for state in self.absorb_s1.iter_mut().chain(&mut self.absorb_s2) {
            *state = flush(*state);
        }
    }

    /// Puts every read position where it should be at once.
    fn place(&mut self, t: &Tuning) {
        self.delay = std::array::from_fn(|line| self.read_position(line, t));
        self.delay_step = [0.0; LINES];
    }

    /// One sample in (both channels, diffused) and the wet sound out.
    ///
    /// Written as passes over arrays of the 16 lines rather than a loop
    /// doing everything for one line at a time: the lines are independent
    /// until the Hadamard mix, so the compiler runs each pass four lines
    /// at a time in SIMD. Only the reads from the lines (at 16 different
    /// delays) and the writes back are one line at a time.
    #[inline]
    fn tick(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        let line_len = self.line_len;
        let mask = line_len - 1;
        let write = self.write;
        // Read every line where its LFO has it now: the four samples
        // around the point `whole` samples ago, `t` of the way from the
        // first pair's second to the next.
        let (mut x0, mut x1, mut x2, mut x3) = (
            [0.0f32; LINES],
            [0.0f32; LINES],
            [0.0f32; LINES],
            [0.0f32; LINES],
        );
        let mut t = [0.0f32; LINES];
        for (i, line) in self.lines.chunks_exact(line_len).enumerate() {
            // Sliced to the mask's reach, so the compiler can see every
            // masked index is in bounds and check none of them.
            let line = &line[..=mask];
            self.delay[i] += self.delay_step[i];
            let delay = self.delay[i];
            // The ceiling, without the library call: the delay is
            // positive, so truncation is its floor.
            let floor = delay as usize;
            let whole = floor + usize::from(floor as f32 != delay);
            t[i] = whole as f32 - delay;
            let index = write + line_len - whole;
            x0[i] = line[(index - 1) & mask];
            x1[i] = line[index & mask];
            x2[i] = line[(index + 1) & mask];
            x3[i] = line[(index + 2) & mask];
        }
        let mut y = [0.0f32; LINES];
        for i in 0..LINES {
            y[i] = dsp::catmull_rom(x0[i], x1[i], x2[i], x3[i], t[i]);
        }

        // Each line's loss over its length. The states are flushed once a
        // chunk (`flush_states`), not here, so this is straight arithmetic.
        let (a1, a2, a3) = (self.absorb_a1, self.absorb_a2, self.absorb_a3);
        for (i, y) in y.iter_mut().enumerate() {
            let x = *y;
            let (s1, s2) = (self.absorb_s1[i], self.absorb_s2[i]);
            let v3 = x - s2;
            let v1 = a1 * s1 + a2 * v3;
            let v2 = s2 + a2 * s1 + a3 * v3;
            self.absorb_s1[i] = 2.0 * v1 - s1;
            self.absorb_s2[i] = 2.0 * v2 - s2;
            *y = self.absorb_m0[i] * x + self.absorb_m1[i] * v1 + self.absorb_m2[i] * v2;
        }

        let (mut wet_l, mut wet_r) = (0.0, 0.0);
        for i in 0..LINES {
            wet_l += OUT_L[i] * y[i];
            wet_r += OUT_R[i] * y[i];
        }

        hadamard(&mut y);
        let mut fed = [0.0f32; LINES];
        for i in 0..LINES {
            fed[i] = flush(y[i] + FEED_GAIN * (FEED_L[i] * in_l + FEED_R[i] * in_r));
        }
        for (line, fed) in self.lines.chunks_exact_mut(line_len).zip(fed) {
            line[write] = fed;
        }
        self.write = (write + 1) & mask;
        (wet_l * self.wet_gain, wet_r * self.wet_gain)
    }
}

/// Dattorro's plate.
#[derive(Debug, Clone, Default)]
struct Plate {
    diffusers: [Ring; 4],
    /// Per half: the modulated all-pass, the first delay, the second
    /// all-pass and the second delay.
    tank: [[Ring; 4]; 2],
    /// The scale the tank is read at now (samples per sample of the
    /// paper's rate), gliding to `scale` so slowly that neither half's
    /// whole loop changes by more than [`MAX_READ_SLOPE`] a sample; and
    /// whether it has been placed since a clear.
    scale_now: f32,
    placed: bool,
    damp: [f32; 2],
    /// Each half's output last sample, which feeds the other.
    back: [f32; 2],
    lfo_phase: f32,
    /// Samples per sample of the paper's rate, and at the size now.
    rate_ratio: f32,
    scale: f32,
    /// The gain applied twice a half-loop, the damping one-pole's pole, the
    /// modulation's swing (samples) and its step per sample.
    decay: f32,
    pole: f32,
    excursion: f32,
    lfo_step: f32,
}

impl Plate {
    fn allocate(&mut self, sample_rate: f32) {
        let ratio = sample_rate / PLATE_RATE;
        for (ring, len) in self.diffusers.iter_mut().zip(PLATE_DIFFUSERS) {
            ring.allocate((len * ratio).ceil() as usize + 4);
        }
        let swing = MOD_DEPTH_MOST * PLATE_EXCURSION;
        for (rings, lengths) in self.tank.iter_mut().zip(PLATE_TANK) {
            for (element, (ring, len)) in rings.iter_mut().zip(lengths).enumerate() {
                let len = len + if element == 0 { swing } else { 0.0 };
                ring.allocate((len * ratio).ceil() as usize + 4);
            }
        }
    }

    fn clear(&mut self) {
        for ring in self
            .diffusers
            .iter_mut()
            .chain(self.tank.iter_mut().flatten())
        {
            ring.clear();
        }
        self.placed = false;
        self.damp = [0.0; 2];
        self.back = [0.0; 2];
        self.lfo_phase = 0.0;
    }

    fn update(&mut self, size: f32, t: &Tuning) {
        let ratio = t.sample_rate / PLATE_RATE;
        self.rate_ratio = ratio;
        self.scale = ratio * PLATE_SMALLEST.powf(1.0 - size / 100.0);
        // Two gains a half-loop, so each takes half the loss that loses
        // 60 dB in `decay` seconds over that half.
        let half_loop: f32 = PLATE_TANK.iter().flatten().sum::<f32>() / 2.0 * self.scale;
        self.decay = 10f32.powf(-1.5 * half_loop / (t.decay * t.sample_rate));
        self.pole = (-TAU * t.damping / t.sample_rate).exp();
        self.excursion = t.depth * PLATE_EXCURSION * ratio;
        self.lfo_step = t.rate * PLATE_LFO_HZ / t.sample_rate;
    }

    /// One sample in (both channels) and the wet sound out.
    #[inline]
    fn tick(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        // The tank's scale glides: every element's length moves with it,
        // so a half's loop, four lengths long, bends the pitch no more
        // than one of the hall's lines.
        let loop_len = PLATE_TANK[0].iter().sum::<f32>();
        let most = MAX_READ_SLOPE / loop_len;
        self.scale_now = if self.placed {
            self.scale_now + (self.scale - self.scale_now).clamp(-most, most)
        } else {
            self.scale
        };
        let ratio = self.scale_now;
        // The input diffusers keep their lengths whatever the size.
        let mut x = 0.5 * (in_l + in_r);
        for ((ring, len), gain) in self
            .diffusers
            .iter_mut()
            .zip(PLATE_DIFFUSERS)
            .zip(PLATE_DIFFUSION)
        {
            x = ring_allpass(ring, x, gain, (len * self.rate_ratio).max(2.0));
        }
        self.lfo_phase += self.lfo_step;
        if self.lfo_phase >= 1.0 {
            self.lfo_phase -= 1.0;
        }
        let back = self.back;
        for half in 0..2 {
            // Where each element should be read now, the first swinging
            // with the LFO (the halves a quarter cycle apart).
            let swing = self.excursion * (TAU * (self.lfo_phase + 0.25 * half as f32)).sin();
            let lengths: [f32; 4] = std::array::from_fn(|element| {
                let swing = if element == 0 { swing } else { 0.0 };
                let ring_len = self.tank[half][element].len() as f32 - 3.0;
                (PLATE_TANK[half][element] * ratio + swing).clamp(2.0, ring_len)
            });
            let [first, delay_a, second, delay_b] = &mut self.tank[half];
            let input = x + back[1 - half];
            let a = ring_allpass(first, input, PLATE_DECAY_DIFFUSION[0], lengths[0]);
            let b = ring_delay(delay_a, a, lengths[1]);
            self.damp[half] = flush(b + self.pole * (self.damp[half] - b));
            let c = self.damp[half] * self.decay;
            let d = ring_allpass(second, c, PLATE_DECAY_DIFFUSION[1], lengths[2]);
            self.back[half] = ring_delay(delay_b, d, lengths[3]) * self.decay;
        }
        self.placed = true;
        let mut out = [0.0f32; 2];
        for (side, taps) in PLATE_TAPS.iter().enumerate() {
            for &(half, element, position, sign) in taps {
                let ring = &self.tank[half][element];
                let age = (position * ratio).min(ring.len() as f32 - 4.0);
                out[side] += sign * ring.get_between(age);
            }
        }
        (PLATE_LEVEL * out[0], PLATE_LEVEL * out[1])
    }
}

/// The room's early reflections: taps of the pre-delayed input.
#[derive(Debug, Clone, Default)]
struct Early {
    line: Ring,
    /// Where each tap reads now (samples), gliding to the size.
    taps: [f32; 12],
    placed: bool,
    /// The taps' times at the size now, in samples.
    wanted: [f32; 12],
}

impl Early {
    fn allocate(&mut self, sample_rate: f32) {
        let longest = EARLY_MS[EARLY_MS.len() - 1] * 0.001 * sample_rate;
        self.line.allocate(longest.ceil() as usize + 4);
    }

    fn clear(&mut self) {
        self.line.clear();
        self.placed = false;
    }

    fn update(&mut self, size: f32, sample_rate: f32) {
        let scale = ROOM_SMALLEST.powf(1.0 - size / 100.0);
        self.wanted = EARLY_MS.map(|ms| (ms * scale * 0.001 * sample_rate).max(2.0));
    }

    #[inline]
    fn tick(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        self.line.push(flush(0.5 * (in_l + in_r)));
        let (mut left, mut right) = (0.0, 0.0);
        let mut level = 1.0;
        for (tap, (&wanted, &pan)) in self
            .taps
            .iter_mut()
            .zip(self.wanted.iter().zip(&EARLY_PANS))
        {
            *tap = if self.placed {
                *tap + (wanted - *tap).clamp(-MAX_READ_SLOPE, MAX_READ_SLOPE)
            } else {
                wanted
            };
            let x = level * self.line.get_between(*tap);
            left += (0.5 + 0.5 * pan) * x;
            right += (0.5 - 0.5 * pan) * x;
            level *= EARLY_FALL;
        }
        self.placed = true;
        (EARLY_LEVEL * left, EARLY_LEVEL * right)
    }
}

pub struct Reverb {
    values: Values<COUNT>,
    sample_rate: f32,
    predelay: [Ring; 2],
    predelay_tap: Tap,
    diffusers: [[Allpass; 4]; 2],
    /// The hall's network and the room's, the plate, the room's early
    /// reflections.
    hall: Network,
    room: Network,
    plate: Plate,
    early: Early,
    /// How much of each type is heard: 1 for the one chosen, 0 for the
    /// others, in between while switching; and whether each has been
    /// cleared since it was last heard.
    types: [Smoothed; TYPES],
    cleared: [bool; TYPES],
    size: Smoothed,
    log_decay: Smoothed,
    log_damping: Smoothed,
    log_low_cut: Smoothed,
    width: Smoothed,
    mix: Smoothed,
    /// Modulation depth and rate, as multiples of the default's.
    mod_depth: Smoothed,
    mod_rate: Smoothed,
    low_cut: Coefs,
    low_cut_state: [SvfState; 2],
    /// Samples until the networks next follow their parameters.
    countdown: usize,
    wet_peak: PeakMeter,
}

impl Reverb {
    pub fn new() -> Reverb {
        let mut reverb = Reverb {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            predelay: [Ring::default(), Ring::default()],
            predelay_tap: Tap::new(0.0),
            diffusers: Default::default(),
            hall: Network::default(),
            room: Network::default(),
            plate: Plate::default(),
            early: Early::default(),
            types: [
                Smoothed::new(1.0, TYPE_FADE_MS),
                Smoothed::new(0.0, TYPE_FADE_MS),
                Smoothed::new(0.0, TYPE_FADE_MS),
            ],
            cleared: [false; TYPES],
            size: Smoothed::new(0.0, SIZE_GLIDE_MS),
            log_decay: Smoothed::new(0.0, DECAY_GLIDE_MS),
            log_damping: Smoothed::new(0.0, FILTER_GLIDE_MS),
            log_low_cut: Smoothed::new(0.0, FILTER_GLIDE_MS),
            width: Smoothed::new(0.0, SMOOTH_MS),
            mix: Smoothed::new(0.0, SMOOTH_MS),
            mod_depth: Smoothed::new(1.0, SMOOTH_MS),
            mod_rate: Smoothed::new(1.0, SMOOTH_MS),
            low_cut: Coefs::IDENTITY,
            low_cut_state: [SvfState::default(); 2],
            countdown: 0,
            wet_peak: PeakMeter::default(),
        };
        for index in 0..COUNT {
            reverb.apply(index, Pace::Knob);
        }
        reverb
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`. A knob's pre-delay crossfades; a modulator's glides. The
    /// type always crossfades.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            SIZE => self.size.move_to(value, pace),
            DECAY => self.log_decay.move_to(value.log2(), pace),
            PREDELAY if pace == Pace::Modulation => {
                self.predelay_tap
                    .glide_to(self.predelay_age(false), MAX_BLOCK);
            }
            PREDELAY => self.predelay_tap.set_target(self.predelay_age(true)),
            DAMPING => self.log_damping.move_to(value.log2(), pace),
            LOW_CUT => self.log_low_cut.move_to(value.log2(), pace),
            WIDTH => self.width.move_to(value / 100.0, pace),
            MIX => self.mix.move_to(value / 100.0, pace),
            TYPE => {
                for (kind, weight) in self.types.iter_mut().enumerate() {
                    weight.set_target(if kind == value as usize { 1.0 } else { 0.0 });
                }
            }
            MOD_DEPTH => self.mod_depth.move_to(value / MOD_DEPTH_DEFAULT, pace),
            MOD_RATE => self.mod_rate.move_to(value / MOD_RATE_DEFAULT, pace),
            _ => {}
        }
    }

    /// The pre-delay in samples, whole ones if `whole`.
    fn predelay_age(&self, whole: bool) -> f32 {
        let samples = self.values[PREDELAY] * 0.001 * self.sample_rate;
        let samples = if whole { samples.round() } else { samples };
        samples.min(self.predelay[0].len().saturating_sub(3) as f32)
    }

    fn tuning(&self) -> Tuning {
        Tuning {
            sample_rate: self.sample_rate,
            scale: scale(self.size.current()),
            decay: self.log_decay.current().exp2(),
            damping: self.log_damping.current().exp2(),
            depth: self.mod_depth.current(),
            rate: self.mod_rate.current(),
        }
    }

    /// Whether type `kind` is heard, or fading.
    fn heard(&self, kind: usize) -> bool {
        self.types[kind].current() > 0.0 || self.types[kind].is_smoothing()
    }

    /// Works out every heard type's lengths and losses from the smoothed
    /// parameters.
    fn update_networks(&mut self) {
        let t = self.tuning();
        if self.heard(HALL) {
            self.hall.update(t.scale, &t);
        }
        if self.heard(ROOM) {
            self.room.update(ROOM_SCALE * t.scale, &t);
            self.early.update(self.size.current(), self.sample_rate);
        }
        if self.heard(PLATE) {
            self.plate.update(self.size.current(), &t);
        }
    }

    fn update_low_cut(&mut self) {
        let hz = self.log_low_cut.current().exp2();
        self.low_cut = Coefs::new(&Shape::highpass(hz, BUTTERWORTH_Q, self.sample_rate));
    }

    /// Jumps every parameter to its value, leaving what rings alone.
    fn snap(&mut self) {
        let smoothers = [
            &mut self.size,
            &mut self.log_decay,
            &mut self.log_damping,
            &mut self.log_low_cut,
            &mut self.width,
            &mut self.mix,
            &mut self.mod_depth,
            &mut self.mod_rate,
        ];
        for smoothed in smoothers.into_iter().chain(&mut self.types) {
            smoothed.settle();
        }
        self.predelay_tap.jump(self.predelay_age(true));
        // Worked out for every type, so whichever is chosen starts tuned.
        let t = self.tuning();
        self.hall.update(t.scale, &t);
        self.room.update(ROOM_SCALE * t.scale, &t);
        self.early.update(self.size.current(), self.sample_rate);
        self.plate.update(self.size.current(), &t);
        self.update_low_cut();
        self.hall.place(&t);
        self.room.place(&t);
        self.countdown = 0;
        self.values.refresh();
    }

    /// Called every `CHUNK` samples.
    fn update(&mut self) {
        let moving = [
            &self.size,
            &self.log_decay,
            &self.log_damping,
            &self.mod_depth,
            &self.mod_rate,
        ]
        .iter()
        .any(|s| s.is_smoothing());
        if moving {
            for smoothed in [
                &mut self.size,
                &mut self.log_decay,
                &mut self.log_damping,
                &mut self.mod_depth,
                &mut self.mod_rate,
            ] {
                smoothed.tick();
            }
            self.update_networks();
        }
        if self.log_low_cut.is_smoothing() {
            self.log_low_cut.tick();
            self.update_low_cut();
        }
        let t = self.tuning();
        if self.heard(HALL) {
            self.hall.update_read_positions(&t);
        }
        if self.heard(ROOM) {
            self.room.update_read_positions(&t);
        }
        // A type faded out is cleared once, so it starts silent when it's
        // chosen again.
        for kind in 0..TYPES {
            if self.heard(kind) {
                self.cleared[kind] = false;
            } else if !self.cleared[kind] {
                self.cleared[kind] = true;
                match kind {
                    HALL => self.hall.clear(),
                    ROOM => {
                        self.room.clear();
                        self.early.clear();
                    }
                    _ => self.plate.clear(),
                }
                // Tuned for when it comes back.
                self.update_networks();
            }
        }
    }

    /// Runs `left` and `right` (one chunk or less) through the reverb.
    fn run(&mut self, left: &mut [f32], right: &mut [f32]) {
        let heard = [self.heard(HALL), self.heard(PLATE), self.heard(ROOM)];
        let (low_cut, mut wet_peak) = (self.low_cut, 0.0f32);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (dry_l, dry_r) = (*l, *r);
            let [predelay_l, predelay_r] = &mut self.predelay;
            predelay_l.push(dry_l);
            predelay_r.push(dry_r);
            let taps = self.predelay_tap.advance();
            let (in_l, in_r) = (taps.read(predelay_l), taps.read(predelay_r));
            let weights = [
                self.types[HALL].tick(),
                self.types[PLATE].tick(),
                self.types[ROOM].tick(),
            ];

            let (mut wet_l, mut wet_r) = (0.0, 0.0);
            if heard[HALL] || heard[ROOM] {
                let [diffusers_l, diffusers_r] = &mut self.diffusers;
                let (mut diffused_l, mut diffused_r) = (in_l, in_r);
                for (diffuser, &gain) in diffusers_l.iter_mut().zip(&DIFFUSER_GAINS) {
                    diffused_l = diffuser.tick(diffused_l, gain);
                }
                for (diffuser, &gain) in diffusers_r.iter_mut().zip(&DIFFUSER_GAINS) {
                    diffused_r = diffuser.tick(diffused_r, gain);
                }
                if heard[HALL] {
                    let (hall_l, hall_r) = self.hall.tick(diffused_l, diffused_r);
                    wet_l += weights[HALL] * hall_l;
                    wet_r += weights[HALL] * hall_r;
                }
                if heard[ROOM] {
                    let (early_l, early_r) = self.early.tick(in_l, in_r);
                    let (tail_l, tail_r) = self.room.tick(diffused_l, diffused_r);
                    wet_l += weights[ROOM] * (early_l + tail_l);
                    wet_r += weights[ROOM] * (early_r + tail_r);
                }
            }
            if heard[PLATE] {
                let (plate_l, plate_r) = self.plate.tick(in_l, in_r);
                wet_l += weights[PLATE] * plate_l;
                wet_r += weights[PLATE] * plate_r;
            }

            let [low_cut_l, low_cut_r] = &mut self.low_cut_state;
            let wet_l = low_cut_l.tick(&low_cut, wet_l);
            let wet_r = low_cut_r.tick(&low_cut, wet_r);
            let mid = 0.5 * (wet_l + wet_r);
            let side = 0.5 * (wet_l - wet_r) * self.width.tick();
            let mix = self.mix.tick();
            *l = dry_l * (1.0 - mix) + (mid + side) * mix;
            *r = dry_r * (1.0 - mix) + (mid - side) * mix;
            wet_peak = wet_peak.max(wet_l.abs()).max(wet_r.abs());
        }
        for state in &mut self.low_cut_state {
            state.flush();
        }
        self.hall.flush_states();
        self.room.flush_states();
        self.wet_peak.update(wet_peak, left.len());
    }
}

impl Default for Reverb {
    fn default() -> Self {
        Reverb::new()
    }
}

impl Effect for Reverb {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        let samples_per_ms = 0.001 * sample_rate;
        self.hall.allocate(sample_rate);
        self.room.allocate(sample_rate);
        self.plate.allocate(sample_rate);
        self.early.allocate(sample_rate);
        for ring in &mut self.predelay {
            ring.allocate((MAX_PREDELAY_MS * samples_per_ms).ceil() as usize + 4);
        }
        self.predelay_tap
            .set_fade_length((PREDELAY_FADE_MS * samples_per_ms) as usize);
        self.predelay_tap.set_max_slope(MAX_READ_SLOPE);
        for (diffusers, lengths) in self.diffusers.iter_mut().zip(&DIFFUSER_MS) {
            for (diffuser, &ms) in diffusers.iter_mut().zip(lengths) {
                diffuser.allocate((ms * samples_per_ms).round() as usize);
            }
        }
        for smoothed in [
            &mut self.size,
            &mut self.log_decay,
            &mut self.log_damping,
            &mut self.log_low_cut,
            &mut self.mod_depth,
            &mut self.mod_rate,
        ] {
            smoothed.prepare_every(sample_rate, CHUNK);
        }
        for smoothed in [&mut self.width, &mut self.mix]
            .into_iter()
            .chain(&mut self.types)
        {
            smoothed.prepare(sample_rate);
        }
        self.wet_peak.prepare(sample_rate);
        self.reset();
    }

    fn reset(&mut self) {
        self.hall.clear();
        self.room.clear();
        self.plate.clear();
        self.early.clear();
        self.cleared = [false; TYPES];
        for ring in &mut self.predelay {
            ring.clear();
        }
        for diffuser in self.diffusers.iter_mut().flatten() {
            diffuser.buffer.fill(0.0);
        }
        self.low_cut_state = [SvfState::default(); 2];
        self.wet_peak.reset();
        self.snap();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.values.played();
        if self.hall.lines.is_empty() {
            // Not prepared: pass the sound through.
            return;
        }
        let len = left.len().min(right.len());
        let mut start = 0;
        while start < len {
            if self.countdown == 0 {
                self.update();
                self.countdown = CHUNK;
            }
            let n = self.countdown.min(len - start);
            self.run(&mut left[start..start + n], &mut right[start..start + n]);
            self.countdown -= n;
            start += n;
        }
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn receive(&mut self, index: usize, value: f32, pace: Pace) {
        if self.values.set(index, value) {
            self.apply(index, pace);
            if self.values.is_fresh() {
                self.snap();
            }
        }
    }

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out[METER_WET] = self.wet_peak.level();
        for (kind, weight) in self.types.iter().enumerate() {
            out[METER_TYPES + kind] = weight.current();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, noise, peak, rms, run, set};

    /// Wet only, no pre-delay, nothing filtered, unless `settings` say otherwise.
    fn reverb(settings: &[(&str, f32)]) -> Reverb {
        let mut reverb = Reverb::new();
        reverb.prepare(RATE, 64);
        let defaults = [
            ("mix", 100.0),
            ("predelay", 0.0),
            ("low_cut", 20.0),
            ("damping", 20_000.0),
        ];
        for &(id, value) in defaults.iter().chain(settings) {
            set(&mut reverb, id, value);
        }
        reverb.reset();
        reverb
    }

    fn impulse_response(reverb: &mut Reverb, seconds: f32) -> (Vec<f32>, Vec<f32>) {
        let len = (seconds * RATE) as usize;
        let (mut left, mut right) = (vec![0.0; len], vec![0.0; len]);
        left[0] = 1.0;
        right[0] = 1.0;
        run(reverb, &mut left, &mut right, 64, &Context::DEFAULT);
        (left, right)
    }

    /// RT60 in seconds of the energy in `channels`: the slope of Schroeder's
    /// backward-integrated energy decay curve, fitted from -5 to -35 dB.
    fn rt60(channels: &[&[f32]]) -> f32 {
        let len = channels[0].len();
        let mut curve = vec![0.0f64; len];
        let mut sum = 0.0;
        for n in (0..len).rev() {
            sum += channels
                .iter()
                .map(|c| f64::from(c[n]).powi(2))
                .sum::<f64>();
            curve[n] = sum;
        }
        let total = curve[0];
        let points: Vec<(f64, f64)> = curve
            .iter()
            .enumerate()
            .map(|(n, &e)| (n as f64 / f64::from(RATE), 10.0 * (e / total).log10()))
            .filter(|&(_, db)| (-35.0..=-5.0).contains(&db))
            .collect();
        let count = points.len() as f64;
        let (mean_t, mean_db) = points.iter().fold((0.0, 0.0), |(t, d), &(pt, pd)| {
            (t + pt / count, d + pd / count)
        });
        let (mut covariance, mut variance) = (0.0, 0.0);
        for &(t, db) in &points {
            covariance += (t - mean_t) * (db - mean_db);
            variance += (t - mean_t) * (t - mean_t);
        }
        (-60.0 / (covariance / variance)) as f32
    }

    /// `x` through a bandpass around `hz`, twice.
    fn band(x: &[f32], hz: f32) -> Vec<f32> {
        let q = 2.0;
        let shape = Shape {
            g: prewarp(hz, RATE),
            k: 1.0 / q,
            m0: 0.0,
            m1: 1.0 / q,
            m2: 0.0,
        };
        let coefs = Coefs::new(&shape);
        let (mut first, mut second) = (SvfState::default(), SvfState::default());
        x.iter()
            .map(|&s| second.tick(&coefs, first.tick(&coefs, s)))
            .collect()
    }

    #[test]
    fn the_tail_falls_60_db_in_the_decay_time() {
        let mut worst = 0.0f32;
        for (size, decay) in [
            (60.0, 0.5),
            (60.0, 2.0),
            (60.0, 5.0),
            (0.0, 1.0),
            (100.0, 3.0),
            (30.0, 10.0),
        ] {
            let mut reverb = reverb(&[("size", size), ("decay", decay)]);
            let (left, right) = impulse_response(&mut reverb, 1.2 * decay + 0.5);
            let broadband = rt60(&[&left, &right]);
            // RT60 is usually quoted around 1 kHz.
            let mid = rt60(&[&band(&left, 1000.0), &band(&right, 1000.0)]);
            let high = rt60(&[&band(&left, 10_000.0), &band(&right, 10_000.0)]);
            let error = mid / decay - 1.0;
            worst = worst.max(error.abs());
            println!(
                "size {size}%, decay {decay} s: RT60 {mid:.3} s at 1 kHz ({:+.1}%), {broadband:.3} s broadband, {high:.3} s at 10 kHz",
                100.0 * error
            );
            assert!(error.abs() < 0.05, "size {size}, decay {decay}: {mid}");
            assert!(
                (broadband / decay - 1.0).abs() < 0.1,
                "size {size}, decay {decay}: {broadband}"
            );
        }
        println!("largest RT60 error at 1 kHz {:.1}%", 100.0 * worst);
    }

    #[test]
    fn highs_above_the_damping_frequency_die_faster() {
        let mut reverb = reverb(&[("decay", 2.0), ("damping", 1000.0)]);
        let (left, right) = impulse_response(&mut reverb, 3.0);
        let low = rt60(&[&band(&left, 250.0), &band(&right, 250.0)]);
        let high = rt60(&[&band(&left, 6000.0), &band(&right, 6000.0)]);
        println!("damping 1 kHz, decay 2 s: RT60 {low:.2} s at 250 Hz, {high:.2} s at 6 kHz");
        assert!((low - 2.0).abs() < 0.25, "{low}");
        assert!((high / 2.0 - HIGH_DECAY_RATIO).abs() < 0.07, "{high}");
    }

    #[test]
    fn the_tail_ends_in_silence() {
        let mut reverb = reverb(&[("decay", 1.0), ("size", 100.0)]);
        let input = noise(4800, 5);
        let (mut left, mut right) = (input.clone(), input);
        left.resize(48_000 * 20, 0.0);
        right.resize(48_000 * 20, 0.0);
        run(&mut reverb, &mut left, &mut right, 64, &Context::DEFAULT);
        assert!(left.iter().chain(&right).all(|s| s.is_finite()));
        let second = |n: usize| {
            peak(&left[n * 48_000..(n + 1) * 48_000])
                .max(peak(&right[n * 48_000..(n + 1) * 48_000]))
        };
        println!(
            "peak in second 1: {:e}, 5: {:e}, 10: {:e}, 19: {:e}",
            second(1),
            second(5),
            second(10),
            second(19)
        );
        assert!(second(5) < 1e-6);
        assert!(
            left[48_000 * 19..]
                .iter()
                .chain(&right[48_000 * 19..])
                .all(|&s| s == 0.0)
        );
    }

    fn correlation(a: &[f32], b: &[f32]) -> f64 {
        let dot = |x: &[f32], y: &[f32]| {
            x.iter()
                .zip(y)
                .map(|(&p, &q)| f64::from(p) * f64::from(q))
                .sum::<f64>()
        };
        dot(a, b) / (dot(a, a) * dot(b, b)).sqrt()
    }

    #[test]
    fn width_decorrelates_the_sides() {
        let input = noise(48_000 * 3, 11);
        let mut correlations = Vec::new();
        for width in [100.0, 50.0, 0.0] {
            let mut reverb = reverb(&[("width", width)]);
            let (mut left, mut right) = (input.clone(), input.clone());
            run(&mut reverb, &mut left, &mut right, 64, &Context::DEFAULT);
            let tail = 48_000..;
            correlations.push(correlation(&left[tail.clone()], &right[tail]));
            if width == 0.0 {
                assert_eq!(left, right);
            }
        }
        println!("left/right correlation at width 100, 50, 0%: {correlations:.3?}");
        assert!(correlations[0].abs() < 0.1, "{correlations:?}");
        assert!(correlations[1] > correlations[0] + 0.3);

        // An impulse response too, which also shows how loud the wet signal is.
        let mut struck = reverb(&[]);
        let (left, right) = impulse_response(&mut struck, 3.0);
        let impulse = correlation(&left, &right);
        let mut steady = reverb(&[]);
        let (mut l, mut r) = (input.clone(), input.clone());
        run(&mut steady, &mut l, &mut r, 64, &Context::DEFAULT);
        let level = rms(&l[48_000..]) / rms(&input[48_000..]);
        println!(
            "impulse response correlation {impulse:.3}; wet level for noise at the defaults {:.1} dB",
            crate::testing::db(level)
        );
        assert!(impulse.abs() < 0.1);
    }

    #[test]
    fn mix_zero_is_dry() {
        let input = noise(20_000, 2);
        let mut reverb = reverb(&[("mix", 0.0)]);
        let (mut left, mut right) = (input.clone(), input.clone());
        right.iter_mut().for_each(|s| *s *= -0.5);
        let expected_right = right.clone();
        run(&mut reverb, &mut left, &mut right, 64, &Context::DEFAULT);
        assert_eq!(left, input);
        assert_eq!(right, expected_right);
    }

    #[test]
    fn moving_size_and_predelay_stays_smooth() {
        let mut reverb = reverb(&[("mix", 50.0), ("decay", 4.0)]);
        let input = crate::testing::sine(220.0, 0.5, 48_000 * 4, RATE);
        let (mut left, mut right) = (input.clone(), input.clone());
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            let t = block as f32 * 64.0 / RATE;
            set(&mut reverb, "size", 50.0 + 50.0 * (TAU * t).sin());
            if block % 200 == 0 {
                set(
                    &mut reverb,
                    "predelay",
                    if block % 400 == 0 { 0.0 } else { 200.0 },
                );
            }
            reverb.process(l, r, &Context::DEFAULT);
        }
        let step = left
            .windows(2)
            .chain(right.windows(2))
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(left.iter().chain(&right).all(|s| s.is_finite()));
        println!(
            "largest step while moving size and pre-delay: {step:.4}; peak {:.3}",
            peak(&left).max(peak(&right))
        );
        assert!(step < 0.1, "{step}");
    }

    #[test]
    fn the_plate_and_the_room_fall_60_db_in_the_decay_time() {
        for (kind, size, decay, tolerance) in [
            (PLATE, 60.0, 1.0, 0.15),
            (PLATE, 100.0, 3.0, 0.15),
            (PLATE, 20.0, 0.6, 0.2),
            (ROOM, 50.0, 0.8, 0.15),
            (ROOM, 100.0, 2.0, 0.15),
        ] {
            let mut reverb = reverb(&[("type", kind as f32), ("size", size), ("decay", decay)]);
            let (left, right) = impulse_response(&mut reverb, 1.2 * decay + 0.5);
            let mid = rt60(&[&band(&left, 1000.0), &band(&right, 1000.0)]);
            let error = mid / decay - 1.0;
            println!(
                "{} size {size}%, decay {decay} s: RT60 {mid:.3} s at 1 kHz ({:+.1}%)",
                TYPE_LABELS[kind],
                100.0 * error
            );
            assert!(error.abs() < tolerance, "{kind} {size} {decay}: {mid}");
        }
    }

    #[test]
    fn the_room_starts_with_early_reflections() {
        // A click: the room answers within a few ms with separate echoes,
        // before the tail has built up; the hall answers later and denser.
        let first_echo = |kind: usize| {
            let mut reverb = reverb(&[("type", kind as f32), ("size", 0.0), ("decay", 1.0)]);
            let (left, right) = impulse_response(&mut reverb, 0.3);
            let loudest = peak(&left).max(peak(&right));
            left.iter()
                .zip(&right)
                .position(|(l, r)| l.abs().max(r.abs()) > 0.3 * loudest)
                .unwrap()
        };
        let (room, hall) = (first_echo(ROOM), first_echo(HALL));
        println!("loudest echoes start: room {room} samples, hall {hall}");
        let earliest = (EARLY_MS[0] * ROOM_SMALLEST * 0.001 * RATE) as usize;
        assert!(room >= earliest && room < 4 * earliest, "{room}");
        assert!(room < hall, "{room} {hall}");
    }

    #[test]
    fn every_type_is_about_as_loud() {
        let input = noise(48_000 * 3, 21);
        let level = |kind: usize| {
            let mut reverb = reverb(&[("type", kind as f32)]);
            let (mut left, mut right) = (input.clone(), input.clone());
            run(&mut reverb, &mut left, &mut right, 64, &Context::DEFAULT);
            crate::testing::db(rms(&left[48_000..]) / rms(&input[48_000..]))
        };
        let levels = [level(HALL), level(PLATE), level(ROOM)];
        println!("wet levels for noise, hall, plate, room: {levels:.1?} dB");
        for kind in [PLATE, ROOM] {
            assert!((levels[kind] - levels[HALL]).abs() < 4.0, "{levels:?}");
        }
    }

    #[test]
    fn switching_type_crossfades() {
        let input = crate::testing::sine(220.0, 0.5, 48_000 * 2, RATE);
        let largest_step = |reverb: &mut Reverb, switch: bool| {
            let (mut left, mut right) = (input.clone(), input.clone());
            for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
                if switch && block % 300 == 150 {
                    set(reverb, "type", ((block / 300) % TYPES) as f32 + 1.0);
                }
                reverb.process(l, r, &Context::DEFAULT);
            }
            left[4800..]
                .windows(2)
                .chain(right[4800..].windows(2))
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max)
        };
        let settled = (0..TYPES)
            .map(|kind| largest_step(&mut reverb(&[("type", kind as f32), ("mix", 50.0)]), false))
            .fold(0.0f32, f32::max);
        let switching = largest_step(&mut reverb(&[("mix", 50.0)]), true);
        println!("largest step switching types {switching:.4}, settled {settled:.4}");
        assert!(switching < 1.3 * settled, "{switching} against {settled}");
        // The meters say which is heard.
        let mut room = reverb(&[("type", ROOM as f32)]);
        let (mut l, mut r) = (input[..4800].to_vec(), input[..4800].to_vec());
        run(&mut room, &mut l, &mut r, 64, &Context::DEFAULT);
        let mut meters = [0.0; MAX_METERS];
        room.meters(&mut meters);
        assert_eq!(&meters[METER_TYPES..METER_TYPES + TYPES], &[0.0, 0.0, 1.0]);
        assert!(meters[METER_WET] > 0.01);
    }

    #[test]
    fn modulation_moves_the_tail_and_stays_bounded() {
        let input = noise(48_000, 13);
        let tail = |settings: &[(&str, f32)]| {
            let mut reverb = reverb(settings);
            let (mut left, mut right) = (input.clone(), input.clone());
            left.resize(96_000, 0.0);
            right.resize(96_000, 0.0);
            run(&mut reverb, &mut left, &mut right, 64, &Context::DEFAULT);
            left
        };
        for kind in [HALL, PLATE, ROOM] {
            let still = tail(&[("type", kind as f32), ("mod_depth", 0.0)]);
            let deep = tail(&[
                ("type", kind as f32),
                ("mod_depth", 100.0),
                ("mod_rate", 5.0),
            ]);
            assert!(deep.iter().all(|s| s.is_finite()) && peak(&deep) < 4.0);
            let apart: Vec<f32> = still.iter().zip(&deep).map(|(a, b)| a - b).collect();
            assert!(
                rms(&apart[48_000..]) > 0.1 * rms(&still[48_000..]),
                "{kind}"
            );
        }
    }
}
