//! Halfband filters for oversampling by two: polyphase IIR filters after
//! Laurent de Soras' HIIR library, themselves after Valenzuela and
//! Constantinides' elliptic halfband design.
//!
//! The lowpass is `H(z) = ½·(A₀(z²) + z⁻¹·A₁(z²))`, each `Aₖ` a chain of
//! first-order allpasses. Run at the lower rate, the two chains are the two
//! phases of the higher-rate signal: upsampling feeds one input sample to
//! both and takes one output from each, downsampling feeds each chain every
//! other input sample and averages them. That costs one multiply per
//! coefficient per low-rate sample, about a fifth of a linear-phase FIR of
//! the same steepness, with a delay of a few samples instead of dozens. The
//! phase is not linear near the top of the band, which a distortion doesn't
//! mind: dry and shaped signals are mixed before the filters, so they stay
//! aligned with each other. The waveshaper and the distortion use them.

use dsp::flush;

/// The most allpass coefficients one stage may have; the steepest design
/// used here needs fewer.
pub(crate) const MAX_COEFS: usize = 12;

/// A halfband lowpass: its coefficients, alternating between the two
/// chains (even indices to the first, odd to the second).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Design {
    coefs: [f32; MAX_COEFS],
    count: usize,
}

impl Design {
    /// A halfband lowpass whose transition band is `transition` wide (a
    /// fraction of the higher sample rate, centred on a quarter of it) and
    /// whose stop band lies at least `attenuation` dB down. The pass band is
    /// then flat to within far less than 0.001 dB. Not for the audio thread
    /// (it is cheap, but loops until series converge).
    pub fn new(attenuation: f64, transition: f64) -> Design {
        let transition = transition.clamp(1e-4, 0.49);
        let (k, q) = transition_params(transition);
        let order = order(attenuation, q);
        let count = ((order - 1) / 2).min(MAX_COEFS);
        let mut coefs = [0.0; MAX_COEFS];
        for (index, coef) in coefs.iter_mut().enumerate().take(count) {
            *coef = coefficient(index, k, q, order) as f32;
        }
        Design { coefs, count }
    }

    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.count
    }
}

/// HIIR's selectivity `k` and nome `q` for a transition band of `transition`.
fn transition_params(transition: f64) -> (f64, f64) {
    let k = ((1.0 - transition * 2.0) * std::f64::consts::FRAC_PI_4)
        .tan()
        .powi(2);
    let kk = (1.0 - k * k).powf(0.25);
    let e = 0.5 * (1.0 - kk) / (1.0 + kk);
    let e4 = e.powi(4);
    let q = e * (1.0 + e4 * (2.0 + e4 * (15.0 + 150.0 * e4)));
    (k, q)
}

/// The odd filter order that reaches `attenuation` dB for nome `q`.
fn order(attenuation: f64, q: f64) -> usize {
    let power = 10f64.powf(-attenuation / 10.0);
    let a = power / (1.0 - power);
    let order = ((a * a / 16.0).ln() / q.ln()).ceil().max(3.0) as usize;
    order | 1
}

fn coefficient(index: usize, k: f64, q: f64, order: usize) -> f64 {
    use std::f64::consts::PI;
    let c = (index + 1) as f64;
    let order = order as f64;

    let mut numerator = 0.0;
    let mut sign = 1.0;
    for i in 0..64 {
        let i = i as f64;
        let term = q.powf(i * (i + 1.0)) * ((i * 2.0 + 1.0) * c * PI / order).sin() * sign;
        numerator += term;
        sign = -sign;
        if term.abs() < 1e-100 {
            break;
        }
    }
    let mut denominator = 0.0;
    let mut sign = -1.0;
    for i in 1..64 {
        let i = i as f64;
        let term = q.powf(i * i) * (i * 2.0 * c * PI / order).cos() * sign;
        denominator += term;
        sign = -sign;
        if term.abs() < 1e-100 {
            break;
        }
    }
    let ww = numerator * q.powf(0.25) / (denominator + 0.5);
    let wwsq = ww * ww;
    let x = ((1.0 - wwsq * k) * (1.0 - wwsq / k)).sqrt() / (1.0 + wwsq);
    (1.0 - x) / (1.0 + x)
}

/// One halfband filter's memory, for one channel in one direction.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Halfband {
    x: [f32; MAX_COEFS],
    y: [f32; MAX_COEFS],
}

impl Halfband {
    /// Runs both chains one step: `paths[0]` through the even coefficients,
    /// `paths[1]` through the odd ones. Each chain is its own loop: they
    /// don't depend on each other, so the processor overlaps them.
    #[inline]
    fn step(&mut self, design: &Design, paths: [f32; 2]) -> [f32; 2] {
        let count = design.count.min(MAX_COEFS);
        let mut even = paths[0];
        for i in (0..count).step_by(2) {
            let out = (even - self.y[i]) * design.coefs[i] + self.x[i];
            self.x[i] = even;
            self.y[i] = out;
            even = out;
        }
        let mut odd = paths[1];
        for i in (1..count).step_by(2) {
            let out = (odd - self.y[i]) * design.coefs[i] + self.x[i];
            self.x[i] = odd;
            self.y[i] = out;
            odd = out;
        }
        [even, odd]
    }

    /// One sample in, the two samples that replace it at twice the rate out.
    #[inline]
    pub fn upsample(&mut self, design: &Design, input: f32) -> [f32; 2] {
        self.step(design, [input, input])
    }

    /// Two samples in (earlier first), one out at half the rate.
    #[inline]
    pub fn downsample(&mut self, design: &Design, early: f32, late: f32) -> f32 {
        let [a, b] = self.step(design, [late, early]);
        0.5 * (a + b)
    }

    pub fn clear(&mut self) {
        *self = Halfband::default();
    }

    /// Ends a decay in exact zeros rather than denormals.
    pub fn flush(&mut self) {
        for v in self.x.iter_mut().chain(self.y.iter_mut()) {
            *v = flush(*v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gain in dB of downsampling a sine at `freq` (a fraction of the higher
    /// rate), from its RMS level once the filter has settled. (Peaks of a
    /// sampled sine miss the true peak.)
    fn through_downsampler(design: &Design, freq: f64) -> f64 {
        let mut filter = Halfband::default();
        let len = 40_000;
        let mut power = 0.0f64;
        for n in 0..len {
            let s = |t: usize| (std::f64::consts::TAU * freq * t as f64).sin() as f32;
            let out = filter.downsample(design, s(2 * n), s(2 * n + 1));
            if n >= len / 2 {
                power += f64::from(out).powi(2);
            }
        }
        let rms = (power / (len / 2) as f64).sqrt();
        20.0 * (rms * std::f64::consts::SQRT_2).max(1e-12).log10()
    }

    #[test]
    fn halfbands_pass_the_band_and_stop_its_mirror() {
        for (transition, attenuation) in [(0.05, 100.0), (0.275, 100.0), (0.3875, 100.0)] {
            let design = Design::new(attenuation, transition);
            assert!(design.count() >= 1 && design.count() <= MAX_COEFS);
            // Pass band at the higher rate: up to a quarter minus half the
            // transition; stop band from a quarter plus half of it.
            let edge = 0.25 - transition / 2.0;
            let stop = 0.25 + transition / 2.0;
            let mut worst_pass = 0.0f64;
            for step in 1..=10 {
                let f = edge * step as f64 / 10.0;
                worst_pass = worst_pass.max(through_downsampler(&design, f).abs());
            }
            let mut worst_stop = f64::NEG_INFINITY;
            for step in 0..=10 {
                let f = stop + (0.499 - stop) * step as f64 / 10.0;
                worst_stop = worst_stop.max(through_downsampler(&design, f));
            }
            println!(
                "transition {transition}: {} coefficients, pass band within {worst_pass:.5} dB, stop band {worst_stop:.1} dB",
                design.count()
            );
            assert!(worst_pass < 0.01, "{worst_pass}");
            assert!(worst_stop < -90.0, "{worst_stop}");
        }
    }

    #[test]
    fn upsampling_then_downsampling_keeps_the_level() {
        let design = Design::new(100.0, 0.05);
        let (mut up, mut down) = (Halfband::default(), Halfband::default());
        for freq in [0.01, 0.1, 0.3, 0.44] {
            let mut power = 0.0f64;
            for n in 0..20_000 {
                let x = (std::f64::consts::TAU * freq * n as f64).sin() as f32;
                let [a, b] = up.upsample(&design, x);
                let y = down.downsample(&design, a, b);
                if n >= 10_000 {
                    power += f64::from(y).powi(2);
                }
            }
            let level = (2.0 * power / 10_000.0).sqrt();
            assert!((level - 1.0).abs() < 1e-3, "{freq}: {level}");
        }
    }
}
