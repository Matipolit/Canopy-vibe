//! Level meters for the UI: a peak meter's needle, an RMS meter, and the
//! dB they read.

use dsp::flush;

use crate::dynamics::coefficient;

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
pub(crate) const METER_FLOOR_DB: f32 = -100.0;

/// A level in dB for a meter: [`METER_FLOOR_DB`] for silence, and never NaN
/// or infinite.
pub(crate) fn meter_db(level: f32) -> f32 {
    if level.is_nan() {
        return METER_FLOOR_DB;
    }
    dsp::gain_to_db(level.abs().min(METER_MAX)).max(METER_FLOOR_DB)
}

/// The largest magnitude in `samples`; NaNs are skipped.
#[inline]
pub(crate) fn block_peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |p, s| p.max(s.abs()))
}

/// A peak meter's needle: jumps to each new peak and falls back at
/// [`PEAK_FALL_DB_PER_S`]. Fed once per block.
#[derive(Debug, Clone)]
pub(crate) struct PeakMeter {
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
pub(crate) struct RmsMeter {
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
}
