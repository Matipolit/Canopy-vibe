mod envelope;
mod gain;
pub mod noise;
pub mod one_pole;
mod oscillator;
pub mod simd;
mod smoothed;
pub mod svf;

pub use envelope::{Adsr, AdsrDesc, Segment, StageDesc, curve_coefficients, stage_shape};
pub use gain::Gain;
pub use noise::xorshift32;
pub use one_pole::DcBlocker;
pub use oscillator::{Oscillator, Waveform, poly_blep};
pub use smoothed::{MAX_BLOCK, Pace, Smoothed};
pub use svf::{FilterMode, Svf, SvfOutput};

/// Values in a feedback path smaller than this are flushed to zero, so a tail
/// ends in exact silence instead of in denormal numbers, which are very slow
/// on most CPUs. About -400 dB: far below anything audible.
const TINY: f32 = 1e-20;

/// `x`, or zero if it is tiny. NaN is flushed too, so one bad sample can't
/// poison a feedback loop forever.
#[inline]
pub fn flush(x: f32) -> f32 {
    if x.abs() >= TINY { x } else { 0.0 }
}

/// [`flush`], and zero for infinity as well: for filter states at the end of
/// a block, so nothing that came in with it stays stuck in them.
#[inline]
pub fn settle(x: f32) -> f32 {
    if x.is_finite() { flush(x) } else { 0.0 }
}

/// Catmull-Rom interpolation between `x1` and `x2`, `t` of the way, with
/// `x0` and `x3` the samples either side: a cubic through all four that
/// keeps the slope continuous from one interval to the next.
#[inline(always)]
pub fn catmull_rom(x0: f32, x1: f32, x2: f32, x3: f32, t: f32) -> f32 {
    let c1 = 0.5 * (x2 - x0);
    let c2 = x0 - 2.5 * x1 + 2.0 * x2 - 0.5 * x3;
    let c3 = 0.5 * (x3 - x0) + 1.5 * (x1 - x2);
    ((c3 * t + c2) * t + c1) * t + x1
}

/// The frequency of MIDI note `note` (fractional for detuned or bent
/// notes), A4 = 69 at 440 Hz, in equal temperament.
#[inline]
pub fn note_hz(note: f32) -> f32 {
    440.0 * ((note - 69.0) / 12.0).exp2()
}

/// A rational approximation of `tanh`, `x·(27 + x²) / (27 + 9x²)`: within
/// a fraction of a percent of it, exact at 0, and reaching ±1 at ±3 with
/// zero slope, where it is clamped, so it is smooth everywhere and never
/// exceeds 1. Far cheaper than `tanh`, for saturation per sample per voice.
#[inline(always)]
pub fn rational_tanh(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

/// `x` split into its whole part, as an index, and that part as a float:
/// `(x as usize, (x as usize) as f32)` for a position into a table or a
/// delay line, `0..2³¹`, and 0 for a negative or NaN `x`, as `as usize`
/// gives. A float has no single instruction to become a `usize` or come
/// back from one on x86, so those casts compile to a dozen instructions
/// with branches; through `i32` each is one, which matters per sample.
#[inline(always)]
pub fn split_index(x: f32) -> (usize, f32) {
    let whole = (x as i32).max(0);
    (whole as usize, whole as f32)
}

/// A stereo effect that processes audio in place.
pub trait Processor: Send {
    /// Called before audio starts and whenever the sample rate changes.
    /// This is where delay lines and other buffers get allocated.
    fn prepare(&mut self, sample_rate: f32);

    /// Clear internal state (filter memory, delay lines) without reallocating.
    fn reset(&mut self) {}

    /// Process one block. `left` and `right` always have the same length.
    /// Must be real-time safe.
    fn process(&mut self, left: &mut [f32], right: &mut [f32]);
}

/// Converts decibels to a linear amplitude factor.
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Converts a linear amplitude factor to decibels. Silence maps to -180 dB
/// rather than negative infinity, so the result is always safe to do math on.
pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.abs().max(1e-9).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_round_trip() {
        for db in [-60.0, -12.0, -6.0, 0.0, 6.0] {
            assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-4);
        }
        assert!((db_to_gain(-6.0) - 0.501).abs() < 1e-3);
        assert_eq!(gain_to_db(0.0), -180.0);
    }

    #[test]
    fn rational_tanh_is_smooth_bounded_and_odd() {
        // Near ±3 the curve is flat to within rounding of the last bit.
        let ulp = 1e-6;
        let mut previous = rational_tanh(-4.0);
        assert_eq!(previous, -1.0);
        for i in -399..=400 {
            let x = i as f32 * 0.01;
            let y = rational_tanh(x);
            assert!(y >= previous - ulp, "monotonic at {x}");
            assert!(y.abs() <= 1.0 + ulp);
            assert_eq!(rational_tanh(-x), -y);
            previous = y;
        }
        assert_eq!(rational_tanh(3.0), 1.0);
    }

    #[test]
    fn catmull_rom_passes_through_its_points_and_lines() {
        assert_eq!(catmull_rom(0.0, 1.0, 2.0, 3.0, 0.0), 1.0);
        assert!((catmull_rom(0.0, 1.0, 2.0, 3.0, 0.25) - 1.25).abs() < 1e-6);
        assert!((catmull_rom(4.0, 3.0, 2.0, 1.0, 0.5) - 2.5).abs() < 1e-6);
    }

    #[test]
    fn flush_and_settle_end_decays_and_bad_values_in_zeros() {
        assert!(flush(1e-30) == 0.0 && flush(f32::NAN) == 0.0 && flush(0.5) == 0.5);
        assert_eq!(flush(f32::INFINITY), f32::INFINITY);
        assert!(settle(f32::INFINITY) == 0.0 && settle(f32::NAN) == 0.0 && settle(-0.5) == -0.5);
    }
}
