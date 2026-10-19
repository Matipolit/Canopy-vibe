//! Frequency Shifter: moves every frequency in the sound up or down by the
//! same number of Hz, after Harald Bode's frequency shifter (Serum 2's
//! "Bode" module), with a delay fed back through it, so each repeat comes
//! back shifted further: the rising or falling spirals of a barber-pole.
//!
//! Unlike a pitch shifter, which multiplies frequencies, a shift adds to
//! them: harmonics stop being multiples of anything, so a few Hz make a
//! slow phasing swirl, tens a metallic clang, hundreds a bell or a robot.
//!
//! **How**: the sound is split into two copies 90° apart at every
//! frequency (an analytic signal), by two chains of all-pass filters
//! (Olli Niemitalo's design of a polyphase IIR Hilbert transformer: four
//! sections in z² each, 90° within 2° from 20 Hz to 20 kHz at 48 kHz, and
//! within half a degree over most of the band; see the test). Multiplied by a cosine and a sine at the shift and summed, one
//! sideband cancels: what's left is the sound moved up (or, the other
//! sign, down). Ring blends in the other sideband too, at equal power: at
//! 100% it's a ring modulator, each frequency split into one above and one
//! below.
//!
//! **Shift** is a share of **Range**, so the same knob sweeps a few Hz
//! finely or kilohertz boldly. **Stereo** turns the right channel's shift
//! against the left's: at 100% one side goes up as the other goes down.
//! **Feedback** sends the output through a delay of **Delay** ms back into
//! the shifter, with **Blur** wobbling that delay a little (chorus, wow and
//! flutter), a 10 Hz high-pass against DC, and loud repeats bent down
//! smoothly so it can't run away. **Mix** blends with the dry sound.
//!
//! **Modulation** (see `lib.rs`): every float glides, at the knob's pace or
//! within a block for a modulator; the oscillators follow the shift every
//! sample without restarting, so sweeping it, even through zero, is smooth;
//! the delay's read position never moves faster than [`MAX_TAP_SLOPE`].
//!
//! The loop also has a gentle low-pass ([`LOOP_HZ`], or less of the sample
//! rate at low ones): near Nyquist and near DC the two copies drift from
//! 90° apart, where a shift can come out up to 3 dB louder than it went
//! in, and the loop must lose on every pass whatever the feedback.

use std::f32::consts::TAU;

use instruments::ParamDescriptor;

use crate::line::{Ring, contain, slew};
use crate::meter::PeakMeter;
use crate::{Category, Context, Effect, EffectDescriptor, MAX_METERS, Values, choice, float};
use dsp::one_pole::{DcBlocker, pole, tpt_lowpass};
use dsp::settle;
use dsp::{Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.frequency_shifter",
    name: "Frequency Shifter",
    description: "Bode-style frequency shifter and ring modulator, with a shifting feedback delay",
    category: Category::Modulation,
    params: PARAMS,
    meters: &[
        "Output peak (linear, 1 = 0 dBFS)",
        "Shift now, left (Hz)",
        "Shift now, right (Hz)",
    ],
    create: || Box::new(FrequencyShifter::new()),
    ..EffectDescriptor::BASE
};

/// The ranges Shift spans, in Hz either way, and their labels.
pub const RANGES: [f32; 4] = [10.0, 100.0, 1000.0, 5000.0];
const RANGE_LABELS: &[&str] = &["±10 Hz", "±100 Hz", "±1 kHz", "±5 kHz"];

const PARAMS: &[ParamDescriptor] = &[
    float("shift", "Shift", -100.0, 100.0, 5.0, "%", false),
    choice("range", "Range", RANGE_LABELS, 2),
    float("ring", "Ring mod", 0.0, 100.0, 0.0, "%", false),
    float("stereo", "Stereo", 0.0, 100.0, 0.0, "%", false),
    float("delay", "Delay", 1.0, 2000.0, 150.0, "ms", true),
    float("feedback", "Feedback", 0.0, 95.0, 0.0, "%", false),
    float("blur", "Blur", 0.0, 100.0, 0.0, "%", false),
    float("mix", "Mix", 0.0, 100.0, 50.0, "%", false),
    float("output", "Output", -24.0, 12.0, 0.0, "dB", false),
];

const COUNT: usize = 9;

/// -100% to 100% of [`RANGE`].
pub const SHIFT: usize = 0;
pub const RANGE: usize = 1;
/// 0%: one sideband (a shift); 100%: both (a ring modulator).
pub const RING: usize = 2;
/// 0%: both sides shift alike; 100%: the right shifts the other way.
pub const STEREO: usize = 3;
pub const DELAY: usize = 4;
pub const FEEDBACK: usize = 5;
pub const BLUR: usize = 6;
pub const MIX: usize = 7;
pub const OUTPUT: usize = 8;

/// Meter slots, in [`DESCRIPTOR`]'s `meters` order.
pub const METER_OUTPUT: usize = 0;
pub const METER_SHIFT_LEFT: usize = 1;
pub const METER_SHIFT_RIGHT: usize = 2;

/// Niemitalo's all-pass coefficients: the two chains whose outputs are 90°
/// apart (the first one sample later).
const CHAIN_A: [f32; 4] = [0.692_387_8, 0.936_065_43, 0.988_229_5, 0.998_748_86];
const CHAIN_B: [f32; 4] = [0.402_192_1, 0.856_171_1, 0.972_290_96, 0.995_288_5];
/// The fastest the delay's read position moves, in samples per sample.
pub const MAX_TAP_SLOPE: f32 = 0.5;
/// How far Blur wobbles the delay at 100%, in ms either way, and how fast.
const BLUR_MS: f32 = 6.0;
const BLUR_HZ: [f32; 2] = [0.71, 1.37];
/// The shortest delay read, in samples.
const MIN_DELAY: f32 = 2.0;
/// Feedback louder than this is bent down smoothly.
const CEILING: f32 = 4.0;
/// The high-pass in the feedback loop, and its low-pass (at most this
/// share of the sample rate).
const DC_HZ: f32 = 10.0;
pub const LOOP_HZ: f32 = 16_000.0;
const LOOP_HIGHEST: f32 = 0.35;
/// How long the floats take to reach a new value; the delay longer, as
/// moving it bends the repeats' pitch.
const SMOOTH_MS: f32 = 20.0;
const DELAY_GLIDE_MS: f32 = 80.0;

/// One chain of four first-order all-passes in z²: each `y[n] =
/// a²·(x[n] + y[n-2]) - x[n-2]`.
#[derive(Debug, Clone, Copy, Default)]
struct Chain {
    /// Per section: the last two inputs and outputs.
    x: [[f32; 2]; 4],
    y: [[f32; 2]; 4],
}

impl Chain {
    #[inline]
    fn tick(&mut self, coefs: &[f32; 4], input: f32) -> f32 {
        let mut x = input;
        for (section, &a) in coefs.iter().enumerate() {
            let [x1, x2] = self.x[section];
            let [y1, y2] = self.y[section];
            let y = a * a * (x + y2) - x2;
            self.x[section] = [x, x1];
            self.y[section] = [y, y1];
            x = y;
        }
        x
    }

    fn settle(&mut self) {
        for v in self.x.iter_mut().chain(&mut self.y).flatten() {
            *v = settle(*v);
        }
    }
}

/// One channel's analytic signal: the two chains, and the first's extra
/// sample of delay.
#[derive(Debug, Clone, Copy, Default)]
struct Hilbert {
    a: Chain,
    b: Chain,
    late: f32,
}

impl Hilbert {
    /// `x` as two signals 90° apart: (in phase, in quadrature).
    #[inline]
    fn tick(&mut self, x: f32) -> (f32, f32) {
        let re = self.late;
        self.late = self.a.tick(&CHAIN_A, x);
        let im = self.b.tick(&CHAIN_B, x);
        (re, im)
    }

    fn settle(&mut self) {
        self.a.settle();
        self.b.settle();
        self.late = settle(self.late);
    }
}

/// The shift in Hz of each side for these parameter values (in
/// [`DESCRIPTOR`]'s order): what the panel shows before any sound.
pub fn shifts(values: &[f32]) -> [f32; 2] {
    let value = |index: usize| crate::param_value(PARAMS, values, index);
    let range = RANGES[(value(RANGE) as usize).min(RANGES.len() - 1)];
    let left = value(SHIFT) / 100.0 * range;
    [left, left * (1.0 - 2.0 * value(STEREO) / 100.0)]
}

pub struct FrequencyShifter {
    values: Values<COUNT>,
    sample_rate: f32,
    hilberts: [Hilbert; 2],
    /// Each side's oscillator, in cycles (0 to 1).
    phases: [f64; 2],
    /// Each side's delay line, where it's read, and the loop's DC
    /// high-pass's last input and output.
    lines: [Ring; 2],
    taps: [f32; 2],
    placed: bool,
    dc: [DcBlocker; 2],
    dc_pole: f32,
    /// The loop's low-pass: its gain and each side's memory.
    loop_gain: f32,
    loop_state: [f32; 2],
    /// The longest delay the lines can be read at, in samples.
    longest: f32,
    /// Blur's LFOs, in cycles.
    blur_phases: [f32; 2],
    /// The shift in Hz (left side), the stereo turn (0 to 1), ring (0 to
    /// 1), the delay (ms), feedback (0 to 0.95), blur (0 to 1), mix, and
    /// output (a gain).
    shift: Smoothed,
    stereo: Smoothed,
    ring: Smoothed,
    delay: Smoothed,
    feedback: Smoothed,
    blur: Smoothed,
    mix: Smoothed,
    output: Smoothed,
    /// The shift of each side last sample, for the meters.
    shift_now: [f32; 2],
    output_peak: PeakMeter,
}

impl FrequencyShifter {
    pub fn new() -> FrequencyShifter {
        let smoothed = |value: f32| Smoothed::new(value, SMOOTH_MS);
        let mut shifter = FrequencyShifter {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            hilberts: [Hilbert::default(); 2],
            phases: [0.0; 2],
            lines: [Ring::default(), Ring::default()],
            taps: [0.0; 2],
            placed: false,
            dc: [DcBlocker::default(); 2],
            dc_pole: 0.0,
            loop_gain: 1.0,
            loop_state: [0.0; 2],
            longest: MIN_DELAY,
            blur_phases: [0.0, 0.5],
            shift: smoothed(0.0),
            stereo: smoothed(0.0),
            ring: smoothed(0.0),
            delay: Smoothed::new(100.0, DELAY_GLIDE_MS),
            feedback: smoothed(0.0),
            blur: smoothed(0.0),
            mix: smoothed(0.0),
            output: smoothed(1.0),
            shift_now: [0.0; 2],
            output_peak: PeakMeter::default(),
        };
        shifter.prepare(48_000.0, 0);
        shifter
    }

    fn smoothers(&mut self) -> [&mut Smoothed; 8] {
        [
            &mut self.shift,
            &mut self.stereo,
            &mut self.ring,
            &mut self.delay,
            &mut self.feedback,
            &mut self.blur,
            &mut self.mix,
            &mut self.output,
        ]
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`. A new range rescales the shift at the knob's pace.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            SHIFT | RANGE => {
                let pace = if index == RANGE { Pace::Knob } else { pace };
                self.shift.move_to(shifts(&self.values)[0], pace);
            }
            RING => self.ring.move_to(value / 100.0, pace),
            STEREO => self.stereo.move_to(value / 100.0, pace),
            DELAY => self.delay.move_to(value, pace),
            FEEDBACK => self.feedback.move_to(value / 100.0, pace),
            BLUR => self.blur.move_to(value / 100.0, pace),
            MIX => self.mix.move_to(value / 100.0, pace),
            OUTPUT => self.output.move_to(dsp::db_to_gain(value), pace),
            _ => {}
        }
    }
}

impl Default for FrequencyShifter {
    fn default() -> Self {
        FrequencyShifter::new()
    }
}

impl Effect for FrequencyShifter {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        let max_ms = PARAMS[DELAY].clamp(f32::MAX) + BLUR_MS;
        let len = (max_ms * 0.001 * sample_rate).ceil() as usize + 4;
        for line in &mut self.lines {
            line.allocate(len);
        }
        self.longest = len as f32 - 3.0;
        self.dc_pole = pole(DC_HZ, sample_rate);
        let corner = LOOP_HZ.min(LOOP_HIGHEST * sample_rate);
        let g = (std::f32::consts::PI * corner / sample_rate).tan();
        self.loop_gain = g / (1.0 + g);
        for smoothed in self.smoothers() {
            smoothed.prepare(sample_rate);
        }
        self.output_peak.prepare(sample_rate);
        for index in 0..COUNT {
            self.apply(index, Pace::Knob);
        }
        self.reset();
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
        for smoothed in self.smoothers() {
            smoothed.settle();
        }
        self.hilberts = [Hilbert::default(); 2];
        self.phases = [0.0; 2];
        self.placed = false;
        self.dc = [DcBlocker::default(); 2];
        self.loop_state = [0.0; 2];
        self.blur_phases = [0.0, 0.5];
        self.shift_now = [0.0; 2];
        self.output_peak.reset();
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.values.played();
        if self.lines[0].len() <= 1 {
            // Not prepared: pass the sound through.
            return;
        }
        let sample_rate = self.sample_rate;
        let per_ms = sample_rate * 0.001;
        let len = left.len().min(right.len());
        let mut output_peak = 0.0f32;
        for n in 0..len {
            let shift = self.shift.tick();
            let stereo = self.stereo.tick();
            let ring = self.ring.tick();
            let delay = self.delay.tick();
            let feedback = self.feedback.tick();
            let blur = self.blur.tick();
            let mix = self.mix.tick();
            let output = self.output.tick();
            let shifts = [shift, shift * (1.0 - 2.0 * stereo)];
            let mut wobble = 0.0;
            for (phase, hz) in self.blur_phases.iter_mut().zip(BLUR_HZ) {
                *phase += hz / sample_rate;
                if *phase >= 1.0 {
                    *phase -= 1.0;
                }
                wobble += 0.5 * (TAU * *phase).sin();
            }
            let wanted =
                ((delay + blur * BLUR_MS * wobble) * per_ms).clamp(MIN_DELAY, self.longest);
            let clean = |x: f32| if x.is_finite() { x } else { 0.0 };
            let dry = [clean(left[n]), clean(right[n])];
            let mut out = [0.0f32; 2];
            for side in 0..2 {
                // What comes back round the loop.
                let at = slew(&mut self.taps[side], wanted, self.placed, MAX_TAP_SLOPE);
                let back = self.lines[side].delayed(at);
                // The loop's low-pass, a trapezoidal one-pole.
                let back = tpt_lowpass(&mut self.loop_state[side], self.loop_gain, back);
                let blocked = self.dc[side].tick(back, self.dc_pole);

                let (re, im) = self.hilberts[side].tick(dry[side] + feedback * blocked);
                let phase = &mut self.phases[side];
                *phase += f64::from(shifts[side] / sample_rate);
                *phase -= phase.floor();
                let (sin, cos) = (std::f64::consts::TAU * *phase).sin_cos();
                let (sin, cos) = (sin as f32, cos as f32);
                // The quadrature copy leads, so this sum is the upper
                // sideband.
                let up = re * cos + im * sin;
                let down = re * cos - im * sin;
                let shifted = up + ring * ((up + down) * std::f32::consts::FRAC_1_SQRT_2 - up);
                self.lines[side].push(settle(contain(shifted, CEILING)));
                out[side] = output * (dry[side] + mix * (shifted - dry[side]));
            }
            self.placed = true;
            self.shift_now = shifts;
            left[n] = out[0];
            right[n] = out[1];
            output_peak = output_peak.max(out[0].abs()).max(out[1].abs());
        }
        for hilbert in &mut self.hilberts {
            hilbert.settle();
        }
        for dc in &mut self.dc {
            dc.flush();
        }
        for v in &mut self.loop_state {
            *v = settle(*v);
        }
        self.output_peak.update(output_peak, len);
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn receive(&mut self, index: usize, value: f32, pace: Pace) {
        if self.values.set(index, value) {
            self.apply(index, pace);
            if self.values.is_fresh() {
                // Cheap: the lines clear in constant time.
                self.reset();
            }
        }
    }

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out[METER_OUTPUT] = self.output_peak.level();
        out[METER_SHIFT_LEFT] = self.shift_now[0];
        out[METER_SHIFT_RIGHT] = self.shift_now[1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, db, noise, peak, rms, run, set, sine};

    fn shifter(settings: &[(&str, f32)]) -> FrequencyShifter {
        let mut shifter = FrequencyShifter::new();
        shifter.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut shifter, id, value);
        }
        shifter.reset();
        shifter
    }

    /// The amplitude of `x`'s component at `freq` Hz, by its DFT there.
    fn amplitude(x: &[f32], freq: f64) -> f64 {
        let step = std::f64::consts::TAU * freq / f64::from(RATE);
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &s) in x.iter().enumerate() {
            re += f64::from(s) * (step * n as f64).cos();
            im -= f64::from(s) * (step * n as f64).sin();
        }
        2.0 * (re * re + im * im).sqrt() / x.len() as f64
    }

    /// A 1 kHz sine through `settings`, settled: each side's last 0.5 s.
    fn tone_through(settings: &[(&str, f32)]) -> (Vec<f32>, Vec<f32>) {
        let input = sine(1000.0, 0.5, 72_000, RATE);
        let mut shifter = shifter(settings);
        let (mut left, mut right) = (input.clone(), input);
        run(&mut shifter, &mut left, &mut right, 64, &Context::DEFAULT);
        (left[48_000..].to_vec(), right[48_000..].to_vec())
    }

    #[test]
    fn the_two_chains_are_ninety_degrees_apart() {
        let mut worst = 0.0f64;
        for freq in [20.0, 50.0, 200.0, 1000.0, 5000.0, 12_000.0, 20_000.0] {
            let mut hilbert = Hilbert::default();
            let input = sine(freq, 0.5, 96_000, RATE);
            let (re, im): (Vec<f32>, Vec<f32>) = input.iter().map(|&x| hilbert.tick(x)).unzip();
            // Over a whole number of cycles at the end: equal levels, and
            // no correlation (90° apart).
            let cycles = (freq * 2.0).floor();
            let span = (cycles * f64::from(RATE) / freq) as usize;
            let (re, im) = (&re[re.len() - span..], &im[im.len() - span..]);
            let dot = re
                .iter()
                .zip(im)
                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                .sum::<f64>();
            let angle = (dot / (rms(re) * rms(im) * span as f64))
                .clamp(-1.0, 1.0)
                .acos()
                .to_degrees();
            let levels = db(rms(re) / rms(im));
            println!("{freq} Hz: {angle:.2}°, levels {levels:+.3} dB");
            worst = worst.max((angle - 90.0).abs());
            assert!(levels.abs() < 0.05, "{freq}");
        }
        assert!(worst < 2.0, "{worst}");
    }

    #[test]
    fn a_shift_moves_every_frequency_by_the_same_hz() {
        // +10% of ±1 kHz: 1 kHz to 1.1 kHz, its mirror at 900 Hz gone.
        let (left, _) = tone_through(&[("shift", 10.0), ("mix", 100.0)]);
        let (up, mirror, original) = (
            amplitude(&left, 1100.0),
            amplitude(&left, 900.0),
            amplitude(&left, 1000.0),
        );
        println!("up {up:.3}, mirror {mirror:.5}, original {original:.5}");
        assert!((up - 0.5).abs() < 0.01);
        assert!(db(mirror / up) < -40.0 && db(original / up) < -40.0);
        // Down, finely: -50% of ±100 Hz.
        let (left, _) = tone_through(&[("shift", -50.0), ("range", 1.0), ("mix", 100.0)]);
        assert!((amplitude(&left, 950.0) - 0.5).abs() < 0.01);
        assert!(amplitude(&left, 1050.0) < 0.005);
    }

    #[test]
    fn ring_mod_keeps_both_sidebands_and_stereo_splits_them() {
        let (left, _) = tone_through(&[("shift", 20.0), ("ring", 100.0), ("mix", 100.0)]);
        let (up, down) = (amplitude(&left, 1200.0), amplitude(&left, 800.0));
        println!("ring: 1.2 kHz {up:.3}, 800 Hz {down:.3}");
        assert!((up - down).abs() < 0.01 && (up - 0.5 / 2f64.sqrt()).abs() < 0.01);
        let (left, right) = tone_through(&[("shift", 20.0), ("stereo", 100.0), ("mix", 100.0)]);
        assert!(amplitude(&left, 1200.0) > 0.45 && amplitude(&right, 800.0) > 0.45);
        assert!(amplitude(&left, 800.0) < 0.01 && amplitude(&right, 1200.0) < 0.01);
    }

    #[test]
    fn feedback_spirals_up_and_dies_away() {
        // 100 Hz shift, 50 ms delay, 70% feedback: a short burst returns
        // at 1.1, then 1.2 kHz, quieter each time.
        let mut shifter = shifter(&[
            ("shift", 100.0),
            ("range", 1.0),
            ("delay", 50.0),
            ("feedback", 70.0),
            ("mix", 100.0),
        ]);
        let mut input = sine(1000.0, 0.5, 2400, RATE);
        input.extend(std::iter::repeat_n(0.0, 96_000));
        let (mut left, mut right) = (input.clone(), input);
        run(&mut shifter, &mut left, &mut right, 64, &Context::DEFAULT);
        let pass = |k: usize| &left[k * 2400 + 300..k * 2400 + 2100];
        let first = amplitude(pass(1), 1200.0);
        let second = amplitude(pass(2), 1300.0);
        println!("repeats: 1.2 kHz {first:.3}, 1.3 kHz {second:.3}");
        assert!(first > 0.2 && second > 0.1 && second < first);
        assert!(peak(&left[left.len() - 4800..]) < 1e-4);
    }

    #[test]
    fn sweeping_the_shift_through_zero_is_smooth() {
        let input = sine(220.0, 0.5, 96_000, RATE);
        let natural = input
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        let mut shifter = shifter(&[("range", 1.0), ("mix", 100.0)]);
        let (mut left, mut right) = (input.clone(), input);
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            let t = block as f32 * 64.0 / RATE;
            shifter.modulate(SHIFT, 100.0 * (TAU * 3.0 * t).sin());
            shifter.process(l, r, &Context::DEFAULT);
        }
        let steepest = left[4800..]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        println!("steepest {steepest:.4}, the input's {natural:.4}");
        assert!(steepest < 1.6 * natural);
    }

    #[test]
    fn mix_zero_is_dry_and_reset_is_silent() {
        let input = noise(9600, 4);
        let mut dry = shifter(&[("mix", 0.0), ("feedback", 90.0)]);
        let right: Vec<f32> = input.iter().map(|s| -0.5 * s).collect();
        let (mut l, mut r) = (input.clone(), right.clone());
        run(&mut dry, &mut l, &mut r, 64, &Context::DEFAULT);
        assert_eq!((l, r), (input.clone(), right));

        let mut wet = shifter(&[("feedback", 90.0), ("mix", 100.0)]);
        let (mut l, mut r) = (input.clone(), input);
        run(&mut wet, &mut l, &mut r, 64, &Context::DEFAULT);
        wet.reset();
        assert_eq!(wet.meter(), 0.0);
        let (mut l, mut r) = (vec![0.0; 4800], vec![0.0; 4800]);
        run(&mut wet, &mut l, &mut r, 64, &Context::DEFAULT);
        assert!(l.iter().chain(&r).all(|&s| s == 0.0));
    }
}
