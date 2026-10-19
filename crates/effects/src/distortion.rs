//! Distortion: the modes of Serum's distortion and Vital's (their names as
//! Serum 2 lists them, and Vital's bit crush), a filter before or after
//! them that morphs from low-pass through band-pass to high-pass, drive,
//! a bias, oversampling, a dry/wet mix and an output level.
//!
//! **Modes** ([`Mode`]): most are curves, what each input level becomes,
//! drawn by [`curve`]; drive is the gain into them (0 to 36 dB as it turns
//! from 0 to 100%).
//! - **Tube**: rounds the top off softly and the bottom harder: even
//!   harmonics, like an overdriven valve. **Asym**: a soft clip that
//!   flattens the bottom at half the top's level.
//! - **Soft clip** (`tanh`), **Hard clip** (flat at ±1), **Soft sat** (a
//!   cubic that reaches ±1 with no corner, gentlest of all), **Tape sat**
//!   (a knee softer than tanh's, compressing rather than clipping) and
//!   **Overdrive** (an arctangent: never flat, always some clean left).
//! - **Diode 1**: a pair of diodes, a sharper knee than tanh's. **Diode
//!   2**: a single diode, so the bottom clips at twice the top's level:
//!   lopsided, even harmonics.
//! - **Stomp box**: a fuzz pedal's transistor, biased off centre and driven
//!   hard: lopsided and buzzy.
//! - **Linear fold** reflects off ±1, **Sine fold** is a sine of the
//!   input, the classic west-coast folds; **Sine shaper** is a sine of the
//!   soft-clipped input, which wraps the loud parts into new partials but
//!   stops folding as they get louder.
//! - **Zero-square** squares the input, keeping its sign: quiet parts
//!   shrink more than loud ones, the "crossover" distortion of a cheap
//!   amplifier, then flattens at ±1.
//! - **Rectify** turns the bottom half up (an octave up, fuzzy) and rounds
//!   it off.
//! - **Downsample** holds every sample for a while: drive sets for how
//!   long, from one sample to 64, the rate falling with no filter before it
//!   so it aliases, as it should. **Bit crush** rounds to fewer levels:
//!   drive takes it from 16 bits to 2.
//!
//! **Bias** moves where on the curve the sound sits (asymmetry for the
//! symmetric curves); the curve's value at the bias alone is taken away, so
//! silence stays silent, and a 10 Hz high-pass takes out the DC the
//! asymmetric ones make with sound.
//!
//! **Filter**: off, before the curve (to choose what distorts: a low-pass
//! first keeps the distortion dark and the highs clean of it) or after (to
//! tame what it made). A state-variable filter whose type morphs low-pass →
//! band-pass → high-pass, with its frequency and resonance.
//!
//! **Oversampling** runs the filter and the curve at 2, 4 or 8 times the
//! sample rate between halfband filters (`creative_halfband.rs`), as the
//! waveshaper does, so the harmonics the curve makes above the audio band
//! don't fold back down as aliasing; the dry sound is mixed in up there
//! too, so a blend has no comb from their phase.
//!
//! **Switching** the mode, the filter's place or the oversampling
//! crossfades; every float glides, at the knob's pace or within a block
//! for a modulator (see "Modulation" in `lib.rs`), and the filter is
//! retuned every sample while it moves.

use std::f32::consts::PI;

use instruments::ParamDescriptor;

use crate::halfband::iir::Design;
use crate::meter::PeakMeter;
pub use crate::shaping::Mode;
use crate::shaping::{self, CRUSH_BITS, DC_HZ, OVERSAMPLING, Oversampler, STAGES, bend};
use crate::{Category, Context, Effect, EffectDescriptor, MAX_METERS, Values, choice, float};
use dsp::one_pole::{DcBlocker, pole};
use dsp::settle;
use dsp::svf::{Coefs, Shape, SvfState, tan_fast};
use dsp::{Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.distortion",
    name: "Distortion",
    description: "Serum-style distortion: tube, clips, diodes, folds, downsample, bit crush, with a pre or post filter",
    category: Category::Distortion,
    params: PARAMS,
    meters: &[
        "Output peak (linear, 1 = 0 dBFS)",
        "Input peak (linear, 1 = 0 dBFS)",
    ],
    tap: true,
    create: || Box::new(Distortion::new()),
    ..EffectDescriptor::BASE
};

pub const MODES: &[&str] = &[
    "Tube",
    "Soft clip",
    "Hard clip",
    "Diode 1",
    "Diode 2",
    "Linear fold",
    "Sine fold",
    "Zero-square",
    "Downsample",
    "Asym",
    "Rectify",
    "Sine shaper",
    "Stomp box",
    "Tape sat",
    "Overdrive",
    "Soft sat",
    "Bit crush",
];

/// Where the filter goes: [`FILTER_OFF`] to [`FILTER_POST`].
pub const FILTER_PLACES: &[&str] = &["Off", "Pre", "Post"];
pub const FILTER_OFF: usize = 0;
pub const FILTER_PRE: usize = 1;
pub const FILTER_POST: usize = 2;

const PARAMS: &[ParamDescriptor] = &[
    choice("mode", "Mode", MODES, 0),
    float("drive", "Drive", 0.0, 100.0, 40.0, "%", false),
    float("bias", "Bias", -100.0, 100.0, 0.0, "%", false),
    choice("filter", "Filter", FILTER_PLACES, FILTER_OFF),
    float("filter_type", "Filter type", 0.0, 100.0, 0.0, "%", false),
    float(
        "filter_freq",
        "Filter frequency",
        20.0,
        20_000.0,
        2000.0,
        "Hz",
        true,
    ),
    float(
        "filter_res",
        "Filter resonance",
        0.0,
        100.0,
        20.0,
        "%",
        false,
    ),
    choice("oversampling", "Oversampling", OVERSAMPLING, 2),
    float("mix", "Mix", 0.0, 100.0, 100.0, "%", false),
    float("output", "Output", -24.0, 24.0, 0.0, "dB", false),
];

const COUNT: usize = 10;

pub const MODE: usize = 0;
/// 0 to 100%: 0 to 36 dB into the curve, or how long Downsample holds and
/// how coarse Bit crush rounds.
pub const DRIVE: usize = 1;
pub const BIAS: usize = 2;
/// Off, Pre or Post.
pub const FILTER: usize = 3;
/// 0% low-pass, 50% band-pass, 100% high-pass, and blends between.
pub const FILTER_TYPE: usize = 4;
pub const FILTER_FREQ: usize = 5;
pub const FILTER_RES: usize = 6;
pub const OVERSAMPLE: usize = 7;
pub const MIX: usize = 8;
pub const OUTPUT: usize = 9;

/// Drive at 100%, in dB.
pub const MAX_DRIVE_DB: f32 = 36.0;
/// The longest Downsample holds a sample, at 100% drive, in samples.
pub const MAX_HOLD: f32 = 64.0;
/// The filter's Q at no resonance and at full.
const FILTER_Q: (f32, f32) = (std::f32::consts::FRAC_1_SQRT_2, 15.0);
/// The highest the filter is tuned, as a share of the sample rate it
/// runs at.
const FILTER_HIGHEST: f32 = 0.45;
/// How long the floats take to reach a new value.
const SMOOTH_MS: f32 = 20.0;
/// How long switching the mode, the filter's place or the oversampling
/// crossfades.
const FADE_MS: f32 = 15.0;

/// The gain into the curve, Bit crush's levels and Downsample's hold (in
/// samples at the original rate), at drive `drive` (0 to 1).
fn drive_terms(drive: f32) -> (f32, f32, f32) {
    let drive = drive.clamp(0.0, 1.0);
    let gain = dsp::db_to_gain(MAX_DRIVE_DB * drive);
    let bits = CRUSH_BITS.0 + (CRUSH_BITS.1 - CRUSH_BITS.0) * drive;
    let hold = MAX_HOLD.powf(drive);
    (gain, (bits - 1.0).exp2(), hold)
}

/// Parameter `index`'s value from `values`, or its default where it's
/// missing or NaN.
fn value_of(values: &[f32], index: usize) -> f32 {
    crate::param_value(PARAMS, values, index)
}

/// The distortion's static transfer function: what an input sample `x`
/// (1 = 0 dBFS) becomes at parameter values `values` (in [`DESCRIPTOR`]'s
/// order), drive, mode, bias, output and mix included; the filter, the
/// DC high-pass and Downsample's hold, which depend on what's around the
/// sample, aren't. What the panel draws.
pub fn curve(values: &[f32], x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    let mode = Mode::from_index(value_of(values, MODE));
    let (gain, levels, _) = drive_terms(value_of(values, DRIVE) / 100.0);
    let gain = if mode.drives() { gain } else { 1.0 };
    let bias = value_of(values, BIAS) / 100.0;
    let wet = bend(mode, gain * x + bias, levels) - bend(mode, bias, levels);
    let mix = value_of(values, MIX) / 100.0;
    x + mix * (dsp::db_to_gain(value_of(values, OUTPUT)) * wet - x)
}

/// The filter's gain at `freq` Hz at these parameter values, as `(low,
/// band, high)` are blended: what the panel draws under the curve.
pub fn filter_response(values: &[f32], freq: f32, sample_rate: f32) -> f32 {
    let morph = value_of(values, FILTER_TYPE) / 100.0;
    let resonance = value_of(values, FILTER_RES) / 100.0;
    let q = FILTER_Q.0 * (FILTER_Q.1 / FILTER_Q.0).powf(resonance);
    let k = f64::from(1.0 / q);
    let corner = value_of(values, FILTER_FREQ).min(FILTER_HIGHEST * sample_rate);
    let g = (std::f64::consts::PI * f64::from(corner) / f64::from(sample_rate)).tan();
    let t = (std::f64::consts::PI * f64::from(freq.clamp(0.0, 0.4999 * sample_rate))
        / f64::from(sample_rate))
    .tan();
    let w = t / g;
    let [m0, m1, m2] = morph_mix(morph, k as f32).map(f64::from);
    // (m0·(s² + k·s + 1) + m1·s + m2) / (s² + k·s + 1) at s = j·w.
    let (den_re, den_im) = (1.0 - w * w, k * w);
    let num_re = m0 * den_re + m2;
    let num_im = m0 * den_im + m1 * w;
    (((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)).sqrt()) as f32
}

/// The state-variable filter's output mix for `morph` (0 low, 0.5 band,
/// 1 high) at damping `k`: how much input, band and low make the output.
#[inline]
fn morph_mix(morph: f32, k: f32) -> [f32; 3] {
    let t = 2.0 * morph.clamp(0.0, 1.0);
    if t <= 1.0 {
        // Low to (normalized) band.
        [0.0, t * k, 1.0 - t]
    } else {
        // Band to high (`x - k·band - low`).
        let h = t - 1.0;
        [h, k * (1.0 - 2.0 * h), -h]
    }
}

/// The filter's coefficients at one rate: `freq` and `q`, with the output
/// mix `morph` (see [`morph_mix`]) asks for.
fn tuning(freq: f32, q: f32, morph: f32, rate: f32) -> Coefs {
    let g = tan_fast(PI * freq.max(10.0).min(FILTER_HIGHEST * rate) / rate);
    let k = 1.0 / q;
    let [m0, m1, m2] = morph_mix(morph, k);
    Coefs::new(&Shape { g, k, m0, m1, m2 })
}

/// What every sub-sample of one input sample is shaped with.
#[derive(Debug, Clone, Copy)]
struct Shaping {
    /// The mode heard, and the one faded to while switching (the same
    /// otherwise), with how far the fade has got.
    from: Mode,
    to: Mode,
    fade: f32,
    gain_from: f32,
    gain_to: f32,
    /// Each mode's curve at the bias alone, taken away so silence stays
    /// silent.
    offset_from: f32,
    offset_to: f32,
    bias: f32,
    levels: f32,
    /// Downsample's hold, in samples at the original rate.
    hold: f32,
    /// How much of the sound goes through the filter off, before and
    /// after the curve (1 for the place chosen; in between while
    /// switching).
    places: [f32; 3],
    output: f32,
    mix: f32,
}

/// One channel's memory in one path: the filters before and after the
/// curve, Downsample's held samples (for the sound with and without the
/// filter before it), the DC high-pass.
#[derive(Debug, Clone, Copy, Default)]
struct Voice {
    pre: SvfState,
    post: SvfState,
    held: [f32; 2],
    count: f32,
    dc: DcBlocker,
}

impl Voice {
    fn flush(&mut self) {
        self.pre.flush();
        self.post.flush();
        for x in &mut self.held {
            *x = settle(*x);
        }
        self.dc.flush();
    }

    /// One sub-sample through `mode`, at `factor` sub-samples a sample;
    /// `which` says which held sample is Downsample's.
    #[inline]
    fn bend(
        &mut self,
        mode: Mode,
        x: f32,
        gain: f32,
        offset: f32,
        s: &Shaping,
        which: usize,
    ) -> f32 {
        if mode == Mode::Downsample {
            // The held sample is taken when the count wraps: see `tick`.
            return self.held[which];
        }
        bend(mode, gain * x + s.bias, s.levels) - offset
    }

    /// One sub-sample: the filter where it's placed, the curve (both
    /// while switching), the DC high-pass, the output level and the mix.
    #[inline]
    fn tick(&mut self, s: &Shaping, tuning: &Coefs, dc_pole: f32, factor: f32, dry: f32) -> f32 {
        // Downsample takes a new sample once per hold, for the sound
        // before and after the filter.
        let filtered = if s.places[FILTER_PRE] > 0.0 {
            self.pre.tick(tuning, dry)
        } else {
            dry
        };
        if s.from == Mode::Downsample || s.to == Mode::Downsample {
            self.count -= 1.0;
            if self.count <= 0.0 {
                self.count += (s.hold * factor).max(1.0);
                self.held = [dry, filtered];
            }
        }
        let curve = |voice: &mut Voice, x: f32, which: usize| {
            let mut y = voice.bend(s.from, x, s.gain_from, s.offset_from, s, which);
            if s.from != s.to {
                let to = voice.bend(s.to, x, s.gain_to, s.offset_to, s, which);
                y += s.fade * (to - y);
            }
            y
        };
        let [off, pre, post] = s.places;
        let mut wet = 0.0;
        if off > 0.0 || post > 0.0 {
            let shaped = curve(self, dry, 0);
            wet += off * shaped;
            if post > 0.0 {
                wet += post * self.post.tick(tuning, shaped);
            }
        }
        if pre > 0.0 {
            wet += pre * curve(self, filtered, 1);
        }
        let blocked = self.dc.tick(wet, dc_pole);
        dry + s.mix * (s.output * blocked - dry)
    }
}

/// The signal path at one oversampling factor, for both channels: up, the
/// voices, down again.
#[derive(Debug, Clone)]
struct Path {
    oversampler: Oversampler,
    voices: [Voice; 2],
    /// The DC high-pass's pole and the filter's tuning, at this path's
    /// rate.
    dc_pole: f32,
    tuning: Coefs,
}

impl Path {
    fn new(stages: usize) -> Path {
        Path {
            oversampler: Oversampler::new(stages),
            voices: [Voice::default(); 2],
            dc_pole: 1.0,
            tuning: Coefs::default(),
        }
    }

    fn factor(&self) -> usize {
        self.oversampler.factor()
    }

    fn clear(&mut self) {
        self.oversampler.clear();
        self.voices = [Voice::default(); 2];
    }

    fn flush(&mut self) {
        self.oversampler.flush();
        for voice in &mut self.voices {
            voice.flush();
        }
    }

    /// One sample of each channel through the path.
    #[inline]
    fn tick(&mut self, designs: &[Design; STAGES], shaping: &Shaping, input: [f32; 2]) -> [f32; 2] {
        let factor = self.factor() as f32;
        let Path {
            oversampler,
            voices,
            dc_pole,
            tuning,
        } = self;
        std::array::from_fn(|channel| {
            let voice = &mut voices[channel];
            oversampler.tick(designs, channel, input[channel], |x| {
                voice.tick(shaping, tuning, *dc_pole, factor, x)
            })
        })
    }
}

pub struct Distortion {
    values: Values<COUNT>,
    sample_rate: f32,
    designs: [Design; STAGES],
    /// A path per oversampling choice.
    paths: [Path; 4],
    /// The oversampling heard, and while switching the one fading out.
    active: usize,
    fading_from: Option<usize>,
    /// How far the switch has got (0 to 1); shared by every crossfade's
    /// step.
    switch: f32,
    fade_step: f32,
    mode: Mode,
    next_mode: Mode,
    mode_fade: f32,
    /// How much of the sound takes each filter place.
    places: [Smoothed; 3],
    /// Drive (0 to 1), bias (-1 to 1), the filter's type (0 to 1),
    /// frequency (log2 Hz) and resonance (0 to 1), mix, output (a gain).
    drive: Smoothed,
    bias: Smoothed,
    filter_type: Smoothed,
    log_freq: Smoothed,
    resonance: Smoothed,
    mix: Smoothed,
    output: Smoothed,
    /// [`drive_terms`] at the drive now.
    drive_gain: f32,
    levels: f32,
    hold: f32,
    input_peak: PeakMeter,
    output_peak: PeakMeter,
}

impl Distortion {
    pub fn new() -> Distortion {
        let smoothed = |value: f32| Smoothed::new(value, SMOOTH_MS);
        let mut distortion = Distortion {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            designs: shaping::designs(),
            paths: [0, 1, 2, 3].map(Path::new),
            active: 0,
            fading_from: None,
            switch: 0.0,
            fade_step: 1.0,
            mode: Mode::Tube,
            next_mode: Mode::Tube,
            mode_fade: 0.0,
            places: [
                Smoothed::new(1.0, FADE_MS),
                Smoothed::new(0.0, FADE_MS),
                Smoothed::new(0.0, FADE_MS),
            ],
            drive: smoothed(0.0),
            bias: smoothed(0.0),
            filter_type: smoothed(0.0),
            log_freq: smoothed(10.0),
            resonance: smoothed(0.0),
            mix: smoothed(1.0),
            output: smoothed(1.0),
            drive_gain: 1.0,
            levels: 1.0,
            hold: 1.0,
            input_peak: PeakMeter::default(),
            output_peak: PeakMeter::default(),
        };
        distortion.prepare(48_000.0, 0);
        distortion
    }

    fn smoothers(&mut self) -> [&mut Smoothed; 10] {
        let [off, pre, post] = &mut self.places;
        [
            off,
            pre,
            post,
            &mut self.drive,
            &mut self.bias,
            &mut self.filter_type,
            &mut self.log_freq,
            &mut self.resonance,
            &mut self.mix,
            &mut self.output,
        ]
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`. The mode and oversampling switch in `process`.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            DRIVE => self.drive.move_to(value / 100.0, pace),
            BIAS => self.bias.move_to(value / 100.0, pace),
            FILTER => {
                for (place, weight) in self.places.iter_mut().enumerate() {
                    weight.set_target(if place == value as usize { 1.0 } else { 0.0 });
                }
            }
            FILTER_TYPE => self.filter_type.move_to(value / 100.0, pace),
            FILTER_FREQ => self.log_freq.move_to(value.log2(), pace),
            FILTER_RES => self.resonance.move_to(value / 100.0, pace),
            MIX => self.mix.move_to(value / 100.0, pace),
            OUTPUT => self.output.move_to(dsp::db_to_gain(value), pace),
            _ => {}
        }
    }

    fn wanted_mode(&self) -> Mode {
        Mode::from_index(self.values[MODE])
    }

    fn wanted_path(&self) -> usize {
        (self.values[OVERSAMPLE].round().max(0.0) as usize).min(STAGES)
    }

    fn update_drive(&mut self) {
        (self.drive_gain, self.levels, self.hold) = drive_terms(self.drive.current());
    }

    /// Works the filter's tuning out for every path that's heard.
    fn update_tuning(&mut self) {
        let freq = self.log_freq.current().exp2();
        let resonance = self.resonance.current().clamp(0.0, 1.0);
        let q = FILTER_Q.0 * (FILTER_Q.1 / FILTER_Q.0).powf(resonance);
        let morph = self.filter_type.current();
        let sample_rate = self.sample_rate;
        let (active, old) = (self.active, self.fading_from);
        for (index, path) in self.paths.iter_mut().enumerate() {
            if index == active || Some(index) == old {
                let rate = sample_rate * path.factor() as f32;
                path.tuning = tuning(freq, q, morph, rate);
            }
        }
    }

    /// Starts switching the mode or the oversampling if asked and not
    /// already switching; a change that arrives mid-switch waits for it
    /// to finish.
    fn start_switches(&mut self) {
        let wanted = self.wanted_mode();
        if self.mode == self.next_mode && wanted != self.mode {
            self.next_mode = wanted;
            self.mode_fade = 0.0;
        }
        let wanted = self.wanted_path();
        if self.fading_from.is_none() && wanted != self.active {
            self.paths[wanted].clear();
            self.fading_from = Some(self.active);
            self.active = wanted;
            self.switch = 0.0;
            self.update_tuning();
        }
    }
}

impl Default for Distortion {
    fn default() -> Self {
        Distortion::new()
    }
}

impl Effect for Distortion {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        for smoothed in self.smoothers() {
            smoothed.prepare(sample_rate);
        }
        for path in &mut self.paths {
            let rate = sample_rate * path.factor() as f32;
            path.dc_pole = pole(DC_HZ, rate);
        }
        self.fade_step = 1.0 / (FADE_MS * 0.001 * sample_rate).max(1.0);
        self.input_peak.prepare(sample_rate);
        self.output_peak.prepare(sample_rate);
        for index in 0..COUNT {
            self.apply(index, Pace::Knob);
        }
        self.reset();
    }

    fn reset(&mut self) {
        for smoothed in self.smoothers() {
            smoothed.settle();
        }
        self.update_drive();
        self.mode = self.wanted_mode();
        self.next_mode = self.mode;
        self.mode_fade = 0.0;
        self.active = self.wanted_path();
        self.fading_from = None;
        self.switch = 0.0;
        for path in &mut self.paths {
            path.clear();
        }
        self.update_tuning();
        self.input_peak.reset();
        self.output_peak.reset();
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.values.played();
        self.start_switches();
        let len = left.len().min(right.len());
        let (mut input_peak, mut output_peak) = (0.0f32, 0.0f32);
        for n in 0..len {
            if self.drive.is_smoothing() {
                self.drive.tick();
                self.update_drive();
            }
            if self.log_freq.is_smoothing()
                || self.resonance.is_smoothing()
                || self.filter_type.is_smoothing()
            {
                self.log_freq.tick();
                self.resonance.tick();
                self.filter_type.tick();
                self.update_tuning();
            }
            let bias = self.bias.tick();
            let (from, to) = (self.mode, self.next_mode);
            let gain = |mode: Mode| if mode.drives() { self.drive_gain } else { 1.0 };
            let offset = |mode: Mode| {
                if mode == Mode::Downsample {
                    0.0
                } else {
                    bend(mode, bias, self.levels)
                }
            };
            let [off, pre, post] = &mut self.places;
            let shaping = Shaping {
                from,
                to,
                fade: self.mode_fade,
                gain_from: gain(from),
                gain_to: gain(to),
                offset_from: offset(from),
                offset_to: offset(to),
                bias,
                levels: self.levels,
                hold: self.hold,
                places: [off.tick(), pre.tick(), post.tick()],
                output: self.output.tick(),
                mix: self.mix.tick(),
            };
            if from != to {
                self.mode_fade += self.fade_step;
                if self.mode_fade >= 1.0 {
                    self.mode = to;
                    self.mode_fade = 0.0;
                }
            }

            // A non-finite sample would stick in the filters forever.
            let clean = |x: f32| if x.is_finite() { x } else { 0.0 };
            let x = [clean(left[n]), clean(right[n])];
            input_peak = input_peak.max(x[0].abs()).max(x[1].abs());
            let designs = &self.designs;
            let mut y = self.paths[self.active].tick(designs, &shaping, x);
            if let Some(old) = self.fading_from {
                let faded = self.paths[old].tick(designs, &shaping, x);
                for (y, z) in y.iter_mut().zip(faded) {
                    *y = z + self.switch * (*y - z);
                }
                self.switch += self.fade_step;
                if self.switch >= 1.0 {
                    self.fading_from = None;
                }
            }
            output_peak = output_peak.max(y[0].abs()).max(y[1].abs());
            left[n] = y[0];
            right[n] = y[1];
        }
        // Only the paths running carry anything to flush: the others
        // were cleared when they were left.
        self.paths[self.active].flush();
        if let Some(old) = self.fading_from {
            self.paths[old].flush();
        }
        self.input_peak.update(input_peak, len);
        self.output_peak.update(output_peak, len);
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
        out[0] = self.output_peak.level();
        out[1] = self.input_peak.level();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, db, noise, peak, rms, run, set, sine, values};

    fn distortion(settings: &[(&str, f32)]) -> Distortion {
        let mut distortion = Distortion::new();
        distortion.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut distortion, id, value);
        }
        distortion.reset();
        distortion
    }

    fn processed(settings: &[(&str, f32)], input: &[f32]) -> Vec<f32> {
        let mut distortion = distortion(settings);
        let (mut left, mut right) = (input.to_vec(), input.to_vec());
        run(
            &mut distortion,
            &mut left,
            &mut right,
            64,
            &Context::DEFAULT,
        );
        left
    }

    #[test]
    fn parameters_are_laid_out_as_documented() {
        assert_eq!(PARAMS.len(), COUNT);
        let ids = [
            (MODE, "mode"),
            (DRIVE, "drive"),
            (BIAS, "bias"),
            (FILTER, "filter"),
            (FILTER_TYPE, "filter_type"),
            (FILTER_FREQ, "filter_freq"),
            (FILTER_RES, "filter_res"),
            (OVERSAMPLE, "oversampling"),
            (MIX, "mix"),
            (OUTPUT, "output"),
        ];
        for (index, id) in ids {
            assert_eq!(PARAMS[index].id, id);
        }
        assert_eq!(MODES.len(), Mode::ALL.len());
        for (index, mode) in Mode::ALL.iter().enumerate() {
            assert_eq!(mode.index(), index);
        }
    }

    #[test]
    fn every_curve_passes_zero_and_stays_bounded() {
        for mode in Mode::ALL {
            assert_eq!(bend(mode, 0.0, 128.0), 0.0, "{mode:?}");
            if mode == Mode::Downsample {
                // Not a curve: its hold is elsewhere.
                continue;
            }
            for i in -2000..=2000 {
                let x = i as f32 / 20.0;
                let y = bend(mode, x, 128.0);
                assert!(y.is_finite() && y.abs() <= 2.01, "{mode:?} at {x}: {y}");
            }
            // Gentle near zero: a quiet sound isn't made louder than 1:1
            // much, nor silenced (save Zero-square, which is the point).
            if !matches!(mode, Mode::ZeroSquare | Mode::BitCrush | Mode::Rectify) {
                let slope = bend(mode, 1e-3, 1e6) / 1e-3;
                assert!((0.5..=5.0).contains(&slope), "{mode:?}: {slope}");
            }
        }
    }

    #[test]
    fn the_curves_have_their_shapes() {
        let at = |settings: &[(&str, f32)], x: f32| curve(&values(PARAMS, settings), x);
        let mode = |m: Mode| ("mode", m.index() as f32);
        // Hard clip with no drive: straight, then flat at 1.
        let hard = [mode(Mode::HardClip), ("drive", 0.0)];
        assert!((at(&hard, 0.5) - 0.5).abs() < 1e-6);
        assert_eq!(at(&hard, 3.0), 1.0);
        // The lopsided ones: Diode 2's bottom clips later than its top,
        // Asym's sooner.
        let diode = [mode(Mode::Diode2), ("drive", 0.0)];
        assert!(-at(&diode, -4.0) > 1.5 * at(&diode, 4.0));
        let asym = [mode(Mode::Asym), ("drive", 0.0)];
        assert!((at(&asym, -10.0) + 0.5).abs() < 1e-3 && (at(&asym, 10.0) - 1.0).abs() < 1e-3);
        // Rectify is even; the folds fold.
        let rectify = [mode(Mode::Rectify), ("drive", 0.0)];
        assert!((at(&rectify, 0.4) - at(&rectify, -0.4)).abs() < 1e-6);
        let fold = [mode(Mode::LinearFold), ("drive", 0.0)];
        assert!((at(&fold, 1.5) - 0.5).abs() < 1e-6);
        let sine_fold = [mode(Mode::SineFold), ("drive", 0.0)];
        assert!((at(&sine_fold, 2.0)).abs() < 1e-6);
        // Zero-square shrinks quiet parts.
        let square = [mode(Mode::ZeroSquare), ("drive", 0.0)];
        assert!((at(&square, 0.5) - 0.25).abs() < 1e-6);
        // Bit crush at full drive: 2 bits, steps of a half.
        let crush = [mode(Mode::BitCrush), ("drive", 100.0)];
        assert_eq!(at(&crush, 0.3), 0.5);
        assert_eq!(at(&crush, 0.2), 0.0);
        // Drive is up to 36 dB into the curve.
        let driven = [mode(Mode::HardClip), ("drive", 50.0)];
        let gain = dsp::db_to_gain(18.0);
        assert!((at(&driven, 0.01) - 0.01 * gain).abs() < 1e-5);
        // Bias moves the curve but silence stays silent.
        let biased = [mode(Mode::SoftClip), ("drive", 0.0), ("bias", 50.0)];
        assert_eq!(at(&biased, 0.0), 0.0);
        assert!((at(&biased, 0.5) - (1f32.tanh() - 0.5f32.tanh())).abs() < 1e-6);
    }

    #[test]
    fn silence_stays_silent_in_every_mode_at_any_bias() {
        for mode in Mode::ALL {
            for bias in [-100.0, -30.0, 0.0, 70.0] {
                for filter in 0..3 {
                    let out = processed(
                        &[
                            ("mode", mode.index() as f32),
                            ("bias", bias),
                            ("drive", 80.0),
                            ("filter", filter as f32),
                            ("filter_res", 90.0),
                        ],
                        &[0.0; 2048],
                    );
                    assert!(out.iter().all(|&s| s == 0.0), "{mode:?} {bias} {filter}");
                }
            }
        }
    }

    #[test]
    fn downsample_holds_samples_and_bit_crush_rounds() {
        // At 100% drive, oversampling off: each sample held for 64 (the DC
        // high-pass after lets each held stretch sag a little).
        let input = sine(220.0, 0.5, 9600, RATE);
        let out = processed(
            &[
                ("mode", Mode::Downsample.index() as f32),
                ("drive", 100.0),
                ("oversampling", 0.0),
            ],
            &input,
        );
        let changes = out[4800..]
            .windows(2)
            .filter(|w| (w[0] - w[1]).abs() > 1e-3)
            .count();
        println!("downsample: {changes} changes in 4800 samples");
        assert!((70..=80).contains(&changes), "{changes}");
        // A held sine is mostly its own fundamental still.
        assert!(db(rms(&out[4800..]) / rms(&input[4800..])).abs() < 1.5);

        let out = processed(
            &[
                ("mode", Mode::BitCrush.index() as f32),
                ("drive", 100.0),
                ("oversampling", 0.0),
            ],
            &input,
        );
        // Steps of a half (give or take the high-pass's sag).
        let mut levels: Vec<i32> = out[4800..]
            .iter()
            .map(|s| (s * 4.0).round() as i32)
            .collect();
        levels.sort_unstable();
        levels.dedup();
        println!("bit crush at 2 bits: levels {levels:?}");
        assert!(levels.len() <= 5, "{levels:?}");
    }

    /// The level of an `freq` Hz sine through `settings`, settled, in dB
    /// against the input.
    fn gain_db(settings: &[(&str, f32)], freq: f64, amplitude: f32) -> f64 {
        let input = sine(freq, amplitude, 24_000, RATE);
        let out = processed(settings, &input);
        db(rms(&out[12_000..]) / rms(&input[12_000..]))
    }

    #[test]
    fn the_filter_shapes_what_distorts_or_what_it_made() {
        let quiet = |settings: &[(&str, f32)], freq: f64| {
            let mut all = vec![("mode", Mode::SoftClip.index() as f32), ("drive", 0.0)];
            all.extend_from_slice(settings);
            gain_db(&all, freq, 0.01)
        };
        // Low-pass at 500 Hz, before or after a curve that's straight for
        // a quiet sound: the 4 kHz tone is cut either way, as drawn.
        for place in [FILTER_PRE, FILTER_POST] {
            let settings = [
                ("filter", place as f32),
                ("filter_freq", 500.0),
                ("filter_res", 0.0),
            ];
            let cut = quiet(&settings, 4000.0);
            let drawn = db(f64::from(filter_response(
                &values(PARAMS, &settings),
                4000.0,
                RATE,
            )));
            println!("place {place}: 4 kHz {cut:.1} dB, drawn {drawn:.1}");
            assert!((cut - drawn).abs() < 0.5 && cut < -30.0);
            assert!(quiet(&settings, 100.0).abs() < 0.3);
        }
        // High-pass morph.
        let high = [
            ("filter", FILTER_POST as f32),
            ("filter_type", 100.0),
            ("filter_freq", 2000.0),
        ];
        assert!(quiet(&high, 200.0) < -30.0 && quiet(&high, 10_000.0).abs() < 0.5);
        // Before the curve, a low-pass keeps a loud high tone from
        // distorting; after it, the harmonics it made are what's cut.
        let loud = |place: usize| {
            let input = sine(3000.0, 0.8, 24_000, RATE);
            let out = processed(
                &[
                    ("mode", Mode::HardClip.index() as f32),
                    ("drive", 60.0),
                    ("filter", place as f32),
                    ("filter_freq", 300.0),
                    ("filter_res", 0.0),
                ],
                &input,
            );
            rms(&out[12_000..])
        };
        assert!(loud(FILTER_PRE) < 0.1 * loud(FILTER_OFF));
    }

    #[test]
    fn oversampling_reduces_aliasing() {
        // A 5 kHz sine hard-clipped: what lands between its harmonics
        // under 20 kHz is aliasing. Measured as the residue after taking
        // the harmonics out with a long window's DFT.
        const PERIOD: usize = 4096;
        const BIN: usize = 431;
        let aliasing = |oversampling: f32| {
            let input = sine(
                f64::from(RATE) * BIN as f64 / PERIOD as f64,
                0.5,
                PERIOD * 4,
                RATE,
            );
            let out = processed(
                &[
                    ("mode", Mode::HardClip.index() as f32),
                    ("drive", 40.0),
                    ("oversampling", oversampling),
                ],
                &input,
            );
            let window = &out[out.len() - PERIOD..];
            let audible = (20_000.0 / RATE * PERIOD as f32) as usize;
            let (mut harmonics, mut aliases) = (0.0f64, 0.0f64);
            for bin in 1..=audible {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for (i, &s) in window.iter().enumerate() {
                    let phase = std::f64::consts::TAU * (bin * i % PERIOD) as f64 / PERIOD as f64;
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
        };
        let (off, eight) = (aliasing(0.0), aliasing(3.0));
        println!("hard clip aliasing: off {off:.1} dB, 8x {eight:.1} dB");
        assert!(eight < off - 20.0 && eight < -50.0);
    }

    #[test]
    fn mix_zero_without_oversampling_is_the_dry_signal() {
        let input = noise(4800, 4);
        let out = processed(
            &[
                ("mix", 0.0),
                ("oversampling", 0.0),
                ("drive", 100.0),
                ("filter", 1.0),
            ],
            &input,
        );
        assert_eq!(out, input);
    }

    #[test]
    fn switching_mode_filter_or_oversampling_does_not_click() {
        let input = sine(110.0, 0.4, 48_000, RATE);
        let largest_step = |out: &[f32]| {
            out.windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max)
        };
        let settled = Mode::ALL
            .iter()
            .filter(|m| m.drives())
            .map(|m| {
                let out = processed(&[("mode", m.index() as f32), ("drive", 20.0)], &input);
                largest_step(&out[4800..])
            })
            .fold(0.0f32, f32::max);
        let mut switching = distortion(&[("drive", 20.0)]);
        let (mut left, mut right) = (input.clone(), input);
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            if block % 30 == 15 {
                let step = block / 30;
                let modes = [Mode::Tube, Mode::HardClip, Mode::Overdrive, Mode::SineFold];
                set(&mut switching, "mode", modes[step % 4].index() as f32);
                set(&mut switching, "oversampling", (step % 4) as f32);
                set(&mut switching, "filter", (step % 3) as f32);
            }
            switching.process(l, r, &Context::DEFAULT);
        }
        let worst = largest_step(&left[4800..]);
        println!("largest step switching {worst:.4}, settled at most {settled:.4}");
        assert!(worst < 1.5 * settled, "{worst} against {settled}");
    }

    #[test]
    fn meters_read_the_peaks_and_reset_clears_them() {
        let mut distortion = distortion(&[("mode", 2.0), ("drive", 50.0)]);
        let input = sine(200.0, 0.5, 24_000, RATE);
        let (mut left, mut right) = (input.clone(), input);
        run(
            &mut distortion,
            &mut left,
            &mut right,
            64,
            &Context::DEFAULT,
        );
        let mut meters = [0.0; MAX_METERS];
        distortion.meters(&mut meters);
        assert!((meters[1] - 0.5).abs() < 0.01, "{meters:?}");
        assert!(
            (meters[0] - peak(&left[12_000..])).abs() < 0.05,
            "{meters:?}"
        );
        distortion.reset();
        assert_eq!(distortion.meter(), 0.0);
    }
}
