//! What the dynamics effects share: decibels as powers of two, envelope
//! time constants, and the compressor's static curve.

/// `20 · log10(x) = DB_PER_LOG2 · log2(x)`.
pub(crate) const DB_PER_LOG2: f32 = 6.020_6;

/// A one-pole smoothing coefficient for a time constant of `ms`, for a
/// filter updated `rate` times a second.
pub(crate) fn coefficient(ms: f32, rate: f32) -> f32 {
    (-1.0 / (ms * 0.001 * rate)).exp()
}

/// The reduction in dB the static curve calls for at `level_db`: none below
/// the knee, `slope` (`1 - 1/ratio`) of the excess above it, and a quadratic
/// blend across the `knee` dB around the threshold.
#[inline]
pub(crate) fn static_reduction(level_db: f32, threshold: f32, knee: f32, slope: f32) -> f32 {
    let over = level_db - threshold;
    if 2.0 * over >= knee {
        slope * over
    } else if 2.0 * over <= -knee {
        0.0
    } else {
        let into = over + 0.5 * knee;
        slope * into * into / (2.0 * knee)
    }
}
