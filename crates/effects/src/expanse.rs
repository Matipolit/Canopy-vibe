//! Expanse: a delay that turns into a reverb. Eight delay lines in a
//! feedback network, their lengths set by Time and spread by Warp: at no
//! warp every line is as long as the time and the network is a clean
//! echo; as warp grows the lines fan out to shorter lengths and each echo
//! smears into the next, until at full warp it is a reverb's wash. Density
//! diffuses the sound on its way in and inside every pass, from separate
//! repeats to a smooth cloud; Feedback sets how much of each pass goes
//! round again (at 100% it never dies away).
//!
//! # The network
//!
//! The lines' outputs are mixed by an 8×8 Hadamard matrix (orthogonal, so
//! no energy is gained or lost in the mixing) and written back with the
//! input. The input goes in, and the output is taken, along two vectors the
//! matrix leaves as they are (its eigenvectors for 1, one per side, at
//! right angles to each other): with every line the same length, a sound
//! then comes back unchanged each pass, a plain echo at the time on its own
//! side. Each line keeps `feedback` of its level per time's worth of delay
//! (Jot's method: a line half as long keeps the square root a pass), so
//! however warp spreads the lengths the decay stays the same. Inside every
//! pass each line goes through an allpass (the smear: Density times the
//! mode's share, a sixth of the line's length at most), a one-pole high
//! cut and a one-pole low cut, so each repeat is darker and thinner than
//! the last; and past twice full scale it is bent smoothly towards four
//! times, so no setting runs away. The reads drift on slow LFOs (Mod rate and depth,
//! a different speed for each line), cubic-interpolated.
//!
//! # Modes
//!
//! The modes are shapes of that network, after the families of sounds a
//! delay-reverb like this makes:
//!
//! - how far full warp spreads the lines (the shortest line's share of the
//!   time), or whether the lengths are simple fractions of it, which
//!   repeat in patterns;
//! - an attack: the input arrives as a swell of echoes over a share of the
//!   time rather than at once, for the slow-blooming modes;
//! - how much of Density smears inside the loop;
//! - a longer decay than the feedback alone gives;
//! - echoes of the whole sound: the network made short and quick to fade,
//!   inside a loop as long as the time that feeds its output back in with
//!   the feedback, for long reverberant repeats.
//!
//! Every one of those is continuous, so switching the mode glides from one
//! shape to the next over [`MODE_GLIDE_MS`] rather than switching anything.
//!
//! # Moving
//!
//! The time follows the tempo (a note value) or is free, up to
//! [`MAX_DELAY_S`]. Changing it moves every read like a tape head, never
//! faster than [`MAX_READ_SLOPE`] samples a sample, so the echoes bend in
//! pitch as they go, which is part of the sound of these effects; so do
//! warp and a mode change. Every float glides, at the knob's pace or within
//! a block for a modulator (see "Modulation" in `lib.rs`).

use instruments::ParamDescriptor;

use crate::delay::{NOTE_LABELS, NOTES};
use crate::line::{Ring, ring_allpass};
use crate::{Context, Effect, EffectDescriptor, Values, choice, float};
use dsp::one_pole::tpt_lowpass;
use dsp::svf::prewarp;
use dsp::{Pace, Smoothed, flush, rational_tanh};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.expanse",
    name: "Expanse",
    description: "Echoes that spread and smear into reverb",
    category: crate::Category::Space,
    params: PARAMS,
    meters: &[],
    create: || Box::new(Expanse::new()),
    ..EffectDescriptor::BASE
};

/// A mode: the network's shape (see the module's notes).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    /// The shortest line's share of the time at full warp; the longest is
    /// always the whole time.
    pub spread: f32,
    /// How far the lengths at full warp are [`PATTERN`]'s simple fractions
    /// instead of `spread`'s fan, `0..=1`.
    pub pattern: f32,
    /// Every line's length as a share of the time.
    pub scale: f32,
    /// The input's swell, as a share of the time; 0 for none.
    pub attack: f32,
    /// How much of Density diffuses inside the loop, `0..=1`.
    pub smear: f32,
    /// The decay as a multiple of what the feedback alone gives.
    pub decay: f32,
    /// How much the sound repeats as a whole, `0..=1`.
    pub echo: f32,
}

const fn shape(
    spread: f32,
    pattern: f32,
    scale: f32,
    attack: f32,
    smear: f32,
    decay: f32,
    echo: f32,
) -> Shape {
    Shape {
        spread,
        pattern,
        scale,
        attack,
        smear,
        decay,
        echo,
    }
}

/// The modes, in the order of the `mode` parameter's labels.
pub const MODES: [(&str, Shape); 13] = [
    // Fast attack, shorter decay, dense.
    ("Twin", shape(0.35, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0)),
    // Fast attack, shorter decay, as dense as Density says.
    ("Split", shape(0.45, 0.0, 1.0, 0.03, 0.6, 1.0, 0.0)),
    // A medium swell, longer decay.
    ("Bloom", shape(0.3, 0.0, 1.0, 0.25, 0.9, 1.4, 0.0)),
    // A slow swell, longer decay, dense.
    ("Rise", shape(0.3, 0.0, 1.0, 0.55, 1.0, 1.4, 0.0)),
    // A medium swell, a very long decay.
    ("Abyss", shape(0.25, 0.0, 1.0, 0.3, 0.8, 2.5, 0.0)),
    // The slowest swell, a very long decay, very dense.
    ("Haze", shape(0.25, 0.0, 1.0, 1.0, 1.0, 2.5, 0.0)),
    // Fast attack, shorter decay, sparse.
    ("Harp", shape(0.5, 0.0, 1.0, 0.0, 0.15, 1.0, 0.0)),
    // Fast attack, shorter decay, medium density.
    ("Horn", shape(0.4, 0.0, 1.0, 0.0, 0.45, 1.0, 0.0)),
    // A long reverb in long echoes of itself.
    ("Cloud", shape(0.3, 0.0, 0.15, 0.2, 1.0, 1.0, 1.0)),
    // A long reverb in very long echoes, slowly.
    ("Far cloud", shape(0.3, 0.0, 0.25, 0.5, 1.0, 1.0, 1.0)),
    // Patterns of repeats, fast attack.
    ("Ripple", shape(0.4, 1.0, 1.0, 0.0, 0.3, 1.0, 0.0)),
    // The same, smaller.
    ("Small ripple", shape(0.4, 1.0, 0.5, 0.0, 0.3, 1.0, 0.0)),
    // Sparse at first, building to a lush tail.
    ("Swirl", shape(0.3, 0.6, 1.0, 0.0, 0.6, 1.6, 0.0)),
];

pub const MODE_LABELS: &[&str] = &[
    "Twin",
    "Split",
    "Bloom",
    "Rise",
    "Abyss",
    "Haze",
    "Harp",
    "Horn",
    "Cloud",
    "Far cloud",
    "Ripple",
    "Small ripple",
    "Swirl",
];

const PARAMS: &[ParamDescriptor] = &[
    choice("mode", "Mode", MODE_LABELS, 0),
    choice("sync", "Sync", &["Tempo", "Free"], 1),
    choice("note", "Time", NOTE_LABELS, 8),
    float("time", "Time (free)", 1.0, 2000.0, 250.0, "ms", true),
    float("warp", "Warp", 0.0, 100.0, 50.0, "%", false),
    float("feedback", "Feedback", 0.0, 100.0, 60.0, "%", false),
    float("density", "Density", 0.0, 100.0, 50.0, "%", false),
    float("width", "Width", -100.0, 100.0, 100.0, "%", false),
    float("low_cut", "Low cut", 10.0, 2000.0, 10.0, "Hz", true),
    float(
        "high_cut", "High cut", 200.0, 20_000.0, 20_000.0, "Hz", true,
    ),
    float("mod_rate", "Mod rate", 0.01, 10.0, 0.5, "Hz", true),
    float("mod_depth", "Mod depth", 0.0, 100.0, 50.0, "%", false),
    float("mix", "Mix", 0.0, 100.0, 50.0, "%", false),
];

const COUNT: usize = 13;

pub const MODE: usize = 0;
const SYNC: usize = 1;
const NOTE: usize = 2;
const TIME: usize = 3;
const WARP: usize = 4;
const FEEDBACK: usize = 5;
const DENSITY: usize = 6;
const WIDTH: usize = 7;
const LOW_CUT: usize = 8;
const HIGH_CUT: usize = 9;
const MOD_RATE: usize = 10;
const MOD_DEPTH: usize = 11;
const MIX: usize = 12;

const LINES: usize = 8;
/// The longest time: a synced note longer than this is held to it.
pub const MAX_DELAY_S: f32 = 3.0;
/// Each line's length at full warp as a power of the mode's spread:
/// 0 is the whole time, 1 the spread. Neighbours in the matrix differ.
const FAN: [f32; LINES] = [0.0, 0.57, 0.29, 0.86, 0.14, 0.71, 0.43, 1.0];
/// The lengths a patterned mode reaches instead, as shares of the time.
const PATTERN: [f32; LINES] = [1.0, 0.75, 0.5, 0.625, 0.375, 0.875, 0.25, 2.0 / 3.0];
/// Each line's LFO speed as a multiple of the rate, and its depth's share.
const MOD_RATES: [f32; LINES] = [1.0, 1.13, 0.87, 1.27, 0.79, 1.07, 0.93, 1.19];
const MOD_SHARES: [f32; LINES] = [1.0, 0.8, 0.9, 0.7, 0.85, 0.75, 0.95, 0.65];
/// How far the reads drift at full depth.
const MAX_MOD_MS: f32 = 5.0;
/// The smear allpasses: their length as a share of their line's, at most
/// [`SMEAR_MAX_MS`], varied a little line to line, and their gain at full.
const SMEAR_SHARE: f32 = 1.0 / 6.0;
const SMEAR_MAX_MS: f32 = 60.0;
const SMEAR_VARY: [f32; LINES] = [1.0, 0.77, 1.21, 0.89, 1.13, 0.71, 0.93, 1.07];
const SMEAR_GAIN: f32 = 0.65;
/// The input diffusers at full density, and their gain there; the right
/// channel's are this much longer.
const DIFFUSER_MS: [f32; 4] = [4.771, 3.595, 12.73, 9.307];
const DIFFUSER_GAIN: f32 = 0.7;
const DIFFUSER_RIGHT: f32 = 1.07;
/// The swell's taps, at even steps across the attack, louder as they go.
const SWELL_TAPS: usize = 6;
/// The network's gain per pass of its longest line in the echoing modes,
/// where the repeats' feedback is the loop's around it.
const ECHO_NETWORK_GAIN: f32 = 0.6;
/// Shortest line, in samples.
const SHORTEST: f32 = 2.0;
/// Where the loop starts to be bent down, and how loud it can get.
const KNEE: f32 = 2.0;
const HEADROOM: f32 = 4.0;

/// Smoothing, and the chunk the slower values move in.
const SMOOTH_MS: f32 = 20.0;
const MODE_GLIDE_MS: f32 = 200.0;
const CHUNK: usize = 32;
/// The fastest the lines' reads move, in samples per sample; and the
/// diffusers' and smears', which Density moves, so a fast LFO on it
/// doesn't warble the whole sound.
const MAX_READ_SLOPE: f32 = 0.5;
const MAX_DIFFUSER_SLOPE: f32 = 0.05;

/// A position that moves towards where it should be a step a sample.
#[derive(Debug, Clone, Copy, Default)]
struct Glide {
    now: f32,
    step: f32,
}

impl Glide {
    /// Sets off towards `target`, to arrive in `samples` if the slope
    /// allows; settles exactly once close.
    fn aim(&mut self, target: f32, samples: usize, slope: f32) {
        let distance = target - self.now;
        if distance.abs() < 1e-3 {
            self.now = target;
            self.step = 0.0;
        } else {
            self.step = (distance / samples as f32).clamp(-slope, slope);
        }
    }

    fn jump(&mut self, target: f32) {
        self.now = target;
        self.step = 0.0;
    }

    /// [`Glide::aim`] once placed, [`Glide::jump`] before.
    fn steer(&mut self, target: f32, placed: bool, slope: f32) {
        if placed {
            self.aim(target, CHUNK, slope);
        } else {
            self.jump(target);
        }
    }

    #[inline(always)]
    fn next(&mut self) -> f32 {
        self.now += self.step;
        self.now
    }
}

/// The 8×8 Hadamard matrix times `x`, normalised (orthogonal).
#[inline(always)]
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
    let norm = (LINES as f32).sqrt().recip();
    for v in x {
        *v *= norm;
    }
}

/// The input and output vectors, one per side: eigenvectors of the
/// Hadamard matrix for 1, at right angles to each other, unit length.
fn sides() -> [[f32; LINES]; 2] {
    let column = |j: usize| {
        let mut e = [0.0; LINES];
        e[j] = 1.0;
        hadamard(&mut e);
        e
    };
    let unit = |v: [f32; LINES]| {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.map(|x| x / norm)
    };
    let left = unit(std::array::from_fn(|i| {
        f32::from(u8::from(i == 0)) + column(0)[i]
    }));
    let w: [f32; LINES] = std::array::from_fn(|i| f32::from(u8::from(i == 4)) + column(4)[i]);
    let along = w.iter().zip(&left).map(|(a, b)| a * b).sum::<f32>();
    let right = unit(std::array::from_fn(|i| w[i] - along * left[i]));
    [left, right]
}

/// The loop's soft ceiling: untouched up to [`KNEE`], then bent smoothly
/// (the slope carries on through the knee) towards [`HEADROOM`].
#[inline(always)]
fn bend(x: f32) -> f32 {
    let over = x.abs() - KNEE;
    if over <= 0.0 {
        return x;
    }
    let room = HEADROOM - KNEE;
    (KNEE + room * rational_tanh(over / room)).copysign(x)
}

pub struct Expanse {
    values: Values<COUNT>,
    sample_rate: f32,
    lines: [Ring; LINES],
    smear: [Ring; LINES],
    /// Per channel: the input's history for the swell, its diffusers, and
    /// the whole-sound echo.
    history: [Ring; 2],
    diffusers: [[Ring; 4]; 2],
    echoes: [Ring; 2],
    /// Where each line is read, in samples ago (its length), and each
    /// smear allpass's length.
    reads: [Glide; LINES],
    smear_len: [Glide; LINES],
    diffuser_len: [Glide; 4],
    /// The swell's length, and the echo's.
    swell: Glide,
    echo_len: Glide,
    lfo: [f32; LINES],
    /// One-pole states per line: high cut, then low cut.
    high_state: [f32; LINES],
    low_state: [f32; LINES],
    /// Worked out each chunk.
    gains: [f32; LINES],
    high_gain: f32,
    low_gain: f32,
    smear_gain: f32,
    diffuser_gain: f32,
    swell_mix: f32,
    echo_feedback: f32,
    /// The delay asked for, in samples, and how the time last moved.
    delay: f32,
    time_pace: Pace,
    placed: bool,
    countdown: usize,
    sides: [[f32; LINES]; 2],
    warp: Smoothed,
    feedback: Smoothed,
    density: Smoothed,
    log_low_cut: Smoothed,
    log_high_cut: Smoothed,
    log_mod_rate: Smoothed,
    mod_depth: Smoothed,
    /// The mode's shape, gliding between modes.
    spread: Smoothed,
    pattern: Smoothed,
    scale: Smoothed,
    attack: Smoothed,
    smear_amount: Smoothed,
    decay: Smoothed,
    echo: Smoothed,
    width: Smoothed,
    mix: Smoothed,
}

impl Expanse {
    pub fn new() -> Expanse {
        let slow = |value| Smoothed::new(value, SMOOTH_MS);
        let mode = |value| Smoothed::new(value, MODE_GLIDE_MS);
        let mut expanse = Expanse {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            lines: Default::default(),
            smear: Default::default(),
            history: Default::default(),
            diffusers: Default::default(),
            echoes: Default::default(),
            reads: [Glide::default(); LINES],
            smear_len: [Glide::default(); LINES],
            diffuser_len: [Glide::default(); 4],
            swell: Glide::default(),
            echo_len: Glide::default(),
            lfo: [0.0; LINES],
            high_state: [0.0; LINES],
            low_state: [0.0; LINES],
            gains: [0.0; LINES],
            high_gain: 1.0,
            low_gain: 0.0,
            smear_gain: 0.0,
            diffuser_gain: 0.0,
            swell_mix: 0.0,
            echo_feedback: 0.0,
            delay: 1.0,
            time_pace: Pace::Knob,
            placed: false,
            countdown: 0,
            sides: sides(),
            warp: slow(0.0),
            feedback: slow(0.0),
            density: slow(0.0),
            log_low_cut: slow(0.0),
            log_high_cut: slow(0.0),
            log_mod_rate: slow(0.0),
            mod_depth: slow(0.0),
            spread: mode(1.0),
            pattern: mode(0.0),
            scale: mode(1.0),
            attack: mode(0.0),
            smear_amount: mode(0.0),
            decay: mode(1.0),
            echo: mode(0.0),
            width: slow(1.0),
            mix: slow(0.0),
        };
        for index in 0..COUNT {
            expanse.apply(index, Pace::Knob);
        }
        expanse.prepare_smoothers();
        expanse.reset();
        expanse
    }

    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            MODE => {
                let (_, s) = MODES[(value.round().max(0.0) as usize).min(MODES.len() - 1)];
                self.spread.set_target(s.spread);
                self.pattern.set_target(s.pattern);
                self.scale.set_target(s.scale);
                self.attack.set_target(s.attack);
                self.smear_amount.set_target(s.smear);
                self.decay.set_target(s.decay);
                self.echo.set_target(s.echo);
            }
            WARP => self.warp.move_to(value / 100.0, pace),
            FEEDBACK => self.feedback.move_to(value / 100.0, pace),
            DENSITY => self.density.move_to(value / 100.0, pace),
            WIDTH => self.width.move_to(value / 100.0, pace),
            LOW_CUT => self.log_low_cut.move_to(value.log2(), pace),
            HIGH_CUT => self.log_high_cut.move_to(value.log2(), pace),
            MOD_RATE => self.log_mod_rate.move_to(value.log2(), pace),
            MOD_DEPTH => self.mod_depth.move_to(value / 100.0, pace),
            MIX => self.mix.move_to(value / 100.0, pace),
            TIME => self.time_pace = pace,
            _ => self.time_pace = Pace::Knob,
        }
    }

    fn chunked(&mut self) -> [&mut Smoothed; 14] {
        [
            &mut self.warp,
            &mut self.feedback,
            &mut self.density,
            &mut self.log_low_cut,
            &mut self.log_high_cut,
            &mut self.log_mod_rate,
            &mut self.mod_depth,
            &mut self.spread,
            &mut self.pattern,
            &mut self.scale,
            &mut self.attack,
            &mut self.smear_amount,
            &mut self.decay,
            &mut self.echo,
        ]
    }

    fn prepare_smoothers(&mut self) {
        let sample_rate = self.sample_rate;
        for smoothed in self.chunked() {
            smoothed.prepare_every(sample_rate, CHUNK);
        }
        self.width.prepare(sample_rate);
        self.mix.prepare(sample_rate);
    }

    /// The delay asked for, in samples.
    fn delay_samples(&self, context: &Context) -> f32 {
        let seconds = if self.values[SYNC] < 0.5 {
            let bpm = if context.bpm.is_finite() && context.bpm > 0.0 {
                context.bpm
            } else {
                120.0
            };
            let note = (self.values[NOTE].max(0.0) as usize).min(NOTES.len() - 1);
            (NOTES[note].1 * 60.0 / bpm) as f32
        } else {
            self.values[TIME] / 1000.0
        };
        (seconds.min(MAX_DELAY_S) * self.sample_rate).max(SHORTEST)
    }

    /// Moves the slower values a chunk on and works out where everything
    /// should be by the end of it.
    fn update(&mut self) {
        let placed = self.placed;
        let sr = self.sample_rate;
        let warp = self.warp.tick();
        let feedback = self.feedback.tick();
        let density = self.density.tick();
        let low_cut = self.log_low_cut.tick().exp2();
        let high_cut = self.log_high_cut.tick().exp2();
        let mod_rate = self.log_mod_rate.tick().exp2();
        let mod_depth = self.mod_depth.tick();
        let spread = self.spread.tick();
        let pattern = self.pattern.tick();
        let scale = self.scale.tick();
        let attack = self.attack.tick();
        let smear = self.smear_amount.tick();
        let decay = self.decay.tick();
        let echo = self.echo.tick();
        let d = self.delay;

        // The loop's gain per pass of its longest line: the feedback,
        // stretched by the mode's decay, or the short network's in the
        // echoing modes (whose repeats take the feedback instead).
        let stretched = feedback.max(1e-9).log2() / decay.max(0.1);
        let network = (1.0 - echo) * stretched + echo * ECHO_NETWORK_GAIN.log2();
        let chunk_s = CHUNK as f32 / sr;
        let depth = mod_depth * MAX_MOD_MS * 0.001 * sr;
        for i in 0..LINES {
            let fan = spread.powf(FAN[i]);
            let full = fan + pattern * (PATTERN[i] - fan);
            let share = 1.0 + warp * (full - 1.0);
            self.gains[i] = (network * share).exp2();
            self.lfo[i] = (self.lfo[i] + mod_rate * MOD_RATES[i] * chunk_s).fract();
            let drift = depth * MOD_SHARES[i] * (std::f32::consts::TAU * self.lfo[i]).sin();
            let smear_len = (smear
                * density
                * (SMEAR_SHARE * scale * share * d).min(SMEAR_MAX_MS * 0.001 * sr)
                * SMEAR_VARY[i])
                .max(1.0);
            self.smear_len[i].steer(smear_len, placed, MAX_DIFFUSER_SLOPE);
            // The smear's own length is part of the pass.
            let length = (scale * share * d + drift - smear_len).max(SHORTEST);
            self.reads[i].steer(length, placed, MAX_READ_SLOPE);
        }
        for (k, glide) in self.diffuser_len.iter_mut().enumerate() {
            glide.steer(
                (density * DIFFUSER_MS[k] * 0.001 * sr).max(1.0),
                placed,
                MAX_DIFFUSER_SLOPE,
            );
        }
        let history = (self.history[0].len() - 4) as f32;
        self.swell
            .steer((attack * d).min(history), placed, MAX_READ_SLOPE);
        self.swell_mix = (3.0 * attack).min(1.0);
        self.echo_len.steer(d, placed, MAX_READ_SLOPE);
        self.echo_feedback = echo * feedback;
        self.smear_gain = SMEAR_GAIN * smear * density;
        self.diffuser_gain = DIFFUSER_GAIN * density;
        let one_pole = |hz: f32| {
            let g = prewarp(hz.min(0.45 * sr), sr);
            g / (1.0 + g)
        };
        self.high_gain = one_pole(high_cut);
        self.low_gain = one_pole(low_cut);
    }
}

impl Default for Expanse {
    fn default() -> Self {
        Expanse::new()
    }
}

impl Effect for Expanse {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        let samples = |ms: f32| (ms * 0.001 * sample_rate).ceil() as usize + 8;
        let longest = MAX_DELAY_S * 1000.0;
        for line in &mut self.lines {
            line.allocate(samples(longest + MAX_MOD_MS));
        }
        for ring in &mut self.smear {
            ring.allocate(samples(SMEAR_MAX_MS * 1.25));
        }
        for side in 0..2 {
            self.history[side].allocate(samples(longest));
            self.echoes[side].allocate(samples(longest));
            for (k, ring) in self.diffusers[side].iter_mut().enumerate() {
                ring.allocate(samples(DIFFUSER_MS[k] * DIFFUSER_RIGHT));
            }
        }
        self.prepare_smoothers();
        self.reset();
    }

    fn reset(&mut self) {
        for ring in self
            .lines
            .iter_mut()
            .chain(&mut self.smear)
            .chain(&mut self.history)
            .chain(&mut self.echoes)
            .chain(self.diffusers.iter_mut().flatten())
        {
            ring.clear();
        }
        self.high_state = [0.0; LINES];
        self.low_state = [0.0; LINES];
        // From the same phases every time, so renders repeat exactly.
        self.lfo = std::array::from_fn(|i| (i as f32 * 0.618_034).fract());
        for smoothed in self.chunked() {
            smoothed.settle();
        }
        self.width.settle();
        self.mix.settle();
        self.placed = false;
        self.countdown = 0;
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: &Context) {
        self.values.played();
        if self.lines[0].len() <= 1 {
            return;
        }
        self.delay = self.delay_samples(context);
        let [feed_l, feed_r] = self.sides;
        let len = left.len().min(right.len());
        let mut start = 0;
        while start < len {
            if self.countdown == 0 {
                self.update();
                self.placed = true;
                self.countdown = CHUNK;
            }
            let n = self.countdown.min(len - start);
            for i in start..start + n {
                let (x_l, x_r) = (left[i], right[i]);

                // The swell: the input as a crescendo of echoes across the
                // attack, blended in by how much attack the mode has.
                let swell = self.swell.next();
                let (mut in_l, mut in_r) = (x_l, x_r);
                if self.swell_mix > 0.0 {
                    let [h_l, h_r] = &mut self.history;
                    h_l.push(x_l);
                    h_r.push(x_r);
                    let (mut sum_l, mut sum_r) = (0.0, 0.0);
                    for k in 0..SWELL_TAPS {
                        let weight = (k + 1) as f32 * 2.0 / (SWELL_TAPS * (SWELL_TAPS + 1)) as f32;
                        let age = swell * (k + 1) as f32 / SWELL_TAPS as f32;
                        sum_l += weight * h_l.get_between(age);
                        sum_r += weight * h_r.get_between(age);
                    }
                    in_l += self.swell_mix * (sum_l - x_l);
                    in_r += self.swell_mix * (sum_r - x_r);
                } else {
                    self.history[0].push(x_l);
                    self.history[1].push(x_r);
                }

                // Diffused on the way in.
                for k in 0..4 {
                    let delay = self.diffuser_len[k].next();
                    let gain = self.diffuser_gain;
                    in_l = ring_allpass(&mut self.diffusers[0][k], in_l, gain, delay);
                    in_r = ring_allpass(
                        &mut self.diffusers[1][k],
                        in_r,
                        gain,
                        1.0 + (delay - 1.0) * DIFFUSER_RIGHT,
                    );
                }

                // The whole sound's echo, back in.
                let echo_age = self.echo_len.next() - 1.0;
                if self.echo_feedback > 0.0 {
                    in_l += self.echo_feedback * self.echoes[0].get_between(echo_age);
                    in_r += self.echo_feedback * self.echoes[1].get_between(echo_age);
                }

                // The network: read, smear, filter, tap, lose, mix, write.
                let mut y = [0.0f32; LINES];
                for (line, y) in y.iter_mut().enumerate() {
                    let age = self.reads[line].next() - 1.0;
                    let read = self.lines[line].get_between(age);
                    let smeared = ring_allpass(
                        &mut self.smear[line],
                        read,
                        self.smear_gain,
                        self.smear_len[line].next(),
                    );
                    let dark = tpt_lowpass(&mut self.high_state[line], self.high_gain, smeared);
                    *y = dark - tpt_lowpass(&mut self.low_state[line], self.low_gain, dark);
                }
                let (mut wet_l, mut wet_r) = (0.0, 0.0);
                for line in 0..LINES {
                    wet_l += feed_l[line] * y[line];
                    wet_r += feed_r[line] * y[line];
                    y[line] *= self.gains[line];
                }
                hadamard(&mut y);
                for line in 0..LINES {
                    let fed = y[line] + feed_l[line] * in_l + feed_r[line] * in_r;
                    self.lines[line].push(flush(bend(fed)));
                }
                self.echoes[0].push(flush(wet_l));
                self.echoes[1].push(flush(wet_r));

                let width = self.width.tick();
                let mid = 0.5 * (wet_l + wet_r);
                let side = 0.5 * (wet_l - wet_r) * width;
                let mix = self.mix.tick();
                left[i] = x_l + mix * (mid + side - x_l);
                right[i] = x_r + mix * (mid - side - x_r);
            }
            for state in self.high_state.iter_mut().chain(&mut self.low_state) {
                *state = flush(*state);
            }
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
                self.reset();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, rms, run, set};

    fn expanse(settings: &[(&str, f32)]) -> Expanse {
        let mut e = Expanse::new();
        e.prepare(RATE, 64);
        let defaults = [
            ("mix", 100.0),
            ("mod_depth", 0.0),
            ("density", 0.0),
            ("sync", 1.0),
        ];
        for &(id, value) in defaults.iter().chain(settings) {
            set(&mut e, id, value);
        }
        e.reset();
        e
    }

    fn impulse(e: &mut Expanse, len: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut left, mut right) = (vec![0.0; len], vec![0.0; len]);
        left[0] = 1.0;
        right[0] = 1.0;
        run(e, &mut left, &mut right, 64, &Context::DEFAULT);
        (left, right)
    }

    #[test]
    fn the_sides_are_unit_eigenvectors_at_right_angles() {
        let [l, r] = sides();
        for v in [l, r] {
            let mut h = v;
            hadamard(&mut h);
            for i in 0..LINES {
                assert!((h[i] - v[i]).abs() < 1e-6);
            }
            assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-6);
        }
        assert!(l.iter().zip(&r).map(|(a, b)| a * b).sum::<f32>().abs() < 1e-6);
    }

    #[test]
    fn without_warp_it_is_a_clean_echo() {
        // An impulse at 100 ms and feedback a half: echoes at 4800, 9600,
        // ... (and the four samples the idle input diffusers take), with
        // nothing between.
        let mut e = expanse(&[("time", 100.0), ("warp", 0.0), ("feedback", 50.0)]);
        let (left, right) = impulse(&mut e, 4 * 4800 + 100);
        for k in 1..=3 {
            let at = k * 4800 + 4;
            let peak = (at - 8..at + 8)
                .max_by(|&a, &b| left[a].abs().total_cmp(&left[b].abs()))
                .unwrap();
            // Three passes of the high cut can move the peak a sample on.
            assert!(peak.abs_diff(at) <= 1, "echo {k} at {peak}");
            // Only the low cut's slow undershoot between them.
            let between = left[at - 4700..at - 100]
                .iter()
                .fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(between < 3e-3, "echo {k}: {between} between");
        }
        assert!((left[4804] - right[4804]).abs() < 1e-4);

        // A 1 kHz burst, where the filters in the loop take nothing: each
        // echo half the last, the first as loud as the burst.
        let mut e = expanse(&[("time", 100.0), ("warp", 0.0), ("feedback", 50.0)]);
        let mut left = vec![0.0; 5 * 4800];
        for (n, s) in left.iter_mut().take(480).enumerate() {
            let window = (std::f32::consts::PI * n as f32 / 480.0).sin().powi(2);
            *s = window * (std::f32::consts::TAU * 1000.0 * n as f32 / RATE).sin();
        }
        let mut right = left.clone();
        let burst = rms(&left[..480]);
        run(&mut e, &mut left, &mut right, 64, &Context::DEFAULT);
        let level = |k: usize| rms(&left[k * 4800 + 4..k * 4800 + 484]);
        assert!(
            (level(1) / burst - 1.0).abs() < 0.01,
            "{}",
            level(1) / burst
        );
        for k in 2..=4 {
            let ratio = level(k) / level(k - 1);
            assert!((ratio - 0.5).abs() < 0.005, "echo {k}: {ratio} of the last");
        }
    }

    #[test]
    fn warp_spreads_the_echoes_into_a_wash() {
        // The share of the first 400 ms's energy outside the echo times.
        let spread = |warp: f32| {
            let mut e = expanse(&[("time", 100.0), ("warp", warp), ("feedback", 70.0)]);
            let (left, _) = impulse(&mut e, 19_200);
            let total: f64 = left.iter().map(|s| f64::from(s * s)).sum();
            let on_echoes: f64 = (1..4)
                .map(|k| {
                    left[k * 4800 + 2..k * 4800 + 7]
                        .iter()
                        .map(|s| f64::from(s * s))
                        .sum::<f64>()
                })
                .sum();
            1.0 - on_echoes / total
        };
        let (clean, washed) = (spread(0.0), spread(100.0));
        println!("energy between echoes: {clean:.3} at no warp, {washed:.3} at full");
        assert!(clean < 0.05);
        assert!(washed > 0.3);
    }

    #[test]
    fn feedback_sets_the_decay_however_the_lines_spread() {
        // The level a second in against the first 100 ms, at full warp:
        // 60% a 100 ms pass is -4.4 dB a pass, -44 dB over ten.
        let mut e = expanse(&[("time", 100.0), ("warp", 100.0), ("feedback", 60.0)]);
        let (left, right) = impulse(&mut e, 72_000);
        let early = rms(&left[4800..9600]) + rms(&right[4800..9600]);
        let late = rms(&left[52_800..57_600]) + rms(&right[52_800..57_600]);
        let fall = 20.0 * (late / early).log10();
        let expected = 10.0 * 20.0 * 0.6f64.log10();
        println!("fell {fall:.1} dB over ten passes, expected about {expected:.1}");
        assert!((fall - expected).abs() < 6.0, "{fall}");
    }

    #[test]
    fn full_feedback_rings_on_but_stays_bounded() {
        let mut e = expanse(&[
            ("time", 50.0),
            ("warp", 70.0),
            ("feedback", 100.0),
            ("density", 100.0),
        ]);
        let len = 48_000 * 6;
        let mut left = crate::testing::noise(len, 7);
        left[48_000..].fill(0.0);
        let mut right = left.clone();
        run(&mut e, &mut left, &mut right, 64, &Context::DEFAULT);
        let tail = rms(&left[len - 48_000..]);
        assert!(tail > 1e-3, "still ringing: {tail}");
        assert!(
            left.iter()
                .chain(&right)
                .all(|s| s.is_finite() && s.abs() < 3.0 * HEADROOM)
        );
    }

    #[test]
    fn every_mode_makes_a_tail_and_switching_glides() {
        for mode in 0..MODES.len() {
            let mut e = expanse(&[("mode", mode as f32), ("time", 200.0), ("density", 60.0)]);
            let (left, right) = impulse(&mut e, 96_000);
            let level = rms(&left) + rms(&right);
            assert!(
                level > 1e-3 && level.is_finite(),
                "{}: {level}",
                MODES[mode].0
            );
        }
        // From one mode to another under a steady sine: no step much
        // larger than the sine's own.
        let mut e = expanse(&[("time", 200.0), ("density", 60.0), ("mix", 50.0)]);
        let len = 48_000 * 2;
        let input = crate::testing::sine(220.0, 0.25, len, RATE);
        let (mut left, mut right) = (input.clone(), input);
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            if block == 375 {
                e.set_param(MODE, 8.0);
            }
            e.process(l, r, &Context::DEFAULT);
        }
        let step = left
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(step < 0.2, "{step}");
    }

    #[test]
    fn negative_width_swaps_the_sides() {
        let mut wide = expanse(&[("time", 30.0), ("warp", 80.0), ("width", 100.0)]);
        let mut flipped = expanse(&[("time", 30.0), ("warp", 80.0), ("width", -100.0)]);
        let input = crate::testing::noise(9600, 3);
        let (mut a_l, mut a_r) = (input.clone(), vec![0.0; 9600]);
        let (mut b_l, mut b_r) = (input, vec![0.0; 9600]);
        run(&mut wide, &mut a_l, &mut a_r, 64, &Context::DEFAULT);
        run(&mut flipped, &mut b_l, &mut b_r, 64, &Context::DEFAULT);
        for i in 0..9600 {
            assert!((a_l[i] - b_r[i]).abs() < 1e-5 && (a_r[i] - b_l[i]).abs() < 1e-5);
        }
    }
}
