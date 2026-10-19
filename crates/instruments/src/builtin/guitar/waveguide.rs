//! One polarization of a vibrating string as a digital waveguide: an
//! extended Karplus-Strong loop (Jaffe & Smith 1983). A delay line holds one
//! period of the wave; each sample read back goes through a loss filter (the
//! string's damping), a dispersion filter (its stiffness) and back into the
//! line, plus whatever drives the string from outside (a pluck, the bridge).
//!
//! The loop is a single delay line rather than two travelling-wave rails
//! (the "single-delay-loop" form of Karjalainen, Välimäki & Tolonen 1998).
//! Heard from the bridge, a plucked string is fully described by its period,
//! its losses and its stiffness, and one line costs half as much. Where the
//! string is plucked is in the shape of the excitation instead, and where a
//! pickup sits is a comb read from the line's history ([`Loop::comb`]),
//! since both are the sum of a wave and its reflection a fixed time apart.
//!
//! Tuning: the loop's period is the line delay plus the phase delay of every
//! filter in it at the fundamental. The filters' delays are computed and
//! taken off, and the remainder, fractional, is read from the line with a
//! third-order Lagrange interpolator (Laakso, Välimäki, Karjalainen & Laine
//! 1996), which stays smooth while the delay moves (bends, vibrato, slides,
//! tension modulation) where a Thiran allpass would ring.
//!
//! Losses: fitted to two decay times, the fundamental's and the treble's, as
//! Välimäki, Huopaniemi, Karjalainen & Jánosy 1996 fit their one-pole loop
//! filter; here a first-order shelf, whose loss grows with frequency the way
//! measured strings' does (see [`Loss`]). Stiffness: a cascade of first-order allpasses
//! that puts one chosen upper partial where `f_n = n f₀ √(1 + B n²)` says
//! (after Van Duyne & Smith 1994 and Rauhala & Välimäki 2006).

use std::f32::consts::TAU;

/// `ln(1000)`: an amplitude decaying at `LN_1000 / t` nepers per second
/// falls by 60 dB in `t` seconds.
pub(super) const LN_1000: f32 = 6.907_755;

/// Shortest line delay the interpolator can read: its four taps sit
/// `delay - 1 ..= delay + 2` samples back, and the newest must already have
/// been written.
pub(super) const MIN_DELAY: f32 = 2.0;

/// First-order allpasses in the dispersion filter. Each adds a little more
/// delay to low partials than to high ones, the way a stiff string carries
/// high frequencies faster, which sharpens the upper partials.
pub(super) const DISPERSION_STAGES: usize = 4;

/// Most negative dispersion coefficient. Closer to -1 the stages' delay at
/// low frequencies grows without bound.
const MAX_DISPERSION_POLE: f32 = 0.9;

/// The dispersion is matched at the partial nearest this frequency: high
/// enough for stiffness to be audible, low enough to be a strong partial...
const DISPERSION_MATCH_HZ: f32 = 2500.0;

/// ...but at most this partial. A first-order allpass's delay falls off
/// with frequency like a stiff string's at first and then levels out, so a
/// cascade matched far up stretches the partials below too much, and one
/// matched low stretches those above too little. Matched here, partials 2
/// to 12 fit the string's `B` within about 5%; above, on the low wound
/// strings, the stretch grows more slowly than the string's would.
const DISPERSION_MATCH_PARTIAL: f32 = 14.0;

/// At most this share of the loop goes to the dispersion filter, so short
/// loops keep room for the interpolator and the loss filter.
const DISPERSION_BUDGET: f32 = 0.25;

/// The loop's gain at DC never reaches 1, whatever the decay asks for, so
/// the loop can't build up.
const MAX_LOOP_GAIN: f32 = 0.999_95;

/// The loop's frequency-dependent loss: a first-order shelf,
/// `(b0 + b1 z⁻¹) / (1 + a1 z⁻¹)`, losing little at the fundamental and more
/// above its corner, levelling off in the treble. Measured strings lose
/// their partials at a rate that grows roughly in proportion to frequency
/// from a few hundred hertz to a few kilohertz and then much more slowly;
/// a shelf with its corner near 1 kHz follows that, where the usual
/// one-pole lowpass (whose loss grows with the square of frequency) leaves
/// the mid partials ringing too long or dulls the treble too soon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Loss {
    pub b0: f32,
    pub b1: f32,
    pub a1: f32,
}

impl Loss {
    pub const SILENT: Loss = Loss {
        b0: 0.0,
        b1: 0.0,
        a1: 0.0,
    };

    /// The shelf whose gain is exactly `gain0` at `w0`, `gain_high` well
    /// above the corner `corner` (all in radians per sample), designed from
    /// the analog shelf `(G∞ s + G₀ Ω) / (s + Ω)` by the bilinear transform,
    /// which maps its magnitude exactly.
    pub fn fit(w0: f32, gain0: f32, corner: f32, gain_high: f32) -> Loss {
        let corner = corner.clamp(1e-4, 3.0);
        let omega = (0.5 * corner).tan();
        let t = (0.5 * w0.clamp(1e-5, 3.1)).tan();
        let high = gain_high.min(gain0).max(0.0);
        // |H(w0)|² = (G₀² Ω² + G∞² t²) / (Ω² + t²): solve for G₀.
        let dc_squared =
            (gain0 * gain0 * (t * t + omega * omega) - high * high * t * t) / (omega * omega);
        let dc = dc_squared.max(high * high).sqrt().min(MAX_LOOP_GAIN);
        let norm = 1.0 / (1.0 + omega);
        Loss {
            b0: (high + dc * omega) * norm,
            b1: (dc * omega - high) * norm,
            a1: (omega - 1.0) * norm,
        }
    }

    /// The response at `w`, given `sin w` and `cos w`: (real, imaginary).
    fn response(&self, sin: f32, cos: f32) -> (f32, f32) {
        // (b0 + b1 e^-jw) / (1 + a1 e^-jw)
        let (nr, ni) = (self.b0 + self.b1 * cos, -self.b1 * sin);
        let (dr, di) = (1.0 + self.a1 * cos, -self.a1 * sin);
        let d = dr * dr + di * di;
        ((nr * dr + ni * di) / d, (ni * dr - nr * di) / d)
    }

    /// Phase delay in samples at `w`, given `sin w` and `cos w`.
    fn phase_delay(&self, sin: f32, cos: f32, w: f32) -> f32 {
        let (re, im) = self.response(sin, cos);
        -im.atan2(re) / w
    }

    /// Gain at `w` radians per sample.
    #[cfg(test)]
    pub fn gain(&self, w: f32) -> f32 {
        let (re, im) = self.response(w.sin(), w.cos());
        (re * re + im * im).sqrt()
    }
}

/// Phase delay in samples of the allpass `(a + z⁻¹) / (1 + a z⁻¹)` at `w`,
/// given `sin w` and `cos w`.
fn allpass_phase_delay(a: f32, sin: f32, cos: f32, w: f32) -> f32 {
    1.0 - 2.0 * (a * sin).atan2(1.0 + a * cos) / w
}

/// The dispersion coefficient for a string with inharmonicity `b` playing
/// `hz`: with it, [`DISPERSION_STAGES`] allpasses put the partial nearest
/// [`DISPERSION_MATCH_HZ`] where stiffness puts it, at `n hz sqrt(1 + b n²)`.
/// Zero (no dispersion) when that partial is too close to the fundamental to
/// matter or the loop is too short to spare the delay.
pub(super) fn dispersion_coefficient(b: f32, hz: f32, sample_rate: f32) -> f32 {
    let partial_number = matched_partial(hz);
    if partial_number < 2.0 || b <= 0.0 {
        return 0.0;
    }
    let stretch = (1.0 + b * partial_number * partial_number).sqrt();
    let partial_hz = partial_number * hz * stretch;
    if partial_hz > 0.4 * sample_rate {
        return 0.0;
    }
    let period = sample_rate / hz;
    let w0 = TAU * hz / sample_rate;
    let wn = TAU * partial_hz / sample_rate;
    // Partial n resonates when the loop is shorter at its frequency than at
    // the fundamental by this much; each stage provides its share.
    let wanted = period * (1.0 - 1.0 / stretch) / DISPERSION_STAGES as f32;
    let (s0, c0) = w0.sin_cos();
    let (sn, cn) = wn.sin_cos();
    let spread = |a: f32| allpass_phase_delay(a, s0, c0, w0) - allpass_phase_delay(a, sn, cn, wn);
    // The spread grows monotonically as the coefficient goes from 0 to -1.
    let (mut stiff, mut slack) = (-MAX_DISPERSION_POLE, 0.0f32);
    let a = if spread(stiff) <= wanted {
        stiff
    } else {
        for _ in 0..24 {
            let mid = 0.5 * (stiff + slack);
            if spread(mid) > wanted {
                stiff = mid;
            } else {
                slack = mid;
            }
        }
        0.5 * (stiff + slack)
    };
    // Each stage delays low frequencies by (1 - a) / (1 + a) samples.
    let per_stage = DISPERSION_BUDGET * period / DISPERSION_STAGES as f32;
    let budget_limit = (1.0 - per_stage) / (1.0 + per_stage);
    a.max(budget_limit).min(0.0)
}

/// The partial at which [`dispersion_coefficient`] matches a note at `hz`.
pub(super) fn matched_partial(hz: f32) -> f32 {
    (DISPERSION_MATCH_HZ / hz)
        .round()
        .min(DISPERSION_MATCH_PARTIAL)
}

/// Phase delay in samples of the third-order Lagrange interpolator reading
/// `delay` samples back, at `w`, given `sin w` and `cos w`.
fn lagrange_phase_delay(delay: f32, sin: f32, cos: f32, w: f32) -> f32 {
    let whole = delay.floor();
    let h = lagrange(delay - whole);
    // Taps at whole - 1 + k samples back, k = 0..3.
    let (cos2, sin2) = (2.0 * cos * cos - 1.0, 2.0 * sin * cos);
    let (cos3, sin3) = (cos * (4.0 * cos * cos - 3.0), sin * (3.0 - 4.0 * sin * sin));
    let re = h[0] + h[1] * cos + h[2] * cos2 + h[3] * cos3;
    let im = h[1] * sin + h[2] * sin2 + h[3] * sin3;
    whole - 1.0 + im.atan2(re) / w
}

/// Third-order Lagrange coefficients for taps 0..3 samples apart, reading
/// `1 + frac` samples behind the first: the interpolator is flattest in the
/// middle of its span.
#[inline]
fn lagrange(frac: f32) -> [f32; 4] {
    let (d0, d1, d2, d3) = (frac + 1.0, frac, frac - 1.0, frac - 2.0);
    [
        -d1 * d2 * d3 * (1.0 / 6.0),
        d0 * d2 * d3 * 0.5,
        -d0 * d1 * d3 * 0.5,
        d0 * d1 * d2 * (1.0 / 6.0),
    ]
}

/// One polarization's loop: the delay line and the filters in it.
pub(super) struct Loop {
    line: Vec<f32>,
    mask: usize,
    /// Where the next sample is written.
    write: usize,
    /// Line delay in samples, moving by `delay_step` per sample so pitch
    /// changes are smooth within a control interval.
    delay: f32,
    delay_step: f32,
    max_delay: f32,
    loss: Loss,
    loss_state: f32,
    dispersion: f32,
    stages: usize,
    allpass_in: [f32; DISPERSION_STAGES],
    allpass_out: [f32; DISPERSION_STAGES],
    /// Phase delay of the loop's filters (and the interpolator's error) at
    /// the fundamental, from the last [`Loop::tune`]. While only the pitch
    /// moves it changes by hundredths of a sample, so it is reused.
    filter_delay: f32,
    /// The whole period in samples (line plus filters) at the last tuning.
    period: f32,
}

impl Loop {
    pub fn new() -> Self {
        Self {
            line: Vec::new(),
            mask: 0,
            write: 0,
            delay: MIN_DELAY,
            delay_step: 0.0,
            max_delay: MIN_DELAY,
            loss: Loss::SILENT,
            loss_state: 0.0,
            dispersion: 0.0,
            stages: 0,
            allpass_in: [0.0; DISPERSION_STAGES],
            allpass_out: [0.0; DISPERSION_STAGES],
            filter_delay: 0.0,
            period: MIN_DELAY,
        }
    }

    /// Allocates a line long enough for `max_delay` samples.
    pub fn prepare(&mut self, max_delay: f32) {
        // Room for the interpolator's taps beyond the longest delay.
        let length = (max_delay.ceil() as usize + 4).next_power_of_two();
        self.line = vec![0.0; length];
        self.mask = length - 1;
        self.max_delay = max_delay;
        self.clear();
    }

    /// Empties the line and the filters: a still string.
    pub fn clear(&mut self) {
        self.line.fill(0.0);
        self.write = 0;
        self.loss_state = 0.0;
        self.allpass_in = [0.0; DISPERSION_STAGES];
        self.allpass_out = [0.0; DISPERSION_STAGES];
        self.delay_step = 0.0;
    }

    /// Sets the stiffness (see [`dispersion_coefficient`]); 0 for none.
    /// Takes effect at the next [`Loop::tune`].
    pub fn set_dispersion(&mut self, coefficient: f32) {
        if coefficient != self.dispersion {
            self.dispersion = coefficient;
            self.allpass_in = [0.0; DISPERSION_STAGES];
            self.allpass_out = [0.0; DISPERSION_STAGES];
        }
        self.stages = if coefficient < 0.0 {
            DISPERSION_STAGES
        } else {
            0
        };
    }

    /// Period in samples at the last tuning.
    pub fn period(&self) -> f32 {
        self.period
    }

    /// Tunes the loop to `hz` (clamped to what it can play) with losses of
    /// `gain0` per period at the fundamental and `gain_high` in the treble,
    /// above `corner` radians per sample (see [`Loss`]). The delay ramps
    /// there over `ramp` samples, or jumps if 0.
    pub fn tune(
        &mut self,
        rate: f32,
        hz: f32,
        gain0: f32,
        corner: f32,
        gain_high: f32,
        ramp: usize,
    ) {
        let hz = hz.min(rate / (MIN_DELAY + 2.0));
        let w0 = TAU * hz / rate;
        let (sin, cos) = w0.sin_cos();
        self.loss = Loss::fit(w0, gain0, corner, gain_high);
        let filters = self.loss.phase_delay(sin, cos, w0)
            + self.stages as f32 * allpass_phase_delay(self.dispersion, sin, cos, w0);
        let mut target = (rate / hz - filters).max(MIN_DELAY);
        // The interpolator's own phase delay drifts from the asked-for delay
        // at high frequencies; one correction step takes it to within a
        // fraction of a cent.
        target -= lagrange_phase_delay(target, sin, cos, w0) - target;
        self.filter_delay = rate / hz - target;
        self.period = rate / hz;
        self.ramp_to(target, ramp);
    }

    /// Only the pitch moved, a little: moves the delay for `hz` without
    /// refitting the filters.
    pub fn retarget(&mut self, rate: f32, hz: f32, ramp: usize) {
        let hz = hz.min(rate / (MIN_DELAY + 2.0));
        self.period = rate / hz;
        self.ramp_to(self.period - self.filter_delay, ramp);
    }

    /// Stops a ramp where it is.
    pub fn hold(&mut self) {
        self.delay_step = 0.0;
    }

    /// Moves the line delay to `target` over `ramp` samples, or at once if 0.
    fn ramp_to(&mut self, target: f32, ramp: usize) {
        let target = target.clamp(MIN_DELAY, self.max_delay);
        if ramp == 0 {
            self.delay = target;
            self.delay_step = 0.0;
        } else {
            self.delay_step = (target - self.delay) / ramp as f32;
        }
    }

    /// Runs the loop for `output.len()` samples, adding `input` (as long as
    /// `output`) into it, and writes what it carries to `output`: the force
    /// on the bridge.
    pub fn run(&mut self, input: &[f32], output: &mut [f32]) {
        // Most of the time the pitch holds still and the interpolator's
        // coefficients can be computed once per call; and only notes low
        // enough to need it have dispersion.
        match (self.delay_step != 0.0, self.stages > 0) {
            (false, false) => self.run_with::<false, false>(input, output),
            (false, true) => self.run_with::<false, true>(input, output),
            (true, false) => self.run_with::<true, false>(input, output),
            (true, true) => self.run_with::<true, true>(input, output),
        }
    }

    #[inline(always)]
    fn run_with<const MOVING: bool, const DISPERSION: bool>(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) {
        let Self {
            line,
            mask,
            write,
            delay,
            delay_step,
            loss,
            loss_state,
            dispersion,
            allpass_in,
            allpass_out,
            ..
        } = self;
        let mask = *mask;
        let Loss { b0, b1, a1 } = *loss;
        let coefficient = *dispersion;
        let (mut ap_in, mut ap_out) = (*allpass_in, *allpass_out);
        let (mut w, mut d, step) = (*write, *delay, *delay_step);
        let mut lowpassed = *loss_state;
        let held = d.max(MIN_DELAY);
        let (held_whole, held_h) = (held as usize, lagrange(held - held.floor()));
        for (&x, out) in input.iter().zip(output.iter_mut()) {
            let (whole, h) = if MOVING {
                let now = d.max(MIN_DELAY);
                let whole = now as usize;
                d += step;
                (whole, lagrange(now - whole as f32))
            } else {
                (held_whole, held_h)
            };
            let newest = w.wrapping_sub(whole - 1);
            let read = h[0] * line[newest & mask]
                + h[1] * line[newest.wrapping_sub(1) & mask]
                + h[2] * line[newest.wrapping_sub(2) & mask]
                + h[3] * line[newest.wrapping_sub(3) & mask];
            // Transposed direct form II: one state.
            let shelved = b0 * read + lowpassed;
            lowpassed = b1 * read - a1 * shelved;
            let mut v = shelved;
            if DISPERSION {
                for (x, y) in ap_in.iter_mut().zip(ap_out.iter_mut()) {
                    let out = coefficient * (v - *y) + *x;
                    *x = v;
                    *y = out;
                    v = out;
                }
            }
            v += x;
            line[w] = v;
            w = (w + 1) & mask;
            *out = v;
        }
        (*allpass_in, *allpass_out) = (ap_in, ap_out);
        (*write, *delay) = (w, d);
        // Denormals would slow a decaying loop to a crawl on some CPUs.
        *loss_state = if lowpassed.abs() < 1e-20 {
            0.0
        } else {
            lowpassed
        };
    }

    /// Adds `gain` times the comb `v[n] - v[n - distance]` over the last
    /// `out.len()` samples the loop produced to `out`: how a sensor that
    /// sits `distance / period` of the way along the string from the bridge
    /// hears it (a pickup), up to its own filtering. `distance` is in
    /// samples, at most the period.
    pub fn comb(&self, distance: f32, gain: f32, out: &mut [f32]) {
        let mask = self.mask;
        let distance = distance.clamp(1.0, self.max_delay);
        let whole = distance as usize;
        let frac = distance - whole as f32;
        let first = self.write.wrapping_sub(out.len());
        for (i, out) in out.iter_mut().enumerate() {
            let now = first.wrapping_add(i);
            let back = now.wrapping_sub(whole);
            let earlier = self.line[back & mask] * (1.0 - frac)
                + self.line[back.wrapping_sub(1) & mask] * frac;
            *out += gain * (self.line[now & mask] - earlier);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    /// Frequency of the partial near `hz`, from how far its phase turns
    /// between two Hann windows `hop` samples apart. The windows span enough
    /// periods to keep the neighbouring partials out, and the hop is as long
    /// as an error of half a semitone allows, so phase noise matters little.
    fn partial_hz(signal: &[f32], hz: f32, rate: f32) -> f32 {
        let window = ((8.0 * rate / hz) as usize).clamp(2048, signal.len() / 2);
        let hop = ((rate / (0.06 * hz)) as usize).clamp(1, signal.len() - window);
        let phase = |start: usize| {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for n in 0..window {
                let hann = 0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / window as f64).cos();
                let arg = std::f64::consts::TAU * f64::from(hz) * n as f64 / f64::from(rate);
                let x = f64::from(signal[start + n]) * hann;
                re += x * arg.cos();
                im -= x * arg.sin();
            }
            im.atan2(re)
        };
        let turned = phase(hop) - phase(0);
        let expected = std::f64::consts::TAU * f64::from(hz) * hop as f64 / f64::from(rate);
        let error = (turned - expected + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU)
            - std::f64::consts::PI;
        ((expected + error) * f64::from(rate) / (std::f64::consts::TAU * hop as f64)) as f32
    }

    fn cents(measured: f32, expected: f32) -> f32 {
        1200.0 * (measured / expected).log2()
    }

    /// A loop at `hz` with a plain pulse put in, run for `samples`.
    fn ring(hz: f32, rate: f32, dispersion: f32, t60: [f32; 2], samples: usize) -> Vec<f32> {
        let mut string = Loop::new();
        string.prepare(rate / 20.0);
        string.set_dispersion(dispersion);
        let gain = |t60: f32| (-LN_1000 / (t60 * hz)).exp();
        string.tune(rate, hz, gain(t60[0]), TAU * 1000.0 / rate, gain(t60[1]), 0);
        // A pulse a seventh of the period long: few partials sit on its nulls.
        let length = (0.13 * rate / hz) as usize;
        let input: Vec<f32> = (0..samples).map(|n| (n < length) as u8 as f32).collect();
        let mut out = vec![0.0; samples];
        for (i, o) in input.chunks(16).zip(out.chunks_mut(16)) {
            string.run(i, o);
        }
        out
    }

    #[test]
    fn fractional_delay_tunes_every_note_to_within_a_cent() {
        for rate in [44_100.0, 48_000.0, 96_000.0] {
            for note in (28..=112).step_by(7) {
                let hz = 440.0 * ((note as f32 - 69.0) / 12.0).exp2();
                let out = ring(hz, rate, 0.0, [5.0, 1.0], rate as usize / 2);
                let measured = partial_hz(&out[rate as usize / 10..], hz, rate);
                let off = cents(measured, hz);
                assert!(off.abs() < 1.0, "note {note} at {rate} Hz: {off} cents");
            }
        }
    }

    #[test]
    fn dispersion_keeps_the_fundamental_in_tune_and_stretches_the_partials() {
        let hz = 82.41;
        let b = 2e-5;
        let coefficient = dispersion_coefficient(b, hz, RATE);
        assert!(coefficient < -0.1, "{coefficient}");
        let out = ring(hz, RATE, coefficient, [5.0, 3.0], RATE as usize);
        let fundamental = partial_hz(&out[4800..], hz, RATE);
        assert!(cents(fundamental, hz).abs() < 1.0, "{fundamental}");
        // The partial it was matched at is sharp by about what stiffness
        // would make it.
        let n = DISPERSION_MATCH_PARTIAL;
        let stiff = n * hz * (1.0 + b * n * n).sqrt();
        let measured = partial_hz(&out[4800..], stiff, RATE);
        let expected_cents = cents(stiff, n * hz);
        let got_cents = cents(measured, n * hz);
        assert!(
            (got_cents - expected_cents).abs() < 0.25 * expected_cents,
            "partial {n} sharp by {got_cents} cents, stiffness says {expected_cents}"
        );
    }

    #[test]
    fn loss_filter_hits_the_fundamental_and_the_treble() {
        let (w0, corner) = (TAU * 110.0 / RATE, TAU * 1000.0 / RATE);
        let loss = Loss::fit(w0, 0.998, corner, 0.95);
        assert!((loss.gain(w0) - 0.998).abs() < 1e-5);
        // A decade above the corner it is within a few percent of the shelf.
        let treble = loss.gain(TAU * 10_000.0 / RATE);
        assert!((treble - 0.95).abs() < 3e-3, "{treble}");
        // Halfway (in loss) near the corner.
        let mid = loss.gain(corner);
        assert!(mid < 0.99 && mid > 0.955, "{mid}");
        // DC gain stays below 1 even for a sustain longer than any string's.
        let long = Loss::fit(w0, 0.999_99, corner, 0.9);
        assert!(long.gain(0.0) < 1.0);
        // A treble loss beyond all reason still leaves a stable filter.
        let dark = Loss::fit(w0, 0.998, corner, 0.0);
        assert!(dark.a1.abs() < 1.0 && dark.gain(3.0) < 0.05);
    }

    #[test]
    fn the_comb_hears_nothing_of_partials_with_a_node_at_its_tap() {
        // A tap a quarter of the way along cancels every fourth partial.
        let hz = 200.0;
        let mut string = Loop::new();
        string.prepare(RATE / 20.0);
        let gain = (-LN_1000 / (5.0 * hz)).exp();
        string.tune(RATE, hz, gain, TAU * 1000.0 / RATE, gain, 0);
        let samples = RATE as usize / 2;
        let length = (0.13 * RATE / hz) as usize;
        let mut heard = vec![0.0; samples];
        let mut out = [0.0; 16];
        for start in (0..samples).step_by(16) {
            let input: Vec<f32> = (start..start + 16)
                .map(|n| (n < length) as u8 as f32)
                .collect();
            string.run(&input, &mut out);
            string.comb(0.25 * string.period(), 1.0, &mut heard[start..start + 16]);
        }
        let level = |hz: f32| {
            let window = &heard[RATE as usize / 4..];
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, &x) in window.iter().enumerate() {
                let arg = std::f64::consts::TAU * f64::from(hz) * n as f64 / f64::from(RATE);
                re += f64::from(x) * arg.cos();
                im += f64::from(x) * arg.sin();
            }
            (re * re + im * im).sqrt()
        };
        let notched = level(4.0 * hz);
        let neighbours = level(3.0 * hz).min(level(5.0 * hz));
        assert!(notched < 0.05 * neighbours, "{notched} vs {neighbours}");
    }
}
