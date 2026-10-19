//! What a layer sounds from: oscillators, modal resonators, and noises.
//!
//! Every source plays at a pitch (the noises that have none ignore it) and
//! has a Shape control of its own, from 0 to 1; [`Source::shape`] names it.
//! Pitched sources take their frequency as a multiplier each sample, which
//! is how pitch sweeps and FM reach them.

use std::f32::consts::TAU;

use super::parts::{FLOOR, Noise, fract, sin_cycles, t60_mult, wrap};
use dsp::poly_blep as blep;

/// Every source a layer can play.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    /// Shape: feedback, which brightens it towards a saw.
    #[default]
    Sine,
    /// Shape: wavefolding, for more and more harmonics.
    Triangle,
    /// Shape: hard sync to a second oscillator up to two octaves above.
    Saw,
    /// Shape: pulse width, from square to a thin pulse.
    Square,
    /// Six square waves at inharmonic ratios, the TR-808's cymbal and hat
    /// source. Pitch is the lowest; Shape spreads the others (0.5 is the
    /// 808's ratios).
    Metal,
    /// A drum head's modes: the circular membrane's six lowest. Shape damps
    /// the upper ones sooner.
    Membrane,
    /// A bell's or metal bar's partials. Shape damps the upper ones sooner.
    Bell,
    /// Equal energy at every frequency: bright hiss.
    White,
    /// −3 dB an octave: as much in each octave, the even rush of a cymbal.
    Pink,
    /// −6 dB an octave: rumble and thunder.
    Brown,
    /// +3 dB an octave: fizz.
    Blue,
    /// +6 dB an octave: thin, glassy hiss.
    Violet,
    /// Random clicks, Pitch of them a second. Shape smears each into a
    /// short decay, from dry crackle to a rain of grains.
    Crackle,
    /// White noise held for Pitch-rate steps, like a sample rate reducer:
    /// pitched, chippy noise. Shape crushes it to fewer bits.
    Digital,
    /// A few cycles of a sine at Pitch, then nothing: a click or a thump.
    /// Shape sets how many, from half a cycle to four.
    Impulse,
}

impl Source {
    pub const ALL: [Source; 15] = [
        Source::Sine,
        Source::Triangle,
        Source::Saw,
        Source::Square,
        Source::Metal,
        Source::Membrane,
        Source::Bell,
        Source::White,
        Source::Pink,
        Source::Brown,
        Source::Blue,
        Source::Violet,
        Source::Crackle,
        Source::Digital,
        Source::Impulse,
    ];

    /// Their names, in the order of the Source parameter's values.
    pub const LABELS: [&str; 15] = [
        "Sine",
        "Triangle",
        "Saw",
        "Square",
        "Metal",
        "Membrane",
        "Bell",
        "White Noise",
        "Pink Noise",
        "Brown Noise",
        "Blue Noise",
        "Violet Noise",
        "Crackle",
        "Digital Noise",
        "Impulse",
    ];

    /// The source a Source parameter's value selects.
    pub fn from_value(value: f32) -> Source {
        Self::ALL[(value.max(0.0) as usize).min(Self::ALL.len() - 1)]
    }

    pub fn label(self) -> &'static str {
        Self::LABELS[self as usize]
    }

    /// What its Shape control does, if it has one.
    pub fn shape(self) -> Option<&'static str> {
        match self {
            Source::Sine => Some("Feedback"),
            Source::Triangle => Some("Fold"),
            Source::Saw => Some("Sync"),
            Source::Square => Some("Width"),
            Source::Metal => Some("Spread"),
            Source::Membrane | Source::Bell => Some("Damping"),
            Source::Crackle => Some("Smear"),
            Source::Digital => Some("Crush"),
            Source::Impulse => Some("Cycles"),
            Source::White | Source::Pink | Source::Brown | Source::Blue | Source::Violet => None,
        }
    }

    /// What its Pitch control sets, if anything: a pitch, or how often
    /// something happens.
    pub fn pitch(self) -> Option<&'static str> {
        match self {
            Source::Crackle => Some("Density"),
            Source::Digital => Some("Rate"),
            Source::White | Source::Pink | Source::Brown | Source::Blue | Source::Violet => None,
            _ => Some("Pitch"),
        }
    }

    /// Whether it starts where its Phase control says. Metal's oscillators
    /// start at random phases, as free-running ones would; the noises have
    /// no phase.
    pub fn has_phase(self) -> bool {
        matches!(
            self,
            Source::Sine
                | Source::Triangle
                | Source::Saw
                | Source::Square
                | Source::Membrane
                | Source::Bell
                | Source::Impulse
        )
    }

    /// Whether it's one of the noises (whose Pitch does nothing).
    pub fn is_noise(self) -> bool {
        self.pitch().is_none()
    }

    /// Whether its waveform steps, which a highpass would turn into a click
    /// as it starts: those run a moment before the hit, as the TR-808's
    /// oscillators run all the time.
    pub(super) fn steps(self) -> bool {
        matches!(self, Source::Metal | Source::Square)
    }
}

/// The TR-808's six oscillators (205.3, 304.4, 369.6, 522.7, 540 and
/// 800 Hz) as ratios to the lowest.
const METAL_RATIOS: [f32; 6] = [1.0, 1.4827, 1.8003, 2.5460, 2.6303, 3.8967];

/// The modes of a circular membrane struck off center, (0,1) (1,1) (2,1)
/// (0,2) (3,1) (1,2), with how strongly each sounds.
const MEMBRANE_RATIOS: [f32; 6] = [1.0, 1.594, 2.136, 2.296, 2.653, 2.918];
const MEMBRANE_LEVELS: [f32; 6] = [1.0, 0.55, 0.4, 0.3, 0.22, 0.16];

/// A bell's partials, close to a cymbal bell's and a cowbell's.
const BELL_RATIOS: [f32; 6] = [1.0, 2.136, 3.218, 4.68, 5.94, 7.36];
const BELL_LEVELS: [f32; 6] = [0.6, 1.0, 0.75, 0.55, 0.35, 0.2];

/// At full Damping, a mode an octave above the lowest dies this many times
/// faster than the layer's decay (and higher ones faster still).
const DAMPING: f32 = 6.0;

/// Feedback at full Shape, in cycles of phase per unit of output: about as
/// far as a sine goes before it's a saw.
const FEEDBACK: f32 = 0.22;

/// Triangle's fold: the input gain at full Shape.
const FOLDS: f32 = 4.0;

/// Saw's sync: the second oscillator's pitch at full Shape, as a ratio.
const SYNC_RATIO: f32 = 4.0;

/// Square's thinnest pulse, as a share of the cycle.
const MIN_WIDTH: f32 = 0.03;

/// Sine and Saw: below this, a DC blocker takes the offset that feedback
/// and sync leave.
const DC_BLOCK_HZ: f32 = 5.0;

/// Crackle's smear at full Shape: each click decays over about this long.
const SMEAR_MS: f32 = 3.0;

/// Digital's crush: the bits at full Shape, against 16 at none.
const MIN_BITS: f32 = 1.0;

/// Gains that give each noise the RMS of white noise (0.577).
const PINK_GAIN: f32 = 0.334;
const BROWN_GAIN: f32 = 3.16;
const BLUE_GAIN: f32 = 1.67;
const VIOLET_GAIN: f32 = 0.707;

/// The highest frequency an oscillator plays, as a share of the sample
/// rate; above it its harmonics would only alias.
const MAX_INC: f32 = 0.45;

/// One source's state through a hit.
#[derive(Debug, Clone, Default)]
pub(super) struct Osc {
    source: Source,
    /// Cycles per sample at the layer's pitch, before sweeps and FM.
    inc: f32,
    phase: f32,
    shape: f32,
    /// Sine: the last two outputs, which feed back.
    last: [f32; 2],
    /// Saw: the synced oscillator's phase and pitch ratio; and the previous
    /// sample, which goes out one sample late so that a step found now can
    /// still correct it.
    slave: f32,
    ratio: f32,
    held: f32,
    /// Square: the pulse's width, and the offset that centers it. Sine and
    /// Saw: the DC blocker's coefficient (0 for none).
    width: f32,
    dc: f32,
    /// Metal, Membrane and Bell: each partial's phase, ratio, level and
    /// per-sample damping.
    phases: [f32; 6],
    ratios: [f32; 6],
    levels: [f32; 6],
    damps: [f32; 6],
    noise: Noise,
    /// The noises' filter states, and the last sample for differences.
    state: [f32; 3],
    prev: f32,
    /// Digital: the size of one step of the crushed signal (0 for none).
    step: f32,
    /// Impulse: the phase where it stops.
    end: f32,
}

impl Osc {
    /// Starts a hit at `hz` with `shape` (`0..=1`) from `phase` (in cycles).
    /// `decay_ms` is the layer's decay, which modal damping is relative to.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &mut self,
        source: Source,
        hz: f32,
        shape: f32,
        phase: f32,
        decay_ms: f32,
        sample_rate: f32,
        seed: u32,
    ) {
        self.source = source;
        self.inc = hz / sample_rate;
        self.shape = shape;
        self.phase = fract(phase.max(0.0));
        self.noise = Noise::new(seed);
        self.last = [0.0; 2];
        self.state = [0.0; 3];
        self.prev = 0.0;
        self.held = 0.0;
        // Feedback and sync at ratios that aren't whole leave an offset,
        // which a highpass far below hearing takes out.
        self.dc = if shape > 0.0 {
            1.0 - TAU * DC_BLOCK_HZ / sample_rate
        } else {
            0.0
        };
        match source {
            Source::Saw => {
                self.ratio = 1.0 + (SYNC_RATIO - 1.0) * shape;
                // The saw is 0 at phase 0 and rising, as the sine is.
                self.slave = fract(self.phase + 0.5);
                self.held = 2.0 * self.slave - 1.0;
            }
            Source::Square => {
                self.width = 0.5 - (0.5 - MIN_WIDTH) * shape;
                self.dc = 2.0 * self.width - 1.0;
                // Narrow pulses swing from about 0 to 2 once centered: back to 1.
                self.levels[0] = 1.0 / (1.0 + self.dc.abs());
            }
            Source::Metal => {
                for (i, ratio) in METAL_RATIOS.iter().enumerate() {
                    self.ratios[i] = 1.0 + (ratio - 1.0) * 2.0 * shape;
                    self.phases[i] = self.noise.next_phase();
                }
            }
            Source::Membrane | Source::Bell => {
                let (ratios, levels) = if source == Source::Membrane {
                    (MEMBRANE_RATIOS, MEMBRANE_LEVELS)
                } else {
                    (BELL_RATIOS, BELL_LEVELS)
                };
                let total: f32 = levels.iter().sum();
                for i in 0..6 {
                    self.ratios[i] = ratios[i];
                    self.levels[i] = levels[i] / total;
                    self.phases[i] = self.phase;
                    // Damping grows with distance from the lowest mode,
                    // which rings as long as the layer's envelope lets it.
                    let rate = shape * DAMPING * (ratios[i] - ratios[0]);
                    self.damps[i] = if rate > 0.0 {
                        t60_mult(decay_ms / rate, sample_rate)
                    } else {
                        1.0
                    };
                }
            }
            Source::Crackle => {
                // Each click decays by this much a sample.
                self.step = if shape > 0.0 {
                    t60_mult(SMEAR_MS * shape, sample_rate)
                } else {
                    0.0
                };
            }
            Source::Digital => {
                let bits = 16.0 - (16.0 - MIN_BITS) * shape;
                self.step = if shape > 0.0 {
                    1.0 / (bits - 1.0).exp2()
                } else {
                    0.0
                };
                // Something to hold from the first sample.
                let x = self.noise.next();
                self.held = self.crush(x);
            }
            Source::Impulse => {
                self.end = self.phase + 0.5 + 3.5 * shape;
            }
            _ => {}
        }
    }

    fn crush(&self, x: f32) -> f32 {
        if self.step > 0.0 {
            (x / self.step).round() * self.step
        } else {
            x
        }
    }

    /// The next sample, with the pitch multiplied by `ratio` (sweeps and
    /// FM; FM can make it negative, which the sines follow through zero and
    /// the others stop at).
    #[inline]
    pub fn next(&mut self, ratio: f32) -> f32 {
        match self.source {
            Source::Sine => {
                // Feedback FM, from the mean of the last two outputs, as the
                // DX7 does, so that it settles instead of hunting.
                let feedback = FEEDBACK * self.shape * 0.5 * (self.last[0] + self.last[1]);
                let y = sin_cycles(self.phase + feedback);
                self.last = [y, self.last[0]];
                self.phase = wrap(self.phase + (self.inc * ratio).clamp(-MAX_INC, MAX_INC));
                self.block_dc(y)
            }
            Source::Triangle => {
                let t = self.phase;
                // From 0, rising, as the sine.
                let tri = if t < 0.25 {
                    4.0 * t
                } else if t < 0.75 {
                    2.0 - 4.0 * t
                } else {
                    4.0 * t - 4.0
                };
                self.phase = wrap(t + (self.inc * ratio).clamp(-MAX_INC, MAX_INC));
                if self.shape > 0.0 {
                    fold(tri * (1.0 + (FOLDS - 1.0) * self.shape))
                } else {
                    tri
                }
            }
            Source::Saw => self.saw(ratio),
            Source::Square => {
                let dt = (self.inc * ratio).clamp(0.0, MAX_INC);
                let (t, w) = (self.phase, self.width);
                let naive = if t < w { 1.0 } else { -1.0 };
                let fall = fract(t - w + 1.0);
                let out = naive + blep(t, dt) - blep(fall, dt) - self.dc;
                self.phase = wrap(t + dt);
                out * self.levels[0]
            }
            Source::Metal => {
                let base = self.inc * ratio;
                let mut sum = 0.0;
                for (phase, r) in self.phases.iter_mut().zip(self.ratios) {
                    let dt = (base * r).clamp(0.0, MAX_INC);
                    let t = *phase;
                    let (naive, half) = if t < 0.5 {
                        (1.0, t + 0.5)
                    } else {
                        (-1.0, t - 0.5)
                    };
                    sum += naive + blep(t, dt) - blep(half, dt);
                    *phase = wrap(t + dt);
                }
                sum * (1.0 / 6.0)
            }
            Source::Membrane | Source::Bell => {
                let base = self.inc * ratio;
                let mut sum = 0.0;
                for i in 0..6 {
                    let level = self.levels[i];
                    if level == 0.0 {
                        continue;
                    }
                    let inc = base * self.ratios[i];
                    // Above the highest pitch it would only alias: silent.
                    if inc.abs() < MAX_INC {
                        sum += sin_cycles(self.phases[i]) * level;
                    }
                    self.phases[i] = wrap(self.phases[i] + inc.clamp(-MAX_INC, MAX_INC));
                    let level = level * self.damps[i];
                    self.levels[i] = if level < FLOOR { 0.0 } else { level };
                }
                sum
            }
            Source::White => self.noise.next(),
            Source::Pink => self.pink(),
            Source::Brown => {
                // A leaky integrator: −6 dB an octave down to about 15 Hz.
                let [b, ..] = &mut self.state;
                *b = 0.998 * *b + 0.02 * self.noise.next();
                *b * BROWN_GAIN
            }
            Source::Blue => {
                let pink = self.pink();
                let out = (pink - self.prev) * BLUE_GAIN;
                self.prev = pink;
                out
            }
            Source::Violet => {
                let white = self.noise.next();
                let out = (white - self.prev) * VIOLET_GAIN;
                self.prev = white;
                out
            }
            Source::Crackle => {
                // A click with probability density / sample rate.
                let chance = (self.inc * ratio).clamp(0.0, 1.0);
                let click = if self.noise.next_phase() < chance {
                    self.noise.next()
                } else {
                    0.0
                };
                self.prev = self.prev * self.step + click;
                self.prev
            }
            Source::Digital => {
                self.phase += (self.inc * ratio).max(0.0);
                if self.phase >= 1.0 {
                    self.phase = fract(self.phase);
                    let x = self.noise.next();
                    self.held = self.crush(x);
                }
                self.held
            }
            Source::Impulse => {
                if self.phase >= self.end {
                    return 0.0;
                }
                let out = sin_cycles(self.phase);
                self.phase += (self.inc * ratio).clamp(0.0, MAX_INC);
                out
            }
        }
    }

    /// Paul Kellet's economy pink filter on white noise: within 0.5 dB of
    /// −3 dB an octave across the audio band.
    #[inline]
    fn pink(&mut self) -> f32 {
        let white = self.noise.next();
        let [b0, b1, b2] = &mut self.state;
        *b0 = 0.99765 * *b0 + white * 0.099_046;
        *b1 = 0.963 * *b1 + white * 0.296_516_4;
        *b2 = 0.57 * *b2 + white * 1.052_691_3;
        (*b0 + *b1 + *b2 + white * 0.1848) * PINK_GAIN
    }

    /// A saw hard-synced to an oscillator at the layer's pitch: when that
    /// one completes a cycle, the saw restarts from 0 wherever it was. Both
    /// its own steps and the restarts get a PolyBLEP correction; since a
    /// restart is only found once it has happened, the output is the
    /// previous sample, corrected then.
    #[inline]
    fn saw(&mut self, ratio: f32) -> f32 {
        let dt = (self.inc * ratio).clamp(0.0, MAX_INC);
        let slave_dt = (dt * self.ratio).min(MAX_INC);
        let mut held = self.held;
        let mut slave = self.slave + slave_dt;
        let mut correction = 0.0;
        // A step of `height` that happened `d` samples ago (0..1), split
        // between the sample before it and the one after.
        let step = |height: f32, d: f32, held: &mut f32, correction: &mut f32| {
            *held += 0.5 * height * d * d;
            *correction -= 0.5 * height * (1.0 - d) * (1.0 - d);
        };
        let master = self.phase + dt;
        if master >= 1.0 && dt > 0.0 {
            // Synced: the master's cycle ended `d` samples ago.
            let d = (master - 1.0) / dt;
            let mut before = slave - d * slave_dt;
            if before >= 1.0 {
                // It wrapped on its own first, earlier in this sample.
                before -= 1.0;
                let wrapped = (slave - 1.0) / slave_dt.max(1e-9);
                step(-2.0, wrapped.min(1.0), &mut held, &mut correction);
            }
            // From where it was to the middle of its cycle, where it's 0.
            step(1.0 - 2.0 * before, d, &mut held, &mut correction);
            slave = 0.5 + d * slave_dt;
            self.phase = master - 1.0;
        } else {
            self.phase = master;
            if slave >= 1.0 {
                slave -= 1.0;
                step(-2.0, slave / slave_dt.max(1e-9), &mut held, &mut correction);
            }
        }
        self.slave = fract(slave);
        self.held = 2.0 * self.slave - 1.0 + correction;
        self.block_dc(held)
    }

    /// A one-pole highpass at [`DC_BLOCK_HZ`], when `dc` is set.
    #[inline]
    fn block_dc(&mut self, x: f32) -> f32 {
        if self.dc == 0.0 {
            return x;
        }
        let out = x - self.prev + self.dc * self.state[0];
        self.prev = x;
        self.state[0] = out;
        out
    }
}

/// Folds `x` back into `-1..=1` at its edges, as many times as it takes:
/// a triangle-shaped transfer curve.
#[inline]
fn fold(x: f32) -> f32 {
    // A triangle wave of x with period 4, through (0, 0) and (1, 1).
    let u = fract((x + 1.0) * 0.25 + 4.0);
    1.0 - (4.0 * u - 2.0).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn run(source: Source, hz: f32, shape: f32, samples: usize) -> Vec<f32> {
        let mut osc = Osc::default();
        osc.start(source, hz, shape, 0.0, 1000.0, RATE, 0x1234_5678);
        (0..samples).map(|_| osc.next(1.0)).collect()
    }

    fn rms(signal: &[f32]) -> f32 {
        (signal.iter().map(|s| s * s).sum::<f32>() / signal.len() as f32).sqrt()
    }

    /// Rising zero crossings a second.
    fn pitch(signal: &[f32]) -> f32 {
        let crossings = signal
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        crossings as f32 * RATE / signal.len() as f32
    }

    #[test]
    fn fold_is_identity_inside_and_reflects_outside() {
        for x in [-1.0, -0.4, 0.0, 0.3, 1.0] {
            assert!((fold(x) - x).abs() < 1e-6, "{x}");
        }
        assert!((fold(1.5) - 0.5).abs() < 1e-6);
        assert!((fold(-2.5) - 0.5).abs() < 1e-6);
        assert!((fold(3.0) - -1.0).abs() < 1e-6);
    }

    #[test]
    fn oscillators_play_at_their_pitch_without_dc() {
        for source in [
            Source::Sine,
            Source::Triangle,
            Source::Saw,
            Source::Square,
            Source::Membrane,
            Source::Bell,
        ] {
            for shape in [0.0, 0.5, 1.0] {
                let out = run(source, 220.0, shape, 48_000);
                let mean = out.iter().sum::<f32>() / out.len() as f32;
                assert!(mean.abs() < 0.01, "{source:?} at {shape}: DC {mean}");
                assert!(out.iter().all(|s| s.abs() <= 1.6), "{source:?} bounded");
                if shape == 0.0 && matches!(source, Source::Sine | Source::Triangle) {
                    assert_eq!(out[0], 0.0, "{source:?} starts at zero");
                }
                // The plain waveforms cross zero once a cycle.
                if shape == 0.0 && !matches!(source, Source::Membrane | Source::Bell) {
                    let hz = pitch(&out);
                    assert!((hz - 220.0).abs() < 2.0, "{source:?}: {hz} Hz");
                }
            }
        }
    }

    #[test]
    fn the_saw_at_no_sync_is_a_plain_saw() {
        let out = run(Source::Saw, 100.0, 0.0, 4800);
        // 480 samples a cycle, rising by 2/480 a sample apart from its
        // steps, which the correction spreads over two samples.
        let steps = out.windows(2).filter(|w| w[0] > 0.3 && w[1] < 0.0).count();
        assert_eq!(steps, 10, "one step a cycle");
        assert!(out[1..].iter().all(|s| s.abs() <= 1.05));
        assert!(out[200].abs() < 0.9 && out[200] > out[199]);
    }

    #[test]
    fn noises_have_the_rms_of_white_noise_and_their_tilt() {
        let white = rms(&run(Source::White, 0.0, 0.0, 96_000));
        for source in [Source::Pink, Source::Brown, Source::Blue, Source::Violet] {
            let out = run(source, 0.0, 0.0, 96_000);
            let level = rms(&out[4800..]);
            let db = 20.0 * (level / white).log10();
            assert!(db.abs() < 1.5, "{source:?}: {db} dB from white");
        }
        // Brown is darkest and violet brightest: compare how much each
        // changes from one sample to the next.
        let roughness = |source| {
            let out = run(source, 0.0, 0.0, 48_000);
            let diff: Vec<f32> = out.windows(2).map(|w| w[1] - w[0]).collect();
            rms(&diff) / rms(&out)
        };
        let order = [
            Source::Brown,
            Source::Pink,
            Source::White,
            Source::Blue,
            Source::Violet,
        ]
        .map(roughness);
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    }

    #[test]
    fn crackle_clicks_density_times_a_second_and_digital_holds() {
        let out = run(Source::Crackle, 500.0, 0.0, 48_000);
        let clicks = out.iter().filter(|s| **s != 0.0).count();
        assert!((400..600).contains(&clicks), "{clicks}");
        let out = run(Source::Digital, 1000.0, 0.0, 48_000);
        let changes = out.windows(2).filter(|w| w[0] != w[1]).count();
        assert!((990..=1000).contains(&changes), "{changes}");
        let crushed = run(Source::Digital, 1000.0, 1.0, 4800);
        assert!(crushed.iter().all(|s| s.abs() == 1.0 || *s == 0.0));
    }

    #[test]
    fn an_impulse_plays_its_cycles_and_stops() {
        let mut osc = Osc::default();
        osc.start(Source::Impulse, 1000.0, 0.0, 0.0, 100.0, RATE, 1);
        let out: Vec<f32> = (0..480).map(|_| osc.next(1.0)).collect();
        // Half a cycle at 1 kHz is 24 samples.
        assert!(out[..24].iter().all(|s| *s >= 0.0));
        assert!(out[25..].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn modal_damping_shortens_the_upper_modes() {
        let ring = |shape| {
            let out = run(Source::Membrane, 200.0, shape, 48_000);
            rms(&out[24_000..]) / rms(&out[..480])
        };
        assert!(ring(1.0) < ring(0.0) * 0.9);
    }

    #[test]
    fn sine_feedback_adds_harmonics() {
        let roughness = |shape| {
            let out = run(Source::Sine, 200.0, shape, 4800);
            let diff: Vec<f32> = out.windows(2).map(|w| w[1] - w[0]).collect();
            rms(&diff)
        };
        assert!(roughness(1.0) > roughness(0.0) * 1.3);
    }
}
