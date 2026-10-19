//! Wavetable oscillators: unison stacks reading band-limited tables, with
//! two warp slots that bend the phase or the output.
//!
//! An oscillator renders a short block (one control interval) at a time.
//! Everything that can move (each unison voice's frequency, stereo gains,
//! warp amounts, band limit and table position) ramps linearly across the
//! block from the last control tick's value to the next, so modulation is
//! smooth however fast it is.
//!
//! The second warp slot works on what the first one made: its phase warp
//! picks where in the first slot's wave to read, and its output warp
//! changes that reading. The first slot's mode is a const generic of the
//! render loop, so one warp costs what it always did; the second, when on,
//! is a match per sample that the branch predictor learns at once.
//!
//! With a spectral morph on, the voices read a pair of frames the morph
//! made (see `morph`) instead of the table, crossfading from the older to
//! the newer across the block, so the morph moves as smoothly as the
//! position does.

use std::f32::consts::TAU;

use super::tables::{Level, Table, read};
use dsp::rational_tanh;

pub const MAX_UNISON: usize = 16;
/// Samples per control interval; oscillator blocks are never longer.
pub const BLOCK: usize = 32;

/// One block of each unison voice's output, for cross-modulation. Twice a
/// control interval long, for oscillators rendering at twice the rate.
pub type Unison = [[f32; 2 * BLOCK]; MAX_UNISON];

pub const WARP_OFF: usize = 0;
/// Hard sync: the table plays up to 8 times per cycle, restarting each cycle.
pub const WARP_SYNC: usize = 1;
/// Sync with each cycle faded in and out by a half-sine window, which
/// removes the restart's click for a vocal, softer sync.
pub const WARP_SYNC_WIN: usize = 2;
/// Phase distortion squeezing the wave toward the middle of the cycle.
pub const WARP_BEND_POS: usize = 3;
/// Phase distortion pushing the wave out toward the cycle's edges.
pub const WARP_BEND_NEG: usize = 4;
/// Plays the whole wave in a shrinking first part of the cycle, then
/// silence: pulse-width modulation for any table.
pub const WARP_PWM: usize = 5;
/// Smoothly squeezes the first half of the wave into less than half the cycle.
pub const WARP_ASYM_POS: usize = 6;
/// Smoothly stretches the first half of the wave over more than half the cycle.
pub const WARP_ASYM_NEG: usize = 7;
/// Fades to the wave played forward then backward within each cycle.
pub const WARP_MIRROR: usize = 8;
/// Inverts the end of the cycle, from `1 - amount` on.
pub const WARP_FLIP: usize = 9;
/// Rounds the output to ever fewer levels: bit reduction.
pub const WARP_QUANTIZE: usize = 10;
/// Phase modulation by the other oscillator, up to two cycles deep.
pub const WARP_FM: usize = 11;
/// Amplitude modulation by the other oscillator.
pub const WARP_AM: usize = 12;
/// Ring modulation (multiplication) by the other oscillator.
pub const WARP_RM: usize = 13;
/// Bend both ways from the middle of the knob: below it as Bend-, above
/// as Bend+ (Serum's Bend +/-), so one modulation sweeps through both.
pub const WARP_BEND_BI: usize = 14;
/// Asym both ways from the middle of the knob, as Bend ± does for Bend.
pub const WARP_ASYM_BI: usize = 15;
/// Drives the wave into a wavefolder: past full scale it folds back.
pub const WARP_FOLD: usize = 16;
/// Drives the wave into a soft clipper, toward a square.
pub const WARP_DRIVE: usize = 17;
/// Squeezes the whole wave into a shrinking middle of the cycle, holding
/// its ends either side (Vital's Squeeze): a vocal, formant-like sweep.
pub const WARP_SQUEEZE: usize = 18;
/// Reads the wave where the oscillator's remap curve says (Serum's Remap):
/// the curve maps the cycle's place to the place read, and the amount
/// fades from reading straight to reading through the curve.
pub const WARP_REMAP: usize = 19;
/// The remap curve on each half of the cycle, the second half mirrored:
/// symmetric remapping without drawing a symmetric curve (Serum's Remap 2).
pub const WARP_REMAP_MIRROR: usize = 20;
/// The mirrored remap on each half of the cycle: the curve four times
/// over, every other one mirrored (Serum's Remap 4).
pub const WARP_REMAP_X4: usize = 21;
pub const WARP_COUNT: usize = 22;

/// Points a remap curve is looked up from, plus one at the end: enough
/// that a jump in the curve (two nodes at one place) reads as one.
pub const REMAP_SIZE: usize = 1024;
/// The steepest a remap curve counts as for the band limit: steeper
/// stretches are jumps, which no band limit smooths.
const REMAP_MAX_SLOPE: f32 = 16.0;
/// A remap curve as a table: the place read at `i / REMAP_SIZE` of the cycle.
pub type Remap = [f32; REMAP_SIZE + 1];
/// The remap that changes nothing.
pub const STRAIGHT_REMAP: Remap = {
    let mut table = [0.0; REMAP_SIZE + 1];
    let mut i = 0;
    while i <= REMAP_SIZE {
        table[i] = i as f32 / REMAP_SIZE as f32;
        i += 1;
    }
    table
};

/// Whether warp mode `mode` reads through the remap curve.
pub const fn is_remap(mode: usize) -> bool {
    matches!(mode, WARP_REMAP | WARP_REMAP_MIRROR | WARP_REMAP_X4)
}

/// How much faster than the note a remap warp at `amount` reads the
/// table, at most, for a curve whose steepest stretch rises `slope` times
/// faster than a straight line (see [`remap_slope`]).
pub fn remap_speed(amount: f32, slope: f32) -> f32 {
    1.0 + amount.clamp(0.0, 1.0) * (slope - 1.0).max(0.0) * SIDEBAND_MARGIN
}

/// The steepest stretch of remap curve `remap`, as a multiple of a
/// straight line's slope, jumps left out (see [`REMAP_MAX_SLOPE`]).
pub fn remap_slope(remap: &Remap) -> f32 {
    remap
        .windows(2)
        .map(|w| (w[1] - w[0]).abs() * REMAP_SIZE as f32)
        .filter(|&slope| slope <= 2.0 * REMAP_MAX_SLOPE)
        .fold(1.0, f32::max)
        .min(REMAP_MAX_SLOPE)
}

/// The remap curve at `x` (`0..=1`), between its table's points.
#[inline(always)]
fn remap_at(remap: &Remap, x: f32) -> f32 {
    let place = x.clamp(0.0, 1.0) * REMAP_SIZE as f32;
    let i = (place as usize).min(REMAP_SIZE - 1);
    let t = place - i as f32;
    remap[i] + (remap[i + 1] - remap[i]) * t
}

/// Whether warp mode `mode` listens to another oscillator.
pub const fn is_cross_mod(mode: usize) -> bool {
    matches!(mode, WARP_FM | WARP_AM | WARP_RM)
}

const SYNC_MAX: f32 = 7.0;
const BEND_MAX: f32 = 0.95;
const PWM_MAX: f32 = 0.95;
/// Asym's skew: `p + b·sin²(πp)` stays monotonic while `π·b < 1`.
const ASYM_MAX: f32 = 0.3;
const FM_DEPTH: f32 = 2.0;
/// How far Fold drives the wave at full amount: up to four folds.
const FOLD_MAX: f32 = 7.0;
/// Drive's gain into the clipper at full amount (about 24 dB).
const DRIVE_MAX: f32 = 15.0;
/// Squeeze's narrowest wave, as a share of the cycle.
const SQUEEZE_MIN: f32 = 0.1;
/// Bend and Asym are smooth phase modulations: besides reading the table
/// faster, they add sidebands around each harmonic. Widening the band
/// estimate by half the extra speed covers them (Carson's rule).
const SIDEBAND_MARGIN: f32 = 1.5;

/// How much faster than the note a warp ever reads the table, at its
/// fastest point, with room for the sidebands it adds: the band limit is
/// chosen for this so the warp doesn't alias. Sync, PWM, Flip, Quantize,
/// Fold and Drive add hard edges or harmonics of their own, which alias a
/// little whatever the band limit, as in analog-modelled synths (the High
/// quality setting halves it). Cross-modulation is handled by the caller,
/// which knows the other oscillator's pitch. Two warp slots read at most
/// the product of their speeds.
pub fn warp_speed(mode: usize, amount: f32) -> f32 {
    let a = amount.clamp(0.0, 1.0);
    match mode {
        WARP_SYNC | WARP_SYNC_WIN => 1.0 + SYNC_MAX * a,
        WARP_BEND_POS | WARP_BEND_NEG => 1.0 + BEND_MAX * a * SIDEBAND_MARGIN,
        WARP_BEND_BI => 1.0 + BEND_MAX * (2.0 * a - 1.0).abs() * SIDEBAND_MARGIN,
        WARP_PWM => 1.0 / (1.0 - PWM_MAX * a),
        WARP_ASYM_POS | WARP_ASYM_NEG => {
            1.0 + std::f32::consts::PI * ASYM_MAX * a * SIDEBAND_MARGIN
        }
        WARP_ASYM_BI => {
            1.0 + std::f32::consts::PI * ASYM_MAX * (2.0 * a - 1.0).abs() * SIDEBAND_MARGIN
        }
        WARP_MIRROR if a > 0.0 => 2.0,
        WARP_SQUEEZE => 1.0 / squeeze_width(a),
        _ => 1.0,
    }
}

/// How deep FM is at warp amount `amount`, in cycles of phase per unit of
/// the modulator: the caller's band limit needs it.
pub fn fm_depth(amount: f32) -> f32 {
    FM_DEPTH * amount.clamp(0.0, 1.0)
}

#[inline(always)]
fn squeeze_width(a: f32) -> f32 {
    1.0 - (1.0 - SQUEEZE_MIN) * a.clamp(0.0, 1.0)
}

/// `sin(2π p)`, accurate to about 2e-4, without a library call.
#[inline(always)]
pub fn fast_sin(p: f32) -> f32 {
    // Fold onto -1/4..1/4 of a turn, where a short odd polynomial is enough.
    let mut x = p - (p as i32) as f32;
    if x < 0.0 {
        x += 1.0;
    }
    let mut x = x - 0.5;
    if x > 0.25 {
        x = 0.5 - x;
    } else if x < -0.25 {
        x = -0.5 - x;
    }
    let t = -TAU * x;
    let t2 = t * t;
    t * (1.0 - t2 / 6.0 * (1.0 - t2 / 20.0 * (1.0 - t2 / 42.0)))
}

/// `x` wrapped into `0..1`.
#[inline(always)]
fn wrap(x: f32) -> f32 {
    let f = x - (x as i32) as f32;
    if f < 0.0 { f + 1.0 } else { f }
}

/// Where in the table warp `W` reads at phase `p` with amount `a` and
/// modulator `m`.
#[inline(always)]
fn warp_phase<const W: usize>(p: f32, a: f32, m: f32, r: &Remap) -> f32 {
    match W {
        WARP_REMAP | WARP_REMAP_MIRROR | WARP_REMAP_X4 => {
            // The curve on each half, the second turned about the middle.
            let mirrored = |p: f32| {
                if p < 0.5 {
                    0.5 * remap_at(r, 2.0 * p)
                } else {
                    1.0 - 0.5 * remap_at(r, 2.0 - 2.0 * p)
                }
            };
            let read = match W {
                WARP_REMAP => remap_at(r, p),
                WARP_REMAP_MIRROR => mirrored(p),
                // The mirrored remap again on each half.
                _ => {
                    if p < 0.5 {
                        0.5 * mirrored(2.0 * p)
                    } else {
                        0.5 + 0.5 * mirrored(2.0 * p - 1.0)
                    }
                }
            };
            (p + (read - p) * a).clamp(0.0, 0.999_999)
        }
        WARP_SYNC | WARP_SYNC_WIN => {
            let x = p * (1.0 + SYNC_MAX * a);
            x - (x as u32) as f32
        }
        WARP_BEND_POS => p - BEND_MAX * a * fast_sin(p) / TAU,
        WARP_BEND_NEG => p + BEND_MAX * a * fast_sin(p) / TAU,
        WARP_BEND_BI => p - BEND_MAX * (2.0 * a - 1.0) * fast_sin(p) / TAU,
        WARP_PWM => {
            let width = 1.0 - PWM_MAX * a;
            if p < width { p / width } else { 0.0 }
        }
        WARP_ASYM_POS | WARP_ASYM_NEG | WARP_ASYM_BI => {
            // p + b·sin²(πp), with sin² written as (1 - cos 2πp) / 2. Smooth,
            // so unlike a two-slope (Casio CZ) skew it adds no sharp corners.
            let b = match W {
                WARP_ASYM_POS => ASYM_MAX * a,
                WARP_ASYM_NEG => -ASYM_MAX * a,
                _ => ASYM_MAX * (2.0 * a - 1.0),
            };
            (p + b * 0.5 * (1.0 - fast_sin(p + 0.25))).clamp(0.0, 0.999_999)
        }
        WARP_SQUEEZE => {
            // The wave's ends are the same sample, so holding them either
            // side of the squeezed wave adds no step.
            ((p - 0.5) / squeeze_width(a) + 0.5).clamp(0.0, 0.999_999)
        }
        WARP_FM => wrap(p + FM_DEPTH * a * m),
        _ => p,
    }
}

/// The mirrored read position: forward over the first half, back over the second.
#[inline(always)]
fn mirror(p: f32) -> f32 {
    let x = 2.0 * p;
    if x < 1.0 { x } else { (2.0 - x).min(0.999_999) }
}

/// A triangle fold of `v` into `-1..=1`: unchanged inside, reflected at
/// the edges as often as needed.
#[inline(always)]
fn fold(v: f32) -> f32 {
    let x = v - 1.0;
    let x = x - 4.0 * (x * 0.25).floor();
    (x - 2.0).abs() - 1.0
}

/// Warp `W`'s change to the output `x`.
#[inline(always)]
fn warp_output<const W: usize>(x: f32, p: f32, a: f32, m: f32, quantize: f32) -> f32 {
    match W {
        WARP_SYNC_WIN => x * fast_sin(0.5 * p),
        WARP_PWM => {
            if p < 1.0 - PWM_MAX * a {
                x
            } else {
                0.0
            }
        }
        WARP_FLIP => {
            if p >= 1.0 - a {
                -x
            } else {
                x
            }
        }
        WARP_QUANTIZE => {
            if quantize > 0.0 {
                (x * quantize).round() / quantize
            } else {
                x
            }
        }
        WARP_AM => x * (1.0 - 0.5 * a + 0.5 * a * m),
        WARP_RM => x * (1.0 - a + a * m),
        WARP_FOLD => fold(x * (1.0 + FOLD_MAX * a)),
        WARP_DRIVE => {
            // Faded in over the first tenth, so a little drive is a little.
            let g = 1.0 + DRIVE_MAX * a * a;
            let clipped = rational_tanh(g * x) / rational_tanh(g);
            x + (clipped - x) * (a * 10.0).min(1.0)
        }
        _ => x,
    }
}

/// [`warp_phase`] for a mode chosen at run time.
#[inline(always)]
fn warp_phase_dyn(mode: usize, p: f32, a: f32, m: f32, r: &Remap) -> f32 {
    match mode {
        WARP_REMAP => warp_phase::<WARP_REMAP>(p, a, m, r),
        WARP_REMAP_MIRROR => warp_phase::<WARP_REMAP_MIRROR>(p, a, m, r),
        WARP_REMAP_X4 => warp_phase::<WARP_REMAP_X4>(p, a, m, r),
        WARP_SYNC | WARP_SYNC_WIN => warp_phase::<WARP_SYNC>(p, a, m, r),
        WARP_BEND_POS => warp_phase::<WARP_BEND_POS>(p, a, m, r),
        WARP_BEND_NEG => warp_phase::<WARP_BEND_NEG>(p, a, m, r),
        WARP_BEND_BI => warp_phase::<WARP_BEND_BI>(p, a, m, r),
        WARP_PWM => warp_phase::<WARP_PWM>(p, a, m, r),
        WARP_ASYM_POS => warp_phase::<WARP_ASYM_POS>(p, a, m, r),
        WARP_ASYM_NEG => warp_phase::<WARP_ASYM_NEG>(p, a, m, r),
        WARP_ASYM_BI => warp_phase::<WARP_ASYM_BI>(p, a, m, r),
        WARP_SQUEEZE => warp_phase::<WARP_SQUEEZE>(p, a, m, r),
        WARP_FM => warp_phase::<WARP_FM>(p, a, m, r),
        _ => p,
    }
}

/// [`warp_output`] for a mode chosen at run time.
#[inline(always)]
fn warp_output_dyn(mode: usize, x: f32, p: f32, a: f32, m: f32, quantize: f32) -> f32 {
    match mode {
        WARP_SYNC_WIN => warp_output::<WARP_SYNC_WIN>(x, p, a, m, quantize),
        WARP_PWM => warp_output::<WARP_PWM>(x, p, a, m, quantize),
        WARP_FLIP => warp_output::<WARP_FLIP>(x, p, a, m, quantize),
        WARP_QUANTIZE => warp_output::<WARP_QUANTIZE>(x, p, a, m, quantize),
        WARP_AM => warp_output::<WARP_AM>(x, p, a, m, quantize),
        WARP_RM => warp_output::<WARP_RM>(x, p, a, m, quantize),
        WARP_FOLD => warp_output::<WARP_FOLD>(x, p, a, m, quantize),
        WARP_DRIVE => warp_output::<WARP_DRIVE>(x, p, a, m, quantize),
        _ => x,
    }
}

/// The first warp slot, `W` at amount `a`, at phase `p`, on the wave that
/// `sample` reads (a function of the table phase).
#[inline(always)]
fn first_warp<const W: usize>(
    p: f32,
    a: f32,
    m: f32,
    quantize: f32,
    r: &Remap,
    sample: impl Fn(f32) -> f32,
) -> f32 {
    let mut x = sample(warp_phase::<W>(p, a, m, r));
    if W == WARP_MIRROR {
        x += (sample(mirror(p)) - x) * a;
    }
    warp_output::<W>(x, p, a, m, quantize)
}

/// The second warp slot, `mode` at amount `a`, at phase `p`, on the wave
/// the first slot makes (`first`, a function of the phase).
#[inline(always)]
fn second_warp(
    mode: usize,
    p: f32,
    a: f32,
    m: f32,
    quantize: f32,
    r: &Remap,
    first: impl Fn(f32) -> f32,
) -> f32 {
    if mode == WARP_MIRROR {
        let x = first(p);
        x + (first(mirror(p)) - x) * a
    } else {
        warp_output_dyn(
            mode,
            first(warp_phase_dyn(mode, p, a, m, r)),
            p,
            a,
            m,
            quantize,
        )
    }
}

/// Levels per unit for quantize at amount `a`, or 0 for none.
fn quantize_levels(a: f32) -> f32 {
    if a <= 0.001 {
        0.0
    } else {
        (1.0 + 7.0 * (1.0 - a.min(1.0))).exp2()
    }
}

/// One sample of a table at frame position `frame` (in frames) and table
/// phase `p`, interpolating between the two nearest frames.
#[inline(always)]
fn read_frames(level: &Level, frames: usize, frame: f32, p: f32) -> f32 {
    let i = (frame as usize).min(frames - 1);
    let t = frame - i as f32;
    let j = (i + 1).min(frames - 1);
    let a = read(level.frame(i), level.size, level.mask, p);
    let b = read(level.frame(j), level.size, level.mask, p);
    a + (b - a) * t
}

/// A single warped sample of one cycle's band limit `level` (with
/// `frames` frames) at frame position `frame`, phase `p`, through both
/// warp slots (`(mode, amount)` each) and remap curve `r`, with `m`
/// standing in for the other oscillator: what [`warped_sample`] draws, for
/// any frames.
pub fn warped_level_sample(
    level: &Level,
    frames: usize,
    frame: f32,
    warps: [(usize, f32); 2],
    r: &Remap,
    p: f32,
    m: f32,
) -> f32 {
    let sample = |q: f32| read_frames(level, frames, frame, q);
    let [(mode, a), (mode2, a2)] = warps.map(|(mode, a)| (mode, a.clamp(0.0, 1.0)));
    let (q, q2) = (quantize_levels(a), quantize_levels(a2));
    let first = |p: f32| {
        macro_rules! warped {
            ($w:expr) => {
                first_warp::<{ $w }>(p, a, m, q, r, sample)
            };
        }
        match mode {
            WARP_REMAP => warped!(WARP_REMAP),
            WARP_REMAP_MIRROR => warped!(WARP_REMAP_MIRROR),
            WARP_REMAP_X4 => warped!(WARP_REMAP_X4),
            WARP_SYNC => warped!(WARP_SYNC),
            WARP_SYNC_WIN => warped!(WARP_SYNC_WIN),
            WARP_BEND_POS => warped!(WARP_BEND_POS),
            WARP_BEND_NEG => warped!(WARP_BEND_NEG),
            WARP_PWM => warped!(WARP_PWM),
            WARP_ASYM_POS => warped!(WARP_ASYM_POS),
            WARP_ASYM_NEG => warped!(WARP_ASYM_NEG),
            WARP_MIRROR => warped!(WARP_MIRROR),
            WARP_FLIP => warped!(WARP_FLIP),
            WARP_QUANTIZE => warped!(WARP_QUANTIZE),
            WARP_FM => warped!(WARP_FM),
            WARP_AM => warped!(WARP_AM),
            WARP_RM => warped!(WARP_RM),
            WARP_BEND_BI => warped!(WARP_BEND_BI),
            WARP_ASYM_BI => warped!(WARP_ASYM_BI),
            WARP_FOLD => warped!(WARP_FOLD),
            WARP_DRIVE => warped!(WARP_DRIVE),
            WARP_SQUEEZE => warped!(WARP_SQUEEZE),
            _ => warped!(WARP_OFF),
        }
    };
    if mode2 == WARP_OFF {
        first(p)
    } else {
        second_warp(mode2, p, a2, m, q2, r, first)
    }
}

/// A single warped sample, for drawing: table `table` at position `0..=1`,
/// phase `p`, through both warp slots (`(mode, amount)` each), with `m`
/// standing in for the other oscillator.
pub fn warped_sample(
    table: &Table,
    position: f32,
    warps: [(usize, f32); 2],
    r: &Remap,
    p: f32,
    m: f32,
) -> f32 {
    let frame = position.clamp(0.0, 1.0) * (table.frames - 1) as f32;
    warped_level_sample(&table.levels[0], table.frames, frame, warps, r, p, m)
}

/// Where an oscillator should be at the next control tick.
#[derive(Debug, Clone, Copy)]
pub struct OscTargets {
    pub count: usize,
    /// Phase increment per sample of each unison voice.
    pub inc: [f32; MAX_UNISON],
    pub gain_l: [f32; MAX_UNISON],
    pub gain_r: [f32; MAX_UNISON],
    /// Table position, `0..=1`, before position spread, for the meters.
    pub position: f32,
    /// Each unison voice's table position; they differ with position spread.
    pub positions: [f32; MAX_UNISON],
    /// Each unison voice's amount for each warp slot.
    pub warp: [[f32; MAX_UNISON]; 2],
    /// The two amounts before warp spread, for the meters.
    pub warp_amounts: [f32; 2],
    /// Each unison voice's band limit: the table level to read.
    pub level: [u8; MAX_UNISON],
}

impl Default for OscTargets {
    fn default() -> Self {
        Self {
            count: 1,
            inc: [0.0; MAX_UNISON],
            gain_l: [0.0; MAX_UNISON],
            gain_r: [0.0; MAX_UNISON],
            position: 0.0,
            positions: [0.0; MAX_UNISON],
            warp: [[0.0; MAX_UNISON]; 2],
            warp_amounts: [0.0; 2],
            level: [0; MAX_UNISON],
        }
    }
}

/// What an oscillator's voices read this block.
#[derive(Clone, Copy)]
pub enum Frames<'a> {
    /// The table, each voice at its own band limit and position.
    Table(&'a Table),
    /// Two frames a spectral morph made (see `morph`), one band limit for
    /// every voice, crossfading from the first to the second: `t` of the
    /// way at the block's start, `t_step` more each sample.
    Morphed {
        level: &'a Level,
        t: f32,
        t_step: f32,
    },
}

/// One oscillator's unison stack within a voice.
#[derive(Debug, Clone)]
pub struct UnisonOsc {
    /// Unison voices rendered: the target count, or more while removed
    /// voices fade out.
    count: usize,
    /// The count to drop to once the fade finishes.
    next_count: usize,
    pub phase: [f32; MAX_UNISON],
    inc: [f32; MAX_UNISON],
    inc_step: [f32; MAX_UNISON],
    gain_l: [f32; MAX_UNISON],
    gain_l_step: [f32; MAX_UNISON],
    gain_r: [f32; MAX_UNISON],
    gain_r_step: [f32; MAX_UNISON],
    /// Each voice's amount for each warp slot; they differ with warp spread.
    warp: [[f32; MAX_UNISON]; 2],
    warp_step: [[f32; MAX_UNISON]; 2],
    /// Each voice's band limit. Stacked voices an octave or two apart need
    /// different ones, or the lower layers would lose their top harmonics.
    /// Bytes, like the two below, to keep the voice's hot data small.
    level: [u8; MAX_UNISON],
    /// The band limit being faded out after `level` changed, so a pitch
    /// crossing an octave doesn't drop its top harmonics in one sample.
    old_level: [u8; MAX_UNISON],
    /// Samples into that crossfade, which lasts one control interval.
    fade: [u8; MAX_UNISON],
    /// Each voice's position in frames, `0..=frames - 1`.
    frame: [f32; MAX_UNISON],
    frame_step: [f32; MAX_UNISON],
    /// The position before spread and warp amounts before spread, as of
    /// the last tick.
    position: f32,
    warp_amounts: [f32; 2],
    /// Silenced by [`Self::park`] and untouched since.
    parked: bool,
}

impl Default for UnisonOsc {
    fn default() -> Self {
        Self {
            count: 0,
            next_count: 0,
            phase: [0.0; MAX_UNISON],
            inc: [0.0; MAX_UNISON],
            inc_step: [0.0; MAX_UNISON],
            gain_l: [0.0; MAX_UNISON],
            gain_l_step: [0.0; MAX_UNISON],
            gain_r: [0.0; MAX_UNISON],
            gain_r_step: [0.0; MAX_UNISON],
            warp: [[0.0; MAX_UNISON]; 2],
            warp_step: [[0.0; MAX_UNISON]; 2],
            level: [0; MAX_UNISON],
            old_level: [0; MAX_UNISON],
            fade: [BLOCK as u8; MAX_UNISON],
            frame: [0.0; MAX_UNISON],
            frame_step: [0.0; MAX_UNISON],
            position: 0.0,
            warp_amounts: [0.0; 2],
            parked: false,
        }
    }
}

impl UnisonOsc {
    /// The current table position, `0..=1`, before position spread, for the meters.
    pub fn position(&self) -> f32 {
        self.position
    }

    /// The warp slots' modulated amounts (before spread), for the meters.
    pub fn warps(&self) -> [f32; 2] {
        self.warp_amounts
    }

    /// Unison voices being rendered.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Silences the stack while its oscillator is off, so that switched
    /// back on it starts at its pitch rather than sweeping there. Only the
    /// first call after [`Self::set_targets`] does anything, so an
    /// oscillator that stays off costs nothing per control tick.
    pub fn park(&mut self, frames: usize) {
        if !self.parked {
            self.set_targets(&OscTargets::default(), frames, BLOCK, true);
            self.parked = true;
        }
    }

    /// Heads for `t` over `steps` samples, or jumps there if `immediate`
    /// (a note starting).
    pub fn set_targets(&mut self, t: &OscTargets, frames: usize, steps: usize, immediate: bool) {
        self.parked = false;
        let count = t.count.clamp(1, MAX_UNISON);
        let last = (frames.max(1) - 1) as f32;
        let frame_of = |u: usize| t.positions[u].clamp(0.0, 1.0) * last;
        self.position = t.position;
        self.warp_amounts = t.warp_amounts;
        if immediate || steps == 0 {
            self.fade = [BLOCK as u8; MAX_UNISON];
            self.level = t.level;
            self.count = count;
            self.next_count = count;
            self.inc = t.inc;
            self.gain_l = t.gain_l;
            self.gain_r = t.gain_r;
            for u in count..MAX_UNISON {
                self.gain_l[u] = 0.0;
                self.gain_r[u] = 0.0;
            }
            self.inc_step = [0.0; MAX_UNISON];
            self.gain_l_step = [0.0; MAX_UNISON];
            self.gain_r_step = [0.0; MAX_UNISON];
            self.frame = std::array::from_fn(frame_of);
            self.frame_step = [0.0; MAX_UNISON];
            self.warp = t.warp;
            self.warp_step = [[0.0; MAX_UNISON]; 2];
            return;
        }
        // A fade-out that finished last interval: those voices are silent now.
        if self.next_count < self.count {
            for u in self.next_count..self.count {
                self.gain_l[u] = 0.0;
                self.gain_r[u] = 0.0;
            }
            self.count = self.next_count;
        }
        let inv = 1.0 / steps as f32;
        if count >= self.count {
            // New voices fade in from silence (their gains are 0).
            self.count = count;
        }
        self.next_count = count;
        for u in 0..self.count {
            let (target_l, target_r) = if u < count {
                if self.inc[u] == 0.0 {
                    // Never sounded: start at pitch, warp, position and
                    // band limit rather than sweep to them.
                    self.inc[u] = t.inc[u];
                    self.warp[0][u] = t.warp[0][u];
                    self.warp[1][u] = t.warp[1][u];
                    self.level[u] = t.level[u];
                    self.frame[u] = frame_of(u);
                    self.fade[u] = BLOCK as u8;
                }
                if t.level[u] != self.level[u] {
                    self.old_level[u] = self.level[u];
                    self.level[u] = t.level[u];
                    self.fade[u] = 0;
                }
                self.inc_step[u] = (t.inc[u] - self.inc[u]) * inv;
                for slot in 0..2 {
                    self.warp_step[slot][u] = (t.warp[slot][u] - self.warp[slot][u]) * inv;
                }
                self.frame_step[u] = (frame_of(u) - self.frame[u]) * inv;
                (t.gain_l[u], t.gain_r[u])
            } else {
                // Fading out: keep the pitch, warp and position, drop the level.
                self.inc_step[u] = 0.0;
                self.warp_step[0][u] = 0.0;
                self.warp_step[1][u] = 0.0;
                self.frame_step[u] = 0.0;
                (0.0, 0.0)
            };
            self.gain_l_step[u] = (target_l - self.gain_l[u]) * inv;
            self.gain_r_step[u] = (target_r - self.gain_r[u]) * inv;
        }
    }

    /// Adds `left.len()` (at most two control intervals) samples of this
    /// oscillator to `left` and `right`, reading `frames`, through warp
    /// slots `modes` (the remap warps reading through `remap`).
    /// `modulator` holds the unison voices of what it listens to, unscaled,
    /// and how many there are, for the cross-modulation warps; `store`, if
    /// given, receives this one's for others to use.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        frames: Frames,
        modes: [usize; 2],
        remap: &Remap,
        modulator: Option<(&Unison, usize)>,
        store: Option<&mut Unison>,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let modulator = modulator.filter(|(_, count)| *count > 0);
        // Cross-modulation with nothing to listen to is off.
        let [first, second] = modes.map(|mode| {
            if is_cross_mod(mode) && modulator.is_none() {
                WARP_OFF
            } else {
                mode
            }
        });
        macro_rules! go {
            ($w:expr) => {
                if second == WARP_OFF {
                    self.render_warp::<{ $w }, false>(
                        frames, second, remap, modulator, store, left, right,
                    )
                } else {
                    self.render_warp::<{ $w }, true>(
                        frames, second, remap, modulator, store, left, right,
                    )
                }
            };
        }
        match first {
            WARP_REMAP => go!(WARP_REMAP),
            WARP_REMAP_MIRROR => go!(WARP_REMAP_MIRROR),
            WARP_REMAP_X4 => go!(WARP_REMAP_X4),
            WARP_SYNC => go!(WARP_SYNC),
            WARP_SYNC_WIN => go!(WARP_SYNC_WIN),
            WARP_BEND_POS => go!(WARP_BEND_POS),
            WARP_BEND_NEG => go!(WARP_BEND_NEG),
            WARP_PWM => go!(WARP_PWM),
            WARP_ASYM_POS => go!(WARP_ASYM_POS),
            WARP_ASYM_NEG => go!(WARP_ASYM_NEG),
            WARP_MIRROR => go!(WARP_MIRROR),
            WARP_FLIP => go!(WARP_FLIP),
            WARP_QUANTIZE => go!(WARP_QUANTIZE),
            WARP_FM => go!(WARP_FM),
            WARP_AM => go!(WARP_AM),
            WARP_RM => go!(WARP_RM),
            WARP_BEND_BI => go!(WARP_BEND_BI),
            WARP_ASYM_BI => go!(WARP_ASYM_BI),
            WARP_FOLD => go!(WARP_FOLD),
            WARP_DRIVE => go!(WARP_DRIVE),
            WARP_SQUEEZE => go!(WARP_SQUEEZE),
            _ => go!(WARP_OFF),
        }
    }

    /// Every unison voice with first warp `W`; `SECOND`: warp slot 2 is on,
    /// in mode `mode2`.
    #[allow(clippy::too_many_arguments)]
    fn render_warp<const W: usize, const SECOND: bool>(
        &mut self,
        frames: Frames,
        mode2: usize,
        remap: &Remap,
        modulator: Option<(&Unison, usize)>,
        mut store: Option<&mut Unison>,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let len = left.len().min(right.len()).min(2 * BLOCK);
        let (left, right) = (&mut left[..len], &mut right[..len]);
        let no_mod = [0.0f32; 2 * BLOCK];
        for u in 0..self.count {
            let modulation = match modulator {
                Some((buffers, count)) => &buffers[u % count],
                None => &no_mod,
            };
            let store = store.as_deref_mut().map(|s| &mut s[u]);
            match frames {
                Frames::Table(table) => {
                    let view = View::new(&table.levels[usize::from(self.level[u])]);
                    let old = View::new(&table.levels[usize::from(self.old_level[u])]);
                    let positions = (self.frame[u], self.frame_step[u]);
                    let last = table.frames - 1;
                    if usize::from(self.fade[u]) < BLOCK && self.old_level[u] != self.level[u] {
                        self.render_voice::<W, SECOND, true>(
                            u, view, old, last, positions, mode2, remap, modulation, store, left,
                            right,
                        );
                    } else {
                        self.render_voice::<W, SECOND, false>(
                            u, view, old, last, positions, mode2, remap, modulation, store, left,
                            right,
                        );
                    }
                }
                Frames::Morphed { level, t, t_step } => {
                    let view = View::new(level);
                    self.render_voice::<W, SECOND, false>(
                        u,
                        view,
                        view,
                        1,
                        (t, t_step),
                        mode2,
                        remap,
                        modulation,
                        store,
                        left,
                        right,
                    );
                }
            }
        }
        if let Frames::Table(table) = frames {
            let last_frame = (table.frames - 1) as f32;
            let voices = self.frame.iter_mut().zip(&self.frame_step).take(self.count);
            for (frame, step) in voices {
                *frame = (*frame + step * len as f32).clamp(0.0, last_frame);
            }
        }
    }

    /// Unison voice `u`, reading `view` (and crossfading from its old band
    /// limit `old` during this block if `FADE`) between frames 0 and
    /// `last`, from frame position `positions.0` moving `positions.1` per
    /// sample.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn render_voice<const W: usize, const SECOND: bool, const FADE: bool>(
        &mut self,
        u: usize,
        view: View,
        old: View,
        last_frame: usize,
        positions: (f32, f32),
        mode2: usize,
        remap: &Remap,
        modulation: &[f32; 2 * BLOCK],
        mut store: Option<&mut [f32; 2 * BLOCK]>,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let fade_start = usize::from(self.fade[u]);
        let [mut warp, mut warp2] = [self.warp[0][u], self.warp[1][u]];
        let [warp_step, warp2_step] = [self.warp_step[0][u], self.warp_step[1][u]];
        // Quantize's levels are set per block: an `exp2` per sample would
        // cost more than everything else here.
        let quantize = if W == WARP_QUANTIZE {
            quantize_levels(warp)
        } else {
            0.0
        };
        let quantize2 = if SECOND && mode2 == WARP_QUANTIZE {
            quantize_levels(warp2)
        } else {
            0.0
        };
        let mut phase = self.phase[u];
        let mut inc = self.inc[u];
        let inc_step = self.inc_step[u];
        let mut gain_l = self.gain_l[u];
        let mut gain_r = self.gain_r[u];
        let (gain_l_step, gain_r_step) = (self.gain_l_step[u], self.gain_r_step[u]);
        let (mut frame, frame_step) = positions;
        let len = left.len().min(right.len()).min(2 * BLOCK);
        // Which frames to read and where they start, worked out once when
        // the position holds still (usual), and per sample when it moves.
        let moving = frame_step != 0.0;
        let mut i = (frame as usize).min(last_frame);
        let mut t = frame - i as f32;
        let mut j = (i + 1).min(last_frame);
        let mut at = view.frames(i, j);
        let mut old_at = old.frames(i, j);
        for n in 0..len {
            let m = modulation[n];
            if moving {
                i = (frame as usize).min(last_frame);
                t = frame - i as f32;
                j = (i + 1).min(last_frame);
                at = view.frames(i, j);
                old_at = old.frames(i, j);
            }
            let sample = |p: f32| {
                let x = view.read_at(at, t, p);
                if FADE {
                    let y = old.read_at(old_at, t, p);
                    let w = ((fade_start + n + 1) as f32 / BLOCK as f32).min(1.0);
                    y + (x - y) * w
                } else {
                    x
                }
            };
            let x = if SECOND {
                let first = |p: f32| first_warp::<W>(p, warp, m, quantize, remap, sample);
                second_warp(mode2, phase, warp2, m, quantize2, remap, first)
            } else {
                let mut x = sample(warp_phase::<W>(phase, warp, m, remap));
                if W == WARP_MIRROR {
                    x += (sample(mirror(phase)) - x) * warp;
                }
                warp_output::<W>(x, phase, warp, m, quantize)
            };
            if let Some(store) = store.as_deref_mut() {
                store[n] = x;
            }
            left[n] += x * gain_l;
            right[n] += x * gain_r;
            phase += inc;
            if phase >= 1.0 {
                phase -= 1.0;
            }
            inc += inc_step;
            gain_l += gain_l_step;
            gain_r += gain_r_step;
            frame += frame_step;
            warp += warp_step;
            warp2 += warp2_step;
        }
        self.phase[u] = phase;
        self.inc[u] = inc;
        self.gain_l[u] = gain_l;
        self.gain_r[u] = gain_r;
        // Worked out again rather than taken from the loop, so the loop's
        // ramps are dead code for warps that don't read their amount.
        for slot in 0..2 {
            let end = self.warp[slot][u] + self.warp_step[slot][u] * len as f32;
            self.warp[slot][u] = end.clamp(0.0, 1.0);
        }
        self.fade[u] = (fade_start + len).min(BLOCK) as u8;
    }
}

/// One band-limit level of a table, ready for reading frames.
#[derive(Clone, Copy)]
struct View<'a> {
    data: &'a [f32],
    size: f32,
    mask: usize,
    stride: usize,
}

impl<'a> View<'a> {
    fn new(level: &'a Level) -> Self {
        Self {
            data: &level.data,
            size: level.size as f32,
            mask: level.mask,
            stride: level.stride,
        }
    }

    /// Where frames `i` and `j` start in the data, for [`View::read_at`].
    #[inline(always)]
    fn frames(&self, i: usize, j: usize) -> (usize, usize) {
        (i * self.stride, j * self.stride)
    }

    /// Phase `p` of the two frames starting at `at` (see [`View::frames`]),
    /// mixed `t` of the way to the second.
    #[inline(always)]
    fn read_at(&self, at: (usize, usize), t: f32, p: f32) -> f32 {
        let x = p * self.size;
        let k = x as usize;
        let f = x - k as f32;
        let k = k & self.mask;
        let base_a = at.0 + k;
        let a0 = self.data[base_a];
        let a1 = self.data[base_a + 1];
        let a = a0 + (a1 - a0) * f;
        if t == 0.0 {
            // On a frame exactly: the next one would be weighed by zero,
            // which (the tables being finite) leaves `a` as it is.
            return a;
        }
        let base_b = at.1 + k;
        let b0 = self.data[base_b];
        let b1 = self.data[base_b + 1];
        let b = b0 + (b1 - b0) * f;
        a + (b - a) * t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_sin_is_close() {
        for i in -200..200 {
            let p = i as f32 / 97.0;
            let error = (fast_sin(p) - (TAU * p).sin()).abs();
            assert!(error < 3e-4, "{p}: {error}");
        }
    }

    #[test]
    fn phase_warps_stay_inside_the_cycle() {
        // A straight remap curve and a bent one.
        let bent: Remap = std::array::from_fn(|i| (i as f32 / REMAP_SIZE as f32).powi(2));
        for r in [&STRAIGHT_REMAP, &bent] {
            phase_warps_stay_inside_the_cycle_with(r);
        }
    }

    fn phase_warps_stay_inside_the_cycle_with(r: &Remap) {
        for i in 0..1000 {
            let p = i as f32 / 1000.0;
            for a in [0.0, 0.3, 1.0] {
                for m in [-1.0, 0.0, 1.0] {
                    for q in [
                        warp_phase::<WARP_SYNC>(p, a, m, r),
                        warp_phase::<WARP_BEND_POS>(p, a, m, r),
                        warp_phase::<WARP_BEND_NEG>(p, a, m, r),
                        warp_phase::<WARP_BEND_BI>(p, a, m, r),
                        warp_phase::<WARP_PWM>(p, a, m, r),
                        warp_phase::<WARP_ASYM_POS>(p, a, m, r),
                        warp_phase::<WARP_ASYM_NEG>(p, a, m, r),
                        warp_phase::<WARP_ASYM_BI>(p, a, m, r),
                        warp_phase::<WARP_SQUEEZE>(p, a, m, r),
                        warp_phase::<WARP_FM>(p, a, m, r),
                        mirror(p),
                    ] {
                        assert!((0.0..1.0).contains(&q), "{p} {a} {m}: {q}");
                    }
                    for mode in 0..WARP_COUNT {
                        let q = warp_phase_dyn(mode, p, a, m, r);
                        assert!((0.0..1.0).contains(&q), "mode {mode} {p} {a} {m}: {q}");
                    }
                }
            }
        }
    }

    #[test]
    fn remap_warps_read_where_the_curve_says() {
        let bent: Remap = std::array::from_fn(|i| (i as f32 / REMAP_SIZE as f32).powi(2));
        for i in 0..200 {
            let p = i as f32 / 200.0;
            for mode in [WARP_REMAP, WARP_REMAP_MIRROR, WARP_REMAP_X4] {
                // Amount 0 reads straight, whatever the curve; a straight
                // curve reads straight at any amount.
                let q = warp_phase_dyn(mode, p, 0.0, 0.0, &bent);
                assert!((q - p).abs() < 1e-5, "mode {mode} at {p}: {q}");
                let q = warp_phase_dyn(mode, p, 1.0, 0.0, &STRAIGHT_REMAP);
                assert!((q - p).abs() < 1e-4, "mode {mode} at {p}: {q}");
            }
            let q = warp_phase_dyn(WARP_REMAP, p, 1.0, 0.0, &bent);
            assert!((q - p * p).abs() < 1e-4, "{p}: {q}");
            // Halfway, halfway between.
            let q = warp_phase_dyn(WARP_REMAP, p, 0.5, 0.0, &bent);
            assert!((q - 0.5 * (p + p * p)).abs() < 1e-4, "{p}: {q}");
        }
        // Remap 2's second half mirrors its first; Remap 4 is Remap 2 on
        // each half.
        for i in 1..100 {
            let p = i as f32 / 200.0;
            let first = warp_phase_dyn(WARP_REMAP_MIRROR, p, 1.0, 0.0, &bent);
            let second = warp_phase_dyn(WARP_REMAP_MIRROR, 1.0 - p, 1.0, 0.0, &bent);
            assert!((first + second - 1.0).abs() < 1e-4, "{p}: {first} {second}");
            let p = i as f32 / 400.0;
            let quarter = warp_phase_dyn(WARP_REMAP_X4, p, 1.0, 0.0, &bent);
            let third = warp_phase_dyn(WARP_REMAP_X4, p + 0.5, 1.0, 0.0, &bent);
            assert!(
                (third - quarter - 0.5).abs() < 1e-4,
                "{p}: {quarter} {third}"
            );
            let second = warp_phase_dyn(WARP_REMAP_X4, 0.5 - p, 1.0, 0.0, &bent);
            assert!(
                (quarter + second - 0.5).abs() < 1e-4,
                "{p}: {quarter} {second}"
            );
            let curve = 0.25 * (4.0 * p) * (4.0 * p);
            assert!((quarter - curve).abs() < 1e-3, "{p}: {quarter} {curve}");
        }
        // The band limit knows how fast the curve reads at most.
        assert!((remap_slope(&STRAIGHT_REMAP) - 1.0).abs() < 1e-3);
        assert!((remap_slope(&bent) - 2.0).abs() < 0.02);
        assert_eq!(remap_speed(0.0, 2.0), 1.0);
        assert!(remap_speed(1.0, 2.0) > remap_speed(0.5, 2.0));
        assert_eq!(remap_speed(1.0, 1.0), 1.0);
    }

    #[test]
    fn either_slot_alone_warps_the_same() {
        let table = &super::super::tables::bank().tables[0];
        for mode in 0..WARP_COUNT {
            for i in 0..64 {
                let p = i as f32 / 64.0;
                let first = warped_sample(
                    table,
                    0.5,
                    [(mode, 0.6), (WARP_OFF, 0.0)],
                    &STRAIGHT_REMAP,
                    p,
                    0.3,
                );
                let second = warped_sample(
                    table,
                    0.5,
                    [(WARP_OFF, 0.0), (mode, 0.6)],
                    &STRAIGHT_REMAP,
                    p,
                    0.3,
                );
                assert!((first - second).abs() < 1e-6, "mode {mode} at {p}");
            }
        }
    }

    #[test]
    fn bipolar_warps_rest_in_the_middle_and_reach_both_ends() {
        let table = &super::super::tables::bank().tables[0];
        let at = |mode: usize, a: f32, p: f32| {
            warped_sample(
                table,
                0.6,
                [(mode, a), (WARP_OFF, 0.0)],
                &STRAIGHT_REMAP,
                p,
                0.0,
            )
        };
        for i in 0..64 {
            let p = i as f32 / 64.0;
            let plain = at(WARP_OFF, 0.0, p);
            assert!((at(WARP_BEND_BI, 0.5, p) - plain).abs() < 1e-6);
            assert!((at(WARP_ASYM_BI, 0.5, p) - plain).abs() < 1e-6);
            assert!((at(WARP_BEND_BI, 1.0, p) - at(WARP_BEND_POS, 1.0, p)).abs() < 1e-5);
            assert!((at(WARP_BEND_BI, 0.0, p) - at(WARP_BEND_NEG, 1.0, p)).abs() < 1e-5);
            assert!((at(WARP_ASYM_BI, 0.0, p) - at(WARP_ASYM_NEG, 1.0, p)).abs() < 1e-5);
            // Nothing changes at amount 0 for the one-way warps.
            for mode in [WARP_FOLD, WARP_DRIVE, WARP_SQUEEZE] {
                assert!((at(mode, 0.0, p) - plain).abs() < 1e-6, "mode {mode}");
            }
        }
    }

    #[test]
    fn fold_reflects_and_drive_saturates() {
        assert_eq!(fold(0.5), 0.5);
        assert!((fold(1.5) - 0.5).abs() < 1e-6);
        assert!((fold(-1.5) + 0.5).abs() < 1e-6);
        assert!((fold(3.0) + 1.0).abs() < 1e-6);
        for i in -40..=40 {
            let v = i as f32 * 0.37;
            assert!((-1.0..=1.0).contains(&fold(v)), "{v}");
        }
        // Driven hard, a sine flattens toward a square, peaking at 1.
        let driven = warp_output::<WARP_DRIVE>(0.3, 0.0, 1.0, 0.0, 0.0);
        assert!(driven > 0.9 && driven <= 1.0, "{driven}");
        assert!((warp_output::<WARP_DRIVE>(1.0, 0.0, 1.0, 0.0, 0.0) - 1.0).abs() < 1e-5);
    }
}
