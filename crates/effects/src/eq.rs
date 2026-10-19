//! Parametric EQ: up to 24 bands, each a low cut, low shelf, bell, notch,
//! high shelf, high cut or band pass, plus an output gain. Bells and shelves can be
//! dynamic: their gain moves with the level around their own frequency. Its
//! UI is a curve with a handle per band that is on, drawn with
//! [`response_db`], or [`response_db_with`] for the curve as the dynamic
//! bands move it, over the spectrum of its output.
//!
//! Each band is one trapezoidal state-variable filter (see `filter.rs`); the
//! type only changes how its outputs are mixed, so cuts are 12 dB/octave
//! with Q as resonance, and bells and shelves are the bilinear transforms of
//! the classic analog ones. A cut's slope runs it through up to four of
//! them in a row, for 24, 36 or 48 dB/octave; each has the n-th root of
//! the band's Q, so the corner sits where a single filter's would (3 dB
//! down at the default Q) and resonance doesn't compound. Frequency, gain
//! and Q glide in log/dB terms (at the knob's pace, or within a block for a
//! modulator: see "Modulation" in `lib.rs`); the filter coefficients are
//! worked out from them every [`CHUNK`] samples and interpolated linearly
//! in between, which this topology tolerates at any speed. Switching a band on, off or to another type fades it through
//! "no change". A band that is off, or a bell or shelf at 0 dB that isn't
//! moving, is skipped, so it costs nothing.
//!
//! A dynamic band listens through a band-pass at its own frequency and Q,
//! with unity gain at the centre, to the EQ's input, or to the sidechain
//! when one is routed. Every [`CHUNK`] samples, the loudest band-passed
//! sample's excess over `threshold` moves the band's gain by 1 dB per dB, up
//! to `range` (negative ducks, like a de-esser or a resonance tamer;
//! positive lifts what gets loud). As in the compressor, that movement is
//! held at its peaks and let go at the release rate, then approached at the
//! attack rate; it joins the band's gain where the coefficients are worked
//! out, so it glides like a knob move. A band's gain plus its movement
//! stays within ±[`MAX_GAIN_DB`]. The detectors cost nothing while no band
//! is dynamic.

use instruments::ParamDescriptor;

use crate::dynamics::{DB_PER_LOG2, coefficient};
use crate::meter::{PeakMeter, block_peak};
use crate::{Context, Effect, EffectDescriptor, OFF_ON, Values, choice, float};
use dsp::svf::{Coefs, Shape, SvfState, prewarp, response_tan};
use dsp::{Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.eq",
    name: "EQ",
    description: "Parametric equalizer with up to 24 bands, any of them dynamic",
    category: crate::Category::Eq,
    params: PARAMS,
    meters: &[
        "Band 1 dynamic gain (dB)",
        "Band 2 dynamic gain (dB)",
        "Band 3 dynamic gain (dB)",
        "Band 4 dynamic gain (dB)",
        "Band 5 dynamic gain (dB)",
        "Band 6 dynamic gain (dB)",
        "Band 7 dynamic gain (dB)",
        "Band 8 dynamic gain (dB)",
        "Band 9 dynamic gain (dB)",
        "Band 10 dynamic gain (dB)",
        "Band 11 dynamic gain (dB)",
        "Band 12 dynamic gain (dB)",
        "Band 13 dynamic gain (dB)",
        "Band 14 dynamic gain (dB)",
        "Band 15 dynamic gain (dB)",
        "Band 16 dynamic gain (dB)",
        "Band 17 dynamic gain (dB)",
        "Band 18 dynamic gain (dB)",
        "Band 19 dynamic gain (dB)",
        "Band 20 dynamic gain (dB)",
        "Band 21 dynamic gain (dB)",
        "Band 22 dynamic gain (dB)",
        "Band 23 dynamic gain (dB)",
        "Band 24 dynamic gain (dB)",
        "Output peak left (dB)",
        "Output peak right (dB)",
    ],
    tap: true,
    sidechain: true,
    create: || Box::new(Eq::new()),
    ..EffectDescriptor::BASE
};

pub const BANDS: usize = 24;
/// Parameters per band, in this order.
pub const BAND_PARAMS: usize = 9;
pub const ON: usize = 0;
pub const TYPE: usize = 1;
pub const FREQ: usize = 2;
pub const GAIN: usize = 3;
pub const Q: usize = 4;
/// Whether the band's gain follows its level.
pub const DYN: usize = 5;
/// The level (dB) above which a dynamic band's gain starts to move.
pub const THRESHOLD: usize = 6;
/// How far (dB) a dynamic band's gain moves at most; negative ducks.
pub const RANGE: usize = 7;
/// A cut's slope, as an index into [`SLOPES`]: 12 to 48 dB/octave.
pub const SLOPE: usize = 8;

/// Slopes a low or high cut can have; each 12 dB/octave is one filter.
pub const SLOPES: &[&str] = &["12 dB/oct", "24 dB/oct", "36 dB/oct", "48 dB/oct"];
/// The most filters a band runs in a row.
const MAX_STAGES: usize = 4;
/// The output gain's index, after every band's.
pub const OUTPUT: usize = BANDS * BAND_PARAMS;
/// The dynamic bands' attack and release, shared by all of them.
pub const ATTACK: usize = OUTPUT + 1;
pub const RELEASE: usize = OUTPUT + 2;

/// Meter slots: the first [`BANDS`] are each band's dynamic gain offset in
/// dB (what to pass to [`response_db_with`]), then the output's peaks.
pub const METER_PEAK_LEFT: usize = BANDS;
pub const METER_PEAK_RIGHT: usize = BANDS + 1;

/// A band's gain plus its dynamic movement stays within this many dB
/// either way.
pub const MAX_GAIN_DB: f32 = 30.0;

/// A band parameter's index.
pub const fn param(band: usize, which: usize) -> usize {
    band * BAND_PARAMS + which
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandType {
    LowCut,
    LowShelf,
    Bell,
    Notch,
    HighShelf,
    HighCut,
    /// Passes a band around its frequency, at unity there, and cuts the
    /// rest at 6 dB/octave each side; Q sets how wide.
    BandPass,
}

impl BandType {
    /// In the order of the type parameter's values (band pass came last, so
    /// projects from before it keep their types).
    pub const ALL: [BandType; 7] = [
        BandType::LowCut,
        BandType::LowShelf,
        BandType::Bell,
        BandType::Notch,
        BandType::HighShelf,
        BandType::HighCut,
        BandType::BandPass,
    ];

    pub fn from_index(index: f32) -> BandType {
        Self::ALL[(index.round().max(0.0) as usize).min(Self::ALL.len() - 1)]
    }

    /// Whether the band's gain does anything (cuts and notches have none),
    /// and so whether it can be dynamic.
    pub fn has_gain(self) -> bool {
        matches!(
            self,
            BandType::LowShelf | BandType::Bell | BandType::HighShelf
        )
    }

    /// Whether its slope can be steeper than 12 dB/octave.
    pub fn has_slope(self) -> bool {
        matches!(self, BandType::LowCut | BandType::HighCut)
    }

    /// How many filters in a row it runs at slope `slope` (an index into
    /// [`SLOPES`]).
    fn stages(self, slope: f32) -> usize {
        if self.has_slope() {
            (slope.round().max(0.0) as usize + 1).min(MAX_STAGES)
        } else {
            1
        }
    }
}

const TYPES: &[&str] = &[
    "Low cut",
    "Low shelf",
    "Bell",
    "Notch",
    "High shelf",
    "High cut",
    "Band pass",
];

macro_rules! band {
    ($n:literal, $on:expr, $kind:expr, $freq:expr, $q:expr) => {
        [
            choice(concat!("b", $n, "_on"), concat!("Band ", $n), OFF_ON, $on),
            choice(
                concat!("b", $n, "_type"),
                concat!("Band ", $n, " type"),
                TYPES,
                $kind,
            ),
            float(
                concat!("b", $n, "_freq"),
                concat!("Band ", $n, " frequency"),
                20.0,
                20_000.0,
                $freq,
                "Hz",
                true,
            ),
            float(
                concat!("b", $n, "_gain"),
                concat!("Band ", $n, " gain"),
                -24.0,
                24.0,
                0.0,
                "dB",
                false,
            ),
            float(
                concat!("b", $n, "_q"),
                concat!("Band ", $n, " Q"),
                0.1,
                18.0,
                $q,
                "",
                true,
            ),
            choice(
                concat!("b", $n, "_dyn"),
                concat!("Band ", $n, " dynamic"),
                OFF_ON,
                0,
            ),
            float(
                concat!("b", $n, "_threshold"),
                concat!("Band ", $n, " threshold"),
                -60.0,
                0.0,
                -24.0,
                "dB",
                false,
            ),
            float(
                concat!("b", $n, "_range"),
                concat!("Band ", $n, " range"),
                -24.0,
                24.0,
                -6.0,
                "dB",
                false,
            ),
            choice(
                concat!("b", $n, "_slope"),
                concat!("Band ", $n, " slope"),
                SLOPES,
                0,
            ),
        ]
    };
}

/// Bands 1 to 6 as they always were; 7 to 24 start off, as bells with Q 1
/// filling the gaps between them, all 24 about 0.4 octave apart, so a band
/// added without choosing a frequency doesn't land on another.
const BAND_LIST: [[ParamDescriptor; BAND_PARAMS]; BANDS] = [
    band!("1", 0, 0, 30.0, 0.71),
    band!("2", 1, 1, 100.0, 0.71),
    band!("3", 1, 2, 400.0, 1.0),
    band!("4", 1, 2, 1500.0, 1.0),
    band!("5", 1, 4, 6000.0, 0.71),
    band!("6", 0, 5, 18_000.0, 0.71),
    band!("7", 0, 2, 40.0, 1.0),
    band!("8", 0, 2, 52.0, 1.0),
    band!("9", 0, 2, 70.0, 1.0),
    band!("10", 0, 2, 120.0, 1.0),
    band!("11", 0, 2, 160.0, 1.0),
    band!("12", 0, 2, 210.0, 1.0),
    band!("13", 0, 2, 280.0, 1.0),
    band!("14", 0, 2, 480.0, 1.0),
    band!("15", 0, 2, 640.0, 1.0),
    band!("16", 0, 2, 840.0, 1.0),
    band!("17", 0, 2, 1100.0, 1.0),
    band!("18", 0, 2, 1900.0, 1.0),
    band!("19", 0, 2, 2500.0, 1.0),
    band!("20", 0, 2, 3400.0, 1.0),
    band!("21", 0, 2, 4500.0, 1.0),
    band!("22", 0, 2, 7800.0, 1.0),
    band!("23", 0, 2, 10_000.0, 1.0),
    band!("24", 0, 2, 13_500.0, 1.0),
];

const GLOBALS: [ParamDescriptor; 3] = [
    float("output", "Output", -24.0, 24.0, 0.0, "dB", false),
    float("attack", "Dynamic attack", 0.5, 200.0, 10.0, "ms", true),
    float(
        "release",
        "Dynamic release",
        10.0,
        2000.0,
        150.0,
        "ms",
        true,
    ),
];

const COUNT: usize = RELEASE + 1;

/// Every band's parameters in band order, then the global ones.
const fn flatten(
    bands: [[ParamDescriptor; BAND_PARAMS]; BANDS],
    globals: [ParamDescriptor; 3],
) -> [ParamDescriptor; COUNT] {
    let mut out = [globals[0]; COUNT];
    let mut index = 0;
    while index < OUTPUT {
        out[index] = bands[index / BAND_PARAMS][index % BAND_PARAMS];
        index += 1;
    }
    let mut global = 0;
    while global < globals.len() {
        out[OUTPUT + global] = globals[global];
        global += 1;
    }
    out
}

const PARAM_ARRAY: [ParamDescriptor; COUNT] = flatten(BAND_LIST, GLOBALS);
const PARAMS: &[ParamDescriptor] = &PARAM_ARRAY;

/// While a band glides, its coefficients are worked out every this many
/// samples and interpolated per sample in between. The dynamic bands'
/// levels are taken over the same stretches.
const CHUNK: usize = 16;
/// How long frequency, gain, Q, threshold and range take to reach a new
/// value.
const GLIDE_MS: f32 = 20.0;
/// How long a band takes to fade in or out when it is switched on or off or
/// changes type.
const FADE_MS: f32 = 10.0;
/// How long the output gain takes to reach a new value.
const OUTPUT_MS: f32 = 20.0;
/// A dynamic movement this small (dB) is none, so a release ends in exact
/// zero and a flat band stops costing anything.
const NO_MOVEMENT: f32 = 1e-4;

/// The EQ's gain at `frequency` in dB, for parameter values `values` (in
/// [`DESCRIPTOR`]'s order) at `sample_rate`: the curve as set, which the UI
/// draws.
///
/// Exactly the response the EQ settles to when no dynamic band is moving,
/// from the same filter shapes, evaluated in closed form. Cheap enough to
/// call per pixel: one `tan` per band that is on, no allocation. Missing or
/// NaN values count as defaults.
pub fn response_db(values: &[f32], frequency: f32, sample_rate: f32) -> f32 {
    response_db_with(values, &[], frequency, sample_rate)
}

/// [`response_db`] with each band's gain moved by `offsets_db[band]`, the
/// dynamic gain offsets the EQ's meters publish in their first [`BANDS`]
/// slots: the curve as it is heard right now, moving with the music.
/// Missing or non-finite offsets count as 0; offsets on cuts and notches do
/// nothing, as in the EQ.
pub fn response_db_with(
    values: &[f32],
    offsets_db: &[f32],
    frequency: f32,
    sample_rate: f32,
) -> f32 {
    let value = |index: usize| crate::param_value(PARAMS, values, index);
    let sample_rate = if sample_rate > 0.0 {
        sample_rate
    } else {
        48_000.0
    };
    let tan_w = response_tan(frequency, sample_rate);
    let mut power = 1.0f64;
    for band in 0..BANDS {
        if value(param(band, ON)) < 0.5 {
            continue;
        }
        let offset = match offsets_db.get(band) {
            Some(offset) if offset.is_finite() => *offset,
            _ => 0.0,
        };
        let kind = BandType::from_index(value(param(band, TYPE)));
        let stages = kind.stages(value(param(band, SLOPE)));
        let shape = band_shape(
            kind,
            value(param(band, FREQ)),
            total_gain(value(param(band, GAIN)), offset),
            stage_q(value(param(band, Q)), stages),
            sample_rate,
        );
        if !shape.is_identity() {
            power *= shape.power_at(tan_w).powi(stages as i32);
        }
    }
    (10.0 * power.max(1e-30).log10()) as f32 + value(OUTPUT)
}

/// Each of `stages` filters' Q, so that in a row they peak (or dip at the
/// corner) as much as one filter at `q` would.
fn stage_q(q: f32, stages: usize) -> f32 {
    if stages > 1 {
        q.powf(1.0 / stages as f32)
    } else {
        q
    }
}

/// A band's gain with its dynamic movement, within the limits.
fn total_gain(gain_db: f32, offset_db: f32) -> f32 {
    (gain_db + offset_db).clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
}

/// The filter shape of one band, for processing and drawing alike.
///
/// The bell and shelves follow Simper's derivations: a bell's damping
/// shrinks as it boosts so boosts and cuts mirror each other, and a shelf's
/// tuning is offset by `√A` so its midpoint lands on `freq`.
fn band_shape(kind: BandType, freq: f32, gain_db: f32, q: f32, sample_rate: f32) -> Shape {
    let g = prewarp(freq, sample_rate);
    let k = 1.0 / q;
    // A² is the band's gain as an amplitude factor.
    let a = || 10f32.powf(gain_db / 40.0);
    match kind {
        BandType::LowCut => Shape::highpass(freq, q, sample_rate),
        BandType::HighCut => Shape::lowpass(freq, q, sample_rate),
        BandType::Notch => Shape {
            g,
            k,
            m0: 1.0,
            m1: -k,
            m2: 0.0,
        },
        BandType::BandPass => Shape {
            g,
            k,
            m0: 0.0,
            m1: k,
            m2: 0.0,
        },
        BandType::Bell => {
            let a = a();
            let k = k / a;
            Shape {
                g,
                k,
                m0: 1.0,
                m1: k * (a * a - 1.0),
                m2: 0.0,
            }
        }
        BandType::LowShelf => {
            let a = a();
            Shape {
                g: g / a.sqrt(),
                k,
                m0: 1.0,
                m1: k * (a - 1.0),
                m2: a * a - 1.0,
            }
        }
        BandType::HighShelf => {
            let a = a();
            Shape {
                g: g * a.sqrt(),
                k,
                m0: a * a,
                m1: k * (1.0 - a) * a,
                m2: 1.0 - a * a,
            }
        }
    }
}

/// `shape` faded towards "no change": `mix` 0 is the input, 1 the band.
fn faded(shape: Shape, mix: f32) -> Shape {
    if mix >= 1.0 {
        return shape;
    }
    Shape {
        m0: 1.0 + (shape.m0 - 1.0) * mix,
        m1: shape.m1 * mix,
        m2: shape.m2 * mix,
        ..shape
    }
}

struct Band {
    on: bool,
    /// The type asked for.
    kind: BandType,
    /// The type being heard; differs from `kind` while fading out to switch.
    playing: BandType,
    /// The slope asked for, as an index into [`SLOPES`].
    slope: f32,
    /// How many filters in a row are heard; like `playing`, it changes
    /// only once the band has faded out.
    stages: usize,
    log_freq: Smoothed,
    gain: Smoothed,
    log_q: Smoothed,
    /// How much of the band is heard, from 0 (bypassed) to 1.
    mix: Smoothed,
    /// The shape this sample, the one at the end of the chunk, and the
    /// per-sample step between them while `ramping`.
    shape: Shape,
    target: Shape,
    step: Shape,
    ramping: bool,
    /// Coefficients for `shape` while it holds still.
    coefs: Coefs,
    /// Whether the band changes the sound at all; if not it's skipped.
    active: bool,
    /// The shape must be worked out again even though nothing glides.
    stale: bool,
    /// Each filter's memory, left and right.
    state: [[SvfState; MAX_STAGES]; 2],

    /// Whether the gain follows the band's level.
    dynamic: bool,
    threshold: Smoothed,
    range: Smoothed,
    /// Whether the detector listens: the band is on, dynamic, of a type
    /// with a gain, and has a range.
    detecting: bool,
    /// The detector's unity-gain band-pass, worked out for `detect_tuning`
    /// (the band's log2 frequency and Q).
    detect_coefs: Coefs,
    detect_tuning: (f32, f32),
    detect_state: [SvfState; 2],
    /// The loudest band-passed sample so far in this chunk.
    chunk_peak: f32,
    /// The movement the level calls for (dB), held at its peaks and let go
    /// at the release rate.
    held: f32,
    /// `held` approached at the attack rate: how far the gain has moved.
    amount: f32,
    /// The dynamic gain offset, `amount` in `range`'s direction, in dB.
    offset: f32,
    /// The offset the band's shape was last worked out with.
    shaped_offset: f32,
}

impl Band {
    fn new() -> Band {
        let identity = Shape {
            g: 1.0,
            k: 1.0,
            m0: 1.0,
            m1: 0.0,
            m2: 0.0,
        };
        Band {
            on: false,
            kind: BandType::Bell,
            playing: BandType::Bell,
            slope: 0.0,
            stages: 1,
            log_freq: Smoothed::new(10.0, GLIDE_MS),
            gain: Smoothed::new(0.0, GLIDE_MS),
            log_q: Smoothed::new(0.0, GLIDE_MS),
            mix: Smoothed::new(0.0, FADE_MS),
            shape: identity,
            target: identity,
            step: identity,
            ramping: false,
            coefs: Coefs::IDENTITY,
            active: false,
            stale: true,
            state: [[SvfState::default(); MAX_STAGES]; 2],
            dynamic: false,
            threshold: Smoothed::new(0.0, GLIDE_MS),
            range: Smoothed::new(0.0, GLIDE_MS),
            detecting: false,
            detect_coefs: Coefs::IDENTITY,
            detect_tuning: (f32::NAN, f32::NAN),
            detect_state: [SvfState::default(); 2],
            chunk_peak: 0.0,
            held: 0.0,
            amount: 0.0,
            offset: 0.0,
            shaped_offset: 0.0,
        }
    }

    fn set(&mut self, which: usize, value: f32, pace: Pace) {
        match which {
            ON => self.on = value >= 0.5,
            TYPE => self.kind = BandType::from_index(value),
            FREQ => self.log_freq.move_to(value.log2(), pace),
            GAIN => self.gain.move_to(value, pace),
            Q => self.log_q.move_to(value.log2(), pace),
            DYN => self.dynamic = value >= 0.5,
            THRESHOLD => self.threshold.move_to(value, pace),
            RANGE => self.range.move_to(value, pace),
            SLOPE => self.slope = value,
            _ => {}
        }
    }

    /// The filters in a row its type and slope call for.
    fn wanted_stages(&self) -> usize {
        self.kind.stages(self.slope)
    }

    fn smoothers(&mut self) -> [&mut Smoothed; 6] {
        [
            &mut self.log_freq,
            &mut self.gain,
            &mut self.log_q,
            &mut self.mix,
            &mut self.threshold,
            &mut self.range,
        ]
    }

    /// Smoothers tick once per chunk.
    fn prepare(&mut self, sample_rate: f32) {
        for smoothed in self.smoothers() {
            smoothed.prepare_every(sample_rate, CHUNK);
        }
    }

    /// Jump to the current settings with silent filter memory and no
    /// dynamic movement.
    fn snap(&mut self, sample_rate: f32) {
        for smoothed in self.smoothers() {
            smoothed.settle();
        }
        self.playing = self.kind;
        self.stages = self.wanted_stages();
        let mix = if self.on { 1.0 } else { 0.0 };
        self.mix.set_immediate(mix);
        self.state = [[SvfState::default(); MAX_STAGES]; 2];
        self.chunk_peak = 0.0;
        self.held = 0.0;
        self.amount = 0.0;
        self.offset = 0.0;
        self.shaped_offset = 0.0;
        self.detecting = false;
        // Worked out afresh: the sample rate may have changed.
        self.detect_tuning = (f32::NAN, f32::NAN);
        self.update_detector(sample_rate);
        self.settle(faded(self.current_shape(sample_rate), mix));
        self.stale = false;
    }

    fn current_shape(&self, sample_rate: f32) -> Shape {
        band_shape(
            self.playing,
            self.log_freq.current().exp2(),
            total_gain(self.gain.current(), self.offset),
            stage_q(self.log_q.current().exp2(), self.stages),
            sample_rate,
        )
    }

    /// Hold `shape` still.
    fn settle(&mut self, shape: Shape) {
        self.shape = shape;
        self.target = shape;
        self.ramping = false;
        self.coefs = Coefs::new(&shape);
        self.active = !shape.is_identity();
        if !self.active {
            self.state = [[SvfState::default(); MAX_STAGES]; 2];
        }
    }

    /// Called at the start of every chunk.
    fn update(&mut self, sample_rate: f32) {
        self.threshold.tick();
        self.range.tick();

        // A type or slope change fades the band out, switches, then fades
        // it back in.
        let switching = self.playing != self.kind || self.stages != self.wanted_stages();
        if switching && self.mix.current() == 0.0 {
            self.playing = self.kind;
            self.stages = self.wanted_stages();
            self.state = [[SvfState::default(); MAX_STAGES]; 2];
            self.stale = true;
        }
        let switching = self.playing != self.kind || self.stages != self.wanted_stages();
        let mix_target = if self.on && !switching { 1.0 } else { 0.0 };
        if self.mix.target() != mix_target {
            self.mix.set_target(mix_target);
        }

        if self.mix.current() == 0.0 && !self.mix.is_smoothing() {
            // Off and silent: take new settings at once, unheard, for free.
            if self.active
                || self.stale
                || self.log_freq.is_smoothing()
                || self.gain.is_smoothing()
                || self.log_q.is_smoothing()
            {
                for smoothed in [&mut self.log_freq, &mut self.gain, &mut self.log_q] {
                    smoothed.settle();
                }
                let shape = faded(self.current_shape(sample_rate), 0.0);
                self.settle(shape);
                self.stale = false;
            }
            self.shaped_offset = self.offset;
            return;
        }

        let gliding = self.mix.is_smoothing()
            || self.log_freq.is_smoothing()
            || self.gain.is_smoothing()
            || self.log_q.is_smoothing()
            || self.offset != self.shaped_offset;
        if !gliding && !self.stale {
            if self.ramping {
                self.settle(self.target);
            }
            return;
        }

        let mix = self.mix.tick();
        self.log_freq.tick();
        self.gain.tick();
        self.log_q.tick();
        let target = faded(self.current_shape(sample_rate), mix);
        self.shaped_offset = self.offset;
        self.stale = false;
        if self.ramping {
            // Land exactly where the last chunk was heading.
            self.shape = self.target;
        } else if !self.active {
            // Silent bands' memory is cleared; start from "no change" at the
            // new tuning and let the mix bring the band in.
            self.shape = Shape {
                m0: 1.0,
                m1: 0.0,
                m2: 0.0,
                ..target
            };
        }
        if target.is_identity() && self.shape.is_identity() {
            self.settle(target);
            return;
        }
        self.step = self.shape.step_to(&target, CHUNK);
        self.target = target;
        self.ramping = true;
        self.active = true;
    }

    /// Called at the start of every chunk, after [`Self::update`]: starts or
    /// stops the detector and keeps it tuned to the band.
    fn update_detector(&mut self, sample_rate: f32) {
        let detecting = self.on
            && self.dynamic
            && self.kind.has_gain()
            && (self.range.current() != 0.0 || self.range.target() != 0.0);
        if detecting != self.detecting {
            self.detecting = detecting;
            self.detect_state = [SvfState::default(); 2];
            self.chunk_peak = 0.0;
        }
        if !detecting {
            return;
        }
        // A NaN tuning (after a snap) never compares equal, so it is always
        // worked out.
        let tuning = (self.log_freq.current(), self.log_q.current());
        if tuning != self.detect_tuning {
            self.detect_tuning = tuning;
            let k = 1.0 / tuning.1.exp2();
            self.detect_coefs = Coefs::new(&Shape {
                g: prewarp(tuning.0.exp2(), sample_rate),
                k,
                m0: 0.0,
                m1: k,
                m2: 0.0,
            });
        }
    }

    /// Listens to a stretch of the detector's signal, before the bands
    /// change it.
    fn detect(&mut self, left: &[f32], right: &[f32]) {
        if !self.detecting {
            return;
        }
        let c = self.detect_coefs;
        let [state_l, state_r] = &mut self.detect_state;
        let mut peak = self.chunk_peak;
        // Plain comparisons rather than `f32::max`, whose care for NaN
        // costs several instructions a sample; a NaN is passed over the
        // same way (the comparison is false).
        for (&l, &r) in left.iter().zip(right) {
            let a = state_l.tick(&c, l).abs();
            let b = state_r.tick(&c, r).abs();
            if a > peak {
                peak = a;
            }
            if b > peak {
                peak = b;
            }
        }
        self.chunk_peak = peak;
    }

    /// At the end of every chunk: moves the gain offset towards what the
    /// chunk's level calls for. `attack` and `release` are chunk-rate
    /// one-pole coefficients.
    fn follow(&mut self, attack: f32, release: f32) {
        if !self.detecting && self.held == 0.0 && self.amount == 0.0 {
            return;
        }
        let range = self.range.current();
        let span = range.abs();
        let wanted = if self.detecting && self.chunk_peak > 0.0 {
            let level = DB_PER_LOG2 * self.chunk_peak.log2();
            (level - self.threshold.current()).clamp(0.0, span)
        } else {
            0.0
        };
        self.chunk_peak = 0.0;
        for state in &mut self.detect_state {
            state.flush();
        }

        self.held = if wanted >= self.held {
            wanted
        } else {
            wanted + release * (self.held - wanted)
        };
        if self.held < NO_MOVEMENT {
            self.held = 0.0;
        }
        self.amount = if self.held > self.amount {
            self.held + attack * (self.amount - self.held)
        } else {
            self.held
        };
        // Within the range even while it glides, so a range crossing zero
        // takes the movement through zero instead of flipping it.
        self.amount = self.amount.min(span);
        self.offset = if self.amount > 0.0 {
            self.amount.copysign(range)
        } else {
            0.0
        };
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.active {
            return;
        }
        let Band {
            shape,
            step,
            coefs,
            ramping,
            stages,
            state: [state_l, state_r],
            ..
        } = self;
        let (state_l, state_r) = (&mut state_l[..*stages], &mut state_r[..*stages]);
        if *ramping {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                shape.advance(step);
                let c = Coefs::new(shape);
                for (sl, sr) in state_l.iter_mut().zip(state_r.iter_mut()) {
                    *l = sl.tick(&c, *l);
                    *r = sr.tick(&c, *r);
                }
            }
        } else {
            let c = *coefs;
            match state_l.len() {
                0 => {}
                1 => cascade::<1>(&c, state_l, state_r, left, right),
                2 => cascade::<2>(&c, state_l, state_r, left, right),
                3 => cascade::<3>(&c, state_l, state_r, left, right),
                _ => cascade::<MAX_STAGES>(&c, state_l, state_r, left, right),
            }
        }
        for state in state_l.iter_mut().chain(state_r.iter_mut()) {
            state.flush();
        }
    }
}

/// Runs `left` and `right` through `N` filters of `c` in a row, sample by
/// sample, with every filter's memory (`N` of `state_l` and `state_r`) held
/// in locals for the block.
///
/// Locals, and `N` known when compiled, so the memory provably stays in
/// registers: under fat LTO (`dist`) the compiler otherwise stored and
/// reloaded it every sample, on the filters' feedback paths, and six bells
/// ran 30% slower. Sample by sample rather than filter by filter, so the
/// processor overlaps the next sample's first filter with this one's later
/// ones: a steep cut ran 14% slower filter by filter.
#[inline(always)]
fn cascade<const N: usize>(
    c: &Coefs,
    state_l: &mut [SvfState],
    state_r: &mut [SvfState],
    left: &mut [f32],
    right: &mut [f32],
) {
    let mut a: [SvfState; N] = state_l[..N].try_into().unwrap();
    let mut b: [SvfState; N] = state_r[..N].try_into().unwrap();
    for (l, r) in left.iter_mut().zip(right.iter_mut()) {
        let (mut x, mut y) = (*l, *r);
        for k in 0..N {
            x = a[k].tick(c, x);
            y = b[k].tick(c, y);
        }
        (*l, *r) = (x, y);
    }
    state_l[..N].copy_from_slice(&a);
    state_r[..N].copy_from_slice(&b);
}

/// A stretch of a sidechain channel, or less if it's shorter than the block.
fn stretch(channel: &[f32], start: usize, end: usize) -> &[f32] {
    &channel[start.min(channel.len())..end.min(channel.len())]
}

pub struct Eq {
    values: Values<COUNT>,
    sample_rate: f32,
    bands: [Band; BANDS],
    /// Output gain as an amplitude factor.
    output: Smoothed,
    /// The dynamic bands' attack and release, as chunk-rate coefficients.
    attack: f32,
    release: f32,
    /// Samples left until the bands next update their coefficients.
    countdown: usize,
    /// Whether any band's detector is listening this chunk.
    detecting: bool,
    peak_left: PeakMeter,
    peak_right: PeakMeter,
}

impl Eq {
    pub fn new() -> Eq {
        let mut eq = Eq {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            bands: std::array::from_fn(|_| Band::new()),
            output: Smoothed::new(1.0, OUTPUT_MS),
            attack: 0.0,
            release: 0.0,
            countdown: 0,
            detecting: false,
            peak_left: PeakMeter::default(),
            peak_right: PeakMeter::default(),
        };
        eq.prepare(48_000.0, 0);
        eq
    }

    /// Hands parameter `index`'s value to whatever uses it.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        let chunk_rate = self.sample_rate / CHUNK as f32;
        match index {
            OUTPUT => self.output.move_to(dsp::db_to_gain(value), pace),
            ATTACK => self.attack = coefficient(value, chunk_rate),
            RELEASE => self.release = coefficient(value, chunk_rate),
            _ => self.bands[index / BAND_PARAMS].set(index % BAND_PARAMS, value, pace),
        }
    }

    fn snap(&mut self) {
        for band in &mut self.bands {
            band.snap(self.sample_rate);
        }
        self.output.settle();
        self.countdown = 0;
        self.detecting = false;
        self.peak_left.reset();
        self.peak_right.reset();
        self.values.refresh();
    }
}

impl Default for Eq {
    fn default() -> Self {
        Eq::new()
    }
}

impl Effect for Eq {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        for band in &mut self.bands {
            band.prepare(sample_rate);
        }
        self.output.prepare(sample_rate);
        self.peak_left.prepare(sample_rate);
        self.peak_right.prepare(sample_rate);
        for index in 0..COUNT {
            self.apply(index, Pace::Knob);
        }
        self.snap();
    }

    fn reset(&mut self) {
        self.snap();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: &Context) {
        self.values.played();
        let len = left.len().min(right.len());
        let (left, right) = (&mut left[..len], &mut right[..len]);
        let mut start = 0;
        while start < len {
            if self.countdown == 0 {
                self.detecting = false;
                for band in &mut self.bands {
                    band.update(self.sample_rate);
                    band.update_detector(self.sample_rate);
                    self.detecting |= band.detecting;
                }
                self.countdown = CHUNK;
            }
            let end = start + self.countdown.min(len - start);

            // The detectors hear the input before any band changes it.
            if self.detecting {
                let (detect_l, detect_r) = match context.sidechain {
                    Some((side_l, side_r)) => {
                        (stretch(side_l, start, end), stretch(side_r, start, end))
                    }
                    None => (&left[start..end], &right[start..end]),
                };
                for band in &mut self.bands {
                    band.detect(detect_l, detect_r);
                }
            }

            let (l, r) = (&mut left[start..end], &mut right[start..end]);
            for band in &mut self.bands {
                band.process(l, r);
            }
            self.countdown -= end - start;
            if self.countdown == 0 {
                for band in &mut self.bands {
                    band.follow(self.attack, self.release);
                }
            }
            start = end;
        }

        if self.output.is_smoothing() || self.output.current() != 1.0 {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                let gain = self.output.tick();
                *l *= gain;
                *r *= gain;
            }
        }
        self.peak_left.update(block_peak(left), len);
        self.peak_right.update(block_peak(right), len);
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn receive(&mut self, index: usize, value: f32, pace: Pace) {
        if self.values.set(index, value) {
            self.apply(index, pace);
            if self.values.is_fresh() {
                if index == OUTPUT {
                    self.output.settle();
                } else if index < OUTPUT {
                    self.bands[index / BAND_PARAMS].snap(self.sample_rate);
                }
            }
        }
    }

    fn meters(&self, out: &mut [f32; crate::MAX_METERS]) {
        for (slot, band) in out.iter_mut().zip(&self.bands) {
            *slot = band.offset;
        }
        out[METER_PEAK_LEFT] = self.peak_left.db();
        out[METER_PEAK_RIGHT] = self.peak_right.db();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults;
    use crate::testing::{RATE, db, noise, peak, rms, run, sine};

    /// (band, type, frequency, gain, Q) for each band that's on; the rest are off.
    type Setting = &'static [(usize, BandType, f32, f32, f32)];

    fn values(setting: Setting, output: f32) -> [f32; COUNT] {
        let mut values: [f32; COUNT] = defaults(PARAMS);
        for band in 0..BANDS {
            values[param(band, ON)] = 0.0;
        }
        for &(band, kind, freq, gain, q) in setting {
            values[param(band, ON)] = 1.0;
            values[param(band, TYPE)] =
                BandType::ALL.iter().position(|&k| k == kind).unwrap() as f32;
            values[param(band, FREQ)] = freq;
            values[param(band, GAIN)] = gain;
            values[param(band, Q)] = q;
        }
        values[OUTPUT] = output;
        values
    }

    fn eq_with(values: &[f32; COUNT], sample_rate: f32) -> Eq {
        let mut eq = Eq::new();
        eq.prepare(sample_rate, 64);
        for (index, &value) in values.iter().enumerate() {
            eq.set_param(index, value);
        }
        eq.reset();
        eq
    }

    /// The EQ's steady-state gain in dB for a sine at `freq`, over whole
    /// cycles, with `side` (if any) as the sidechain; and the EQ, to read
    /// its meters.
    fn measured_with(
        values: &[f32; COUNT],
        freq: f64,
        sample_rate: f32,
        side: Option<&[f32]>,
    ) -> (f64, Eq) {
        let mut eq = eq_with(values, sample_rate);
        let settle = sample_rate as usize / 2;
        let cycles = (0.25 * freq).ceil();
        let window = (cycles * f64::from(sample_rate) / freq).round() as usize;
        let input = sine(freq, 0.5, settle + window, sample_rate);
        let (mut left, mut right) = (input.clone(), input.clone());
        for start in (0..input.len()).step_by(64) {
            let end = (start + 64).min(input.len());
            let context = Context {
                sidechain: side.map(|s| (&s[start..end], &s[start..end])),
                ..Context::DEFAULT
            };
            eq.process(&mut left[start..end], &mut right[start..end], &context);
        }
        assert_eq!(left, right);
        (db(rms(&left[settle..]) / rms(&input[settle..])), eq)
    }

    /// The EQ's steady-state gain in dB for a sine at `freq`, over whole cycles.
    fn measured_db(values: &[f32; COUNT], freq: f64, sample_rate: f32) -> f64 {
        measured_with(values, freq, sample_rate, None).0
    }

    fn log_frequencies(count: usize) -> impl Iterator<Item = f64> {
        (0..count).map(move |i| 25.0 * (18_000.0f64 / 25.0).powf(i as f64 / (count - 1) as f64))
    }

    #[test]
    fn processing_matches_the_drawn_response() {
        let settings: [(Setting, f32); 4] = [
            (
                &[
                    (0, BandType::LowShelf, 200.0, -6.0, 0.71),
                    (2, BandType::Bell, 1000.0, 12.0, 2.0),
                    (5, BandType::HighCut, 8000.0, 0.0, 0.71),
                ],
                0.0,
            ),
            (
                &[
                    (0, BandType::LowCut, 80.0, 0.0, 2.0),
                    (3, BandType::Notch, 3000.0, 0.0, 4.0),
                    (4, BandType::HighShelf, 5000.0, 9.0, 1.5),
                ],
                -3.0,
            ),
            (
                &[
                    (1, BandType::Bell, 120.0, -18.0, 10.0),
                    (2, BandType::Bell, 2500.0, 6.0, 0.3),
                    (3, BandType::HighShelf, 12_000.0, -12.0, 0.71),
                ],
                6.0,
            ),
            (
                &[
                    (0, BandType::LowCut, 300.0, 0.0, 0.5),
                    (1, BandType::LowShelf, 60.0, 15.0, 3.0),
                    (2, BandType::Bell, 15_000.0, 9.0, 18.0),
                    (5, BandType::HighCut, 2000.0, 0.0, 6.0),
                    (17, BandType::Bell, 400.0, -9.0, 4.0),
                    (23, BandType::HighShelf, 9000.0, 4.0, 0.71),
                ],
                0.0,
            ),
        ];
        let mut worst = 0.0f64;
        for sample_rate in [48_000.0, 44_100.0] {
            for (setting, output) in settings {
                let values = values(setting, output);
                for freq in log_frequencies(30) {
                    let expected = f64::from(response_db(&values, freq as f32, sample_rate));
                    let measured = measured_db(&values, freq, sample_rate);
                    if expected < -50.0 {
                        // Down in the noise of the measurement: just deep.
                        assert!(
                            measured < -45.0,
                            "{freq} Hz: {measured} dB, drawn {expected}"
                        );
                        continue;
                    }
                    let error = (measured - expected).abs();
                    worst = worst.max(error);
                    assert!(
                        error < 0.5,
                        "{setting:?} at {sample_rate} Hz, {freq:.0} Hz: measured {measured:.2} dB, drawn {expected:.2} dB"
                    );
                }
            }
        }
        println!("largest difference between processing and response_db: {worst:.4} dB");
    }

    #[test]
    fn band_passes_and_steep_cuts_match_the_drawn_response() {
        let mut values = values(
            &[
                (0, BandType::LowCut, 200.0, 0.0, 0.71),
                (1, BandType::BandPass, 1500.0, 0.0, 2.0),
                (5, BandType::HighCut, 6000.0, 0.0, 0.71),
            ],
            0.0,
        );
        values[param(0, SLOPE)] = 3.0;
        values[param(5, SLOPE)] = 1.0;
        for freq in log_frequencies(24) {
            let expected = f64::from(response_db(&values, freq as f32, RATE));
            let measured = measured_db(&values, freq, RATE);
            if expected < -50.0 {
                assert!(
                    measured < -45.0,
                    "{freq} Hz: {measured} dB, drawn {expected}"
                );
                continue;
            }
            assert!(
                (measured - expected).abs() < 0.5,
                "{freq:.0} Hz: measured {measured:.2} dB, drawn {expected:.2} dB"
            );
        }
        // The band pass is unity at its centre and falls away either side.
        let alone = values_with_slope(&[(1, BandType::BandPass, 1500.0, 0.0, 2.0)], 0);
        assert!(measured_db(&alone, 1500.0, RATE).abs() < 0.2);
        assert!(measured_db(&alone, 375.0, RATE) < -15.0);
        assert!(measured_db(&alone, 6000.0, RATE) < -15.0);
    }

    /// `setting` with every band's slope at index `slope`.
    fn values_with_slope(setting: Setting, slope: usize) -> [f32; COUNT] {
        let mut values = values(setting, 0.0);
        for band in 0..BANDS {
            values[param(band, SLOPE)] = slope as f32;
        }
        values
    }

    #[test]
    fn steeper_slopes_add_12_db_per_octave_each() {
        for (slope, expected) in [(0, 12.0), (1, 24.0), (2, 36.0), (3, 48.0)] {
            // Far below the corner, where the slope has settled; the drawn
            // response is what's heard (see the test above), and measuring
            // 100 dB down would only measure rounding.
            let low_cut = values_with_slope(&[(0, BandType::LowCut, 4000.0, 0.0, 0.71)], slope);
            let fall = response_db(&low_cut, 125.0, RATE) - response_db(&low_cut, 62.5, RATE);
            assert!(
                (fall - expected).abs() < 0.5,
                "slope {slope}: {fall:.2} dB an octave"
            );
            // A slope means nothing to a bell.
            let bell = values_with_slope(&[(0, BandType::Bell, 1000.0, 6.0, 1.0)], slope);
            assert!((measured_db(&bell, 1000.0, RATE) - 6.0).abs() < 0.1);
        }
    }

    #[test]
    fn cuts_fall_at_12_db_per_octave() {
        let low_cut = values(&[(0, BandType::LowCut, 1000.0, 0.0, 0.71)], 0.0);
        let slope = measured_db(&low_cut, 125.0, RATE) - measured_db(&low_cut, 62.5, RATE);
        println!("low cut at 1 kHz: {slope:.2} dB from 62.5 to 125 Hz");
        assert!((slope - 12.0).abs() < 0.5, "{slope}");
        assert!(
            (measured_db(&low_cut, 1000.0, RATE) + 3.0).abs() < 0.1,
            "-3 dB at the cutoff"
        );

        let high_cut = values(&[(5, BandType::HighCut, 250.0, 0.0, 0.71)], 0.0);
        let slope = measured_db(&high_cut, 1000.0, RATE) - measured_db(&high_cut, 2000.0, RATE);
        println!("high cut at 250 Hz: {slope:.2} dB from 1 to 2 kHz");
        assert!((slope - 12.0).abs() < 0.5, "{slope}");

        // Q is resonance: a peak of about Q at the cutoff.
        let resonant = values(&[(0, BandType::LowCut, 1000.0, 0.0, 8.0)], 0.0);
        let peak_db = measured_db(&resonant, 1000.0, RATE);
        assert!((peak_db - db(8.0)).abs() < 0.3, "{peak_db}");
    }

    #[test]
    fn flat_or_off_bands_are_transparent() {
        let input = noise(10_000, 7);
        let flat = values(
            &[
                (0, BandType::Bell, 50.0, 0.0, 5.0),
                (1, BandType::LowShelf, 200.0, 0.0, 0.71),
                (2, BandType::HighShelf, 4000.0, 0.0, 2.0),
                (20, BandType::Bell, 900.0, 0.0, 1.0),
            ],
            0.0,
        );
        let all_off = values(&[], 0.0);
        for values in [defaults(PARAMS), flat, all_off] {
            let mut eq = eq_with(&values, RATE);
            let (mut left, mut right) = (input.clone(), input.clone());
            run(&mut eq, &mut left, &mut right, 64, &Context::DEFAULT);
            assert_eq!(left, input);
            assert_eq!(right, input);
            assert!(
                eq.bands.iter().all(|band| !band.active && !band.detecting),
                "flat bands cost nothing"
            );
        }
    }

    #[test]
    fn new_bands_start_off_as_spread_bells() {
        let defaults: [f32; COUNT] = defaults(PARAMS);
        let mut frequencies = Vec::new();
        for band in 6..BANDS {
            assert_eq!(defaults[param(band, ON)], 0.0);
            assert_eq!(
                BandType::from_index(defaults[param(band, TYPE)]),
                BandType::Bell
            );
            assert_eq!(defaults[param(band, Q)], 1.0);
            assert_eq!(defaults[param(band, DYN)], 0.0);
            frequencies.push(defaults[param(band, FREQ)]);
        }
        // Bands 1 to 6 kept their defaults; every default frequency differs.
        assert_eq!(defaults[param(1, FREQ)], 100.0);
        assert_eq!(
            BandType::from_index(defaults[param(4, TYPE)]),
            BandType::HighShelf
        );
        frequencies.extend((0..6).map(|band| defaults[param(band, FREQ)]));
        frequencies.sort_by(f32::total_cmp);
        assert!(
            frequencies.windows(2).all(|w| w[1] > w[0] * 1.15),
            "{frequencies:?}"
        );
        assert_eq!(PARAMS[OUTPUT].id, "output");
        assert_eq!(PARAMS[param(23, RANGE)].id, "b24_range");
    }

    /// Peak output while the frequencies of `setting`'s bands follow
    /// `freq_at(block)`, set once per 64-frame block like automation.
    fn sweep_peak(setting: Setting, freq_at: impl Fn(usize) -> f32, blocks: usize) -> f32 {
        let mut eq = eq_with(&values(setting, 0.0), RATE);
        let input = noise(blocks * 64, 3);
        let (mut left, mut right) = (input.clone(), input);
        let mut loudest = 0.0f32;
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            for &(band, ..) in setting {
                eq.set_param(param(band, FREQ), freq_at(block));
            }
            eq.process(l, r, &Context::DEFAULT);
            assert!(
                l.iter().chain(r.iter()).all(|s| s.is_finite()),
                "block {block}"
            );
            loudest = loudest.max(peak(l)).max(peak(r));
        }
        loudest
    }

    #[test]
    fn fast_frequency_automation_stays_bounded() {
        let settings: [Setting; 3] = [
            &[(2, BandType::Bell, 1000.0, 18.0, 10.0)],
            &[(0, BandType::LowCut, 1000.0, 0.0, 18.0)],
            &[(5, BandType::HighCut, 1000.0, 0.0, 18.0)],
        ];
        // 20 Hz to 20 kHz and back eight times a second.
        let sweep = |block: usize| {
            let t = (block as f32 * 64.0 / RATE * 8.0).fract();
            20.0 * 1000f32.powf(1.0 - (2.0 * t - 1.0).abs())
        };
        let blocks = RATE as usize * 2 / 64;
        for setting in settings {
            let swept = sweep_peak(setting, sweep, blocks);
            // The same filter held still at many frequencies.
            let still = (0..12)
                .map(|i| sweep_peak(setting, |_| 20.0 * 1000f32.powf(i as f32 / 11.0), 400))
                .fold(0.0f32, f32::max);
            println!("{setting:?}: peak {swept:.2} while sweeping, {still:.2} held still");
            assert!(swept < 2.0 * still, "{setting:?}: {swept} against {still}");
        }
    }

    #[test]
    fn switching_bands_on_off_and_type_does_not_click() {
        let mut eq = eq_with(&values(&[(2, BandType::Bell, 200.0, 12.0, 1.0)], 0.0), RATE);
        let input = sine(200.0, 0.25, RATE as usize * 3, RATE);
        let (mut left, mut right) = (input.clone(), input.clone());
        let switches = [
            (0.5, ON, 0.0),
            (1.0, ON, 1.0),
            (1.5, TYPE, 0.0),
            (2.0, FREQ, 2000.0),
            (2.5, TYPE, 2.0),
        ];
        let mut next = 0;
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            let time = block as f32 * 64.0 / RATE;
            if let Some(&(at, which, value)) = switches.get(next)
                && time >= at
            {
                eq.set_param(param(2, which), value);
                next += 1;
            }
            eq.process(l, r, &Context::DEFAULT);
        }
        // The loudest steady state is the 12 dB bell: four times the input,
        // whose largest step is 0.25 · 2π · 200 / 48000.
        let largest_step = left
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        let steady = 4.0 * 0.25 * std::f32::consts::TAU * 200.0 / RATE;
        println!("largest step {largest_step:.4}, steady bell {steady:.4}");
        assert!(largest_step < 1.25 * steady, "{largest_step}");
    }

    /// Makes `band` dynamic with `threshold` and `range`.
    fn dynamic(mut values: [f32; COUNT], band: usize, threshold: f32, range: f32) -> [f32; COUNT] {
        values[param(band, DYN)] = 1.0;
        values[param(band, THRESHOLD)] = threshold;
        values[param(band, RANGE)] = range;
        values
    }

    #[test]
    fn processing_matches_response_db_with_the_dynamic_offsets() {
        // A dynamic bell at 1 kHz keyed by a sidechain tone there, so its
        // movement holds still while the input sweeps every frequency.
        let base = values(
            &[
                (0, BandType::LowShelf, 200.0, -3.0, 0.71),
                (2, BandType::Bell, 1000.0, 6.0, 2.0),
                (9, BandType::HighShelf, 6000.0, 0.0, 0.71),
            ],
            0.0,
        );
        let values = dynamic(dynamic(base, 2, -30.0, -12.0), 9, -40.0, 9.0);
        let mut worst = 0.0f64;
        // -20 dB is 10 dB over: a 10 dB duck; -6 dB would be 24, held to 12.
        // The high shelf's wide band-pass at 6 kHz hears the key 12.5 dB
        // down: 7.5 dB over its threshold, then 21.5, held to its 9.
        for (key_db, expected_offset, shelf_offset) in [(-20.0, -10.0, 7.5), (-6.0, -12.0, 9.0)] {
            let side = sine(1000.0, dsp::db_to_gain(key_db), RATE as usize, RATE);
            let mut last_offsets = [0.0f32; BANDS];
            for freq in log_frequencies(24) {
                let (measured, eq) = measured_with(&values, freq, RATE, Some(&side));
                let mut meters = [0.0; crate::MAX_METERS];
                eq.meters(&mut meters);
                let offsets = &meters[..BANDS];
                let expected = f64::from(response_db_with(&values, offsets, freq as f32, RATE));
                let error = (measured - expected).abs();
                worst = worst.max(error);
                assert!(
                    error < 0.15,
                    "key {key_db} dB, {freq:.0} Hz: measured {measured:.2} dB, drawn {expected:.2} dB"
                );
                last_offsets.copy_from_slice(offsets);
            }
            println!("key at {key_db} dB: offsets {:?}", &last_offsets[..10]);
            assert!(
                (last_offsets[2] - expected_offset).abs() < 0.3,
                "{last_offsets:?}"
            );
            assert!(
                (last_offsets[9] - shelf_offset).abs() < 0.7,
                "{last_offsets:?}"
            );
        }
        println!("largest difference between processing and response_db_with: {worst:.4} dB");
        assert_eq!(
            response_db_with(&values, &[0.0; BANDS], 500.0, RATE),
            response_db(&values, 500.0, RATE)
        );
    }

    /// The meters after a second of a sine at `freq` and `level_db`.
    fn meters_after(values: &[f32; COUNT], freq: f64, level_db: f32) -> [f32; crate::MAX_METERS] {
        let mut eq = eq_with(values, RATE);
        let input = sine(freq, dsp::db_to_gain(level_db), RATE as usize, RATE);
        let (mut left, mut right) = (input.clone(), input);
        run(&mut eq, &mut left, &mut right, 64, &Context::DEFAULT);
        assert!(left.iter().all(|s| s.is_finite()));
        let mut meters = [0.0; crate::MAX_METERS];
        eq.meters(&mut meters);
        meters
    }

    #[test]
    fn dynamic_bands_hear_their_own_frequency_only() {
        let values = dynamic(
            values(&[(4, BandType::Bell, 1000.0, 0.0, 2.0)], 0.0),
            4,
            -30.0,
            -12.0,
        );
        let at_home = meters_after(&values, 1000.0, -10.0);
        // Three octaves up the band-pass is 24 dB down: under the threshold.
        let far = meters_after(&values, 8000.0, -10.0);
        let below = meters_after(&values, 125.0, -10.0);
        println!(
            "band 5's movement: {} dB for 1 kHz, {} dB for 8 kHz, {} dB for 125 Hz",
            at_home[4], far[4], below[4]
        );
        assert!((at_home[4] + 12.0).abs() < 0.01);
        assert_eq!(far[4], 0.0);
        assert_eq!(below[4], 0.0);
        // And the 1 kHz tone comes out 12 dB down.
        let (measured, _) = measured_with(&values, 1000.0, RATE, None);
        assert!((measured + 12.0).abs() < 0.2, "{measured}");
        // The output meters read the output's sample peaks (an 8 kHz sine's
        // fall up to 1.25 dB under its true peak).
        assert!((at_home[METER_PEAK_LEFT] + 22.0).abs() < 0.2);
        assert!((at_home[METER_PEAK_RIGHT] + 22.0).abs() < 0.2);
        assert!(far[METER_PEAK_RIGHT] < -10.0 && far[METER_PEAK_RIGHT] > -11.3);

        // Upward: a positive range lifts the band when it gets loud.
        let lifting = dynamic(values, 4, -30.0, 6.0);
        assert!((meters_after(&lifting, 1000.0, -10.0)[4] - 6.0).abs() < 0.01);
    }

    #[test]
    fn dynamic_bands_retune_when_the_sample_rate_changes() {
        let values = dynamic(
            values(&[(0, BandType::Bell, 1000.0, 0.0, 2.0)], 0.0),
            0,
            -30.0,
            -12.0,
        );
        let mut eq = eq_with(&values, RATE);
        let tone = |rate: f32| sine(1000.0, dsp::db_to_gain(-10.0), rate as usize, rate);
        let (mut left, mut right) = (tone(RATE), tone(RATE));
        run(&mut eq, &mut left, &mut right, 64, &Context::DEFAULT);
        assert!((eq.meter() + 12.0).abs() < 0.01);
        // Twice the rate: a detector left as it was would listen at 2 kHz
        // and hear the 1 kHz tone 10 dB down.
        eq.prepare(96_000.0, 64);
        let (mut left, mut right) = (tone(96_000.0), tone(96_000.0));
        run(&mut eq, &mut left, &mut right, 64, &Context::DEFAULT);
        assert!((eq.meter() + 12.0).abs() < 0.01, "{}", eq.meter());
    }

    #[test]
    fn dynamic_attack_and_release_follow_their_settings() {
        for (attack, release) in [(5.0, 100.0), (20.0, 500.0), (1.0, 50.0)] {
            // Band 1, whose movement is the first meter.
            let mut values = dynamic(
                values(&[(0, BandType::Bell, 1000.0, 0.0, 2.0)], 0.0),
                0,
                -40.0,
                -12.0,
            );
            values[ATTACK] = attack;
            values[RELEASE] = release;
            let mut eq = eq_with(&values, RATE);
            // A key tone 30 dB over the threshold from 0.5 s to 1.5 s, then
            // 1.5 s of silence; the movement saturates at the range.
            let len = RATE as usize * 3;
            let (on, off) = (len / 6, len / 2);
            let tone = sine(1000.0, dsp::db_to_gain(-10.0), len, RATE);
            let key: Vec<f32> = tone
                .iter()
                .enumerate()
                .map(|(n, &s)| if (on..off).contains(&n) { s } else { 0.0 })
                .collect();
            let input = noise(len, 5);
            let (mut left, mut right) = (input.clone(), input);
            let mut movement = Vec::new();
            for start in (0..len).step_by(CHUNK) {
                let end = start + CHUNK;
                let context = Context {
                    sidechain: Some((&key[start..end], &key[start..end])),
                    ..Context::DEFAULT
                };
                eq.process(&mut left[start..end], &mut right[start..end], &context);
                movement.push(-eq.meter());
            }
            let time_constant_ms = |from: usize, end: f32| {
                let start = movement[from];
                let mark = start + (end - start) * (1.0 - (-1.0f32).exp());
                let chunks = movement[from..]
                    .iter()
                    .position(|&m| if end > start { m >= mark } else { m <= mark })
                    .unwrap();
                (chunks + 1) as f32 * CHUNK as f32 / RATE * 1000.0
            };
            let measured_attack = time_constant_ms(on / CHUNK, 12.0);
            let measured_release = time_constant_ms(off / CHUNK, 0.0);
            println!(
                "dynamic attack {attack} ms measured {measured_attack:.2} ms, release {release} ms measured {measured_release:.1} ms"
            );
            assert!(
                (measured_attack - attack).abs() < 0.15 * attack + 0.7,
                "{measured_attack}"
            );
            assert!(
                (measured_release - release).abs() < 0.1 * release + 1.0,
                "{measured_release}"
            );
            // 1.5 s after the key stopped, an exponential release.
            let left_over = *movement.last().unwrap();
            let expected = 12.0 * (-1500.0 / release).exp();
            assert!(
                (left_over - expected).abs() < 0.1 * expected + 1e-3,
                "{left_over} against {expected}"
            );
        }
    }

    #[test]
    fn silence_stays_silent_with_every_band_dynamic() {
        let mut values: [f32; COUNT] = defaults(PARAMS);
        for band in 0..BANDS {
            values[param(band, ON)] = 1.0;
            values[param(band, TYPE)] = 2.0;
            values[param(band, GAIN)] = 12.0;
            values = dynamic(values, band, -60.0, 24.0);
        }
        let mut eq = eq_with(&values, RATE);
        let (mut left, mut right) = (vec![0.0; 4800], vec![0.0; 4800]);
        run(&mut eq, &mut left, &mut right, 64, &Context::DEFAULT);
        assert!(left.iter().chain(&right).all(|&s| s == 0.0));
        let mut meters = [0.0; crate::MAX_METERS];
        eq.meters(&mut meters);
        assert!(meters[..BANDS].iter().all(|&m| m == 0.0));
        assert_eq!(meters[METER_PEAK_LEFT], -100.0);
    }
}
