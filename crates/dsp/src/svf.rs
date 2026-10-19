//! Andrew Simper's trapezoidal state-variable filter (Cytomic,
//! "SvfLinearTrapOptimised2"), the one filter structure everything here is
//! built from. It is the bilinear transform of an analog prototype, so its
//! response has a closed form for drawing; it stays stable while its
//! coefficients move every sample; and one structure gives every shape: its
//! output is either a mix of the input, bandpass and lowpass ([`Shape`],
//! [`Coefs`], [`SvfState::tick`]: an EQ band, a shelf, a low cut) or all
//! three responses at once ([`SvfState::outputs`]: a crossover, a
//! synthesizer's resonant filter, [`Svf`]).

use std::f32::consts::PI;

use crate::settle;

/// Lowest cutoff. Below this the filter is inaudible as a filter, and the
/// integrator steps get small enough to lose precision in `f32`.
const MIN_CUTOFF: f32 = 10.0;

/// Highest cutoff as a fraction of the sample rate. The prewarped gain
/// `tan(π · cutoff / sample_rate)` goes to infinity at Nyquist (0.5).
const MAX_CUTOFF_RATIO: f32 = 0.49;

/// The lowest frequency [`prewarp`] tunes to.
const MIN_FREQ: f32 = 1.0;

/// Damping (`1 / Q`) at resonance 0: Q = 0.5, two real poles, no peak.
const MAX_DAMPING: f32 = 2.0;

/// Damping at resonance 1: Q = 20, a long ring that sounds like
/// self-oscillation. Zero would be a lossless resonator, which grows without
/// bound when the input has energy at the cutoff.
const MIN_DAMPING: f32 = 0.05;

/// Q of a Butterworth response: the flattest passband with no peak.
pub const BUTTERWORTH_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// The integrator gain for a cutoff, `tan(π · freq / sample_rate)`, prewarped
/// so the cutoff lands where asked instead of drifting down near Nyquist.
/// Kept between 1 Hz and just below Nyquist; NaN counts as the minimum.
#[inline]
pub fn prewarp(freq: f32, sample_rate: f32) -> f32 {
    // max first so NaN becomes the minimum.
    let freq = freq.max(MIN_FREQ).min(sample_rate * MAX_CUTOFF_RATIO);
    (PI * freq / sample_rate).tan()
}

/// `tan(x)` for `x` from 0 to about 1.45 (`0.46·π`), to a few parts in a
/// million: the sine and cosine series, far cheaper than the library call,
/// which matters for filters retuned every sample while a modulator moves
/// them.
#[inline]
pub fn tan_fast(x: f32) -> f32 {
    let x2 = x * x;
    let sin = x
        * (1.0
            + x2 * (-1.0 / 6.0
                + x2 * (1.0 / 120.0
                    + x2 * (-1.0 / 5040.0 + x2 * (1.0 / 362_880.0 - x2 / 39_916_800.0)))));
    let cos = 1.0
        + x2 * (-0.5
            + x2 * (1.0 / 24.0
                + x2 * (-1.0 / 720.0
                    + x2 * (1.0 / 40_320.0 + x2 * (-1.0 / 3_628_800.0 + x2 / 479_001_600.0)))));
    sin / cos
}

/// `tan(π · freq / sample_rate)` in double precision, for evaluating
/// responses; just below Nyquist at most, where the response has its limit.
#[inline]
pub fn response_tan(freq: f32, sample_rate: f32) -> f64 {
    let ratio = (f64::from(freq.max(0.0)) / f64::from(sample_rate)).min(0.4999);
    (std::f64::consts::PI * ratio).tan()
}

/// One filter setting: tuning `g`, damping `k` (`1 / Q`) and how much of the
/// input (`m0`), bandpass (`m1`) and lowpass (`m2`) make the output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    pub g: f32,
    pub k: f32,
    pub m0: f32,
    pub m1: f32,
    pub m2: f32,
}

impl Shape {
    #[inline]
    pub fn lowpass(freq: f32, q: f32, sample_rate: f32) -> Shape {
        Shape {
            g: prewarp(freq, sample_rate),
            k: 1.0 / q,
            m0: 0.0,
            m1: 0.0,
            m2: 1.0,
        }
    }

    #[inline]
    pub fn highpass(freq: f32, q: f32, sample_rate: f32) -> Shape {
        let k = 1.0 / q;
        Shape {
            g: prewarp(freq, sample_rate),
            k,
            m0: 1.0,
            m1: -k,
            m2: -1.0,
        }
    }

    /// Whether the output is exactly the input, so running it can be skipped.
    #[inline]
    pub fn is_identity(&self) -> bool {
        self.m0 == 1.0 && self.m1 == 0.0 && self.m2 == 0.0
    }

    /// `|H|²` at the frequency whose [`response_tan`] is `tan_w`.
    ///
    /// The filter maps the analog prototype
    /// `H(s) = (m0·(s² + k·s + 1) + m1·s + m2) / (s² + k·s + 1)`
    /// through the bilinear transform, so on the unit circle `s = j·tan_w / g`.
    #[inline]
    pub fn power_at(&self, tan_w: f64) -> f64 {
        let (g, k) = (f64::from(self.g), f64::from(self.k));
        let (m0, m1, m2) = (f64::from(self.m0), f64::from(self.m1), f64::from(self.m2));
        let w = tan_w / g;
        let w2 = w * w;
        let num_re = m0 * (1.0 - w2) + m2;
        let num_im = (m0 * k + m1) * w;
        let den_re = 1.0 - w2;
        let den_im = k * w;
        (num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)
    }

    /// The per-sample increment that takes `self` to `to` in `steps` samples.
    #[inline]
    pub fn step_to(&self, to: &Shape, steps: usize) -> Shape {
        let inv = 1.0 / steps as f32;
        Shape {
            g: (to.g - self.g) * inv,
            k: (to.k - self.k) * inv,
            m0: (to.m0 - self.m0) * inv,
            m1: (to.m1 - self.m1) * inv,
            m2: (to.m2 - self.m2) * inv,
        }
    }

    #[inline]
    pub fn advance(&mut self, step: &Shape) {
        self.g += step.g;
        self.k += step.k;
        self.m0 += step.m0;
        self.m1 += step.m1;
        self.m2 += step.m2;
    }
}

/// A [`Shape`] with its coefficients worked out, ready to run. The default
/// is silent: it outputs nothing until tuned.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Coefs {
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    m0: f32,
    m1: f32,
    m2: f32,
}

impl Coefs {
    pub const IDENTITY: Coefs = Coefs {
        k: 0.0,
        a1: 1.0,
        a2: 0.0,
        a3: 0.0,
        m0: 1.0,
        m1: 0.0,
        m2: 0.0,
    };

    #[inline]
    pub fn new(shape: &Shape) -> Coefs {
        let (a1, a2, a3) = gains(shape.g, shape.k);
        Coefs {
            k: shape.k,
            a1,
            a2,
            a3,
            m0: shape.m0,
            m1: shape.m1,
            m2: shape.m2,
        }
    }

    /// A Butterworth filter at `freq`, for [`SvfState::outputs`]: its
    /// lowpass and highpass are a [`Crossover`]'s halves.
    #[inline]
    pub fn butterworth(freq: f32, sample_rate: f32) -> Coefs {
        Coefs::new(&Shape::lowpass(freq, BUTTERWORTH_Q, sample_rate))
    }

    /// The damping, `1 / Q`.
    #[inline]
    pub fn k(&self) -> f32 {
        self.k
    }
}

/// Simper's gains `a1`, `a2`, `a3` for integrator gain `g` and damping `k`.
#[inline]
fn gains(g: f32, k: f32) -> (f32, f32, f32) {
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    (a1, a2, g * a2)
}

/// Simper's step, on the integrators' memory `s1` and `s2`: the bandpass
/// `v1` and lowpass `v2` for input `x`.
#[inline(always)]
fn step(s1: &mut f32, s2: &mut f32, a1: f32, a2: f32, a3: f32, x: f32) -> (f32, f32) {
    let v3 = x - *s2;
    let v1 = a1 * *s1 + a2 * v3;
    let v2 = *s2 + a2 * *s1 + a3 * v3;
    *s1 = 2.0 * v1 - *s1;
    *s2 = 2.0 * v2 - *s2;
    (v1, v2)
}

/// One channel's filter memory (Simper's `ic1eq` and `ic2eq`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SvfState {
    s1: f32,
    s2: f32,
}

impl SvfState {
    /// One step: the bandpass `v1` and lowpass `v2` for input `x`.
    #[inline(always)]
    fn step(&mut self, c: &Coefs, x: f32) -> (f32, f32) {
        step(&mut self.s1, &mut self.s2, c.a1, c.a2, c.a3, x)
    }

    /// The shape's output for input `x`.
    #[inline(always)]
    pub fn tick(&mut self, c: &Coefs, x: f32) -> f32 {
        let (v1, v2) = self.step(c, x);
        c.m0 * x + c.m1 * v1 + c.m2 * v2
    }

    /// All three responses for input `x`; `c`'s mix is ignored.
    #[inline(always)]
    pub fn outputs(&mut self, c: &Coefs, x: f32) -> SvfOutput {
        let (v1, v2) = self.step(c, x);
        SvfOutput {
            lowpass: v2,
            highpass: x - c.k * v1 - v2,
            bandpass: v1,
        }
    }

    /// Ends a decay in exact zeros rather than denormals, and forgets NaN
    /// and infinity that came in with the block.
    pub fn flush(&mut self) {
        self.s1 = settle(self.s1);
        self.s2 = settle(self.s2);
    }
}

/// One channel of a Linkwitz-Riley 24 dB/octave crossover: one filter gives
/// a Butterworth lowpass and highpass at once, and a second of each squares
/// them. The halves sum to an allpass: flat in level at every frequency.
#[derive(Debug, Clone, Copy, Default)]
pub struct Crossover {
    first: SvfState,
    low: SvfState,
    high: SvfState,
}

impl Crossover {
    /// (low, high) for input `x`, split at `c` ([`Coefs::butterworth`]).
    #[inline(always)]
    pub fn split(&mut self, c: &Coefs, x: f32) -> (f32, f32) {
        let first = self.first.outputs(c, x);
        (
            self.low.outputs(c, first.lowpass).lowpass,
            self.high.outputs(c, first.highpass).highpass,
        )
    }

    pub fn flush(&mut self) {
        for state in [&mut self.first, &mut self.low, &mut self.high] {
            state.flush();
        }
    }
}

/// Which output of an [`Svf`] to listen to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterMode {
    #[default]
    Lowpass,
    Highpass,
    Bandpass,
}

/// All three responses for one input sample. They come from the same state,
/// so computing the ones you don't use is nearly free.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SvfOutput {
    pub lowpass: f32,
    pub highpass: f32,
    /// Peaks at the cutoff with gain Q, like the resonant peak of the other two.
    pub bandpass: f32,
}

impl SvfOutput {
    #[inline]
    pub fn get(self, mode: FilterMode) -> f32 {
        match mode {
            FilterMode::Lowpass => self.lowpass,
            FilterMode::Highpass => self.highpass,
            FilterMode::Bandpass => self.bandpass,
        }
    }
}

/// Resonant state-variable filter, 12 dB/octave, one channel.
///
/// Topology-preserving transform (zero-delay feedback) after Andrew Simper's
/// "Linear Trapezoidal Integrated SVF": it matches the analog filter's
/// response up to the cutoff and stays stable while the cutoff moves every
/// sample, which is what envelope sweeps need.
///
/// Coefficients are cached. Setting a cutoff or resonance equal to the current
/// one costs a comparison; a new cutoff costs one `tan`, a new resonance one
/// division.
#[derive(Debug, Clone)]
pub struct Svf {
    sample_rate: f32,
    cutoff: f32,
    resonance: f32,
    /// `tan(π · cutoff / sample_rate)`: integrator gain, prewarped so the
    /// cutoff lands where asked instead of drifting down near Nyquist.
    g: f32,
    /// Damping, `1 / Q`.
    k: f32,
    /// Simper's gains and memory as plain fields rather than a [`Coefs`]
    /// and an [`SvfState`]: nested, they made the Poly Synth's voice loop
    /// 4% slower.
    a1: f32,
    a2: f32,
    a3: f32,
    s1: f32,
    s2: f32,
}

impl Svf {
    /// `resonance` is `0..=1`; see [`Self::set_resonance`].
    pub fn new(cutoff: f32, resonance: f32) -> Self {
        let mut svf = Self {
            sample_rate: 44_100.0,
            cutoff: 0.0,
            resonance: 0.0,
            g: 0.0,
            k: MAX_DAMPING,
            a1: 1.0,
            a2: 0.0,
            a3: 0.0,
            s1: 0.0,
            s2: 0.0,
        };
        svf.set_resonance(resonance);
        svf.cutoff = sanitize_cutoff(cutoff);
        svf.update_g();
        svf
    }

    /// Clears the state too, since it was shaped by the old sample rate.
    pub fn prepare(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.update_g();
        self.reset();
    }

    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }

    /// Cutoff in Hz. Kept between 10 Hz and just below Nyquist when the
    /// coefficients are computed; NaN counts as the minimum.
    pub fn set_cutoff(&mut self, hz: f32) {
        let hz = sanitize_cutoff(hz);
        if hz != self.cutoff {
            self.cutoff = hz;
            self.update_g();
        }
    }

    /// `0..=1`, from no peak (Q = 0.5) to a long ring (Q = 20). Linear in
    /// damping like an analog resonance knob, so most of the peak arrives in
    /// the top quarter of the range. Out-of-range values and NaN are clamped.
    pub fn set_resonance(&mut self, resonance: f32) {
        // NaN becomes 0 rather than poisoning the state.
        let resonance = if resonance.is_nan() {
            0.0
        } else {
            resonance.clamp(0.0, 1.0)
        };
        if resonance != self.resonance {
            self.resonance = resonance;
            self.k = MAX_DAMPING - (MAX_DAMPING - MIN_DAMPING) * resonance;
            self.update_a();
        }
    }

    /// The cutoff last set, before clamping to the sample rate's range.
    pub fn cutoff(&self) -> f32 {
        self.cutoff
    }

    pub fn resonance(&self) -> f32 {
        self.resonance
    }

    #[inline]
    pub fn process_sample(&mut self, input: f32) -> SvfOutput {
        let (v1, v2) = step(&mut self.s1, &mut self.s2, self.a1, self.a2, self.a3, input);
        SvfOutput {
            lowpass: v2,
            highpass: input - self.k * v1 - v2,
            bandpass: v1,
        }
    }

    fn update_g(&mut self) {
        let hz = self.cutoff.min(self.sample_rate * MAX_CUTOFF_RATIO);
        self.g = (PI * hz / self.sample_rate).tan();
        self.update_a();
    }

    fn update_a(&mut self) {
        (self.a1, self.a2, self.a3) = gains(self.g, self.k);
    }
}

/// Applies the lower limit. The upper one depends on the sample rate, so it
/// is applied when computing coefficients, which lets a cutoff above Nyquist
/// come back into range after `prepare` raises the sample rate.
fn sanitize_cutoff(hz: f32) -> f32 {
    // max rather than clamp so NaN becomes the minimum.
    hz.max(MIN_CUTOFF)
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use super::*;

    const SAMPLE_RATE: f32 = 48_000.0;

    /// Steady-state gain of `mode` for a sine at `hz`, measured by RMS after
    /// the filter has settled.
    fn gain_at(mode: FilterMode, cutoff: f32, resonance: f32, hz: f32) -> f32 {
        let mut svf = Svf::new(cutoff, resonance);
        svf.prepare(SAMPLE_RATE);
        let samples = SAMPLE_RATE as usize;
        let settle = samples / 2;
        let (mut in_sq, mut out_sq) = (0.0f64, 0.0f64);
        for n in 0..samples {
            let x = (TAU * hz * n as f32 / SAMPLE_RATE).sin();
            let y = svf.process_sample(x).get(mode);
            if n >= settle {
                in_sq += (x * x) as f64;
                out_sq += (y * y) as f64;
            }
        }
        (out_sq / in_sq).sqrt() as f32
    }

    #[test]
    fn lowpass_passes_lows_and_cuts_highs_by_12_db_per_octave() {
        let low = gain_at(FilterMode::Lowpass, 1000.0, 0.0, 100.0);
        assert!(low > 0.95, "100 Hz through a 1 kHz lowpass: {low}");
        // A decade above the cutoff, two poles give about -40 dB (0.01);
        // prewarping pushes it a little lower.
        let high = gain_at(FilterMode::Lowpass, 1000.0, 0.0, 10_000.0);
        assert!((0.004..0.015).contains(&high), "10 kHz: {high}");
    }

    #[test]
    fn highpass_passes_highs_and_cuts_lows_by_12_db_per_octave() {
        let high = gain_at(FilterMode::Highpass, 1000.0, 0.0, 10_000.0);
        assert!(high > 0.95, "10 kHz through a 1 kHz highpass: {high}");
        let low = gain_at(FilterMode::Highpass, 1000.0, 0.0, 100.0);
        assert!((0.005..0.015).contains(&low), "100 Hz: {low}");
    }

    #[test]
    fn bandpass_peaks_at_the_cutoff() {
        let center = gain_at(FilterMode::Bandpass, 1000.0, 0.0, 1000.0);
        assert!(
            (center - 0.5).abs() < 0.02,
            "gain Q = 0.5 at the cutoff: {center}"
        );
        // 6 dB/octave on each side: about 0.02 at a fiftieth and at 20 times the cutoff.
        assert!(gain_at(FilterMode::Bandpass, 1000.0, 0.0, 20.0) < 0.03);
        assert!(gain_at(FilterMode::Bandpass, 1000.0, 0.0, 20_000.0) < 0.03);
    }

    #[test]
    fn resonance_raises_the_peak_to_q() {
        let flat = gain_at(FilterMode::Lowpass, 1000.0, 0.0, 1000.0);
        let peak = gain_at(FilterMode::Lowpass, 1000.0, 1.0, 1000.0);
        assert!((flat - 0.5).abs() < 0.02, "{flat}");
        assert!((peak - 1.0 / MIN_DAMPING).abs() < 1.0, "{peak}");
    }

    #[test]
    fn stays_bounded_at_full_resonance_while_sweeping_to_nyquist() {
        let mut svf = Svf::new(20.0, 1.0);
        svf.prepare(SAMPLE_RATE);
        // White-ish noise from a xorshift, so every frequency gets excited.
        let mut seed = 0x1234_5678u32;
        let mut peak = 0.0f32;
        for n in 0..(SAMPLE_RATE as usize * 4) {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let x = seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
            // Exponential sweeps from 20 Hz to past Nyquist and back, twice a second.
            let t = (n as f32 / SAMPLE_RATE * 2.0).fract();
            let sweep = 1.0 - (2.0 * t - 1.0).abs();
            svf.set_cutoff(20.0 * 2f32.powf(sweep * 11.0));
            let out = svf.process_sample(x);
            for y in [out.lowpass, out.highpass, out.bandpass] {
                assert!(y.is_finite(), "sample {n}");
                peak = peak.max(y.abs());
            }
        }
        assert!(peak < 50.0, "peak {peak}");
    }

    #[test]
    fn cutoff_above_nyquist_and_nan_stay_usable() {
        for (cutoff, resonance) in [(1e9, 1.0), (f32::INFINITY, 0.5), (f32::NAN, f32::NAN)] {
            let mut svf = Svf::new(cutoff, resonance);
            svf.prepare(SAMPLE_RATE);
            for n in 0..1000 {
                let x = if n % 50 < 25 { 1.0 } else { -1.0 };
                let y = svf.process_sample(x).lowpass;
                assert!(y.is_finite() && y.abs() < 50.0, "{cutoff} {resonance}: {y}");
            }
        }
    }

    #[test]
    fn reset_clears_state() {
        let mut svf = Svf::new(500.0, 0.9);
        svf.prepare(SAMPLE_RATE);
        for _ in 0..1000 {
            svf.process_sample(1.0);
        }
        assert_ne!(svf.process_sample(0.0).lowpass, 0.0, "still ringing");
        svf.reset();
        for _ in 0..100 {
            assert_eq!(svf.process_sample(0.0), SvfOutput::default());
        }
    }
}
