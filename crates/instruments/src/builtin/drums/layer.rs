//! A layer: one sound of a pad's, from a [`Source`] through drive, a filter
//! with its own envelope, and an amplitude envelope, to a level and a pan.
//! A pad sums its layers; each can also frequency-modulate the next.

use std::f32::consts::FRAC_PI_4;
use std::f32::consts::SQRT_2;

use dsp::{Svf, db_to_gain};

use super::parts::{Crunch, FLOOR, Noise, SILENCE, Sweep, bandpass_norm, velocity_gain};
use super::source::{Osc, Source};

/// A layer's parameters, as offsets into each of a pad's layer blocks (see
/// [`super::Voice::layer_param`]). They are read as a hit starts.
pub struct Layer;

impl Layer {
    /// [`Mode`]: off, on, or heard only through the next layer's FM.
    pub const MODE: usize = 0;
    /// [`Source`].
    pub const SOURCE: usize = 1;
    /// In dB.
    pub const LEVEL: usize = 2;
    /// `-1` (left) to `1` (right), constant power.
    pub const PAN: usize = 3;
    /// `0..=1`: how much velocity changes this layer's level, on top of
    /// the pad's own sensitivity: up, and soft hits lose this layer first.
    pub const VELOCITY: usize = 4;
    /// In Hz: the pitch of an oscillator, the density of crackle, the rate
    /// of digital noise. Moves with the pad's and the kit's Tune.
    pub const PITCH: usize = 5;
    /// `0..=1`, the source's own control (see [`Source::shape`]).
    pub const SHAPE: usize = 6;
    /// In degrees: where the oscillator starts. 0 starts a sine at zero,
    /// without a click; 90 at its peak, with one.
    pub const PHASE: usize = 7;
    /// `0..=1`: how far the layer above (if playing) bends this one's
    /// pitch, sample by sample: frequency modulation.
    pub const FM: usize = 8;
    /// In semitones: where the pitch starts, above Pitch (or below, when
    /// negative), before it falls back.
    pub const PITCH_ENV: usize = 9;
    /// In ms: how long the pitch takes to get 95% of the way back.
    pub const PITCH_TIME: usize = 10;
    /// In ms: how long after the hit the layer starts.
    pub const DELAY: usize = 11;
    /// In ms: the rise to full level; at its minimum, none.
    pub const ATTACK: usize = 12;
    /// In ms: how long it stays at full level; at its minimum, not at all.
    pub const HOLD: usize = 13;
    /// In ms: the fall from full level, to −60 dB.
    pub const DECAY: usize = 14;
    /// `0..=1`: the decay's shape, from exponential (an analog drum's) to
    /// a straight line to silence (a gated, punchier one).
    pub const CURVE: usize = 15;
    /// How many times the envelope strikes (a clap's hands), as an index:
    /// 0 is once.
    pub const BURSTS: usize = 16;
    /// In ms: the time between bursts, which wobbles a little each time.
    pub const GAP: usize = 17;
    /// [`FilterKind`].
    pub const FILTER: usize = 18;
    /// In Hz.
    pub const CUTOFF: usize = 19;
    /// `0..=1`.
    pub const RESONANCE: usize = 20;
    /// In octaves: where the cutoff starts, above Cutoff (or below), before
    /// it sweeps back.
    pub const FILTER_ENV: usize = 21;
    /// In ms: how long the cutoff takes to get 95% of the way back.
    pub const FILTER_TIME: usize = 22;
    /// `0..=1`: how much the cutoff follows the pad's and the kit's Tune:
    /// at 1, a semitone up moves it a semitone up.
    pub const TRACK: usize = 23;
    /// `0..=1`: saturation on the source, before the filter.
    pub const DRIVE: usize = 24;
    /// How many parameters a layer has.
    pub const COUNT: usize = 25;
}

/// Whether a layer plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    On,
    /// Silent itself, but running, so it modulates the next layer.
    FmOnly,
}

impl Mode {
    pub const LABELS: [&str; 3] = ["Off", "On", "FM Only"];

    pub fn from_value(value: f32) -> Mode {
        match value.round() as i32 {
            1 => Mode::On,
            2 => Mode::FmOnly,
            _ => Mode::Off,
        }
    }
}

/// A layer's or a pad's filter. The 6 dB slopes are gentle, without
/// resonance: for rounding off fizz, or thinning out rumble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterKind {
    #[default]
    Off,
    Low6,
    Low12,
    Low24,
    High6,
    High12,
    High24,
    Band,
    Notch,
}

impl FilterKind {
    pub const ALL: [FilterKind; 9] = [
        FilterKind::Off,
        FilterKind::Low6,
        FilterKind::Low12,
        FilterKind::Low24,
        FilterKind::High6,
        FilterKind::High12,
        FilterKind::High24,
        FilterKind::Band,
        FilterKind::Notch,
    ];

    pub const LABELS: [&str; 9] = [
        "Off", "LP 6", "LP 12", "LP 24", "HP 6", "HP 12", "HP 24", "BP", "Notch",
    ];

    /// Whether its Resonance does anything.
    pub fn resonates(self) -> bool {
        !matches!(self, FilterKind::Off | FilterKind::Low6 | FilterKind::High6)
    }

    pub fn from_value(value: f32) -> FilterKind {
        Self::ALL[(value.max(0.0) as usize).min(Self::ALL.len() - 1)]
    }
}

/// How often a filter sweep moves the cutoff, in samples. Each move costs
/// a `tan`; every third of a millisecond is smooth to the ear.
pub(super) const CONTROL_SAMPLES: u32 = 16;

/// One pole, 6 dB/octave, by the same topology-preserving transform as
/// `dsp::Svf`, so that its cutoff lands where asked up to Nyquist.
#[derive(Debug, Clone, Copy, Default)]
struct OnePole {
    /// `g / (1 + g)`, with `g` the prewarped integrator gain.
    gain: f32,
    state: f32,
    sample_rate: f32,
}

impl OnePole {
    fn set_cutoff(&mut self, hz: f32) {
        let hz = hz.clamp(10.0, self.sample_rate * 0.49);
        let g = (std::f32::consts::PI * hz / self.sample_rate).tan();
        self.gain = g / (1.0 + g);
    }

    /// The lowpass output; the highpass is the input minus it.
    #[inline]
    fn lowpass(&mut self, x: f32) -> f32 {
        dsp::one_pole::tpt_lowpass(&mut self.state, self.gain, x)
    }
}

/// A 6, 12 or 24 dB/octave filter: a one-pole, or `dsp::Svf`s (the 24 dB
/// slopes are two in a row, the first without resonance).
#[derive(Debug, Clone)]
pub(super) struct Filter {
    kind: FilterKind,
    stages: [Svf; 2],
    pole: OnePole,
    /// Scales the bandpass to unity at its center, whatever the resonance.
    norm: f32,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            kind: FilterKind::Off,
            stages: [Svf::new(1000.0, 0.0), Svf::new(1000.0, 0.0)],
            pole: OnePole {
                sample_rate: 48_000.0,
                ..OnePole::default()
            },
            norm: 1.0,
        }
    }
}

impl Filter {
    /// Sets it up from silence.
    pub fn start(&mut self, kind: FilterKind, cutoff: f32, resonance: f32, sample_rate: f32) {
        self.kind = kind;
        let doubled = matches!(kind, FilterKind::Low24 | FilterKind::High24);
        let first = if doubled { 0.0 } else { resonance };
        for (stage, resonance) in self.stages.iter_mut().zip([first, resonance]) {
            stage.set_resonance(resonance);
            stage.set_cutoff(cutoff);
            stage.prepare(sample_rate);
        }
        self.pole = OnePole {
            sample_rate,
            ..OnePole::default()
        };
        self.pole.set_cutoff(cutoff);
        self.norm = bandpass_norm(resonance);
    }

    /// Moves the cutoff, keeping the state.
    pub fn set_cutoff(&mut self, hz: f32) {
        match self.kind {
            FilterKind::Off => {}
            FilterKind::Low6 | FilterKind::High6 => self.pole.set_cutoff(hz),
            FilterKind::Low24 | FilterKind::High24 => {
                self.stages[0].set_cutoff(hz);
                self.stages[1].set_cutoff(hz);
            }
            _ => self.stages[0].set_cutoff(hz),
        }
    }

    pub fn set_resonance(&mut self, resonance: f32) {
        let doubled = matches!(self.kind, FilterKind::Low24 | FilterKind::High24);
        self.stages[usize::from(doubled)].set_resonance(resonance);
        self.norm = bandpass_norm(resonance);
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let [first, second] = &mut self.stages;
        match self.kind {
            FilterKind::Off => x,
            FilterKind::Low6 => self.pole.lowpass(x),
            FilterKind::High6 => x - self.pole.lowpass(x),
            FilterKind::Low12 => first.process_sample(x).lowpass,
            FilterKind::Low24 => {
                second
                    .process_sample(first.process_sample(x).lowpass)
                    .lowpass
            }
            FilterKind::High12 => first.process_sample(x).highpass,
            FilterKind::High24 => {
                second
                    .process_sample(first.process_sample(x).highpass)
                    .highpass
            }
            FilterKind::Band => first.process_sample(x).bandpass * self.norm,
            FilterKind::Notch => {
                let out = first.process_sample(x);
                out.lowpass + out.highpass
            }
        }
    }
}

/// How far the gap before each burst varies, either way: hands never clap
/// in perfect time.
const JITTER: f32 = 0.15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Stage {
    #[default]
    Done,
    Waiting,
    Attack,
    Hold,
    Decay,
}

/// Times for an [`Envelope`], in samples.
#[derive(Debug, Clone, Copy)]
pub(super) struct Times {
    pub delay: f32,
    pub attack: f32,
    pub hold: f32,
    pub decay: f32,
    pub gap: f32,
}

/// A layer's amplitude envelope: delay, attack, hold, then a decay from
/// exponential to straight, struck again after each gap for as many
/// bursts as it has.
#[derive(Debug, Clone, Default)]
pub(super) struct Envelope {
    stage: Stage,
    /// Samples left of Waiting or Hold.
    countdown: u32,
    level: f32,
    attack_step: f32,
    hold: u32,
    exp: f32,
    exp_mult: f32,
    lin: f32,
    lin_step: f32,
    curve: f32,
    bursts_left: u32,
    /// Samples until the next burst.
    burst_countdown: u32,
    gap: f32,
    jitter: Noise,
}

impl Envelope {
    /// Starts it: `bursts` strikes (at least one), `curve` from exponential (0) to straight (1).
    pub fn start(&mut self, times: Times, curve: f32, bursts: u32, seed: u32) {
        self.stage = Stage::Waiting;
        self.countdown = times.delay as u32;
        self.level = 0.0;
        // An attack under a sample is none.
        self.attack_step = 1.0 / times.attack.max(1.0);
        self.hold = times.hold as u32;
        let decay = times.decay.max(1.0);
        // 60 dB over the decay.
        self.exp_mult = (-1000.0f32.ln() / decay).exp();
        self.lin_step = 1.0 / decay;
        self.curve = curve.clamp(0.0, 1.0);
        self.bursts_left = bursts.max(1);
        self.gap = times.gap.max(1.0);
        self.jitter = Noise::new(seed);
    }

    /// Samples before it starts.
    pub fn waiting(&self) -> u32 {
        if self.stage == Stage::Waiting {
            self.countdown
        } else {
            0
        }
    }

    /// Skips `samples` of waiting (no more than [`Envelope::waiting`]).
    pub fn skip(&mut self, samples: u32) {
        self.countdown -= samples.min(self.countdown);
    }

    pub fn is_done(&self) -> bool {
        self.stage == Stage::Done
    }

    /// Its level now, `0..=1`.
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Strikes: rises from wherever it is.
    fn strike(&mut self) {
        self.stage = Stage::Attack;
        self.bursts_left -= 1;
        if self.bursts_left > 0 {
            let wobble = 1.0 + JITTER * self.jitter.next();
            self.burst_countdown = (self.gap * wobble).max(1.0) as u32;
        }
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        match self.stage {
            Stage::Done => return 0.0,
            Stage::Waiting => {
                if self.countdown > 0 {
                    self.countdown -= 1;
                    return 0.0;
                }
                self.strike();
            }
            _ => {
                if self.bursts_left > 0 {
                    if self.burst_countdown == 0 {
                        self.strike();
                    } else {
                        self.burst_countdown -= 1;
                    }
                }
            }
        }
        match self.stage {
            Stage::Attack => {
                self.level = (self.level + self.attack_step).min(1.0);
                if self.level >= 1.0 {
                    self.stage = Stage::Hold;
                    self.countdown = self.hold;
                }
            }
            Stage::Hold => {
                if self.countdown == 0 {
                    self.stage = Stage::Decay;
                    self.exp = 1.0;
                    self.lin = 1.0;
                } else {
                    self.countdown -= 1;
                }
            }
            Stage::Decay => {
                self.exp *= self.exp_mult;
                if self.exp < FLOOR {
                    self.exp = 0.0;
                }
                self.lin = (self.lin - self.lin_step).max(0.0);
                self.level = self.exp + self.curve * (self.lin - self.exp);
                if self.level < SILENCE && self.bursts_left == 0 {
                    self.stage = Stage::Done;
                    self.level = 0.0;
                }
            }
            Stage::Done | Stage::Waiting => {}
        }
        self.level
    }
}

/// How far FM at full amount swings the pitch, as a multiple of it, for
/// a modulator at full level.
const FM_DEPTH: f32 = 8.0;

/// How long sources that step run before a hit sounds (see [`Source::steps`]).
const SETTLE_MS: f32 = 1.0;

/// Attacks and holds this short are none at all: the hit starts at full
/// level. (Their knobs used to stop here, so projects keep this for none.)
pub(super) const INSTANT_MS: f32 = 0.1;

/// What a layer is told about the hit it starts.
#[derive(Debug, Clone, Copy)]
pub(super) struct HitInfo {
    pub sample_rate: f32,
    /// Frequency multiplier from the pad's and the kit's Tune and the note
    /// played (some alternative notes play a pad a few semitones off).
    pub ratio: f32,
    /// The pad's Length, as a multiplier of hold and decay times.
    pub length: f32,
    pub velocity: f32,
    pub seed: u32,
}

/// One layer through one hit.
#[derive(Debug, Clone, Default)]
pub(super) struct LayerHit {
    /// Still sounding (or waiting to).
    pub active: bool,
    /// Heard (rather than only modulating the next layer).
    pub audible: bool,
    /// Into the left and right channels.
    pub gains: [f32; 2],
    /// Its level, as a gain, for the meter.
    pub peak: f32,
    osc: Osc,
    pitch: Sweep,
    fm: f32,
    env: Envelope,
    filter: Filter,
    /// The cutoff before its sweep, and the sweep.
    cutoff: f32,
    filter_sweep: Sweep,
    countdown: u32,
    crunch: Crunch,
}

impl LayerHit {
    /// Starts a hit with the layer's parameters `p` (see [`Layer`]).
    pub fn start(&mut self, p: &[f32], hit: &HitInfo) {
        let mode = Mode::from_value(p[Layer::MODE]);
        self.active = mode != Mode::Off;
        if !self.active {
            return;
        }
        self.audible = mode == Mode::On;
        let rate = hit.sample_rate;
        let ms = |ms: f32| ms * 0.001 * rate;
        let decay = p[Layer::DECAY] * hit.length;
        let source = Source::from_value(p[Layer::SOURCE]);
        let mut seeds = Noise::new(hit.seed);
        self.osc.start(
            source,
            p[Layer::PITCH] * hit.ratio,
            p[Layer::SHAPE],
            p[Layer::PHASE] / 360.0,
            decay,
            rate,
            seeds.next_u32(),
        );
        self.pitch
            .start(p[Layer::PITCH_ENV], p[Layer::PITCH_TIME], rate);
        self.fm = FM_DEPTH * p[Layer::FM] * p[Layer::FM];
        self.crunch = Crunch::new(p[Layer::DRIVE]);

        let kind = FilterKind::from_value(p[Layer::FILTER]);
        self.cutoff = p[Layer::CUTOFF] * hit.ratio.powf(p[Layer::TRACK]);
        let control_rate = rate / CONTROL_SAMPLES as f32;
        let octaves = p[Layer::FILTER_ENV];
        self.filter_sweep
            .start(12.0 * octaves, p[Layer::FILTER_TIME], control_rate);
        let first = self.cutoff * self.filter_sweep.next();
        self.filter.start(kind, first, p[Layer::RESONANCE], rate);
        self.countdown = CONTROL_SAMPLES;
        if source.steps() && kind != FilterKind::Off {
            for _ in 0..ms(SETTLE_MS) as usize {
                let x = self.crunch.process(self.osc.next(1.0));
                self.filter.process(x);
            }
        }

        let short = |value: f32| {
            if value <= INSTANT_MS { 0.0 } else { ms(value) }
        };
        let times = Times {
            delay: ms(p[Layer::DELAY]),
            attack: short(p[Layer::ATTACK]),
            hold: short(p[Layer::HOLD] * hit.length),
            decay: ms(decay),
            gap: ms(p[Layer::GAP]),
        };
        let bursts = p[Layer::BURSTS] as u32 + 1;
        self.env
            .start(times, p[Layer::CURVE], bursts, seeds.next_u32());

        let level = db_to_gain(p[Layer::LEVEL]) * velocity_gain(hit.velocity, p[Layer::VELOCITY]);
        // Constant power, scaled so the center is unity in each channel.
        let angle = (p[Layer::PAN] + 1.0) * FRAC_PI_4;
        self.gains = [level * SQRT_2 * angle.cos(), level * SQRT_2 * angle.sin()];
        self.peak = level;
    }

    /// Its envelope now.
    pub fn level(&self) -> f32 {
        self.env.level()
    }

    /// Writes its next `out.len()` samples into `out`, after its envelope
    /// and before its level and pan, with FM from `fm` (the layer above's
    /// output for the same samples) if given. Once it ends, `out` is
    /// zeroed and it stops being active.
    pub fn render(&mut self, fm: Option<&[f32]>, out: &mut [f32]) {
        let waiting = (self.env.waiting() as usize).min(out.len());
        out[..waiting].fill(0.0);
        self.env.skip(waiting as u32);
        let fm = fm.filter(|_| self.fm > 0.0);
        let sweeping = self.filter_sweep.is_moving();
        for i in waiting..out.len() {
            let env = self.env.next();
            let mut ratio = self.pitch.next();
            if let Some(fm) = fm {
                ratio *= 1.0 + self.fm * fm[i];
            }
            let mut x = self.osc.next(ratio);
            if self.crunch.is_on() {
                x = self.crunch.process(x);
            }
            if sweeping {
                self.countdown -= 1;
                if self.countdown == 0 {
                    self.countdown = CONTROL_SAMPLES;
                    self.filter
                        .set_cutoff(self.cutoff * self.filter_sweep.next());
                }
            }
            out[i] = self.filter.process(x) * env;
            if self.env.is_done() {
                out[i + 1..].fill(0.0);
                self.active = false;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn envelope(times: Times, curve: f32, bursts: u32) -> Vec<f32> {
        let mut env = Envelope::default();
        env.start(times, curve, bursts, 1);
        (0..48_000).map(|_| env.next()).collect()
    }

    const PLAIN: Times = Times {
        delay: 0.0,
        attack: 0.0,
        hold: 0.0,
        decay: 4800.0,
        gap: 480.0,
    };

    #[test]
    fn an_instant_envelope_starts_at_full_level_and_falls_60_db_in_its_decay() {
        let out = envelope(PLAIN, 0.0, 1);
        assert_eq!(out[0], 1.0);
        let at_decay = out[4801];
        assert!((at_decay - 0.001).abs() < 1e-4, "{at_decay}");
        assert_eq!(*out.last().unwrap(), 0.0);
    }

    #[test]
    fn delay_attack_and_hold_come_first() {
        let times = Times {
            delay: 100.0,
            attack: 50.0,
            hold: 200.0,
            ..PLAIN
        };
        let out = envelope(times, 0.0, 1);
        assert!(out[..100].iter().all(|s| *s == 0.0));
        assert!((out[124] - 0.5).abs() < 0.03, "halfway up: {}", out[124]);
        assert!(out[150..350].iter().all(|s| *s == 1.0));
        assert!(out[400] < 1.0);
    }

    #[test]
    fn a_straight_curve_reaches_silence_at_its_decay() {
        let out = envelope(PLAIN, 1.0, 1);
        assert!((out[2400] - 0.5).abs() < 0.01, "{}", out[2400]);
        assert_eq!(out[4802], 0.0);
    }

    #[test]
    fn bursts_strike_again_after_each_gap() {
        let times = Times {
            decay: 300.0,
            ..PLAIN
        };
        let out = envelope(times, 0.0, 4);
        let strikes = out.windows(2).filter(|w| w[1] > w[0] + 0.5).count() + 1;
        assert_eq!(strikes, 4);
        let last = out.iter().rposition(|s| *s == 1.0).unwrap();
        assert!((1200..1800).contains(&last), "{last}");
    }

    #[test]
    fn filters_pass_and_stop_where_they_should() {
        // Steady-state gain for a sine at `hz` through `kind` at 1 kHz.
        let gain = |kind, hz: f32| {
            let mut filter = Filter::default();
            filter.start(kind, 1000.0, 0.0, RATE);
            let mut peak = 0.0f32;
            for n in 0..48_000 {
                let x = (std::f32::consts::TAU * hz * n as f32 / RATE).sin();
                let y = filter.process(x);
                if n > 24_000 {
                    peak = peak.max(y.abs());
                }
            }
            peak
        };
        use FilterKind::*;
        assert!(gain(Low6, 100.0) > 0.99 && (gain(Low6, 1000.0) - 0.707).abs() < 0.01);
        assert!(gain(High6, 10_000.0) > 0.99 && (gain(High6, 1000.0) - 0.707).abs() < 0.01);
        // Prewarping squeezes the slope towards Nyquist: under the analog 0.1.
        assert!(gain(Low6, 10_000.0) > 0.07 && gain(Low6, 10_000.0) < 0.1);
        assert!(gain(Low12, 100.0) > 0.95 && gain(Low12, 10_000.0) < 0.02);
        assert!(gain(Low24, 10_000.0) < gain(Low12, 10_000.0) * 0.1);
        assert!(gain(High12, 10_000.0) > 0.95 && gain(High12, 100.0) < 0.02);
        assert!(gain(High24, 100.0) < gain(High12, 100.0) * 0.1);
        assert!((gain(Band, 1000.0) - 1.0).abs() < 0.02 && gain(Band, 30.0) < 0.1);
        assert!(gain(Notch, 1000.0) < 0.02 && gain(Notch, 100.0) > 0.95);
        assert!((gain(Off, 5000.0) - 1.0).abs() < 1e-3);
    }
}
