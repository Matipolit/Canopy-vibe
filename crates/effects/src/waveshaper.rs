//! Waveshaper: drives the sound into a shaping curve (soft or hard
//! clipping, a tube-like asymmetric curve, two kinds of wavefolding, a
//! rectifier or a bitcrusher), with a bias that moves where on the curve it
//! sits, then a tone control (a lowpass), an output level and a dry/wet mix.
//! Its UI shows the curve, drawn with [`curve`], and an oscilloscope of the
//! output from the tap.
//!
//! A curve makes harmonics far above the input's highest frequency; those
//! past half the sample rate would fold back down as inharmonic aliasing.
//! So the curve can run at 2, 4 or 8 times the sample rate: each doubling is
//! a pair of halfband filters (see `creative_halfband.rs`) that remove what
//! lies above the audio band before coming back down. The dry signal is
//! mixed in at the high rate as well, before the filters, so a parallel
//! blend has no comb filtering from their phase.
//!
//! Bias alone would add a constant offset, so the curve's value at the bias
//! is taken away: silence stays exactly silent. The DC an asymmetric curve
//! still makes with signal (it rises with the level) is removed by a 10 Hz
//! high-pass. Switching the shape or the oversampling crossfades over a few
//! milliseconds instead of jumping; drive, bias, tone, output and mix glide,
//! at the knob's pace or, for a modulator, within a block (see "Modulation"
//! in `lib.rs`).
//!
//! **Drawn** is a curve of the user's own: [`CURVE_POINTS`] points spread
//! evenly over inputs from -1 to 1, each the output there (stored `0..=1`
//! for -1 to 1, as the Shaper keeps its shapes), read with straight lines
//! between them and held at the ends beyond full scale. Each point glides
//! like any knob, so redrawing it while it plays doesn't click. FL Studio's
//! Fruity WaveShaper comes in on it.

use instruments::ParamDescriptor;

use crate::halfband::iir::Design;
use crate::shaping::{self, CRUSH_BITS, DC_HZ, Mode, OVERSAMPLING, Oversampler, STAGES, bend};
use crate::{Category, Context, Effect, EffectDescriptor, Values, choice, float};
use dsp::one_pole::{DcBlocker, pole};
use dsp::svf::{BUTTERWORTH_Q, Coefs, Shape, SvfState};
use dsp::{Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.waveshaper",
    name: "Waveshaper",
    description: "Drive into a clipping, folding or crushing curve, oversampled to stay clean",
    category: Category::Distortion,
    params: PARAMS,
    levels: crate::meter::Levels::BOTH,
    tap: true,
    create: || Box::new(Waveshaper::new()),
    ..EffectDescriptor::BASE
};

/// The curves, in the order of the `shape` parameter's labels: some of the
/// distortion's.
pub const CURVES: [Mode; 7] = [
    Mode::SoftClip,
    Mode::HardClip,
    Mode::Tube,
    Mode::LinearFold,
    Mode::SineFold,
    Mode::Rectify,
    Mode::BitCrush,
];

/// The `shape` choice that plays the drawn curve, after [`CURVES`].
pub const DRAWN: usize = CURVES.len();

/// What a shape plays: one of the built-in curves, or the drawn one.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Curve {
    Built(Mode),
    Drawn,
}

/// The curve a `shape` value picks.
fn curve_at(index: f32) -> Curve {
    let index = index.round().max(0.0) as usize;
    match CURVES.get(index) {
        Some(mode) => Curve::Built(*mode),
        None => Curve::Drawn,
    }
}

/// How many points the drawn curve has, from input -1 to 1.
pub const CURVE_POINTS: usize = 65;

/// The drawn curve's output (-1 to 1) for input `x`: straight lines
/// between its points (`0..=1`, for -1 to 1), held beyond them.
#[inline(always)]
pub fn drawn_at(points: &[f32; CURVE_POINTS], x: f32) -> f32 {
    let position = (x.clamp(-1.0, 1.0) + 1.0) * 0.5 * (CURVE_POINTS - 1) as f32;
    let (index, _) = dsp::split_index(position);
    let index = index.min(CURVE_POINTS - 2);
    let t = position - index as f32;
    let (a, b) = (points[index], points[index + 1]);
    2.0 * (a + (b - a) * t) - 1.0
}

/// A curve's value at `x`.
#[inline(always)]
fn curve_value(curve: Curve, x: f32, levels: f32, points: &[f32; CURVE_POINTS]) -> f32 {
    match curve {
        Curve::Built(mode) => bend(mode, x, levels),
        Curve::Drawn => drawn_at(points, x),
    }
}

const SHAPES: &[&str] = &[
    "Soft clip",
    "Hard clip",
    "Tube",
    "Foldback",
    "Sine fold",
    "Rectify",
    "Bitcrush",
    "Drawn",
];

const KNOBS: [ParamDescriptor; CURVE_START] = [
    float("drive", "Drive", 0.0, 48.0, 12.0, "dB", false),
    choice("shape", "Shape", SHAPES, 0),
    float("bias", "Bias", -100.0, 100.0, 0.0, "%", false),
    choice("oversampling", "Oversampling", OVERSAMPLING, 2),
    float("tone", "Tone", 200.0, 20_000.0, 20_000.0, "Hz", true),
    float("output", "Output", -24.0, 24.0, 0.0, "dB", false),
    float("mix", "Mix", 0.0, 100.0, 100.0, "%", false),
];

/// The drawn curve's points, after the knobs.
pub const CURVE_START: usize = 7;
const COUNT: usize = CURVE_START + CURVE_POINTS;

const CURVE_IDS: [&str; CURVE_POINTS] = [
    "curve_p0",
    "curve_p1",
    "curve_p2",
    "curve_p3",
    "curve_p4",
    "curve_p5",
    "curve_p6",
    "curve_p7",
    "curve_p8",
    "curve_p9",
    "curve_p10",
    "curve_p11",
    "curve_p12",
    "curve_p13",
    "curve_p14",
    "curve_p15",
    "curve_p16",
    "curve_p17",
    "curve_p18",
    "curve_p19",
    "curve_p20",
    "curve_p21",
    "curve_p22",
    "curve_p23",
    "curve_p24",
    "curve_p25",
    "curve_p26",
    "curve_p27",
    "curve_p28",
    "curve_p29",
    "curve_p30",
    "curve_p31",
    "curve_p32",
    "curve_p33",
    "curve_p34",
    "curve_p35",
    "curve_p36",
    "curve_p37",
    "curve_p38",
    "curve_p39",
    "curve_p40",
    "curve_p41",
    "curve_p42",
    "curve_p43",
    "curve_p44",
    "curve_p45",
    "curve_p46",
    "curve_p47",
    "curve_p48",
    "curve_p49",
    "curve_p50",
    "curve_p51",
    "curve_p52",
    "curve_p53",
    "curve_p54",
    "curve_p55",
    "curve_p56",
    "curve_p57",
    "curve_p58",
    "curve_p59",
    "curve_p60",
    "curve_p61",
    "curve_p62",
    "curve_p63",
    "curve_p64",
];

const CURVE_NAMES: [&str; CURVE_POINTS] = [
    "Curve point 1",
    "Curve point 2",
    "Curve point 3",
    "Curve point 4",
    "Curve point 5",
    "Curve point 6",
    "Curve point 7",
    "Curve point 8",
    "Curve point 9",
    "Curve point 10",
    "Curve point 11",
    "Curve point 12",
    "Curve point 13",
    "Curve point 14",
    "Curve point 15",
    "Curve point 16",
    "Curve point 17",
    "Curve point 18",
    "Curve point 19",
    "Curve point 20",
    "Curve point 21",
    "Curve point 22",
    "Curve point 23",
    "Curve point 24",
    "Curve point 25",
    "Curve point 26",
    "Curve point 27",
    "Curve point 28",
    "Curve point 29",
    "Curve point 30",
    "Curve point 31",
    "Curve point 32",
    "Curve point 33",
    "Curve point 34",
    "Curve point 35",
    "Curve point 36",
    "Curve point 37",
    "Curve point 38",
    "Curve point 39",
    "Curve point 40",
    "Curve point 41",
    "Curve point 42",
    "Curve point 43",
    "Curve point 44",
    "Curve point 45",
    "Curve point 46",
    "Curve point 47",
    "Curve point 48",
    "Curve point 49",
    "Curve point 50",
    "Curve point 51",
    "Curve point 52",
    "Curve point 53",
    "Curve point 54",
    "Curve point 55",
    "Curve point 56",
    "Curve point 57",
    "Curve point 58",
    "Curve point 59",
    "Curve point 60",
    "Curve point 61",
    "Curve point 62",
    "Curve point 63",
    "Curve point 64",
    "Curve point 65",
];

/// The knobs, then the drawn curve's points, a straight line by default.
static ALL: [ParamDescriptor; COUNT] = {
    let mut all = [KNOBS[0]; COUNT];
    let mut i = 0;
    while i < CURVE_START {
        all[i] = KNOBS[i];
        i += 1;
    }
    let mut k = 0;
    while k < CURVE_POINTS {
        let straight = k as f32 / (CURVE_POINTS - 1) as f32;
        all[CURVE_START + k] = float(CURVE_IDS[k], CURVE_NAMES[k], 0.0, 1.0, straight, "", false);
        k += 1;
    }
    all
};

const PARAMS: &[ParamDescriptor] = &ALL;

const DRIVE: usize = 0;
const SHAPE: usize = 1;
const BIAS: usize = 2;
const OVERSAMPLE: usize = 3;
const TONE: usize = 4;
const OUTPUT: usize = 5;
const MIX: usize = 6;

/// How long drive, bias, output and mix take to reach a new value.
const SMOOTH_MS: f32 = 20.0;
/// How long the tone filter takes to glide to a new cutoff.
const TONE_GLIDE_MS: f32 = 30.0;
/// The tone filter's coefficients follow their glide every this many samples.
const CHUNK: usize = 16;
/// How long switching the shape crossfades.
const SHAPE_FADE_MS: f32 = 15.0;
/// How long switching the oversampling crossfades.
const OVERSAMPLING_FADE_MS: f32 = 20.0;

/// The gain drive applies to a curve's input, and the bitcrusher's levels,
/// for `drive_db`. The bitcrusher takes drive as how coarse to be instead of
/// as gain: more drive, fewer bits.
fn drive_terms(drive_db: f32) -> (f32, f32) {
    let gain = dsp::db_to_gain(drive_db);
    let amount = (drive_db / 48.0).clamp(0.0, 1.0);
    let bits = CRUSH_BITS.0 + (CRUSH_BITS.1 - CRUSH_BITS.0) * amount;
    (gain, (bits - 1.0).exp2())
}

/// The gain into `curve` for a drive gain of `gain`.
#[inline]
fn input_gain(curve: Curve, gain: f32) -> f32 {
    if curve == Curve::Built(Mode::BitCrush) {
        1.0
    } else {
        gain
    }
}

/// The waveshaper's static transfer function: what an input sample `x`
/// (1 = 0 dBFS) becomes at parameter values `values` (in [`DESCRIPTOR`]'s
/// order), drive, shape, bias, output and mix included; the tone filter
/// and the DC high-pass, which depend on frequency, aren't. What the UI
/// draws. Missing or NaN values count as defaults.
pub fn curve(values: &[f32], x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    let value = |index: usize| crate::param_value(PARAMS, values, index);
    let curve = curve_at(value(SHAPE));
    let points: [f32; CURVE_POINTS] = std::array::from_fn(|k| value(CURVE_START + k));
    let (gain, levels) = drive_terms(value(DRIVE));
    let bias = value(BIAS) / 100.0;
    let at = |x: f32| curve_value(curve, x, levels, &points);
    let wet = at(input_gain(curve, gain) * x + bias) - at(bias);
    let mix = value(MIX) / 100.0;
    x + mix * (dsp::db_to_gain(value(OUTPUT)) * wet - x)
}

/// What every sub-sample of one input sample is shaped with.
#[derive(Debug, Clone, Copy)]
struct Bend {
    /// The curve heard, and the one faded to while switching (the same
    /// otherwise), with how far the fade has got.
    from: Curve,
    to: Curve,
    fade: f32,
    gain_from: f32,
    gain_to: f32,
    /// Each curve's value at the bias alone, taken away so silence stays
    /// silent.
    offset_from: f32,
    offset_to: f32,
    bias: f32,
    levels: f32,
    output: f32,
    mix: f32,
}

impl Bend {
    /// A placeholder, before a block's samples are worked out.
    const NONE: Bend = Bend {
        from: Curve::Drawn,
        to: Curve::Drawn,
        fade: 0.0,
        gain_from: 0.0,
        gain_to: 0.0,
        offset_from: 0.0,
        offset_to: 0.0,
        bias: 0.0,
        levels: 1.0,
        output: 0.0,
        mix: 0.0,
    };
}

/// The signal path at one oversampling factor, for both channels: up, the
/// curve, DC high-pass and tone, the mix, and down again.
#[derive(Debug, Clone)]
struct Path {
    oversampler: Oversampler,
    dc: [DcBlocker; 2],
    /// The DC high-pass's pole, at this path's rate.
    dc_pole: f32,
    tone: [SvfState; 2],
    tone_coefs: Coefs,
}

impl Path {
    fn new(stages: usize) -> Path {
        Path {
            oversampler: Oversampler::new(stages),
            dc: [DcBlocker::default(); 2],
            dc_pole: 1.0,
            tone_coefs: Coefs::IDENTITY,
            tone: [SvfState::default(); 2],
        }
    }

    fn rate(&self, sample_rate: f32) -> f32 {
        sample_rate * self.oversampler.factor() as f32
    }

    fn clear(&mut self) {
        self.oversampler.clear();
        self.dc = [DcBlocker::default(); 2];
        self.tone = [SvfState::default(); 2];
    }

    fn flush(&mut self) {
        self.oversampler.flush();
        for dc in &mut self.dc {
            dc.flush();
        }
        for state in &mut self.tone {
            state.flush();
        }
    }

    /// A block of each channel, `x`, through the path into `out`, each
    /// input sample bent as its `bends` says, with the drawn curve's
    /// `points`: what sample by sample would give, to the bit. A block
    /// whose curve holds (no switch) runs [`run_plain`] for that curve.
    fn run(
        &mut self,
        designs: &[Design; STAGES],
        bends: &[Bend],
        points: &[f32; CURVE_POINTS],
        x: [&[f32]; 2],
        out: [&mut [f32]; 2],
    ) {
        let shift = self.oversampler.factor().trailing_zeros();
        let Path {
            oversampler,
            dc,
            dc_pole,
            tone,
            tone_coefs,
        } = self;
        let curve = bends[0].from;
        let plain = bends.iter().all(|b| b.from == curve && b.to == curve);
        oversampler.run(designs, x, out, |channel, high| {
            let (dc, tone) = (&mut dc[channel], &mut tone[channel]);
            if !plain {
                for (i, x) in high.iter_mut().enumerate() {
                    *x = shape(
                        &bends[i >> shift],
                        points,
                        dc,
                        *dc_pole,
                        tone,
                        tone_coefs,
                        *x,
                    );
                }
                return;
            }
            macro_rules! plain {
                ($($mode:ident),*) => {
                    match curve {
                        $(Curve::Built(Mode::$mode) => run_plain(
                            Curve::Built(Mode::$mode), bends, shift, points, dc, *dc_pole, tone,
                            tone_coefs, high,
                        ),)*
                        Curve::Drawn => run_plain(
                            Curve::Drawn, bends, shift, points, dc, *dc_pole, tone, tone_coefs,
                            high,
                        ),
                    }
                };
            }
            plain!(
                Tube, SoftClip, HardClip, Diode1, Diode2, LinearFold, SineFold, ZeroSquare,
                Downsample, Asym, Rectify, SineShaper, StompBox, TapeSat, Overdrive, SoftSat,
                BitCrush
            );
        });
    }
}

/// One sub-sample through the curve, the DC high-pass, the tone filter,
/// the output level and the mix.
#[inline(always)]
fn shape(
    p: &Bend,
    points: &[f32; CURVE_POINTS],
    dc: &mut DcBlocker,
    dc_pole: f32,
    tone: &mut SvfState,
    tone_coefs: &Coefs,
    dry: f32,
) -> f32 {
    let mut wet = curve_value(p.from, p.gain_from * dry + p.bias, p.levels, points) - p.offset_from;
    if p.from != p.to {
        let to = curve_value(p.to, p.gain_to * dry + p.bias, p.levels, points) - p.offset_to;
        wet += p.fade * (to - wet);
    }
    let blocked = dc.tick(wet, dc_pole);
    let toned = tone.tick(tone_coefs, blocked);
    dry + p.mix * (p.output * toned - dry)
}

/// [`shape`] over a block of sub-samples (`1 << shift` per input sample)
/// whose curve holds at `curve`, known here so its `match` is settled once.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn run_plain(
    curve: Curve,
    bends: &[Bend],
    shift: u32,
    points: &[f32; CURVE_POINTS],
    dc: &mut DcBlocker,
    dc_pole: f32,
    tone: &mut SvfState,
    tone_coefs: &Coefs,
    high: &mut [f32],
) {
    let (mut dc_state, mut tone_state) = (*dc, *tone);
    for (i, x) in high.iter_mut().enumerate() {
        let p = &bends[i >> shift];
        let dry = *x;
        let wet = curve_value(curve, p.gain_from * dry + p.bias, p.levels, points) - p.offset_from;
        let blocked = dc_state.tick(wet, dc_pole);
        let toned = tone_state.tick(tone_coefs, blocked);
        *x = dry + p.mix * (p.output * toned - dry);
    }
    (*dc, *tone) = (dc_state, tone_state);
}

pub struct Waveshaper {
    values: Values<COUNT>,
    sample_rate: f32,
    designs: [Design; STAGES],
    /// A path per oversampling choice.
    paths: [Path; 4],
    /// The oversampling heard, and while switching the one fading out.
    active: usize,
    fading_from: Option<usize>,
    /// How far the switch has got (0 to 1) and how far it moves per sample.
    switch: f32,
    switch_step: f32,
    curve: Curve,
    next_curve: Curve,
    curve_fade: f32,
    /// The drawn curve's points as played, each gliding to its knob, and
    /// whether any is still on its way; bumped whenever they move, so the
    /// offsets know to follow.
    points: [f32; CURVE_POINTS],
    point_glides: [Smoothed; CURVE_POINTS],
    points_moving: bool,
    points_version: u32,
    curve_step: f32,
    drive_db: Smoothed,
    drive_gain: f32,
    levels: f32,
    bias: Smoothed,
    /// Each curve's value at the bias alone (`Bend::offset_from` and
    /// `offset_to`), and the curves, bias and levels they were worked out
    /// for.
    offsets: (f32, f32),
    offsets_for: Option<(Curve, Curve, u32, u32, u32)>,
    output: Smoothed,
    mix: Smoothed,
    log_tone: Smoothed,
    /// Samples until the tone filter next follows its glide.
    countdown: usize,
}

impl Waveshaper {
    pub fn new() -> Waveshaper {
        let mut waveshaper = Waveshaper {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            designs: shaping::designs(),
            paths: [0, 1, 2, 3].map(Path::new),
            active: 0,
            fading_from: None,
            switch: 0.0,
            switch_step: 1.0,
            curve: Curve::Built(Mode::SoftClip),
            next_curve: Curve::Built(Mode::SoftClip),
            curve_fade: 0.0,
            points: [0.0; CURVE_POINTS],
            point_glides: std::array::from_fn(|_| Smoothed::new(0.0, SMOOTH_MS)),
            points_moving: false,
            points_version: 0,
            curve_step: 1.0,
            drive_db: Smoothed::new(0.0, SMOOTH_MS),
            drive_gain: 1.0,
            levels: 1.0,
            bias: Smoothed::new(0.0, SMOOTH_MS),
            offsets: (0.0, 0.0),
            offsets_for: None,
            output: Smoothed::new(1.0, SMOOTH_MS),
            mix: Smoothed::new(1.0, SMOOTH_MS),
            log_tone: Smoothed::new(0.0, TONE_GLIDE_MS),
            countdown: 0,
        };
        for index in 0..COUNT {
            waveshaper.apply(index, Pace::Knob);
        }
        waveshaper.prepare(48_000.0, 0);
        waveshaper
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            DRIVE => self.drive_db.move_to(value, pace),
            BIAS => self.bias.move_to(value / 100.0, pace),
            TONE => self.log_tone.move_to(value.log2(), pace),
            OUTPUT => self.output.move_to(dsp::db_to_gain(value), pace),
            MIX => self.mix.move_to(value / 100.0, pace),
            CURVE_START.. => {
                self.point_glides[index - CURVE_START].move_to(value, pace);
                self.points_moving = true;
            }
            // Shape and oversampling switch with a crossfade, started in
            // `process`.
            _ => {}
        }
    }

    /// Moves the drawn curve's points a chunk on, while any is moving.
    fn update_points(&mut self) {
        if !self.points_moving {
            return;
        }
        let mut moving = false;
        for (point, glide) in self.points.iter_mut().zip(&mut self.point_glides) {
            if glide.is_smoothing() {
                *point = glide.tick();
                moving |= glide.is_smoothing();
            }
        }
        self.points_moving = moving;
        self.points_version = self.points_version.wrapping_add(1);
    }

    fn wanted_curve(&self) -> Curve {
        curve_at(self.values[SHAPE])
    }

    fn wanted_path(&self) -> usize {
        (self.values[OVERSAMPLE].round().max(0.0) as usize).min(STAGES)
    }

    fn update_drive(&mut self) {
        (self.drive_gain, self.levels) = drive_terms(self.drive_db.current());
    }

    fn update_tone(&mut self, force: bool) {
        if force || self.log_tone.is_smoothing() {
            let hz = self.log_tone.tick().exp2();
            for path in &mut self.paths {
                let rate = path.rate(self.sample_rate);
                path.tone_coefs = Coefs::new(&Shape::lowpass(hz, BUTTERWORTH_Q, rate));
            }
        }
    }

    /// The next input sample's bend: every per-sample setting moved on a
    /// sample, and a curve switch moved on.
    #[inline]
    fn next_bend(&mut self) -> Bend {
        if self.drive_db.is_smoothing() {
            self.drive_db.tick();
            self.update_drive();
        }
        let bias = self.bias.tick();
        let (from, to) = (self.curve, self.next_curve);
        // The curves' values at the bias alone change only when the
        // bias, the curves or the drive do, which is seldom: not a
        // curve evaluation a sample.
        let key = (
            from,
            to,
            bias.to_bits(),
            self.levels.to_bits(),
            self.points_version,
        );
        if self.offsets_for != Some(key) {
            self.offsets = (
                curve_value(from, bias, self.levels, &self.points),
                curve_value(to, bias, self.levels, &self.points),
            );
            self.offsets_for = Some(key);
        }
        let bend = Bend {
            from,
            to,
            fade: self.curve_fade,
            gain_from: input_gain(from, self.drive_gain),
            gain_to: input_gain(to, self.drive_gain),
            offset_from: self.offsets.0,
            offset_to: self.offsets.1,
            bias,
            levels: self.levels,
            output: self.output.tick(),
            mix: self.mix.tick(),
        };
        if from != to {
            self.curve_fade += self.curve_step;
            if self.curve_fade >= 1.0 {
                self.curve = to;
                self.curve_fade = 0.0;
            }
        }
        bend
    }

    /// Starts switching the shape or oversampling if asked and not already
    /// switching; a change that arrives mid-switch waits for it to finish.
    fn start_switches(&mut self) {
        let wanted = self.wanted_curve();
        if self.curve == self.next_curve && wanted != self.curve {
            self.next_curve = wanted;
            self.curve_fade = 0.0;
        }
        let wanted = self.wanted_path();
        if self.fading_from.is_none() && wanted != self.active {
            self.paths[wanted].clear();
            self.fading_from = Some(self.active);
            self.active = wanted;
            self.switch = 0.0;
        }
    }
}

impl Default for Waveshaper {
    fn default() -> Self {
        Waveshaper::new()
    }
}

impl Effect for Waveshaper {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        for smoothed in [
            &mut self.drive_db,
            &mut self.bias,
            &mut self.output,
            &mut self.mix,
        ] {
            smoothed.prepare(sample_rate);
        }
        self.log_tone.prepare_every(sample_rate, CHUNK);
        for glide in &mut self.point_glides {
            glide.prepare_every(sample_rate, CHUNK);
        }
        for path in &mut self.paths {
            let rate = path.rate(sample_rate);
            path.dc_pole = pole(DC_HZ, rate);
        }
        self.curve_step = 1.0 / (SHAPE_FADE_MS * 0.001 * sample_rate).max(1.0);
        self.switch_step = 1.0 / (OVERSAMPLING_FADE_MS * 0.001 * sample_rate).max(1.0);
        self.reset();
    }

    fn reset(&mut self) {
        for smoothed in [
            &mut self.drive_db,
            &mut self.bias,
            &mut self.output,
            &mut self.mix,
            &mut self.log_tone,
        ] {
            smoothed.settle();
        }
        self.update_drive();
        for (point, glide) in self.points.iter_mut().zip(&mut self.point_glides) {
            glide.settle();
            *point = glide.current();
        }
        self.points_moving = false;
        self.points_version = self.points_version.wrapping_add(1);
        self.curve = self.wanted_curve();
        self.next_curve = self.curve;
        self.curve_fade = 0.0;
        self.active = self.wanted_path();
        self.fading_from = None;
        self.switch = 0.0;
        for path in &mut self.paths {
            path.clear();
        }
        self.update_tone(true);
        self.countdown = 0;
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.values.played();
        self.start_switches();
        let len = left.len().min(right.len());
        // A non-finite sample would stick in the filters forever.
        let clean = |x: f32| if x.is_finite() { x } else { 0.0 };
        // A stretch at a time between the tone's and the drawn curve's
        // updates: each sample's bend first, then the paths over the
        // stretch, then the oversampling switch's crossfade.
        let mut start = 0;
        while start < len {
            if self.countdown == 0 {
                self.update_tone(false);
                self.update_points();
                self.countdown = CHUNK;
            }
            let n = self.countdown.min(len - start);
            let (left, right) = (&mut left[start..start + n], &mut right[start..start + n]);
            start += n;
            let mut bends = [Bend::NONE; CHUNK];
            let mut x = [[0.0f32; CHUNK]; 2];
            for i in 0..n {
                self.countdown -= 1;
                bends[i] = self.next_bend();
                x[0][i] = clean(left[i]);
                x[1][i] = clean(right[i]);
            }
            let (designs, points) = (&self.designs, &self.points);
            let [x_l, x_r] = &x;
            let mut y = [[0.0f32; CHUNK]; 2];
            let mut z = [[0.0f32; CHUNK]; 2];
            {
                let [y_l, y_r] = &mut y;
                self.paths[self.active].run(
                    designs,
                    &bends[..n],
                    points,
                    [&x_l[..n], &x_r[..n]],
                    [&mut y_l[..n], &mut y_r[..n]],
                );
            }
            if let Some(old) = self.fading_from {
                let [z_l, z_r] = &mut z;
                self.paths[old].run(
                    designs,
                    &bends[..n],
                    points,
                    [&x_l[..n], &x_r[..n]],
                    [&mut z_l[..n], &mut z_r[..n]],
                );
            }
            for i in 0..n {
                let mut out = [y[0][i], y[1][i]];
                if self.fading_from.is_some() {
                    for (y, z) in out.iter_mut().zip([z[0][i], z[1][i]]) {
                        *y = z + self.switch * (*y - z);
                    }
                    self.switch += self.switch_step;
                    if self.switch >= 1.0 {
                        self.fading_from = None;
                    }
                }
                left[i] = out[0];
                right[i] = out[1];
            }
        }
        // Only the paths running carry anything to flush: the others
        // were cleared when they were left.
        self.paths[self.active].flush();
        if let Some(old) = self.fading_from {
            self.paths[old].flush();
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
    use crate::testing::{RATE, peak, run, run_measured, set, sine, values};

    fn waveshaper(settings: &[(&str, f32)]) -> Waveshaper {
        let mut waveshaper = Waveshaper::new();
        waveshaper.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut waveshaper, id, value);
        }
        waveshaper.reset();
        waveshaper
    }

    /// The power of every DFT bin of `x` up to `last`, each as its share of
    /// the mean square.
    fn spectrum(x: &[f32], last: usize) -> Vec<f64> {
        let n = x.len();
        let twiddles: Vec<(f64, f64)> = (0..n)
            .map(|i| {
                let phase = std::f64::consts::TAU * i as f64 / n as f64;
                (phase.cos(), phase.sin())
            })
            .collect();
        (0..=last)
            .map(|bin| {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for (i, &s) in x.iter().enumerate() {
                    let (cos, sin) = twiddles[bin * i % n];
                    re += f64::from(s) * cos;
                    im -= f64::from(s) * sin;
                }
                2.0 * (re * re + im * im) / (n * n) as f64
            })
            .collect()
    }

    /// Aliasing in the audible band, in dB relative to the harmonics: a
    /// sine at exactly bin `BIN` of a `PERIOD`-sample window is shaped, and
    /// once the output settles into repeating every period, whatever lies
    /// below 20 kHz but off the harmonics is aliasing. (What folds into
    /// 20-24 kHz can't be heard; the halfbands' transition band lets some
    /// of it through.)
    fn aliasing_db(settings: &[(&str, f32)]) -> f64 {
        const PERIOD: usize = 4096;
        const BIN: usize = 431; // 5050.8 Hz at 48 kHz; prime, so aliases spread.
        let mut waveshaper = waveshaper(settings);
        let input = sine(
            f64::from(RATE) * BIN as f64 / PERIOD as f64,
            0.5,
            PERIOD * 6,
            RATE,
        );
        let (mut left, mut right) = (input.clone(), input);
        run(
            &mut waveshaper,
            &mut left,
            &mut right,
            64,
            &Context::DEFAULT,
        );
        let window = &left[left.len() - PERIOD..];
        let audible = (20_000.0 / RATE * PERIOD as f32) as usize;
        let bins = spectrum(window, audible);
        let (mut harmonics, mut aliases) = (0.0, 0.0);
        for (bin, power) in bins.iter().enumerate().skip(1) {
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
        let cases: [(&str, f32, f32); 4] = [
            ("soft clip", 0.0, 18.0),
            ("hard clip", 1.0, 12.0),
            ("tube", 2.0, 18.0),
            ("sine fold", 4.0, 24.0),
        ];
        for (name, shape, drive) in cases {
            let measured: Vec<f64> = (0..4)
                .map(|oversampling| {
                    aliasing_db(&[
                        ("shape", shape),
                        ("drive", drive),
                        ("oversampling", oversampling as f32),
                    ])
                })
                .collect();
            println!(
                "{name} at {drive} dB drive, 5 kHz sine at -6 dBFS: aliasing off {:.1} dB, 2x {:.1} dB, 4x {:.1} dB, 8x {:.1} dB",
                measured[0], measured[1], measured[2], measured[3]
            );
            // Each doubling helps, unless what's left is already down at
            // the halfbands' stop band (loud harmonics just past Nyquist,
            // 100 dB down) or rounding noise.
            for pair in measured.windows(2) {
                assert!(
                    pair[1] < pair[0] - 3.0 || pair[1] < -90.0,
                    "{name}: {measured:?}"
                );
            }
            assert!(measured[2] < -50.0, "{name}: {measured:?}");
        }
        // A soft curve at moderate drive is essentially clean from 4x on.
        let soft = aliasing_db(&[("shape", 0.0), ("drive", 6.0), ("oversampling", 2.0)]);
        println!("soft clip at 6 dB drive, 4x: aliasing {soft:.1} dB");
        assert!(soft < -85.0, "{soft}");
    }

    #[test]
    fn silence_stays_exactly_silent_at_any_bias() {
        for shape in 0..CURVES.len() {
            for bias in [-100.0, -37.0, 0.0, 55.0, 100.0] {
                let mut waveshaper = waveshaper(&[
                    ("shape", shape as f32),
                    ("bias", bias),
                    ("drive", 30.0),
                    ("oversampling", 3.0),
                ]);
                let (mut left, mut right) = (vec![0.0; 2048], vec![0.0; 2048]);
                run(
                    &mut waveshaper,
                    &mut left,
                    &mut right,
                    64,
                    &Context::DEFAULT,
                );
                assert!(left.iter().chain(&right).all(|&s| s == 0.0));
                assert_eq!(curve(&values(PARAMS, &[("bias", bias)]), 0.0), 0.0);
            }
        }
    }

    #[test]
    fn the_curves_have_their_shapes() {
        let at = |settings: &[(&str, f32)], x: f32| curve(&values(PARAMS, settings), x);
        // Hard clip at +6 dB: doubled, then flat at 1.
        let hard = [("shape", 1.0), ("drive", 20.0 * 2f32.log10())];
        assert!((at(&hard, 0.25) - 0.5).abs() < 1e-5);
        assert_eq!(at(&hard, 0.9), 1.0);
        assert_eq!(at(&hard, -3.0), -1.0);
        // Soft clip: tanh.
        let soft = [("shape", 0.0), ("drive", 0.0)];
        assert!((at(&soft, 0.5) - 0.5f32.tanh()).abs() < 1e-6);
        // Foldback at 0 dB: 1.5 folds back to 0.5, 3 to -1.
        let fold = [("shape", 3.0), ("drive", 0.0)];
        assert!((at(&fold, 1.5) - 0.5).abs() < 1e-6);
        assert!((at(&fold, 3.0) + 1.0).abs() < 1e-6);
        // Tube: asymmetric.
        let tube = [("shape", 2.0), ("drive", 12.0)];
        assert!(at(&tube, 1.0) > 0.95 && at(&tube, -1.0) > -0.71);
        // Rectify: symmetric in x.
        let rectify = [("shape", 5.0), ("drive", 0.0)];
        assert_eq!(at(&rectify, 0.3), at(&rectify, -0.3));
        // Bitcrush at full drive: two bits, steps of 0.5.
        let crush = [("shape", 6.0), ("drive", 48.0)];
        assert_eq!(at(&crush, 0.2), 0.0);
        assert_eq!(at(&crush, 0.3), 0.5);
        assert_eq!(at(&crush, -0.8), -1.0);
        // Output and mix: half wet, -6 dB.
        let blend = [
            ("shape", 1.0),
            ("drive", 0.0),
            ("mix", 50.0),
            ("output", -6.0),
        ];
        let expected = 0.5 * 2.0 + 0.5 * dsp::db_to_gain(-6.0);
        assert!((at(&blend, 2.0) - expected).abs() < 1e-5);
        // Bias moves the operating point but the curve still passes 0.
        let biased = [("shape", 0.0), ("drive", 0.0), ("bias", 50.0)];
        assert!((at(&biased, 0.5) - (1f32.tanh() - 0.5f32.tanh())).abs() < 1e-6);
    }

    #[test]
    fn mix_at_zero_without_oversampling_is_the_dry_signal() {
        let input = sine(220.0, 0.8, 9600, RATE);
        let mut waveshaper = waveshaper(&[
            ("mix", 0.0),
            ("oversampling", 0.0),
            ("drive", 40.0),
            ("shape", 3.0),
        ]);
        let (mut left, mut right) = (input.clone(), input.clone());
        run(
            &mut waveshaper,
            &mut left,
            &mut right,
            64,
            &Context::DEFAULT,
        );
        assert_eq!(left, input);
        assert_eq!(right, input);
    }

    #[test]
    fn hard_clip_output_peaks_at_the_ceiling_and_meters_follow() {
        let input = sine(200.0, 0.5, 24_000, RATE);
        let mut waveshaper = waveshaper(&[("shape", 1.0), ("drive", 24.0)]);
        let (mut left, mut right) = (input.clone(), input);
        let levels = run_measured(
            &mut waveshaper,
            &mut left,
            &mut right,
            64,
            DESCRIPTOR.levels,
        );
        let out = peak(&left[12_000..]);
        assert!(out > 0.95 && out < 1.15, "{out}");
        assert!(
            (levels.input.peak() - dsp::gain_to_db(0.5)).abs() < 0.2,
            "{levels:?}"
        );
        assert!(
            (levels.output.peak() - dsp::gain_to_db(out)).abs() < 0.4,
            "{levels:?}"
        );
    }

    #[test]
    fn switching_shape_or_oversampling_doesnt_click() {
        // A low sine: from one sample to the next it barely moves, so a
        // click is a jump much larger than the waveform's own steps.
        let input = sine(110.0, 0.4, 48_000, RATE);
        let largest_step = |out: &[f32]| {
            out.windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max)
        };
        let mut steady = waveshaper(&[("shape", 0.0), ("drive", 6.0)]);
        let (mut left, mut right) = (input.clone(), input.clone());
        run(&mut steady, &mut left, &mut right, 64, &Context::DEFAULT);
        let normal = largest_step(&left[4800..]);

        let mut switching = waveshaper(&[("shape", 0.0), ("drive", 6.0)]);
        let (mut left, mut right) = (input.clone(), input);
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            if block % 50 == 25 {
                let step = block / 50;
                set(&mut switching, "oversampling", (step % 4) as f32);
                set(&mut switching, "shape", [0.0, 2.0, 1.0][step % 3]);
            }
            switching.process(l, r, &Context::DEFAULT);
        }
        let worst = largest_step(&left[4800..]);
        println!("largest step: steady {normal:.4}, switching {worst:.4}");
        assert!(worst < 2.5 * normal, "{worst} against {normal}");
    }

    /// The points of a drawn curve given as a function of the input.
    fn drawn(f: impl Fn(f32) -> f32) -> Vec<(String, f32)> {
        (0..CURVE_POINTS)
            .map(|k| {
                let x = -1.0 + 2.0 * k as f32 / (CURVE_POINTS - 1) as f32;
                (format!("curve_p{k}"), 0.5 * (f(x) + 1.0))
            })
            .collect()
    }

    fn with(settings: &[(&str, f32)], points: &[(String, f32)]) -> Vec<f32> {
        let mut all: Vec<(&str, f32)> = settings.to_vec();
        all.extend(points.iter().map(|(id, v)| (id.as_str(), *v)));
        crate::testing::values(PARAMS, &all)
    }

    #[test]
    fn the_drawn_curve_is_straight_until_drawn() {
        let values = with(&[("shape", DRAWN as f32), ("drive", 0.0)], &[]);
        for x in [-1.0, -0.37, 0.0, 0.5, 1.0] {
            assert!((curve(&values, x) - x).abs() < 1e-6, "{x}");
        }
        // Beyond full scale it holds its ends.
        assert!((curve(&values, 1.7) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_drawn_curve_plays_what_is_drawn() {
        // A hard clip at half scale, drawn.
        let points = drawn(|x| (2.0 * x).clamp(-1.0, 1.0));
        let values = with(&[("shape", DRAWN as f32), ("drive", 0.0)], &points);
        assert!((curve(&values, 0.25) - 0.5).abs() < 1e-5);
        assert!((curve(&values, 0.75) - 1.0).abs() < 1e-5);
        assert!((curve(&values, -0.75) + 1.0).abs() < 1e-5);
        // Played: a full-scale sine comes out clipped flat at full scale,
        // and as loud as the curve says halfway up.
        let mut settings: Vec<(&str, f32)> =
            vec![("shape", DRAWN as f32), ("drive", 0.0), ("oversampling", 0.0)];
        settings.extend(points.iter().map(|(id, v)| (id.as_str(), *v)));
        let mut shaper = waveshaper(&settings);
        let input = sine(100.0, 1.0, 9600, RATE);
        let (mut left, mut right) = (input.clone(), input);
        run(&mut shaper, &mut left, &mut right, 64, &Context::DEFAULT);
        // (The tone filter at 20 kHz rings a little on the clipped edges.)
        let peak = left[4800..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 1.0).abs() < 0.15, "{peak}");
        // A sine is past half scale for two thirds of each cycle.
        let flat = left[4800..].iter().filter(|s| s.abs() > 0.85).count();
        assert!(flat > 4800 * 3 / 5, "clipped for most of each cycle: {flat}");
    }

    #[test]
    fn redrawing_the_curve_while_it_plays_doesnt_click() {
        let input = sine(110.0, 0.8, 48_000, RATE);
        let largest_step = |out: &[f32]| {
            out.windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max)
        };
        let base = [("shape", DRAWN as f32), ("drive", 0.0)];
        // Every so often, the whole curve flips between straight and a
        // hard clip at a quarter of full scale: no step steeper than
        // either makes held still.
        let clipped = drawn(|x| (4.0 * x).clamp(-1.0, 1.0));
        let straight = drawn(|x| x);
        let mut normal = 0.0f32;
        for points in [&clipped, &straight] {
            let mut settings: Vec<(&str, f32)> = base.to_vec();
            settings.extend(points.iter().map(|(id, v)| (id.as_str(), *v)));
            let mut steady = waveshaper(&settings);
            let (mut left, mut right) = (input.clone(), input.clone());
            run(&mut steady, &mut left, &mut right, 64, &Context::DEFAULT);
            normal = normal.max(largest_step(&left[4800..]));
        }
        let mut redrawn = waveshaper(&base);
        let (mut left, mut right) = (input.clone(), input);
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            if block % 50 == 25 {
                let points = if (block / 50) % 2 == 0 { &clipped } else { &straight };
                for (id, value) in points {
                    set(&mut redrawn, id, *value);
                }
            }
            redrawn.process(l, r, &Context::DEFAULT);
        }
        let worst = largest_step(&left[4800..]);
        println!("largest step: steady {normal:.4}, redrawn {worst:.4}");
        assert!(worst < 1.5 * normal, "{worst} against {normal}");
    }
}
