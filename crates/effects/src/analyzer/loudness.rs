//! Loudness and peak measurement for the analyzer: ITU-R BS.1770-4 (the
//! K-weighting filter and gated integrated loudness), EBU Tech 3342
//! (loudness range) and the true-peak meter of BS.1770-4 Annex 2.

/// Loudness of K-weighted mean square `power`, in LUFS (BS.1770's formula
/// for two channels with unit weights: `-0.691 + 10·log10(Σ zᵢ)`).
pub(crate) fn lufs(power: f64) -> f64 {
    -0.691 + 10.0 * power.max(1e-30).log10()
}

/// The power whose loudness is `lufs`.
#[cfg(test)]
pub(crate) fn power(lufs: f64) -> f64 {
    10f64.powf((lufs + 0.691) / 10.0)
}

/// A biquad's coefficients (`a0` = 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Biquad {
    pub b: [f64; 3],
    pub a: [f64; 2],
}

/// The two stages of BS.1770's K-weighting at `sample_rate`: a high shelf
/// (+4 dB above about 2 kHz, the head's effect) and a high-pass (the "RLB"
/// curve, falling below about 60 Hz). The standard gives coefficients for
/// 48 kHz only; these come from the analog prototypes they were made from
/// (as in libebur128), so they match it at 48 kHz and keep the same curve
/// at other rates.
pub(crate) fn k_weighting(sample_rate: f64) -> [Biquad; 2] {
    use std::f64::consts::PI;

    let f0 = 1_681.974_450_955_533;
    let gain_db = 3.999_843_853_973_347;
    let q = 0.707_175_236_955_419_6;
    let k = (PI * f0 / sample_rate).tan();
    let vh = 10f64.powf(gain_db / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };

    let f0 = 38.135_470_876_024_44;
    let q = 0.500_327_037_323_877_3;
    let k = (PI * f0 / sample_rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let high_pass = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    [shelf, high_pass]
}

/// A biquad's memory (transposed direct form II).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct BiquadState {
    s1: f64,
    s2: f64,
}

impl BiquadState {
    #[inline]
    pub fn tick(&mut self, c: &Biquad, x: f64) -> f64 {
        let y = c.b[0] * x + self.s1;
        self.s1 = c.b[1] * x - c.a[0] * y + self.s2;
        self.s2 = c.b[2] * x - c.a[1] * y;
        y
    }

    /// Ends a decay in exact zeros rather than denormals.
    pub fn flush(&mut self) {
        for s in [&mut self.s1, &mut self.s2] {
            if s.abs() < 1e-30 {
                *s = 0.0;
            }
        }
    }
}

/// Blocks quieter than this don't count towards integrated loudness or
/// loudness range at all (the absolute gate).
pub(crate) const ABSOLUTE_GATE: f64 = -70.0;
/// Histogram bins per LU: block loudness is kept to a tenth of a LU.
const BINS_PER_LU: f64 = 10.0;
/// From the absolute gate up to +30 LUFS.
pub(crate) const BINS: usize = 1000;

/// Loudness values of blocks above the absolute gate, kept as a histogram
/// (how many blocks, and their summed power, per tenth of a LU), so that
/// hours of measurement take a fixed few kilobytes and no allocation. The
/// power sums make the gated means exact; only blocks within a tenth of a
/// LU of the relative gate are counted in or out as a group.
#[derive(Debug, Clone)]
pub(crate) struct Gate {
    counts: [u32; BINS],
    powers: [f64; BINS],
    total: f64,
    count: u64,
}

impl Gate {
    pub fn new() -> Gate {
        Gate {
            counts: [0; BINS],
            powers: [0.0; BINS],
            total: 0.0,
            count: 0,
        }
    }

    pub fn clear(&mut self) {
        self.counts.fill(0);
        self.powers.fill(0.0);
        self.total = 0.0;
        self.count = 0;
    }

    fn bin(lufs: f64) -> usize {
        (((lufs - ABSOLUTE_GATE) * BINS_PER_LU).floor().max(0.0) as usize).min(BINS - 1)
    }

    fn centre(bin: usize) -> f64 {
        ABSOLUTE_GATE + (bin as f64 + 0.5) / BINS_PER_LU
    }

    /// Adds a block of mean square `power`, unless it is below the
    /// absolute gate.
    pub fn add(&mut self, power: f64) {
        let loudness = lufs(power);
        // (A NaN power is left out too.)
        if loudness.is_nan() || loudness < ABSOLUTE_GATE {
            return;
        }
        let bin = Gate::bin(loudness);
        self.counts[bin] += 1;
        self.powers[bin] += power;
        self.total += power;
        self.count += 1;
    }

    /// The first bin at or above `offset` LU relative to the mean power of
    /// every block above the absolute gate.
    fn relative_gate(&self, offset: f64) -> usize {
        Gate::bin(lufs(self.total / self.count as f64) + offset)
    }

    /// Integrated loudness (BS.1770-4): the mean power of the blocks no
    /// more than 10 LU below the mean of all blocks above the absolute gate.
    pub fn integrated(&self) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        let start = self.relative_gate(-10.0);
        let (mut sum, mut count) = (0.0, 0u64);
        for bin in start..BINS {
            sum += self.powers[bin];
            count += u64::from(self.counts[bin]);
        }
        (count > 0).then(|| lufs(sum / count as f64))
    }

    /// Loudness range (EBU Tech 3342) of short-term values: from the 10th
    /// to the 95th percentile of those no more than 20 LU below the mean.
    pub fn range(&self) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        let start = self.relative_gate(-20.0);
        let count: u64 = self.counts[start..].iter().map(|&c| u64::from(c)).sum();
        if count == 0 {
            return None;
        }
        let percentile = |share: f64| {
            let wanted = (share * (count - 1) as f64).round() as u64;
            let mut seen = 0u64;
            for bin in start..BINS {
                seen += u64::from(self.counts[bin]);
                if seen > wanted {
                    return Gate::centre(bin);
                }
            }
            Gate::centre(BINS - 1)
        };
        Some(percentile(0.95) - percentile(0.10))
    }
}

/// Taps per phase of the true-peak interpolator: how many samples back
/// each interpolated point still depends on.
pub(crate) const TAPS: usize = 32;
/// Where in its window the interpolator places the sample it interpolates
/// after: its delay, in samples.
const CENTRE: usize = TAPS / 2 - 1;
/// The window's shape: higher is less ripple but a wider transition band.
const KAISER_BETA: f64 = 5.5;

/// Four-times oversampling for true peak (BS.1770-4 Annex 2): the three
/// in-between points of each sample, from windowed-sinc interpolation over
/// 32 samples. Passes up to about 0.45 of the sample rate (21.6 kHz at
/// 48 kHz) and has each phase's gain exactly 1 at DC.
#[derive(Debug, Clone)]
pub(crate) struct TruePeak {
    phases: [[f32; TAPS]; 3],
    /// Per channel: the last `TAPS` samples, twice over, so a window of
    /// them is always one contiguous slice.
    history: [[f32; 2 * TAPS]; 2],
    position: usize,
}

/// The modified Bessel function I₀, by its series.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..50 {
        term *= (x / (2.0 * k as f64)).powi(2);
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}

impl TruePeak {
    pub fn new() -> TruePeak {
        let mut phases = [[0.0; TAPS]; 3];
        let half = (TAPS / 2) as f64;
        for (p, phase) in phases.iter_mut().enumerate() {
            let fraction = (p + 1) as f64 / 4.0;
            let mut taps = [0.0f64; TAPS];
            for (t, tap) in taps.iter_mut().enumerate() {
                // The tap for the sample `offset` samples before the
                // interpolated point.
                let offset = CENTRE as f64 - t as f64 + fraction;
                let x = std::f64::consts::PI * offset;
                let sinc = if x == 0.0 { 1.0 } else { x.sin() / x };
                let r = (offset / half).clamp(-1.0, 1.0);
                *tap =
                    sinc * bessel_i0(KAISER_BETA * (1.0 - r * r).sqrt()) / bessel_i0(KAISER_BETA);
            }
            let sum: f64 = taps.iter().sum();
            for (out, tap) in phase.iter_mut().zip(taps) {
                *out = (tap / sum) as f32;
            }
        }
        TruePeak {
            phases,
            history: [[0.0; 2 * TAPS]; 2],
            position: 0,
        }
    }

    pub fn clear(&mut self) {
        self.history = [[0.0; 2 * TAPS]; 2];
        self.position = 0;
    }

    /// Takes one sample per channel; returns, per channel, the largest
    /// absolute value among that sample and the three points interpolated
    /// after the sample `CENTRE` samples back.
    #[inline]
    pub fn push(&mut self, left: f32, right: f32) -> [f32; 2] {
        let position = self.position;
        let mut peaks = [left.abs(), right.abs()];
        for (channel, x) in [left, right].into_iter().enumerate() {
            let history = &mut self.history[channel];
            history[position] = x;
            history[position + TAPS] = x;
            // Oldest first.
            let window = &history[position + 1..position + 1 + TAPS];
            for phase in &self.phases {
                let y: f32 = window.iter().zip(phase).map(|(a, b)| a * b).sum();
                peaks[channel] = peaks[channel].max(y.abs());
            }
        }
        self.position = (position + 1) % TAPS;
        peaks
    }

    /// Ends a decay in exact zeros rather than denormals.
    pub fn flush(&mut self) {
        for x in self.history.iter_mut().flatten() {
            if x.abs() < 1e-30 {
                *x = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn k_weighting_matches_the_standard_at_48k() {
        // BS.1770-4, tables 1 and 2.
        let [shelf, high_pass] = k_weighting(48_000.0);
        let close = |a: f64, b: f64| (a - b).abs() < 1e-10;
        assert!(close(shelf.b[0], 1.535_124_859_586_97), "{shelf:?}");
        assert!(close(shelf.b[1], -2.691_696_189_406_38));
        assert!(close(shelf.b[2], 1.198_392_810_852_85));
        assert!(close(shelf.a[0], -1.690_659_293_182_41));
        assert!(close(shelf.a[1], 0.732_480_774_215_85));
        assert_eq!(high_pass.b, [1.0, -2.0, 1.0]);
        assert!(
            close(high_pass.a[0], -1.990_047_454_833_98),
            "{high_pass:?}"
        );
        assert!(close(high_pass.a[1], 0.990_072_250_366_21));
    }

    #[test]
    fn the_gate_keeps_tenths_of_a_lu() {
        let mut gate = Gate::new();
        assert_eq!(gate.integrated(), None);
        gate.add(power(-80.0));
        assert_eq!(gate.integrated(), None, "below the absolute gate");
        for _ in 0..10 {
            gate.add(power(-23.0));
        }
        for _ in 0..10 {
            gate.add(power(-40.0));
        }
        let integrated = gate.integrated().unwrap();
        assert!((integrated + 23.0).abs() < 1e-9, "{integrated}");
        let range = gate.range().unwrap();
        assert!((range - 17.0).abs() < 0.11, "{range}");
    }
}
