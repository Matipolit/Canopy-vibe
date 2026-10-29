//! Spectral morphs, after Vital's: each reshapes the current frame's
//! harmonics (stretching them, scaling their envelope, filtering, smearing
//! or dispersing them) before the frame is rendered back into one cycle.
//! Working on harmonics keeps the result band-limited however far a morph
//! goes, which bending the waveform itself could not.
//!
//! Each voice renders its oscillator's morphed frame at the band limit its
//! unison voices need (the plainest of theirs, so none aliases), with an
//! inverse FFT, whenever the morph, the position or the band limit has
//! moved, at most every [`INTERVAL`] control ticks. The voices read the
//! newest frame crossfaded from the one before over that interval
//! ([`super::osc::Frames::Morphed`]), so a modulated morph moves smoothly
//! and a still one costs nothing after its first frame.

use std::f32::consts::{PI, TAU};
use std::sync::LazyLock;

use super::tables::{Level, MAX_HARMONICS, Table, level_harmonics, level_size};

pub const MORPH_OFF: usize = 0;
/// Scales the frame's spectral envelope (its formants) as Formant Scale
/// does, but pinned to frequency rather than to the note: the formants
/// stay where they are in Hz as the note moves, as a vocoder's would.
pub const MORPH_VOCODE: usize = 1;
/// Scales the spectral envelope from two octaves down (amount 0) to two
/// up (1), keeping the harmonics in place: 0.5 leaves it alone.
pub const MORPH_FORMANT_SCALE: usize = 2;
/// Spreads the harmonics apart (keeping the fundamental), up to four
/// times their spacing, each landing between the nearest two.
pub const MORPH_HARMONIC_STRETCH: usize = 3;
/// Moves each harmonic to `k^(1 + amount/2)`, the higher ever further:
/// a metallic, bell-like spread.
pub const MORPH_INHARMONIC_STRETCH: usize = 4;
/// Smears each harmonic into the ones above it, brightening and blurring.
pub const MORPH_SMEAR: usize = 5;
/// Scales each harmonic by a random amount, a new set as the amount moves.
pub const MORPH_RANDOM_AMPLITUDES: usize = 6;
/// Removes the harmonics above a cutoff that falls from the top to the
/// fundamental as the amount rises.
pub const MORPH_LOW_PASS: usize = 7;
/// Removes the harmonics below a cutoff that rises from the fundamental.
pub const MORPH_HIGH_PASS: usize = 8;
/// Delays the harmonics by ever more the higher they are: the same
/// spectrum, a different (swirling, when moving) wave.
pub const MORPH_PHASE_DISPERSE: usize = 9;
/// Crossfades the frame into itself an octave up: swept from 0 to 1 and
/// round again, it rises forever, as a Shepard tone does.
pub const MORPH_SHEPARD: usize = 10;
/// Reads each harmonic from a different place in the table, the higher
/// ones further along (Vital's spectral time skew).
pub const MORPH_SKEW: usize = 11;
pub const MORPH_COUNT: usize = 12;

/// Control ticks between new frames, at most: each new one costs an
/// inverse FFT per voice, and the crossfade between them hides the steps.
pub const INTERVAL: u32 = 2;
/// The note Vocode leaves the formants alone at: C4.
const VOCODE_REFERENCE_HZ: f32 = 261.63;
/// Random Amplitudes' sets of random gains, which the amount moves through.
const RANDOM_SETS: usize = 8;
/// The largest frame rendered: a level-0 cycle.
const MAX_SIZE: usize = level_size(0);

/// Whether morph `mode` at `amount` leaves the frame as it is, so the
/// oscillator can read the table directly instead.
pub fn is_identity(mode: usize, amount: f32) -> bool {
    match mode {
        MORPH_OFF => true,
        MORPH_FORMANT_SCALE => (amount - 0.5).abs() < 1e-4,
        MORPH_VOCODE => false,
        _ => amount <= 1e-4,
    }
}

/// Twiddle factors for the largest transform (`e^{2πik/N}`, `k < N/2`)
/// and bit-reversal tables for every size used. Built once, before any
/// voice runs (see [`prepare`]).
struct Fft {
    cos: Vec<f32>,
    sin: Vec<f32>,
    /// Bit-reversed index for each size, by `log2(size)`.
    reverse: Vec<Vec<u16>>,
}

static FFT: LazyLock<Fft> = LazyLock::new(|| {
    let n = MAX_SIZE;
    let (cos, sin) = (0..n / 2)
        .map(|k| {
            let angle = TAU * k as f32 / n as f32;
            (angle.cos(), angle.sin())
        })
        .unzip();
    let reverse = (0..=n.trailing_zeros())
        .map(|bits| {
            let size = 1usize << bits;
            (0..size)
                .map(|i| {
                    if bits == 0 {
                        0
                    } else {
                        (i.reverse_bits() >> (usize::BITS - bits)) as u16
                    }
                })
                .collect()
        })
        .collect();
    Fft { cos, sin, reverse }
});

/// Builds the transform's tables, which the first use would otherwise do
/// on the audio thread. Call when the synth is made.
pub fn prepare() {
    LazyLock::force(&FFT);
    LazyLock::force(&RANDOM_GAINS);
}

/// The inverse transform of `re` + i·`im` in place (`e^{+2πikn/N}`, not
/// scaled), for sizes that are powers of two up to [`MAX_SIZE`].
fn inverse_fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let fft = &*FFT;
    let reverse = &fft.reverse[n.trailing_zeros() as usize];
    for (i, &j) in reverse.iter().enumerate().take(n) {
        let j = usize::from(j);
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        let stride = MAX_SIZE / len;
        for start in (0..n).step_by(len) {
            for k in 0..half {
                let (w_re, w_im) = (fft.cos[k * stride], fft.sin[k * stride]);
                let (a, b) = (start + k, start + k + half);
                let t_re = re[b] * w_re - im[b] * w_im;
                let t_im = re[b] * w_im + im[b] * w_re;
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
            }
        }
        len <<= 1;
    }
}

/// Random gains for Random Amplitudes, `0..=2`, set by set.
static RANDOM_GAINS: LazyLock<Vec<f32>> = LazyLock::new(|| {
    (0..(RANDOM_SETS + 1) * MAX_HARMONICS)
        .map(|i| {
            let mut x = (i as u32).wrapping_mul(0x9E37_79B9) ^ 0x5BD1_E995;
            x ^= x >> 15;
            x = x.wrapping_mul(0x2C1B_3C6D);
            x ^= x >> 12;
            x = x.wrapping_mul(0x297A_2D39);
            x ^= x >> 15;
            2.0 * (x >> 8) as f32 / (1 << 24) as f32
        })
        .collect()
});

/// Working space for rendering morphed frames, shared by every voice.
pub struct Scratch {
    /// The frame's harmonics before and after the morph, harmonic 1 at 0.
    cos: [f32; MAX_HARMONICS],
    sin: [f32; MAX_HARMONICS],
    out_cos: [f32; MAX_HARMONICS],
    out_sin: [f32; MAX_HARMONICS],
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Default for Scratch {
    fn default() -> Self {
        Self {
            cos: [0.0; MAX_HARMONICS],
            sin: [0.0; MAX_HARMONICS],
            out_cos: [0.0; MAX_HARMONICS],
            out_sin: [0.0; MAX_HARMONICS],
            re: vec![0.0; MAX_SIZE],
            im: vec![0.0; MAX_SIZE],
        }
    }
}

/// What a morphed frame was made from, so an unchanged one isn't redone.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Key {
    mode: usize,
    amount: f32,
    /// Position in frames.
    frame: f32,
    level: usize,
    /// Vocode's note, in Hz.
    hz: f32,
    /// The table, by address: a new user table is a new frame.
    table: usize,
}

/// One oscillator's morphed frames within a voice.
pub struct MorphState {
    /// Two frames at one band limit: the one before and the newest.
    level: Level,
    key: Option<Key>,
    /// Where the crossfade from the older frame to the newest is, `0..=1`,
    /// and how far it moves per sample.
    t: f32,
    t_step: f32,
    /// Control ticks since the newest frame was made.
    age: u32,
}

impl Default for MorphState {
    fn default() -> Self {
        Self {
            level: Level {
                size: MAX_SIZE,
                mask: MAX_SIZE - 1,
                stride: MAX_SIZE + 1,
                data: vec![0.0; 2 * (MAX_SIZE + 1)],
            },
            key: None,
            t: 1.0,
            t_step: 0.0,
            age: 0,
        }
    }
}

impl MorphState {
    /// Forgets the frames, so the next tick makes a fresh one at once.
    pub fn reset(&mut self) {
        self.key = None;
        self.t = 1.0;
        self.t_step = 0.0;
    }

    /// Whether a morphed frame is ready to play: the oscillator reads the
    /// table itself until one is (and whenever the morph is off).
    pub fn is_ready(&self) -> bool {
        self.key.is_some()
    }

    /// The two frames and where the crossfade between them is, for this
    /// block (see [`super::osc::Frames::Morphed`]).
    pub fn frames(&self) -> (&Level, f32, f32) {
        (&self.level, self.t, self.t_step)
    }

    /// Moves the crossfade on by `samples` (one block, as rendered).
    pub fn advance(&mut self, samples: usize) {
        self.t = (self.t + self.t_step * samples as f32).min(1.0);
    }

    /// Once per control tick: makes a new frame if what it is made from
    /// has moved (and the last is at least [`INTERVAL`] ticks old), else
    /// lets the crossfade finish. `position` is `0..=1` through `table`;
    /// `level` the band limit to render at; `hz` the note's frequency;
    /// `steps` the samples until the next tick (twice a control interval
    /// when rendering at twice the rate). `immediate`: a note is starting,
    /// so the new frame plays at once.
    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        scratch: &mut Scratch,
        table: &Table,
        mode: usize,
        amount: f32,
        position: f32,
        level: usize,
        hz: f32,
        steps: usize,
        immediate: bool,
    ) {
        self.age = self.age.saturating_add(1);
        let key = Key {
            mode,
            amount,
            frame: position.clamp(0.0, 1.0) * (table.frames - 1) as f32,
            level,
            hz: if mode == MORPH_VOCODE { hz } else { 0.0 },
            table: table as *const Table as usize,
        };
        let moved = match self.key {
            None => true,
            Some(old) => {
                old.mode != key.mode
                    || old.level != key.level
                    || old.table != key.table
                    || (old.amount - key.amount).abs() > 1e-4
                    || (old.frame - key.frame).abs() > 1e-3
                    || (old.hz - key.hz).abs() > 0.01 * old.hz.max(1.0)
            }
        };
        if !moved || (!immediate && self.age < INTERVAL && self.key.is_some()) {
            self.t_step = (1.0 - self.t).max(0.0) / steps.max(1) as f32;
            return;
        }
        // A new band limit changes the frame's length: no crossfade then.
        let fade = !immediate && self.key.is_some_and(|old| old.level == key.level);
        let size = level_size(level);
        self.level.size = size;
        self.level.mask = size - 1;
        self.level.stride = size + 1;
        if fade {
            // The newest frame becomes the one before.
            let stride = self.level.stride;
            self.level.data.copy_within(stride..2 * stride, 0);
        }
        render(scratch, table, &key, &mut self.level);
        if !fade {
            let stride = self.level.stride;
            self.level.data.copy_within(stride..2 * stride, 0);
        }
        self.key = Some(key);
        self.age = 0;
        if fade {
            self.t = 0.0;
            self.t_step = 1.0 / (steps.max(1) as f32 * INTERVAL as f32);
        } else {
            self.t = 1.0;
            self.t_step = 0.0;
        }
    }
}

/// Renders the morphed frame `key` describes into frame 1 of `level`.
fn render(scratch: &mut Scratch, table: &Table, key: &Key, level: &mut Level) {
    let harmonics = level_harmonics(key.level).min(MAX_HARMONICS);
    gather(scratch, table, key, harmonics);
    morph(scratch, key, harmonics);
    // Back to one cycle: a·cos + b·sin = Re((a - ib)·e^{iθ}), split
    // between bins k and N - k.
    let size = level.size;
    let (re, im) = (&mut scratch.re[..size], &mut scratch.im[..size]);
    re.fill(0.0);
    im.fill(0.0);
    for k in 1..=harmonics.min(size / 2 - 1) {
        let (a, b) = (scratch.out_cos[k - 1] * 0.5, scratch.out_sin[k - 1] * 0.5);
        re[k] = a;
        im[k] = -b;
        re[size - k] = a;
        im[size - k] = b;
    }
    inverse_fft(re, im);
    let stride = level.stride;
    let frame = &mut level.data[stride..2 * stride];
    frame[..size].copy_from_slice(re);
    frame[size] = re[0];
}

/// The frame's harmonics at the key's position, interpolated between the
/// two nearest frames (Skew reads each harmonic at a place of its own).
fn gather(scratch: &mut Scratch, table: &Table, key: &Key, harmonics: usize) {
    let last = (table.frames - 1) as f32;
    // Stretches and scales reach down to lower harmonics only, so the
    // band-limited count is all any morph needs.
    for k in 1..=harmonics {
        let frame = if key.mode == MORPH_SKEW {
            // Higher harmonics read further on, folding back at the ends.
            let reach = key.amount * 2.0 * (k - 1) as f32 / harmonics.max(1) as f32;
            let place = key.frame / last.max(1.0) + reach;
            let folded = 1.0 - ((place.rem_euclid(2.0)) - 1.0).abs();
            folded * last
        } else {
            key.frame
        };
        let i = (frame as usize).min(table.frames - 1);
        let t = frame - i as f32;
        let j = (i + 1).min(table.frames - 1);
        let (cos_i, sin_i) = table.harmonics(i);
        let (cos_j, sin_j) = table.harmonics(j);
        scratch.cos[k - 1] = cos_i[k - 1] + (cos_j[k - 1] - cos_i[k - 1]) * t;
        scratch.sin[k - 1] = sin_i[k - 1] + (sin_j[k - 1] - sin_i[k - 1]) * t;
    }
}

/// The harmonics' total power, for keeping a reshaping morph's level.
fn power(cos: &[f32], sin: &[f32]) -> f32 {
    cos.iter().zip(sin).map(|(c, s)| c * c + s * s).sum()
}

/// Adds harmonic `(c, s)` at fractional harmonic number `at` into the
/// output, split between the two nearest whole harmonics.
#[inline]
fn deposit(out_cos: &mut [f32], out_sin: &mut [f32], at: f32, c: f32, s: f32) {
    let k = at as usize;
    let t = at - k as f32;
    let n = out_cos.len();
    if k >= 1 && k <= n {
        out_cos[k - 1] += c * (1.0 - t);
        out_sin[k - 1] += s * (1.0 - t);
    }
    if k < n && t > 0.0 {
        out_cos[k] += c * t;
        out_sin[k] += s * t;
    }
}

/// The spectral envelope (the frame's harmonics read at fractional
/// harmonic number `x`, interpolated in level and phase).
#[inline]
fn envelope_at(cos: &[f32], sin: &[f32], x: f32) -> (f32, f32) {
    if x < 1.0 {
        return (0.0, 0.0);
    }
    let k = x as usize;
    let t = x - k as f32;
    let at = |k: usize| {
        if k >= 1 && k <= cos.len() {
            (cos[k - 1], sin[k - 1])
        } else {
            (0.0, 0.0)
        }
    };
    let (c0, s0) = at(k);
    let (c1, s1) = at(k + 1);
    (c0 + (c1 - c0) * t, s0 + (s1 - s0) * t)
}

/// Applies the key's morph to the gathered harmonics, into `out_*`.
fn morph(scratch: &mut Scratch, key: &Key, harmonics: usize) {
    let h = harmonics;
    let Scratch {
        cos,
        sin,
        out_cos,
        out_sin,
        ..
    } = scratch;
    let (cos, sin) = (&cos[..h], &sin[..h]);
    let (out_cos, out_sin) = (&mut out_cos[..h], &mut out_sin[..h]);
    let m = key.amount.clamp(0.0, 1.0);
    let mut keep_power = true;
    match key.mode {
        MORPH_VOCODE | MORPH_FORMANT_SCALE => {
            // Harmonic k takes the envelope's level at k / ratio, keeping
            // its own phase relation: the formants move, the pitch doesn't.
            let mut ratio = (4.0 * m - 2.0).exp2();
            if key.mode == MORPH_VOCODE {
                ratio *= VOCODE_REFERENCE_HZ / key.hz.max(1.0);
            }
            for k in 1..=h {
                let (c, s) = envelope_at(cos, sin, k as f32 / ratio);
                out_cos[k - 1] = c;
                out_sin[k - 1] = s;
            }
        }
        MORPH_HARMONIC_STRETCH => {
            out_cos.fill(0.0);
            out_sin.fill(0.0);
            let spacing = (2.0 * m).exp2();
            for k in 1..=h {
                let at = 1.0 + (k - 1) as f32 * spacing;
                deposit(out_cos, out_sin, at, cos[k - 1], sin[k - 1]);
            }
        }
        MORPH_INHARMONIC_STRETCH => {
            out_cos.fill(0.0);
            out_sin.fill(0.0);
            let power = 1.0 + 0.5 * m;
            for k in 1..=h {
                let at = (k as f32).powf(power);
                deposit(out_cos, out_sin, at, cos[k - 1], sin[k - 1]);
            }
        }
        MORPH_SMEAR => {
            // Each harmonic gathers a share of the smeared one below it,
            // falling off as a saw's harmonics do.
            let carry = 0.97 * m;
            let (mut c_prev, mut s_prev) = (0.0f32, 0.0f32);
            for k in 1..=h {
                let fall = (k - 1) as f32 / k as f32;
                let c = cos[k - 1] + carry * c_prev * fall;
                let s = sin[k - 1] + carry * s_prev * fall;
                out_cos[k - 1] = c;
                out_sin[k - 1] = s;
                (c_prev, s_prev) = (c, s);
            }
        }
        MORPH_RANDOM_AMPLITUDES => {
            let gains = &*RANDOM_GAINS;
            let place = m * RANDOM_SETS as f32;
            let set = (place as usize).min(RANDOM_SETS - 1);
            let t = place - set as f32;
            let depth = (4.0 * m).min(1.0);
            for k in 1..=h {
                let a = gains[set * MAX_HARMONICS + k - 1];
                let b = gains[(set + 1) * MAX_HARMONICS + k - 1];
                let gain = 1.0 + (a + (b - a) * t - 1.0) * depth;
                out_cos[k - 1] = cos[k - 1] * gain;
                out_sin[k - 1] = sin[k - 1] * gain;
            }
        }
        MORPH_LOW_PASS | MORPH_HIGH_PASS => {
            keep_power = false;
            // The cutoff moves in octaves over the harmonics there are;
            // the harmonic it sits in passes in part, so sweeps are smooth.
            let top = h.max(1) as f32;
            for k in 1..=h {
                let x = k as f32;
                let gain = if key.mode == MORPH_LOW_PASS {
                    (top.powf(1.0 - m) - x + 1.0).clamp(0.0, 1.0)
                } else {
                    (x - top.powf(m) + 1.0).clamp(0.0, 1.0)
                };
                out_cos[k - 1] = cos[k - 1] * gain;
                out_sin[k - 1] = sin[k - 1] * gain;
            }
        }
        MORPH_PHASE_DISPERSE => {
            keep_power = false;
            // A phase growing with the square of the harmonic: a chirp,
            // the top harmonics a few cycles late at full amount.
            let alpha = 4.0 * PI * m / h.max(1) as f32;
            for k in 1..=h {
                let (sin_t, cos_t) = (alpha * (k * k) as f32).sin_cos();
                let (c, s) = (cos[k - 1], sin[k - 1]);
                out_cos[k - 1] = c * cos_t + s * sin_t;
                out_sin[k - 1] = s * cos_t - c * sin_t;
            }
        }
        MORPH_SHEPARD => {
            // At 1, only the even harmonics remain, holding what harmonic
            // k/2 held: the frame an octave up.
            for k in 1..=h {
                let (c, s) = (cos[k - 1], sin[k - 1]);
                let (up_c, up_s) = if k % 2 == 0 {
                    (cos[k / 2 - 1], sin[k / 2 - 1])
                } else {
                    (0.0, 0.0)
                };
                out_cos[k - 1] = c + (up_c - c) * m;
                out_sin[k - 1] = s + (up_s - s) * m;
            }
        }
        _ => {
            keep_power = false;
            out_cos.copy_from_slice(cos);
            out_sin.copy_from_slice(sin);
        }
    }
    if keep_power {
        // Reshaping moves energy around rather than adding or taking it:
        // keep the frame's loudness as it was.
        let before = power(cos, sin);
        let after = power(out_cos, out_sin);
        if after > 1e-12 && before > 1e-12 {
            let scale = (before / after).sqrt().min(16.0);
            for (c, s) in out_cos.iter_mut().zip(out_sin.iter_mut()) {
                *c *= scale;
                *s *= scale;
            }
        }
    }
}

/// One cycle of `table` at `position` with morph `mode` at `amount` (and
/// Vocode's note at `hz`), at the fullest band limit, as frame 1 of a
/// two-frame level: what the oscillator's display draws, warps and all.
/// Allocates; for the UI thread.
pub fn morphed_level(table: &Table, position: f32, mode: usize, amount: f32, hz: f32) -> Level {
    let mut scratch = Scratch::default();
    let mut level = MorphState::default().level;
    let key = Key {
        mode,
        amount,
        frame: position.clamp(0.0, 1.0) * (table.frames - 1) as f32,
        level: 0,
        hz,
        table: 0,
    };
    render(&mut scratch, table, &key, &mut level);
    level
}

/// Draws one cycle of [`morphed_level`] into `out`, one point per element.
#[cfg(test)]
fn draw(table: &Table, position: f32, mode: usize, amount: f32, hz: f32, out: &mut [f32]) {
    let level = morphed_level(table, position, mode, amount, hz);
    let frame = level.frame(1);
    let n = out.len().max(1) as f32;
    for (i, sample) in out.iter_mut().enumerate() {
        let p = i as f32 / n;
        *sample = super::tables::read(frame, level.size, level.mask, p);
    }
}

/// The morphed frame's harmonic levels, harmonic 1 first, as many as `out`
/// holds: for drawing the spectrum.
pub fn spectrum(table: &Table, position: f32, mode: usize, amount: f32, hz: f32, out: &mut [f32]) {
    let mut scratch = Scratch::default();
    let key = Key {
        mode,
        amount,
        frame: position.clamp(0.0, 1.0) * (table.frames - 1) as f32,
        level: 0,
        hz,
        table: 0,
    };
    gather(&mut scratch, table, &key, MAX_HARMONICS);
    morph(&mut scratch, &key, MAX_HARMONICS);
    for (k, level) in out.iter_mut().enumerate() {
        *level = if k < MAX_HARMONICS {
            scratch.out_cos[k].hypot(scratch.out_sin[k])
        } else {
            0.0
        };
    }
}

#[cfg(test)]
mod tests {
    use super::super::osc::BLOCK;
    use super::super::tables::bank;
    use super::*;

    fn frame_of(mode: usize, amount: f32, position: f32) -> Vec<f32> {
        let mut out = vec![0.0; 256];
        draw(bank().table(0), position, mode, amount, 261.63, &mut out);
        out
    }

    #[test]
    fn the_inverse_transform_rebuilds_the_table() {
        // No morph renders the frame the table holds.
        let table = bank().table(0);
        for position in [0.0, 0.4, 2.0 / 3.0, 1.0] {
            let morphed = frame_of(MORPH_LOW_PASS, 0.0, position);
            for (i, x) in morphed.iter().enumerate() {
                let p = i as f32 / 256.0;
                let expected = table.sample(0, position, p);
                assert!(
                    (x - expected).abs() < 2e-3,
                    "{position} at {p}: {x} vs {expected}"
                );
            }
        }
    }

    #[test]
    fn identity_amounts_change_nothing() {
        let plain = frame_of(MORPH_OFF, 0.0, 0.7);
        for mode in 1..MORPH_COUNT {
            let amount = if mode == MORPH_FORMANT_SCALE {
                0.5
            } else {
                0.0
            };
            if !is_identity(mode, amount) {
                continue;
            }
            let morphed = frame_of(mode, amount, 0.7);
            for (a, b) in plain.iter().zip(&morphed) {
                assert!((a - b).abs() < 2e-3, "mode {mode}");
            }
        }
        // Vocode at its reference note and middle amount is the frame too.
        let vocoded = frame_of(MORPH_VOCODE, 0.5, 0.7);
        for (a, b) in plain.iter().zip(&vocoded) {
            assert!((a - b).abs() < 2e-3);
        }
    }

    #[test]
    fn every_morph_stays_finite_and_bounded_and_moves() {
        let plain = frame_of(MORPH_OFF, 0.0, 0.7);
        for mode in 1..MORPH_COUNT {
            for amount in [0.1, 0.35, 0.8, 1.0] {
                let morphed = frame_of(mode, amount, 0.7);
                assert!(
                    morphed.iter().all(|x| x.is_finite() && x.abs() < 8.0),
                    "mode {mode} at {amount}"
                );
            }
            let far = frame_of(mode, 0.9, 0.7);
            let moved = plain.iter().zip(&far).any(|(a, b)| (a - b).abs() > 0.05);
            assert!(moved, "mode {mode} changes the frame");
        }
    }

    #[test]
    fn low_pass_leaves_the_fundamental_and_high_pass_takes_it() {
        let mut levels = [0.0f32; 16];
        let table = bank().table(0);
        spectrum(table, 2.0 / 3.0, MORPH_LOW_PASS, 1.0, 261.63, &mut levels);
        assert!(
            levels[0] > 0.3 && levels[3..].iter().all(|l| *l < 1e-6),
            "{levels:?}"
        );
        // At 0.3 the cutoff sits at 512^0.3, about the 6.5th harmonic.
        spectrum(table, 2.0 / 3.0, MORPH_HIGH_PASS, 0.3, 261.63, &mut levels);
        assert!(
            levels[..5].iter().all(|l| *l < 1e-6) && levels[6] > 0.0,
            "{levels:?}"
        );
        // Stretching keeps the fundamental and the loudness.
        let mut plain = [0.0f32; 64];
        let mut stretched = [0.0f32; 64];
        spectrum(table, 2.0 / 3.0, MORPH_OFF, 0.0, 261.63, &mut plain);
        spectrum(
            table,
            2.0 / 3.0,
            MORPH_HARMONIC_STRETCH,
            0.5,
            261.63,
            &mut stretched,
        );
        assert!(stretched[1] < plain[1] * 0.5, "the 2nd harmonic moved up");
        let energy = |l: &[f32]| l.iter().map(|x| x * x).sum::<f32>();
        let ratio = energy(&stretched) / energy(&plain);
        assert!((0.8..1.25).contains(&ratio), "{ratio}");
    }

    #[test]
    fn a_state_redoes_its_frame_only_when_something_moved() {
        prepare();
        let table = bank().table(0);
        let mut scratch = Scratch::default();
        let mut state = MorphState::default();
        state.tick(
            &mut scratch,
            table,
            MORPH_SMEAR,
            0.5,
            0.5,
            3,
            261.0,
            BLOCK,
            true,
        );
        let (_, t, _) = state.frames();
        assert_eq!(t, 1.0, "a note starts on its frame");
        let first: Vec<f32> = state.level.frame(1).to_vec();
        for _ in 0..4 {
            state.tick(
                &mut scratch,
                table,
                MORPH_SMEAR,
                0.5,
                0.5,
                3,
                261.0,
                BLOCK,
                false,
            );
        }
        assert_eq!(state.level.frame(1), &first[..], "nothing moved");
        state.tick(
            &mut scratch,
            table,
            MORPH_SMEAR,
            0.9,
            0.5,
            3,
            261.0,
            BLOCK,
            false,
        );
        let (_, t, step) = state.frames();
        assert_eq!(t, 0.0, "crossfades from the old frame");
        assert!(step > 0.0);
        assert_eq!(state.level.frame(0), &first[..]);
        assert_ne!(state.level.frame(1), &first[..]);
        state.advance(BLOCK);
        state.advance(BLOCK);
        assert!((state.frames().1 - 1.0).abs() < 1e-6);
    }
}
