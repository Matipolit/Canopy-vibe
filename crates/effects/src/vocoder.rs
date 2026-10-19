//! Vocoder: the track's own sound (the carrier, say a synth) speaks with
//! the spectrum of another track routed in as its sidechain (the
//! modulator, say a voice). Without a sidechain the carrier passes through
//! untouched.
//!
//! A channel vocoder: both signals are split into the same 8 to 32 bands,
//! spaced evenly in octaves between the low and high frequencies. Each band
//! is a fourth-order Butterworth bandpass (two trapezoidal SVF resonators,
//! staggered just above and below the band's centre: a flat top and steep
//! skirts, so neighbouring bands don't bleed into each other much), with
//! its width a share of the spacing between bands. In each band an
//! envelope follower measures the modulator's level, with the attack and
//! release set (but never faster than a cycle of the band's frequency,
//! which would ripple), and a slower one the carrier's. The carrier's band
//! is then turned to the modulator's level: multiplied by the ratio of the
//! two. So whatever the carrier's own tilt (a saw's falling highs, a pad's
//! thin lows), the output has the modulator's spectrum and roughly its
//! loudness. A band where the carrier has next to nothing is not turned up
//! past a floor (40 dB below the carrier's loudest band, and never below
//! -60 dBFS), so filter leakage and noise stay quiet.
//!
//! The meters are the modulator's level in each band, lowest first: the
//! voice's spectrum as the vocoder hears it.
//!
//! Formant shift moves the carrier's bands up or down against the
//! modulator's, so the voice's formants land higher or lower: a smaller or
//! larger "throat". Consonants like "s" and "t" are mostly noise above the
//! bands, which a pitched carrier can't reproduce; sibilance adds
//! high-passed white noise following the level of the modulator's own
//! highs. Band layout changes glide; changing the number of bands dips the
//! vocoded sound for a few milliseconds to swap the filters. Attack and
//! release glide too, and retune only the envelopes, never the filters, so
//! modulating them stays cheap. A modulator's moves arrive within a block
//! (see "Modulation" in `lib.rs`). A sidechain coming or going crossfades
//! with the dry carrier.

use instruments::ParamDescriptor;

use crate::{Category, Context, Effect, EffectDescriptor, MAX_METERS, Values, choice, float};
use dsp::flush;
use dsp::svf::{BUTTERWORTH_Q, Coefs, Shape, SvfState, prewarp};
use dsp::{Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.vocoder",
    name: "Vocoder",
    description: "Imposes a sidechained voice's spectrum on the track's sound",
    category: Category::Modulation,
    params: PARAMS,
    meters: &[
        "Band 1 level (linear RMS of the modulator, lowest band)",
        "Band 2 level",
        "Band 3 level",
        "Band 4 level",
        "Band 5 level",
        "Band 6 level",
        "Band 7 level",
        "Band 8 level",
        "Band 9 level",
        "Band 10 level",
        "Band 11 level",
        "Band 12 level",
        "Band 13 level",
        "Band 14 level",
        "Band 15 level",
        "Band 16 level",
        "Band 17 level",
        "Band 18 level",
        "Band 19 level",
        "Band 20 level",
        "Band 21 level",
        "Band 22 level",
        "Band 23 level",
        "Band 24 level",
        "Band 25 level",
        "Band 26 level",
        "Band 27 level",
        "Band 28 level",
        "Band 29 level",
        "Band 30 level",
        "Band 31 level",
        "Band 32 level (0 past the number of bands)",
    ],
    sidechain: true,
    create: || Box::new(Vocoder::new()),
    ..EffectDescriptor::BASE
};

/// The band counts offered, as labels and values.
const BAND_LABELS: &[&str] = &[
    "8", "10", "12", "14", "16", "18", "20", "22", "24", "26", "28", "30", "32",
];

pub const MAX_BANDS: usize = 32;

/// The number of bands for the `bands` parameter's value.
pub fn band_count(value: f32) -> usize {
    let index = (value.round().max(0.0) as usize).min(BAND_LABELS.len() - 1);
    8 + 2 * index
}

const PARAMS: &[ParamDescriptor] = &[
    choice("bands", "Bands", BAND_LABELS, 4),
    float("low", "Lowest band", 50.0, 1000.0, 100.0, "Hz", true),
    float("high", "Highest band", 1000.0, 18_000.0, 8000.0, "Hz", true),
    float("width", "Band width", 25.0, 200.0, 100.0, "%", false),
    float("attack", "Attack", 0.5, 100.0, 4.0, "ms", true),
    float("release", "Release", 5.0, 1000.0, 60.0, "ms", true),
    float("formant", "Formant shift", -12.0, 12.0, 0.0, "st", false),
    float("sibilance", "Sibilance", 0.0, 100.0, 30.0, "%", false),
    float("mix", "Mix", 0.0, 100.0, 100.0, "%", false),
    float("output", "Output", -24.0, 24.0, 0.0, "dB", false),
];

const COUNT: usize = 10;

const BANDS: usize = 0;
const LOW: usize = 1;
const HIGH: usize = 2;
const WIDTH: usize = 3;
const ATTACK: usize = 4;
const RELEASE: usize = 5;
const FORMANT: usize = 6;
const SIBILANCE: usize = 7;
const MIX: usize = 8;
const OUTPUT: usize = 9;

/// How long mix, output and sibilance take to reach a new value.
const SMOOTH_MS: f32 = 20.0;
/// How long the band layout (range, width, formant) glides.
const LAYOUT_GLIDE_MS: f32 = 30.0;
/// The layout follows its glide every this many samples.
const CHUNK: usize = 16;
/// How long the vocoded sound dips to swap the number of bands.
const SWITCH_MS: f32 = 10.0;
/// How long a sidechain arriving or leaving crossfades.
const PRESENCE_MS: f32 = 20.0;
/// The carrier's envelope: slow, so turning its bands to the modulator's
/// levels doesn't flatten its waveform.
const CARRIER_MS: f32 = 20.0;
/// Where the sibilance (and the modulator's highs it follows) starts.
const SIBILANCE_HZ: f32 = 5000.0;
/// The sibilance follower's attack and release.
const SIBILANCE_TIMES_MS: (f32, f32) = (1.0, 40.0);
/// A band's carrier level (mean square) is never taken as lower than this
/// share of the loudest band's...
const RELATIVE_FLOOR: f32 = 1e-4;
/// ...nor than this (-60 dBFS RMS), so a near-silent carrier isn't turned up.
const ABSOLUTE_FLOOR: f32 = 1e-6;

/// One-pole coefficient for a time constant of `seconds`.
fn coefficient(seconds: f32, sample_rate: f32) -> f32 {
    1.0 - (-1.0 / (seconds * sample_rate).max(1e-3)).exp()
}

/// A band's two sections, for a Butterworth bandpass of `width` octaves
/// between its -3 dB points: each section's frequency as a multiple of the
/// band's centre and its Q, and the gain that brings the pair's peak to 1.
///
/// The bandpass is the classic transform of a second-order Butterworth
/// lowpass, `S = (s² + 1) / (d·s)` with `d` the bandwidth as a share of the
/// centre frequency. Its four poles come in two conjugate pairs, one
/// slightly above the centre and one below: two resonators, staggered.
/// That makes the top flat and the skirts steeper than two identical
/// resonators of the same width would (-12 rather than -9 dB at the next
/// band, -25 rather than -19 two bands away, at the default spacing).
fn butterworth_pair(width: f32) -> [(f32, f32); 2] {
    let d = f64::from((0.5 * width).exp2() - (-0.5 * width).exp2());
    // Lowpass pole p = (-1 + j) / √2; the bandpass poles solve
    // s² - p·d·s + 1 = 0.
    let h = std::f64::consts::FRAC_1_SQRT_2;
    let (pr, pi) = (-h * d, h * d);
    // (p·d)² - 4
    let (a, b) = (pr * pr - pi * pi - 4.0, 2.0 * pr * pi);
    let r = a.hypot(b);
    let (qr, qi) = (((r + a) / 2.0).sqrt(), b.signum() * ((r - a) / 2.0).sqrt());
    [(pr + qr, pi + qi), (pr - qr, pi - qi)].map(|(re, im)| {
        let (re, im) = (re / 2.0, im / 2.0);
        let omega = re.hypot(im);
        (omega as f32, (omega / (-2.0 * re)) as f32)
    })
}

/// The gain that brings the peak of `butterworth_pair(width)`'s two unity
/// peak resonators in series to 1.
fn pair_gain(width: f32, sections: [(f32, f32); 2]) -> f32 {
    let d = (0.5 * width).exp2() - (-0.5 * width).exp2();
    let [(w1, q1), (w2, q2)] = sections;
    d * d * q1 * q2 / (w1 * w2)
}

/// Bandpass coefficients (peak gain 1, times a scale) for a bank, one
/// array per term, so the per-band loop runs over plain arrays.
#[derive(Debug, Clone, Copy)]
struct BankCoefs {
    a1: [f32; MAX_BANDS],
    a2: [f32; MAX_BANDS],
    a3: [f32; MAX_BANDS],
    /// What the bandpass output is multiplied by: the damping, which
    /// brings its peak to 1, times any further gain.
    out: [f32; MAX_BANDS],
}

impl BankCoefs {
    const SILENT: BankCoefs = BankCoefs {
        a1: [0.0; MAX_BANDS],
        a2: [0.0; MAX_BANDS],
        a3: [0.0; MAX_BANDS],
        out: [0.0; MAX_BANDS],
    };

    fn set(&mut self, band: usize, freq: f32, q: f32, gain: f32, sample_rate: f32) {
        let g = prewarp(freq, sample_rate);
        let k = 1.0 / q;
        let a1 = 1.0 / (1.0 + g * (g + k));
        self.a1[band] = a1;
        self.a2[band] = g * a1;
        self.a3[band] = g * g * a1;
        self.out[band] = k * gain;
    }
}

/// One bandpass per band: Simper's SVF memory, one array per term.
#[derive(Debug, Clone, Copy)]
struct Bank {
    s1: [f32; MAX_BANDS],
    s2: [f32; MAX_BANDS],
}

impl Bank {
    const CLEAR: Bank = Bank {
        s1: [0.0; MAX_BANDS],
        s2: [0.0; MAX_BANDS],
    };

    /// Band `i`'s bandpass of `x`.
    #[inline(always)]
    fn tick(&mut self, c: &BankCoefs, i: usize, x: f32) -> f32 {
        let v3 = x - self.s2[i];
        let v1 = c.a1[i] * self.s1[i] + c.a2[i] * v3;
        let v2 = self.s2[i] + c.a2[i] * self.s1[i] + c.a3[i] * v3;
        self.s1[i] = 2.0 * v1 - self.s1[i];
        self.s2[i] = 2.0 * v2 - self.s2[i];
        c.out[i] * v1
    }

    fn flush(&mut self) {
        for v in self.s1.iter_mut().chain(self.s2.iter_mut()) {
            *v = flush(*v);
        }
    }
}

pub struct Vocoder {
    values: Values<COUNT>,
    sample_rate: f32,
    /// The number of bands running.
    bands: usize,
    /// Each band's two sections, for the modulator and the carrier.
    modulator_coefs: [BankCoefs; 2],
    carrier_coefs: [BankCoefs; 2],
    /// The two sections' memories: the modulator's, and the carrier's for
    /// each channel.
    modulator: [Bank; 2],
    carrier: [[Bank; 2]; 2],
    /// Per band: mean-square levels of the modulator and the carrier.
    modulator_level: [f32; MAX_BANDS],
    carrier_level: [f32; MAX_BANDS],
    /// Per band: envelope coefficients for the modulator's attack and
    /// release and the carrier's follower.
    attack: [f32; MAX_BANDS],
    release: [f32; MAX_BANDS],
    carrier_speed: [f32; MAX_BANDS],
    /// The floor under the carrier's band levels, from the loudest one.
    floor: f32,
    log_low: Smoothed,
    log_high: Smoothed,
    width: Smoothed,
    formant: Smoothed,
    /// The attack and release, as log2 seconds. They glide, and only the
    /// envelope speeds are worked out again as they do, never the filters.
    log_attack: Smoothed,
    log_release: Smoothed,
    /// Samples until the layout next follows its glide.
    countdown: usize,
    /// The vocoded sound's level while swapping the number of bands.
    switch: Smoothed,
    /// How much of the vocoder is heard against the dry carrier: up while a
    /// sidechain is routed in, down while not.
    presence: Smoothed,
    sibilance_coefs: Coefs,
    /// High-pass pairs for the modulator's highs and for the noise.
    modulator_highs: [SvfState; 2],
    noise_highs: [SvfState; 2],
    highs_level: f32,
    highs_speed: (f32, f32),
    /// Scales the high-passed noise to unit RMS.
    noise_scale: f32,
    noise_state: u32,
    sibilance: Smoothed,
    mix: Smoothed,
    output: Smoothed,
    /// Passing the carrier through with everything cleared, waiting for a
    /// sidechain.
    idle: bool,
}

const NOISE_SEED: u32 = 0x2545_f491;

impl Vocoder {
    pub fn new() -> Vocoder {
        let mut vocoder = Vocoder {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            bands: 16,
            modulator_coefs: [BankCoefs::SILENT; 2],
            carrier_coefs: [BankCoefs::SILENT; 2],
            modulator: [Bank::CLEAR; 2],
            carrier: [[Bank::CLEAR; 2]; 2],
            modulator_level: [0.0; MAX_BANDS],
            carrier_level: [0.0; MAX_BANDS],
            attack: [0.0; MAX_BANDS],
            release: [0.0; MAX_BANDS],
            carrier_speed: [0.0; MAX_BANDS],
            floor: ABSOLUTE_FLOOR,
            log_low: Smoothed::new(0.0, LAYOUT_GLIDE_MS),
            log_high: Smoothed::new(0.0, LAYOUT_GLIDE_MS),
            width: Smoothed::new(1.0, LAYOUT_GLIDE_MS),
            formant: Smoothed::new(0.0, LAYOUT_GLIDE_MS),
            log_attack: Smoothed::new(0.0, SMOOTH_MS),
            log_release: Smoothed::new(0.0, SMOOTH_MS),
            countdown: 0,
            switch: Smoothed::new(1.0, SWITCH_MS),
            presence: Smoothed::new(0.0, PRESENCE_MS),
            sibilance_coefs: Coefs::IDENTITY,
            modulator_highs: [SvfState::default(); 2],
            noise_highs: [SvfState::default(); 2],
            highs_level: 0.0,
            highs_speed: (1.0, 1.0),
            noise_scale: 1.0,
            noise_state: NOISE_SEED,
            sibilance: Smoothed::new(0.0, SMOOTH_MS),
            mix: Smoothed::new(1.0, SMOOTH_MS),
            output: Smoothed::new(1.0, SMOOTH_MS),
            idle: true,
        };
        for index in 0..COUNT {
            vocoder.apply(index, Pace::Knob);
        }
        vocoder.prepare(48_000.0, 0);
        vocoder
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            LOW => self.log_low.move_to(value.log2(), pace),
            HIGH => self.log_high.move_to(value.log2(), pace),
            WIDTH => self.width.move_to(value / 100.0, pace),
            FORMANT => self.formant.move_to(value / 12.0, pace),
            ATTACK => self.log_attack.move_to((value * 0.001).log2(), pace),
            RELEASE => self.log_release.move_to((value * 0.001).log2(), pace),
            SIBILANCE => self.sibilance.move_to(value / 100.0, pace),
            MIX => self.mix.move_to(value / 100.0, pace),
            OUTPUT => self.output.move_to(dsp::db_to_gain(value), pace),
            // The band count switches with a dip, started in `process`.
            _ => {}
        }
    }

    fn wanted_bands(&self) -> usize {
        band_count(self.values[BANDS])
    }

    /// Works out every band's filters and envelope speeds from the current
    /// (gliding) range, width, formant shift, attack and release.
    fn layout(&mut self) {
        self.tune_bands();
        self.update_speeds();
    }

    /// Where the bands lie: the lowest's and the spacing in log2 Hz.
    fn band_spacing(&self) -> (f32, f32) {
        let low = self.log_low.current();
        // At least half an octave between the lowest and highest bands.
        let span = (self.log_high.current() - low).max(0.5);
        (low, span / (self.bands - 1) as f32)
    }

    /// Works out every band's filters from the current (gliding) range,
    /// width and formant shift.
    fn tune_bands(&mut self) {
        let sample_rate = self.sample_rate;
        let bands = self.bands;
        let (low, spacing) = self.band_spacing();
        let width = (spacing * self.width.current()).max(0.01);
        let sections = butterworth_pair(width);
        // The second section carries the pair's gain.
        let gains = [1.0, pair_gain(width, sections)];
        let shift = self.formant.current();
        for band in 0..bands {
            let octave = low + band as f32 * spacing;
            let freq = octave.exp2();
            let carrier_freq = (octave + shift).exp2();
            for (section, ((ratio, q), gain)) in sections.into_iter().zip(gains).enumerate() {
                self.modulator_coefs[section].set(band, freq * ratio, q, gain, sample_rate);
                self.carrier_coefs[section].set(band, carrier_freq * ratio, q, gain, sample_rate);
            }
        }
    }

    /// Works out every band's envelope speeds from the current (gliding)
    /// attack and release and where the band lies.
    fn update_speeds(&mut self) {
        let sample_rate = self.sample_rate;
        let (low, spacing) = self.band_spacing();
        let attack = self.log_attack.current().exp2();
        let release = self.log_release.current().exp2();
        for band in 0..self.bands {
            // Never faster than a cycle of the band (two for the slower
            // followers), whose level would otherwise ripple.
            let cycle = (-(low + band as f32 * spacing)).exp2();
            self.attack[band] = coefficient(attack.max(cycle), sample_rate);
            self.release[band] = coefficient(release.max(2.0 * cycle), sample_rate);
            self.carrier_speed[band] =
                coefficient((CARRIER_MS * 0.001).max(2.0 * cycle), sample_rate);
        }
    }

    fn clear_bands(&mut self) {
        self.modulator = [Bank::CLEAR; 2];
        self.carrier = [[Bank::CLEAR; 2]; 2];
        self.modulator_level = [0.0; MAX_BANDS];
        self.carrier_level = [0.0; MAX_BANDS];
        self.floor = ABSOLUTE_FLOOR;
    }

    fn clear(&mut self) {
        self.clear_bands();
        self.modulator_highs = [SvfState::default(); 2];
        self.noise_highs = [SvfState::default(); 2];
        self.highs_level = 0.0;
        self.noise_state = NOISE_SEED;
    }

    /// Every `CHUNK` samples: the layout follows its glide, and a change in
    /// the number of bands dips out, swaps and comes back.
    fn update(&mut self) {
        let mut moved = false;
        for smoothed in [
            &mut self.log_low,
            &mut self.log_high,
            &mut self.width,
            &mut self.formant,
        ] {
            if smoothed.is_smoothing() {
                smoothed.tick();
                moved = true;
            }
        }
        let wanted = self.wanted_bands();
        if wanted != self.bands && self.switch.target() > 0.0 {
            self.switch.set_target(0.0);
        }
        if self.switch.target() == 0.0 && !self.switch.is_smoothing() {
            self.bands = wanted;
            self.clear_bands();
            self.switch.set_target(1.0);
            moved = true;
        }
        let times_moved = self.log_attack.is_smoothing() || self.log_release.is_smoothing();
        if times_moved {
            self.log_attack.tick();
            self.log_release.tick();
        }
        if moved {
            self.layout();
        } else if times_moved {
            self.update_speeds();
        }
    }

    /// White noise with unit RMS.
    #[inline]
    fn noise(&mut self) -> f32 {
        let state = dsp::xorshift32(&mut self.noise_state);
        // Uniform in ±√3.
        (state as f32 / u32::MAX as f32 * 2.0 - 1.0) * 1.732_050_8
    }
}

impl Default for Vocoder {
    fn default() -> Self {
        Vocoder::new()
    }
}

impl Effect for Vocoder {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        for smoothed in [
            &mut self.switch,
            &mut self.presence,
            &mut self.sibilance,
            &mut self.mix,
            &mut self.output,
        ] {
            smoothed.prepare(sample_rate);
        }
        for smoothed in [
            &mut self.log_low,
            &mut self.log_high,
            &mut self.width,
            &mut self.formant,
            &mut self.log_attack,
            &mut self.log_release,
        ] {
            smoothed.prepare_every(sample_rate, CHUNK);
        }
        self.sibilance_coefs =
            Coefs::new(&Shape::highpass(SIBILANCE_HZ, BUTTERWORTH_Q, sample_rate));
        self.highs_speed = (
            coefficient(SIBILANCE_TIMES_MS.0 * 0.001, sample_rate),
            coefficient(SIBILANCE_TIMES_MS.1 * 0.001, sample_rate),
        );
        // White noise high-passed keeps about the share of its power above
        // the corner.
        let kept = (1.0 - 2.0 * SIBILANCE_HZ / sample_rate).max(0.05);
        self.noise_scale = 1.0 / kept.sqrt();
        self.reset();
    }

    fn reset(&mut self) {
        for smoothed in [
            &mut self.log_low,
            &mut self.log_high,
            &mut self.width,
            &mut self.formant,
            &mut self.log_attack,
            &mut self.log_release,
            &mut self.sibilance,
            &mut self.mix,
            &mut self.output,
        ] {
            smoothed.settle();
        }
        self.switch.set_immediate(1.0);
        self.presence.set_immediate(0.0);
        self.bands = self.wanted_bands();
        self.layout();
        self.clear();
        self.idle = true;
        self.countdown = 0;
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: &Context) {
        let first = self.values.is_fresh();
        self.values.played();
        let len = left.len().min(right.len());
        let present = if context.sidechain.is_some() {
            1.0
        } else {
            0.0
        };
        if first {
            self.presence.set_immediate(present);
        } else if self.presence.target() != present {
            self.presence.set_target(present);
        }
        if present == 0.0 && !self.presence.is_smoothing() {
            // No sidechain: the carrier passes untouched, and the vocoder
            // starts afresh when one arrives.
            if !self.idle {
                self.clear();
                self.idle = true;
            }
            return;
        }
        self.idle = false;
        let (side_l, side_r) = context.sidechain.unwrap_or((&[], &[]));

        for n in 0..len {
            if self.countdown == 0 {
                self.update();
                self.countdown = CHUNK;
            }
            self.countdown -= 1;

            let side = |channel: &[f32]| channel.get(n).copied().unwrap_or(0.0);
            let modulator = 0.5 * (side(side_l) + side(side_r));
            let modulator = if modulator.is_finite() {
                modulator
            } else {
                0.0
            };
            let (dry_l, dry_r) = (left[n], right[n]);

            let (mut wet_l, mut wet_r) = (0.0f32, 0.0f32);
            let mut loudest = 0.0f32;
            let floor = self.floor;
            let [mod_a, mod_b] = &mut self.modulator;
            let [[left_a, left_b], [right_a, right_b]] = &mut self.carrier;
            let [ma, mb] = &self.modulator_coefs;
            let [ca, cb] = &self.carrier_coefs;
            for i in 0..self.bands {
                let m = mod_b.tick(mb, i, mod_a.tick(ma, i, modulator));
                let power = m * m;
                let level = &mut self.modulator_level[i];
                let speed = if power > *level {
                    self.attack[i]
                } else {
                    self.release[i]
                };
                *level += speed * (power - *level);

                let l = left_b.tick(cb, i, left_a.tick(ca, i, dry_l));
                let r = right_b.tick(cb, i, right_a.tick(ca, i, dry_r));
                let carrier = &mut self.carrier_level[i];
                *carrier += self.carrier_speed[i] * (0.5 * (l * l + r * r) - *carrier);
                // Plain comparisons: `f32::max` spends instructions on
                // NaN, which these can't be.
                if *carrier > loudest {
                    loudest = *carrier;
                }
                let over = if *carrier > floor { *carrier } else { floor };
                let gain = (*level / over).sqrt();
                wet_l += l * gain;
                wet_r += r * gain;
            }
            self.floor = (loudest * RELATIVE_FLOOR).max(ABSOLUTE_FLOOR);

            // Sibilance: noise above the bands, at the level of the
            // modulator's own highs there.
            let [highs_a, highs_b] = &mut self.modulator_highs;
            let highs = highs_b.tick(
                &self.sibilance_coefs,
                highs_a.tick(&self.sibilance_coefs, modulator),
            );
            let power = highs * highs;
            let speed = if power > self.highs_level {
                self.highs_speed.0
            } else {
                self.highs_speed.1
            };
            self.highs_level += speed * (power - self.highs_level);
            let noise = self.noise();
            let [noise_a, noise_b] = &mut self.noise_highs;
            let hiss = noise_b.tick(
                &self.sibilance_coefs,
                noise_a.tick(&self.sibilance_coefs, noise),
            );
            let hiss = hiss * self.noise_scale * self.highs_level.sqrt() * self.sibilance.tick();

            let level = self.switch.tick() * self.output.tick();
            let mix = self.mix.tick();
            let presence = self.presence.tick();
            let wet_l = level * (wet_l + hiss);
            let wet_r = level * (wet_r + hiss);
            left[n] = dry_l + presence * mix * (wet_l - dry_l);
            right[n] = dry_r + presence * mix * (wet_r - dry_r);
        }

        for bank in self
            .modulator
            .iter_mut()
            .chain(self.carrier.iter_mut().flatten())
        {
            bank.flush();
        }
        for level in self
            .modulator_level
            .iter_mut()
            .chain(self.carrier_level.iter_mut())
        {
            *level = flush(*level);
        }
        for state in self.modulator_highs.iter_mut().chain(&mut self.noise_highs) {
            state.flush();
        }
        self.highs_level = flush(self.highs_level);
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
        for (band, slot) in out.iter_mut().enumerate().take(MAX_BANDS) {
            *slot = if band < self.bands {
                self.modulator_level[band].sqrt()
            } else {
                0.0
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, db, noise, rms, set, sine};

    fn vocoder(settings: &[(&str, f32)]) -> Vocoder {
        let mut vocoder = Vocoder::new();
        vocoder.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut vocoder, id, value);
        }
        vocoder.reset();
        vocoder
    }

    /// Runs `carrier` through `vocoder` with `modulator` as the sidechain.
    fn vocode(vocoder: &mut Vocoder, carrier: &[f32], modulator: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let (mut left, mut right) = (carrier.to_vec(), carrier.to_vec());
        for start in (0..carrier.len()).step_by(64) {
            let end = (start + 64).min(carrier.len());
            let context = Context {
                sidechain: Some((&modulator[start..end], &modulator[start..end])),
                ..Context::DEFAULT
            };
            vocoder.process(&mut left[start..end], &mut right[start..end], &context);
        }
        (left, right)
    }

    /// Level in dB of `x` in a narrow band around `freq`.
    fn band_db(x: &[f32], freq: f32) -> f64 {
        let k = 1.0 / 8.0;
        let coefs = Coefs::new(&Shape {
            g: prewarp(freq, RATE),
            k,
            m0: 0.0,
            m1: k,
            m2: 0.0,
        });
        let (mut a, mut b) = (SvfState::default(), SvfState::default());
        let out: Vec<f32> = x
            .iter()
            .map(|&s| b.tick(&coefs, a.tick(&coefs, s)))
            .collect();
        db(rms(&out[out.len() / 2..]))
    }

    fn sum(a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter().zip(b).map(|(x, y)| x + y).collect()
    }

    /// `|H|` of a unity-peak resonator at `ratio` of the centre with `q`,
    /// at `freq` times the centre (the analog prototype).
    fn resonator(ratio: f32, q: f32, freq: f32) -> f32 {
        let x = freq / ratio - ratio / freq;
        1.0 / (1.0 + q * q * x * x).sqrt()
    }

    #[test]
    fn each_band_is_a_butterworth_bandpass() {
        for width in [0.1, 0.42, 1.0, 2.5] {
            let sections = butterworth_pair(width);
            let gain = pair_gain(width, sections);
            let response = |freq: f32| {
                let [(r1, q1), (r2, q2)] = sections;
                gain * resonator(r1, q1, freq) * resonator(r2, q2, freq)
            };
            assert!((sections[0].0 * sections[1].0 - 1.0).abs() < 1e-5);
            assert!((response(1.0) - 1.0).abs() < 1e-4, "{width}");
            // -3 dB at the edges, and flat in between.
            for edge in [(0.5 * width).exp2(), (-0.5 * width).exp2()] {
                let db = 20.0 * response(edge).log10();
                assert!((db + 3.01).abs() < 0.02, "{width}: {db}");
            }
            assert!(response((0.2 * width).exp2()) > 0.97);
        }
    }

    #[test]
    fn without_a_sidechain_the_carrier_passes_untouched() {
        let carrier = noise(9600, 5);
        let mut vocoder = vocoder(&[("formant", 7.0), ("sibilance", 100.0)]);
        let (mut left, mut right) = (carrier.clone(), carrier.clone());
        crate::testing::run(&mut vocoder, &mut left, &mut right, 64, &Context::DEFAULT);
        assert_eq!(left, carrier);
        assert_eq!(right, carrier);
        assert_eq!(vocoder.meter(), 0.0);
    }

    #[test]
    fn a_modulator_opens_only_its_own_bands() {
        // A broadband carrier; a modulator with energy at 300 Hz and 2.5 kHz
        // only.
        let len = 48_000;
        let carrier: Vec<f32> = noise(len, 7).iter().map(|s| 0.3 * s).collect();
        let modulator = sum(&sine(300.0, 0.3, len, RATE), &sine(2500.0, 0.3, len, RATE));
        let mut vocoder = vocoder(&[("sibilance", 0.0)]);
        let (left, _) = vocode(&mut vocoder, &carrier, &modulator);

        // 16 bands from 100 Hz to 8 kHz are 0.42 octaves apart: 300 Hz sits
        // between bands 3 and 4, 2.5 kHz on band 11. The other frequencies
        // are band centres at least three bands from either.
        let open = [band_db(&left, 300.0), band_db(&left, 2500.0)];
        let shut = [
            band_db(&left, 100.0),
            band_db(&left, 773.0),
            band_db(&left, 1035.0),
            band_db(&left, 5973.0),
        ];
        println!("open bands {open:.1?} dB, others {shut:.1?} dB");
        // What reaches a shut band is mostly the open bands' skirts.
        let quietest_open = open.iter().copied().fold(f64::INFINITY, f64::min);
        let loudest_shut = shut.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(
            quietest_open > loudest_shut + 20.0,
            "{open:?} against {shut:?}"
        );

        // The meters show the same: the bands holding 300 Hz and 2.5 kHz
        // stand out.
        let mut meters = [0.0; MAX_METERS];
        vocoder.meters(&mut meters);
        let loudest = meters[..16].iter().copied().fold(0.0f32, f32::max);
        let lit = meters[..16].iter().filter(|&&m| m > 0.1 * loudest).count();
        assert!((2..=6).contains(&lit), "{meters:?}");
        assert!(meters[16..].iter().all(|&m| m == 0.0));
    }

    #[test]
    fn the_output_follows_the_modulators_level() {
        let len = 48_000;
        // A dull carrier (a 110 Hz saw-ish buzz) still speaks at the
        // modulator's level: its bands are turned to the modulator's.
        let carrier: Vec<f32> = (0..len)
            .map(|n| 0.5 * (((n as f32 * 110.0 / RATE) % 1.0) * 2.0 - 1.0))
            .collect();
        let modulator: Vec<f32> = noise(len, 3).iter().map(|s| 0.1 * s).collect();
        let mut vocoder = vocoder(&[("sibilance", 0.0)]);
        let (left, _) = vocode(&mut vocoder, &carrier, &modulator);
        let out = db(rms(&left[len / 2..]));
        // White noise between 100 Hz and 8 kHz holds about a third of its
        // power: the output should be in that region.
        let expected = db(rms(&modulator) * (7900.0f64 / 24_000.0).sqrt());
        println!("output {out:.1} dB, modulator in the bands {expected:.1} dB");
        assert!((out - expected).abs() < 4.0, "{out} against {expected}");
    }

    #[test]
    fn formant_shift_moves_the_voice_up() {
        let len = 48_000;
        let carrier: Vec<f32> = noise(len, 11).iter().map(|s| 0.3 * s).collect();
        let modulator = sine(1000.0, 0.3, len, RATE);
        let mut plain = vocoder(&[("sibilance", 0.0)]);
        let (left, _) = vocode(&mut plain, &carrier, &modulator);
        let (at_1k, at_2k) = (band_db(&left, 1000.0), band_db(&left, 2000.0));
        let mut meters = [0.0; MAX_METERS];
        plain.meters(&mut meters);
        println!("unshifted: 1 kHz {at_1k:.1} dB, 2 kHz {at_2k:.1} dB; bands {meters:.3?}");
        assert!(at_1k > at_2k + 15.0);
        let mut shifted = vocoder(&[("sibilance", 0.0), ("formant", 12.0)]);
        let (left, _) = vocode(&mut shifted, &carrier, &modulator);
        let (at_1k, at_2k) = (band_db(&left, 1000.0), band_db(&left, 2000.0));
        println!("an octave up: 1 kHz {at_1k:.1} dB, 2 kHz {at_2k:.1} dB");
        assert!(at_2k > at_1k + 15.0);
    }

    #[test]
    fn sibilance_lets_consonants_through() {
        let len = 48_000;
        // A carrier with nothing up high, and a modulator that is all hiss.
        let carrier = sine(220.0, 0.5, len, RATE);
        let modulator: Vec<f32> = noise(len, 13).iter().map(|s| 0.3 * s).collect();
        let highs = |sibilance: f32| {
            let mut vocoder = vocoder(&[("sibilance", sibilance)]);
            let (left, _) = vocode(&mut vocoder, &carrier, &modulator);
            band_db(&left, 10_000.0)
        };
        let (without, with) = (highs(0.0), highs(100.0));
        println!("10 kHz: {without:.1} dB without sibilance, {with:.1} dB with");
        assert!(with > without + 30.0);
    }

    #[test]
    fn changing_the_band_count_and_losing_the_sidechain_are_smooth() {
        let len = 24_000;
        let carrier = sine(220.0, 0.5, len, RATE);
        let modulator = sine(220.0, 0.5, len, RATE);
        // Without sibilance: cutting the modulator off mid-wave is a click
        // in the modulator itself, which the sibilance rightly hisses at.
        let mut vocoder = vocoder(&[("sibilance", 0.0)]);
        let (mut left, mut right) = (carrier.clone(), carrier.clone());
        for (block, start) in (0..len).step_by(64).enumerate() {
            let end = start + 64;
            if block == 100 {
                set(&mut vocoder, "bands", 12.0);
            }
            let context = Context {
                sidechain: (block < 250).then(|| (&modulator[start..end], &modulator[start..end])),
                ..Context::DEFAULT
            };
            vocoder.process(&mut left[start..end], &mut right[start..end], &context);
        }
        let largest = |x: &[f32]| {
            x.windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max)
        };
        // The vocoded sine before any change, and the dry one, set how far
        // the output may move in one sample.
        let steady = largest(&left[2000..6400]).max(largest(&carrier));
        let worst = largest(&left[2000..]);
        println!("largest step: steady {steady:.4}, through the changes {worst:.4}");
        assert!(worst < 1.3 * steady, "{worst} against {steady}");
        assert_eq!(vocoder.bands, 32);
        // Long after the sidechain went: the carrier, untouched.
        assert_eq!(&left[len - 1000..], &carrier[len - 1000..]);
    }
}
