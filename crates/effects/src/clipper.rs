//! Clipper: a mastering clipper, in the spirit of StandardCLIP and KClip.
//! Input gain drives the sound into a ceiling; the clipper flattens what
//! goes over, hard, through a smooth knee, or along a tanh, at up to 16
//! times the sample rate so the harmonics it makes don't alias. Then a
//! safety keeps the ceiling: off, as sample peaks, or (the default) as true
//! peaks. Delta plays only what was taken off.
//!
//! **Signal path**, per channel: input gain, up (linear-phase halfbands,
//! `mastering_halfband.rs`), the curve at the high rate, down again, the
//! safety (`mastering_safety.rs`, both channels together), output gain.
//! The curve's knee starts `softness` of the way down from the ceiling to
//! silence; below it nothing is touched, so with oversampling off what
//! stays under the knee comes out bit for bit, and with it on it comes out
//! within the halfbands' ripple (a few millionths).
//!
//! **Latency** ([`latency`]) is exact: the halfbands' delay, made a whole
//! number of samples by delaying one high-rate sample where a stage's
//! would fall between two (`plan`), plus the true-peak safety's lookahead
//! when it's on. Off with no safety, none. At 48 kHz: 67, 76, 80 and 81
//! samples for 2x to 16x, and 43 more for the true-peak safety (the
//! defaults, 4x with it: 119 samples, 2.5 ms).
//!
//! **Switching** shape, oversampling, safety or Delta crossfades over a few
//! milliseconds instead of jumping; input, ceiling, softness and output
//! glide. Oversampling and safety change the latency, so their crossfade
//! briefly hears both; the engine's delay compensation follows.

use instruments::ParamDescriptor;

mod safety;

use self::safety::Safety;
use crate::dynamics::coefficient;
use crate::halfband::fir::{Design, Down, Up};
use crate::line::Ring;
use crate::meter::{PeakMeter, RmsMeter};
use crate::truepeak::TruePeak;
use crate::{
    Category, Context, Effect, EffectDescriptor, MAX_METERS, OFF_ON, Values, choice, float,
};
use dsp::flush;
use dsp::{Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.clipper",
    name: "Clipper",
    description: "Mastering clipper: hard, smooth or tanh, oversampled, under a true-peak ceiling",
    category: Category::Dynamics,
    params: PARAMS,
    meters: &[
        "Clipped off: the most in a block, falling back (dB)",
        "Input peak, after input gain (dB)",
        "Output true peak (dB)",
        "Samples clipped over the last 0.3 s (%)",
        "True-peak safety's gain reduction, falling back (dB)",
        "Input RMS, before input gain (dB)",
        "Output RMS (dB)",
    ],
    tap: true,
    latency,
    create: || Box::new(Clipper::new()),
    ..EffectDescriptor::BASE
};

const SHAPES: &[&str] = &["Hard", "Smooth", "Tanh"];
const OVERSAMPLING: &[&str] = &["Off", "2x", "4x", "8x", "16x"];
const SAFETY: &[&str] = &["Off", "Sample peak", "True peak"];

const PARAMS: &[ParamDescriptor] = &[
    float("input", "Input gain", -12.0, 24.0, 0.0, "dB", false),
    float("ceiling", "Ceiling", -12.0, 0.0, -0.3, "dB", false),
    choice("shape", "Shape", SHAPES, 0),
    float("softness", "Softness", 0.0, 100.0, 30.0, "%", false),
    choice("oversampling", "Oversampling", OVERSAMPLING, 2),
    choice("safety", "Ceiling safety", SAFETY, 2),
    float("output", "Output", -24.0, 0.0, 0.0, "dB", false),
    choice("delta", "Delta", OFF_ON, 0),
];

const COUNT: usize = 8;

pub const INPUT: usize = 0;
pub const CEILING: usize = 1;
pub const SHAPE: usize = 2;
pub const SOFTNESS: usize = 3;
pub const OVERSAMPLE: usize = 4;
pub const SAFETY_MODE: usize = 5;
pub const OUTPUT: usize = 6;
pub const DELTA: usize = 7;

/// Meter slots, in [`DESCRIPTOR`]'s `meters` order.
pub const METER_CLIP: usize = 0;
pub const METER_INPUT_PEAK: usize = 1;
pub const METER_OUTPUT_PEAK: usize = 2;
pub const METER_CLIPPING: usize = 3;
pub const METER_SAFETY: usize = 4;
pub const METER_INPUT_RMS: usize = 5;
pub const METER_OUTPUT_RMS: usize = 6;

/// Halfband stages at 16x.
const MAX_STAGES: usize = 4;
/// Most samples one input sample becomes.
const MAX_FACTOR: usize = 1 << MAX_STAGES;
/// How much of the band oversampling keeps, as a fraction of the sample
/// rate: 21.6 kHz at 48 kHz, 19.8 kHz at 44.1 kHz.
const PASS_BAND: f64 = 0.45;
/// What the halfbands' Kaiser windows are shaped for: a little more than
/// the 100 dB they're held to, since a short window's sidelobes come out
/// higher than Kaiser's formula says.
const WINDOW_DB: f64 = 104.0;
/// Each stage's halfband half-length: the shortest whose stop band, beyond
/// the mirror of [`PASS_BAND`], lies 100 dB down, with the pass band flat
/// to two hundred-thousandths (checked by a test). The first stage does the
/// real work; the others only have to clear images far away.
const HALF_LENGTHS: [usize; MAX_STAGES] = [67, 17, 15, 9];
/// The first stage's way down is steeper than a halfband: flat to
/// [`PASS_BAND`] and 100 dB down from half the sample rate, where a
/// halfband is still only 6 dB down. So what the clipping made just below
/// and above Nyquist doesn't fold back or linger at the top of the band,
/// where no short true-peak detector reads it fully. Its half-length, the
/// shortest that does (also checked by a test), and its cutoff, a fraction
/// of twice the sample rate.
const STEEP_HALF: usize = 133;
const STEEP_CUTOFF: f64 = (PASS_BAND / 2.0 + 0.25) / 2.0;

/// How long input, ceiling, softness and output take to reach a new value.
const SMOOTH_MS: f32 = 20.0;
const SHAPE_FADE_MS: f32 = 15.0;
/// Oversampling and safety switches crossfade this long.
const SWITCH_FADE_MS: f32 = 20.0;
const DELTA_FADE_MS: f32 = 10.0;
/// How fast the clipped and safety meters fall back, as the peak meters do.
const METER_FALL_DB_PER_S: f32 = 20.0;
/// What the share of clipped samples averages over.
const CLIPPING_MS: f32 = 300.0;
/// Input samples a path is run over before it's faded in, so it starts
/// with its filters full rather than ringing from silence: more than the
/// longest path's impulse response.
const PRIME: usize = 192;

/// The curves, in the order of the `shape` parameter's labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Flat at the ceiling: the most transparent below it, the brightest
    /// over it.
    Hard,
    /// A quadratic knee from where softness puts it, reaching the ceiling
    /// at a finite level and staying there.
    Smooth,
    /// Straight to the knee, then a tanh towards the ceiling, which it
    /// only approaches: rounder and warmer, more saturation than clipping.
    Tanh,
}

impl Shape {
    pub const ALL: [Shape; 3] = [Shape::Hard, Shape::Smooth, Shape::Tanh];

    pub fn from_index(index: f32) -> Shape {
        Self::ALL[(index.round().max(0.0) as usize).min(Self::ALL.len() - 1)]
    }
}

/// Which ceiling the safety keeps, in the order of the `safety` labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// None: what oversampled clipping overshoots by passes.
    Off,
    /// A final clamp at the ceiling: sample peaks never cross it.
    Sample,
    /// The lookahead safety: true peaks never cross it.
    TruePeak,
}

impl Mode {
    fn from_index(index: f32) -> Mode {
        [Mode::Off, Mode::Sample, Mode::TruePeak][(index.round().max(0.0) as usize).min(2)]
    }
}

/// `x` through `shape` with its knee at `knee` and its top at `ceiling`
/// (both positive, `knee <= ceiling`).
#[inline]
fn bend(shape: Shape, x: f32, ceiling: f32, knee: f32) -> f32 {
    let a = x.abs();
    if shape == Shape::Hard || a <= knee {
        return x.clamp(-ceiling, ceiling);
    }
    let width = ceiling - knee;
    if width <= ceiling * 1e-6 {
        return x.clamp(-ceiling, ceiling);
    }
    let into = a - knee;
    let y = match shape {
        Shape::Smooth if into >= 2.0 * width => ceiling,
        Shape::Smooth => a - into * into / (4.0 * width),
        _ => knee + width * (into / width).tanh(),
    };
    y.min(ceiling).copysign(x)
}

/// A parameter's value from a list in [`PARAMS`] order: missing or NaN
/// values count as defaults.
fn value(values: &[f32], index: usize) -> f32 {
    crate::param_value(PARAMS, values, index)
}

/// The ceiling and where the knee starts, as amplitudes (1 = 0 dBFS), at
/// parameter values `values`. A hard clip's knee is the ceiling.
pub fn knee(values: &[f32]) -> (f32, f32) {
    let ceiling = dsp::db_to_gain(value(values, CEILING));
    let soft = value(values, SOFTNESS) / 100.0;
    match Shape::from_index(value(values, SHAPE)) {
        Shape::Hard => (ceiling, ceiling),
        _ => (ceiling, ceiling * (1.0 - soft)),
    }
}

/// The clipper's static transfer function: what an input sample `x` (1 =
/// 0 dBFS) becomes at parameter values `values`, input and output gain
/// included; oversampling's filters and the safety, which depend on what's
/// around the sample, aren't. What the UI draws.
pub fn transfer(values: &[f32], x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    let (ceiling, knee) = knee(values);
    let shape = Shape::from_index(value(values, SHAPE));
    let driven = dsp::db_to_gain(value(values, INPUT)) * x;
    dsp::db_to_gain(value(values, OUTPUT)) * bend(shape, driven, ceiling, knee)
}

/// The transition band of halfband stage `stage` (0 doubles the sample
/// rate), as a fraction of its higher rate: what's kept, [`PASS_BAND`] of
/// the original rate, and its mirror around a quarter.
#[cfg(test)]
fn transition(stage: usize) -> f64 {
    2.0 * (0.25 - PASS_BAND / f64::from(2u32 << stage))
}

/// Stage `stage`'s filters' delays, up and down, in samples of its higher
/// rate.
fn delays(stage: usize) -> (usize, usize) {
    let down = if stage == 0 {
        STEEP_HALF
    } else {
        HALF_LENGTHS[stage]
    };
    (HALF_LENGTHS[stage], down)
}

/// For oversampling by `1 << stages`: the latency in samples, and which
/// stages delay their inner signal one high-rate sample so that each stage's
/// round trip comes back on a whole sample of its lower rate.
fn plan(stages: usize) -> (usize, [bool; MAX_STAGES]) {
    let mut pads = [false; MAX_STAGES];
    // The round trip through the stages inside, at their outer rate.
    let mut round_trip = 0;
    for stage in (0..stages.min(MAX_STAGES)).rev() {
        let (up, down) = delays(stage);
        let inner = round_trip + up + down;
        pads[stage] = inner % 2 == 1;
        round_trip = (inner + usize::from(pads[stage])) / 2;
    }
    (round_trip, pads)
}

/// The filters of every stage, up and down.
#[derive(Debug, Clone)]
struct Filters {
    up: Design,
    down: Design,
}

impl Filters {
    /// Allocates: not for the audio thread.
    fn all() -> Vec<Filters> {
        (0..MAX_STAGES)
            .map(|stage| {
                let up = Design::halfband(HALF_LENGTHS[stage], WINDOW_DB);
                let down = if stage == 0 {
                    Design::lowpass(STEEP_HALF, STEEP_CUTOFF, WINDOW_DB)
                } else {
                    up.clone()
                };
                Filters { up, down }
            })
            .collect()
    }
}

/// [`EffectDescriptor::latency`]: how many samples late the clipper plays
/// at `values` and `sample_rate`.
pub fn latency(values: &[f32], sample_rate: f32) -> usize {
    let stages = value(values, OVERSAMPLE) as usize;
    let safety = match Mode::from_index(value(values, SAFETY_MODE)) {
        Mode::TruePeak => safety::latency_samples(sample_rate),
        _ => 0,
    };
    plan(stages).0 + safety
}

/// What every high-rate sample of one input sample is clipped with.
#[derive(Debug, Clone, Copy)]
struct Clip {
    /// The shape heard, and the one faded to while switching (the same
    /// otherwise), with how far the fade has got.
    from: Shape,
    to: Shape,
    fade: f32,
    ceiling: f32,
    knee: f32,
    /// Above this the curve is no longer straight.
    touches: f32,
}

impl Clip {
    #[inline]
    fn apply(&self, x: f32) -> f32 {
        let y = bend(self.from, x, self.ceiling, self.knee);
        if self.from == self.to {
            y
        } else {
            y + self.fade * (bend(self.to, x, self.ceiling, self.knee) - y)
        }
    }
}

/// What the clipping did to one input sample.
#[derive(Debug, Clone, Copy, Default)]
struct Stats {
    /// The largest magnitude that went into the curve.
    peak: f32,
    /// How many of its high-rate samples (both channels) were bent.
    touched: u32,
}

/// One doubling of the rate, for both channels.
#[derive(Debug, Clone)]
struct Stage {
    up: [Up; 2],
    down: [Down; 2],
    /// Whether the signal inside is delayed one sample of this stage's
    /// higher rate, and that sample.
    pad: bool,
    held: [f32; 2],
}

/// The signal path at one oversampling factor: up, the curve, down.
#[derive(Debug, Clone)]
struct Path {
    stages: Vec<Stage>,
    latency: usize,
    /// What one high-rate sample is of an input sample's, both channels.
    share: f32,
}

impl Path {
    /// Allocates: not for the audio thread.
    fn new(filters: &[Filters], stages: usize) -> Path {
        let (latency, pads) = plan(stages);
        Path {
            share: 1.0 / (2 << stages) as f32,
            stages: filters[..stages]
                .iter()
                .zip(pads)
                .map(|(filters, pad)| Stage {
                    up: [Up::new(&filters.up), Up::new(&filters.up)],
                    down: [Down::new(&filters.down), Down::new(&filters.down)],
                    pad,
                    held: [0.0; 2],
                })
                .collect(),
            latency,
        }
    }

    fn clear(&mut self) {
        for stage in &mut self.stages {
            for filter in &mut stage.up {
                filter.clear();
            }
            for filter in &mut stage.down {
                filter.clear();
            }
            stage.held = [0.0; 2];
        }
    }

    /// One sample of `channel` through the path, and if `halves`, the
    /// path's output half a sample on (else, or when it doesn't oversample,
    /// the same sample again).
    #[inline]
    fn tick(
        &mut self,
        filters: &[Filters],
        channel: usize,
        x: f32,
        clip: &Clip,
        stats: &mut Stats,
        halves: bool,
    ) -> [f32; 2] {
        let mut buffer = [0.0f32; MAX_FACTOR];
        let mut spare = [0.0f32; MAX_FACTOR];
        buffer[0] = x;
        let mut len = 1;
        for (stage, filters) in self.stages.iter_mut().zip(filters) {
            let filter = &mut stage.up[channel];
            for i in 0..len {
                let [a, b] = filter.push(&filters.up, buffer[i]);
                spare[2 * i] = a;
                spare[2 * i + 1] = b;
            }
            len *= 2;
            if stage.pad {
                let held = &mut stage.held[channel];
                for sample in &mut spare[..len] {
                    *held = std::mem::replace(sample, *held);
                }
            }
            std::mem::swap(&mut buffer, &mut spare);
        }
        for sample in &mut buffer[..len] {
            let a = sample.abs();
            stats.peak = stats.peak.max(a);
            if a > clip.touches {
                stats.touched += 1;
                *sample = clip.apply(*sample);
            }
        }
        let mut half = buffer[0];
        for (index, (stage, filters)) in self.stages.iter_mut().zip(filters).enumerate().rev() {
            let filter = &mut stage.down[channel];
            if index == 0 && halves {
                [buffer[0], half] = filter.push_both(&filters.down, buffer[0], buffer[1]);
                break;
            }
            len /= 2;
            for i in 0..len {
                buffer[i] = filter.push(&filters.down, buffer[2 * i], buffer[2 * i + 1]);
            }
        }
        [buffer[0], half]
    }

    /// Whether [`Path::tick`] gives values half a sample on.
    fn halves(&self) -> bool {
        !self.stages.is_empty()
    }
}

/// A fade from 0 to 1 that, once started, runs to the end.
#[derive(Debug, Clone, Copy)]
struct Fade {
    at: f32,
    step: f32,
}

impl Fade {
    /// Moves on a sample; returns where it was, and whether it's over.
    #[inline]
    fn advance(&mut self) -> (f32, bool) {
        let at = self.at;
        self.at += self.step;
        (at, self.at >= 1.0)
    }
}

pub struct Clipper {
    values: Values<COUNT>,
    sample_rate: f32,
    filters: Vec<Filters>,
    /// A path per oversampling choice.
    paths: Vec<Path>,
    /// The oversampling heard, and while switching the one fading out.
    active: usize,
    fading_from: Option<usize>,
    switch: Fade,
    shape: Shape,
    next_shape: Shape,
    shape_fade: Fade,
    mode: Mode,
    mode_from: Option<Mode>,
    mode_fade: Fade,
    /// How much of the output is the delta (0 to 1), and how fast it moves.
    delta: f32,
    delta_step: f32,
    input: Smoothed,
    ceiling: Smoothed,
    softness: Smoothed,
    output: Smoothed,
    /// The driven input, for the delta: read as late as the path.
    dry: [Ring; 2],
    safety: Safety,
    /// What the output is clamped to: the ceiling, or while it moves, the
    /// highest it has been since the samples now leaving were clipped.
    clamp: f32,
    clamp_hold: usize,
    input_peak: PeakMeter,
    output_peak: PeakMeter,
    output_detector: TruePeak,
    input_rms: RmsMeter,
    output_rms: RmsMeter,
    /// dB clipped off and taken by the safety, held and falling back.
    clip_db: f32,
    safety_db: f32,
    meter_fall: f32,
    /// The share of samples clipped, and `1 -` its one-pole coefficient.
    clipping: f32,
    clipping_rate: f32,
}

impl Clipper {
    pub fn new() -> Clipper {
        let filters = Filters::all();
        let paths = (0..=MAX_STAGES)
            .map(|stages| Path::new(&filters, stages))
            .collect();
        let ring = || {
            let mut ring = Ring::default();
            ring.allocate(PRIME.max(plan(MAX_STAGES).0) + 1);
            ring
        };
        let fade = Fade { at: 0.0, step: 1.0 };
        let mut clipper = Clipper {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            filters,
            paths,
            active: 0,
            fading_from: None,
            switch: fade,
            shape: Shape::Hard,
            next_shape: Shape::Hard,
            shape_fade: fade,
            mode: Mode::TruePeak,
            mode_from: None,
            mode_fade: fade,
            delta: 0.0,
            delta_step: 1.0,
            input: Smoothed::new(1.0, SMOOTH_MS),
            ceiling: Smoothed::new(1.0, SMOOTH_MS),
            softness: Smoothed::new(0.0, SMOOTH_MS),
            output: Smoothed::new(1.0, SMOOTH_MS),
            dry: [ring(), ring()],
            safety: Safety::new(),
            clamp: 1.0,
            clamp_hold: 0,
            input_peak: PeakMeter::default(),
            output_peak: PeakMeter::default(),
            output_detector: TruePeak::new(),
            input_rms: RmsMeter::default(),
            output_rms: RmsMeter::default(),
            clip_db: 0.0,
            safety_db: 0.0,
            meter_fall: 0.0,
            clipping: 0.0,
            clipping_rate: 1.0,
        };
        clipper.prepare(48_000.0, 0);
        clipper
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            INPUT => self.input.move_to(dsp::db_to_gain(value), pace),
            CEILING => {
                let ceiling = dsp::db_to_gain(value);
                self.ceiling.move_to(ceiling, pace);
                // Samples already clipped against the old ceiling are still
                // on their way out.
                self.clamp = self.clamp.max(ceiling);
                self.clamp_hold = plan(MAX_STAGES).0
                    + safety::latency_samples(self.sample_rate)
                    + (SMOOTH_MS * 0.001 * self.sample_rate) as usize
                    + 1;
            }
            SOFTNESS => self.softness.move_to(value / 100.0, pace),
            OUTPUT => self.output.move_to(dsp::db_to_gain(value), pace),
            // Shape, oversampling, safety and delta switch with a
            // crossfade, started in `process`.
            _ => {}
        }
    }

    fn wanted_shape(&self) -> Shape {
        Shape::from_index(self.values[SHAPE])
    }

    fn wanted_path(&self) -> usize {
        (self.values[OVERSAMPLE].round().max(0.0) as usize).min(MAX_STAGES)
    }

    fn wanted_mode(&self) -> Mode {
        Mode::from_index(self.values[SAFETY_MODE])
    }

    /// Starts any switch asked for and not already under way; one that
    /// arrives mid-switch waits for it to finish.
    fn start_switches(&mut self) {
        let wanted = self.wanted_shape();
        if self.shape == self.next_shape && wanted != self.shape {
            self.next_shape = wanted;
            self.shape_fade.at = 0.0;
        }
        let wanted = self.wanted_path();
        if self.fading_from.is_none() && wanted != self.active {
            self.prime(wanted);
            self.fading_from = Some(self.active);
            self.active = wanted;
            self.switch.at = 0.0;
        }
        let wanted = self.wanted_mode();
        if self.mode_from.is_none() && wanted != self.mode {
            self.mode_from = Some(self.mode);
            self.mode = wanted;
            self.mode_fade.at = 0.0;
        }
    }

    /// Clears path `index` and runs it over the last [`PRIME`] input
    /// samples, so it fades in already in step with the sound.
    fn prime(&mut self, index: usize) {
        let ceiling = self.ceiling.current();
        let knee = ceiling * (1.0 - self.softness.current());
        let clip = Clip {
            from: self.shape,
            to: self.shape,
            fade: 0.0,
            ceiling,
            knee,
            touches: if self.shape == Shape::Hard {
                ceiling
            } else {
                knee
            },
        };
        let path = &mut self.paths[index];
        path.clear();
        let mut ignored = Stats::default();
        for age in (0..PRIME).rev() {
            for (channel, ring) in self.dry.iter().enumerate() {
                path.tick(
                    &self.filters,
                    channel,
                    ring.get(age),
                    &clip,
                    &mut ignored,
                    false,
                );
            }
        }
    }

    /// The sound and its dry counterpart after the safety in `mode`.
    #[inline]
    fn after_safety(
        mode: Mode,
        wet: [f32; 2],
        dry: [f32; 2],
        safe: ([f32; 2], [f32; 2]),
        limit: f32,
    ) -> ([f32; 2], [f32; 2]) {
        match mode {
            Mode::Off => (wet, dry),
            Mode::Sample => (wet.map(|s| s.clamp(-limit, limit)), dry),
            Mode::TruePeak => safe,
        }
    }
}

impl Default for Clipper {
    fn default() -> Self {
        Clipper::new()
    }
}

impl Effect for Clipper {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        for smoothed in [
            &mut self.input,
            &mut self.ceiling,
            &mut self.softness,
            &mut self.output,
        ] {
            smoothed.prepare(sample_rate);
        }
        self.safety.prepare(sample_rate);
        self.input_peak.prepare(sample_rate);
        self.output_peak.prepare(sample_rate);
        self.input_rms.prepare(sample_rate);
        self.output_rms.prepare(sample_rate);
        let step = |ms: f32| 1.0 / (ms * 0.001 * sample_rate).max(1.0);
        self.shape_fade.step = step(SHAPE_FADE_MS);
        self.switch.step = step(SWITCH_FADE_MS);
        self.mode_fade.step = step(SWITCH_FADE_MS);
        self.delta_step = step(DELTA_FADE_MS);
        self.meter_fall = METER_FALL_DB_PER_S / sample_rate;
        self.clipping_rate = 1.0 - coefficient(CLIPPING_MS, sample_rate);
        for index in 0..COUNT {
            self.apply(index, Pace::Knob);
        }
        self.reset();
    }

    fn reset(&mut self) {
        for smoothed in [
            &mut self.input,
            &mut self.ceiling,
            &mut self.softness,
            &mut self.output,
        ] {
            smoothed.settle();
        }
        self.shape = self.wanted_shape();
        self.next_shape = self.shape;
        self.active = self.wanted_path();
        self.fading_from = None;
        self.mode = self.wanted_mode();
        self.mode_from = None;
        self.delta = if self.values[DELTA] >= 0.5 { 1.0 } else { 0.0 };
        for path in &mut self.paths {
            path.clear();
        }
        for ring in &mut self.dry {
            ring.clear();
        }
        self.safety.reset();
        self.clamp = self.ceiling.target();
        self.clamp_hold = 0;
        self.input_peak.reset();
        self.output_peak.reset();
        self.output_detector.reset();
        self.input_rms.reset();
        self.output_rms.reset();
        self.clip_db = 0.0;
        self.safety_db = 0.0;
        self.clipping = 0.0;
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.values.played();
        self.start_switches();
        let len = left.len().min(right.len());
        let delta_on = self.values[DELTA] >= 0.5;
        let (mut loudest_in, mut loudest_out, mut driven) = (0.0f32, 0.0f32, 0.0f32);
        let mut least_gain = 1.0f32;
        for n in 0..len {
            let gain = self.input.tick();
            let ceiling = self.ceiling.tick();
            let knee = ceiling * (1.0 - self.softness.tick());
            let output = self.output.tick();
            let (from, to) = (self.shape, self.next_shape);
            let mut clip = Clip {
                from,
                to,
                fade: 0.0,
                ceiling,
                knee,
                touches: if from == Shape::Hard && to == Shape::Hard {
                    ceiling
                } else {
                    knee
                },
            };
            if from != to {
                let (at, done) = self.shape_fade.advance();
                clip.fade = at;
                if done {
                    self.shape = to;
                }
            }

            // A non-finite sample would ring through the filters.
            let sane = |x: f32| if x.is_finite() { x } else { 0.0 };
            let raw = [sane(left[n]), sane(right[n])];
            self.input_rms
                .add(0.5 * (raw[0] * raw[0] + raw[1] * raw[1]));
            let x = raw.map(|s| s * gain);
            loudest_in = loudest_in.max(x[0].abs()).max(x[1].abs());
            for (ring, &s) in self.dry.iter_mut().zip(&x) {
                ring.push(s);
            }

            let mut stats = Stats::default();
            let filters = &self.filters;
            // Only the true-peak safety reads between the samples.
            let want = self.mode == Mode::TruePeak || self.mode_from == Some(Mode::TruePeak);
            let path = &mut self.paths[self.active];
            let lateness = path.latency;
            let mut has_halves = want && path.halves();
            let (mut wet, mut halves) = ([0.0; 2], [0.0; 2]);
            for channel in 0..2 {
                [wet[channel], halves[channel]] =
                    path.tick(filters, channel, x[channel], &clip, &mut stats, want);
            }
            let mut dry = [self.dry[0].get(lateness), self.dry[1].get(lateness)];
            if let Some(old) = self.fading_from {
                let (at, done) = self.switch.advance();
                let path = &mut self.paths[old];
                let lateness = path.latency;
                has_halves &= path.halves();
                let mut ignored = Stats::default();
                for channel in 0..2 {
                    let [y, half] =
                        path.tick(filters, channel, x[channel], &clip, &mut ignored, want);
                    wet[channel] = y + at * (wet[channel] - y);
                    halves[channel] = half + at * (halves[channel] - half);
                    let d = self.dry[channel].get(lateness);
                    dry[channel] = d + at * (dry[channel] - d);
                }
                if done {
                    self.fading_from = None;
                }
            }
            driven = driven.max(stats.peak);
            let share = stats.touched as f32 * self.paths[self.active].share;
            self.clipping += (share - self.clipping) * self.clipping_rate;

            let limit = if self.clamp_hold > 0 {
                self.clamp_hold -= 1;
                self.clamp
            } else {
                self.clamp = ceiling;
                ceiling
            };
            // When not heard, the safety only keeps its delay lines going,
            // so switching to it finds it ready.
            let safe = if want {
                self.safety
                    .tick(wet, has_halves.then_some(halves), dry, ceiling, limit)
            } else {
                self.safety.idle(wet, dry);
                (wet, dry)
            };
            let (before, before_dry) = (wet, dry);
            let (mut wet, mut dry) =
                Clipper::after_safety(self.mode, before, before_dry, safe, limit);
            if self.mode == Mode::TruePeak || self.mode_from == Some(Mode::TruePeak) {
                least_gain = least_gain.min(self.safety.gain);
            }
            if let Some(old) = self.mode_from {
                let (at, done) = self.mode_fade.advance();
                let (old_wet, old_dry) =
                    Clipper::after_safety(old, before, before_dry, safe, limit);
                for channel in 0..2 {
                    wet[channel] = old_wet[channel] + at * (wet[channel] - old_wet[channel]);
                    dry[channel] = old_dry[channel] + at * (dry[channel] - old_dry[channel]);
                }
                if done {
                    self.mode_from = None;
                }
            }

            if delta_on && self.delta < 1.0 {
                self.delta = (self.delta + self.delta_step).min(1.0);
            } else if !delta_on && self.delta > 0.0 {
                self.delta = (self.delta - self.delta_step).max(0.0);
            }
            let out = if self.delta == 0.0 {
                wet.map(|s| s * output)
            } else {
                // (1 - delta)·wet + delta·(dry - wet)
                [0, 1].map(|c| output * (wet[c] + self.delta * (dry[c] - 2.0 * wet[c])))
            };
            let peak = self.output_detector.push(out[0], out[1], true);
            loudest_out = loudest_out.max(peak);
            self.output_rms
                .add(0.5 * (out[0] * out[0] + out[1] * out[1]));
            left[n] = out[0];
            right[n] = out[1];
        }
        if len == 0 {
            return;
        }
        self.input_rms.flush();
        self.output_rms.flush();
        self.clipping = flush(self.clipping);
        self.input_peak.update(loudest_in, len);
        self.output_peak.update(loudest_out, len);
        let fall = self.meter_fall * len as f32;
        let ceiling = self.ceiling.current();
        let knee = ceiling * (1.0 - self.softness.current());
        let bent = bend(self.next_shape, driven, ceiling, knee);
        let clipped = if driven > 0.0 && bent > 0.0 && bent < driven {
            dsp::gain_to_db(driven / bent)
        } else {
            0.0
        };
        self.clip_db = clipped.max(self.clip_db - fall).max(0.0);
        let taken = if least_gain < 1.0 {
            -dsp::gain_to_db(least_gain)
        } else {
            0.0
        };
        self.safety_db = taken.max(self.safety_db - fall).max(0.0);
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

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out[METER_INPUT_PEAK] = self.input_peak.db();
        out[METER_OUTPUT_PEAK] = self.output_peak.db();
        out[METER_CLIP] = self.clip_db.min(100.0);
        out[METER_CLIPPING] = (100.0 * self.clipping).clamp(0.0, 100.0);
        out[METER_SAFETY] = self.safety_db.min(100.0);
        out[METER_INPUT_RMS] = self.input_rms.db();
        out[METER_OUTPUT_RMS] = self.output_rms.db();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, noise, peak, run, set, sine, values};

    fn clipper(settings: &[(&str, f32)]) -> Clipper {
        let mut clipper = Clipper::new();
        clipper.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut clipper, id, value);
        }
        clipper.reset();
        clipper
    }

    /// Runs a stereo signal (the same on both sides) through, in blocks
    /// of 64, followed by enough silence for everything to come out.
    fn through(clipper: &mut Clipper, signal: &[f32]) -> Vec<f32> {
        let mut left = signal.to_vec();
        left.resize(signal.len() + 256, 0.0);
        let mut right = left.clone();
        run(clipper, &mut left, &mut right, 64, &Context::DEFAULT);
        assert_eq!(left, right);
        left
    }

    /// The peak of `signal` as reconstructed between its samples: 16 points
    /// per sample from a long Kaiser-windowed sinc (±96 samples, β = 10),
    /// refined by a parabola through the loudest point and its neighbours.
    /// Far more precise than the safety's own detector.
    fn true_peak(signal: &[f32]) -> f32 {
        const HALF: isize = 96;
        const STEPS: usize = 16;
        let i0 = |x: f64| {
            let (mut sum, mut term) = (1.0, 1.0);
            for k in 1..60 {
                term *= x / 2.0 / k as f64;
                sum += term * term;
            }
            sum
        };
        let kernels: Vec<Vec<f64>> = (1..STEPS)
            .map(|step| {
                let fraction = step as f64 / STEPS as f64;
                (-HALF + 1..=HALF)
                    .map(|k| {
                        let t = fraction - k as f64;
                        let edge = (1.0 - (t / HALF as f64).powi(2)).max(0.0);
                        let x = std::f64::consts::PI * t;
                        x.sin() / x * i0(10.0 * edge.sqrt()) / i0(10.0)
                    })
                    .collect()
            })
            .collect();
        let sample_peak = peak(signal);
        let mut loudest = f64::from(sample_peak);
        for n in 0..signal.len().saturating_sub(1) {
            // Only stretches next to loud samples can hold the peak.
            if signal[n].abs().max(signal[n + 1].abs()) < 0.5 * sample_peak {
                continue;
            }
            let mut points = vec![f64::from(signal[n])];
            for kernel in &kernels {
                let mut sum = 0.0;
                for (k, weight) in (-HALF + 1..=HALF).zip(kernel) {
                    let index = n as isize + k;
                    if index >= 0 && (index as usize) < signal.len() {
                        sum += f64::from(signal[index as usize]) * weight;
                    }
                }
                points.push(sum);
            }
            points.push(f64::from(signal[n + 1]));
            for w in points.windows(3) {
                let (a, b, c) = (w[0].abs(), w[1].abs(), w[2].abs());
                if b >= a && b >= c {
                    let curvature = a - 2.0 * b + c;
                    let top = if curvature < 0.0 {
                        b - (c - a).powi(2) / (8.0 * curvature)
                    } else {
                        b
                    };
                    loudest = loudest.max(top);
                }
            }
        }
        loudest as f32
    }

    /// White noise low-passed at 19 kHz by a long Kaiser-windowed sinc (100
    /// dB down from 20 kHz), scaled to peak at 1: as dense as noise gets in
    /// the audio band.
    fn audio_band_noise(len: usize, seed: u32) -> Vec<f32> {
        const HALF: isize = 128;
        let cutoff = 19_000.0 / f64::from(RATE);
        let i0 = |x: f64| {
            let (mut sum, mut term) = (1.0, 1.0);
            for k in 1..60 {
                term *= x / 2.0 / k as f64;
                sum += term * term;
            }
            sum
        };
        let kernel: Vec<f64> = (-HALF..=HALF)
            .map(|k| {
                let t = k as f64;
                let sinc = if k == 0 {
                    2.0 * cutoff
                } else {
                    (std::f64::consts::TAU * cutoff * t).sin() / (std::f64::consts::PI * t)
                };
                let edge = 1.0 - (t / (HALF + 1) as f64).powi(2);
                sinc * i0(10.0 * edge.sqrt()) / i0(10.0)
            })
            .collect();
        let white = noise(len + 2 * HALF as usize, seed);
        let filtered: Vec<f32> = (0..len)
            .map(|n| {
                kernel
                    .iter()
                    .enumerate()
                    .map(|(k, weight)| f64::from(white[n + k]) * weight)
                    .sum::<f64>() as f32
            })
            .collect();
        let loudest = peak(&filtered);
        filtered.iter().map(|s| s / loudest).collect()
    }

    /// Test material, all within the audio band and peaking near 0 dBFS:
    /// dense noise, the classic inter-sample over (a quarter-rate sine 45°
    /// off its peaks), a sweep up to 20 kHz, a kick under hats, and a
    /// band-limited square wave.
    fn material(len: usize) -> Vec<(&'static str, Vec<f32>)> {
        let t = |n: usize| n as f64 / f64::from(RATE);
        let quarter = (0..len)
            .map(|n| (std::f64::consts::FRAC_PI_2 * n as f64 + std::f64::consts::FRAC_PI_4).sin())
            .map(|s| s as f32)
            .collect();
        let seconds = len as f64 / f64::from(RATE);
        let sweep = (0..len)
            .map(|n| {
                let rate = 1000f64.ln() / seconds;
                let phase = std::f64::consts::TAU * 20.0 * ((rate * t(n)).exp() - 1.0) / rate;
                phase.sin() as f32
            })
            .collect();
        let hats = audio_band_noise(len, 7);
        // Every 50 ms a kick and a hat, each fading in over a millisecond
        // and out before the next, so they stay in the audio band.
        let drums = (0..len)
            .map(|n| {
                let beat = t(n) % 0.05;
                let fade = (beat / 0.001).min((0.05 - beat) / 0.004).min(1.0);
                let fade = 0.5 - 0.5 * (std::f64::consts::PI * fade).cos();
                let kick = (std::f64::consts::TAU * 55.0 * beat).sin() * (-beat * 60.0).exp();
                let hat = f64::from(hats[n]) * (-beat * 200.0).exp();
                (fade * (0.8 * kick + 0.25 * hat)) as f32
            })
            .collect();
        let square = (0..len)
            .map(|n| {
                (1..=19)
                    .step_by(2)
                    .map(|k| (std::f64::consts::TAU * 1000.0 * k as f64 * t(n)).sin() / k as f64)
                    .sum::<f64>() as f32
            })
            .collect();
        vec![
            ("noise", audio_band_noise(len, 3)),
            ("quarter-rate sine", quarter),
            ("sweep", sweep),
            ("drums", drums),
            ("square wave", square),
        ]
    }

    #[test]
    fn the_curves_have_their_shapes() {
        let at = |settings: &[(&str, f32)], x: f32| transfer(&values(PARAMS, settings), x);
        let ceiling = dsp::db_to_gain(-0.3);
        // Hard: straight up to the ceiling, flat above.
        assert_eq!(at(&[], 0.5), 0.5);
        assert_eq!(at(&[], 0.9), 0.9);
        assert_eq!(at(&[], 3.0), ceiling);
        assert_eq!(at(&[], -3.0), -ceiling);
        // Input and output gain: +6 dB in, -6 dB out.
        let gains = [("input", 6.0), ("output", -6.0)];
        let expected = dsp::db_to_gain(-6.0) * (dsp::db_to_gain(6.0) * 0.25);
        assert!((at(&gains, 0.25) - expected).abs() < 1e-6);
        // Smooth and tanh: straight below the knee, bending above it, never
        // over the ceiling, and rising all the way.
        for shape in [1.0, 2.0] {
            for softness in [0.0, 10.0, 50.0, 100.0] {
                let settings = [("shape", shape), ("softness", softness)];
                let (top, knee) = knee(&values(PARAMS, &settings));
                assert_eq!(top, ceiling);
                assert!((knee - ceiling * (1.0 - softness / 100.0)).abs() < 1e-6);
                if knee > 0.01 {
                    assert_eq!(at(&settings, 0.99 * knee), 0.99 * knee);
                }
                let mut last = 0.0;
                for i in 1..=400 {
                    let x = i as f32 / 100.0;
                    let y = at(&settings, x);
                    assert!(y <= ceiling && y >= last, "{shape} {softness}: {x} -> {y}");
                    assert_eq!(at(&settings, -x), -y);
                    last = y;
                }
                if softness == 0.0 {
                    assert_eq!(at(&settings, 2.0), ceiling, "no softness is hard");
                }
            }
        }
        // The smooth knee reaches the ceiling at twice its width past it;
        // the tanh only approaches it.
        let smooth = [("shape", 1.0), ("softness", 50.0)];
        assert_eq!(at(&smooth, 1.5 * ceiling), ceiling);
        assert!(at(&smooth, 1.2 * ceiling) < ceiling);
        let tanh = [("shape", 2.0), ("softness", 50.0)];
        assert!(at(&tanh, 1.5 * ceiling) < 0.99 * ceiling);
        assert!(at(&tanh, 6.0) > 0.999 * ceiling);
    }

    /// How far a design is from ideal: the pass band's (up to `edge`)
    /// largest deviation from 1 and the stop band's (from `stop`) largest
    /// gain, frequencies as fractions of its rate.
    fn deviations(design: &Design, edge: f64, stop: f64) -> (f64, f64) {
        let (mut pass, mut leak) = (0.0f64, 0.0f64);
        for i in 0..=8000 {
            let f = 0.5 * i as f64 / 8000.0;
            let gain = design.response(f);
            if f <= edge {
                pass = pass.max((gain - 1.0).abs());
            } else if f >= stop {
                leak = leak.max(gain.abs());
            }
        }
        (pass, leak)
    }

    #[test]
    fn each_stage_stops_what_would_alias() {
        // 100 dB down, flat to 2e-5 (0.0002 dB).
        let good = |(pass, leak): (f64, f64)| pass <= 2e-5 && leak <= 1e-5;
        let mut checks = Vec::new();
        let mut check = |what: &str,
                         half: usize,
                         make: &dyn Fn(usize) -> Design,
                         bands: (f64, f64)| {
            let (pass, leak) = deviations(&make(half), bands.0, bands.1);
            let shortest = (1..200)
                .map(|c| 2 * c + 1)
                .find(|&c| good(deviations(&make(c), bands.0, bands.1)))
                .unwrap();
            println!(
                "{what}: half-length {half} (shortest that does: {shortest}), pass band within {pass:.1e}, stop band {:.1} dB",
                20.0 * leak.log10()
            );
            checks.push((what.to_owned(), good((pass, leak)), half == shortest));
        };
        for (stage, &half) in HALF_LENGTHS.iter().enumerate() {
            let width = transition(stage);
            check(
                &format!("stage {stage} halfband"),
                half,
                &|c| Design::halfband(c, WINDOW_DB),
                (0.25 - width / 2.0, 0.25 + width / 2.0),
            );
        }
        check(
            "stage 0 down",
            STEEP_HALF,
            &|c| Design::lowpass(c, STEEP_CUTOFF, WINDOW_DB),
            (PASS_BAND / 2.0, 0.25),
        );
        for (what, good, shortest) in checks {
            assert!(good, "{what}");
            assert!(shortest, "{what} could be shorter");
        }
    }

    #[test]
    fn latencies_are_whole_samples_and_as_planned() {
        let latency_at = |oversampling: f32, safety: f32, rate: f32| {
            latency(
                &values(
                    PARAMS,
                    &[("oversampling", oversampling), ("safety", safety)],
                ),
                rate,
            )
        };
        let plain: Vec<usize> = (0..=4).map(|o| latency_at(o as f32, 0.0, RATE)).collect();
        let safe: Vec<usize> = (0..=4).map(|o| latency_at(o as f32, 2.0, RATE)).collect();
        println!("latency at 48 kHz, Off to 16x: {plain:?}; with the true-peak safety {safe:?}");
        println!(
            "at 44.1 kHz with the true-peak safety: {:?}",
            (0..=4)
                .map(|o| latency_at(o as f32, 2.0, 44_100.0))
                .collect::<Vec<_>>()
        );
        assert_eq!(plain, [0, 100, 109, 113, 114]);
        assert_eq!(safe, [43, 143, 152, 156, 157]);
        assert_eq!(
            latency_at(2.0, 1.0, RATE),
            109,
            "the sample-peak safety adds none"
        );
        assert_eq!(latency(&[], RATE), 152, "the defaults");
    }

    #[test]
    fn quiet_sound_passes_bit_exact_without_oversampling() {
        // Sample peaks at -3 dBFS and true peaks under the -0.3 dB ceiling:
        // untouched, only late.
        for (name, piece) in material(4800) {
            let input: Vec<f32> = piece.iter().map(|s| s * dsp::db_to_gain(-3.0)).collect();
            for safety in [0.0, 1.0, 2.0] {
                for shape in [0.0, 1.0, 2.0] {
                    // Smooth and tanh at 5%: their knee is above -3 dBFS.
                    let settings = [
                        ("oversampling", 0.0),
                        ("safety", safety),
                        ("shape", shape),
                        ("softness", 5.0),
                    ];
                    let mut clipper = clipper(&settings);
                    let out = through(&mut clipper, &input);
                    let delay = latency(&values(PARAMS, &settings), RATE);
                    assert!(out[..delay].iter().all(|&s| s == 0.0));
                    assert!(
                        out[delay..delay + input.len()] == input[..],
                        "{name}, safety {safety}, shape {shape}"
                    );
                }
            }
        }
    }

    #[test]
    fn quiet_sound_passes_within_the_ripple_with_oversampling() {
        // Each piece of material at -6 dBFS (under the ceiling even between
        // samples), faded in and out so that nothing in it, its edges
        // included, goes past 0.45 of the sample rate.
        let pieces: Vec<Vec<f32>> = material(9600)
            .into_iter()
            .map(|(_, signal)| {
                let fade = 1024;
                let len = signal.len();
                signal
                    .iter()
                    .enumerate()
                    .map(|(n, s)| {
                        let edge = n.min(len - 1 - n).min(fade) as f32 / fade as f32;
                        0.5 * s * (0.5 - 0.5 * (std::f32::consts::PI * edge).cos())
                    })
                    .collect()
            })
            .collect();
        for oversampling in 1..=4 {
            for safety in [0.0, 2.0] {
                let settings = [("oversampling", oversampling as f32), ("safety", safety)];
                let delay = latency(&values(PARAMS, &settings), RATE);
                let mut worst = 0.0f32;
                for input in &pieces {
                    let out = through(&mut clipper(&settings), input);
                    worst = input
                        .iter()
                        .zip(&out[delay..])
                        .map(|(a, b)| (a - b).abs())
                        .fold(worst, f32::max);
                }
                println!(
                    "{}x, safety {safety}: at most {worst:.2e} ({:.1} dBFS) from the input, {delay} samples late",
                    1 << oversampling,
                    dsp::gain_to_db(worst)
                );
                assert!(worst < 2e-5, "{worst}");
            }
        }
    }

    #[test]
    // The oversampling setting is also where its results go.
    #[allow(clippy::needless_range_loop)]
    fn the_ceiling_holds_between_samples() {
        let material = material(4800);
        // The worst true peak over the ceiling (dB), per safety, then
        // oversampling, then material.
        let mut worst = [[[f32::NEG_INFINITY; 5]; 5]; 3];
        for (m, (name, signal)) in material.iter().enumerate() {
            for drive in [0.0, 6.0, 18.0] {
                for shape in [0.0, 1.0, 2.0] {
                    for oversampling in 0..=4 {
                        for (safety, ceiling) in
                            [(2.0, -0.3), (2.0, -6.0), (1.0, -0.3), (0.0, -0.3)]
                        {
                            let mut clipper = clipper(&[
                                ("input", drive),
                                ("ceiling", ceiling),
                                ("shape", shape),
                                ("softness", 40.0),
                                ("oversampling", oversampling as f32),
                                ("safety", safety),
                            ]);
                            let out = through(&mut clipper, signal);
                            let over = dsp::gain_to_db(true_peak(&out)) - ceiling;
                            let slot = &mut worst[safety as usize][oversampling][m];
                            *slot = slot.max(over);
                            if safety > 0.0 {
                                let sample_peak = peak(&out);
                                assert!(
                                    sample_peak <= dsp::db_to_gain(ceiling),
                                    "{name}, +{drive} dB into {ceiling} dB, shape {shape}, {}x, safety {safety}: sample peak {sample_peak}",
                                    1 << oversampling
                                );
                            }
                        }
                    }
                }
            }
        }
        let names: Vec<&str> = material.iter().map(|(name, _)| *name).collect();
        println!(
            "true peak over the ceiling, worst over drives 0-18 dB and shapes (dB): {names:?}"
        );
        for (safety, label) in ["no safety", "sample-peak safety", "true-peak safety"]
            .iter()
            .enumerate()
        {
            for (oversampling, row) in worst[safety].iter().enumerate() {
                let row: Vec<String> = row.iter().map(|v| format!("{v:+.3}")).collect();
                println!(
                    "  {label:<18} {:>2}x: {}",
                    1 << oversampling,
                    row.join("  ")
                );
            }
        }
        for oversampling in 1..=4 {
            for (m, over) in worst[2][oversampling].iter().enumerate() {
                assert!(
                    *over <= 0.0,
                    "{}, {}x: true peak {over:+.3} dB over",
                    names[m],
                    1 << oversampling
                );
            }
        }
        // Clipped at the sample rate, the sound aliases right up to
        // Nyquist, where no short true-peak detector reads fully: within a
        // dB on dense material driven hard, within a few hundredths on
        // tonal material.
        for (m, over) in worst[2][0].iter().enumerate() {
            let allowed = if m == 0 || m == 2 { 1.0 } else { 0.05 };
            assert!(
                *over <= allowed,
                "{}, oversampling off: true peak {over:+.3} dB over",
                names[m]
            );
        }
    }

    /// Aliasing in the audible band, in dB relative to the harmonics: a
    /// sine at exactly bin `BIN` of a `PERIOD`-sample window, clipped hard,
    /// and once the output repeats every period, whatever lies below 20 kHz
    /// but off the harmonics is aliasing.
    fn aliasing_db(settings: &[(&str, f32)]) -> f64 {
        const PERIOD: usize = 4096;
        const BIN: usize = 431; // 5050.8 Hz at 48 kHz; prime, so aliases spread.
        let mut clipper = clipper(settings);
        let input = sine(
            f64::from(RATE) * BIN as f64 / PERIOD as f64,
            0.5,
            PERIOD * 4,
            RATE,
        );
        let out = through(&mut clipper, &input);
        let window = &out[input.len() - PERIOD..input.len()];
        let audible = (20_000.0 / RATE * PERIOD as f32) as usize;
        let n = PERIOD;
        let (mut harmonics, mut aliases) = (0.0f64, 0.0f64);
        for bin in 1..=audible {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (i, &s) in window.iter().enumerate() {
                let phase = std::f64::consts::TAU * (bin * i % n) as f64 / n as f64;
                re += f64::from(s) * phase.cos();
                im -= f64::from(s) * phase.sin();
            }
            let power = re * re + im * im;
            if bin % BIN == 0 {
                harmonics += power;
            } else {
                aliases += power;
            }
        }
        10.0 * (aliases.max(1e-30) / harmonics).log10()
    }

    #[test]
    fn oversampling_reduces_aliasing() {
        for (name, shape, drive) in [("hard", 0.0, 12.0), ("tanh", 2.0, 12.0)] {
            for safety in [0.0, 2.0] {
                let measured: Vec<f64> = (0..=4)
                    .map(|oversampling| {
                        aliasing_db(&[
                            ("shape", shape),
                            ("softness", 50.0),
                            ("input", drive),
                            ("oversampling", oversampling as f32),
                            ("safety", safety),
                        ])
                    })
                    .collect();
                println!(
                    "{name}, 5 kHz sine at -6 dBFS +{drive} dB into -0.3 dB, safety {safety}: aliasing off {:.1} dB, 2x {:.1}, 4x {:.1}, 8x {:.1}, 16x {:.1}",
                    measured[0], measured[1], measured[2], measured[3], measured[4]
                );
                // Each doubling helps (a hard clip's harmonics fall slowly,
                // so what folds back around the high rate shrinks by 4 to
                // 13 dB a doubling) until what's left is down at the
                // halfbands' stop band.
                for pair in measured.windows(2) {
                    assert!(
                        pair[1] < pair[0] - 4.0 || pair[1] < -80.0,
                        "{name}: {measured:?}"
                    );
                }
                assert!(measured[2] < -50.0, "{name}: {measured:?}");
                assert!(measured[4] < -65.0, "{name}: {measured:?}");
            }
        }
    }

    #[test]
    fn delta_is_what_was_taken_off() {
        let input: Vec<f32> = material(4800).into_iter().flat_map(|(_, s)| s).collect();
        for oversampling in 0..=4 {
            for safety in [0.0, 1.0, 2.0] {
                let settings = [
                    ("input", 6.0),
                    ("oversampling", oversampling as f32),
                    ("safety", safety),
                    ("shape", 1.0),
                ];
                let clipped = through(&mut clipper(&settings), &input);
                let mut with_delta = settings.to_vec();
                with_delta.push(("delta", 1.0));
                let delta = through(&mut clipper(&with_delta), &input);
                let delay = latency(&values(PARAMS, &settings), RATE);
                let gain = dsp::db_to_gain(6.0);
                let worst = (0..input.len())
                    .map(|n| {
                        let driven = if n >= delay {
                            gain * input[n - delay]
                        } else {
                            0.0
                        };
                        (clipped[n] + delta[n] - driven).abs()
                    })
                    .fold(0.0f32, f32::max);
                assert!(worst < 1e-5, "{oversampling} {safety}: {worst}");
                assert!(peak(&delta) > 0.1, "it clipped something");
            }
        }
    }

    #[test]
    fn switching_anything_doesnt_click() {
        // A low sine: from one sample to the next it barely moves, so a
        // click is a jump much larger than the waveform's own steps.
        let input = sine(110.0, 0.4, 96_000, RATE);
        let largest_step = |out: &[f32]| {
            out.windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max)
        };
        let settings = [("input", 6.0), ("shape", 1.0)];
        let steady = through(&mut clipper(&settings), &input);
        let normal = largest_step(&steady[4800..input.len()]);

        type Switch = (&'static str, &'static [f32]);
        let switches: [Switch; 6] = [
            ("oversampling", &[0.0, 3.0, 1.0, 4.0, 2.0]),
            ("shape", &[0.0, 2.0, 1.0]),
            ("safety", &[2.0, 0.0, 1.0, 2.0]),
            ("softness", &[30.0, 80.0]),
            ("ceiling", &[-0.3, -3.0, -1.0]),
            ("delta", &[1.0, 0.0]),
        ];
        let mut report = Vec::new();
        for (id, choices) in switches {
            let mut switching = clipper(&settings);
            let (mut left, mut right) = (input.clone(), input.clone());
            for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
                if block % 50 == 25 {
                    set(&mut switching, id, choices[(block / 50) % choices.len()]);
                }
                switching.process(l, r, &Context::DEFAULT);
            }
            let worst = largest_step(&left[4800..]);
            report.push(format!("{id} {worst:.4}"));
            // Delta switches to a different sound altogether: it may only
            // glide there.
            let allowed = if id == "delta" { 3.0 } else { 1.5 };
            assert!(worst < allowed * normal, "{id}: {worst} against {normal}");
        }
        println!(
            "largest step: steady {normal:.4}, switching {}",
            report.join(", ")
        );
    }

    #[test]
    fn meters_read_levels_clipping_and_the_safety() {
        // A 1 kHz sine at -6.02 dBFS, +12 dB into -0.3 dB: 6.28 dB clipped.
        let mut clipper = clipper(&[("input", 12.0)]);
        let input = sine(1000.0, 0.5, 3 * RATE as usize, RATE);
        let (mut left, mut right) = (input.clone(), input);
        run(&mut clipper, &mut left, &mut right, 64, &Context::DEFAULT);
        let mut meters = [0.0; MAX_METERS];
        clipper.meters(&mut meters);
        println!("meters: {:?}", &meters[..7]);
        assert!((meters[METER_INPUT_PEAK] - 5.98).abs() < 0.02);
        assert!(meters[METER_OUTPUT_PEAK] <= -0.3 && meters[METER_OUTPUT_PEAK] > -1.0);
        // The input's peak over the ceiling.
        assert!((meters[METER_CLIP] - (5.98 + 0.3)).abs() < 0.02);
        // |1.99 sin| > 0.966 for 68% of the time.
        assert!((meters[METER_CLIPPING] - 68.0).abs() < 2.0);
        assert!(meters[METER_SAFETY] > 0.0 && meters[METER_SAFETY] < 2.0);
        assert!((meters[METER_INPUT_RMS] + 9.03).abs() < 0.05);
        assert!(meters[METER_OUTPUT_RMS] > -3.5 && meters[METER_OUTPUT_RMS] < -1.0);

        // Below the ceiling, nothing clipped and no safety.
        let mut quiet = super::tests::clipper(&[]);
        let input = sine(1000.0, 0.5, RATE as usize / 2, RATE);
        let (mut left, mut right) = (input.clone(), input);
        run(&mut quiet, &mut left, &mut right, 64, &Context::DEFAULT);
        quiet.meters(&mut meters);
        assert_eq!(meters[METER_CLIP], 0.0);
        assert_eq!(meters[METER_CLIPPING], 0.0);
        assert_eq!(meters[METER_SAFETY], 0.0);
        assert!((meters[METER_OUTPUT_PEAK] + 6.02).abs() < 0.05);
    }

    #[test]
    fn silence_and_garbage() {
        let mut clipper = clipper(&[("input", 24.0), ("oversampling", 4.0)]);
        let silent = through(&mut clipper, &vec![0.0; 4800]);
        assert!(silent.iter().all(|&s| s == 0.0));
        let mut garbage = vec![f32::NAN, f32::INFINITY, -1e30, 0.5, f32::NEG_INFINITY];
        let mut other = garbage.clone();
        clipper.process(&mut garbage, &mut other, &Context::DEFAULT);
        let after = through(&mut clipper, &vec![0.0; 1000]);
        let limit = dsp::db_to_gain(-0.3);
        assert!(after.iter().all(|s| s.is_finite() && s.abs() <= limit));
        assert!(after[after.len() - 100..].iter().all(|&s| s == 0.0));
    }
}
