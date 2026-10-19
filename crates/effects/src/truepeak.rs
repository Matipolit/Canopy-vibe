//! True-peak detection for the limiter: how loud a signal gets between its
//! samples once a converter reconstructs it, which can be several dB above
//! the samples themselves (a sine at a quarter of the sample rate, sampled
//! 45° off its peaks, reads 3 dB low).
//!
//! Each new sample completes the stretch between the samples [`DELAY`] and
//! `DELAY - 1` ago. Three points inside it are interpolated (4× oversampling,
//! as ITU-R BS.1770 meters do) with a Kaiser-windowed sinc of [`TAPS`]
//! samples, and wherever the oversampled signal turns, a parabola through
//! the three points around the turn estimates the peak between them, which
//! takes the oversampling's own under-reading from up to 0.7 dB near Nyquist
//! down to a few hundredths. The stretch's peak is the largest of its two
//! samples, the three points and those estimates, over both channels.

/// Samples the interpolator looks at, centred on the stretch measured.
pub(crate) const TAPS: usize = 24;
/// How far behind the newest sample the stretch measured starts.
pub(crate) const DELAY: usize = TAPS / 2;
/// Points interpolated inside each stretch (4× oversampling).
const POINTS: usize = 3;
/// The Kaiser window's shape: low enough that the interpolation stays
/// accurate close to Nyquist, where inter-sample peaks are largest.
const BETA: f64 = 6.0;

/// The modified Bessel function of the first kind, order zero, which
/// shapes the Kaiser window.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..50 {
        term *= half / k as f64;
        sum += term * term;
        if term * term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// The height of the parabola through three equally spaced points if the
/// middle one is a turn of the signal (a peak or a trough), otherwise just
/// the middle point's magnitude.
#[inline]
pub(crate) fn turn_height(before: f32, middle: f32, after: f32) -> f32 {
    let curvature = before - 2.0 * middle + after;
    let turns = if middle > 0.0 {
        middle >= before && middle >= after && curvature < 0.0
    } else {
        middle <= before && middle <= after && curvature > 0.0
    };
    if turns {
        let slope = after - before;
        (middle - slope * slope / (8.0 * curvature)).abs()
    } else {
        middle.abs()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TruePeak {
    /// The interpolation filters for the points 1/4, 2/4 and 3/4 of the
    /// way through the stretch, oldest sample first.
    kernels: [[f32; TAPS]; POINTS],
    /// Each channel's last [`TAPS`] samples, written twice so that they are
    /// in order from `position` on without wrapping.
    history: [[f32; 2 * TAPS]; 2],
    position: usize,
    /// Each channel's last interpolated point of the previous stretch, the
    /// neighbour of this stretch's first sample.
    previous: [f32; 2],
}

impl TruePeak {
    pub fn new() -> TruePeak {
        let half = TAPS as f64 / 2.0;
        let mut kernels = [[0.0; TAPS]; POINTS];
        for (point, kernel) in kernels.iter_mut().enumerate() {
            let fraction = (point + 1) as f64 / (POINTS + 1) as f64;
            let mut taps = [0.0f64; TAPS];
            for (tap, value) in taps.iter_mut().enumerate() {
                // How far this tap's sample is from the point, in samples.
                let t = (TAPS - 1 - DELAY) as f64 - tap as f64 + fraction;
                let sinc = if t == 0.0 {
                    1.0
                } else {
                    (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
                };
                let edge = (1.0 - (t / half).powi(2)).max(0.0);
                *value = sinc * bessel_i0(BETA * edge.sqrt()) / bessel_i0(BETA);
            }
            // Unity gain at DC.
            let sum: f64 = taps.iter().sum();
            for (out, value) in kernel.iter_mut().zip(taps) {
                *out = (value / sum) as f32;
            }
        }
        TruePeak {
            kernels,
            history: [[0.0; 2 * TAPS]; 2],
            position: 0,
            previous: [0.0; 2],
        }
    }

    pub fn reset(&mut self) {
        self.history = [[0.0; 2 * TAPS]; 2];
        self.position = 0;
        self.previous = [0.0; 2];
    }

    /// Takes the next stereo sample and returns the peak magnitude of the
    /// stretch it completes: between its samples too if `between`, else
    /// only at them.
    #[inline]
    pub fn push(&mut self, left: f32, right: f32, between: bool) -> f32 {
        let at = self.position;
        for (history, sample) in self.history.iter_mut().zip([left, right]) {
            history[at] = sample;
            history[at + TAPS] = sample;
        }
        self.position = if at + 1 == TAPS { 0 } else { at + 1 };
        let start = self.position;

        let mut peak = 0.0f32;
        for (history, previous) in self.history.iter().zip(&mut self.previous) {
            let window: &[f32; TAPS] = history[start..start + TAPS].try_into().unwrap();
            let first = window[TAPS - 1 - DELAY];
            let last = window[TAPS - DELAY];
            peak = peak.max(first.abs()).max(last.abs());
            if !between {
                *previous = 0.0;
                continue;
            }
            let mut points = [*previous, first, 0.0, 0.0, 0.0, last];
            for (point, kernel) in points[2..2 + POINTS].iter_mut().zip(&self.kernels) {
                *point = dot(window, kernel);
            }
            *previous = points[POINTS + 1];
            for turn in points.windows(3).take(POINTS + 1) {
                peak = peak.max(turn_height(turn[0], turn[1], turn[2]));
            }
        }
        peak
    }
}

/// `Σ a·b`, in four running sums so the additions can overlap.
#[inline]
fn dot(a: &[f32; TAPS], b: &[f32; TAPS]) -> f32 {
    let mut sums = [0.0f32; 4];
    for (a, b) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for lane in 0..4 {
            sums[lane] += a[lane] * b[lane];
        }
    }
    (sums[0] + sums[1]) + (sums[2] + sums[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The detector's reading, in dB relative to the true amplitude, for
    /// sines at `ratio` of the sample rate, over many phases: the worst
    /// under-reading and the worst over-reading.
    fn reading_range(ratio: f64) -> (f64, f64) {
        let mut detector = TruePeak::new();
        let (mut lowest, mut highest) = (f64::INFINITY, f64::NEG_INFINITY);
        for phase in 0..16 {
            detector.reset();
            let offset = phase as f64 / 16.0 * std::f64::consts::TAU;
            let mut loudest = 0.0f32;
            for n in 0..2000 {
                let s = (std::f64::consts::TAU * ratio * n as f64 + offset).sin() as f32;
                let peak = detector.push(s, 0.0, true);
                if n > 200 {
                    loudest = loudest.max(peak);
                }
            }
            let reading = 20.0 * f64::from(loudest).log10();
            lowest = lowest.min(reading);
            highest = highest.max(reading);
        }
        (lowest, highest)
    }

    #[test]
    fn reads_the_peaks_between_samples() {
        let mut worst = (0.0f64, 0.0f64);
        for ratio in [0.01, 0.05, 0.1, 0.17, 0.25, 0.3, 0.35, 0.4, 0.43, 0.45] {
            let (low, high) = reading_range(ratio);
            println!("sine at {ratio} fs reads {low:+.4} to {high:+.4} dB");
            worst = (worst.0.min(low), worst.1.max(high));
        }
        println!(
            "true-peak reading over 0.01-0.45 fs: {:+.4} to {:+.4} dB",
            worst.0, worst.1
        );
        assert!(worst.0 > -0.05 && worst.1 < 0.05, "{worst:?}");

        // Sample peaks alone miss the classic 3 dB inter-sample over.
        let mut detector = TruePeak::new();
        let mut samples_only = 0.0f32;
        for n in 0..200 {
            let s = (std::f64::consts::FRAC_PI_2 * n as f64 + std::f64::consts::FRAC_PI_4).sin();
            samples_only = samples_only.max(detector.push(s as f32, 0.0, false));
        }
        assert!((samples_only - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }
}
