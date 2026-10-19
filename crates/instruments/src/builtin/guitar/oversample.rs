//! Half-band filters to run the amp's waveshaper at a higher sample rate.
//!
//! A waveshaper adds harmonics far above the input's highest frequency;
//! those above half the sample rate fold back down as inharmonic aliases,
//! the fizz of cheap digital distortion. Shaping at four times the rate
//! leaves room for the harmonics, which the down-sampling filters then
//! remove.
//!
//! The filters only need to keep aliases out of the band the cabinet passes
//! (a few kHz), so a short windowed-sinc half-band is enough. Half of a
//! half-band's taps are zero, and the polyphase forms below skip them.

use std::f64::consts::PI;

/// Taps of the half-band filter. `4k + 3`, so the center tap is odd and the
/// zero taps fall on even offsets from it. 31 puts the stopband at about
/// 0.38 of the high rate, so what still aliases lands high in the low
/// rate's band, where the cabinet has already rolled off.
const TAPS: usize = 31;
const CENTER: usize = (TAPS - 1) / 2;
/// Distinct non-zero coefficients besides the center one (they come in
/// symmetric pairs).
const PAIRS: usize = TAPS.div_ceil(4);
/// Low-rate samples the non-zero taps span.
const SPAN: usize = 2 * PAIRS;
/// The center tap's position, in low-rate samples.
const DELAY: usize = (CENTER - 1) / 2;

/// The non-zero coefficients below the center, `h[0], h[2], ..., h[CENTER - 1]`,
/// of a half-band lowpass windowed with a four-term Blackman-Harris window.
/// Scaled so the filter passes DC at unity (the center tap is 0.5).
fn coefficients() -> [f32; PAIRS] {
    let mut h = [0.0f64; PAIRS];
    for (j, h) in h.iter_mut().enumerate() {
        let k = 2 * j;
        let x = (k as f64 - CENTER as f64) / 2.0;
        let sinc = (PI * x).sin() / (PI * x);
        let phase = 2.0 * PI * k as f64 / (TAPS - 1) as f64;
        let window = 0.35875 - 0.48829 * phase.cos() + 0.14128 * (2.0 * phase).cos()
            - 0.01168 * (3.0 * phase).cos();
        *h = 0.5 * sinc * window;
    }
    // Each coefficient appears twice; together they must sum to 0.5.
    let sum: f64 = h.iter().sum();
    h.map(|h| (h * 0.25 / sum) as f32)
}

/// The last `N` samples, latest first, readable as one slice without
/// shifting: each sample is written twice, `N` apart, and the window slides
/// backwards through the doubled buffer.
#[derive(Debug, Clone)]
struct History<const N: usize, const TWICE: usize> {
    samples: [f32; TWICE],
    newest: usize,
}

impl<const N: usize, const TWICE: usize> History<N, TWICE> {
    fn new() -> Self {
        Self {
            samples: [0.0; TWICE],
            newest: 0,
        }
    }

    #[inline]
    fn push(&mut self, x: f32) -> &[f32; N] {
        self.newest = if self.newest == 0 {
            N - 1
        } else {
            self.newest - 1
        };
        self.samples[self.newest] = x;
        self.samples[self.newest + N] = x;
        // TWICE is 2 N, so this always fits.
        self.samples[self.newest..self.newest + N]
            .try_into()
            .expect("history window")
    }
}

/// Doubles the sample rate.
#[derive(Debug, Clone)]
pub(super) struct Upsampler {
    h: [f32; PAIRS],
    history: History<SPAN, { 2 * SPAN }>,
}

impl Upsampler {
    pub fn new() -> Self {
        Self {
            h: coefficients(),
            history: History::new(),
        }
    }

    pub fn reset(&mut self) {
        self.history = History::new();
    }

    /// Two output samples, in order, for one input sample.
    #[inline]
    pub fn process(&mut self, x: f32) -> [f32; 2] {
        let recent = self.history.push(x);
        let mut even = 0.0;
        for j in 0..PAIRS {
            even += self.h[j] * (recent[j] + recent[SPAN - 1 - j]);
        }
        // Zero-stuffing halves the level; the factor of two restores it.
        [2.0 * even, recent[DELAY]]
    }
}

/// Halves the sample rate.
#[derive(Debug, Clone)]
pub(super) struct Downsampler {
    h: [f32; PAIRS],
    /// The odd input samples.
    odd: History<SPAN, { 2 * SPAN }>,
    /// The even ones, back to the center tap.
    even: History<{ DELAY + 1 }, { 2 * (DELAY + 1) }>,
}

impl Downsampler {
    pub fn new() -> Self {
        Self {
            h: coefficients(),
            odd: History::new(),
            even: History::new(),
        }
    }

    pub fn reset(&mut self) {
        self.odd = History::new();
        self.even = History::new();
    }

    /// One output sample for two input samples, in order.
    #[inline]
    pub fn process(&mut self, pair: [f32; 2]) -> f32 {
        let mut y = 0.5 * self.even.push(pair[0])[DELAY];
        let odd = self.odd.push(pair[1]);
        for j in 0..PAIRS {
            y += self.h[j] * (odd[j] + odd[SPAN - 1 - j]);
        }
        y
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use super::*;

    fn peak(signal: &[f32]) -> f32 {
        signal.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn up_then_down_passes_the_band_the_cabinet_passes() {
        // Up to 0.15 of the low rate, 7.2 kHz at 48 kHz.
        for cycles_per_sample in [0.01, 0.1, 0.15] {
            let (mut up, mut down) = (Upsampler::new(), Downsampler::new());
            let out: Vec<f32> = (0..4000)
                .map(|n| {
                    let x = (TAU * cycles_per_sample * n as f32).sin();
                    down.process(up.process(x))
                })
                .collect();
            let level = peak(&out[1000..]);
            assert!(
                (level - 1.0).abs() < 0.02,
                "{cycles_per_sample} cycles/sample: {level}"
            );
        }
    }

    #[test]
    fn down_sampling_rejects_what_would_alias_into_the_low_band() {
        // At the high rate, 0.4 to 0.5 cycles per sample fold back to below
        // 0.2 of the low rate (9.6 kHz at 48 kHz), under the cabinet's rolloff.
        for cycles_per_sample in [0.4, 0.45, 0.49] {
            let mut down = Downsampler::new();
            let out: Vec<f32> = (0..4000)
                .map(|n| {
                    let pair = [
                        (TAU * cycles_per_sample * (2 * n) as f32).sin(),
                        (TAU * cycles_per_sample * (2 * n + 1) as f32).sin(),
                    ];
                    down.process(pair)
                })
                .collect();
            let level = peak(&out[1000..]);
            assert!(level < 1e-3, "{cycles_per_sample}: {level}");
        }
    }
}
