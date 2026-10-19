//! The built-in wavetables, generated once per process in code.
//!
//! Every table is [`FRAME_COUNT`] single-cycle frames. Each frame is defined
//! by its harmonics (either directly, or by sampling a shape finely and
//! taking its FFT), then rendered at [`LEVELS`] band limits, one per octave:
//! level `l` keeps the harmonics up to `512 >> l`. An oscillator plays the
//! richest level whose top harmonic stays below Nyquist, so high notes don't
//! alias. Lower levels need fewer samples, so each level is four times its
//! harmonic count long (at least [`MIN_SIZE`]), which keeps linear
//! interpolation's images far below the harmonics while the whole bank fits
//! in about 25 MB.
//!
//! User tables ([`Table::from_cycles`]) are made the same way from sampled
//! single-cycle frames, each taken apart into its harmonics by FFT.
//!
//! Every table also keeps its frames' harmonics ([`Table::harmonics`]),
//! which the spectral morphs (`morph`) reshape and render again per voice.

use std::f64::consts::{PI, TAU};
use std::sync::LazyLock;

/// Frames per table, morphed between by the position knob.
pub const FRAME_COUNT: usize = 64;
/// Harmonics in the fullest level: enough for the bass, where 512 harmonics
/// of a 40 Hz note already reach 20 kHz.
pub const MAX_HARMONICS: usize = 512;
/// Band-limited versions of each frame, an octave apart.
pub const LEVELS: usize = 10;
/// Shortest stored cycle. Small levels hold only a few harmonics, and a
/// longer table makes linear interpolation more accurate for almost no memory.
pub const MIN_SIZE: usize = 512;
/// How finely shape-defined frames are sampled before the FFT: eight samples
/// per kept harmonic, so the discontinuities' own aliasing barely touches them.
const SHAPE_SIZE: usize = 4096;

/// The built-in tables, in the bank's order. The table menus list the
/// first [`FIRST_TABLES`] of them, then "User", then the rest: the menus
/// store their choice's place, and "User" was last when only those
/// existed (see `params::table_of`).
pub const TABLE_NAMES: [&str; 19] = [
    "Basic Shapes",
    "PWM",
    "Harmonics",
    "Analog Sweep",
    "Formant",
    "Sync",
    "Reso",
    "Growl",
    "Fold",
    "Digital",
    "Organ",
    "Chords",
    "Glass",
    "Grit",
    "FM Ratios",
    "Odd Even",
    "Pluck",
    "Hollow",
    "Fold Saw",
];
/// The built-in tables that came before "User" in the table menus.
pub const FIRST_TABLES: usize = 14;

/// How many harmonics level `level` keeps.
pub const fn level_harmonics(level: usize) -> usize {
    MAX_HARMONICS >> level
}

/// Samples per cycle at `level`.
pub const fn level_size(level: usize) -> usize {
    let size = 4 * level_harmonics(level);
    if size < MIN_SIZE { MIN_SIZE } else { size }
}

/// The richest level that keeps every harmonic of a wave at `hz` below
/// Nyquist. `hz` is the fastest the oscillator reads the table, so warps
/// that speed up part of the cycle pass a higher figure. Every unison
/// voice asks once per control tick, so `ceil(log2)` is read off the
/// float's exponent and mantissa rather than computed.
#[inline]
pub fn level_for(hz: f32, sample_rate: f32) -> usize {
    let ratio = hz * MAX_HARMONICS as f32 / (0.5 * sample_rate);
    if ratio <= 1.0 || ratio.is_nan() {
        return 0;
    }
    let bits = ratio.to_bits();
    let exponent = ((bits >> 23) & 0xff) as usize - 127;
    let above_power_of_two = bits & 0x7f_ffff != 0;
    (exponent + usize::from(above_power_of_two)).min(LEVELS - 1)
}

/// Most frames a user table keeps (Serum's and Vital's tables have up to
/// 256); longer files are thinned out evenly to this many.
pub const MAX_USER_FRAMES: usize = 256;

/// One band limit of every frame of a table.
pub struct Level {
    pub size: usize,
    /// `size - 1`; sizes are powers of two, so this wraps an index.
    pub mask: usize,
    /// Samples per stored frame: the cycle plus a copy of its first sample,
    /// so interpolation can read one past the end without wrapping.
    pub stride: usize,
    pub data: Vec<f32>,
}

impl Level {
    /// Frame `frame`'s samples, with the guard sample at the end.
    #[inline]
    pub fn frame(&self, frame: usize) -> &[f32] {
        &self.data[frame * self.stride..(frame + 1) * self.stride]
    }
}

pub struct Table {
    pub frames: usize,
    pub levels: Vec<Level>,
    /// Each frame's harmonics, scaled as its samples are:
    /// [`MAX_HARMONICS`] cosine amplitudes (harmonics 1 and up), then as
    /// many sine amplitudes, frame after frame (see [`Table::harmonics`]).
    spectra: Vec<f32>,
}

/// Linear interpolation into one stored cycle at `phase` (`0..1`; anything
/// else wraps). `frame` is a [`Level::frame`] slice.
#[inline(always)]
pub fn read(frame: &[f32], size: usize, mask: usize, phase: f32) -> f32 {
    let x = phase * size as f32;
    // Saturating cast: negative or NaN reads sample 0 instead of going out of range.
    let i = x as usize;
    let t = x - i as f32;
    let i = i & mask;
    let a = frame[i];
    let b = frame[i + 1];
    a + (b - a) * t
}

#[cfg(test)]
impl Table {
    /// The sample of a frame position `position` (`0..=1`, interpolating
    /// between neighbouring frames) at `phase`, from level `level`.
    pub fn sample(&self, level: usize, position: f32, phase: f32) -> f32 {
        let level = &self.levels[level.min(LEVELS - 1)];
        let x = position.clamp(0.0, 1.0) * (self.frames - 1) as f32;
        let i = (x as usize).min(self.frames - 1);
        let next = (i + 1).min(self.frames - 1);
        let t = x - i as f32;
        let a = read(level.frame(i), level.size, level.mask, phase);
        let b = read(level.frame(next), level.size, level.mask, phase);
        a + (b - a) * t
    }
}

/// Every table, and the sub oscillator's four shapes.
pub struct Bank {
    pub tables: Vec<Table>,
    /// Sine, triangle, saw and square, one frame each.
    pub sub: Table,
}

static BANK: LazyLock<Bank> = LazyLock::new(Bank::generate);

/// The shared tables. The first call generates them (a fraction of a
/// second), so instruments call this when they are built, never on the
/// audio thread.
pub fn bank() -> &'static Bank {
    &BANK
}

/// A frame's harmonics: `x(p) = Σ cos[k]·cos(2πkp) + sin[k]·sin(2πkp)`, for
/// `k` in `1..=MAX_HARMONICS` (index 0 is unused: DC is always removed).
struct Spectrum {
    cos: Vec<f64>,
    sin: Vec<f64>,
}

impl Spectrum {
    fn new() -> Self {
        Self {
            cos: vec![0.0; MAX_HARMONICS + 1],
            sin: vec![0.0; MAX_HARMONICS + 1],
        }
    }

    /// From a harmonic recipe: `(cos, sin)` amplitude of harmonic `k`.
    fn additive(harmonic: impl Fn(usize) -> (f64, f64)) -> Self {
        let mut spectrum = Self::new();
        for k in 1..=MAX_HARMONICS {
            let (c, s) = harmonic(k);
            spectrum.cos[k] = c;
            spectrum.sin[k] = s;
        }
        spectrum
    }

    /// From a shape over one cycle, `x` in `0..1`, by FFT.
    fn shape(shape: impl Fn(f64) -> f64) -> Self {
        let n = SHAPE_SIZE;
        let mut re: Vec<f64> = (0..n).map(|i| shape(i as f64 / n as f64)).collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im, false);
        let mut spectrum = Self::new();
        for k in 1..=MAX_HARMONICS {
            spectrum.cos[k] = 2.0 * re[k] / n as f64;
            spectrum.sin[k] = -2.0 * im[k] / n as f64;
        }
        spectrum
    }

    /// From one sampled cycle of any length: its harmonics up to
    /// [`MAX_HARMONICS`], or as many as `cycle.len()` samples hold below
    /// their Nyquist (whose own, phaseless bin is left out), by FFT when
    /// the length is a power of two and by DFT otherwise. Non-finite
    /// samples count as silence.
    fn cycle(cycle: &[f32]) -> Self {
        let n = cycle.len();
        let mut spectrum = Self::new();
        if n < 3 {
            return spectrum;
        }
        let top = MAX_HARMONICS.min((n - 1) / 2);
        let sample = |i: usize| {
            let x = cycle[i] as f64;
            if x.is_finite() { x } else { 0.0 }
        };
        let scale = 2.0 / n as f64;
        if n.is_power_of_two() {
            let mut re: Vec<f64> = (0..n).map(sample).collect();
            let mut im = vec![0.0; n];
            fft(&mut re, &mut im, false);
            for k in 1..=top {
                spectrum.cos[k] = re[k] * scale;
                spectrum.sin[k] = -im[k] * scale;
            }
        } else {
            let samples: Vec<f64> = (0..n).map(sample).collect();
            for k in 1..=top {
                // e^{-2πikn/N} by rotation, recomputed every so often so
                // rounding doesn't build up over long cycles.
                let step = -TAU * k as f64 / n as f64;
                let (w_im, w_re) = step.sin_cos();
                let (mut c_re, mut c_im) = (1.0, 0.0);
                let (mut re, mut im) = (0.0, 0.0);
                for (i, x) in samples.iter().enumerate() {
                    if i % 256 == 0 {
                        (c_im, c_re) = (step * i as f64).sin_cos();
                    }
                    re += x * c_re;
                    im += x * c_im;
                    let next = c_re * w_re - c_im * w_im;
                    c_im = c_re * w_im + c_im * w_re;
                    c_re = next;
                }
                spectrum.cos[k] = re * scale;
                spectrum.sin[k] = -im * scale;
            }
        }
        spectrum
    }

    /// One cycle of `size` samples with harmonics up to `harmonics`.
    fn render(&self, size: usize, harmonics: usize) -> Vec<f64> {
        let mut re = vec![0.0; size];
        let mut im = vec![0.0; size];
        for k in 1..=harmonics.min(size / 2 - 1) {
            // a·cos + b·sin = Re((a - ib)·e^{iθ}); split across k and size - k.
            re[k] = self.cos[k] / 2.0;
            im[k] = -self.sin[k] / 2.0;
            re[size - k] = self.cos[k] / 2.0;
            im[size - k] = self.sin[k] / 2.0;
        }
        fft(&mut re, &mut im, true);
        re
    }
}

/// In-place iterative radix-2 FFT; `inverse` flips the sign of the exponent
/// and doesn't scale. Only used while generating tables.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let angle = sign * TAU / len as f64;
        let (w_im, w_re) = angle.sin_cos();
        for start in (0..n).step_by(len) {
            let (mut c_re, mut c_im) = (1.0, 0.0);
            for k in 0..len / 2 {
                let a = start + k;
                let b = a + len / 2;
                let t_re = re[b] * c_re - im[b] * c_im;
                let t_im = re[b] * c_im + im[b] * c_re;
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                let next = c_re * w_re - c_im * w_im;
                c_im = c_re * w_im + c_im * w_re;
                c_re = next;
            }
        }
        len <<= 1;
    }
}

/// How [`Table::build_frames`] scales the frames.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Normalize {
    /// Every frame peaks at 1: positions differ in timbre, not loudness.
    EachFrame,
    /// The loudest frame peaks at 1, and the rest keep their level against
    /// it, as the file has them.
    Table,
}

impl Table {
    /// Renders every level of every frame, each frame scaled so its fullest
    /// level peaks at 1: positions differ in timbre, not loudness.
    fn build(frames: usize, spectrum: impl Fn(f64) -> Spectrum) -> Table {
        Self::build_frames(
            frames,
            |frame| {
                let t = if frames > 1 {
                    frame as f64 / (frames - 1) as f64
                } else {
                    0.0
                };
                spectrum(t)
            },
            Normalize::EachFrame,
        )
    }

    /// A table from sampled single cycles: `cycles` holds `frames` frames
    /// of `frame_size` samples each, one after the other, any number of
    /// either. Each frame is taken apart into its harmonics (up to
    /// [`MAX_HARMONICS`], and as many as its samples can hold), without
    /// DC, and rendered at every band limit like the built-in tables. A
    /// cycle of a length that isn't a power of two is analysed exactly
    /// (by DFT) rather than resampled. More than [`MAX_USER_FRAMES`]
    /// frames are thinned out evenly. The loudest frame peaks at 1, as
    /// the built-in frames do; `None` if every frame is silent.
    pub fn from_cycles(cycles: &[f32], frame_size: usize) -> Option<Table> {
        let frame_size = frame_size.max(1);
        let available = cycles.len() / frame_size;
        if available == 0 {
            return None;
        }
        let frames = available.min(MAX_USER_FRAMES);
        let source = |frame: usize| {
            let index = if frames == available {
                frame
            } else {
                // Evenly from the first to the last.
                (frame as f64 * (available - 1) as f64 / (frames - 1) as f64).round() as usize
            };
            &cycles[index * frame_size..(index + 1) * frame_size]
        };
        let table = Self::build_frames(frames, |f| Spectrum::cycle(source(f)), Normalize::Table);
        let silent = table.levels[0].data.iter().all(|x| *x == 0.0);
        (!silent).then_some(table)
    }

    /// The bytes its samples and harmonics take.
    pub fn memory_bytes(&self) -> usize {
        let samples: usize = self.levels.iter().map(|level| level.data.len()).sum();
        (samples + self.spectra.len()) * std::mem::size_of::<f32>()
    }

    /// Frame `frame`'s harmonics: the cosine and sine amplitudes of
    /// harmonics 1 to [`MAX_HARMONICS`] (index 0 is harmonic 1), so that
    /// the frame is `Σ cos[k-1]·cos(2πkp) + sin[k-1]·sin(2πkp)`.
    pub fn harmonics(&self, frame: usize) -> (&[f32], &[f32]) {
        let frame = frame.min(self.frames - 1);
        let start = frame * 2 * MAX_HARMONICS;
        let (cos, sin) = self.spectra[start..start + 2 * MAX_HARMONICS].split_at(MAX_HARMONICS);
        (cos, sin)
    }

    /// Renders every level of frame `0..frames`, frame `f`'s harmonics
    /// being `spectrum(f)`, scaled as `normalize` says.
    fn build_frames(
        frames: usize,
        spectrum: impl Fn(usize) -> Spectrum,
        normalize: Normalize,
    ) -> Table {
        let mut levels: Vec<Level> = (0..LEVELS)
            .map(|level| {
                let size = level_size(level);
                Level {
                    size,
                    mask: size - 1,
                    stride: size + 1,
                    data: Vec::with_capacity(frames * (size + 1)),
                }
            })
            .collect();
        let mut spectra = Vec::with_capacity(frames * 2 * MAX_HARMONICS);
        let mut loudest = 0.0f64;
        for frame in 0..frames {
            let spectrum = spectrum(frame);
            let full = spectrum.render(level_size(0), level_harmonics(0));
            let peak = full.iter().fold(0.0f64, |m, x| m.max(x.abs()));
            loudest = loudest.max(peak);
            let scale = match normalize {
                Normalize::EachFrame if peak > 1e-9 => 1.0 / peak,
                Normalize::EachFrame => 0.0,
                Normalize::Table => 1.0,
            };
            for (index, level) in levels.iter_mut().enumerate() {
                let cycle = if index == 0 {
                    full.clone()
                } else {
                    spectrum.render(level.size, level_harmonics(index))
                };
                level.data.extend(cycle.iter().map(|x| (x * scale) as f32));
                level.data.push((cycle[0] * scale) as f32);
            }
            let harmonics = 1..=MAX_HARMONICS;
            spectra.extend(harmonics.clone().map(|k| (spectrum.cos[k] * scale) as f32));
            spectra.extend(harmonics.map(|k| (spectrum.sin[k] * scale) as f32));
        }
        if normalize == Normalize::Table {
            // Below this the file is silence, give or take rounding.
            let scale = if loudest > 1e-6 { 1.0 / loudest } else { 0.0 };
            for level in &mut levels {
                for x in &mut level.data {
                    *x = (*x as f64 * scale) as f32;
                }
            }
            for x in &mut spectra {
                *x = (*x as f64 * scale) as f32;
            }
        }
        Table {
            frames,
            levels,
            spectra,
        }
    }
}

impl Bank {
    fn generate() -> Bank {
        let builders: [fn(f64) -> Spectrum; TABLE_NAMES.len()] = [
            basic_shapes,
            pwm,
            harmonic_sweep,
            analog_sweep,
            formant,
            sync,
            reso,
            growl,
            fold,
            digital,
            organ,
            chords,
            glass,
            grit,
            fm_ratios,
            odd_even,
            pluck,
            hollow,
            fold_saw,
        ];
        // Every table is its own job, so the bank takes as long as the
        // slowest table rather than all of them in a row: the first
        // Wavetable in a project (or the app's first frame with one) would
        // otherwise wait a fifth of a second for them.
        let tables = std::thread::scope(|scope| {
            let handles: Vec<_> = builders
                .iter()
                .map(|builder| scope.spawn(move || Table::build(FRAME_COUNT, builder)))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("a table builds"))
                .collect()
        });
        let sub = Table::build(4, |t| {
            let shape = (t * 3.0).round() as usize;
            Spectrum::additive(|k| match shape {
                0 => sine(k),
                1 => triangle(k),
                2 => saw(k),
                _ => square(k),
            })
        });
        Bank { tables, sub }
    }
}

// Harmonic recipes, all in sine phase so they start at zero and crossfade
// without cancelling.

fn sine(k: usize) -> (f64, f64) {
    (0.0, if k == 1 { 1.0 } else { 0.0 })
}

fn triangle(k: usize) -> (f64, f64) {
    if k.is_multiple_of(2) {
        return (0.0, 0.0);
    }
    let sign = if (k / 2).is_multiple_of(2) { 1.0 } else { -1.0 };
    (0.0, sign * 8.0 / (PI * PI * (k * k) as f64))
}

/// Rises from 0 to 1 over the first half cycle, jumps to -1, rises back to 0.
fn saw(k: usize) -> (f64, f64) {
    let sign = if k % 2 == 1 { 1.0 } else { -1.0 };
    (0.0, sign * 2.0 / (PI * k as f64))
}

fn square(k: usize) -> (f64, f64) {
    let amplitude = if k % 2 == 1 {
        4.0 / (PI * k as f64)
    } else {
        0.0
    };
    (0.0, amplitude)
}

fn lerp(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// Phases that spread a dense spectrum's energy over the cycle instead of
/// piling it into one spike (Schroeder's low-crest-factor phases), so
/// normalising to the peak doesn't leave such frames quiet.
fn spread_phase(k: usize, amplitude: f64) -> (f64, f64) {
    let phase = PI * (k * k) as f64 / 64.0;
    (amplitude * phase.sin(), amplitude * phase.cos())
}

/// A small integer hash, for repeatable "random" spectra.
fn hash(mut x: u32) -> f64 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    x as f64 / u32::MAX as f64
}

/// Sine to triangle to saw to square, in thirds.
fn basic_shapes(t: f64) -> Spectrum {
    let x = t * 3.0;
    Spectrum::additive(|k| match x {
        x if x < 1.0 => lerp(sine(k), triangle(k), x),
        x if x < 2.0 => lerp(triangle(k), saw(k), x - 1.0),
        x => lerp(saw(k), square(k), x - 2.0),
    })
}

/// A pulse narrowing from square (50%) to a 2% spike.
fn pwm(t: f64) -> Spectrum {
    let width = 0.5 - 0.48 * t;
    Spectrum::additive(|k| {
        let kw = TAU * k as f64 * width;
        let scale = 2.0 / (PI * k as f64);
        (scale * kw.sin(), scale * (1.0 - kw.cos()))
    })
}

/// The fundamental plus a bright band of harmonics sweeping from the 2nd up
/// to the 48th, widening as it rises: a vowel-like resonance moving up.
fn harmonic_sweep(t: f64) -> Spectrum {
    let center = 2.0 + t * 46.0;
    let width = 1.0 + t * 4.0;
    Spectrum::additive(|k| {
        let d = (k as f64 - center) / width;
        let bump = (-d * d).exp();
        let fundamental = if k == 1 { 0.7 } else { 0.0 };
        (0.0, fundamental + bump * 0.8)
    })
}

/// A saw through a resonant low-pass whose cutoff rises from the 1st
/// harmonic to the 512th: the classic analog filter sweep, frozen per frame.
fn analog_sweep(t: f64) -> Spectrum {
    let cutoff = (t * 9.0).exp2();
    let q = 3.0;
    Spectrum::additive(|k| {
        let r = k as f64 / cutoff;
        let gain = 1.0 / ((1.0 - r * r).powi(2) + (r / q).powi(2)).sqrt();
        let (_, s) = saw(k);
        (0.0, s * gain)
    })
}

/// Formant frequencies (Hz) of the vowels A, E, I, O, U.
const VOWELS: [[f64; 3]; 5] = [
    [730.0, 1090.0, 2440.0],
    [530.0, 1840.0, 2480.0],
    [270.0, 2290.0, 3010.0],
    [570.0, 840.0, 2410.0],
    [300.0, 870.0, 2240.0],
];
const FORMANT_WIDTHS: [f64; 3] = [90.0, 110.0, 160.0];
const FORMANT_GAINS: [f64; 3] = [1.0, 0.5, 0.25];

/// Vowels A, E, I, O, U in turn, as sung at 110 Hz: the formants stay at
/// fixed harmonics, so the vowel moves with the note like a classic vocal table.
fn formant(t: f64) -> Spectrum {
    let x = t * 4.0;
    let i = (x as usize).min(3);
    let f = x - i as f64;
    let formants: [f64; 3] =
        std::array::from_fn(|n| VOWELS[i][n] + (VOWELS[i + 1][n] - VOWELS[i][n]) * f);
    Spectrum::additive(|k| {
        let hz = k as f64 * 110.0;
        let amplitude: f64 = (0..3)
            .map(|n| {
                let d = (hz - formants[n]) / FORMANT_WIDTHS[n];
                FORMANT_GAINS[n] / (1.0 + d * d)
            })
            .sum::<f64>()
            / (k as f64).powf(0.3);
        spread_phase(k, amplitude)
    })
}

/// A saw hard-synced to the fundamental, its own pitch rising from unison to
/// three octaves up.
fn sync(t: f64) -> Spectrum {
    let ratio = 1.0 + t * 7.0;
    Spectrum::shape(|x| 2.0 * (x * ratio).fract() - 1.0)
}

/// Phase-distortion resonance (Casio CZ style): a sine at a rising multiple
/// of the note, faded out across each cycle so the wave stays continuous.
fn reso(t: f64) -> Spectrum {
    let ratio = 1.0 + t * 15.0;
    Spectrum::shape(|x| (1.0 - x) * (TAU * ratio * x).sin())
}

/// Stacked phase modulation with a rising index: from a sine to a snarling,
/// vowel-ish growl.
fn growl(t: f64) -> Spectrum {
    let index = t * 4.0;
    Spectrum::shape(|x| {
        let inner = (TAU * x).sin() * 0.7 * index;
        (TAU * x + index * (TAU * 2.0 * x).sin() + 0.5 * index * (TAU * 3.0 * x + inner).sin())
            .sin()
    })
}

/// A sine through a wavefolder driven ever harder.
fn fold(t: f64) -> Spectrum {
    let drive = (0.5 + t * 5.5) * PI;
    Spectrum::shape(|x| (drive * (TAU * x).sin()).sin())
}

/// A sine-to-saw shape sampled and held at ever fewer steps, and with ever
/// fewer levels: lo-fi, aliased-on-purpose digital tones.
fn digital(t: f64) -> Spectrum {
    let steps = (64.0 / (1.0 + t * 15.0)).round().max(3.0);
    let levels = (16.0 - t * 13.0).round().max(2.0);
    Spectrum::shape(|x| {
        let held = (x * steps).floor() / steps;
        let sine = (TAU * held).sin();
        let ramp = 2.0 * ((held + 0.5).fract()) - 1.0;
        let wave = sine + (ramp - sine) * t;
        (wave * levels).round() / levels
    })
}

/// Harmonics of a tonewheel organ's nine drawbars (16' is the fundamental).
const DRAWBAR_HARMONICS: [usize; 9] = [1, 2, 3, 4, 6, 8, 10, 12, 16];
/// Registrations the table moves through, drawbar settings 0..8.
const REGISTRATIONS: [[u8; 9]; 6] = [
    [8, 0, 0, 0, 0, 0, 0, 0, 0],
    [8, 8, 8, 0, 0, 0, 0, 0, 0],
    [8, 8, 8, 8, 0, 0, 0, 0, 0],
    [8, 6, 8, 8, 0, 0, 0, 0, 8],
    [8, 0, 0, 0, 0, 0, 8, 8, 8],
    [8, 8, 8, 8, 8, 8, 8, 8, 8],
];

/// Organ drawbar registrations, from a lone flute to all bars out.
fn organ(t: f64) -> Spectrum {
    let x = t * (REGISTRATIONS.len() - 1) as f64;
    let i = (x as usize).min(REGISTRATIONS.len() - 2);
    let f = x - i as f64;
    // Each drawbar step is 3 dB; 0 is off.
    let level = |setting: u8| {
        if setting == 0 {
            0.0
        } else {
            10f64.powf(-3.0 * (8.0 - setting as f64) / 20.0)
        }
    };
    Spectrum::additive(|k| {
        let amplitude: f64 = DRAWBAR_HARMONICS
            .iter()
            .enumerate()
            .filter(|(_, h)| **h == k)
            .map(|(bar, _)| {
                let a = level(REGISTRATIONS[i][bar]);
                let b = level(REGISTRATIONS[i + 1][bar]);
                a + (b - a) * f
            })
            .sum();
        (0.0, amplitude)
    })
}

/// Chords built from harmonics over the note: octaves, major, dominant 7th,
/// major 7th, sus2 and minor shapes, morphing one into the next.
const CHORDS: [&[usize]; 6] = [
    &[2, 4, 8],
    &[4, 5, 6, 8],
    &[4, 5, 6, 7],
    &[8, 10, 12, 15],
    &[8, 9, 12, 16],
    &[10, 12, 15, 20],
];

fn chords(t: f64) -> Spectrum {
    let x = t * (CHORDS.len() - 1) as f64;
    let i = (x as usize).min(CHORDS.len() - 2);
    let f = x - i as f64;
    // Each chord tone, plus a quieter copy an octave up so it isn't hollow.
    let weight = |chord: &[usize], k: usize| {
        if chord.contains(&k) {
            1.0
        } else if k.is_multiple_of(2) && chord.contains(&(k / 2)) {
            0.3
        } else {
            0.0
        }
    };
    Spectrum::additive(|k| {
        let bass = match k {
            1 => 0.6,
            2 => 0.3,
            _ => 0.0,
        };
        let chord = weight(CHORDS[i], k) * (1.0 - f) + weight(CHORDS[i + 1], k) * f;
        spread_phase(k, bass + chord * 0.7)
    })
}

/// Sparse partials on square-numbered harmonics (1, 4, 9, 16...), a bell-
/// or glass-like spacing; the weight slides from the low partials to the high.
fn glass(t: f64) -> Spectrum {
    let center = t * 7.0;
    Spectrum::additive(|k| {
        let root = (k as f64).sqrt();
        if root.fract() != 0.0 {
            return (0.0, 0.0);
        }
        let i = root - 1.0;
        let d = (i - center) / 1.5;
        let fundamental = if k == 1 { 0.5 } else { 0.0 };
        (0.0, fundamental + (-d * d).exp() / (1.0 + i * 0.15))
    })
}

/// A fixed random spectrum whose tilt goes from dark to bright and buzzy.
fn grit(t: f64) -> Spectrum {
    let tilt = 2.0 - 1.5 * t;
    Spectrum::additive(|k| {
        let amplitude = (0.2 + 0.8 * hash(k as u32)) / (k as f64).powf(tilt);
        let phase = TAU * hash(k as u32 ^ 0x5555_5555);
        (amplitude * phase.sin(), amplitude * phase.cos())
    })
}

/// Two-operator FM: a sine whose phase a sine at a whole multiple of the
/// note moves, the multiple rising from 1 to 8 across the table, and within
/// each the depth swelling: bells, brass and metallic tones in one table.
fn fm_ratios(t: f64) -> Spectrum {
    let place = (t * 7.999).min(7.999);
    let ratio = 1.0 + place.floor();
    let index = 0.6 + 1.6 * (place - place.floor());
    Spectrum::shape(|x| (TAU * x + index * (TAU * ratio * x).sin()).sin())
}

/// Odd harmonics (a square's) fading into even ones (an octave-up saw's
/// over the fundamental): hollow to bright and nasal.
fn odd_even(t: f64) -> Spectrum {
    Spectrum::additive(|k| {
        let weight = match k {
            1 => 1.0,
            k if k % 2 == 1 => 1.0 - t,
            _ => t,
        };
        (0.0, weight / (k as f64).powf(0.9))
    })
}

/// A plucked string's harmonics dying away, the high ones first: frame 0
/// is the bright pluck, the last almost a sine. Swept by an envelope, a
/// pluck from one oscillator.
fn pluck(t: f64) -> Spectrum {
    Spectrum::additive(|k| {
        let k = k as f64;
        let decay = (-(k - 1.0) * t * 0.3).exp();
        // The pluck's place on the string shapes the harmonics a little.
        let place = 1.0 + 0.35 * (k * 1.3).sin();
        (0.0, place * decay / k)
    })
}

/// A saw losing its low harmonics to a rising high-pass (the fundamental
/// kept faintly, so the note stays): from full to thin and airy.
fn hollow(t: f64) -> Spectrum {
    let cutoff = 1.0 + t * 24.0;
    Spectrum::additive(|k| {
        let r = cutoff / k as f64;
        let gain = 1.0 / (1.0 + r.powi(4));
        let gain = if k == 1 { gain.max(0.15) } else { gain };
        let (_, s) = saw(k);
        (0.0, s * gain)
    })
}

/// A saw through a wavefolder driven ever harder.
fn fold_saw(t: f64) -> Spectrum {
    let drive = 1.0 + t * 5.0;
    Spectrum::shape(|x| {
        let saw = 2.0 * (x + 0.5).fract() - 1.0;
        // A triangle fold: unchanged inside ±1, reflected past it.
        let v = saw * drive - 1.0;
        let v = v - 4.0 * (v / 4.0).floor();
        (v - 2.0).abs() - 1.0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_round_trips() {
        let n = 64;
        let original: Vec<f64> = (0..n).map(|i| ((i * 7) % 13) as f64 - 6.0).collect();
        let mut re = original.clone();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im, false);
        fft(&mut re, &mut im, true);
        for (a, b) in original.iter().zip(&re) {
            assert!((a - b / n as f64).abs() < 1e-9);
        }
    }

    #[test]
    fn levels_are_powers_of_two_and_fit_their_harmonics() {
        for level in 0..LEVELS {
            let size = level_size(level);
            assert!(size.is_power_of_two());
            assert!(size >= 4 * level_harmonics(level));
        }
        assert_eq!(level_harmonics(LEVELS - 1), 1);
    }

    #[test]
    fn level_choice_keeps_harmonics_below_nyquist() {
        let rate = 48_000.0;
        for i in 0..4000 {
            let hz = 20.0 * 1.002f32.powi(i);
            let ratio = hz * MAX_HARMONICS as f32 / (0.5 * rate);
            let exact = if ratio <= 1.0 {
                0
            } else {
                (ratio.log2().ceil() as usize).min(LEVELS - 1)
            };
            assert_eq!(level_for(hz, rate), exact, "{hz} Hz");
        }
        assert_eq!(level_for(f32::NAN, rate), 0);
        assert_eq!(level_for(f32::INFINITY, rate), LEVELS - 1);
        for hz in [20.0, 47.0, 100.0, 440.0, 1000.0, 5000.0, 12_000.0] {
            let level = level_for(hz, rate);
            assert!(
                level == LEVELS - 1 || level_harmonics(level) as f32 * hz <= rate / 2.0,
                "{hz} Hz -> level {level}"
            );
            if level > 0 {
                assert!(
                    level_harmonics(level - 1) as f32 * hz > rate / 2.0,
                    "{hz} Hz could use a richer level than {level}"
                );
            }
        }
    }

    #[test]
    fn sampled_cycles_keep_their_shape_without_dc_and_with_their_levels() {
        // A saw with an offset, and the same at half the level. Sampled
        // with its jump's midpoint on a sample, as a Fourier series has it.
        let saw = |n: usize, gain: f32| -> Vec<f32> {
            (0..n)
                .map(|i| {
                    let p = i as f32 / n as f32;
                    let x = if i == n / 2 {
                        0.0
                    } else {
                        2.0 * (p + 0.5).fract() - 1.0
                    };
                    gain * x + 0.3
                })
                .collect()
        };
        let cycles: Vec<f32> = saw(2048, 1.0).into_iter().chain(saw(2048, 0.5)).collect();
        let table = Table::from_cycles(&cycles, 2048).unwrap();
        assert_eq!(table.frames, 2);
        let level = &table.levels[0];
        let peak = |frame: usize| {
            level
                .frame(frame)
                .iter()
                .fold(0.0f32, |m, x| m.max(x.abs()))
        };
        assert!((0.99..=1.01).contains(&peak(0)), "{}", peak(0));
        assert!((peak(1) / peak(0) - 0.5).abs() < 0.01, "keeps its level");
        for frame in 0..2 {
            let cycle = &level.frame(frame)[..level.size];
            let mean = cycle.iter().sum::<f32>() / cycle.len() as f32;
            assert!(mean.abs() < 1e-4, "frame {frame} has DC {mean}");
        }
        // The built-in saw, Basic Shapes at 2/3, is the same wave, but for
        // the top harmonics, which sampling leaves a little weaker.
        let builtin = &bank().tables[0];
        let (mut worst, mut squares) = (0.0f32, 0.0f32);
        for i in 0..1000 {
            let phase = i as f32 / 1000.0;
            let error = table.sample(0, 0.0, phase) - builtin.sample(0, 2.0 / 3.0, phase);
            worst = worst.max(error.abs());
            squares += error * error;
        }
        let rms = (squares / 1000.0).sqrt();
        println!("sampled saw against the built-in one: rms {rms:.4}, worst {worst:.4}");
        assert!(rms < 0.03 && worst < 0.1, "rms {rms}, worst {worst}");

        // A cycle whose length isn't a power of two (by DFT) matches the
        // same wave sampled at a power of two (by FFT).
        let wave = |n: usize| -> Vec<f32> {
            (0..n)
                .map(|i| {
                    let p = std::f64::consts::TAU * i as f64 / n as f64;
                    (p.sin() + 0.5 * (3.0 * p).cos() - 0.25 * (7.0 * p).sin()) as f32
                })
                .collect()
        };
        let odd = Table::from_cycles(&wave(1500), 1500).unwrap();
        let even = Table::from_cycles(&wave(1024), 1024).unwrap();
        for i in 0..200 {
            let phase = i as f32 / 200.0;
            let (a, b) = (odd.sample(0, 0.0, phase), even.sample(0, 0.0, phase));
            assert!((a - b).abs() < 1e-3, "{phase}: {a} vs {b}");
        }
        // A short cycle keeps only the harmonics it can hold.
        let tiny = Table::from_cycles(&wave(16), 16).unwrap();
        assert!((tiny.sample(0, 0.0, 0.3) - even.sample(0, 0.0, 0.3)).abs() < 1e-3);
        assert!(Table::from_cycles(&[0.0; 64], 64).is_none());
        assert!(Table::from_cycles(&[1.0; 10], 64).is_none());
    }

    #[test]
    fn basic_shapes_start_as_a_sine_and_every_frame_peaks_near_one() {
        let bank = bank();
        assert_eq!(bank.tables.len(), TABLE_NAMES.len());
        let basic = &bank.tables[0];
        for i in 0..64 {
            let phase = i as f32 / 64.0;
            let expected = (std::f32::consts::TAU * phase).sin();
            assert!((basic.sample(0, 0.0, phase) - expected).abs() < 1e-3);
        }
        for (name, table) in TABLE_NAMES.iter().zip(&bank.tables) {
            for frame in 0..table.frames {
                let cycle = table.levels[0].frame(frame);
                let peak = cycle.iter().fold(0.0f32, |m, x| m.max(x.abs()));
                assert!(
                    (0.99..=1.01).contains(&peak),
                    "{name} frame {frame}: {peak}"
                );
                let mean = cycle[..cycle.len() - 1].iter().sum::<f32>() / (cycle.len() - 1) as f32;
                assert!(mean.abs() < 1e-3, "{name} frame {frame} has DC {mean}");
            }
        }
    }
}
