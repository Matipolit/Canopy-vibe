//! A halfband decimator, for oscillators rendering at twice the rate (the
//! High quality setting): a 63-tap linear-phase low-pass at a quarter of
//! the doubled rate, Kaiser-windowed, keeping every other sample.
//!
//! Every other tap of a halfband filter is zero, so each output costs 16
//! symmetric pairs and the center. It passes to about 20 kHz at 48 kHz and
//! stops from about 28 kHz on, so what a hard-edged warp adds between the
//! two Nyquists doesn't fold back into the audible band.

use std::f64::consts::PI;
use std::sync::LazyLock;

/// Taps of the filter; the center is tap `TAPS / 2`.
const TAPS: usize = 63;
const CENTER: usize = TAPS / 2;
/// The Kaiser window's shape: about 75 dB of stopband.
const BETA: f64 = 7.5;
/// Input samples the filter remembers between blocks.
const HISTORY: usize = TAPS - 1;
/// The most input samples one call takes.
pub const MAX_INPUT: usize = 64;

/// The nonzero taps either side of the center, nearest first.
static PAIRS: LazyLock<[f32; CENTER.div_ceil(2)]> = LazyLock::new(|| {
    let bessel = |x: f64| {
        // I0 by its series, plenty converged after 30 terms.
        let mut sum = 1.0;
        let mut term = 1.0;
        for k in 1..30 {
            term *= (x / (2.0 * k as f64)).powi(2);
            sum += term;
        }
        sum
    };
    std::array::from_fn(|j| {
        let k = 2 * j + 1;
        let sinc = (PI * k as f64 / 2.0).sin() / (PI * k as f64);
        let r = k as f64 / CENTER as f64;
        let window = bessel(BETA * (1.0 - r * r).max(0.0).sqrt()) / bessel(BETA);
        (sinc * window) as f32
    })
});

/// Builds the taps, which the first use would otherwise do on the audio
/// thread. Call when the synth is made.
pub fn prepare() {
    LazyLock::force(&PAIRS);
}

/// One channel's decimator state.
#[derive(Debug, Clone)]
pub struct Decimator {
    /// The last [`HISTORY`] inputs, then room for a block of new ones.
    buffer: [f32; HISTORY + MAX_INPUT],
}

impl Default for Decimator {
    fn default() -> Self {
        Self {
            buffer: [0.0; HISTORY + MAX_INPUT],
        }
    }
}

impl Decimator {
    pub fn reset(&mut self) {
        self.buffer.fill(0.0);
    }

    /// Filters `input` (an even number of samples at the doubled rate, at
    /// most [`MAX_INPUT`]) and adds every other result to `output`, which
    /// is half as long.
    pub fn decimate_add(&mut self, input: &[f32], output: &mut [f32]) {
        let n = input.len().min(MAX_INPUT) & !1;
        self.buffer[HISTORY..HISTORY + n].copy_from_slice(&input[..n]);
        let pairs = &*PAIRS;
        for (i, out) in output.iter_mut().take(n / 2).enumerate() {
            // The newest input this output sees, and the filter's middle.
            let middle = HISTORY + 2 * i + 1 - CENTER;
            let buffer = &self.buffer;
            let mut y = 0.5 * buffer[middle];
            for (j, tap) in pairs.iter().enumerate() {
                let k = 2 * j + 1;
                y += tap * (buffer[middle - k] + buffer[middle + k]);
            }
            *out += y;
        }
        self.buffer.copy_within(n..n + HISTORY, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gain for a sine at `hz` at the doubled rate (96 kHz).
    fn gain(hz: f64) -> f64 {
        let mut decimator = Decimator::default();
        let rate = 96_000.0;
        let (mut input, mut output) = (0.0f64, 0.0f64);
        let mut t = 0usize;
        for block in 0..200 {
            let samples: Vec<f32> = (0..64)
                .map(|i| ((2.0 * PI * hz * (t + i) as f64 / rate).sin()) as f32)
                .collect();
            t += 64;
            let mut out = [0.0f32; 32];
            decimator.decimate_add(&samples, &mut out);
            if block > 20 {
                input += samples.iter().map(|x| (x * x) as f64).sum::<f64>() / 2.0;
                output += out.iter().map(|x| (x * x) as f64).sum::<f64>();
            }
        }
        (output / input).sqrt()
    }

    #[test]
    fn passes_the_audible_band_and_stops_above_it() {
        for hz in [100.0, 1000.0, 10_000.0, 18_000.0] {
            let g = gain(hz);
            assert!((g - 1.0).abs() < 0.01, "{hz} Hz: {g}");
        }
        for hz in [29_000.0, 35_000.0, 45_000.0] {
            let g = gain(hz);
            assert!(g < 10f64.powf(-60.0 / 20.0), "{hz} Hz: {g}");
        }
    }
}
