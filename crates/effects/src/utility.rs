//! Utility: a mixing tool for level and the stereo image. It turns the
//! sound up or down, balances it between the sides, narrows or widens it,
//! makes the bass mono below a crossover, flips either side's polarity,
//! swaps the sides or keeps only one of them, the mid or the side, holds
//! one side back a little (Haas widening), takes out DC, and mutes. At its
//! defaults it passes the sound on bit for bit.
//! Its meters are the output's correlation and each side's peak and RMS.
//!
//! **Signal path**, per sample: the DC filter; the channel matrix
//! (polarity, then swap, then which channels are kept, multiplied into one
//! 2×2 matrix); the side delay; mid and side, where bass mono and width
//! act; then one gain per side, made of gain, balance and mute.
//!
//! - **Channels**: Left and Right put that side on both; Mid puts
//!   `(L + R) / 2` on both, which is mono; Side keeps only `(L - R) / 2`,
//!   on the left and inverted on the right: the stereo image without its
//!   centre. They act after the swap, so Left with Swap keeps the right.
//! - **Side delay** holds the left or the right back by up to
//!   [`MAX_DELAY_MS`], in whole samples, as FL's Stereo Enhancer's phase
//!   offset and Stereo Shaper's delay do: a few milliseconds make a mono
//!   source wide (the Haas effect), more is a slapback on one side. It
//!   comes after the matrix, so Left, Right or Mid make the mono source it
//!   widens, and before width and bass mono, so width scales the side it
//!   makes and bass mono takes the comb it leaves out of the lows. It is an
//!   offset between the sides, not latency: nothing else waits for it, and
//!   none is reported. Its lines are fed all along, so a delay dialled in
//!   starts from the real past; with no side delayed they aren't read.
//! - **Width** scales the side and leaves the mid: 0% is mono (both sides
//!   exactly equal), 100% unchanged, 200% the side twice as loud.
//! - **Bass mono** splits the sound at `mono_below` with a Linkwitz-Riley
//!   24 dB/octave crossover (two Butterworth filters in series, as in the
//!   multiband) and keeps only the mid below it: the side goes through the
//!   crossover's high-pass, and the mid through the allpass that the low
//!   and high halves sum to. So above the split mid and side stay in phase
//!   and the image is kept, and mono sound comes out at the level it went
//!   in at every frequency.
//! - **Balance** leaves both sides at unity in the centre and fades the far
//!   side along a quarter cosine as it turns (-3 dB half way, silent at
//!   the end), as the mixer's pan does. On a mono source it is a pan.
//! - **DC filter**: a second-order Butterworth high-pass at [`DC_HZ`],
//!   -0.02 dB at 20 Hz.
//!
//! The filters run in double precision: at 5 Hz and 192 kHz their
//! integrator gain is under 1e-4, where single precision would lose the
//! offset they track.
//!
//! **Switching** glides over [`SMOOTH_MS`]: each side's gain, the matrix's
//! entries (so a polarity flip dips through silence rather than jumping,
//! and a swap crossfades) and the width. The DC filter and bass mono
//! crossfade from the dry sound, their filters running all along so they
//! come in settled; the crossover frequency glides on a log scale. Each
//! side's delay line is read where a tap that crossfades between whole
//! sample ages says (as in the delay), so moving the delay neither clicks
//! nor bends the pitch, and changing sides fades one side's delay out as
//! the other's fades in. A modulator (see "Modulation" in `lib.rs`) moves
//! everything within a block instead: gains, width and the crossover glide
//! there, and the side delay glides between samples, bending that side's
//! pitch a little as it goes. Stages that do nothing are skipped, which is
//! what keeps the defaults exact.
//!
//! No latency: the side delay is an offset between the sides (above).

use std::f32::consts::FRAC_PI_2;

use instruments::ParamDescriptor;

use crate::dynamics::coefficient;
use crate::line::{Ring, Tap};
use crate::meter::{PeakMeter, block_peak, meter_db};
use crate::{Category, Context, Effect, EffectDescriptor, OFF_ON, Values, choice, float};
use dsp::flush;
use dsp::svf::prewarp;
use dsp::{MAX_BLOCK, Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.utility",
    name: "Utility",
    description: "Gain, balance, width, bass mono, Haas delay, polarity and channel tools",
    category: Category::Space,
    params: PARAMS,
    meters: &[
        "Output correlation (-1 opposite, 0 unrelated or silent, 1 mono; over 300 ms)",
        "Output peak left (dB)",
        "Output peak right (dB)",
        "Output RMS left (dB, 300 ms)",
        "Output RMS right (dB, 300 ms)",
    ],
    create: || Box::new(Utility::new()),
    ..EffectDescriptor::BASE
};

/// What [`CHANNELS`] keeps, [`STEREO`] to [`SIDE`].
pub const CHANNEL_MODES: &[&str] = &["Stereo", "Left", "Right", "Mid", "Side"];
/// Which side [`DELAY_SIDE`] holds back: [`DELAY_OFF`] to [`DELAY_RIGHT`].
pub const DELAY_SIDES: &[&str] = &["Off", "Left", "Right"];

const PARAMS: &[ParamDescriptor] = &[
    float("gain", "Gain", -60.0, 24.0, 0.0, "dB", false),
    float("balance", "Balance", -100.0, 100.0, 0.0, "%", false),
    float("width", "Width", 0.0, 200.0, 100.0, "%", false),
    choice("bass_mono", "Bass mono", OFF_ON, 0),
    float("mono_below", "Mono below", 20.0, 1000.0, 120.0, "Hz", true),
    choice("channels", "Channels", CHANNEL_MODES, STEREO),
    choice("swap", "Swap sides", OFF_ON, 0),
    choice("invert_left", "Invert left", OFF_ON, 0),
    choice("invert_right", "Invert right", OFF_ON, 0),
    choice("dc", "DC filter", OFF_ON, 0),
    choice("mute", "Mute", OFF_ON, 0),
    float("delay", "Side delay", 0.1, MAX_DELAY_MS, 15.0, "ms", true),
    choice("delay_side", "Delayed side", DELAY_SIDES, DELAY_OFF),
];

const COUNT: usize = 13;

pub const GAIN: usize = 0;
/// -100 (left) to 100 (right).
pub const BALANCE: usize = 1;
pub const WIDTH: usize = 2;
pub const BASS_MONO: usize = 3;
pub const MONO_BELOW: usize = 4;
pub const CHANNELS: usize = 5;
pub const SWAP: usize = 6;
pub const INVERT_LEFT: usize = 7;
pub const INVERT_RIGHT: usize = 8;
pub const DC: usize = 9;
pub const MUTE: usize = 10;
/// How far [`DELAY_SIDE`] is held back, in ms; heard only with a side
/// chosen, which is what lets its knob be logarithmic.
pub const DELAY: usize = 11;
pub const DELAY_SIDE: usize = 12;

/// [`CHANNELS`] values.
pub const STEREO: usize = 0;
pub const LEFT: usize = 1;
pub const RIGHT: usize = 2;
pub const MID: usize = 3;
pub const SIDE: usize = 4;

/// [`DELAY_SIDE`] values.
pub const DELAY_OFF: usize = 0;
pub const DELAY_LEFT: usize = 1;
pub const DELAY_RIGHT: usize = 2;

/// Meter slots, in [`DESCRIPTOR`]'s `meters` order, all of the output.
/// The correlation comes first so that silence reads 0, as
/// [`Effect::meter`] expects.
pub const METER_CORRELATION: usize = 0;
pub const METER_PEAK_LEFT: usize = 1;
pub const METER_PEAK_RIGHT: usize = 2;
pub const METER_RMS_LEFT: usize = 3;
pub const METER_RMS_RIGHT: usize = 4;

/// How long gains, the matrix, width and the crossfades take to reach a
/// new value.
const SMOOTH_MS: f32 = 20.0;
/// Samples between the crossover frequency's glide steps, each of which
/// works out its filters' coefficients again.
const CHUNK: usize = 16;
/// The DC filter's cutoff.
pub const DC_HZ: f32 = 5.0;
/// The longest side delay.
pub const MAX_DELAY_MS: f32 = 500.0;
/// The fastest a modulated side delay moves, in samples per sample.
const MAX_TAP_SLOPE: f32 = 0.5;
/// How long the correlation and RMS meters average over.
const IMAGE_MS: f32 = 300.0;
/// The meters read no sample louder than this (+120 dB), so garbage still
/// makes finite readings.
const LOUDEST: f32 = 1e6;
/// Below this power in either side (-100 dB each) the correlation reads 0.
const SILENT_POWER: f32 = 1e-20;
/// Filter state smaller than this is flushed to zero, as in `line.rs`.
const TINY: f64 = 1e-20;

/// A 2×2 matrix, `[[ll, lr], [rl, rr]]`: left out is `ll · left + lr ·
/// right`.
type Matrix = [[f32; 2]; 2];
const IDENTITY: Matrix = [[1.0, 0.0], [0.0, 1.0]];

fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    std::array::from_fn(|i| std::array::from_fn(|j| a[i][0] * b[0][j] + a[i][1] * b[1][j]))
}

/// What polarity, swap and channel mode do together, in that order, as
/// one matrix. Its entries are 0, ±0.5 and ±1, so it is exact.
fn channel_matrix(values: &[f32]) -> Matrix {
    let on = |index: usize| values[index] >= 0.5;
    let sign = |index: usize| if on(index) { -1.0 } else { 1.0 };
    let polarity = [[sign(INVERT_LEFT), 0.0], [0.0, sign(INVERT_RIGHT)]];
    let swap = if on(SWAP) {
        [[0.0, 1.0], [1.0, 0.0]]
    } else {
        IDENTITY
    };
    let channels = match values[CHANNELS] as usize {
        LEFT => [[1.0, 0.0], [1.0, 0.0]],
        RIGHT => [[0.0, 1.0], [0.0, 1.0]],
        MID => [[0.5, 0.5], [0.5, 0.5]],
        SIDE => [[0.5, -0.5], [-0.5, 0.5]],
        _ => IDENTITY,
    };
    multiply(&channels, &multiply(&swap, &polarity))
}

/// Each side's level, `(left, right)`, for a balance of `balance` (-1
/// left to 1 right): unity in the centre, the far side fading along a
/// quarter cosine, exactly silent at the end.
pub fn balance_gains(balance: f32) -> (f32, f32) {
    let fade = |turn: f32| (turn.clamp(0.0, 1.0) * FRAC_PI_2).cos().max(0.0);
    (fade(balance), fade(-balance))
}

/// How alike the sides are, from their mean squares and mean product: 0
/// when either is silent.
fn correlation(ll: f32, rr: f32, lr: f32) -> f32 {
    let power = ll * rr;
    if power > SILENT_POWER {
        (lr / power.sqrt()).clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Coefficients of a Butterworth state-variable filter.
#[derive(Debug, Clone, Copy)]
struct Coefs {
    k: f64,
    a1: f64,
    a2: f64,
    a3: f64,
}

impl Coefs {
    fn butterworth(freq: f32, sample_rate: f32) -> Coefs {
        let g = f64::from(prewarp(freq, sample_rate));
        // 1 / Q of a Butterworth response.
        let k = std::f64::consts::SQRT_2;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        Coefs {
            k,
            a1,
            a2,
            a3: g * a2,
        }
    }
}

/// One channel of the trapezoidal state-variable filter of `filter.rs`, in
/// double precision, giving its lowpass, bandpass and highpass at once.
#[derive(Debug, Clone, Copy, Default)]
struct Svf {
    s1: f64,
    s2: f64,
}

impl Svf {
    /// (lowpass, bandpass, highpass) for input `x`.
    #[inline]
    fn tick(&mut self, c: &Coefs, x: f64) -> (f64, f64, f64) {
        let v3 = x - self.s2;
        let v1 = c.a1 * self.s1 + c.a2 * v3;
        let v2 = self.s2 + c.a2 * self.s1 + c.a3 * v3;
        self.s1 = 2.0 * v1 - self.s1;
        self.s2 = 2.0 * v2 - self.s2;
        (v2, v1, x - c.k * v1 - v2)
    }

    /// Ends a decay in zeros rather than denormals, and forgets NaN and
    /// infinity that came in with the block.
    fn flush(&mut self) {
        let clean = |x: f64| {
            if x.abs() >= TINY && x.abs() < f64::INFINITY {
                x
            } else {
                0.0
            }
        };
        self.s1 = clean(self.s1);
        self.s2 = clean(self.s2);
    }
}

/// Bass mono's filters: the mid's allpass, and the side's Linkwitz-Riley
/// high-pass, two Butterworth ones in series.
#[derive(Debug, Clone, Copy, Default)]
struct BassMono {
    mid: Svf,
    side: [Svf; 2],
}

impl BassMono {
    /// The mid, and the side without its lows, both shifted in phase
    /// alike.
    #[inline]
    fn tick(&mut self, c: &Coefs, mid: f32, side: f32) -> (f32, f32) {
        let mid = f64::from(mid);
        let (_, band, _) = self.mid.tick(c, mid);
        let (_, _, high) = self.side[0].tick(c, f64::from(side));
        let (_, _, high) = self.side[1].tick(c, high);
        ((mid - 2.0 * c.k * band) as f32, high as f32)
    }

    fn flush(&mut self) {
        self.mid.flush();
        for svf in &mut self.side {
            svf.flush();
        }
    }
}

pub struct Utility {
    values: Values<COUNT>,
    sample_rate: f32,
    /// Each side's output gain: gain, balance and mute together.
    gains: [Smoothed; 2],
    /// The channel matrix's entries: `ll`, `lr`, `rl`, `rr`.
    matrix: [Smoothed; 4],
    width: Smoothed,
    /// How much of the DC filter's and of bass mono's sound is heard, 0
    /// to 1.
    dc: Smoothed,
    bass: Smoothed,
    /// The crossover frequency as log2, gliding once per [`CHUNK`].
    crossover: Smoothed,
    crossover_coefs: Coefs,
    dc_coefs: Coefs,
    dc_filters: [Svf; 2],
    bass_mono: BassMono,
    /// Each side's delay line, and where it's read: at age 0, or the delay
    /// on the delayed side.
    lines: [Ring; 2],
    taps: [Tap; 2],
    /// The longest age the lines hold.
    longest: usize,
    /// One crossfade of the taps, in samples.
    fade: usize,
    /// Samples left until the crossover's next glide step.
    countdown: usize,
    /// The output's mean squares and mean product over [`IMAGE_MS`], for
    /// the correlation and RMS meters: `ll`, `rr`, `lr`.
    image: [f32; 3],
    /// `1 -` their one-pole coefficient.
    image_rate: f32,
    peaks: [PeakMeter; 2],
}

impl Utility {
    pub fn new() -> Utility {
        let smoothed = |value: f32| Smoothed::new(value, SMOOTH_MS);
        let mut utility = Utility {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            gains: [smoothed(1.0), smoothed(1.0)],
            matrix: [smoothed(1.0), smoothed(0.0), smoothed(0.0), smoothed(1.0)],
            width: smoothed(1.0),
            dc: smoothed(0.0),
            bass: smoothed(0.0),
            crossover: smoothed(120f32.log2()),
            crossover_coefs: Coefs::butterworth(120.0, 48_000.0),
            dc_coefs: Coefs::butterworth(DC_HZ, 48_000.0),
            dc_filters: [Svf::default(); 2],
            bass_mono: BassMono::default(),
            lines: [Ring::default(), Ring::default()],
            taps: [Tap::new(0.0), Tap::new(0.0)],
            longest: 0,
            fade: 1,
            countdown: 0,
            image: [0.0; 3],
            image_rate: 1.0,
            peaks: [PeakMeter::default(), PeakMeter::default()],
        };
        utility.prepare(48_000.0, 0);
        utility
    }

    /// The smoothers on the audio path, which tick every sample.
    fn path(&mut self) -> [&mut Smoothed; 9] {
        let [left, right] = &mut self.gains;
        let [ll, lr, rl, rr] = &mut self.matrix;
        [
            left,
            right,
            ll,
            lr,
            rl,
            rr,
            &mut self.width,
            &mut self.dc,
            &mut self.bass,
        ]
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`. Switches always take the knob's pace.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        let on = if value >= 0.5 { 1.0 } else { 0.0 };
        match index {
            GAIN | BALANCE | MUTE => {
                let pace = if index == MUTE { Pace::Knob } else { pace };
                let gain = if self.values[MUTE] >= 0.5 {
                    0.0
                } else {
                    dsp::db_to_gain(self.values[GAIN])
                };
                let (left, right) = balance_gains(self.values[BALANCE] / 100.0);
                self.gains[0].move_to(gain * left, pace);
                self.gains[1].move_to(gain * right, pace);
            }
            WIDTH => self.width.move_to(value / 100.0, pace),
            BASS_MONO => self.bass.set_target(on),
            MONO_BELOW => self.crossover.move_to(value.log2(), pace),
            CHANNELS | SWAP | INVERT_LEFT | INVERT_RIGHT => {
                let matrix = channel_matrix(&self.values);
                for (smoothed, &target) in self.matrix.iter_mut().zip(matrix.iter().flatten()) {
                    smoothed.set_target(target);
                }
            }
            DC => self.dc.set_target(on),
            DELAY if pace == Pace::Modulation => {
                // Between samples, gliding there within the block.
                let ages = self.delay_ages(false);
                for (tap, age) in self.taps.iter_mut().zip(ages) {
                    tap.glide_to(age, MAX_BLOCK);
                }
            }
            DELAY | DELAY_SIDE => {
                let ages = self.delay_ages(true);
                for (tap, age) in self.taps.iter_mut().zip(ages) {
                    tap.set_target(age);
                }
            }
            _ => {}
        }
    }

    /// How far back each side's line is read, in samples (whole ones if
    /// `whole`): the delay on the delayed side, nothing on the other.
    fn delay_ages(&self, whole: bool) -> [f32; 2] {
        let samples = self.values[DELAY] * self.sample_rate / 1000.0;
        let samples = if whole { samples.round() } else { samples };
        let samples = samples.min(self.longest as f32);
        match self.values[DELAY_SIDE] as usize {
            DELAY_LEFT => [samples, 0.0],
            DELAY_RIGHT => [0.0, samples],
            _ => [0.0, 0.0],
        }
    }

    fn update_crossover(&mut self) {
        self.crossover_coefs =
            Coefs::butterworth(self.crossover.current().exp2(), self.sample_rate);
    }
}

impl Default for Utility {
    fn default() -> Self {
        Utility::new()
    }
}

impl Effect for Utility {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        for smoothed in self.path() {
            smoothed.prepare(sample_rate);
        }
        self.crossover.prepare_every(sample_rate, CHUNK);
        self.dc_coefs = Coefs::butterworth(DC_HZ, sample_rate);
        self.image_rate = 1.0 - coefficient(IMAGE_MS, sample_rate);
        for peak in &mut self.peaks {
            peak.prepare(sample_rate);
        }
        self.longest = (MAX_DELAY_MS * sample_rate / 1000.0).ceil() as usize;
        self.fade = ((SMOOTH_MS * 0.001 * sample_rate) as usize).max(1);
        for (line, tap) in self.lines.iter_mut().zip(&mut self.taps) {
            line.allocate(self.longest + 4);
            tap.set_fade_length(self.fade);
            tap.set_max_slope(MAX_TAP_SLOPE);
        }
        for index in 0..COUNT {
            self.apply(index, Pace::Knob);
        }
        self.reset();
    }

    fn reset(&mut self) {
        for smoothed in self.path() {
            smoothed.settle();
        }
        self.crossover.settle();
        self.update_crossover();
        self.dc_filters = [Svf::default(); 2];
        self.bass_mono = BassMono::default();
        let ages = self.delay_ages(true);
        for ((line, tap), age) in self.lines.iter_mut().zip(&mut self.taps).zip(ages) {
            line.clear();
            tap.jump(age);
        }
        self.countdown = 0;
        self.image = [0.0; 3];
        for peak in &mut self.peaks {
            peak.reset();
        }
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.values.played();
        let len = left.len().min(right.len());
        let (left, right) = (&mut left[..len], &mut right[..len]);
        // The stages with something to do this block; the others leave
        // the sound as it is.
        let dc_on = self.dc.is_away_from(0.0);
        let matrix_on = self
            .matrix
            .iter()
            .zip(IDENTITY.iter().flatten())
            .any(|(smoothed, &rest)| smoothed.is_away_from(rest));
        let delay_on =
            self.values[DELAY_SIDE] as usize != DELAY_OFF || self.taps.iter().any(Tap::is_moving);
        let bass_on = self.bass.is_away_from(0.0);
        let width_on = self.width.is_away_from(1.0);
        let gains_on = self.gains.iter().any(|gain| gain.is_away_from(1.0));
        let image_rate = self.image_rate;
        let meterable = |x: f32| {
            if x.is_finite() {
                x.clamp(-LOUDEST, LOUDEST)
            } else {
                0.0
            }
        };

        for n in 0..len {
            if self.countdown == 0 {
                if self.crossover.is_smoothing() {
                    self.crossover.tick();
                    self.update_crossover();
                }
                self.countdown = CHUNK;
            }
            self.countdown -= 1;

            let (mut l, mut r) = (left[n], right[n]);
            // The filters run whether they are heard or not, so they come
            // in settled.
            let [dc_l, dc_r] = &mut self.dc_filters;
            let high_l = dc_l.tick(&self.dc_coefs, f64::from(l)).2 as f32;
            let high_r = dc_r.tick(&self.dc_coefs, f64::from(r)).2 as f32;
            if dc_on {
                let dc = self.dc.tick();
                l += dc * (high_l - l);
                r += dc * (high_r - r);
            }
            if matrix_on {
                let [ll, lr, rl, rr] = &mut self.matrix;
                let (ll, lr, rl, rr) = (ll.tick(), lr.tick(), rl.tick(), rr.tick());
                (l, r) = (ll * l + lr * r, rl * l + rr * r);
            }
            let [line_l, line_r] = &mut self.lines;
            line_l.push(l);
            line_r.push(r);
            if delay_on {
                let [tap_l, tap_r] = &mut self.taps;
                l = tap_l.advance().read(line_l);
                r = tap_r.advance().read(line_r);
            }
            let (mut mid, mut side) = (0.5 * (l + r), 0.5 * (l - r));
            let (mono_mid, mono_side) = self.bass_mono.tick(&self.crossover_coefs, mid, side);
            if bass_on {
                let bass = self.bass.tick();
                mid += bass * (mono_mid - mid);
                side += bass * (mono_side - side);
            }
            if width_on {
                side *= self.width.tick();
            }
            if bass_on || width_on {
                (l, r) = (mid + side, mid - side);
            }
            if gains_on {
                let [gain_l, gain_r] = &mut self.gains;
                l *= gain_l.tick();
                r *= gain_r.tick();
            }
            left[n] = l;
            right[n] = r;

            let (l, r) = (meterable(l), meterable(r));
            let [ll, rr, lr] = &mut self.image;
            *ll += (l * l - *ll) * image_rate;
            *rr += (r * r - *rr) * image_rate;
            *lr += (l * r - *lr) * image_rate;
        }
        for svf in &mut self.dc_filters {
            svf.flush();
        }
        self.bass_mono.flush();
        for mean in &mut self.image {
            *mean = flush(*mean);
        }
        self.peaks[0].update(block_peak(left), len);
        self.peaks[1].update(block_peak(right), len);
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
        let [ll, rr, lr] = self.image;
        out[METER_CORRELATION] = correlation(ll, rr, lr);
        out[METER_PEAK_LEFT] = self.peaks[0].db();
        out[METER_PEAK_RIGHT] = self.peaks[1].db();
        out[METER_RMS_LEFT] = meter_db(ll.max(0.0).sqrt());
        out[METER_RMS_RIGHT] = meter_db(rr.max(0.0).sqrt());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meter::METER_FLOOR_DB;
    use crate::testing::{RATE, db, noise, rms, run, set, sine};

    fn utility(settings: &[(&str, f32)]) -> Utility {
        let mut utility = Utility::new();
        utility.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut utility, id, value);
        }
        utility.reset();
        utility
    }

    /// A stereo sound with some width: a part both sides share and a part
    /// each has of its own.
    fn stereo(len: usize) -> (Vec<f32>, Vec<f32>) {
        let shared = noise(len, 1);
        let (a, b) = (noise(len, 2), noise(len, 3));
        let left = (0..len).map(|n| 0.4 * shared[n] + 0.2 * a[n]).collect();
        let right = (0..len).map(|n| 0.4 * shared[n] + 0.2 * b[n]).collect();
        (left, right)
    }

    fn processed(settings: &[(&str, f32)], left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let mut utility = utility(settings);
        let (mut l, mut r) = (left.to_vec(), right.to_vec());
        run(&mut utility, &mut l, &mut r, 64, &Context::DEFAULT);
        (l, r)
    }

    /// What `settings` make of a second and a half of `left` and `right`:
    /// the last second only, once the filters have settled, which holds a
    /// whole number of cycles of any whole-Hz tone.
    fn settled(settings: &[(&str, f32)], left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let (l, r) = processed(settings, left, right);
        let skip = l.len() - RATE as usize;
        (l[skip..].to_vec(), r[skip..].to_vec())
    }

    fn mid_side(left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let mid = left.iter().zip(right).map(|(l, r)| 0.5 * (l + r)).collect();
        let side = left.iter().zip(right).map(|(l, r)| 0.5 * (l - r)).collect();
        (mid, side)
    }

    #[test]
    fn the_defaults_pass_the_sound_bit_for_bit() {
        let (left, right) = stereo(19_200);
        assert_eq!(processed(&[], &left, &right), (left.clone(), right.clone()));

        // Every parameter turned up while the sound plays, then back:
        // once the glides end, exact again.
        let mut utility = utility(&[]);
        let (mut l, mut r) = (left.clone(), right.clone());
        let mut at = 0;
        let mut play = |utility: &mut Utility, until: usize| {
            run(
                utility,
                &mut l[at..until],
                &mut r[at..until],
                64,
                &Context::DEFAULT,
            );
            at = until;
        };
        play(&mut utility, 4800);
        for (index, param) in PARAMS.iter().enumerate() {
            let top = match param.kind {
                instruments::ParameterKind::Float { max, .. } => max,
                instruments::ParameterKind::Enum { labels, .. } => (labels.len() - 1) as f32,
            };
            utility.set_param(index, top);
        }
        play(&mut utility, 7200);
        for (index, param) in PARAMS.iter().enumerate() {
            utility.set_param(index, param.default_value());
        }
        play(&mut utility, 19_200);
        let exact = 7200 + 960 + 128;
        assert_eq!(&l[exact..], &left[exact..]);
        assert_eq!(&r[exact..], &right[exact..]);
    }

    #[test]
    fn gain_is_exact_in_db() {
        let (left, right) = stereo(4800);
        for gain in [-60.0f32, -24.0, -6.0, 0.5, 6.0, 24.0] {
            let (l, r) = processed(&[("gain", gain)], &left, &right);
            let factor = 10f64.powf(f64::from(gain) / 20.0);
            for (out, input) in l.iter().chain(&r).zip(left.iter().chain(&right)) {
                let expected = f64::from(*input) * factor;
                let error = (f64::from(*out) - expected).abs();
                assert!(
                    error <= 1e-6 * expected.abs(),
                    "{gain} dB: {out} for {expected}"
                );
            }
        }
    }

    #[test]
    fn width_zero_is_mono_and_more_widens() {
        let (left, right) = stereo(9600);
        let (l, r) = processed(&[("width", 0.0)], &left, &right);
        assert_eq!(l, r);
        let (mid, side) = mid_side(&left, &right);
        for (out, mid) in l.iter().zip(&mid) {
            assert!((out - mid).abs() < 1e-6);
        }

        for (width, gain) in [(50.0, 0.5), (150.0, 1.5), (200.0, 2.0)] {
            let (l, r) = processed(&[("width", width)], &left, &right);
            let (mid_out, side_out) = mid_side(&l, &r);
            let side_change = db(rms(&side_out) / rms(&side));
            println!("width {width}%: side {side_change:+.3} dB");
            assert!((side_change - db(gain)).abs() < 0.001, "{side_change}");
            assert!(db(rms(&mid_out) / rms(&mid)).abs() < 0.001);
        }
    }

    #[test]
    fn bass_mono_makes_the_lows_mono_and_keeps_the_highs_wide() {
        let len = 3 * RATE as usize / 2;
        let settings = [("bass_mono", 1.0), ("mono_below", 200.0)];
        let tone = |freq: f64, amplitude: f32, phase: f64| -> Vec<f32> {
            let step = std::f64::consts::TAU * freq / f64::from(RATE);
            (0..len)
                .map(|n| amplitude * (step * n as f64 + phase).sin() as f32)
                .collect()
        };

        // 30 Hz, louder on the left and out of step: the same on both
        // sides once through, but for what the crossover lets by: the
        // side, 2.7 octaves under it, down 66 dB.
        let (l, r) = settled(&settings, &tone(30.0, 0.5, 0.0), &tone(30.0, 0.3, 1.0));
        let difference: Vec<f32> = l.iter().zip(&r).map(|(l, r)| l - r).collect();
        let apart = db(rms(&difference) / rms(&l));
        println!("30 Hz under a 200 Hz split: the sides differ by {apart:.1} dB");
        assert!(apart < -60.0, "{apart}");

        // Pure side, two octaves under the split: down 48 dB.
        let (l, r) = settled(&settings, &tone(50.0, 0.5, 0.0), &tone(50.0, -0.5, 0.0));
        let (_, side) = mid_side(&l, &r);
        let left_over = db(rms(&side) / (0.5 / std::f64::consts::SQRT_2));
        println!("side at 50 Hz: {left_over:.1} dB");
        assert!(left_over < -47.0, "{left_over}");

        // 5 kHz: mid and side as they were.
        let (left, right) = (tone(5000.0, 0.5, 0.0), tone(5000.0, 0.2, 2.0));
        let (l, r) = settled(&settings, &left, &right);
        let (mid_in, side_in) =
            mid_side(&left[len - RATE as usize..], &right[len - RATE as usize..]);
        let (mid_out, side_out) = mid_side(&l, &r);
        assert!(db(rms(&side_out) / rms(&side_in)).abs() < 0.01);
        assert!(db(rms(&mid_out) / rms(&mid_in)).abs() < 0.01);
    }

    #[test]
    fn the_crossover_sums_flat() {
        let len = 3 * RATE as usize / 2;
        let settings = [("bass_mono", 1.0), ("mono_below", 150.0)];
        let silence = vec![0.0; len];
        let mut worst = 0.0f64;
        for freq in [
            20.0, 50.0, 100.0, 140.0, 150.0, 160.0, 300.0, 1000.0, 5000.0, 15_000.0,
        ] {
            let input = sine(freq, 0.5, len, RATE);
            let level = rms(&input[len - RATE as usize..]);
            // A mono tone comes out at its level...
            let (l, r) = settled(&settings, &input, &input);
            let mono = db(rms(&l) / level);
            assert_eq!(l, r);
            // ...and so does the mono sum of a tone on one side only.
            let (l, r) = settled(&settings, &input, &silence);
            let sum: Vec<f32> = l.iter().zip(&r).map(|(l, r)| l + r).collect();
            let one_side = db(rms(&sum) / level);
            println!("{freq:6} Hz: mono {mono:+.4} dB, one side summed {one_side:+.4} dB");
            worst = worst.max(mono.abs()).max(one_side.abs());
        }
        assert!(worst < 0.1, "{worst} dB");
    }

    #[test]
    fn the_side_delay_holds_one_side_back_by_whole_samples() {
        let (left, right) = stereo(9600);
        for (rate, ms, samples) in [(RATE, 10.0, 480), (44_100.0, 10.0, 441), (RATE, 0.5, 24)] {
            for side in [DELAY_LEFT, DELAY_RIGHT] {
                let mut utility = Utility::new();
                utility.prepare(rate, 64);
                set(&mut utility, "delay", ms);
                set(&mut utility, "delay_side", side as f32);
                let (mut l, mut r) = (left.clone(), right.clone());
                run(&mut utility, &mut l, &mut r, 64, &Context::DEFAULT);
                let ((held, held_in), (other, other_in)) = if side == DELAY_LEFT {
                    ((&l, &left), (&r, &right))
                } else {
                    ((&r, &right), (&l, &left))
                };
                assert_eq!(other, other_in);
                assert!(held[..samples].iter().all(|&s| s == 0.0));
                let kept = held_in.len() - samples;
                assert_eq!(&held[samples..], &held_in[..kept], "{ms} ms at {rate} Hz");
            }
        }
    }

    #[test]
    fn moving_the_side_delay_never_clicks() {
        // A low tone, so a click would stand out against its own steps.
        let len = RATE as usize;
        let tone = sine(100.0, 0.5, len, RATE);
        let natural = tone
            .windows(2)
            .fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
        let mut utility = utility(&[("delay", 5.0), ("delay_side", DELAY_RIGHT as f32)]);
        let (mut l, mut r) = (tone.clone(), tone.clone());
        let quarter = len / 4;
        run(
            &mut utility,
            &mut l[..quarter],
            &mut r[..quarter],
            64,
            &Context::DEFAULT,
        );
        // Longer, then over to the other side, then off.
        let changes = [
            ("delay", 12.0),
            ("delay_side", DELAY_LEFT as f32),
            ("delay_side", DELAY_OFF as f32),
        ];
        for (i, (id, value)) in changes.into_iter().enumerate() {
            set(&mut utility, id, value);
            let span = quarter * (i + 1)..quarter * (i + 2);
            run(
                &mut utility,
                &mut l[span.clone()],
                &mut r[span],
                64,
                &Context::DEFAULT,
            );
        }
        for out in [&l, &r] {
            let jump = out
                .windows(2)
                .fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
            println!("largest step {jump:.5}, the tone's own {natural:.5}");
            assert!(jump < 1.3 * natural, "{jump} against {natural}");
        }
        // Off again: exact once the taps are home.
        let tail = len - 2400;
        assert_eq!((&l[tail..], &r[tail..]), (&tone[tail..], &tone[tail..]));
    }

    #[test]
    fn invert_flips_a_side() {
        let (left, right) = stereo(4800);
        let negative = |x: &[f32]| x.iter().map(|s| -s).collect::<Vec<f32>>();
        let (l, r) = processed(&[("invert_left", 1.0)], &left, &right);
        assert_eq!((l, r), (negative(&left), right.clone()));
        let (l, r) = processed(&[("invert_right", 1.0)], &left, &right);
        assert_eq!((l, r), (left.clone(), negative(&right)));
        let both = [("invert_left", 1.0), ("invert_right", 1.0)];
        assert_eq!(
            processed(&both, &left, &right),
            (negative(&left), negative(&right))
        );
    }

    #[test]
    fn swap_swaps_the_sides() {
        let (left, right) = stereo(4800);
        assert_eq!(
            processed(&[("swap", 1.0)], &left, &right),
            (right.clone(), left.clone())
        );
        // Channels act after the swap.
        let settings = [("swap", 1.0), ("channels", LEFT as f32)];
        assert_eq!(processed(&settings, &left, &right), (right.clone(), right));
    }

    #[test]
    fn channel_modes_keep_a_side_the_mid_or_the_side() {
        let (left, right) = stereo(4800);
        let mode = |channels: usize| processed(&[("channels", channels as f32)], &left, &right);
        assert_eq!(mode(LEFT), (left.clone(), left.clone()));
        assert_eq!(mode(RIGHT), (right.clone(), right.clone()));
        let (mid, side) = mid_side(&left, &right);
        let (l, r) = mode(MID);
        assert_eq!(l, r);
        assert!(l.iter().zip(&mid).all(|(a, b)| (a - b).abs() < 1e-6));
        let (l, r) = mode(SIDE);
        assert!(l.iter().zip(&side).all(|(a, b)| (a - b).abs() < 1e-6));
        assert!(l.iter().zip(&r).all(|(l, r)| *r == -l));
    }

    #[test]
    fn balance_fades_the_far_side() {
        let (left, right) = stereo(4800);
        let (l, r) = processed(&[("balance", -100.0)], &left, &right);
        assert_eq!(l, left);
        assert!(r.iter().all(|&s| s == 0.0));
        let (l, r) = processed(&[("balance", 50.0)], &left, &right);
        assert_eq!(r, right);
        let half = std::f32::consts::FRAC_1_SQRT_2;
        assert!(
            l.iter()
                .zip(&left)
                .all(|(o, i)| (o - i * half).abs() < 1e-6)
        );
        assert_eq!(balance_gains(0.0), (1.0, 1.0));
        assert_eq!(balance_gains(1.0), (0.0, 1.0));
    }

    #[test]
    fn the_dc_filter_takes_out_an_offset() {
        let len = 2 * RATE as usize;
        let tone = sine(100.0, 0.25, len, RATE);
        let left: Vec<f32> = tone.iter().map(|s| s + 0.5).collect();
        let right: Vec<f32> = tone.iter().map(|s| s - 0.3).collect();
        let (l, r) = processed(&[("dc", 1.0)], &left, &right);
        let mean = |x: &[f32]| x.iter().map(|&s| f64::from(s)).sum::<f64>() / x.len() as f64;
        let last = len - RATE as usize;
        for out in [&l, &r] {
            let offset = mean(&out[last..]);
            let level = db(rms(&out[last..]) / rms(&tone[last..]));
            println!("after a second: offset {offset:.2e}, 100 Hz at {level:+.4} dB");
            assert!(offset.abs() < 1e-5, "{offset}");
            assert!(level.abs() < 0.01, "{level}");
        }
    }

    #[test]
    fn every_change_glides() {
        let changes = [
            ("gain", 24.0),
            ("gain", -60.0),
            ("balance", -100.0),
            ("width", 0.0),
            ("width", 200.0),
            ("bass_mono", 1.0),
            ("channels", SIDE as f32),
            ("swap", 1.0),
            ("invert_left", 1.0),
            ("dc", 1.0),
            ("mute", 1.0),
        ];
        for (id, value) in changes {
            // A steady sound, so any jump in it is the change's, after a
            // second for the filters to settle on it.
            let mut utility = utility(&[]);
            let steady = |len: usize| (vec![0.6f32; len], vec![-0.2f32; len]);
            let (mut l, mut r) = steady(RATE as usize);
            run(&mut utility, &mut l, &mut r, 64, &Context::DEFAULT);
            let (mut l, mut r) = steady(4800);
            utility.process(&mut l[..64], &mut r[..64], &Context::DEFAULT);
            set(&mut utility, id, value);
            run(
                &mut utility,
                &mut l[64..],
                &mut r[64..],
                64,
                &Context::DEFAULT,
            );
            for out in [&l, &r] {
                let change = (out[4799] - out[63]).abs();
                let jump = out
                    .windows(2)
                    .fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
                assert!(
                    jump <= change / 500.0 + 1e-6,
                    "{id} {value}: a jump of {jump} in a change of {change}"
                );
            }
            let moved = (l[4799] - l[63]).abs().max((r[4799] - r[63]).abs());
            assert!(moved > 0.1, "{id} {value} did nothing");
        }
    }

    #[test]
    fn mute_fades_to_silence() {
        let (left, right) = stereo(4800);
        let (l, r) = processed(&[("mute", 1.0)], &left, &right);
        assert!(l.iter().chain(&r).all(|&s| s == 0.0));
    }

    #[test]
    fn meters_read_correlation_and_levels() {
        let len = 3 * RATE as usize;
        let read = |left: &[f32], right: &[f32]| {
            let mut utility = utility(&[]);
            let (mut l, mut r) = (left.to_vec(), right.to_vec());
            run(&mut utility, &mut l, &mut r, 64, &Context::DEFAULT);
            let mut meters = [0.0; crate::MAX_METERS];
            utility.meters(&mut meters);
            meters
        };
        let (a, b) = (noise(len, 4), noise(len, 5));
        let inverted: Vec<f32> = a.iter().map(|s| -s).collect();
        let mono = read(&a, &a)[METER_CORRELATION];
        let opposite = read(&a, &inverted)[METER_CORRELATION];
        let unrelated = read(&a, &b)[METER_CORRELATION];
        println!("correlation: mono {mono}, opposite {opposite}, unrelated {unrelated}");
        assert!(mono > 0.999 && opposite < -0.999 && unrelated.abs() < 0.05);

        let tone = sine(1000.0, 0.5, len, RATE);
        let meters = read(&tone, &vec![0.0; len]);
        assert!((meters[METER_PEAK_LEFT] - dsp::gain_to_db(0.5)).abs() < 0.01);
        assert!((meters[METER_RMS_LEFT] - dsp::gain_to_db(0.5 / 2f32.sqrt())).abs() < 0.01);
        assert_eq!(meters[METER_PEAK_RIGHT], METER_FLOOR_DB);
        assert_eq!(meters[METER_RMS_RIGHT], METER_FLOOR_DB);
        assert_eq!(meters[METER_CORRELATION], 0.0);
    }

    #[test]
    fn silence_stays_silent_and_garbage_passes() {
        let mut utility = utility(&[
            ("gain", 24.0),
            ("balance", 40.0),
            ("width", 180.0),
            ("bass_mono", 1.0),
            ("channels", MID as f32),
            ("swap", 1.0),
            ("invert_left", 1.0),
            ("dc", 1.0),
        ]);
        let (mut l, mut r) = (vec![0.0; 4800], vec![0.0; 4800]);
        run(&mut utility, &mut l, &mut r, 64, &Context::DEFAULT);
        assert!(l.iter().chain(&r).all(|&s| s == 0.0));
        // Garbage in: once it has gone, silence again, and finite meters.
        let mut garbage = [f32::NAN, f32::INFINITY, 0.5, f32::NEG_INFINITY];
        let mut other = garbage;
        utility.process(&mut garbage, &mut other, &Context::DEFAULT);
        let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
        utility.process(&mut l, &mut r, &Context::DEFAULT);
        assert!(l.iter().chain(&r).all(|&s| s == 0.0));
        let mut meters = [0.0; crate::MAX_METERS];
        utility.meters(&mut meters);
        assert!(meters.iter().all(|m| m.is_finite()));
    }
}
