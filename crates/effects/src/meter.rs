//! Level meters for the UI: a peak meter's needle, an RMS meter, and the
//! dB they read; and the levels at an effect's edges, which the engine
//! measures for every effect that asks ([`Levels`], [`EdgeMeters`]).
//!
//! **Who measures what.** What goes into an effect and what comes out are
//! measured once, by the engine, the same way for every effect, and only
//! while its panel shows ([`EdgeMeters`], published beside the effect's own
//! meters). An effect's own [`crate::Effect::meters`] are what only it knows:
//! a gain reduction, a clip, an LFO's phase, a level inside it. Those it
//! works out on its own, and anything that costs it per sample only while
//! [`crate::Context::metering`] says someone reads them.

use dsp::flush;

use crate::dynamics::coefficient;
use crate::truepeak::TruePeak;

/// How fast a peak meter falls back after a peak, in dB per second: about
/// a digital peak meter's speed, slow enough that a UI reading the meters
/// at its own frame rate still sees every peak.
const PEAK_FALL_DB_PER_S: f32 = 20.0;
/// How long an RMS meter averages over, in ms: about a VU meter's
/// integration time, so it reads loudness rather than peaks.
const RMS_MS: f32 = 300.0;
/// Meters never read above this (+120 dB), so even an infinite input shows
/// as a finite number.
const METER_MAX: f32 = 1e6;
/// What level meters read for silence, in dB.
pub const METER_FLOOR_DB: f32 = -100.0;

/// A level in dB for a meter: [`METER_FLOOR_DB`] for silence, and never NaN
/// or infinite.
pub fn meter_db(level: f32) -> f32 {
    if level.is_nan() {
        return METER_FLOOR_DB;
    }
    dsp::gain_to_db(level.abs().min(METER_MAX)).max(METER_FLOOR_DB)
}

/// The largest magnitude in `samples`; NaNs are skipped.
#[inline]
pub fn block_peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |p, s| p.max(s.abs()))
}

/// A peak meter's needle: jumps to each new peak and falls back at
/// [`PEAK_FALL_DB_PER_S`]. Fed once per block.
#[derive(Debug, Clone)]
pub struct PeakMeter {
    level: f32,
    /// How much the needle falls per sample, as a gain.
    fall: f32,
}

impl Default for PeakMeter {
    fn default() -> Self {
        PeakMeter {
            level: 0.0,
            fall: 1.0,
        }
    }
}

impl PeakMeter {
    pub fn prepare(&mut self, sample_rate: f32) {
        self.fall = dsp::db_to_gain(-PEAK_FALL_DB_PER_S / sample_rate);
        self.level = 0.0;
    }

    pub fn reset(&mut self) {
        self.level = 0.0;
    }

    /// A block `len` samples long whose largest magnitude was `peak`.
    pub fn update(&mut self, peak: f32, len: usize) {
        let fallen = flush(self.level * self.fall.powi(len.min(1 << 20) as i32));
        self.level = fallen.max(peak.min(METER_MAX));
    }

    /// The needle's level, as a gain.
    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn db(&self) -> f32 {
        meter_db(self.level)
    }
}

/// An RMS meter: the mean square over about [`RMS_MS`], fed per sample. A
/// full-scale sine reads -3 dB.
#[derive(Debug, Clone, Default)]
pub struct RmsMeter {
    mean_square: f32,
    /// `1 -` the one-pole coefficient.
    rate: f32,
}

impl RmsMeter {
    pub fn prepare(&mut self, sample_rate: f32) {
        self.rate = 1.0 - coefficient(RMS_MS, sample_rate);
        self.mean_square = 0.0;
    }

    pub fn reset(&mut self) {
        self.mean_square = 0.0;
    }

    /// One sample's power (its square, or the mean of the channels').
    #[inline]
    pub fn add(&mut self, power: f32) {
        // Written so that NaN and infinity become the meter's top.
        let power = if power < METER_MAX * METER_MAX {
            power
        } else {
            METER_MAX * METER_MAX
        };
        self.mean_square += (power - self.mean_square) * self.rate;
    }

    /// Ends a block: a decay ends in zero, not denormals.
    pub fn flush(&mut self) {
        self.mean_square = flush(self.mean_square);
    }

    pub fn db(&self) -> f32 {
        meter_db(self.mean_square.max(0.0).sqrt())
    }
}

/// Which levels the engine measures at an effect's edges for its panel
/// ([`crate::EffectDescriptor::levels`]): its input as handed to it, its
/// output as it left it, and the output's true peak (between the samples).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Levels {
    pub input: bool,
    pub output: bool,
    pub true_peak: bool,
}

impl Levels {
    pub const NONE: Levels = Levels {
        input: false,
        output: false,
        true_peak: false,
    };
    /// Input and output, peak and RMS.
    pub const BOTH: Levels = Levels {
        input: true,
        output: true,
        true_peak: false,
    };
    pub const OUTPUT: Levels = Levels {
        input: false,
        output: true,
        true_peak: false,
    };

    pub fn any(self) -> bool {
        self.input || self.output
    }
}

/// One edge's levels as a meter reads them, in dB ([`METER_FLOOR_DB`] for
/// silence or what isn't measured): each channel's peak (falling back at
/// 20 dB a second) and RMS (over 300 ms), and both channels' true peak.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    pub peak: [f32; 2],
    pub rms: [f32; 2],
    pub true_peak: f32,
}

impl Level {
    pub const SILENT: Level = Level {
        peak: [METER_FLOOR_DB; 2],
        rms: [METER_FLOOR_DB; 2],
        true_peak: METER_FLOOR_DB,
    };

    /// The louder channel's peak.
    pub fn peak(&self) -> f32 {
        self.peak[0].max(self.peak[1])
    }

    /// Both channels' RMS together: the mean of their powers.
    pub fn rms(&self) -> f32 {
        let power = |db: f32| 10f32.powf(0.1 * db);
        let mean = 0.5 * (power(self.rms[0]) + power(self.rms[1]));
        (10.0 * mean.log10()).max(METER_FLOOR_DB)
    }
}

/// The levels at both of an effect's edges, as published to the UI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EdgeLevels {
    pub input: Level,
    pub output: Level,
}

impl EdgeLevels {
    pub const SILENT: EdgeLevels = EdgeLevels {
        input: Level::SILENT,
        output: Level::SILENT,
    };
    /// Floats in [`EdgeLevels::to_array`].
    pub const SLOTS: usize = 10;

    /// As flat floats, for the engine to publish without locking.
    pub fn to_array(&self) -> [f32; Self::SLOTS] {
        let side = |l: &Level| [l.peak[0], l.peak[1], l.rms[0], l.rms[1], l.true_peak];
        let [a, b, c, d, e] = side(&self.input);
        let [f, g, h, i, j] = side(&self.output);
        [a, b, c, d, e, f, g, h, i, j]
    }

    pub fn from_array(a: [f32; Self::SLOTS]) -> EdgeLevels {
        let side = |s: &[f32]| Level {
            peak: [s[0], s[1]],
            rms: [s[2], s[3]],
            true_peak: s[4],
        };
        EdgeLevels {
            input: side(&a[..5]),
            output: side(&a[5..]),
        }
    }
}

/// One edge's meters.
#[derive(Debug, Clone, Default)]
struct Side {
    peak: [PeakMeter; 2],
    rms: [RmsMeter; 2],
}

impl Side {
    fn prepare(&mut self, sample_rate: f32) {
        for meter in &mut self.peak {
            meter.prepare(sample_rate);
        }
        for meter in &mut self.rms {
            meter.prepare(sample_rate);
        }
    }

    fn reset(&mut self) {
        self.peak.iter_mut().for_each(PeakMeter::reset);
        self.rms.iter_mut().for_each(RmsMeter::reset);
    }

    fn measure(&mut self, left: &[f32], right: &[f32]) {
        let len = left.len().min(right.len());
        for ((peak, rms), channel) in self.peak.iter_mut().zip(&mut self.rms).zip([left, right]) {
            peak.update(block_peak(&channel[..len]), len);
            for &x in &channel[..len] {
                rms.add(x * x);
            }
            rms.flush();
        }
    }

    fn read(&self) -> Level {
        Level {
            peak: [self.peak[0].db(), self.peak[1].db()],
            rms: [self.rms[0].db(), self.rms[1].db()],
            true_peak: METER_FLOOR_DB,
        }
    }
}

/// The meters the engine runs at one effect's edges, as its descriptor's
/// [`Levels`] ask, while the effect's panel shows: the input before
/// `process`, the output after. The same for every effect, so every panel's
/// levels mean the same thing, and none is worked out when no one looks.
#[derive(Debug, Clone)]
pub struct EdgeMeters {
    levels: Levels,
    input: Side,
    output: Side,
    true_peak: TruePeak,
    true_peak_meter: PeakMeter,
}

impl EdgeMeters {
    /// Allocates nothing; built with the effect on the UI thread.
    pub fn new(levels: Levels, sample_rate: f32) -> EdgeMeters {
        let mut meters = EdgeMeters {
            levels,
            input: Side::default(),
            output: Side::default(),
            true_peak: TruePeak::new(),
            true_peak_meter: PeakMeter::default(),
        };
        meters.input.prepare(sample_rate);
        meters.output.prepare(sample_rate);
        meters.true_peak_meter.prepare(sample_rate);
        meters
    }

    /// Back to silence: when the meters start again after no one looked,
    /// or the effect was bypassed.
    pub fn reset(&mut self) {
        self.input.reset();
        self.output.reset();
        self.true_peak.reset();
        self.true_peak_meter.reset();
    }

    /// A block of what goes into the effect.
    pub fn input(&mut self, left: &[f32], right: &[f32]) {
        if self.levels.input {
            self.input.measure(left, right);
        }
    }

    /// The same block as the effect left it.
    pub fn output(&mut self, left: &[f32], right: &[f32]) {
        if self.levels.output {
            self.output.measure(left, right);
        }
        if self.levels.true_peak {
            let mut peak = 0.0f32;
            for (&l, &r) in left.iter().zip(right) {
                peak = peak.max(self.true_peak.push(l, r, true));
            }
            self.true_peak_meter.update(peak, left.len());
        }
    }

    pub fn read(&self) -> EdgeLevels {
        let mut output = self.output.read();
        if self.levels.true_peak {
            output.true_peak = self.true_peak_meter.db();
        }
        EdgeLevels {
            input: self.input.read(),
            output,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meters_read_levels_and_fall_back() {
        let mut peak = PeakMeter::default();
        peak.prepare(48_000.0);
        peak.update(0.5, 64);
        assert!((peak.db() - dsp::gain_to_db(0.5)).abs() < 1e-4);
        // A second later it has fallen 20 dB.
        for _ in 0..750 {
            peak.update(0.0, 64);
        }
        assert!(
            (peak.db() - dsp::gain_to_db(0.05)).abs() < 0.01,
            "{}",
            peak.db()
        );
        peak.update(f32::INFINITY, 1);
        assert!(peak.db().is_finite());

        let mut rms = RmsMeter::default();
        rms.prepare(48_000.0);
        // Five seconds: many times the meter's time constant.
        for n in 0..240_000 {
            let s = (n as f32 * 0.1).sin();
            rms.add(s * s);
        }
        assert!((rms.db() + 3.01).abs() < 0.05, "{}", rms.db());
        rms.add(f32::NAN);
        assert!(rms.db().is_finite());
        assert_eq!(meter_db(f32::NAN), METER_FLOOR_DB);
        assert_eq!(meter_db(0.0), METER_FLOOR_DB);
        assert_eq!(meter_db(1e-7), METER_FLOOR_DB);
    }

    #[test]
    fn edge_meters_read_what_goes_in_and_comes_out() {
        let mut meters = EdgeMeters::new(
            Levels {
                true_peak: true,
                ..Levels::BOTH
            },
            48_000.0,
        );
        // Half scale in, a quarter out on the left; silence on the right.
        let sine = |gain: f32| -> Vec<f32> {
            (0..64)
                .map(|n| gain * (n as f32 * std::f32::consts::TAU / 16.0 + 0.4).sin())
                .collect()
        };
        let silence = [0.0f32; 64];
        for _ in 0..1000 {
            meters.input(&sine(0.5), &silence);
            meters.output(&sine(0.25), &silence);
        }
        let levels = meters.read();
        let near =
            |a: f32, b: f32, what: &str| assert!((a - b).abs() < 0.1, "{what}: {a} against {b}");
        near(levels.input.peak[0], -6.02, "input peak");
        near(levels.output.peak[0], -12.04, "output peak");
        near(levels.input.rms[0], -9.03, "input RMS");
        near(levels.output.rms[0], -15.05, "output RMS");
        near(levels.output.true_peak, -12.04, "true peak");
        assert_eq!(levels.input.peak[1], METER_FLOOR_DB);
        near(levels.input.peak(), -6.02, "louder channel");
        near(levels.input.rms(), -12.04, "both channels' RMS");
        assert_eq!(EdgeLevels::from_array(levels.to_array()), levels);

        meters.reset();
        assert_eq!(meters.read(), EdgeLevels::SILENT);
        // What isn't asked for isn't measured.
        let mut output_only = EdgeMeters::new(Levels::OUTPUT, 48_000.0);
        output_only.input(&sine(1.0), &sine(1.0));
        assert_eq!(output_only.read().input, Level::SILENT);
    }
}
