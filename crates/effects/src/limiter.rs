//! Limiter: a brickwall peak limiter for the end of a mix. Input gain
//! drives the sound into a ceiling it never crosses, measured at the
//! samples or, with true peak on, between them as a converter will
//! reconstruct them. Its meters are the gain reduction and the input and
//! output peaks.
//!
//! For each stretch between two samples, the detector (see
//! `dynamics_truepeak.rs`) gives its peak, and so the gain that would bring
//! it down to the ceiling. That reduction, in dB, is let go at the release
//! rate (an instant attack, the release a one-pole time constant in dB as
//! in the compressor), and the gain it makes is held at its lowest for the
//! lookahead plus [`HOLD_EXTRA`] samples, and averaged over the lookahead,
//! which turns every drop into a straight ramp ending exactly where the
//! peak arrives in the audio, delayed to meet it. Every sample's gain is
//! then at most what any peak within reach needs, so nothing crosses the
//! ceiling; a final clamp at the ceiling only catches rounding. The extra
//! hold keeps the gain flat for a few samples either side of a peak, so the
//! reconstruction around it sees a steady gain rather than the corner of a
//! ramp, which would put the peak back between the samples.
//!
//! Auto release lets go in two stages: a fast one (a third of the release)
//! that recovers quickly after isolated peaks, and a slow one (three times
//! the release) that charges over about [`AUTO_CHARGE_MS`] of sustained
//! limiting, so dense material doesn't pump.
//!
//! Latency: [`latency_samples`], the [`LOOKAHEAD_MS`] lookahead plus the
//! true-peak interpolator's half-width and half the extra hold, 20 samples:
//! 2.4 ms at 48 kHz, 2.5 ms at 44.1 kHz. The same with true peak off, so
//! switching it doesn't move the sound. The engine doesn't compensate for
//! it yet: on the master that only delays the whole mix; on a track it
//! puts that track 2.4 ms late.

use instruments::ParamDescriptor;

use crate::dynamics::{DB_PER_LOG2, coefficient};
use crate::line::Ring;
use crate::meter::{PeakMeter, block_peak};
use crate::{Category, Context, Effect, EffectDescriptor, OFF_ON, Values, choice, float};
use dsp::{Pace, Smoothed};

use crate::lookahead::{self, Lookahead, MIN_GAIN};
use crate::truepeak::TruePeak;

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.limiter",
    name: "Limiter",
    description: "Brickwall limiter with true-peak ceiling and auto release",
    category: Category::Dynamics,
    params: PARAMS,
    meters: &["Gain reduction (dB)", "Input peak (dB)", "Output peak (dB)"],
    latency: |_, sample_rate| latency_samples(sample_rate),
    create: || Box::new(Limiter::new()),
    ..EffectDescriptor::BASE
};

const PARAMS: &[ParamDescriptor] = &[
    float("input", "Input gain", 0.0, 24.0, 0.0, "dB", false),
    float("ceiling", "Ceiling", -12.0, 0.0, -1.0, "dB", false),
    float("release", "Release", 1.0, 1000.0, 100.0, "ms", true),
    choice("auto_release", "Auto release", OFF_ON, 1),
    choice("true_peak", "True peak", OFF_ON, 1),
];

const COUNT: usize = 5;

pub const INPUT: usize = 0;
pub const CEILING: usize = 1;
pub const RELEASE: usize = 2;
pub const AUTO_RELEASE: usize = 3;
pub const TRUE_PEAK: usize = 4;

/// Meter slots, in [`DESCRIPTOR`]'s `meters` order. The input peak is after
/// the input gain, so it compares directly with the ceiling.
pub const METER_REDUCTION: usize = 0;
pub const METER_INPUT_PEAK: usize = 1;
pub const METER_OUTPUT_PEAK: usize = 2;

/// How far ahead the limiter looks, and so how long each drop in gain
/// takes: long enough to turn a peak down smoothly, short enough to keep the
/// latency small.
pub const LOOKAHEAD_MS: f32 = 2.0;
/// How long input gain and ceiling take to reach a new value.
const SMOOTH_MS: f32 = 20.0;
/// Auto release's stages, as multiples of the release set, and how long the
/// slow stage takes to charge.
const AUTO_FAST: f32 = 1.0 / 3.0;
const AUTO_SLOW: f32 = 3.0;
const AUTO_CHARGE_MS: f32 = 300.0;
/// A reduction this small (dB) is none, so a release ends exactly at 1.
const NO_REDUCTION: f32 = 1e-6;
/// True peaks are read 0.05 dB high, more than the detector ever reads
/// low for content up to 0.45 of the sample rate (21.6 kHz at 48 kHz), so
/// the ceiling holds for anything in the audio band.
const TRUE_PEAK_MARGIN: f32 = 1.005_773;

/// How many samples late the limiter's output is at `sample_rate`.
pub fn latency_samples(sample_rate: f32) -> usize {
    lookahead::latency(LOOKAHEAD_MS, sample_rate)
}

pub struct Limiter {
    values: Values<COUNT>,
    sample_rate: f32,
    /// Input gain and ceiling as amplitude factors.
    input: Smoothed,
    ceiling: Smoothed,
    auto_release: bool,
    true_peak: bool,
    /// The per-sample release coefficients of the release set (or auto
    /// release's fast stage) and of auto's slow stage, and `1 -` the slow
    /// stage's charging one.
    fast_keep: f32,
    slow_keep: f32,
    charge_rate: f32,
    detector: TruePeak,
    /// The reduction needed (dB), let go at the release rate; auto
    /// release's slow stage.
    fast: f32,
    slow: f32,
    lookahead: Lookahead,
    /// The audio, delayed to meet its gain.
    lines: [Ring; 2],
    delay: usize,
    /// The gain applied to the latest sample.
    gain: f32,
    /// What the output is clamped to: the ceiling, or while it moves, the
    /// highest it has been since the samples now leaving were measured.
    clamp: f32,
    clamp_hold: usize,
    input_peak: PeakMeter,
    output_peak: PeakMeter,
}

impl Limiter {
    pub fn new() -> Limiter {
        let mut limiter = Limiter {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            input: Smoothed::new(1.0, SMOOTH_MS),
            ceiling: Smoothed::new(1.0, SMOOTH_MS),
            auto_release: true,
            true_peak: true,
            fast_keep: 0.0,
            slow_keep: 0.0,
            charge_rate: 1.0,
            detector: TruePeak::new(),
            fast: 0.0,
            slow: 0.0,
            lookahead: Lookahead::default(),
            lines: [Ring::default(), Ring::default()],
            delay: 0,
            gain: 1.0,
            clamp: 1.0,
            clamp_hold: 0,
            input_peak: PeakMeter::default(),
            output_peak: PeakMeter::default(),
        };
        limiter.prepare(48_000.0, 0);
        limiter
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            INPUT => self.input.move_to(dsp::db_to_gain(value), pace),
            CEILING => {
                let ceiling = dsp::db_to_gain(value);
                self.ceiling.move_to(ceiling, pace);
                // Samples already measured against the old ceiling are
                // still on their way out.
                self.clamp = self.clamp.max(ceiling);
                self.clamp_hold = self.delay + (SMOOTH_MS * 0.001 * self.sample_rate) as usize + 1;
            }
            AUTO_RELEASE => self.auto_release = value >= 0.5,
            TRUE_PEAK => self.true_peak = value >= 0.5,
            _ => {}
        }
        if matches!(index, RELEASE | AUTO_RELEASE) {
            let release = self.values[RELEASE];
            let keep = |ms: f32| coefficient(ms, self.sample_rate);
            if self.auto_release {
                self.fast_keep = keep(release * AUTO_FAST);
                self.slow_keep = keep(release * AUTO_SLOW);
                self.charge_rate = 1.0 - keep(AUTO_CHARGE_MS);
            } else {
                self.fast_keep = keep(release);
            }
        }
    }
}

impl Default for Limiter {
    fn default() -> Self {
        Limiter::new()
    }
}

impl Effect for Limiter {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.delay = latency_samples(sample_rate);
        self.lookahead
            .allocate(lookahead::samples(LOOKAHEAD_MS, sample_rate), 0);
        for line in &mut self.lines {
            line.allocate(self.delay + 1);
        }
        self.input.prepare(sample_rate);
        self.ceiling.prepare(sample_rate);
        self.input_peak.prepare(sample_rate);
        self.output_peak.prepare(sample_rate);
        for index in 0..COUNT {
            self.apply(index, Pace::Knob);
        }
        self.reset();
    }

    fn reset(&mut self) {
        self.input.settle();
        self.ceiling.settle();
        self.clamp = self.ceiling.target();
        self.clamp_hold = 0;
        self.detector.reset();
        self.fast = 0.0;
        self.slow = 0.0;
        self.lookahead.clear();
        for line in &mut self.lines {
            line.clear();
        }
        self.gain = 1.0;
        self.input_peak.reset();
        self.output_peak.reset();
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.values.played();
        let len = left.len().min(right.len());
        let (left, right) = (&mut left[..len], &mut right[..len]);
        let mut loudest_in = 0.0f32;
        for n in 0..len {
            let input = self.input.tick();
            let ceiling = self.ceiling.tick();
            // The last line of defence passes no NaN or infinity on.
            let sane = |x: f32| if x.is_finite() { x } else { 0.0 };
            let (l, r) = (sane(left[n] * input), sane(right[n] * input));
            loudest_in = loudest_in.max(l.abs()).max(r.abs());

            let mut peak = self.detector.push(l, r, self.true_peak);
            if self.true_peak {
                peak *= TRUE_PEAK_MARGIN;
            }
            let (needed, needed_db) = if peak > ceiling {
                let needed = (ceiling / peak).max(MIN_GAIN);
                (needed, -DB_PER_LOG2 * needed.log2())
            } else {
                (1.0, 0.0)
            };
            self.fast = needed_db.max(self.fast * self.fast_keep);
            if self.fast < NO_REDUCTION {
                self.fast = 0.0;
            }
            let reduction = if self.auto_release {
                // Charged from the fast stage, which holds between a
                // waveform's peaks, so it fills up on any sustained limiting.
                self.slow = if self.fast > self.slow {
                    self.slow + (self.fast - self.slow) * self.charge_rate
                } else {
                    self.slow * self.slow_keep
                };
                if self.slow < NO_REDUCTION {
                    self.slow = 0.0;
                }
                self.fast.max(self.slow)
            } else {
                self.fast
            };
            // Never more than this stretch needs, even by a rounding error.
            let envelope = if reduction > 0.0 {
                (-reduction / DB_PER_LOG2).exp2().min(needed)
            } else {
                1.0
            };
            let gain = self.lookahead.push(envelope);

            let [line_l, line_r] = &mut self.lines;
            line_l.push(l);
            line_r.push(r);
            let limit = if self.clamp_hold > 0 {
                self.clamp_hold -= 1;
                self.clamp
            } else {
                self.clamp = ceiling;
                ceiling
            };
            left[n] = (line_l.get(self.delay) * gain).clamp(-limit, limit);
            right[n] = (line_r.get(self.delay) * gain).clamp(-limit, limit);
            self.gain = gain;
        }
        self.input_peak.update(loudest_in, len);
        self.output_peak
            .update(block_peak(left).max(block_peak(right)), len);
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn receive(&mut self, index: usize, value: f32, pace: Pace) {
        if self.values.set(index, value) {
            self.apply(index, pace);
            if self.values.is_fresh() {
                self.reset();
            }
        }
    }

    fn meters(&self, out: &mut [f32; crate::MAX_METERS]) {
        out[METER_REDUCTION] = if self.gain >= 1.0 {
            0.0
        } else {
            -DB_PER_LOG2 * self.gain.log2()
        };
        out[METER_INPUT_PEAK] = self.input_peak.db();
        out[METER_OUTPUT_PEAK] = self.output_peak.db();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, noise, peak, run, set, sine};

    fn limiter(settings: &[(&str, f32)]) -> Limiter {
        let mut limiter = Limiter::new();
        limiter.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut limiter, id, value);
        }
        limiter.reset();
        limiter
    }

    /// The peak of `signal` as reconstructed between its samples: 16 points
    /// per sample from a long Kaiser-windowed sinc, far more precise than
    /// the limiter's own detector.
    fn reconstructed_peak(signal: &[f32]) -> f32 {
        const HALF: isize = 64;
        const STEPS: usize = 16;
        let window = |t: f64| {
            let edge = 1.0 - (t / HALF as f64).powi(2);
            if edge <= 0.0 {
                return 0.0;
            }
            // A Kaiser window, β = 10, by its series.
            let i0 = |x: f64| {
                let (mut sum, mut term) = (1.0, 1.0);
                for k in 1..60 {
                    term *= x / 2.0 / k as f64;
                    sum += term * term;
                }
                sum
            };
            i0(10.0 * edge.sqrt()) / i0(10.0)
        };
        let kernels: Vec<Vec<f64>> = (1..STEPS)
            .map(|step| {
                let fraction = step as f64 / STEPS as f64;
                (-HALF + 1..=HALF)
                    .map(|k| {
                        let t = fraction - k as f64;
                        let x = std::f64::consts::PI * t;
                        x.sin() / x * window(t)
                    })
                    .collect()
            })
            .collect();
        let mut loudest = peak(signal);
        for n in 0..signal.len() {
            for kernel in &kernels {
                let mut sum = 0.0;
                for (k, weight) in (-HALF + 1..=HALF).zip(kernel) {
                    let index = n as isize + k;
                    if index >= 0 && (index as usize) < signal.len() {
                        sum += f64::from(signal[index as usize]) * weight;
                    }
                }
                loudest = loudest.max(sum.abs() as f32);
            }
        }
        loudest
    }

    /// White noise low-passed at 20 kHz by a long windowed sinc: as dense as
    /// noise gets within the audio band, like a mix's cymbals and hiss.
    fn audio_band_noise(len: usize, seed: u32) -> Vec<f32> {
        const HALF: isize = 64;
        let cutoff = 20_000.0 / f64::from(RATE);
        let kernel: Vec<f64> = (-HALF..=HALF)
            .map(|k| {
                let t = k as f64;
                let sinc = if k == 0 {
                    2.0 * cutoff
                } else {
                    (std::f64::consts::TAU * cutoff * t).sin() / (std::f64::consts::PI * t)
                };
                // Blackman window.
                let x = (t / HALF as f64 + 1.0) * std::f64::consts::PI;
                sinc * (0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos())
            })
            .collect();
        let white = noise(len + 2 * HALF as usize, seed);
        let filtered: Vec<f32> = (0..len)
            .map(|n| {
                kernel
                    .iter()
                    .enumerate()
                    .map(|(k, weight)| f64::from(white[n + k]) * weight)
                    .sum::<f64>() as f32
            })
            .collect();
        let loudest = peak(&filtered);
        filtered.iter().map(|s| s / loudest).collect()
    }

    /// Test material, all within the audio band as real recordings are:
    /// dense noise, the classic inter-sample over (a quarter-rate sine
    /// sampled 45° off its peaks), a sweep up to 20 kHz, noise bursts out of
    /// near-silence, and a band-limited square wave, whose ringing peaks
    /// fall between samples.
    fn material() -> Vec<(&'static str, Vec<f32>)> {
        let len = RATE as usize / 2;
        let quarter: Vec<f32> = (0..len)
            .map(|n| {
                (std::f64::consts::FRAC_PI_2 * n as f64 + std::f64::consts::FRAC_PI_4).sin() as f32
            })
            .collect();
        let sweep: Vec<f32> = (0..len)
            .map(|n| {
                let t = n as f64 / f64::from(RATE);
                let seconds = len as f64 / f64::from(RATE);
                // Exponential, 20 Hz to 20 kHz.
                let rate = (1000f64).ln() / seconds;
                let phase = std::f64::consts::TAU * 20.0 * ((rate * t).exp() - 1.0) / rate;
                phase.sin() as f32
            })
            .collect();
        let bursts: Vec<f32> = audio_band_noise(len, 11)
            .iter()
            .enumerate()
            .map(|(n, s)| {
                if (n / 2400) % 3 == 2 {
                    2.0 * s
                } else {
                    0.01 * s
                }
            })
            .collect();
        // Odd harmonics of 1 kHz up to 19 kHz.
        let square: Vec<f32> = (0..len)
            .map(|n| {
                let t = n as f64 / f64::from(RATE);
                (1..=19)
                    .step_by(2)
                    .map(|k| (std::f64::consts::TAU * 1000.0 * k as f64 * t).sin() / k as f64)
                    .sum::<f64>() as f32
            })
            .collect();
        vec![
            ("noise", audio_band_noise(len, 3)),
            ("quarter-rate sine", quarter),
            ("sweep", sweep),
            ("bursts", bursts),
            ("square wave", square),
        ]
    }

    #[test]
    fn the_ceiling_holds_between_samples_too() {
        let mut worst_true = f32::NEG_INFINITY;
        for (name, signal) in material() {
            for (gain, ceiling) in [(0.0, -1.0), (12.0, -0.3), (24.0, -6.0)] {
                for true_peak in [0.0, 1.0] {
                    let mut limiter = limiter(&[
                        ("input", gain),
                        ("ceiling", ceiling),
                        ("true_peak", true_peak),
                        ("release", 50.0),
                    ]);
                    // Followed by silence, so the output ends as the input
                    // did instead of being cut off mid-sound in the delay.
                    let mut padded = signal.clone();
                    padded.resize(signal.len() + 256, 0.0);
                    let (mut left, mut right) = (padded.clone(), padded);
                    run(&mut limiter, &mut left, &mut right, 64, &Context::DEFAULT);
                    let limit = dsp::db_to_gain(ceiling);
                    let sample_peak = peak(&left).max(peak(&right));
                    assert!(
                        sample_peak <= limit,
                        "{name}, +{gain} dB into {ceiling} dB: sample peak {sample_peak}"
                    );
                    if true_peak == 1.0 {
                        let over = dsp::gain_to_db(reconstructed_peak(&left)) - ceiling;
                        worst_true = worst_true.max(over);
                        assert!(
                            over <= 0.0,
                            "{name}, +{gain} dB into {ceiling} dB: true peak {over:+.3} dB over"
                        );
                    }
                }
            }
        }
        println!("closest true peak to the ceiling: {worst_true:+.4} dB");
    }

    /// How far the output's true peak goes over the ceiling (dB, negative
    /// when under) for `signal` at `gain` dB into `ceiling`.
    fn true_peak_over(signal: &[f32], gain: f32, ceiling: f32) -> f32 {
        let mut limiter = limiter(&[
            ("input", gain),
            ("ceiling", ceiling),
            ("true_peak", 1.0),
            ("release", 50.0),
        ]);
        let mut padded = signal.to_vec();
        padded.resize(signal.len() + 256, 0.0);
        let (mut left, mut right) = (padded.clone(), padded);
        run(&mut limiter, &mut left, &mut right, 64, &Context::DEFAULT);
        dsp::gain_to_db(reconstructed_peak(&left)) - ceiling
    }

    #[test]
    fn full_band_noise_is_held_to_within_a_db() {
        // White noise right up to Nyquist: its peaks lean on the top few
        // percent of the band, which no 4× interpolator reconstructs fully
        // (an ITU meter wouldn't see them either). Measured, not promised.
        let over = true_peak_over(&noise(RATE as usize / 4, 3), 6.0, -1.0);
        println!("full-band white noise, +6 dB into -1 dB: true peak {over:+.3} dB");
        assert!(over < 1.5, "{over}");
    }

    #[test]
    fn quiet_sound_passes_untouched_but_late() {
        let mut limiter = limiter(&[("ceiling", -1.0)]);
        let input = sine(440.0, 0.5, 4800, RATE);
        let (mut left, mut right) = (input.clone(), input.clone());
        run(&mut limiter, &mut left, &mut right, 64, &Context::DEFAULT);
        let delay = latency_samples(RATE);
        assert_eq!(&left[delay..], &input[..input.len() - delay]);
        assert!(left[..delay].iter().all(|&s| s == 0.0));
        assert_eq!(limiter.meter(), 0.0);
        println!(
            "latency: {delay} samples, {:.2} ms at 48 kHz",
            delay as f32 / RATE * 1000.0
        );
        assert_eq!(delay, 115);
        assert_eq!(latency_samples(44_100.0), 107);
    }

    /// The gain reduction meter, every 16 samples, for a 1 kHz tone that is
    /// 12 dB over the ceiling for `loud_s` seconds, then 30 dB under it.
    fn reduction_after(settings: &[(&str, f32)], loud_s: f32) -> Vec<f32> {
        let mut limiter = limiter(settings);
        let loud = (loud_s * RATE) as usize;
        let len = loud + RATE as usize;
        let mut input = sine(1000.0, 1.0, len, RATE);
        for s in &mut input[loud..] {
            *s *= 0.008;
        }
        let mut right = input.clone();
        let mut meter = Vec::new();
        for (l, r) in input.chunks_mut(16).zip(right.chunks_mut(16)) {
            limiter.process(l, r, &Context::DEFAULT);
            meter.push(limiter.meter());
        }
        meter
    }

    /// Milliseconds from `from` (in meter readings) until the reduction has
    /// fallen 63% of the way to nothing.
    fn release_ms(meter: &[f32], from: usize) -> f32 {
        let start = meter[from];
        let mark = start * (-1.0f32).exp();
        let readings = meter[from..].iter().position(|&m| m <= mark).unwrap();
        readings as f32 * 16.0 / RATE * 1000.0
    }

    #[test]
    fn release_and_auto_release() {
        let settings = |release: f32, auto: f32| {
            [
                ("ceiling", -12.0),
                ("release", release),
                ("auto_release", auto),
                ("true_peak", 0.0),
            ]
        };
        // The reduction starts to fall once the last loud peak has passed
        // through the lookahead.
        let after_loud = |loud_s: f32| ((loud_s * RATE) as usize + latency_samples(RATE)) / 16 + 1;
        for release in [20.0, 100.0, 400.0] {
            let meter = reduction_after(&settings(release, 0.0), 0.5);
            assert!((meter[after_loud(0.5) - 2] - 12.0).abs() < 0.05);
            let measured = release_ms(&meter, after_loud(0.5));
            println!("release {release} ms: measured {measured:.1} ms");
            assert!(
                (measured - release).abs() < 0.1 * release + 0.5,
                "{measured}"
            );
        }

        // Auto: quick after a short peak, slow after sustained limiting.
        let short = release_ms(
            &reduction_after(&settings(100.0, 1.0), 0.02),
            after_loud(0.02),
        );
        let long = release_ms(
            &reduction_after(&settings(100.0, 1.0), 2.0),
            after_loud(2.0),
        );
        println!("auto release at 100 ms: {short:.1} ms after 20 ms, {long:.1} ms after 2 s");
        assert!(short < 60.0, "{short}");
        assert!(long > 3.0 * short, "{long} against {short}");
    }

    #[test]
    fn meters_read_reduction_and_levels() {
        let mut limiter = limiter(&[("input", 6.0), ("ceiling", -3.0), ("true_peak", 0.0)]);
        let input = sine(1000.0, 0.5, RATE as usize, RATE);
        let (mut left, mut right) = (input.clone(), input);
        run(&mut limiter, &mut left, &mut right, 64, &Context::DEFAULT);
        let mut meters = [0.0; crate::MAX_METERS];
        limiter.meters(&mut meters);
        println!("meters: {:?}", &meters[..3]);
        // -6 dB in, 6 dB of gain: 0 dB into a -3 dB ceiling.
        assert!((meters[METER_REDUCTION] - 3.0).abs() < 0.05);
        assert!(meters[METER_INPUT_PEAK].abs() < 0.05);
        assert!((meters[METER_OUTPUT_PEAK] + 3.0).abs() < 0.05);
    }

    #[test]
    fn silence_stays_silent() {
        let mut limiter = limiter(&[("input", 24.0), ("ceiling", -12.0)]);
        let (mut left, mut right) = (vec![0.0; 4800], vec![0.0; 4800]);
        run(&mut limiter, &mut left, &mut right, 64, &Context::DEFAULT);
        assert!(left.iter().chain(&right).all(|&s| s == 0.0));
        // Garbage in: nothing out but sound within the ceiling.
        let mut garbage = vec![f32::NAN, f32::INFINITY, -1e30, 0.5, f32::NEG_INFINITY];
        let mut other = garbage.clone();
        limiter.process(&mut garbage, &mut other, &Context::DEFAULT);
        assert!(garbage.iter().all(|s| s.is_finite() && s.abs() <= 0.26));
    }
}
