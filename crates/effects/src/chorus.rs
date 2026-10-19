//! Chorus: chorus, flanger or phaser, all swept by one LFO (a sine or a
//! triangle), free in Hz or one cycle per note value of the song's tempo.
//!
//! - **Chorus:** one to four voices per side, each the input delayed by
//!   `delay` and swung around it by the LFO, the voices spread evenly
//!   through its cycle. Up to half the delay either way at full depth.
//! - **Flanger:** one delay per side, swung by the LFO from nothing to twice
//!   `delay` at full depth, and fed back into itself: a comb filter whose
//!   notches move.
//! - **Phaser:** 2 to 12 first-order all-pass filters per side (the
//!   trapezoidal kind, which stays clean while its corner moves every
//!   sample), their corner swept around `center` on a log scale, three
//!   octaves either way at full depth, with feedback around the chain. Mixed
//!   with the dry sound they make half as many notches as there are stages.
//!
//! `delay` is the middle of the sweep in milliseconds for the chorus
//! (usually 5 to 40 ms) and the flanger (usually 0.1 to 10 ms); the phaser
//! uses `center` instead. `feedback` (flanger and phaser; negative flips
//! its polarity, which moves the resonances half way between where positive
//! puts them) also scales the wet sound by `sqrt(1 - feedback²)`, which keeps
//! its loudness on broadband material about where it is without feedback.
//! `spread` is how far the right side's LFO runs ahead of the left's; with
//! several chorus voices the right side's sit between the left's, all the
//! way between at 180°, so even a mono input comes out wide. `mix` blends
//! dry and wet and `output` sets the level of both.
//!
//! The delay lines are read between samples with a cubic through the four
//! nearest (Catmull-Rom). The delay never changes faster than
//! `MAX_SLOPE` samples per sample, which caps how far fast, deep settings
//! bend the pitch, and with it the aliasing. Feedback goes through a 10 Hz
//! high-pass, so DC can't build up, and anything in a feedback loop far
//! louder than full scale is bent down smoothly, so no setting can run away.
//!
//! In tempo sync the LFO stays locked to the bar the way the shaper's
//! shapes do: while the transport rolls, where it is comes from the song
//! position ([`Context::beat`]); while stopped it runs on freely from where
//! it was. When the two disagree (playback starting, a seek, switching to
//! sync) it catches up over a fraction of a second instead of jumping, so
//! the sweep doesn't click. Switching the mode, shape, voices or stages
//! crossfades; everything else glides, at the knob's pace or, for a
//! modulator, within a block (see "Modulation" in `lib.rs`). However the
//! delay, depth, spread or voices move, no voice's delay changes by more
//! than [`MAX_TAP_SLOPE`] samples a sample: a fast modulator bends the
//! pitch, but never so far that a voice stops or runs backwards.

use std::f32::consts::{PI, TAU};

use instruments::ParamDescriptor;

use crate::line::{Ring, contain, slew};
use crate::{Category, Context, Effect, EffectDescriptor, MAX_METERS, Values, choice, float};
use dsp::one_pole::{DcBlocker, pole, tpt_lowpass};
use dsp::{Pace, Smoothed};
use dsp::{flush, settle};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.chorus",
    name: "Chorus",
    description: "Chorus, flanger and phaser, free or synced to the tempo",
    category: Category::Modulation,
    params: PARAMS,
    meters: &[
        "LFO phase, left (0 to 1)",
        "LFO phase, right (0 to 1)",
        "LFO rate (Hz)",
    ],
    create: || Box::new(Chorus::new()),
    ..EffectDescriptor::BASE
};

/// The modes, in the order of the `mode` parameter's labels.
pub const CHORUS: usize = 0;
pub const FLANGER: usize = 1;
pub const PHASER: usize = 2;
const MODES: usize = 3;

/// Note values for tempo sync: one LFO cycle per note value, and its
/// length in quarter notes. Bars are 4/4.
pub const NOTES: [(&str, f64); 17] = [
    ("1/32", 0.125),
    ("1/16 T", 1.0 / 6.0),
    ("1/16", 0.25),
    ("1/16 D", 0.375),
    ("1/8 T", 1.0 / 3.0),
    ("1/8", 0.5),
    ("1/8 D", 0.75),
    ("1/4 T", 2.0 / 3.0),
    ("1/4", 1.0),
    ("1/4 D", 1.5),
    ("1/2 T", 4.0 / 3.0),
    ("1/2", 2.0),
    ("1/2 D", 3.0),
    ("1 bar", 4.0),
    ("2 bars", 8.0),
    ("4 bars", 16.0),
    ("8 bars", 32.0),
];

const NOTE_LABELS: &[&str] = &[
    "1/32", "1/16 T", "1/16", "1/16 D", "1/8 T", "1/8", "1/8 D", "1/4 T", "1/4", "1/4 D", "1/2 T",
    "1/2", "1/2 D", "1 bar", "2 bars", "4 bars", "8 bars",
];

const PARAMS: &[ParamDescriptor] = &[
    choice("mode", "Mode", &["Chorus", "Flanger", "Phaser"], CHORUS),
    choice("sync", "Sync", &["Tempo", "Free"], 1),
    float("rate", "Rate (free)", 0.01, 20.0, 0.8, "Hz", true),
    choice("note", "Rate", NOTE_LABELS, 13),
    choice("shape", "LFO shape", &["Sine", "Triangle"], 0),
    float("depth", "Depth", 0.0, 100.0, 50.0, "%", false),
    float("delay", "Delay", 0.1, 40.0, 10.0, "ms", true),
    float("center", "Center", 50.0, 10_000.0, 800.0, "Hz", true),
    float("feedback", "Feedback", -95.0, 95.0, 0.0, "%", false),
    choice("voices", "Voices", &["1", "2", "3", "4"], 1),
    float("spread", "Spread", 0.0, 180.0, 90.0, "°", false),
    choice("stages", "Stages", &["2", "4", "6", "8", "10", "12"], 1),
    float("mix", "Mix", 0.0, 100.0, 50.0, "%", false),
    float("output", "Output", -24.0, 12.0, 0.0, "dB", false),
];

const COUNT: usize = 14;

const MODE: usize = 0;
const SYNC: usize = 1;
const RATE: usize = 2;
const NOTE: usize = 3;
const SHAPE: usize = 4;
const DEPTH: usize = 5;
const DELAY: usize = 6;
const CENTER: usize = 7;
const FEEDBACK: usize = 8;
const VOICES: usize = 9;
const SPREAD: usize = 10;
const STAGES: usize = 11;
const MIX: usize = 12;
const OUTPUT: usize = 13;

/// The most chorus voices and phaser stages per side.
const MAX_VOICES: usize = 4;
const MAX_STAGES: usize = 12;
/// How far a chorus voice's delay swings either way at full depth, as a
/// share of the delay...
const CHORUS_SWING: f32 = 0.5;
/// ...and the flanger's: all the way down to nothing.
const FLANGER_SWING: f32 = 1.0;
/// How many octaves the phaser's corner moves either way of the center at
/// full depth.
const PHASER_OCTAVES: f32 = 3.0;
/// The lowest corner the phaser's stages are tuned to, and the highest as a
/// fraction of the sample rate.
const PHASER_LOWEST: f32 = 20.0;
const PHASER_HIGHEST: f32 = 0.45;
/// The shortest delay read, in samples: the cubic needs a sample either side.
const MIN_DELAY: f32 = 2.0;
/// The longest delay the lines hold: the longest `delay`, swung all the way
/// by the flanger.
const LONGEST_MS: f32 = 40.0 * (1.0 + FLANGER_SWING);
/// The fastest the delay may change, in samples per sample: how far the
/// pitch of a swept voice may bend up or down, as a share.
const MAX_SLOPE: f32 = 0.35;
/// The fastest any voice's delay may change, LFO and parameters together,
/// in samples per sample.
const MAX_TAP_SLOPE: f32 = 0.5;
/// The slowest rate the slope limit is worked out for.
const MIN_RATE: f32 = 1e-3;
/// How long the LFO takes to catch up with the song (a time constant) when
/// it finds itself somewhere else.
const CATCH_UP_MS: f32 = 100.0;
/// The high-pass in the feedback loops, against DC building up.
const DC_HZ: f32 = 10.0;
/// Feedback louder than this is bent down smoothly, to twice it at most.
const CEILING: f32 = 8.0;
/// How long feedback, mix, output and the phaser's center take to reach a
/// new value.
const SMOOTH_MS: f32 = 20.0;
/// How long the delay and depth glide: moving them bends the pitch, so
/// gently, like turning a tape flanger's knob.
const DELAY_GLIDE_MS: f32 = 80.0;
/// How long the voices and the spread take to move through the cycle.
const PHASE_GLIDE_MS: f32 = 150.0;
/// How long switching the mode, the shape or the stages crossfades.
const FADE_MS: f32 = 30.0;
/// How long the LFO takes to reach a new rate.
const RATE_GLIDE_MS: f32 = 50.0;
/// Tempo used when the one given makes no sense.
const FALLBACK_BPM: f64 = 120.0;

/// The fractional part of `x`, which is at least 0: cheaper than `floor`,
/// which is a library call on plain x86-64.
#[inline]
fn wrap(x: f32) -> f32 {
    x - x as i32 as f32
}

/// `sin(2π · phase)` for `phase` at least 0, to within a few millionths:
/// a Taylor series over the quarter cycle either side of 0, the rest folded
/// onto it. Several times faster than `sin`, which is a library call.
#[inline]
fn sine(phase: f32) -> f32 {
    // -0.5 to 0.5 of a cycle, then -0.25 to 0.25.
    let u = phase - (phase + 0.5) as i32 as f32;
    let u = if u > 0.25 {
        0.5 - u
    } else if u < -0.25 {
        -0.5 - u
    } else {
        u
    };
    let x = TAU * u;
    let x2 = x * x;
    x * (1.0
        + x2 * (-1.0 / 6.0 + x2 * (1.0 / 120.0 + x2 * (-1.0 / 5040.0 + x2 * (1.0 / 362_880.0)))))
}

/// The LFO's value (-1 to 1) `phase` cycles in (at least 0): a sine blended
/// into a triangle by `triangle` (0 to 1). Both start at 0, rising.
#[inline]
fn wave(phase: f32, triangle: f32) -> f32 {
    let sine = if triangle < 1.0 { sine(phase) } else { 0.0 };
    if triangle <= 0.0 {
        return sine;
    }
    let straight = 4.0 * (wrap(phase + 0.75) - 0.5).abs() - 1.0;
    sine + triangle * (straight - sine)
}

/// The tempo to work with: `bpm`, or a sensible one if it makes no sense.
fn tempo(bpm: f64) -> f64 {
    if bpm.is_finite() && bpm > 0.0 {
        bpm.clamp(1.0, 1000.0)
    } else {
        FALLBACK_BPM
    }
}

/// How many quarter notes one LFO cycle lasts in tempo sync, for the
/// `note` parameter's value.
fn cycle_beats(note: f32) -> f64 {
    NOTES[(note.max(0.0) as usize).min(NOTES.len() - 1)].1
}

/// The largest swing (samples either way) an LFO at `rate` Hz may move a
/// delay by without changing it faster than [`MAX_SLOPE`].
#[inline]
fn swing_limit(rate: f32, sample_rate: f32) -> f32 {
    MAX_SLOPE * sample_rate / (TAU * rate.max(MIN_RATE))
}

/// The phaser's corner in Hz with its center at `log_center` (log2 Hz) and
/// the LFO at `lfo`, scaled by `depth` (0 to 1).
#[inline]
fn corner(log_center: f32, depth: f32, lfo: f32, sample_rate: f32) -> f32 {
    (log_center + depth * PHASER_OCTAVES * lfo)
        .exp2()
        .max(PHASER_LOWEST)
        .min(sample_rate * PHASER_HIGHEST)
}

/// The share of the wet sound kept at feedback `feedback` (-1 to 1).
#[inline]
fn compensation(feedback: f32) -> f32 {
    (1.0 - feedback * feedback).max(0.0).sqrt()
}

/// A first-order all-pass in trapezoidal (TPT) form: the one-pole low-pass
/// `lp`, and `2·lp - x`.
#[derive(Debug, Clone, Copy, Default)]
struct AllPass {
    state: f32,
}

impl AllPass {
    /// One sample through, with `gain` = `g / (1 + g)` for the prewarped
    /// corner `g`.
    #[inline]
    fn tick(&mut self, x: f32, gain: f32) -> f32 {
        2.0 * tpt_lowpass(&mut self.state, gain, x) - x
    }
}

/// Parameter `index`'s value from `values` (in [`DESCRIPTOR`]'s order), or
/// its default where it's missing or NaN.
fn value_of(values: &[f32], index: usize) -> f32 {
    crate::param_value(PARAMS, values, index)
}

/// The LFO's rate in Hz at these parameter values and tempo.
pub fn lfo_rate(values: &[f32], bpm: f64) -> f64 {
    if value_of(values, SYNC) < 0.5 {
        tempo(bpm) / 60.0 / cycle_beats(value_of(values, NOTE))
    } else {
        f64::from(value_of(values, RATE))
    }
}

/// Where the sweep reaches at these parameter values (in [`DESCRIPTOR`]'s
/// order) with the LFO at `rate` Hz, from one end to the other: the delay
/// in ms for the chorus and the flanger, the phaser's corner in Hz.
pub fn sweep(values: &[f32], rate: f32, sample_rate: f32) -> (f32, f32) {
    let mode = value_of(values, MODE) as usize;
    let depth = value_of(values, DEPTH) / 100.0;
    if mode == PHASER {
        let log_center = value_of(values, CENTER).log2();
        return (
            corner(log_center, depth, -1.0, sample_rate),
            corner(log_center, depth, 1.0, sample_rate),
        );
    }
    let per_ms = sample_rate / 1000.0;
    let delay = (value_of(values, DELAY) * per_ms).max(MIN_DELAY);
    let share = if mode == CHORUS {
        CHORUS_SWING
    } else {
        FLANGER_SWING
    };
    let swing = (delay * share * depth).min(swing_limit(rate, sample_rate));
    (
        (delay - swing).max(MIN_DELAY) / per_ms,
        (delay + swing) / per_ms,
    )
}

/// The gain of one side at `freq` Hz, dry and wet together (before the
/// output level), with that side's first voice's LFO `phase` cycles in and
/// the LFO running at `rate` Hz, at these parameter values: what the panel
/// draws. The cubic's slight loss of highs between samples and the
/// feedback loops' high-pass are left out.
pub fn response(values: &[f32], phase: f32, rate: f32, freq: f32, sample_rate: f32) -> f32 {
    // Complex numbers as (re, im), in double precision.
    let mul = |a: (f64, f64), b: (f64, f64)| (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0);
    let div = |a: (f64, f64), b: (f64, f64)| {
        let d = b.0 * b.0 + b.1 * b.1;
        ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
    };
    let omega = std::f64::consts::TAU * f64::from(freq.max(0.0)) / f64::from(sample_rate);
    let delayed = |samples: f32| {
        let angle = -omega * f64::from(samples);
        (angle.cos(), angle.sin())
    };

    let phase = if phase.is_finite() {
        phase.rem_euclid(1.0)
    } else {
        0.0
    };
    let mode = value_of(values, MODE) as usize;
    let triangle = value_of(values, SHAPE);
    let depth = value_of(values, DEPTH) / 100.0;
    let feedback = value_of(values, FEEDBACK) / 100.0;
    let mix = f64::from(value_of(values, MIX) / 100.0);
    let delay = (value_of(values, DELAY) * sample_rate / 1000.0).max(MIN_DELAY);
    let limit = swing_limit(rate, sample_rate);
    let wet = match mode {
        CHORUS => {
            let voices = value_of(values, VOICES) + 1.0;
            let swing = (delay * CHORUS_SWING * depth).min(limit);
            let mut sum = (0.0, 0.0);
            for k in 0..voices as usize {
                let lfo = wave(phase + k as f32 / voices, triangle);
                let z = delayed((delay + swing * lfo).max(MIN_DELAY));
                sum = (sum.0 + z.0, sum.1 + z.1);
            }
            let norm = 1.0 / f64::from(voices).sqrt();
            (sum.0 * norm, sum.1 * norm)
        }
        FLANGER => {
            let swing = (delay * FLANGER_SWING * depth).min(limit);
            let z = delayed((delay + swing * wave(phase, triangle)).max(MIN_DELAY));
            let g = f64::from(feedback);
            let c = f64::from(compensation(feedback));
            div((c * z.0, c * z.1), (1.0 - g * z.0, -g * z.1))
        }
        _ => {
            let stages = 2 * (value_of(values, STAGES) as i32 + 1);
            let lfo = wave(phase, triangle);
            let hz = corner(value_of(values, CENTER).log2(), depth, lfo, sample_rate);
            // Each stage is (1 - j·t/g) / (1 + j·t/g), with t and g the
            // prewarped frequency and corner (t kept finite at Nyquist).
            let ratio = (omega / 2.0)
                .min(0.9999 * std::f64::consts::FRAC_PI_2)
                .tan()
                / (std::f64::consts::PI * f64::from(hz) / f64::from(sample_rate)).tan();
            let stage = div((1.0, -ratio), (1.0, ratio));
            let mut chain = (1.0, 0.0);
            for _ in 0..stages {
                chain = mul(chain, stage);
            }
            let g = f64::from(feedback);
            let c = f64::from(compensation(feedback));
            let looped = mul(chain, delayed(1.0));
            div(
                (c * chain.0, c * chain.1),
                (1.0 - g * looped.0, -g * looped.1),
            )
        }
    };
    let re = 1.0 - mix + mix * wet.0;
    let im = mix * wet.1;
    (re * re + im * im).sqrt() as f32
}

pub struct Chorus {
    values: Values<COUNT>,
    sample_rate: f32,
    /// Left and right lines of what came in, with the flanger's feedback.
    lines: [Ring; 2],
    /// The longest delay the lines can be read at, in samples.
    longest: f32,
    /// Where the LFO is for the left side's first voice, in cycles (0 to 1).
    phase: f64,
    /// Whether the LFO has run since `prepare` or `reset`; until it has, it
    /// takes the song's position and its rate at once rather than gliding.
    placed: bool,
    /// The share of the way to the song's position the LFO moves each sample.
    catch_up: f64,
    /// The pole of the feedback loops' high-pass.
    dc_pole: f32,
    /// How much each mode is heard: 1 for the one chosen, 0 for the others;
    /// in between while switching.
    modes: [Smoothed; MODES],
    /// 0 for a sine, 1 for a triangle; in between while switching.
    triangle: Smoothed,
    /// The LFO's rate in Hz, and how fast it follows its parameter.
    rate: Smoothed,
    rate_pace: Pace,
    /// 0 to 1.
    depth: Smoothed,
    /// The middle of the sweep, in ms.
    delay: Smoothed,
    /// The phaser's center, log2 Hz.
    log_center: Smoothed,
    /// -0.95 to 0.95.
    feedback: Smoothed,
    /// How many chorus voices per side (1 to 4), fractional while fading.
    voices: Smoothed,
    /// The right side's LFO offset, in cycles (0 to 0.5).
    spread: Smoothed,
    /// How many phaser stages (2 to 12), fractional while fading.
    stages: Smoothed,
    mix: Smoothed,
    /// The output level, as a gain.
    output: Smoothed,
    /// Per side: the flanger's and the phaser's feedback high-passes.
    flanger_dc: [DcBlocker; 2],
    phaser_dc: [DcBlocker; 2],
    /// Per side: the phaser's stages, and its last output, fed back.
    all_passes: [[AllPass; MAX_STAGES]; 2],
    phaser_out: [f32; 2],
    /// Per side, where each chorus voice and the flanger read their lines
    /// last sample, in samples, so none moves faster than
    /// [`MAX_TAP_SLOPE`]; and whether each mode's have been placed since
    /// it last ran (until then they go straight where they're wanted).
    voice_taps: [[f32; MAX_VOICES]; 2],
    flanger_taps: [f32; 2],
    voices_placed: bool,
    flanger_placed: bool,
    /// What [`Effect::meters`] reports.
    readings: [f32; 3],
}

impl Chorus {
    pub fn new() -> Chorus {
        let mut chorus = Chorus {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            lines: [Ring::default(), Ring::default()],
            longest: MIN_DELAY,
            phase: 0.0,
            placed: false,
            catch_up: 1.0,
            dc_pole: 0.0,
            modes: [
                Smoothed::new(0.0, FADE_MS),
                Smoothed::new(0.0, FADE_MS),
                Smoothed::new(0.0, FADE_MS),
            ],
            triangle: Smoothed::new(0.0, FADE_MS),
            rate: Smoothed::new(1.0, RATE_GLIDE_MS),
            rate_pace: Pace::Knob,
            depth: Smoothed::new(0.0, DELAY_GLIDE_MS),
            delay: Smoothed::new(1.0, DELAY_GLIDE_MS),
            log_center: Smoothed::new(10.0, SMOOTH_MS),
            feedback: Smoothed::new(0.0, SMOOTH_MS),
            voices: Smoothed::new(1.0, PHASE_GLIDE_MS),
            spread: Smoothed::new(0.0, PHASE_GLIDE_MS),
            stages: Smoothed::new(2.0, FADE_MS),
            mix: Smoothed::new(0.0, SMOOTH_MS),
            output: Smoothed::new(1.0, SMOOTH_MS),
            flanger_dc: [DcBlocker::default(); 2],
            phaser_dc: [DcBlocker::default(); 2],
            all_passes: [[AllPass::default(); MAX_STAGES]; 2],
            phaser_out: [0.0; 2],
            voice_taps: [[0.0; MAX_VOICES]; 2],
            flanger_taps: [0.0; 2],
            voices_placed: false,
            flanger_placed: false,
            readings: [0.0; 3],
        };
        for index in 0..COUNT {
            chorus.apply(index, Pace::Knob);
        }
        chorus.prepare_smoothers();
        chorus.reset();
        chorus
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`. Switches (mode, shape, voices, stages) always crossfade.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            MODE => {
                for (mode, weight) in self.modes.iter_mut().enumerate() {
                    weight.set_target(if mode == value as usize { 1.0 } else { 0.0 });
                }
            }
            SHAPE => self.triangle.set_target(value),
            DEPTH => self.depth.move_to(value / 100.0, pace),
            DELAY => self.delay.move_to(value, pace),
            CENTER => self.log_center.move_to(value.log2(), pace),
            FEEDBACK => self.feedback.move_to(value / 100.0, pace),
            VOICES => self.voices.set_target(value + 1.0),
            SPREAD => self.spread.move_to(value / 360.0, pace),
            STAGES => self.stages.set_target(2.0 * (value + 1.0)),
            MIX => self.mix.move_to(value / 100.0, pace),
            OUTPUT => self.output.move_to(dsp::db_to_gain(value), pace),
            // The rate is worked out each block, as it depends on the
            // tempo; this is how fast it follows.
            RATE => self.rate_pace = pace,
            _ => self.rate_pace = Pace::Knob,
        }
    }

    fn smoothers(&mut self) -> impl Iterator<Item = &mut Smoothed> {
        let [chorus, flanger, phaser] = &mut self.modes;
        [
            chorus,
            flanger,
            phaser,
            &mut self.triangle,
            &mut self.rate,
            &mut self.depth,
            &mut self.delay,
            &mut self.log_center,
            &mut self.feedback,
            &mut self.voices,
            &mut self.spread,
            &mut self.stages,
            &mut self.mix,
            &mut self.output,
        ]
        .into_iter()
    }

    fn prepare_smoothers(&mut self) {
        let sample_rate = self.sample_rate;
        for smoothed in self.smoothers() {
            smoothed.prepare(sample_rate);
        }
        self.catch_up =
            1.0 - (-1.0 / (f64::from(CATCH_UP_MS) * 0.001 * f64::from(sample_rate))).exp();
        self.dc_pole = pole(DC_HZ, sample_rate);
    }

    /// Where the right side's first voice is in the LFO's cycle, ahead of
    /// the left's, in the mode that's chosen.
    fn right_offset(&self) -> f32 {
        let spread = self.spread.current();
        if self.values[MODE] as usize == CHORUS {
            spread / self.voices.current()
        } else {
            spread
        }
    }
}

impl Default for Chorus {
    fn default() -> Self {
        Chorus::new()
    }
}

impl Effect for Chorus {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        let len = (LONGEST_MS * sample_rate / 1000.0).ceil() as usize + 4;
        for line in &mut self.lines {
            line.allocate(len);
        }
        self.longest = len as f32 - 3.0;
        self.prepare_smoothers();
        self.reset();
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
        for smoothed in self.smoothers() {
            smoothed.settle();
        }
        self.phase = 0.0;
        self.placed = false;
        self.flanger_dc = [DcBlocker::default(); 2];
        self.phaser_dc = [DcBlocker::default(); 2];
        self.all_passes = [[AllPass::default(); MAX_STAGES]; 2];
        self.phaser_out = [0.0; 2];
        self.voices_placed = false;
        self.flanger_placed = false;
        self.readings = [0.0; 3];
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: &Context) {
        self.values.played();
        if self.lines[0].len() <= 1 {
            // Not prepared: pass the sound through.
            return;
        }
        let rate = lfo_rate(&self.values, context.bpm) as f32;
        if !self.placed {
            self.rate.set_immediate(rate);
        } else if rate != self.rate.target() {
            self.rate.move_to(rate, self.rate_pace);
        }
        // Locked to the song while it rolls: its position, in cycles, at the
        // block's start, and how far it moves each sample.
        let lock =
            (self.values[SYNC] < 0.5 && context.playing && context.beat.is_finite()).then(|| {
                let cycle = cycle_beats(self.values[NOTE]);
                let per_sample = tempo(context.bpm) / 60.0 / cycle / f64::from(self.sample_rate);
                ((context.beat / cycle).rem_euclid(1.0), per_sample)
            });

        let sample_rate = self.sample_rate;
        let per_ms = sample_rate / 1000.0;
        let longest = self.longest;
        let dc_pole = self.dc_pole;
        let len = left.len().min(right.len());
        let mut most_stages = 0;
        for n in 0..len {
            // The LFO, caught up with the song if it's locked to it.
            let rate = self.rate.tick();
            self.phase += f64::from(rate / sample_rate);
            if let Some((start, per_sample)) = lock {
                let song = start + n as f64 * per_sample;
                if self.placed {
                    let ahead = song - self.phase;
                    let behind = ahead - (ahead + 0.5).floor();
                    self.phase += behind * self.catch_up;
                } else {
                    self.phase = song;
                }
            }
            // Back into 0 to 1: it moves less than a cycle a sample.
            if !(0.0..1.0).contains(&self.phase) {
                self.phase -= self.phase.floor();
            }
            self.placed = true;
            let phase = self.phase as f32;

            let weights = [
                self.modes[CHORUS].tick(),
                self.modes[FLANGER].tick(),
                self.modes[PHASER].tick(),
            ];
            let triangle = self.triangle.tick();
            let depth = self.depth.tick();
            let delay = (self.delay.tick() * per_ms).max(MIN_DELAY);
            let log_center = self.log_center.tick();
            let feedback = self.feedback.tick();
            let voices = self.voices.tick();
            let spread = self.spread.tick();
            let stages = self.stages.tick();
            let mix = self.mix.tick();
            let output = self.output.tick();
            let limit = swing_limit(rate, sample_rate);
            let kept = compensation(feedback);
            let dry = [left[n], right[n]];
            let mut wet = [0.0f32; 2];

            if weights[CHORUS] > 0.0 {
                let placed = self.voices_placed;
                let swing = (delay * CHORUS_SWING * depth).min(limit);
                let gain = weights[CHORUS] / voices.sqrt();
                let apart = 1.0 / voices;
                for (side, line) in self.lines.iter().enumerate() {
                    let first = phase + side as f32 * spread * apart;
                    let mut sum = 0.0;
                    for k in 0..MAX_VOICES {
                        // Voices fade in and out one after another.
                        let level = (voices - k as f32).min(1.0);
                        if level <= 0.0 {
                            break;
                        }
                        let lfo = wave(first + k as f32 * apart, triangle);
                        let wanted = (delay + swing * lfo).max(MIN_DELAY).min(longest);
                        let at = slew(&mut self.voice_taps[side][k], wanted, placed, MAX_TAP_SLOPE)
                            .clamp(MIN_DELAY, longest);
                        sum += level * line.delayed(at);
                    }
                    wet[side] += gain * sum;
                }
            }

            // What the flanger feeds back into each line.
            let mut back = [0.0f32; 2];
            if weights[FLANGER] > 0.0 {
                let swing = (delay * FLANGER_SWING * depth).min(limit);
                for (side, line) in self.lines.iter().enumerate() {
                    let lfo = wave(phase + side as f32 * spread, triangle);
                    let wanted = (delay + swing * lfo).max(MIN_DELAY).min(longest);
                    let at = slew(
                        &mut self.flanger_taps[side],
                        wanted,
                        self.flanger_placed,
                        MAX_TAP_SLOPE,
                    )
                    .clamp(MIN_DELAY, longest);
                    let tap = line.delayed(at);
                    wet[side] += weights[FLANGER] * kept * tap;
                    let blocked = self.flanger_dc[side].tick(tap, dc_pole);
                    back[side] = contain(weights[FLANGER] * feedback * blocked, CEILING);
                }
            } else {
                self.flanger_dc = [DcBlocker::default(); 2];
            }
            for (side, line) in self.lines.iter_mut().enumerate() {
                line.push(settle(dry[side] + back[side]));
            }

            if weights[PHASER] > 0.0 {
                // Between two stage counts while switching: the outputs
                // after each, blended.
                let fewer = stages.floor();
                let blend = stages - fewer;
                let (fewer, more) = (fewer as usize, stages.ceil() as usize);
                most_stages = most_stages.max(more);
                for side in 0..2 {
                    let lfo = wave(phase + side as f32 * spread, triangle);
                    let hz = corner(log_center, depth, lfo, sample_rate);
                    let g = (PI * hz / sample_rate).tan();
                    let gain = g / (1.0 + g);
                    let returned = self.phaser_dc[side].tick(self.phaser_out[side], dc_pole);
                    let mut x = dry[side] + feedback * returned;
                    let mut after_fewer = x;
                    for (i, stage) in self.all_passes[side][..more].iter_mut().enumerate() {
                        x = stage.tick(x, gain);
                        if i + 1 == fewer {
                            after_fewer = x;
                        }
                    }
                    let out = after_fewer + blend * (x - after_fewer);
                    self.phaser_out[side] = flush(contain(out, CEILING));
                    wet[side] += weights[PHASER] * kept * out;
                }
            }

            left[n] = output * (dry[0] + mix * (wet[0] - dry[0]));
            right[n] = output * (dry[1] + mix * (wet[1] - dry[1]));
            // Taps that aren't running are placed afresh when they start.
            self.voices_placed = weights[CHORUS] > 0.0;
            self.flanger_placed = weights[FLANGER] > 0.0;
        }

        // Stages that aren't running start from silence when they are again;
        // so does the whole phaser.
        for stages in &mut self.all_passes {
            for (i, stage) in stages.iter_mut().enumerate() {
                stage.state = if i < most_stages {
                    settle(stage.state)
                } else {
                    0.0
                };
            }
        }
        if most_stages == 0 {
            self.phaser_out = [0.0; 2];
            self.phaser_dc = [DcBlocker::default(); 2];
        }
        for out in &mut self.phaser_out {
            *out = settle(*out);
        }
        for block in self.flanger_dc.iter_mut().chain(&mut self.phaser_dc) {
            block.flush();
        }
        self.readings = [
            self.phase as f32,
            (self.phase as f32 + self.right_offset()).rem_euclid(1.0),
            self.rate.current(),
        ];
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
        out[..3].copy_from_slice(&self.readings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, db, noise, peak, rms, run, set, sine, values};

    fn chorus(settings: &[(&str, f32)]) -> Chorus {
        let mut chorus = Chorus::new();
        chorus.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut chorus, id, value);
        }
        chorus.reset();
        chorus
    }

    /// `input` in both channels through `chorus`, in 64-sample blocks.
    fn stereo(chorus: &mut Chorus, input: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let (mut left, mut right) = (input.to_vec(), input.to_vec());
        run(chorus, &mut left, &mut right, 64, &Context::DEFAULT);
        (left, right)
    }

    /// The level of a sine at `freq` Hz through `chorus` (settled), in dB
    /// against the input, measured over the last 4800 samples of a second.
    fn sine_gain(chorus: &mut Chorus, freq: f64) -> f64 {
        let input = sine(freq, 0.5, 48_000, RATE);
        let (left, _) = stereo(chorus, &input);
        db(rms(&left[43_200..]) / rms(&input[43_200..]))
    }

    /// `|H|` of an impulse response at `freq` Hz, by its discrete Fourier
    /// transform there.
    fn magnitude(impulse: &[f32], freq: f64) -> f64 {
        let step = std::f64::consts::TAU * freq / f64::from(RATE);
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &s) in impulse.iter().enumerate() {
            re += f64::from(s) * (step * n as f64).cos();
            im -= f64::from(s) * (step * n as f64).sin();
        }
        (re * re + im * im).sqrt()
    }

    #[test]
    fn parameters_are_laid_out_as_documented() {
        assert_eq!(PARAMS.len(), COUNT);
        let ids = [
            (MODE, "mode"),
            (SYNC, "sync"),
            // (`RATE` here is the test sample rate.)
            (super::RATE, "rate"),
            (NOTE, "note"),
            (SHAPE, "shape"),
            (DEPTH, "depth"),
            (DELAY, "delay"),
            (CENTER, "center"),
            (FEEDBACK, "feedback"),
            (VOICES, "voices"),
            (SPREAD, "spread"),
            (STAGES, "stages"),
            (MIX, "mix"),
            (OUTPUT, "output"),
        ];
        for (index, id) in ids {
            assert_eq!(PARAMS[index].id, id);
        }
        assert_eq!(NOTE_LABELS.len(), NOTES.len());
        for (label, (name, _)) in NOTE_LABELS.iter().zip(NOTES) {
            assert_eq!(*label, name);
        }
        assert_eq!(NOTES[PARAMS[NOTE].default_value() as usize].0, "1 bar");
    }

    #[test]
    fn the_lfo_is_a_sine_or_a_triangle_in_phase() {
        for i in 0..=100 {
            let phase = i as f32 / 100.0;
            let sine = wave(phase, 0.0);
            assert!((sine - (TAU * phase).sin()).abs() < 1e-5);
            let triangle = wave(phase, 1.0);
            assert!((-1.0..=1.0).contains(&triangle));
            // Same sign, same peaks.
            assert!(sine * triangle >= -1e-5, "{phase}: {sine} {triangle}");
        }
        assert!((wave(0.25, 1.0) - 1.0).abs() < 1e-6 && (wave(0.75, 1.0) + 1.0).abs() < 1e-6);
        assert!((wave(0.125, 1.0) - 0.5).abs() < 1e-6);
        assert!((wave(1.3, 0.5) - wave(0.3, 0.5)).abs() < 1e-5);
    }

    #[test]
    fn mix_zero_passes_the_input_unchanged() {
        for mode in [CHORUS, FLANGER, PHASER] {
            let mut chorus = chorus(&[
                ("mode", mode as f32),
                ("mix", 0.0),
                ("feedback", 90.0),
                ("depth", 100.0),
            ]);
            let input = noise(9600, 4);
            let mut right: Vec<f32> = input.iter().map(|s| -0.5 * s).collect();
            let expected_right = right.clone();
            let mut left = input.clone();
            run(&mut chorus, &mut left, &mut right, 64, &Context::DEFAULT);
            assert_eq!(left, input, "mode {mode}");
            assert_eq!(right, expected_right, "mode {mode}");
        }
    }

    #[test]
    fn the_chorus_widens_a_mono_input() {
        let input = noise(48_000, 7);
        let correlation = |left: &[f32], right: &[f32]| {
            let dot: f64 = left
                .iter()
                .zip(right)
                .map(|(&l, &r)| f64::from(l) * f64::from(r))
                .sum();
            dot / (rms(left) * rms(right) * left.len() as f64)
        };
        for voices in 0..4 {
            let mut wide = chorus(&[("voices", voices as f32), ("mix", 100.0)]);
            let (left, right) = stereo(&mut wide, &input);
            let side = rms(&left
                .iter()
                .zip(&right)
                .map(|(l, r)| l - r)
                .collect::<Vec<_>>());
            let c = correlation(&left[4800..], &right[4800..]);
            println!(
                "{} voices: side {:.1} dB, correlation {c:.2}",
                voices + 1,
                db(side / rms(&input))
            );
            assert!(db(side / rms(&input)) > -12.0, "{} voices", voices + 1);
            assert!(c < 0.9, "{} voices: correlation {c}", voices + 1);
        }
        // No spread: both sides the same.
        let mut narrow = chorus(&[("spread", 0.0)]);
        let (left, right) = stereo(&mut narrow, &input);
        assert_eq!(left, right);
    }

    #[test]
    fn a_chorus_voice_bends_the_pitch_a_little() {
        // Depth 50% of a 10 ms delay at 0.8 Hz: ±2.5 ms, so about ±1.3% in
        // pitch (2π · 0.8 Hz · 2.5 ms).
        let (shortest, longest) = sweep(&values(PARAMS, &[]), 0.8, RATE);
        assert!((shortest - 7.5).abs() < 1e-3 && (longest - 12.5).abs() < 1e-3);
        // A 1 kHz sine through one voice alone: its cycles, timed between
        // upward zero crossings, last 1 ms give or take that much.
        let mut wet = chorus(&[("mix", 100.0), ("voices", 0.0)]);
        let input = sine(1000.0, 0.5, 96_000, RATE);
        let (left, _) = stereo(&mut wet, &input);
        let crossings: Vec<f64> = (48_000..left.len())
            .filter(|&n| left[n - 1] <= 0.0 && left[n] > 0.0)
            .map(|n| {
                let (a, b) = (f64::from(left[n - 1]), f64::from(left[n]));
                n as f64 - 1.0 + a / (a - b)
            })
            .collect();
        let periods: Vec<f64> = crossings.windows(2).map(|w| w[1] - w[0]).collect();
        let shortest = periods.iter().copied().fold(f64::INFINITY, f64::min);
        let longest = periods.iter().copied().fold(0.0, f64::max);
        println!("periods {shortest:.3} to {longest:.3} samples");
        let bend = std::f64::consts::TAU * 0.8 * 0.0025;
        assert!((shortest / 48.0 - 1.0 + bend).abs() < 0.002, "{shortest}");
        assert!((longest / 48.0 - 1.0 - bend).abs() < 0.002, "{longest}");
    }

    #[test]
    fn the_flanger_is_a_comb_whose_notches_move() {
        // Depth 0, a 2 ms (96 sample) delay, half wet: notches at 250 Hz and
        // its odd multiples, peaks at 500 Hz and its multiples.
        let still = [
            ("mode", FLANGER as f32),
            ("delay", 2.0),
            ("depth", 0.0),
            ("mix", 50.0),
        ];
        for (freq, expected) in [
            (250.0, f64::NEG_INFINITY),
            (500.0, 0.0),
            (750.0, f64::NEG_INFINITY),
        ] {
            let gain = sine_gain(&mut chorus(&still), freq);
            println!("{freq} Hz: {gain:.1} dB");
            if expected == 0.0 {
                assert!(gain.abs() < 0.1, "{freq} Hz: {gain}");
            } else {
                assert!(gain < -40.0, "{freq} Hz: {gain}");
            }
        }
        // Swept: a steady 1 kHz rises and falls as the notches pass it.
        let mut swept = chorus(&[
            ("mode", FLANGER as f32),
            ("delay", 2.0),
            ("depth", 80.0),
            ("rate", 0.5),
            ("mix", 50.0),
        ]);
        let input = sine(1000.0, 0.5, 96_000, RATE);
        let (left, _) = stereo(&mut swept, &input);
        let levels: Vec<f64> = left
            .chunks(960)
            .map(|chunk| db(rms(chunk) / (0.5 / std::f64::consts::SQRT_2)))
            .collect();
        let (lowest, highest) = levels
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &l| {
                (lo.min(l), hi.max(l))
            });
        println!("1 kHz through the sweep: {lowest:.1} to {highest:.1} dB");
        assert!(lowest < -20.0 && highest > -1.0, "{lowest} {highest}");
        // Several notches pass by in the two seconds.
        let dips = levels
            .windows(3)
            .filter(|w| w[1] < w[0] && w[1] <= w[2] && w[1] < -12.0)
            .count();
        assert!(dips >= 4, "{dips} dips: {levels:.0?}");
    }

    #[test]
    fn feedback_deepens_the_comb_both_ways() {
        // Positive feedback puts resonances on the peaks (500 Hz for 2 ms),
        // negative on the notches (250 Hz).
        let at = |feedback: f32, freq: f64| {
            sine_gain(
                &mut chorus(&[
                    ("mode", FLANGER as f32),
                    ("delay", 2.0),
                    ("depth", 0.0),
                    ("feedback", feedback),
                    ("mix", 100.0),
                ]),
                freq,
            )
        };
        let expected = |feedback: f64| db(compensation(feedback as f32) as f64 / (1.0 - feedback));
        let positive = at(80.0, 500.0);
        let negative = at(-80.0, 250.0);
        println!("+80% at 500 Hz {positive:.2} dB, -80% at 250 Hz {negative:.2} dB");
        assert!((positive - expected(0.8)).abs() < 0.3, "{positive}");
        assert!((negative - expected(0.8)).abs() < 0.3, "{negative}");
        // And the curve the panel draws agrees.
        let drawn = |feedback: f32, freq: f32| {
            let values = values(
                PARAMS,
                &[
                    ("mode", FLANGER as f32),
                    ("delay", 2.0),
                    ("depth", 0.0),
                    ("feedback", feedback),
                    ("mix", 100.0),
                ],
            );
            db(f64::from(response(&values, 0.0, 0.8, freq, RATE)))
        };
        assert!((drawn(80.0, 500.0) - positive).abs() < 0.3);
        assert!((drawn(-80.0, 250.0) - negative).abs() < 0.3);
    }

    #[test]
    fn the_phaser_makes_half_as_many_notches_as_stages() {
        // Depth 0 holds the sweep still, so the impulse response tells all.
        for (index, stages) in [2, 4, 6, 8, 10, 12].into_iter().enumerate() {
            let settings = [
                ("mode", PHASER as f32),
                ("stages", index as f32),
                ("depth", 0.0),
                ("center", 1000.0),
                ("mix", 50.0),
            ];
            let mut phaser = chorus(&settings);
            let mut impulse = vec![0.0; 16_384];
            impulse[0] = 1.0;
            let (left, _) = stereo(&mut phaser, &impulse);
            // 20 Hz to 20 kHz, 24 points an octave.
            let freqs: Vec<f64> = (0..240)
                .map(|i| 20.0 * 2f64.powf(i as f64 / 24.0))
                .collect();
            let gains: Vec<f64> = freqs.iter().map(|&f| db(magnitude(&left, f))).collect();
            let notches: Vec<f64> = (1..gains.len() - 1)
                .filter(|&i| {
                    gains[i] < gains[i - 1] && gains[i] <= gains[i + 1] && gains[i] < -12.0
                })
                .map(|i| freqs[i])
                .collect();
            println!("{stages} stages: notches at {notches:.0?} Hz");
            assert_eq!(notches.len(), stages / 2, "{stages} stages: {notches:?}");
            // The drawn response agrees.
            let values = values(PARAMS, &settings);
            for (&f, &gain) in freqs.iter().zip(&gains).step_by(7) {
                let drawn = db(f64::from(response(&values, 0.0, 0.8, f as f32, RATE)));
                if gain > -30.0 {
                    assert!(
                        (drawn - gain).abs() < 0.5,
                        "{stages} stages, {f} Hz: {drawn} {gain}"
                    );
                }
            }
        }
        // Two stages: the one notch is at the center.
        let mut two = chorus(&[
            ("mode", PHASER as f32),
            ("stages", 0.0),
            ("depth", 0.0),
            ("center", 1000.0),
        ]);
        assert!(sine_gain(&mut two, 1000.0) < -40.0);
    }

    #[test]
    fn the_phaser_sweeps_its_notches() {
        let mut phaser = chorus(&[("mode", PHASER as f32), ("depth", 80.0), ("rate", 1.0)]);
        let input = sine(800.0, 0.5, 96_000, RATE);
        let (left, _) = stereo(&mut phaser, &input);
        let levels: Vec<f64> = left
            .chunks(960)
            .map(|chunk| db(rms(chunk) / (0.5 / std::f64::consts::SQRT_2)))
            .collect();
        let lowest = levels.iter().copied().fold(f64::INFINITY, f64::min);
        let highest = levels.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        println!("800 Hz through the phaser: {lowest:.1} to {highest:.1} dB");
        assert!(lowest < -20.0 && highest > -1.0);
    }

    #[test]
    fn tempo_sync_locks_the_lfo_to_the_beat() {
        // A quarter-note cycle at 120 BPM: 24000 samples.
        let synced = [
            ("sync", 0.0),
            ("note", 8.0),
            ("mode", FLANGER as f32),
            ("depth", 100.0),
        ];
        let play = |chorus: &mut Chorus, from_beat: f64, blocks: usize| {
            let input = noise(blocks * 64, 11);
            let (mut left, mut right) = (input.clone(), input);
            let per_sample = 120.0 / 60.0 / f64::from(RATE);
            for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
                let context = Context {
                    bpm: 120.0,
                    beat: from_beat + (block * 64) as f64 * per_sample,
                    playing: true,
                    sidechain: None,
                };
                chorus.process(l, r, &context);
            }
            left
        };
        let mut meters = [0.0; MAX_METERS];
        // Starting at beat 3.25: a quarter of the way through the cycle,
        // and an LFO second later another half beat on.
        let mut a = chorus(&synced);
        play(&mut a, 3.25, 750);
        a.meters(&mut meters);
        let expected = (3.25 + 750.0 * 64.0 / 24_000.0) % 1.0;
        assert!(
            (meters[0] - expected as f32).abs() < 1e-3,
            "{meters:?} {expected}"
        );
        assert!((meters[2] - 2.0).abs() < 1e-4, "{}", meters[2]);
        // Playing from a whole number of cycles later sounds the same; from
        // half a cycle later, different.
        let same = play(&mut chorus(&synced), 7.25, 750);
        let other = play(&mut chorus(&synced), 7.75, 750);
        let reference = play(&mut chorus(&synced), 3.25, 750);
        assert!(
            same.iter()
                .zip(&reference)
                .all(|(a, b)| (a - b).abs() < 1e-4)
        );
        assert!(
            rms(&other
                .iter()
                .zip(&reference)
                .map(|(a, b)| a - b)
                .collect::<Vec<_>>())
                > 0.05
        );

        // Stopped, it runs on at the tempo's rate from wherever it was;
        // starting playback catches it up with the song in well under a
        // second, without jumping.
        let mut b = chorus(&synced);
        let input = noise(24_000, 12);
        let (mut left, mut right) = (input.clone(), input);
        run(&mut b, &mut left, &mut right, 64, &Context::DEFAULT);
        b.meters(&mut meters);
        assert!(
            meters[0] < 0.01 || meters[0] > 0.99,
            "a whole cycle: {meters:?}"
        );
        play(&mut b, 10.5, 1);
        b.meters(&mut meters);
        assert!(meters[0] < 0.1, "still near where it was: {meters:?}");
        play(&mut b, 10.5 + 64.0 / 24_000.0, 600);
        b.meters(&mut meters);
        let expected = (10.5 + 601.0 * 64.0 / 24_000.0) % 1.0;
        assert!(
            (meters[0] - expected as f32).abs() < 0.01,
            "{meters:?} {expected}"
        );
    }

    #[test]
    fn extreme_feedback_stays_bounded_without_dc() {
        for mode in [FLANGER, PHASER] {
            for feedback in [-95.0, 95.0] {
                for (delay, center) in [(0.1, 50.0), (2.0, 1000.0), (40.0, 10_000.0)] {
                    let settings = [
                        ("mode", mode as f32),
                        ("feedback", feedback),
                        ("delay", delay),
                        ("center", center),
                        ("depth", 100.0),
                        ("rate", 3.0),
                        ("mix", 100.0),
                        ("output", 12.0),
                    ];
                    // Full-scale noise riding on a DC offset of 0.5.
                    let input: Vec<f32> = noise(96_000, 13).iter().map(|s| 0.5 + 0.5 * s).collect();
                    let (left, right) = stereo(&mut chorus(&settings), &input);
                    let loudest = peak(&left).max(peak(&right));
                    let mean = left[48_000..].iter().map(|&s| f64::from(s)).sum::<f64>() / 48_000.0;
                    println!("{settings:?}: peak {loudest:.2}, mean {mean:.3}");
                    assert!(
                        loudest.is_finite() && loudest < 64.0,
                        "{settings:?}: {loudest}"
                    );
                    // No more DC comes out than went in (+12 dB of it); fed
                    // back unfiltered it would be twenty times that.
                    let gain = f64::from(dsp::db_to_gain(12.0));
                    assert!(mean.abs() < 0.5 * gain, "{settings:?}: DC {mean}");
                }
            }
        }
    }

    #[test]
    fn reset_silences_the_tails() {
        for mode in [CHORUS, FLANGER, PHASER] {
            let mut chorus = chorus(&[("mode", mode as f32), ("feedback", 90.0), ("mix", 100.0)]);
            stereo(&mut chorus, &noise(9600, 3));
            chorus.reset();
            let mut meters = [1.0; MAX_METERS];
            chorus.meters(&mut meters);
            assert_eq!(&meters[..3], &[0.0; 3]);
            let (left, right) = stereo(&mut chorus, &[0.0; 9600]);
            assert!(left.iter().chain(&right).all(|&s| s == 0.0), "mode {mode}");
        }
    }

    #[test]
    fn a_bad_input_sample_does_not_poison_it() {
        for mode in [CHORUS, FLANGER, PHASER] {
            let mut chorus = chorus(&[("mode", mode as f32), ("feedback", 90.0)]);
            let mut input = noise(24_000, 5);
            input[100] = f32::NAN;
            input[200] = f32::INFINITY;
            input[300] = f32::NEG_INFINITY;
            let (left, right) = stereo(&mut chorus, &input);
            // Gone once the longest delay has passed and a block has ended.
            assert!(
                left[6000..]
                    .iter()
                    .chain(&right[6000..])
                    .all(|s| s.is_finite()),
                "mode {mode}"
            );
            assert!(rms(&left[12_000..]) > 0.1, "mode {mode}: still sounds");
        }
    }

    /// The largest sample-to-sample step at the output for a 440 Hz sine,
    /// from a tenth of a second in, with `change` applied after `at`
    /// blocks (0: before the first, so it's taken at once).
    fn largest_step(settings: &[(&str, f32)], change: impl Fn(&mut Chorus), at: usize) -> f32 {
        let mut chorus = chorus(settings);
        let input = sine(440.0, 0.5, 96_000, RATE);
        let (mut left, mut right) = (input.clone(), input);
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            if block == at {
                change(&mut chorus);
            }
            chorus.process(l, r, &Context::DEFAULT);
        }
        left[4800..]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn changing_settings_does_not_click() {
        let chorus_mode = [("voices", 3.0), ("depth", 70.0)];
        let flanger_mode = [("mode", FLANGER as f32), ("delay", 3.0), ("feedback", 40.0)];
        let phaser_mode = [("mode", PHASER as f32), ("feedback", 40.0)];
        // (settings, what changes, the change)
        type Case<'a> = (&'a [(&'a str, f32)], &'a str, fn(&mut Chorus));
        let cases: [Case; 12] = [
            (&chorus_mode, "to the flanger", |c| c.set_param(MODE, 1.0)),
            (&chorus_mode, "to the phaser", |c| c.set_param(MODE, 2.0)),
            (&flanger_mode, "to the chorus", |c| c.set_param(MODE, 0.0)),
            (&phaser_mode, "to the flanger", |c| c.set_param(MODE, 1.0)),
            (&chorus_mode, "one voice", |c| c.set_param(VOICES, 0.0)),
            (&chorus_mode, "a triangle", |c| c.set_param(SHAPE, 1.0)),
            (&chorus_mode, "a long delay", |c| c.set_param(DELAY, 40.0)),
            (&chorus_mode, "tempo sync", |c| c.set_param(SYNC, 0.0)),
            (&flanger_mode, "full spread", |c| c.set_param(SPREAD, 180.0)),
            (&flanger_mode, "negative feedback", |c| {
                c.set_param(FEEDBACK, -90.0)
            }),
            (&phaser_mode, "12 stages", |c| c.set_param(STAGES, 5.0)),
            (&phaser_mode, "a high center", |c| {
                c.set_param(CENTER, 8000.0)
            }),
        ];
        for (settings, what, change) in cases {
            // As steep as it gets settled before the change and after it.
            let before = largest_step(settings, |_| {}, usize::MAX);
            let after = largest_step(settings, change, 0);
            let through = largest_step(settings, change, 750);
            println!("{what}: largest step {through:.4} (settled {before:.4}, {after:.4})");
            assert!(through < 1.2 * before.max(after), "{what}: {through}");
        }
    }

    #[test]
    fn the_sweep_never_bends_the_pitch_too_far() {
        // 20 Hz, a 40 ms chorus at full depth would swing ±20 ms: far too
        // fast. It's held to the slope limit.
        let values = values(PARAMS, &[("rate", 20.0), ("delay", 40.0), ("depth", 100.0)]);
        let (shortest, longest) = sweep(&values, 20.0, RATE);
        let swing = (longest - shortest) / 2.0 * 0.001 * RATE;
        let slope = TAU * 20.0 / RATE * swing;
        assert!((slope - MAX_SLOPE).abs() < 1e-3, "{slope}");
        // The output's steepest step stays within what that bend allows.
        let mut chorus = chorus(&[
            ("rate", 20.0),
            ("delay", 40.0),
            ("depth", 100.0),
            ("mix", 100.0),
            ("voices", 0.0),
        ]);
        let input = sine(1000.0, 0.5, 48_000, RATE);
        let (left, _) = stereo(&mut chorus, &input);
        let steepest = left[4800..]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        let allowed = TAU * 1000.0 / RATE * 0.5 * (1.0 + MAX_SLOPE);
        assert!(steepest < 1.02 * allowed, "{steepest} against {allowed}");
    }
}
