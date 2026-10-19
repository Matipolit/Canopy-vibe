//! Multiband compressor: three bands split at two adjustable frequencies,
//! each compressed downward above one threshold and upward below another,
//! then given its own gain. With the defaults it is the "OTT" sound: every
//! band squeezed from both sides into a narrow window, then lifted, which
//! brings out detail and air and makes everything loud and in your face;
//! Depth and Mix dial it back. Its meters are each band's level in and
//! out and how far its gain is moving.
//!
//! The split is two Linkwitz-Riley 24 dB/octave crossovers, each two
//! Butterworth state-variable filters in series, the second splitting what
//! is above the first. The low band also goes through the allpass the
//! second crossover's low and high halves sum to, so all three bands come
//! back together in phase: untouched, they sum to an allpass, flat in
//! level at every frequency and every setting.
//!
//! Each band's level is its RMS over [`RMS_MS`] (a full-scale sine reads
//! -3 dB), both channels linked. Above `threshold` the band is turned down
//! by `1 - 1/ratio` of the excess, as in the compressor; below
//! `up_threshold` it is turned up by `1 - 1/up_ratio` of the shortfall, at
//! most [`MAX_UPWARD`] dB, and letting go between [`UPWARD_FLOOR`] and
//! 20 dB under it, so silence and noise floors aren't dragged up forever.
//! The change approaches its target in dB at the attack rate when the gain
//! falls and the release rate when it rises. Depth scales each band's whole
//! change in dB, its gain included, so 0% is untouched; Mix blends each
//! band with its unprocessed self, which keeps the phase the same and so
//! parallel compression free of comb filtering. Every parameter glides,
//! the attack and release times included: at the knob's pace, or within a
//! block for a modulator (see "Modulation" in `lib.rs`).

use instruments::ParamDescriptor;

use crate::dynamics::{DB_PER_LOG2, coefficient};
use crate::meter::meter_db;
use crate::{Category, Context, Effect, EffectDescriptor, OFF_ON, Values, choice, float};
use dsp::flush;
use dsp::svf::{BUTTERWORTH_Q, Coefs, Crossover, Shape, SvfState, response_tan};
use dsp::{Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.multiband",
    name: "Multiband",
    description: "Three-band upward and downward compressor, OTT-style",
    category: Category::Dynamics,
    params: PARAMS,
    meters: &[
        "Low gain change (dB)",
        "Low input level (dB)",
        "Low output level (dB)",
        "Mid gain change (dB)",
        "Mid input level (dB)",
        "Mid output level (dB)",
        "High gain change (dB)",
        "High input level (dB)",
        "High output level (dB)",
    ],
    create: || Box::new(Multiband::new()),
    ..EffectDescriptor::BASE
};

pub const BANDS: usize = 3;
/// Parameters per band, in this order.
pub const BAND_PARAMS: usize = 9;
/// Downward compression: above this level (dB RMS) the band is turned down.
pub const THRESHOLD: usize = 0;
pub const RATIO: usize = 1;
/// Upward compression: below this level the band is turned up.
pub const UP_THRESHOLD: usize = 2;
pub const UP_RATIO: usize = 3;
pub const ATTACK: usize = 4;
pub const RELEASE: usize = 5;
/// The band's own output gain.
pub const GAIN: usize = 6;
pub const SOLO: usize = 7;
pub const BYPASS: usize = 8;
/// The global parameters' indices, after every band's.
pub const LOW_SPLIT: usize = BANDS * BAND_PARAMS;
pub const HIGH_SPLIT: usize = LOW_SPLIT + 1;
pub const INPUT: usize = LOW_SPLIT + 2;
pub const DEPTH: usize = LOW_SPLIT + 3;
pub const MIX: usize = LOW_SPLIT + 4;
pub const OUTPUT: usize = LOW_SPLIT + 5;

/// A band parameter's index (band 0 is low, 2 high).
pub const fn param(band: usize, which: usize) -> usize {
    band * BAND_PARAMS + which
}

/// Meter slots: three per band, in this order.
pub const METER_GAIN_CHANGE: usize = 0;
pub const METER_INPUT: usize = 1;
pub const METER_OUTPUT: usize = 2;

/// A band meter's slot.
pub const fn meter(band: usize, which: usize) -> usize {
    band * 3 + which
}

macro_rules! band {
    ($id:literal, $name:literal, $threshold:expr, $up:expr, $attack:expr, $release:expr, $gain:expr) => {
        [
            float(
                concat!($id, "_threshold"),
                concat!($name, " threshold"),
                -60.0,
                0.0,
                $threshold,
                "dB",
                false,
            ),
            float(
                concat!($id, "_ratio"),
                concat!($name, " ratio"),
                1.0,
                100.0,
                66.7,
                ":1",
                true,
            ),
            float(
                concat!($id, "_up_threshold"),
                concat!($name, " upward threshold"),
                -80.0,
                0.0,
                $up,
                "dB",
                false,
            ),
            float(
                concat!($id, "_up_ratio"),
                concat!($name, " upward ratio"),
                1.0,
                20.0,
                4.17,
                ":1",
                true,
            ),
            float(
                concat!($id, "_attack"),
                concat!($name, " attack"),
                0.1,
                500.0,
                $attack,
                "ms",
                true,
            ),
            float(
                concat!($id, "_release"),
                concat!($name, " release"),
                10.0,
                3000.0,
                $release,
                "ms",
                true,
            ),
            float(
                concat!($id, "_gain"),
                concat!($name, " gain"),
                -24.0,
                24.0,
                $gain,
                "dB",
                false,
            ),
            choice(concat!($id, "_solo"), concat!($name, " solo"), OFF_ON, 0),
            choice(
                concat!($id, "_bypass"),
                concat!($name, " bypass"),
                OFF_ON,
                0,
            ),
        ]
    };
}

/// The defaults are the classic OTT settings: near-limiting above a
/// threshold around -33 dB, 4.17:1 upward below one around -41 dB, slower
/// times for the lows, and the bands lifted 6 to 10 dB afterwards.
const BAND_LIST: [[ParamDescriptor; BAND_PARAMS]; BANDS] = [
    band!("low", "Low", -33.8, -40.8, 47.8, 282.0, 10.3),
    band!("mid", "Mid", -30.2, -41.8, 22.4, 282.0, 5.7),
    band!("high", "High", -35.5, -40.8, 13.5, 132.0, 10.3),
];

const GLOBALS: [ParamDescriptor; 6] = [
    float("low_split", "Low/mid split", 20.0, 1000.0, 88.0, "Hz", true),
    float(
        "high_split",
        "Mid/high split",
        500.0,
        16_000.0,
        2500.0,
        "Hz",
        true,
    ),
    float("input", "Input gain", -24.0, 24.0, 0.0, "dB", false),
    float("depth", "Depth", 0.0, 100.0, 100.0, "%", false),
    float("mix", "Mix", 0.0, 100.0, 100.0, "%", false),
    float("output", "Output", -24.0, 24.0, 0.0, "dB", false),
];

const COUNT: usize = OUTPUT + 1;

const fn flatten(
    bands: [[ParamDescriptor; BAND_PARAMS]; BANDS],
    globals: [ParamDescriptor; 6],
) -> [ParamDescriptor; COUNT] {
    let mut out = [globals[0]; COUNT];
    let mut index = 0;
    while index < LOW_SPLIT {
        out[index] = bands[index / BAND_PARAMS][index % BAND_PARAMS];
        index += 1;
    }
    let mut global = 0;
    while global < globals.len() {
        out[LOW_SPLIT + global] = globals[global];
        global += 1;
    }
    out
}

const PARAM_ARRAY: [ParamDescriptor; COUNT] = flatten(BAND_LIST, GLOBALS);
const PARAMS: &[ParamDescriptor] = &PARAM_ARRAY;

/// How long each band's level is averaged over.
const RMS_MS: f32 = 10.0;
/// The most upward compression lifts a band, in dB.
const MAX_UPWARD: f32 = 30.0;
/// Below this level (dB RMS) upward compression lets go, to nothing 20 dB
/// further down.
const UPWARD_FLOOR: f32 = -80.0;
const UPWARD_FADE: f32 = 20.0;
/// How long parameters take to reach a new value.
const SMOOTH_MS: f32 = 20.0;
/// The thresholds and ratios, and the attack and release times, are
/// updated every this many samples.
const CHUNK: usize = 16;
/// `10 · log10(x) = DB_PER_LOG2 / 2 · log2(x)`.
const POWER_DB_PER_LOG2: f32 = DB_PER_LOG2 / 2.0;
/// A band's level when it is silent, in dB: far below anything, but finite.
const SILENCE_DB: f32 = -1000.0;

/// The level in dB of each band, as split from the input, at `frequency`:
/// what the UI draws to show the split. `band` 0 is low, 2 high; parameter
/// values `values` are in [`DESCRIPTOR`]'s order. The three add up (in
/// amplitude, in phase) to a flat response, so each is -6 dB at its
/// crossover.
pub fn crossover_response_db(values: &[f32], band: usize, frequency: f32, sample_rate: f32) -> f32 {
    let value = |index: usize| crate::param_value(PARAMS, values, index);
    let sample_rate = if sample_rate > 0.0 {
        sample_rate
    } else {
        48_000.0
    };
    let (low_split, high_split) = splits(value(LOW_SPLIT), value(HIGH_SPLIT));
    let tan_w = response_tan(frequency, sample_rate);
    // A Linkwitz-Riley filter is a Butterworth one twice over.
    let lr4 = |shape: Shape| shape.power_at(tan_w).powi(2);
    let lowpass = |freq| lr4(Shape::lowpass(freq, BUTTERWORTH_Q, sample_rate));
    let highpass = |freq| lr4(Shape::highpass(freq, BUTTERWORTH_Q, sample_rate));
    let power = match band {
        0 => lowpass(low_split),
        1 => highpass(low_split) * lowpass(high_split),
        _ => highpass(low_split) * highpass(high_split),
    };
    (10.0 * power.max(1e-30).log10()) as f32
}

/// The split frequencies used: the high one never below the low one.
fn splits(low: f32, high: f32) -> (f32, f32) {
    (low, high.max(low))
}

/// One channel's filters: both crossovers and the low band's allpass.
#[derive(Debug, Clone, Copy, Default)]
struct Splitter {
    low_split: Crossover,
    high_split: Crossover,
    allpass: SvfState,
}

impl Splitter {
    /// (low, mid, high) for input `x`.
    #[inline]
    fn split(&mut self, low_coefs: &Coefs, high_coefs: &Coefs, x: f32) -> [f32; BANDS] {
        let (low, rest) = self.low_split.split(low_coefs, x);
        let (mid, high) = self.high_split.split(high_coefs, rest);
        // What the high crossover's halves sum to, so the low band lines
        // up with them.
        let band = self.allpass.outputs(high_coefs, low).bandpass;
        [low - 2.0 * high_coefs.k() * band, mid, high]
    }

    fn flush(&mut self) {
        self.low_split.flush();
        self.high_split.flush();
        self.allpass.flush();
    }
}

struct Band {
    threshold: Smoothed,
    /// `1 - 1/ratio` downward and upward.
    slope: Smoothed,
    up_threshold: Smoothed,
    up_slope: Smoothed,
    /// The band's gain in dB.
    gain: Smoothed,
    /// 1 processed, 0 bypassed.
    active: Smoothed,
    /// 1 heard, 0 muted by another band's solo.
    audible: Smoothed,
    solo: bool,
    /// Attack and release as one-pole coefficients, worked out from the
    /// times below every [`CHUNK`] samples while they glide.
    attack: f32,
    release: f32,
    /// Attack and release in log2 ms. They glide, so a modulated time
    /// never lets a lagging gain change catch up all at once.
    log_attack: Smoothed,
    log_release: Smoothed,
    /// The band's mean square, both channels linked.
    mean_square: f32,
    /// The gain change in dB, approaching what the level calls for.
    change: f32,
    /// The band's level and the gain applied to it, last sample, for the
    /// meters.
    level_db: f32,
    applied: f32,
}

impl Band {
    fn new() -> Band {
        Band {
            threshold: Smoothed::new(0.0, SMOOTH_MS),
            slope: Smoothed::new(0.0, SMOOTH_MS),
            up_threshold: Smoothed::new(0.0, SMOOTH_MS),
            up_slope: Smoothed::new(0.0, SMOOTH_MS),
            gain: Smoothed::new(0.0, SMOOTH_MS),
            active: Smoothed::new(1.0, SMOOTH_MS),
            audible: Smoothed::new(1.0, SMOOTH_MS),
            solo: false,
            attack: 0.0,
            release: 0.0,
            log_attack: Smoothed::new(0.0, SMOOTH_MS),
            log_release: Smoothed::new(0.0, SMOOTH_MS),
            mean_square: 0.0,
            change: 0.0,
            level_db: SILENCE_DB,
            applied: 1.0,
        }
    }

    /// The gain-computer smoothers, which tick once per chunk.
    fn curve(&mut self) -> [&mut Smoothed; 4] {
        [
            &mut self.threshold,
            &mut self.slope,
            &mut self.up_threshold,
            &mut self.up_slope,
        ]
    }

    /// The smoothers on the audio path, which tick every sample.
    fn path(&mut self) -> [&mut Smoothed; 3] {
        [&mut self.gain, &mut self.active, &mut self.audible]
    }

    /// The attack and release times, which tick once per chunk.
    fn times(&mut self) -> [&mut Smoothed; 2] {
        [&mut self.log_attack, &mut self.log_release]
    }

    /// Works the attack and release coefficients out from their times.
    fn update_times(&mut self, sample_rate: f32) {
        self.attack = coefficient(self.log_attack.current().exp2(), sample_rate);
        self.release = coefficient(self.log_release.current().exp2(), sample_rate);
    }

    /// The gain change in dB that a level of `level_db` calls for.
    #[inline]
    fn target(&self, level_db: f32) -> f32 {
        let threshold = self.threshold.current();
        let down = if level_db > threshold {
            self.slope.current() * (level_db - threshold)
        } else {
            0.0
        };
        let up_threshold = self.up_threshold.current();
        let up = if level_db < up_threshold {
            let lift = (self.up_slope.current() * (up_threshold - level_db)).min(MAX_UPWARD);
            let fade = ((level_db - UPWARD_FLOOR) / UPWARD_FADE + 1.0).clamp(0.0, 1.0);
            lift * fade
        } else {
            0.0
        };
        up - down
    }
}

pub struct Multiband {
    values: Values<COUNT>,
    sample_rate: f32,
    bands: [Band; BANDS],
    /// The split frequencies, as log2, and their filters' coefficients.
    low_split: Smoothed,
    high_split: Smoothed,
    low_coefs: Coefs,
    high_coefs: Coefs,
    splitters: [Splitter; 2],
    /// Input and output gains as amplitude factors.
    input: Smoothed,
    output: Smoothed,
    depth: Smoothed,
    mix: Smoothed,
    /// `1 -` the RMS detector's one-pole coefficient.
    rms_rate: f32,
    /// Samples left until the next chunk's update.
    countdown: usize,
}

impl Multiband {
    pub fn new() -> Multiband {
        let mut multiband = Multiband {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            bands: std::array::from_fn(|_| Band::new()),
            low_split: Smoothed::new(0.0, SMOOTH_MS),
            high_split: Smoothed::new(0.0, SMOOTH_MS),
            low_coefs: Coefs::butterworth(100.0, 48_000.0),
            high_coefs: Coefs::butterworth(1000.0, 48_000.0),
            splitters: [Splitter::default(); 2],
            input: Smoothed::new(1.0, SMOOTH_MS),
            output: Smoothed::new(1.0, SMOOTH_MS),
            depth: Smoothed::new(1.0, SMOOTH_MS),
            mix: Smoothed::new(1.0, SMOOTH_MS),
            rms_rate: 1.0,
            countdown: 0,
        };
        multiband.prepare(48_000.0, 0);
        multiband
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`; solo and bypass always fade at the knob's.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            LOW_SPLIT | HIGH_SPLIT => {
                let (low, high) = splits(self.values[LOW_SPLIT], self.values[HIGH_SPLIT]);
                self.low_split.move_to(low.log2(), pace);
                self.high_split.move_to(high.log2(), pace);
            }
            INPUT => self.input.move_to(dsp::db_to_gain(value), pace),
            DEPTH => self.depth.move_to(value / 100.0, pace),
            MIX => self.mix.move_to(value / 100.0, pace),
            OUTPUT => self.output.move_to(dsp::db_to_gain(value), pace),
            _ => {
                let band = &mut self.bands[index / BAND_PARAMS];
                match index % BAND_PARAMS {
                    THRESHOLD => band.threshold.move_to(value, pace),
                    RATIO => band.slope.move_to(1.0 - 1.0 / value, pace),
                    UP_THRESHOLD => band.up_threshold.move_to(value, pace),
                    UP_RATIO => band.up_slope.move_to(1.0 - 1.0 / value, pace),
                    ATTACK => band.log_attack.move_to(value.log2(), pace),
                    RELEASE => band.log_release.move_to(value.log2(), pace),
                    GAIN => band.gain.move_to(value, pace),
                    BYPASS => band.active.set_target(if value >= 0.5 { 0.0 } else { 1.0 }),
                    SOLO => {
                        band.solo = value >= 0.5;
                        let any = self.bands.iter().any(|band| band.solo);
                        for band in &mut self.bands {
                            let heard = !any || band.solo;
                            band.audible.set_target(if heard { 1.0 } else { 0.0 });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn update_coefs(&mut self) {
        self.low_coefs = Coefs::butterworth(self.low_split.current().exp2(), self.sample_rate);
        self.high_coefs = Coefs::butterworth(self.high_split.current().exp2(), self.sample_rate);
    }

    /// Every [`CHUNK`] samples: the gain curves and times glide. (The
    /// split frequencies glide every sample, so the crossover's filters
    /// move smoothly however fast they're modulated.)
    fn update(&mut self) {
        for band in &mut self.bands {
            for smoothed in band.curve() {
                smoothed.tick();
            }
            if band.log_attack.is_smoothing() || band.log_release.is_smoothing() {
                for smoothed in band.times() {
                    smoothed.tick();
                }
                band.update_times(self.sample_rate);
            }
        }
    }
}

impl Default for Multiband {
    fn default() -> Self {
        Multiband::new()
    }
}

impl Effect for Multiband {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        for band in &mut self.bands {
            for smoothed in band.curve() {
                smoothed.prepare_every(sample_rate, CHUNK);
            }
            for smoothed in band.times() {
                smoothed.prepare_every(sample_rate, CHUNK);
            }
            for smoothed in band.path() {
                smoothed.prepare(sample_rate);
            }
        }
        self.low_split.prepare(sample_rate);
        self.high_split.prepare(sample_rate);
        for smoothed in [
            &mut self.input,
            &mut self.output,
            &mut self.depth,
            &mut self.mix,
        ] {
            smoothed.prepare(sample_rate);
        }
        self.rms_rate = 1.0 - coefficient(RMS_MS, sample_rate);
        for index in 0..COUNT {
            self.apply(index, Pace::Knob);
        }
        self.reset();
    }

    fn reset(&mut self) {
        for band in &mut self.bands {
            for smoothed in band.curve() {
                smoothed.settle();
            }
            for smoothed in band.path() {
                smoothed.settle();
            }
            for smoothed in band.times() {
                smoothed.settle();
            }
            band.update_times(self.sample_rate);
            band.mean_square = 0.0;
            band.change = 0.0;
            band.level_db = SILENCE_DB;
            band.applied = 1.0;
        }
        for smoothed in [
            &mut self.low_split,
            &mut self.high_split,
            &mut self.input,
            &mut self.output,
            &mut self.depth,
            &mut self.mix,
        ] {
            smoothed.settle();
        }
        self.update_coefs();
        self.splitters = [Splitter::default(); 2];
        self.countdown = 0;
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.values.played();
        let len = left.len().min(right.len());
        for n in 0..len {
            if self.countdown == 0 {
                self.update();
                self.countdown = CHUNK;
            }
            self.countdown -= 1;

            if self.low_split.is_smoothing() || self.high_split.is_smoothing() {
                self.low_split.tick();
                self.high_split.tick();
                self.update_coefs();
            }
            // The input gain goes on each band after the split, which is
            // the same for a steady gain; a moving one then can't bend a
            // stray high in, for the upward compression to lift.
            let input = self.input.tick();
            let [splitter_l, splitter_r] = &mut self.splitters;
            let bands_l = splitter_l
                .split(&self.low_coefs, &self.high_coefs, left[n])
                .map(|band| band * input);
            let bands_r = splitter_r
                .split(&self.low_coefs, &self.high_coefs, right[n])
                .map(|band| band * input);
            let depth = self.depth.tick();
            let mix = self.mix.tick();
            let (mut out_l, mut out_r) = (0.0, 0.0);
            for (band, (l, r)) in self.bands.iter_mut().zip(bands_l.into_iter().zip(bands_r)) {
                let power = (l * l).max(r * r);
                band.mean_square += (power - band.mean_square) * self.rms_rate;
                let level_db = if band.mean_square > 0.0 {
                    (POWER_DB_PER_LOG2 * band.mean_square.log2()).max(SILENCE_DB)
                } else {
                    SILENCE_DB
                };
                let target = band.target(level_db);
                let time = if target < band.change {
                    band.attack
                } else {
                    band.release
                };
                band.change = target + time * (band.change - target);

                let gain_db = band.gain.tick();
                let active = band.active.tick();
                let audible = band.audible.tick();
                let processed = (depth * (band.change + gain_db) / DB_PER_LOG2).exp2();
                let bypassed = 1.0 + active * (processed - 1.0);
                let applied = audible * (1.0 + mix * (bypassed - 1.0));
                out_l += l * applied;
                out_r += r * applied;
                band.level_db = level_db;
                band.applied = applied;
            }
            let output = self.output.tick();
            left[n] = out_l * output;
            right[n] = out_r * output;
        }
        for splitter in &mut self.splitters {
            splitter.flush();
        }
        for band in &mut self.bands {
            band.mean_square = flush(band.mean_square);
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

    fn meters(&self, out: &mut [f32; crate::MAX_METERS]) {
        let depth = self.depth.current();
        for (index, band) in self.bands.iter().enumerate() {
            out[meter(index, METER_GAIN_CHANGE)] = depth * band.active.current() * band.change;
            let level = dsp::db_to_gain(band.level_db);
            out[meter(index, METER_INPUT)] = meter_db(level);
            out[meter(index, METER_OUTPUT)] =
                meter_db(level * band.applied * self.output.current());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults;
    use crate::testing::{RATE, db, rms, run, sine};

    fn values(settings: &[(usize, f32)]) -> [f32; COUNT] {
        let mut values: [f32; COUNT] = defaults(PARAMS);
        for &(index, value) in settings {
            values[index] = value;
        }
        values
    }

    fn multiband(values: &[f32; COUNT]) -> Multiband {
        let mut multiband = Multiband::new();
        multiband.prepare(RATE, 64);
        for (index, &value) in values.iter().enumerate() {
            multiband.set_param(index, value);
        }
        multiband.reset();
        multiband
    }

    /// The steady gain in dB for a sine at `freq` and `level_db` peak, and
    /// the meters after it.
    fn measured(
        values: &[f32; COUNT],
        freq: f64,
        level_db: f32,
    ) -> (f64, [f32; crate::MAX_METERS]) {
        let mut multiband = multiband(values);
        let settle = RATE as usize;
        let cycles = (0.25 * freq).ceil();
        let window = (cycles * f64::from(RATE) / freq).round() as usize;
        let input = sine(freq, dsp::db_to_gain(level_db), settle + window, RATE);
        let (mut left, mut right) = (input.clone(), input.clone());
        run(&mut multiband, &mut left, &mut right, 64, &Context::DEFAULT);
        assert_eq!(left, right);
        assert!(left.iter().all(|s| s.is_finite()));
        let mut meters = [0.0; crate::MAX_METERS];
        multiband.meters(&mut meters);
        (db(rms(&left[settle..]) / rms(&input[settle..])), meters)
    }

    fn log_frequencies(count: usize) -> impl Iterator<Item = f64> {
        (0..count).map(move |i| 25.0 * (18_000.0f64 / 25.0).powf(i as f64 / (count - 1) as f64))
    }

    /// No dynamics and no gain: every band's settings neutral.
    fn neutral(mut values: [f32; COUNT]) -> [f32; COUNT] {
        for band in 0..BANDS {
            values[param(band, RATIO)] = 1.0;
            values[param(band, UP_RATIO)] = 1.0;
            values[param(band, GAIN)] = 0.0;
        }
        values
    }

    #[test]
    fn the_bands_sum_back_flat() {
        let mut worst = 0.0f64;
        let cases = [
            values(&[(DEPTH, 0.0)]),
            values(&[(DEPTH, 0.0), (LOW_SPLIT, 300.0), (HIGH_SPLIT, 600.0)]),
            values(&[(MIX, 0.0), (LOW_SPLIT, 1000.0), (HIGH_SPLIT, 500.0)]),
            neutral(values(&[(LOW_SPLIT, 150.0), (HIGH_SPLIT, 6000.0)])),
            values(&[
                (param(0, BYPASS), 1.0),
                (param(1, BYPASS), 1.0),
                (param(2, BYPASS), 1.0),
            ]),
        ];
        for values in cases {
            for freq in log_frequencies(30) {
                let (gain, _) = measured(&values, freq, -6.0);
                worst = worst.max(gain.abs());
                assert!(gain.abs() < 0.01, "{freq:.0} Hz: {gain:.4} dB");
            }
        }
        println!("largest deviation from flat with the bands untouched: {worst:.5} dB");
    }

    #[test]
    fn solo_hears_one_band_as_the_split_draws_it() {
        let mut worst = 0.0f64;
        for band in 0..BANDS {
            let values = neutral(values(&[(param(band, SOLO), 1.0)]));
            for freq in log_frequencies(24) {
                let (measured, _) = measured(&values, freq, -6.0);
                let expected = f64::from(crossover_response_db(&values, band, freq as f32, RATE));
                if expected < -60.0 {
                    assert!(
                        measured < -55.0,
                        "band {band}, {freq:.0} Hz: {measured:.1} dB"
                    );
                    continue;
                }
                let error = (measured - expected).abs();
                worst = worst.max(error);
                assert!(
                    error < 0.05,
                    "band {band}, {freq:.0} Hz: measured {measured:.3} dB, drawn {expected:.3} dB"
                );
            }
        }
        println!(
            "largest difference between a soloed band and crossover_response_db: {worst:.4} dB"
        );
        // Each band is half (-6 dB) at its crossover.
        let defaults: [f32; COUNT] = defaults(PARAMS);
        assert!((crossover_response_db(&defaults, 0, 88.0, RATE) + 6.02).abs() < 0.05);
        assert!((crossover_response_db(&defaults, 2, 2500.0, RATE) + 6.02).abs() < 0.05);
    }

    /// Downward only: -30 dB threshold, 4:1, with the splits at `low` and
    /// `high`.
    fn downward(low: f32, high: f32) -> [f32; COUNT] {
        let mut values = neutral(values(&[(LOW_SPLIT, low), (HIGH_SPLIT, high)]));
        for band in 0..BANDS {
            values[param(band, THRESHOLD)] = -30.0;
            values[param(band, RATIO)] = 4.0;
            values[param(band, ATTACK)] = 10.0;
            values[param(band, RELEASE)] = 300.0;
        }
        values
    }

    #[test]
    fn each_band_compresses_only_its_own_range() {
        // A -6 dB peak sine reads -9 dB RMS: 21 dB over, 15.75 dB down.
        // Each tone sits far from both splits, so what leaks into the
        // other bands (which aren't turned down) is under -60 dB.
        for (band, freq, low, high) in [
            (0, 100.0, 1000.0, 8000.0),
            (1, 1500.0, 200.0, 12_000.0),
            (2, 16_000.0, 200.0, 3000.0),
        ] {
            let (gain, meters) = measured(&downward(low, high), freq, -6.0);
            println!(
                "{freq} Hz: {gain:.2} dB; gain changes {:.2} {:.2} {:.2}",
                meters[meter(0, METER_GAIN_CHANGE)],
                meters[meter(1, METER_GAIN_CHANGE)],
                meters[meter(2, METER_GAIN_CHANGE)]
            );
            assert!((gain + 15.75).abs() < 0.3, "{gain}");
            for other in 0..BANDS {
                let change = meters[meter(other, METER_GAIN_CHANGE)];
                if other == band {
                    assert!((change + 15.75).abs() < 0.3, "{change}");
                    let input = meters[meter(band, METER_INPUT)];
                    let output = meters[meter(band, METER_OUTPUT)];
                    assert!((input + 9.03).abs() < 0.3, "{input}");
                    assert!((output - (input - 15.75)).abs() < 0.3, "{output}");
                } else {
                    assert!(change.abs() < 0.01, "band {other}: {change}");
                }
            }
        }
    }

    #[test]
    fn upward_compression_lifts_quiet_sound() {
        let mut values = neutral(values(&[(LOW_SPLIT, 300.0), (HIGH_SPLIT, 3000.0)]));
        values[param(1, UP_THRESHOLD)] = -40.0;
        values[param(1, UP_RATIO)] = 4.0;
        values[param(1, RELEASE)] = 50.0;
        // -50 dB peak, -53 dB RMS: 13 dB under, lifted by 9.75 dB.
        let (gain, meters) = measured(&values, 1000.0, -50.0);
        println!("quiet tone lifted {gain:.2} dB");
        assert!((gain - 9.75).abs() < 0.3, "{gain}");
        assert!((meters[meter(1, METER_GAIN_CHANGE)] - 9.75).abs() < 0.3);
        // Half the depth, half the change in dB.
        values[DEPTH] = 50.0;
        let (half, _) = measured(&values, 1000.0, -50.0);
        assert!((half - 4.875).abs() < 0.2, "{half}");
        // Near silence it lets go rather than lifting the noise floor.
        values[DEPTH] = 100.0;
        let (floor, _) = measured(&values, 1000.0, -110.0);
        assert!(floor.abs() < 0.1, "{floor}");
    }

    #[test]
    fn the_defaults_are_the_ott_squeeze() {
        // Loud and quiet tones in each band come out far closer together
        // than they went in, the quiet ones lifted well above where they
        // were. (The upward compression also lifts what of a tone leaks
        // into the neighbouring bands, part of OTT's sound.)
        let defaults: [f32; COUNT] = defaults(PARAMS);
        for freq in [40.0, 470.0, 8000.0] {
            let (loud, _) = measured(&defaults, freq, -6.0);
            let (quiet, meters) = measured(&defaults, freq, -46.0);
            let spread_in = 40.0;
            let spread_out = spread_in + loud - quiet;
            println!(
                "{freq} Hz: loud {loud:+.1} dB, quiet {quiet:+.1} dB: 40 dB in, {spread_out:.1} dB out; meters {:?}",
                &meters[..9]
            );
            assert!(spread_out < 20.0, "{spread_out}");
            assert!(quiet > 10.0 && loud < -5.0);
        }
    }

    #[test]
    fn silence_stays_silent() {
        let mut values: [f32; COUNT] = defaults(PARAMS);
        values[INPUT] = 24.0;
        values[OUTPUT] = 24.0;
        let mut multiband = multiband(&values);
        let (mut left, mut right) = (vec![0.0; 4800], vec![0.0; 4800]);
        run(&mut multiband, &mut left, &mut right, 64, &Context::DEFAULT);
        assert!(left.iter().chain(&right).all(|&s| s == 0.0));
        let mut meters = [0.0; crate::MAX_METERS];
        multiband.meters(&mut meters);
        assert!(meters.iter().all(|m| m.is_finite()));
        assert_eq!(meters[meter(1, METER_INPUT)], -100.0);
    }
}
