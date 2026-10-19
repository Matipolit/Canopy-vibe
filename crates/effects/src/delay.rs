//! Delay: echoes at a note value of the song's tempo or a time in ms, with
//! feedback through low and high cut filters, stereo or ping-pong, and a
//! dry/wet mix.
//!
//! Two delay lines read at a whole number of samples, so echoes land exactly
//! and repeats aren't dulled by interpolation. When the time or the tempo
//! changes, the read tap crossfades to the new time (see `line.rs`) instead
//! of jumping or bending the pitch. A modulated time (see "Modulation" in
//! `lib.rs`) moves the tap like a tape head instead: it glides there within
//! a block, read between samples, and the echoes bend in pitch as it goes,
//! never faster than [`MAX_TAP_SLOPE`]. What comes out of the lines goes through
//! the 12 dB/octave low and high cuts, then both to the output and, scaled
//! by the feedback, back in: each repeat is filtered once more than the last.
//! Ping-pong feeds the input in mono to the left line and crosses the
//! feedback over, so echoes alternate sides.

use instruments::ParamDescriptor;

use crate::line::{Ring, Tap};
use crate::{Context, Effect, EffectDescriptor, Values, choice, float};
use dsp::flush;
use dsp::svf::{BUTTERWORTH_Q, Coefs, Shape, SvfState};
use dsp::{MAX_BLOCK, Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.delay",
    name: "Delay",
    description: "Tempo-synced stereo and ping-pong delay",
    category: crate::Category::Space,
    params: PARAMS,
    meters: &[],
    create: || Box::new(Delay::new()),
    ..EffectDescriptor::BASE
};

/// Note values for tempo sync, and their length in quarter notes.
pub const NOTES: [(&str, f64); 12] = [
    ("1/32", 0.125),
    ("1/16 T", 1.0 / 6.0),
    ("1/16", 0.25),
    ("1/16 D", 0.375),
    ("1/8 T", 1.0 / 3.0),
    ("1/8", 0.5),
    ("1/8 D", 0.75),
    ("1/4 T", 2.0 / 3.0),
    ("1/4", 1.0),
    ("1/4 D", 1.5),
    ("1/2", 2.0),
    ("1 bar", 4.0),
];

const NOTE_LABELS: &[&str] = &[
    "1/32", "1/16 T", "1/16", "1/16 D", "1/8 T", "1/8", "1/8 D", "1/4 T", "1/4", "1/4 D", "1/2",
    "1 bar",
];

const PARAMS: &[ParamDescriptor] = &[
    choice("sync", "Sync", &["Tempo", "Free"], 0),
    choice("note", "Time", NOTE_LABELS, 6),
    float("time", "Time (free)", 1.0, 2000.0, 300.0, "ms", true),
    float("feedback", "Feedback", 0.0, 95.0, 40.0, "%", false),
    choice("mode", "Mode", &["Stereo", "Ping-pong"], 1),
    float("low_cut", "Low cut", 20.0, 2000.0, 150.0, "Hz", true),
    float("high_cut", "High cut", 1000.0, 20_000.0, 8000.0, "Hz", true),
    float("mix", "Mix", 0.0, 100.0, 25.0, "%", false),
];

const COUNT: usize = 8;

const SYNC: usize = 0;
const NOTE: usize = 1;
const TIME: usize = 2;
const FEEDBACK: usize = 3;
const MODE: usize = 4;
const LOW_CUT: usize = 5;
const HIGH_CUT: usize = 6;
const MIX: usize = 7;

/// The longest delay is a bar at this tempo...
const SLOWEST_BPM: f64 = 20.0;
/// ...or the longest free time, whichever is longer.
const LONGEST_FREE_MS: f64 = 2000.0;
/// How long the tap takes to crossfade to a new time.
const FADE_MS: f32 = 30.0;
/// How long feedback, mode and mix take to reach a new value.
const SMOOTH_MS: f32 = 20.0;
/// How long the filters take to glide to a new cutoff.
const FILTER_GLIDE_MS: f32 = 30.0;
/// Filter coefficients follow their glide every this many samples.
const CHUNK: usize = 16;
/// The fastest a modulated time moves the tap, in samples per sample: the
/// echoes' pitch bends by at most this share, and never stops.
const MAX_TAP_SLOPE: f32 = 0.9;

pub struct Delay {
    values: Values<COUNT>,
    sample_rate: f32,
    /// Left and right lines, of the longest delay.
    lines: [Ring; 2],
    /// Where both lines are read, as an age (delay minus one sample).
    tap: Tap,
    /// How the time last changed: a modulator's moves glide the tap.
    time_pace: Pace,
    /// Whether the tap has been placed since `prepare` or `reset`; until it
    /// has, it jumps to the first time asked rather than fading to it.
    placed: bool,
    feedback: Smoothed,
    /// 0 for stereo, 1 for ping-pong; in between only while switching.
    ping_pong: Smoothed,
    mix: Smoothed,
    log_low_cut: Smoothed,
    log_high_cut: Smoothed,
    low_cut: Coefs,
    high_cut: Coefs,
    /// Per channel: low cut, then high cut.
    filters: [[SvfState; 2]; 2],
    /// Samples until the filter coefficients next follow their glide.
    countdown: usize,
}

impl Delay {
    pub fn new() -> Delay {
        let mut delay = Delay {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            lines: [Ring::default(), Ring::default()],
            tap: Tap::new(0.0),
            time_pace: Pace::Knob,
            placed: false,
            feedback: Smoothed::new(0.0, SMOOTH_MS),
            ping_pong: Smoothed::new(0.0, SMOOTH_MS),
            mix: Smoothed::new(0.0, SMOOTH_MS),
            log_low_cut: Smoothed::new(0.0, FILTER_GLIDE_MS),
            log_high_cut: Smoothed::new(0.0, FILTER_GLIDE_MS),
            low_cut: Coefs::IDENTITY,
            high_cut: Coefs::IDENTITY,
            filters: [[SvfState::default(); 2]; 2],
            countdown: 0,
        };
        for index in 0..COUNT {
            delay.apply(index, Pace::Knob);
        }
        delay.prepare_smoothers();
        delay.reset();
        delay
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            FEEDBACK => self.feedback.move_to(value / 100.0, pace),
            MODE => self.ping_pong.set_target(value),
            LOW_CUT => self.log_low_cut.move_to(value.log2(), pace),
            HIGH_CUT => self.log_high_cut.move_to(value.log2(), pace),
            MIX => self.mix.move_to(value / 100.0, pace),
            // The time is worked out each block, as it depends on the
            // tempo; this is how it moves there.
            TIME => self.time_pace = pace,
            _ => self.time_pace = Pace::Knob,
        }
    }

    fn prepare_smoothers(&mut self) {
        let sample_rate = self.sample_rate;
        for smoothed in [&mut self.feedback, &mut self.ping_pong, &mut self.mix] {
            smoothed.prepare(sample_rate);
        }
        for smoothed in [&mut self.log_low_cut, &mut self.log_high_cut] {
            smoothed.prepare_every(sample_rate, CHUNK);
        }
        self.tap
            .set_fade_length((FADE_MS * 0.001 * sample_rate) as usize);
        self.tap.set_max_slope(MAX_TAP_SLOPE);
    }

    /// The longest delay in samples at `sample_rate`.
    fn longest(sample_rate: f32) -> usize {
        let seconds = (4.0 * 60.0 / SLOWEST_BPM).max(LONGEST_FREE_MS / 1000.0);
        (seconds * f64::from(sample_rate)).ceil() as usize + 4
    }

    /// Whether the tap glides rather than crossfades: a modulated free
    /// time.
    fn gliding(&self) -> bool {
        self.time_pace == Pace::Modulation && self.values[SYNC] >= 0.5
    }

    /// The delay asked for, in samples: whole ones unless the time is
    /// being modulated.
    fn delay_samples(&self, context: &Context) -> f32 {
        let seconds = if self.values[SYNC] < 0.5 {
            let bpm = if context.bpm.is_finite() && context.bpm > 0.0 {
                context.bpm
            } else {
                120.0
            };
            NOTES[self.values[NOTE] as usize].1 * 60.0 / bpm
        } else {
            f64::from(self.values[TIME]) / 1000.0
        };
        let samples = seconds * f64::from(self.sample_rate);
        let samples = if self.gliding() {
            samples
        } else {
            samples.round()
        };
        // Room for the interpolator past the longest.
        samples.clamp(1.0, (self.lines[0].len() - 3) as f64) as f32
    }

    fn update_filters(&mut self, force: bool) {
        if force || self.log_low_cut.is_smoothing() {
            let hz = self.log_low_cut.tick().exp2();
            self.low_cut = Coefs::new(&Shape::highpass(hz, BUTTERWORTH_Q, self.sample_rate));
        }
        if force || self.log_high_cut.is_smoothing() {
            let hz = self.log_high_cut.tick().exp2();
            self.high_cut = Coefs::new(&Shape::lowpass(hz, BUTTERWORTH_Q, self.sample_rate));
        }
    }
}

impl Default for Delay {
    fn default() -> Self {
        Delay::new()
    }
}

impl Effect for Delay {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        let longest = Delay::longest(sample_rate);
        for line in &mut self.lines {
            line.allocate(longest);
        }
        self.prepare_smoothers();
        self.reset();
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
        self.placed = false;
        let smoothers = [
            &mut self.feedback,
            &mut self.ping_pong,
            &mut self.mix,
            &mut self.log_low_cut,
            &mut self.log_high_cut,
        ];
        for smoothed in smoothers {
            smoothed.settle();
        }
        self.update_filters(true);
        self.filters = [[SvfState::default(); 2]; 2];
        self.countdown = 0;
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: &Context) {
        self.values.played();
        if self.lines[0].len() <= 1 {
            // Not prepared: pass the sound through.
            return;
        }
        let age = self.delay_samples(context) - 1.0;
        if !self.placed {
            self.tap.jump(age);
            self.placed = true;
        } else if self.gliding() {
            self.tap.glide_to(age, MAX_BLOCK);
        } else {
            self.tap.set_target(age);
        }

        let len = left.len().min(right.len());
        let mut start = 0;
        while start < len {
            if self.countdown == 0 {
                self.update_filters(false);
                self.countdown = CHUNK;
            }
            let n = self.countdown.min(len - start);
            let [line_l, line_r] = &mut self.lines;
            let [[low_l, high_l], [low_r, high_r]] = &mut self.filters;
            for i in start..start + n {
                let (x_l, x_r) = (left[i], right[i]);
                let taps = self.tap.advance();
                let wet_l =
                    high_l.tick(&self.high_cut, low_l.tick(&self.low_cut, taps.read(line_l)));
                let wet_r =
                    high_r.tick(&self.high_cut, low_r.tick(&self.low_cut, taps.read(line_r)));

                let feedback = self.feedback.tick();
                let pong = self.ping_pong.tick();
                // Stereo: each side feeds itself. Ping-pong: the input in mono
                // to the left only, and each side feeds the other.
                let in_l = x_l + pong * (0.5 * (x_l + x_r) - x_l);
                let in_r = x_r - pong * x_r;
                let back_l = wet_l + pong * (wet_r - wet_l);
                let back_r = wet_r + pong * (wet_l - wet_r);
                line_l.push(flush(in_l + feedback * back_l));
                line_r.push(flush(in_r + feedback * back_r));

                let mix = self.mix.tick();
                left[i] = x_l * (1.0 - mix) + wet_l * mix;
                right[i] = x_r * (1.0 - mix) + wet_r * mix;
            }
            for state in [low_l, high_l, low_r, high_r] {
                state.flush();
            }
            self.countdown -= n;
            start += n;
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, rms, run, set};

    fn delay(settings: &[(&str, f32)], sample_rate: f32) -> Delay {
        let mut delay = Delay::new();
        delay.prepare(sample_rate, 64);
        let defaults = [
            ("mix", 100.0),
            ("feedback", 0.0),
            ("mode", 0.0),
            ("low_cut", 20.0),
            ("high_cut", 20_000.0),
        ];
        for &(id, value) in defaults.iter().chain(settings) {
            set(&mut delay, id, value);
        }
        delay.reset();
        delay
    }

    fn impulse_response(delay: &mut Delay, len: usize, bpm: f64) -> (Vec<f32>, Vec<f32>) {
        let (mut left, mut right) = (vec![0.0; len], vec![0.0; len]);
        left[0] = 1.0;
        right[0] = 1.0;
        let context = Context {
            bpm,
            ..Context::DEFAULT
        };
        run(delay, &mut left, &mut right, 64, &context);
        (left, right)
    }

    fn loudest(x: &[f32]) -> usize {
        (0..x.len())
            .max_by(|&a, &b| x[a].abs().total_cmp(&x[b].abs()))
            .unwrap()
    }

    #[test]
    fn echoes_land_on_the_expected_sample() {
        let note = |label: &str| NOTES.iter().position(|n| n.0 == label).unwrap() as f32;
        let cases = [
            // 1/8 dotted at 120 BPM: 0.375 s.
            (
                vec![("sync", 0.0), ("note", note("1/8 D"))],
                120.0,
                RATE,
                18_000,
            ),
            // A quarter-note triplet at 93 BPM: 0.4301 s = 20645.16 samples.
            (
                vec![("sync", 0.0), ("note", note("1/4 T"))],
                93.0,
                RATE,
                20_645,
            ),
            (
                vec![("sync", 0.0), ("note", note("1/32"))],
                140.0,
                44_100.0,
                2363,
            ),
            (vec![("sync", 1.0), ("time", 250.0)], 120.0, RATE, 12_000),
            (vec![("sync", 1.0), ("time", 250.0)], 77.0, 44_100.0, 11_025),
            (vec![("sync", 1.0), ("time", 1.0)], 120.0, RATE, 48),
        ];
        for (settings, bpm, sample_rate, expected) in cases {
            let mut delay = delay(&settings, sample_rate);
            let (left, right) = impulse_response(&mut delay, expected + 1000, bpm);
            assert!(
                left[..expected].iter().all(|&s| s == 0.0),
                "{settings:?}: early sound"
            );
            assert_eq!(loudest(&left), expected, "{settings:?}");
            assert_eq!(left, right);
        }
    }

    #[test]
    fn a_bar_at_20_bpm_fits() {
        let bar = NOTES.len() as f32 - 1.0;
        let mut delay = delay(&[("sync", 0.0), ("note", bar)], RATE);
        let (left, _) = impulse_response(&mut delay, 12 * 48_000 + 100, 20.0);
        assert_eq!(loudest(&left), 12 * 48_000);
    }

    /// RMS of the echoes of a 1 kHz burst, one window per repeat.
    fn echo_levels(settings: &[(&str, f32)], repeats: usize) -> (Vec<f64>, Vec<f64>) {
        let mut delay = delay(settings, RATE);
        let period = 4800;
        let len = period * (repeats + 1);
        let mut left = vec![0.0; len];
        for (n, s) in left.iter_mut().take(192).enumerate() {
            let window = (std::f32::consts::PI * n as f32 / 192.0).sin().powi(2);
            *s = window * (std::f32::consts::TAU * 1000.0 * n as f32 / RATE).sin();
        }
        let mut right = left.clone();
        if settings.contains(&("mode", 1.0)) {
            // Ping-pong takes the input in mono anyway; keep it there.
        } else {
            right.iter_mut().for_each(|s| *s *= 0.5);
        }
        run(&mut delay, &mut left, &mut right, 64, &Context::DEFAULT);
        let level = |x: &[f32], k: usize| rms(&x[k * period - 200..k * period + 600]);
        (
            (1..=repeats).map(|k| level(&left, k)).collect(),
            (1..=repeats).map(|k| level(&right, k)).collect(),
        )
    }

    #[test]
    fn each_repeat_is_quieter_by_the_feedback() {
        for feedback in [30.0, 50.0, 90.0] {
            let (left, right) =
                echo_levels(&[("sync", 1.0), ("time", 100.0), ("feedback", feedback)], 6);
            for channel in [&left, &right] {
                for pair in channel.windows(2) {
                    let ratio = pair[1] / pair[0];
                    assert!(
                        (ratio - f64::from(feedback) / 100.0).abs() < 0.005,
                        "{feedback}%: {ratio}"
                    );
                }
            }
            println!("feedback {feedback}%: repeat levels {left:.4?}");
        }
    }

    #[test]
    fn ping_pong_alternates_sides() {
        let settings = [
            ("sync", 1.0),
            ("time", 100.0),
            ("feedback", 50.0),
            ("mode", 1.0),
        ];
        let (left, right) = echo_levels(&settings, 6);
        for k in 0..6 {
            let (near, far) = if k % 2 == 0 {
                (left[k], right[k])
            } else {
                (right[k], left[k])
            };
            // Not exactly zero: the low cut's tail from the last repeat on
            // this side crosses over too.
            assert!(far < 1e-4 * near, "repeat {}: {near} against {far}", k + 1);
            if k > 0 {
                let previous = left[k - 1].max(right[k - 1]);
                assert!((near / previous - 0.5).abs() < 0.005);
            }
        }
    }

    /// Largest sample-to-sample step at the output for a 440 Hz sine, with
    /// `change` applied after a second and a half.
    fn largest_step_through_change(change: impl Fn(&mut Delay) -> f64) -> f32 {
        let mut delay = delay(
            &[
                ("sync", 1.0),
                ("time", 300.0),
                ("feedback", 40.0),
                ("mix", 50.0),
            ],
            RATE,
        );
        let input = crate::testing::sine(440.0, 0.5, RATE as usize * 4, RATE);
        let (mut left, mut right) = (input.clone(), input);
        let mut bpm = 120.0;
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            if block == 1125 {
                bpm = change(&mut delay);
            }
            delay.process(
                l,
                r,
                &Context {
                    bpm,
                    ..Context::DEFAULT
                },
            );
        }
        left.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn changing_the_time_does_not_click() {
        // The steepest a 440 Hz sine of this level can be: half the dry
        // input plus half the loudest the echoes can add up to.
        let wet = 0.5 / (1.0 - 0.4);
        let limit = std::f32::consts::TAU * 440.0 / RATE * (0.5 * 0.5 + 0.5 * wet);
        let steps = [
            largest_step_through_change(|d| {
                d.set_param(TIME, 700.0);
                120.0
            }),
            largest_step_through_change(|d| {
                d.set_param(TIME, 13.0);
                120.0
            }),
            largest_step_through_change(|d| {
                d.set_param(SYNC, 0.0);
                120.0
            }),
            largest_step_through_change(|d| {
                d.set_param(SYNC, 0.0);
                d.set_param(NOTE, 8.0);
                97.3
            }),
        ];
        println!("largest steps {steps:.4?}, a sine this loud can step {limit:.4}");
        for step in steps {
            assert!(step < 1.05 * limit, "{step}");
        }
    }

    #[test]
    fn a_tempo_change_moves_the_echoes() {
        let mut delay = delay(&[("sync", 0.0), ("note", 8.0)], RATE);
        let (left, _) = impulse_response(&mut delay, 1000, 120.0);
        assert!(left.iter().all(|&s| s == 0.0));
        // After the crossfade, an impulse comes back a quarter at 100 BPM later.
        let mut silence = (vec![0.0; 48_000], vec![0.0; 48_000]);
        let context = Context {
            bpm: 100.0,
            ..Context::DEFAULT
        };
        run(&mut delay, &mut silence.0, &mut silence.1, 64, &context);
        let (left, _) = impulse_response(&mut delay, 30_000, 100.0);
        assert_eq!(loudest(&left), 28_800);
    }
}
