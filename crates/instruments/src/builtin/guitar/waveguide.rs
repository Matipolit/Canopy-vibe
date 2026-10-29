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

use dsp::simd::{F32x4, Mask4};

/// `ln(1000)`: an amplitude decaying at `LN_1000 / t` nepers per second
/// falls by 60 dB in `t` seconds.
pub(super) const LN_1000: f32 = 6.907_755;

/// Shortest line delay the interpolator can read: its four taps sit
/// `delay - 1 ..= delay + 2` samples back, and the newest must already have
/// been written.
pub(super) const MIN_DELAY: f32 = 2.0;

/// A line keeps a copy of its first samples after its end, so the
/// interpolator's four taps, or a stretch of [`MAX_RUN`] readings, can be
/// read in one go wherever they fall.
const MIRROR: usize = MAX_RUN + 8;

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
        self.line = vec![0.0; length + MIRROR];
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
        let (held_whole, held_floor) = dsp::split_index(held);
        let held_h = lagrange(held - held_floor);
        for (&x, out) in input.iter().zip(output.iter_mut()) {
            let (whole, h) = if MOVING {
                let now = d.max(MIN_DELAY);
                let (whole, floor) = dsp::split_index(now);
                d += step;
                (whole, lagrange(now - floor))
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
            if w < MIRROR {
                line[w + mask + 1] = v;
            }
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
        let (whole, floor) = dsp::split_index(distance);
        let frac = distance - floor;
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

/// Loops run side by side by [`run_together`]: as many as a vector
/// register holds.
const LANES: usize = 4;

/// How many samples behind its reading [`Lanes::skewed`] writes a loop's
/// line: a step for the reading and loss filter, one per allpass.
const SKEW: usize = 1 + DISPERSION_STAGES;

/// The longest stretch [`run_together`] runs side by side (a control
/// interval); longer ones run a loop at a time.
const MAX_RUN: usize = 16;

/// A loop for [`run_together`] to run: `input` is added into it and what it
/// carries is written to `output`, as [`Loop::run`] does.
pub(super) struct Job<'a> {
    pub string: &'a mut Loop,
    pub input: &'a [f32],
    pub output: &'a mut [f32],
}

/// The loops of a pass, gathered to run together: at most `CAP`.
pub(super) struct Jobs<'a, const CAP: usize> {
    slots: [Option<Job<'a>>; CAP],
    count: usize,
}

impl<'a, const CAP: usize> Jobs<'a, CAP> {
    pub fn new() -> Self {
        Jobs {
            slots: std::array::from_fn(|_| None),
            count: 0,
        }
    }

    pub fn push(&mut self, job: Job<'a>) {
        self.slots[self.count] = Some(job);
        self.count += 1;
    }

    /// Runs them all (see [`run_together`]).
    pub fn run(mut self, pad: &mut Loop) {
        run_together(&mut self.slots[..self.count], pad);
    }
}

/// Runs every loop in `jobs` as [`Loop::run`] would, [`LANES`] at a time in
/// lockstep where there are enough of them. One loop is a chain of filters
/// each waiting on the one before, which leaves most of a processor idle;
/// four side by side, one per lane of a vector, take about two thirds of the
/// time they would one after another. Each computes exactly what it would
/// alone, to the bit. `pad`, a loop prepared like the others and otherwise
/// unused, fills the lane of a group of three. The jobs are taken.
pub(super) fn run_together(jobs: &mut [Option<Job<'_>>], pad: &mut Loop) {
    // Loops whose pitch holds still read their line with the same
    // interpolator all along, which costs less than one that moves, so
    // they go together, and so do the stiff ones. (An unstable sort: it
    // never allocates.)
    jobs.sort_unstable_by_key(|j| {
        j.as_ref()
            .map(|j| (j.string.delay_step != 0.0, j.string.stages == 0))
    });
    let mut pending = jobs.iter_mut().filter_map(Option::take).peekable();
    while pending.peek().is_some() {
        let mut group: [Option<Job<'_>>; LANES] = std::array::from_fn(|_| pending.next());
        let count = group.iter().flatten().count();
        let alike = group.iter().flatten().all(|j| {
            j.string.mask == pad.mask && j.input.len() <= MAX_RUN && j.output.len() == j.input.len()
        });
        // Fewer than three side by side cost more than one by one.
        if count < 3 || !alike {
            for job in group.iter_mut().flatten() {
                job.string.run(job.input, job.output);
            }
        } else {
            run_group(&mut group, count, pad);
        }
    }
}

fn run_group(group: &mut [Option<Job<'_>>; LANES], count: usize, pad: &mut Loop) {
    let len = group
        .iter()
        .flatten()
        .map(|j| j.input.len())
        .min()
        .unwrap_or(0);
    // The inputs sample by sample, every lane's in one vector.
    let mut rows = [[0.0f32; MAX_RUN]; LANES];
    for (row, job) in rows.iter_mut().zip(group.iter()) {
        if let Some(job) = job {
            row[..len].copy_from_slice(&job.input[..len]);
        }
    }
    let mut inputs = [[0.0f32; LANES]; MAX_RUN];
    for block in (0..MAX_RUN).step_by(4) {
        let samples = F32x4::transpose(rows.each_ref().map(|row| F32x4::load(row, block)));
        for (frame, sample) in inputs[block..].iter_mut().zip(samples) {
            *frame = sample.to_array();
        }
    }
    let moving = group.iter().flatten().any(|j| j.string.delay_step != 0.0);
    let stiff = group
        .iter()
        .flatten()
        .filter(|j| j.string.stages > 0)
        .count();
    let pad: *mut Loop = pad;
    let strings: [*mut Loop; LANES] = std::array::from_fn(|k| match &mut group[k] {
        Some(job) => &mut *job.string as *mut Loop,
        None => pad,
    });
    let mut outputs = [[0.0f32; LANES]; MAX_RUN];
    let (inputs, outputs) = (&inputs[..len], &mut outputs[..len]);
    // SAFETY: the jobs' loops are distinct (each was borrowed mutably), the
    // pad is none of them, and all their lines are as long (checked above).
    unsafe {
        match (moving, stiff) {
            (false, 0) => run_lanes::<false, NONE>(strings, count, inputs, outputs),
            (true, 0) => run_lanes::<true, NONE>(strings, count, inputs, outputs),
            (false, n) if n == count => run_lanes::<false, ALL>(strings, count, inputs, outputs),
            (true, n) if n == count => run_lanes::<true, ALL>(strings, count, inputs, outputs),
            (false, _) => run_lanes::<false, SOME>(strings, count, inputs, outputs),
            (true, _) => run_lanes::<true, SOME>(strings, count, inputs, outputs),
        }
    }
    let mut rows = [[0.0f32; MAX_RUN]; LANES];
    for block in (0..len).step_by(4) {
        let samples: [F32x4; 4] = std::array::from_fn(|i| {
            F32x4::from_array(outputs.get(block + i).copied().unwrap_or([0.0; LANES]))
        });
        for (row, lane) in rows.iter_mut().zip(F32x4::transpose(samples)) {
            row[block..block + 4].copy_from_slice(&lane.to_array());
        }
    }
    for (row, job) in rows.iter().zip(group.iter_mut()) {
        if let Some(job) = job {
            job.output[..len].copy_from_slice(&row[..len]);
        }
    }
}

/// [`lagrange`] for four fractions at once.
#[inline(always)]
fn lagrange4(frac: F32x4) -> [F32x4; 4] {
    let one = F32x4::splat(1.0);
    let (d0, d1, d2, d3) = (frac + one, frac, frac - one, frac - F32x4::splat(2.0));
    let (sixth, half) = (F32x4::splat(1.0 / 6.0), F32x4::splat(0.5));
    [
        -d1 * d2 * d3 * sixth,
        d0 * d2 * d3 * half,
        -d0 * d1 * d3 * half,
        d0 * d1 * d2 * sixth,
    ]
}

/// Which lanes of a group have dispersion: none, all those in use, or some.
const NONE: u8 = 0;
const ALL: u8 = 1;
const SOME: u8 = 2;

/// [`Loop::run_with`] for four loops at once: the same arithmetic in the
/// same order, lane by lane. A lane that isn't moving has a step of 0,
/// which leaves its delay where it is, and one without dispersion passes
/// its allpasses by (`STIFF` says which do; the state of those that don't,
/// which they don't use, isn't kept). Only the first `real` lanes are kept;
/// the rest are padding, whose delay is put out of the way.
///
/// # Safety
/// Each of `strings` must be valid, the first `real` distinct from each
/// other and from the rest, and every line as long as the first's.
#[inline(always)]
unsafe fn run_lanes<const MOVING: bool, const STIFF: u8>(
    strings: [*mut Loop; LANES],
    real: usize,
    inputs: &[[f32; LANES]],
    outputs: &mut [[f32; LANES]],
) {
    // SAFETY: valid pointers (the caller's promise); only read here, and no
    // reference to a loop outlives this.
    let state = |k: usize| unsafe { &*strings[k] };
    let mask = state(0).mask;
    let len = inputs.len().min(outputs.len());
    let lane = |f: &dyn Fn(&Loop) -> f32| F32x4::from_array(std::array::from_fn(|k| f(state(k))));
    // Padding lanes read and write the middle of their line, out of the
    // way of its ends.
    let w: [usize; LANES] =
        std::array::from_fn(|k| if k < real { state(k).write } else { mask / 2 });
    let parked = (MAX_RUN + SKEW + 8) as f32;
    let d = F32x4::from_array(std::array::from_fn(|k| {
        if k < real { state(k).delay } else { parked }
    }));
    let mut lanes = Lanes {
        d,
        step: lane(&|l| l.delay_step),
        b0: lane(&|l| l.loss.b0),
        b1: lane(&|l| l.loss.b1),
        a1: lane(&|l| l.loss.a1),
        coefficient: lane(&|l| l.dispersion),
        stiff: Mask4::from_bools(std::array::from_fn(|k| state(k).stages > 0)),
        lowpassed: lane(&|l| l.loss_state),
        ap_in: std::array::from_fn(|s| lane(&|l| l.allpass_in[s])),
        ap_out: std::array::from_fn(|s| lane(&|l| l.allpass_out[s])),
        // SAFETY: as above; the lines are only written through these
        // pointers from here on.
        lines: std::array::from_fn(|k| unsafe { (*strings[k]).line.as_mut_ptr() }),
        w,
        mask,
        held: [0; LANES],
        held_h: [F32x4::splat(0.0); 4],
    };
    let held = lanes.d.max(F32x4::splat(MIN_DELAY));
    let (held_whole, held_floor) = held.trunc_i32();
    lanes.held = held_whole.map(|w| w as usize);
    lanes.held_h = lagrange4(held - held_floor);

    // The shortest delay over the stretch, with a sample to spare for the
    // step's rounding.
    let end = lanes.d + lanes.step * F32x4::splat(len as f32);
    let shortest = (lanes.d.to_array().into_iter())
        .chain(end.to_array())
        .fold(f32::INFINITY, f32::min);
    // Neither the reads nor the writes wrap around a line in this stretch
    // (reading up to a block of four past it): plain offsets will do.
    let unwrapped = !MOVING
        && (0..LANES).all(|k| {
            let first_read = w[k].wrapping_sub(lanes.held[k] + 2);
            first_read <= mask && w[k] >= MIRROR && w[k] + len + 4 <= mask
        });
    if shortest >= (len + 6) as f32 {
        // SAFETY: passed on from the caller; no sample written is read in
        // this stretch.
        unsafe { lanes.bulk::<MOVING, STIFF>(inputs, outputs) };
    } else if shortest >= (SKEW + 3) as f32 {
        // SAFETY: passed on from the caller; `unwrapped` checked.
        unsafe {
            if unwrapped {
                lanes.skewed::<MOVING, STIFF, true>(inputs, outputs);
            } else {
                lanes.skewed::<MOVING, STIFF, false>(inputs, outputs);
            }
        }
    } else {
        // SAFETY: passed on from the caller.
        unsafe { lanes.in_order::<MOVING, STIFF>(inputs, outputs) };
    }

    let (d, lowpassed) = (lanes.d.to_array(), lanes.lowpassed.to_array());
    let (ap_in, ap_out) = (
        lanes.ap_in.map(F32x4::to_array),
        lanes.ap_out.map(F32x4::to_array),
    );
    for k in 0..real {
        // SAFETY: the first `real` are distinct, and nothing else refers to
        // them now.
        let string = unsafe { &mut *strings[k] };
        if STIFF != NONE && string.stages > 0 {
            for s in 0..DISPERSION_STAGES {
                string.allpass_in[s] = ap_in[s][k];
                string.allpass_out[s] = ap_out[s][k];
            }
        }
        string.write = (w[k] + len) & mask;
        string.delay = d[k];
        // Denormals would slow a decaying loop to a crawl on some CPUs.
        string.loss_state = if lowpassed[k].abs() < 1e-20 {
            0.0
        } else {
            lowpassed[k]
        };
    }
}

/// Four loops' state, lane by lane, while [`run_lanes`] runs them.
struct Lanes {
    d: F32x4,
    step: F32x4,
    b0: F32x4,
    b1: F32x4,
    a1: F32x4,
    coefficient: F32x4,
    stiff: Mask4,
    lowpassed: F32x4,
    ap_in: [F32x4; DISPERSION_STAGES],
    ap_out: [F32x4; DISPERSION_STAGES],
    lines: [*mut f32; LANES],
    /// Where each line is written next, at the start of the stretch.
    w: [usize; LANES],
    mask: usize,
    /// The whole samples of the delay, and the interpolator, while it holds.
    held: [usize; LANES],
    held_h: [F32x4; 4],
}

impl Lanes {
    /// The loops' reading for sample `i` of the stretch, through the loss
    /// filter: each lane's four taps, oldest first, read at once (the
    /// line's mirror keeps them contiguous), then made one vector per tap.
    ///
    /// # Safety
    /// The lines must be valid, and with `UNWRAPPED` the reads must not
    /// wrap around them.
    #[inline(always)]
    unsafe fn interpolate<const MOVING: bool, const UNWRAPPED: bool>(&mut self, i: usize) -> F32x4 {
        let (whole, h) = if MOVING {
            let now = self.d.max(F32x4::splat(MIN_DELAY));
            let (whole, floor) = now.trunc_i32();
            self.d = self.d + self.step;
            (whole.map(|w| w as usize), lagrange4(now - floor))
        } else {
            (self.held, self.held_h)
        };
        let rows: [F32x4; LANES] = std::array::from_fn(|k| {
            let start = (self.w[k] + i).wrapping_sub(whole[k] + 2);
            let start = if UNWRAPPED { start } else { start & self.mask };
            // SAFETY: `start + 3 <= mask + 3`, within the line and its
            // mirror.
            unsafe { F32x4::load_ptr(self.lines[k].add(start)) }
        });
        let [oldest, older, old, newest] = F32x4::transpose(rows);
        h[0] * newest + h[1] * old + h[2] * older + h[3] * oldest
    }

    /// [`Self::interpolate`] through the loss filter.
    ///
    /// # Safety
    /// As for [`Self::interpolate`].
    #[inline(always)]
    unsafe fn read<const MOVING: bool, const UNWRAPPED: bool>(&mut self, i: usize) -> F32x4 {
        // SAFETY: passed on.
        let read = unsafe { self.interpolate::<MOVING, UNWRAPPED>(i) };
        let shelved = self.b0 * read + self.lowpassed;
        self.lowpassed = self.b1 * read - self.a1 * shelved;
        shelved
    }

    /// Allpass `s` of the dispersion filter on `v`.
    #[inline(always)]
    fn allpass<const STIFF: u8>(&mut self, s: usize, v: F32x4) -> F32x4 {
        let (x, y) = (&mut self.ap_in[s], &mut self.ap_out[s]);
        let out = self.coefficient * (v - *y) + *x;
        *x = v;
        *y = out;
        if STIFF == ALL {
            out
        } else {
            F32x4::select(self.stiff, out, v)
        }
    }

    /// `x` into lane `k`'s line at `at` (masked), and its mirror.
    ///
    /// # Safety
    /// The lines must be valid.
    #[inline(always)]
    unsafe fn write_one(&self, k: usize, at: usize, x: f32) {
        let at = at & self.mask;
        // SAFETY: `at <= mask`, and its mirror `at + mask + 1` is within
        // the line when `at < MIRROR`.
        unsafe {
            *self.lines[k].add(at) = x;
            if at < MIRROR {
                *self.lines[k].add(at + self.mask + 1) = x;
            }
        }
    }

    /// Sample `j` of the stretch, `v` lane by lane, into the lines.
    ///
    /// # Safety
    /// The lines must be valid, and with `UNWRAPPED` the writes must not
    /// wrap around them or reach their mirrors.
    #[inline(always)]
    unsafe fn write<const UNWRAPPED: bool>(&self, j: usize, v: [f32; LANES]) {
        let length = self.mask + 1;
        for ((&line, &w), x) in self.lines.iter().zip(&self.w).zip(v) {
            let at = w + j;
            // SAFETY: `at <= mask`, and its mirror `at + length` is within
            // the line when `at < MIRROR`.
            unsafe {
                if UNWRAPPED {
                    *line.add(at) = x;
                } else {
                    let at = at & self.mask;
                    *line.add(at) = x;
                    if at < MIRROR {
                        *line.add(at + length) = x;
                    }
                }
            }
        }
    }

    /// The readings of `blocks` blocks of four samples, the interpolator
    /// holding still: along each lane's line four samples at a time (a
    /// four-tap filter), then turned into one vector per sample.
    ///
    /// # Safety
    /// The lines must be valid.
    #[inline(always)]
    unsafe fn read_ahead(&self, v: &mut [F32x4; MAX_RUN], blocks: usize) {
        // Each lane's interpolator, each coefficient in every slot, and
        // where its oldest tap for the first sample is.
        let h: [[F32x4; 4]; LANES] =
            std::array::from_fn(|k| self.held_h.map(|h| F32x4::splat(h.to_array()[k])));
        let starts: [*const f32; LANES] = std::array::from_fn(|k| {
            let start = self.w[k].wrapping_sub(self.held[k] + 2) & self.mask;
            // SAFETY: `start <= mask`; what's read from it, at most
            // `mask + MAX_RUN + 7`, is within the line and its mirror.
            unsafe { self.lines[k].add(start).cast_const() }
        });
        for b in 0..blocks {
            // Lane by lane, samples `4b..4b + 4` of its reading, then turned
            // so each vector is one sample of every lane.
            let rows: [F32x4; LANES] = std::array::from_fn(|k| {
                // SAFETY: as above.
                let tap = |offset: usize| unsafe { F32x4::load_ptr(starts[k].add(4 * b + offset)) };
                let h = &h[k];
                h[0] * tap(3) + h[1] * tap(2) + h[2] * tap(1) + h[3] * tap(0)
            });
            let samples = F32x4::transpose(rows);
            for (i, sample) in samples.into_iter().enumerate() {
                if 4 * b + i < MAX_RUN {
                    v[4 * b + i] = sample;
                }
            }
        }
    }

    /// When nothing written in the stretch is read back in it: every
    /// reading first (see [`Self::read_ahead`] for an interpolator holding
    /// still), then the loss filter over the stretch and each allpass over
    /// it in turn, then each lane's writes, four samples at a time. The
    /// arithmetic per sample is the same, in the same order.
    ///
    /// # Safety
    /// The lines must be valid, and every delay longer than the stretch
    /// and the block of four the readings round it up to.
    #[inline(always)]
    unsafe fn bulk<const MOVING: bool, const STIFF: u8>(
        &mut self,
        inputs: &[[f32; LANES]],
        outputs: &mut [[f32; LANES]],
    ) {
        let len = inputs.len().min(outputs.len());
        let blocks = len.div_ceil(4);
        let mut v = [F32x4::splat(0.0); MAX_RUN];
        if MOVING {
            // The interpolator changes every sample: read sample by sample.
            for (i, v) in v[..len].iter_mut().enumerate() {
                // SAFETY: passed on.
                *v = unsafe { self.interpolate::<true, false>(i) };
            }
        } else {
            // SAFETY: passed on.
            unsafe { self.read_ahead(&mut v, blocks) };
        }
        for v in &mut v[..len] {
            let read = *v;
            let shelved = self.b0 * read + self.lowpassed;
            self.lowpassed = self.b1 * read - self.a1 * shelved;
            *v = shelved;
        }
        if STIFF != NONE {
            // A pass per allpass: short enough for the processor to start
            // each before the one before has finished.
            for s in 0..DISPERSION_STAGES {
                for v in &mut v[..len] {
                    *v = self.allpass::<STIFF>(s, *v);
                }
            }
        }
        for (j, out) in outputs[..len].iter_mut().enumerate() {
            *out = (v[j] + F32x4::from_array(inputs[j])).to_array();
        }
        for b in 0..blocks {
            let rows: [F32x4; 4] = std::array::from_fn(|i| {
                F32x4::from_array(outputs.get(4 * b + i).copied().unwrap_or([0.0; LANES]))
            });
            let lanes = F32x4::transpose(rows);
            for (k, lane) in lanes.into_iter().enumerate() {
                let first = self.w[k] + 4 * b;
                if 4 * b + 4 <= len && first >= MIRROR && first + 4 <= self.mask + 1 {
                    // SAFETY: within the line, away from its ends.
                    unsafe { lane.store_ptr(self.lines[k].add(first)) };
                } else {
                    for (i, x) in lane.to_array().into_iter().enumerate().take(len - 4 * b) {
                        // SAFETY: masked, and mirrored within the line.
                        unsafe { self.write_one(k, first + i, x) };
                    }
                }
            }
        }
    }

    /// Sample by sample: each reading goes through every filter and into
    /// the line before the next, for delays too short to do otherwise.
    ///
    /// # Safety
    /// The lines must be valid.
    #[inline(always)]
    unsafe fn in_order<const MOVING: bool, const STIFF: u8>(
        &mut self,
        inputs: &[[f32; LANES]],
        outputs: &mut [[f32; LANES]],
    ) {
        for (j, (input, output)) in inputs.iter().zip(outputs.iter_mut()).enumerate() {
            // SAFETY: passed on.
            let mut v = unsafe { self.read::<MOVING, false>(j) };
            if STIFF != NONE {
                for s in 0..DISPERSION_STAGES {
                    v = self.allpass::<STIFF>(s, v);
                }
            }
            let v = (v + F32x4::from_array(*input)).to_array();
            // SAFETY: passed on.
            unsafe { self.write::<false>(j, v) };
            *output = v;
        }
    }

    /// Skewed: each pass through the loop reads and filters sample `i`,
    /// runs allpass `s` on sample `i - 1 - s` and writes sample `i - skew`,
    /// so the filters' recurrences run side by side rather than each
    /// waiting on the whole chain before it. A sample is written before its
    /// turn to be read comes, as every delay is longer than the skew.
    ///
    /// # Safety
    /// The lines must be valid, and with `UNWRAPPED` neither the reads nor
    /// the writes may wrap around them.
    #[inline(always)]
    unsafe fn skewed<const MOVING: bool, const STIFF: u8, const UNWRAPPED: bool>(
        &mut self,
        inputs: &[[f32; LANES]],
        outputs: &mut [[f32; LANES]],
    ) {
        let len = inputs.len().min(outputs.len());
        let skew = if STIFF == NONE { 1 } else { SKEW };
        let mut v = [F32x4::splat(0.0); MAX_RUN];
        // SAFETY (all three): passed on.
        for i in 0..skew {
            unsafe { self.tick::<MOVING, STIFF, UNWRAPPED, true>(i, &mut v, inputs, outputs) };
        }
        for i in skew..len {
            unsafe { self.tick::<MOVING, STIFF, UNWRAPPED, false>(i, &mut v, inputs, outputs) };
        }
        for i in len.max(skew)..len + skew {
            unsafe { self.tick::<MOVING, STIFF, UNWRAPPED, true>(i, &mut v, inputs, outputs) };
        }
    }

    /// One pass of [`Lanes::skewed`]'s loop. With `GUARD` (the first and
    /// last few), the steps that have no sample to work on are skipped.
    ///
    /// # Safety
    /// As for [`Lanes::skewed`].
    #[inline(always)]
    unsafe fn tick<
        const MOVING: bool,
        const STIFF: u8,
        const UNWRAPPED: bool,
        const GUARD: bool,
    >(
        &mut self,
        i: usize,
        v: &mut [F32x4; MAX_RUN],
        inputs: &[[f32; LANES]],
        outputs: &mut [[f32; LANES]],
    ) {
        let len = inputs.len().min(outputs.len());
        let skew = if STIFF == NONE { 1 } else { SKEW };
        if !GUARD || (i >= skew && i - skew < len) {
            let j = i - skew;
            let out = (v[j] + F32x4::from_array(inputs[j])).to_array();
            // SAFETY: passed on.
            unsafe { self.write::<UNWRAPPED>(j, out) };
            outputs[j] = out;
        }
        if STIFF != NONE {
            for s in (0..DISPERSION_STAGES).rev() {
                let j = i.wrapping_sub(1 + s);
                if !GUARD || j < len {
                    v[j] = self.allpass::<STIFF>(s, v[j]);
                }
            }
        }
        if !GUARD || i < len {
            // SAFETY: passed on.
            v[i] = unsafe { self.read::<MOVING, UNWRAPPED>(i) };
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

    #[test]
    fn loops_run_side_by_side_compute_what_they_do_alone() {
        // Loops of every kind a group can hold: held and moving, stiff and
        // not, long and (for the sample-by-sample path) shorter than a
        // stretch, at every write position as they go round.
        let make = |hz: f32, stiffness: f32, ramp: usize| {
            let mut string = Loop::new();
            string.prepare(RATE / 70.0 + 64.0);
            string.set_dispersion(stiffness);
            let gain = |t60: f32| (-LN_1000 / (t60 * hz)).exp();
            string.tune(RATE, hz, gain(4.0), TAU * 1000.0 / RATE, gain(0.8), 0);
            if ramp > 0 {
                string.retarget(RATE, hz * 1.01, ramp);
            }
            string
        };
        let kinds = [
            (82.0, -0.3, 0),
            (110.0, -0.2, 0),
            (147.0, 0.0, 0),
            (196.0, -0.1, 4000),
            (330.0, 0.0, 4000),
            (2600.0, 0.0, 0),
            (5000.0, 0.0, 300),
            (99.0, -0.4, 0),
        ];
        for count in 1..=kinds.len() {
            let mut alone: Vec<Loop> = kinds[..count]
                .iter()
                .map(|&(hz, b, ramp)| make(hz, b, ramp))
                .collect();
            let mut together: Vec<Loop> = kinds[..count]
                .iter()
                .map(|&(hz, b, ramp)| make(hz, b, ramp))
                .collect();
            let mut pad = make(100.0, 0.0, 0);
            let mut seed = 0x1234_5678u32;
            for span in 0..400 {
                // Mostly whole control intervals, sometimes cut short.
                let len = if span % 7 == 3 { 5 } else { MAX_RUN };
                let input: Vec<Vec<f32>> = (0..count)
                    .map(|_| {
                        (0..len)
                            .map(|_| {
                                seed ^= seed << 13;
                                seed ^= seed >> 17;
                                seed ^= seed << 5;
                                if span < 4 {
                                    seed as f32 / u32::MAX as f32 - 0.5
                                } else {
                                    0.0
                                }
                            })
                            .collect()
                    })
                    .collect();
                let mut expected = vec![vec![0.0f32; len]; count];
                for ((string, input), out) in alone.iter_mut().zip(&input).zip(&mut expected) {
                    string.run(input, out);
                }
                let mut got = vec![vec![0.0f32; len]; count];
                let mut jobs = Jobs::<8>::new();
                for ((string, input), out) in together.iter_mut().zip(&input).zip(&mut got) {
                    jobs.push(Job {
                        string,
                        input,
                        output: out,
                    });
                }
                jobs.run(&mut pad);
                for (k, (e, g)) in expected.iter().zip(&got).enumerate() {
                    let same = e.iter().zip(g).all(|(a, b)| a.to_bits() == b.to_bits());
                    assert!(same, "{count} loops, loop {k}, span {span}: {e:?} vs {g:?}");
                }
            }
        }
    }
}
