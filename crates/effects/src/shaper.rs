//! Shaper: volume, filter and pan, each moved by a shape drawn over a note
//! value of the song's tempo, in the spirit of Cableguys' ShaperBox. Out of
//! the box the volume shape is a quarter-note sidechain-style pump.
//!
//! Each module (volume, filter, pan) has its own 32-point shape, rate,
//! depth, phase offset and smoothing, and can be switched on and off. The
//! shapes run in time with the song: while the transport rolls, where each
//! one is comes from the song position ([`Context::beat`]), so they stay
//! locked to the bar wherever playback starts; while stopped they run on
//! freely from where they were.
//!
//! The shape is read between its points with [`shape_at`]: smoothing 0
//! holds each point for its whole step (a gate or step sequencer), more
//! smoothing glides to the next point over that share of the step, and at
//! 100% the glides join into one smooth curve through every point (a
//! Catmull-Rom spline). What each shape asks for then glides for a
//! millisecond or two, so even hard steps don't click.
//!
//! Depth scales how far each module moves from "no change": volume from
//! full level, pan from the centre, the filter from fully open (the top of
//! its range for a low-pass, the bottom for a high-pass). The filter is a
//! trapezoidal SVF (see `filter.rs`) with its cutoff moved every sample, on
//! a log scale between the two range ends. Pan is a constant-power balance.
//! The chain runs filter, then volume, then pan, and the mix blends it with
//! the dry signal.

use std::f32::consts::{FRAC_PI_2, SQRT_2};

use instruments::ParamDescriptor;

use crate::{
    Category, Context, Effect, EffectDescriptor, MAX_METERS, OFF_ON, Values, choice, float,
};
use dsp::svf::{Coefs, Shape, SvfState, prewarp};
use dsp::{Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.shaper",
    name: "Shaper",
    description: "Tempo-synced volume, filter and pan shapes",
    category: Category::Modulation,
    params: PARAMS,
    meters: &[
        "Volume: where the shape is (0 to 1)",
        "Volume: shape value there (0 to 1)",
        "Filter: where the shape is (0 to 1)",
        "Filter: shape value there (0 to 1)",
        "Pan: where the shape is (0 to 1)",
        "Pan: shape value there (0 to 1)",
        "Volume applied (linear gain)",
        "Filter cutoff applied (Hz)",
        "Pan applied (-1 left to 1 right)",
    ],
    create: || Box::new(Shaper::new()),
    ..EffectDescriptor::BASE
};

/// The modules, in parameter order.
pub const MODULES: usize = 3;
pub const VOLUME: usize = 0;
pub const FILTER: usize = 1;
pub const PAN: usize = 2;

/// Points in each shape, evenly spread over one cycle.
pub const POINTS: usize = 32;

/// Parameters per module, in this order: on, rate, depth, phase,
/// smoothing, then the points.
pub const MODULE_PARAMS: usize = 5 + POINTS;
pub const ON: usize = 0;
pub const RATE: usize = 1;
pub const DEPTH: usize = 2;
pub const PHASE: usize = 3;
pub const SMOOTHING: usize = 4;
/// The first point; point `i` is `POINT + i`.
pub const POINT: usize = 5;

/// A module parameter's index.
pub const fn param(module: usize, which: usize) -> usize {
    module * MODULE_PARAMS + which
}

/// The filter's own parameters and the mix, after the modules'.
pub const FILTER_TYPE: usize = MODULES * MODULE_PARAMS;
pub const FILTER_LOW: usize = FILTER_TYPE + 1;
pub const FILTER_HIGH: usize = FILTER_TYPE + 2;
pub const RESONANCE: usize = FILTER_TYPE + 3;
pub const MIX: usize = FILTER_TYPE + 4;

const COUNT: usize = MIX + 1;

/// Rates: one cycle of the shape per note value, and its length in quarter
/// notes. Bars are 4/4.
pub const RATES: [(&str, f64); 16] = [
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
];

const RATE_LABELS: &[&str] = &[
    "1/32", "1/16 T", "1/16", "1/16 D", "1/8 T", "1/8", "1/8 D", "1/4 T", "1/4", "1/4 D", "1/2 T",
    "1/2", "1/2 D", "1 bar", "2 bars", "4 bars",
];

/// A sidechain-style duck: down 24 dB on the beat, back up like a
/// compressor's release (exponentially in dB) over the rest of it.
const PUMP: [f32; POINTS] = [
    0.063, 0.103, 0.154, 0.215, 0.282, 0.353, 0.425, 0.495, 0.560, 0.621, 0.676, 0.724, 0.767,
    0.804, 0.836, 0.863, 0.886, 0.905, 0.921, 0.935, 0.946, 0.955, 0.963, 0.970, 0.975, 0.979,
    0.983, 0.986, 0.988, 0.990, 0.992, 0.994,
];

/// Closed at the start of the bar, open halfway through, closed again.
const SWEEP: [f32; POINTS] = [
    0.000, 0.010, 0.038, 0.084, 0.146, 0.222, 0.309, 0.402, 0.500, 0.598, 0.691, 0.778, 0.854,
    0.916, 0.962, 0.990, 1.000, 0.990, 0.962, 0.916, 0.854, 0.778, 0.691, 0.598, 0.500, 0.402,
    0.309, 0.222, 0.146, 0.084, 0.038, 0.010,
];

/// Centre, right, centre, left.
const SWING: [f32; POINTS] = [
    0.500, 0.598, 0.691, 0.778, 0.854, 0.916, 0.962, 0.990, 1.000, 0.990, 0.962, 0.916, 0.854,
    0.778, 0.691, 0.598, 0.500, 0.402, 0.309, 0.222, 0.146, 0.084, 0.038, 0.010, 0.000, 0.010,
    0.038, 0.084, 0.146, 0.222, 0.309, 0.402,
];

/// The 32 point parameters of a module, called `<prefix>_p0` to
/// `<prefix>_p31` and named "<name> point 1" to "<name> point 32".
macro_rules! points {
    ($prefix:literal, $name:literal, $shape:expr) => {
        points(
            [
                concat!($prefix, "_p0"),
                concat!($prefix, "_p1"),
                concat!($prefix, "_p2"),
                concat!($prefix, "_p3"),
                concat!($prefix, "_p4"),
                concat!($prefix, "_p5"),
                concat!($prefix, "_p6"),
                concat!($prefix, "_p7"),
                concat!($prefix, "_p8"),
                concat!($prefix, "_p9"),
                concat!($prefix, "_p10"),
                concat!($prefix, "_p11"),
                concat!($prefix, "_p12"),
                concat!($prefix, "_p13"),
                concat!($prefix, "_p14"),
                concat!($prefix, "_p15"),
                concat!($prefix, "_p16"),
                concat!($prefix, "_p17"),
                concat!($prefix, "_p18"),
                concat!($prefix, "_p19"),
                concat!($prefix, "_p20"),
                concat!($prefix, "_p21"),
                concat!($prefix, "_p22"),
                concat!($prefix, "_p23"),
                concat!($prefix, "_p24"),
                concat!($prefix, "_p25"),
                concat!($prefix, "_p26"),
                concat!($prefix, "_p27"),
                concat!($prefix, "_p28"),
                concat!($prefix, "_p29"),
                concat!($prefix, "_p30"),
                concat!($prefix, "_p31"),
            ],
            [
                concat!($name, " point 1"),
                concat!($name, " point 2"),
                concat!($name, " point 3"),
                concat!($name, " point 4"),
                concat!($name, " point 5"),
                concat!($name, " point 6"),
                concat!($name, " point 7"),
                concat!($name, " point 8"),
                concat!($name, " point 9"),
                concat!($name, " point 10"),
                concat!($name, " point 11"),
                concat!($name, " point 12"),
                concat!($name, " point 13"),
                concat!($name, " point 14"),
                concat!($name, " point 15"),
                concat!($name, " point 16"),
                concat!($name, " point 17"),
                concat!($name, " point 18"),
                concat!($name, " point 19"),
                concat!($name, " point 20"),
                concat!($name, " point 21"),
                concat!($name, " point 22"),
                concat!($name, " point 23"),
                concat!($name, " point 24"),
                concat!($name, " point 25"),
                concat!($name, " point 26"),
                concat!($name, " point 27"),
                concat!($name, " point 28"),
                concat!($name, " point 29"),
                concat!($name, " point 30"),
                concat!($name, " point 31"),
                concat!($name, " point 32"),
            ],
            $shape,
        )
    };
}

/// A module's settings other than its points.
macro_rules! module {
    ($prefix:literal, $name:literal, $on:expr, $rate:expr, $smoothing:expr) => {
        [
            choice(concat!($prefix, "_on"), $name, OFF_ON, $on),
            choice(
                concat!($prefix, "_rate"),
                concat!($name, " rate"),
                RATE_LABELS,
                $rate,
            ),
            float(
                concat!($prefix, "_depth"),
                concat!($name, " depth"),
                0.0,
                100.0,
                100.0,
                "%",
                false,
            ),
            float(
                concat!($prefix, "_phase"),
                concat!($name, " phase"),
                0.0,
                360.0,
                0.0,
                "°",
                false,
            ),
            float(
                concat!($prefix, "_smooth"),
                concat!($name, " smoothing"),
                0.0,
                100.0,
                $smoothing,
                "%",
                false,
            ),
        ]
    };
}

const fn points(
    ids: [&'static str; POINTS],
    names: [&'static str; POINTS],
    shape: [f32; POINTS],
) -> [ParamDescriptor; POINTS] {
    let mut out = [float("", "", 0.0, 1.0, 0.0, "", false); POINTS];
    let mut i = 0;
    while i < POINTS {
        out[i] = float(ids[i], names[i], 0.0, 1.0, shape[i], "", false);
        i += 1;
    }
    out
}

const HEADS: [[ParamDescriptor; POINT]; MODULES] = [
    module!("vol", "Volume", 1, 8, 100.0),
    module!("flt", "Filter", 0, 13, 100.0),
    module!("pan", "Pan", 0, 11, 100.0),
];

const SHAPES: [[ParamDescriptor; POINTS]; MODULES] = [
    points!("vol", "Volume", PUMP),
    points!("flt", "Filter", SWEEP),
    points!("pan", "Pan", SWING),
];

const TAIL: [ParamDescriptor; COUNT - FILTER_TYPE] = [
    choice("flt_type", "Filter type", &["Low-pass", "High-pass"], 0),
    float(
        "flt_low",
        "Filter range low",
        20.0,
        20_000.0,
        200.0,
        "Hz",
        true,
    ),
    float(
        "flt_high",
        "Filter range high",
        20.0,
        20_000.0,
        12_000.0,
        "Hz",
        true,
    ),
    float("flt_res", "Filter resonance", 0.0, 100.0, 20.0, "%", false),
    float("mix", "Mix", 0.0, 100.0, 100.0, "%", false),
];

const fn all_params() -> [ParamDescriptor; COUNT] {
    let mut out = [TAIL[0]; COUNT];
    let mut module = 0;
    while module < MODULES {
        let mut i = 0;
        while i < POINT {
            out[param(module, i)] = HEADS[module][i];
            i += 1;
        }
        let mut i = 0;
        while i < POINTS {
            out[param(module, POINT + i)] = SHAPES[module][i];
            i += 1;
        }
        module += 1;
    }
    let mut i = 0;
    while i < TAIL.len() {
        out[FILTER_TYPE + i] = TAIL[i];
        i += 1;
    }
    out
}

const ALL_PARAMS: [ParamDescriptor; COUNT] = all_params();
const PARAMS: &[ParamDescriptor] = &ALL_PARAMS;

/// How long depth, mix and the filter's range and resonance take to reach
/// a new value.
const SMOOTH_MS: f32 = 20.0;
/// How long switching a module on or off, or the filter's type, fades.
const FADE_MS: f32 = 10.0;
/// The time constant of the glide that takes the edge off steps in a shape.
const DECLICK_MS: f32 = 1.5;
/// Filter Q at no resonance (Butterworth) and at full resonance.
const Q_RANGE: (f32, f32) = (std::f32::consts::FRAC_1_SQRT_2, 12.0);
/// Tempo used when the one given makes no sense.
const FALLBACK_BPM: f64 = 120.0;

/// A shape's value at `phase` (0 to 1 through its cycle), between its
/// `points` (each 0 to 1, evenly spread, point 0 at phase 0), with
/// `smoothing` from 0 (steps) to 1 (a smooth curve through every point).
///
/// Each point holds for the first `1 - smoothing` of its step, then eases
/// to the next one (a cubic with flat ends) over the rest. As smoothing
/// nears 1 the ends take the slope of a Catmull-Rom spline instead, so at 1
/// the eases join into one curve. The result is kept within 0 to 1.
fn interpolate(points: &[f32], phase: f32, smoothing: f32) -> f32 {
    let at = |i: usize| points[i % POINTS];
    // `phase - phase.floor()` is `phase.rem_euclid(1.0)` to the bit for
    // any finite phase, without the library remainder.
    let position = (phase - phase.floor()) * POINTS as f32;
    let i = (position as usize).min(POINTS - 1);
    let u = position - i as f32;
    let hold = 1.0 - smoothing;
    if u <= hold || smoothing <= 0.0 {
        return at(i);
    }
    let w = ((u - hold) / smoothing).min(1.0);
    let (v0, v1) = (at(i), at(i + 1));
    // Catmull-Rom slopes, per step, scaled into the ease's own span and
    // brought in only as smoothing nears 1.
    let slope = smoothing.powi(4) * smoothing * 0.5;
    let m0 = slope * (v1 - at(i + POINTS - 1));
    let m1 = slope * (at(i + 2) - v0);
    let (w2, w3) = (w * w, w * w * w);
    let value = (2.0 * w3 - 3.0 * w2 + 1.0) * v0
        + (w3 - 2.0 * w2 + w) * m0
        + (3.0 * w2 - 2.0 * w3) * v1
        + (w3 - w2) * m1;
    value.clamp(0.0, 1.0)
}

/// The shape of `module` (see [`VOLUME`], [`FILTER`] and [`PAN`]) at
/// `phase` (0 to 1 through one cycle, before the phase offset), with its
/// smoothing, for parameter values `values` (in [`DESCRIPTOR`]'s order):
/// what the UI draws in the shape editor. 0 to 1: silence to full volume,
/// the bottom to the top of the filter's range, left to right. Missing or
/// NaN values count as defaults.
pub fn shape_at(values: &[f32], module: usize, phase: f32) -> f32 {
    let module = module.min(MODULES - 1);
    let value = |index: usize| crate::param_value(PARAMS, values, index);
    let mut points = [0.0; POINTS];
    for (i, point) in points.iter_mut().enumerate() {
        *point = value(param(module, POINT + i));
    }
    let phase = if phase.is_finite() { phase } else { 0.0 };
    interpolate(&points, phase, value(param(module, SMOOTHING)) / 100.0)
}

/// One module's running state.
#[derive(Debug, Clone)]
struct Module {
    /// How much the module is heard, fading on and off.
    on: Smoothed,
    depth: Smoothed,
    /// What the module applies (volume gain, filter position or pan
    /// position, before the on fade), gliding after what the shape asks.
    value: f32,
    /// Where the shape was read last (0 to 1) and what it read there.
    phase: f32,
    shape: f32,
}

impl Module {
    fn new() -> Module {
        Module {
            on: Smoothed::new(0.0, FADE_MS),
            depth: Smoothed::new(1.0, SMOOTH_MS),
            value: 0.0,
            phase: 0.0,
            shape: 0.0,
        }
    }
}

pub struct Shaper {
    values: Values<COUNT>,
    sample_rate: f32,
    modules: [Module; MODULES],
    /// The song position in quarter notes: taken from the transport while
    /// it rolls, counted on from there while it doesn't.
    beat: f64,
    /// 0 for a low-pass, 1 for a high-pass; in between while switching.
    high_pass: Smoothed,
    log_low: Smoothed,
    log_high: Smoothed,
    /// The filter's damping, `1 / Q`.
    damping: Smoothed,
    filter: [SvfState; 2],
    mix: Smoothed,
    /// The share of the way to its target a module's value moves each sample.
    declick: f32,
    /// The module values have not been set since `reset`, so they start at
    /// their targets rather than gliding there.
    primed: bool,
    /// What [`Effect::meters`] reports after the volume, filter and pan
    /// phases and shapes.
    applied: [f32; 3],
}

impl Shaper {
    pub fn new() -> Shaper {
        let mut shaper = Shaper {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            modules: [Module::new(), Module::new(), Module::new()],
            beat: 0.0,
            high_pass: Smoothed::new(0.0, FADE_MS),
            log_low: Smoothed::new(0.0, SMOOTH_MS),
            log_high: Smoothed::new(0.0, SMOOTH_MS),
            damping: Smoothed::new(1.0, SMOOTH_MS),
            filter: [SvfState::default(); 2],
            mix: Smoothed::new(1.0, SMOOTH_MS),
            declick: 1.0,
            primed: false,
            applied: [0.0; 3],
        };
        for index in 0..COUNT {
            shaper.apply(index, Pace::Knob);
        }
        shaper.prepare(48_000.0, 0);
        shaper
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`; switches always fade at the knob's.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            FILTER_TYPE => self.high_pass.set_target(value),
            FILTER_LOW => self.log_low.move_to(value.log2(), pace),
            FILTER_HIGH => self.log_high.move_to(value.log2(), pace),
            RESONANCE => {
                let q = Q_RANGE.0 * (Q_RANGE.1 / Q_RANGE.0).powf(value / 100.0);
                self.damping.move_to(1.0 / q, pace);
            }
            MIX => self.mix.move_to(value / 100.0, pace),
            _ if index < FILTER_TYPE => {
                let module = &mut self.modules[index / MODULE_PARAMS];
                match index % MODULE_PARAMS {
                    ON => module.on.set_target(value),
                    DEPTH => module.depth.move_to(value / 100.0, pace),
                    // Rate, phase, smoothing and points are read as needed.
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn smoothers(&mut self) -> impl Iterator<Item = &mut Smoothed> {
        let [volume, filter, pan] = &mut self.modules;
        [
            &mut self.high_pass,
            &mut self.log_low,
            &mut self.log_high,
            &mut self.damping,
            &mut self.mix,
            &mut volume.on,
            &mut volume.depth,
            &mut filter.on,
            &mut filter.depth,
            &mut pan.on,
            &mut pan.depth,
        ]
        .into_iter()
    }

    /// A module's cycle length in quarter notes and phase offset (0 to 1).
    fn timing(&self, module: usize) -> (f64, f64) {
        let rate = self.values[param(module, RATE)].round().max(0.0) as usize;
        let cycle = RATES[rate.min(RATES.len() - 1)].1;
        let offset = f64::from(self.values[param(module, PHASE)]) / 360.0;
        (cycle, offset)
    }

    /// Reads module `module`'s shape at song position `beat`: its phase and
    /// raw shape value.
    #[inline]
    fn read(&self, module: usize, beat: f64, timing: (f64, f64)) -> (f32, f32) {
        let (cycle, offset) = timing;
        let turns = beat / cycle + offset;
        let phase = (turns - turns.floor()) as f32;
        let start = param(module, POINT);
        let points = &self.values[start..start + POINTS];
        let smoothing = self.values[param(module, SMOOTHING)] / 100.0;
        (phase, interpolate(points, phase, smoothing))
    }
}

impl Default for Shaper {
    fn default() -> Self {
        Shaper::new()
    }
}

impl Effect for Shaper {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        for smoothed in self.smoothers() {
            smoothed.prepare(sample_rate);
        }
        self.declick = 1.0 - (-1.0 / (DECLICK_MS * 0.001 * sample_rate)).exp();
        self.reset();
    }

    fn reset(&mut self) {
        for smoothed in self.smoothers() {
            smoothed.settle();
        }
        for module in &mut self.modules {
            module.phase = 0.0;
            module.shape = 0.0;
            module.value = 0.0;
        }
        self.beat = 0.0;
        self.filter = [SvfState::default(); 2];
        self.primed = false;
        self.applied = [0.0; 3];
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: &Context) {
        self.values.played();
        let bpm = if context.bpm.is_finite() && context.bpm > 0.0 {
            context.bpm.clamp(1.0, 1000.0)
        } else {
            FALLBACK_BPM
        };
        if context.playing && context.beat.is_finite() {
            self.beat = context.beat;
        }
        let beats_per_sample = bpm / 60.0 / f64::from(self.sample_rate);
        let timing = [self.timing(VOLUME), self.timing(FILTER), self.timing(PAN)];
        let sample_rate = self.sample_rate;

        let len = left.len().min(right.len());
        for n in 0..len {
            let beat = self.beat + n as f64 * beats_per_sample;
            // What each module's shape asks for, with depth, then glided.
            let high_pass = self.high_pass.tick();
            let mut wanted = [0.0f32; MODULES];
            for (m, want) in wanted.iter_mut().enumerate() {
                let (phase, shape) = self.read(m, beat, timing[m]);
                let module = &mut self.modules[m];
                module.phase = phase;
                module.shape = shape;
                let depth = module.depth.tick();
                *want = match m {
                    VOLUME => 1.0 - depth * (1.0 - shape),
                    // Open is the top of the range for a low-pass and the
                    // bottom for a high-pass.
                    FILTER => {
                        let low_pass = 1.0 - depth * (1.0 - shape);
                        low_pass + high_pass * (depth * shape - low_pass)
                    }
                    _ => 0.5 + depth * (shape - 0.5),
                };
            }
            for (module, want) in self.modules.iter_mut().zip(wanted) {
                if self.primed {
                    module.value += self.declick * (want - module.value);
                } else {
                    module.value = want;
                }
            }
            self.primed = true;

            let (dry_l, dry_r) = (left[n], right[n]);
            let (mut l, mut r) = (dry_l, dry_r);

            let log_low = self.log_low.tick();
            let log_high = self.log_high.tick();
            let damping = self.damping.tick();
            let filter = &mut self.modules[FILTER];
            let on = filter.on.tick();
            if on > 0.0 {
                let cutoff = (log_low + filter.value * (log_high - log_low)).exp2();
                let coefs = Coefs::new(&Shape {
                    g: prewarp(cutoff, sample_rate),
                    k: damping,
                    m0: high_pass,
                    m1: -damping * high_pass,
                    m2: 1.0 - 2.0 * high_pass,
                });
                let [state_l, state_r] = &mut self.filter;
                l += on * (state_l.tick(&coefs, l) - l);
                r += on * (state_r.tick(&coefs, r) - r);
                self.applied[1] = cutoff;
            } else {
                // Off: start from silence when switched back on.
                self.filter = [SvfState::default(); 2];
                self.applied[1] = 0.0;
            }

            let volume = &mut self.modules[VOLUME];
            let gain = 1.0 + volume.on.tick() * (volume.value - 1.0);
            l *= gain;
            r *= gain;
            self.applied[0] = gain;

            let pan = &mut self.modules[PAN];
            let on = pan.on.tick();
            if on > 0.0 {
                let position = 0.5 + on * (pan.value - 0.5);
                let angle = position * FRAC_PI_2;
                l *= SQRT_2 * angle.cos();
                r *= SQRT_2 * angle.sin();
                self.applied[2] = 2.0 * position - 1.0;
            } else {
                self.applied[2] = 0.0;
            }

            let mix = self.mix.tick();
            left[n] = dry_l + mix * (l - dry_l);
            right[n] = dry_r + mix * (r - dry_r);
        }
        self.beat += len as f64 * beats_per_sample;
        for state in &mut self.filter {
            state.flush();
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

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        for (m, module) in self.modules.iter().enumerate() {
            out[2 * m] = module.phase;
            out[2 * m + 1] = module.shape;
        }
        out[6..9].copy_from_slice(&self.applied);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, rms, set, values};

    fn shaper(settings: &[(&str, f32)]) -> Shaper {
        let mut shaper = Shaper::new();
        shaper.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut shaper, id, value);
        }
        shaper.reset();
        shaper
    }

    /// A gate: the volume shape alternating 1 and 0 every point.
    fn gate() -> Vec<(String, f32)> {
        (0..POINTS)
            .map(|i| (format!("vol_p{i}"), if i % 2 == 0 { 1.0 } else { 0.0 }))
            .collect()
    }

    fn settings(owned: &[(String, f32)]) -> Vec<(&str, f32)> {
        owned.iter().map(|(id, v)| (id.as_str(), *v)).collect()
    }

    /// Processes a constant 1 through `shaper` from song position `beat`,
    /// in 64-sample blocks with the transport `playing`.
    fn gains(shaper: &mut Shaper, len: usize, bpm: f64, beat: f64, playing: bool) -> Vec<f32> {
        let (mut left, mut right) = (vec![1.0f32; len], vec![1.0f32; len]);
        let per_sample = bpm / 60.0 / f64::from(RATE);
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            let context = Context {
                bpm,
                beat: beat + (block * 64) as f64 * per_sample,
                playing,
                sidechain: None,
            };
            shaper.process(l, r, &context);
        }
        left
    }

    #[test]
    fn parameter_ids_are_laid_out_as_documented() {
        assert_eq!(PARAMS.len(), COUNT);
        assert_eq!(PARAMS[param(VOLUME, ON)].id, "vol_on");
        assert_eq!(PARAMS[param(VOLUME, POINT)].id, "vol_p0");
        assert_eq!(PARAMS[param(FILTER, POINT + 31)].id, "flt_p31");
        assert_eq!(PARAMS[param(PAN, SMOOTHING)].id, "pan_smooth");
        assert_eq!(PARAMS[FILTER_TYPE].id, "flt_type");
        assert_eq!(PARAMS[MIX].id, "mix");
        // (`RATE` here is the test sample rate.)
        assert_eq!(PARAMS[param(VOLUME, super::RATE)].default_value(), 8.0);
        assert_eq!(RATES[8].0, "1/4");
        assert_eq!(RATE_LABELS.len(), RATES.len());
        for (label, (name, _)) in RATE_LABELS.iter().zip(RATES) {
            assert_eq!(*label, name);
        }
    }

    #[test]
    fn shapes_step_glide_and_curve_between_points() {
        let gate = gate();
        let mut steps = settings(&gate);
        steps.push(("vol_smooth", 0.0));
        let steps = values(PARAMS, &steps);
        // Steps: each point holds for its whole 1/32 of the cycle.
        assert_eq!(shape_at(&steps, VOLUME, 0.0), 1.0);
        assert_eq!(shape_at(&steps, VOLUME, 0.99 / 32.0), 1.0);
        assert_eq!(shape_at(&steps, VOLUME, 1.01 / 32.0), 0.0);
        // Half smoothing: holds for half a step, then eases half way.
        let mut half = settings(&gate);
        half.push(("vol_smooth", 50.0));
        let half = values(PARAMS, &half);
        assert_eq!(shape_at(&half, VOLUME, 0.4 / 32.0), 1.0);
        assert!((shape_at(&half, VOLUME, 0.75 / 32.0) - 0.5).abs() < 1e-5);
        // Full smoothing: a smooth curve, symmetric between points here.
        let smooth = values(PARAMS, &settings(&gate));
        let middle = shape_at(&smooth, VOLUME, 0.5 / 32.0);
        assert!((middle - 0.5).abs() < 1e-5, "{middle}");
        // The default pump: down on the beat, up most of the way by the
        // middle, and wrapping around.
        let pump = values(PARAMS, &[]);
        assert!(shape_at(&pump, VOLUME, 0.0) < 0.1);
        assert!(shape_at(&pump, VOLUME, 0.5) > 0.85);
        assert_eq!(shape_at(&pump, VOLUME, 1.25), shape_at(&pump, VOLUME, 0.25));
        assert!(shape_at(&pump, VOLUME, f32::NAN).is_finite());
        // Everything stays within 0 to 1.
        for i in 0..1000 {
            let v = shape_at(&smooth, VOLUME, i as f32 / 1000.0);
            assert!((0.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn the_default_pump_ducks_on_every_beat_in_time_with_the_song() {
        let mut shaper = shaper(&[]);
        // 120 BPM: a beat is 24000 samples. Start playing at beat 3.5.
        let out = gains(&mut shaper, 96_000, 120.0, 3.5, true);
        // Half a beat in, the next beat (4.0) arrives at sample 12000.
        let around = |n: usize| out[n - 50..n + 50].iter().fold(1.0f32, |a, &b| a.min(b));
        for beat_sample in [12_000, 36_000, 60_000, 84_000] {
            assert!(around(beat_sample) < 0.1, "{}", around(beat_sample));
        }
        // Mid-beat, nearly full level.
        assert!(out[24_000] > 0.85, "{}", out[24_000]);
        let mut meters = [0.0; MAX_METERS];
        shaper.meters(&mut meters);
        // Ended at beat 3.5 + 4 = 7.5: half way through a quarter-note cycle.
        assert!((meters[0] - 0.5).abs() < 0.01, "{meters:?}");
        assert!((meters[1] - shape_at(&values(PARAMS, &[]), VOLUME, meters[0])).abs() < 0.01);
    }

    #[test]
    fn stopped_it_runs_freely_and_starting_playback_locks_it() {
        let mut shaper = shaper(&[]);
        // Stopped: runs from 0 regardless of the song position given.
        let free = gains(&mut shaper, 24_000, 120.0, 123.456, false);
        assert!(free[0] < 0.1 && free[12_000] > 0.85);
        let mut meters = [0.0; MAX_METERS];
        shaper.meters(&mut meters);
        assert!(meters[0] > 0.99 || meters[0] < 0.01, "{meters:?}");
        // Playing from beat 10.25: a quarter of the way through a cycle.
        gains(&mut shaper, 64, 120.0, 10.25, true);
        shaper.meters(&mut meters);
        assert!(
            (meters[0] - 0.25 - 64.0 / 24_000.0).abs() < 0.01,
            "{meters:?}"
        );
    }

    #[test]
    fn a_step_gate_doesnt_click() {
        let gate = gate();
        let mut settings = settings(&gate);
        settings.push(("vol_smooth", 0.0));
        settings.push(("vol_rate", 5.0)); // 1/8: 7.8 ms steps.
        let mut shaper = shaper(&settings);
        let out = gains(&mut shaper, 24_000, 120.0, 0.0, true);
        let largest = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        // The declick glide takes a full step over about 1.5 ms.
        assert!(largest < 0.02, "{largest}");
        assert!(out.iter().any(|&g| g < 0.05) && out.iter().any(|&g| g > 0.95));
    }

    #[test]
    fn depth_and_mix_scale_the_movement() {
        let mut half = shaper(&[("vol_depth", 50.0)]);
        let out = gains(&mut half, 24_000, 120.0, 0.0, true);
        let lowest = out.iter().fold(1.0f32, |a, &b| a.min(b));
        // The curve's lowest point (just after the beat, where the spline
        // dips below the first point), halfway to full level.
        let pump = values(PARAMS, &[]);
        let bottom = (0..10_000)
            .map(|i| shape_at(&pump, VOLUME, i as f32 / 10_000.0))
            .fold(1.0f32, f32::min);
        let expected = 1.0 - 0.5 * (1.0 - bottom);
        assert!(
            lowest >= expected - 0.005 && lowest < expected + 0.03,
            "{lowest} against {expected}"
        );

        let mut dry = shaper(&[("mix", 0.0)]);
        let out = gains(&mut dry, 4800, 120.0, 0.0, true);
        assert!(out.iter().all(|&s| s == 1.0));
    }

    #[test]
    fn the_filter_shape_moves_the_cutoff() {
        // Volume off, filter on: a low-pass whose shape is all at the
        // bottom (200 Hz) or all at the top (12 kHz).
        let level = |point: f32| {
            let mut owned: Vec<(String, f32)> =
                (0..POINTS).map(|i| (format!("flt_p{i}"), point)).collect();
            owned.push(("vol_on".into(), 0.0));
            owned.push(("flt_on".into(), 1.0));
            let mut shaper = shaper(&settings(&owned));
            let input = crate::testing::sine(3000.0, 0.5, 24_000, RATE);
            let (mut left, mut right) = (input.clone(), input);
            crate::testing::run(&mut shaper, &mut left, &mut right, 64, &Context::DEFAULT);
            let mut meters = [0.0; MAX_METERS];
            shaper.meters(&mut meters);
            (rms(&left[12_000..]) / 0.5f64.sqrt() * 2.0, meters[7])
        };
        let (closed, low_cutoff) = level(0.0);
        let (open, high_cutoff) = level(1.0);
        assert!(closed < 0.02, "{closed}");
        assert!(open > 0.9, "{open}");
        assert!((low_cutoff - 200.0).abs() < 1.0 && (high_cutoff - 12_000.0).abs() < 10.0);
    }

    #[test]
    fn pan_moves_the_sound_between_the_sides_at_constant_power() {
        let mut owned: Vec<(String, f32)> =
            (0..POINTS).map(|i| (format!("pan_p{i}"), 0.0)).collect();
        owned.push(("vol_on".into(), 0.0));
        owned.push(("pan_on".into(), 1.0));
        let mut shaper = shaper(&settings(&owned));
        let (mut left, mut right) = (vec![0.5f32; 4800], vec![0.5f32; 4800]);
        shaper.process(&mut left, &mut right, &Context::DEFAULT);
        assert!((left[4000] - 0.5 * SQRT_2).abs() < 1e-4, "{}", left[4000]);
        assert!(right[4000].abs() < 1e-4);
    }
}
