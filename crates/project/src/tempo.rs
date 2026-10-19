use std::fmt;

use midi::{PPQ, Tick};
use serde::{Deserialize, Serialize};

/// In microseconds per quarter note, as in a MIDI tempo meta event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tempo(u32);

impl Tempo {
    /// Largest value representable in a MIDI tempo meta event (24 bits).
    pub const MAX_MICROS: u32 = 0xFF_FFFF;
    pub const MIN_BPM: f64 = 20.0;
    pub const MAX_BPM: f64 = 999.0;

    const MICROS_PER_MINUTE: f64 = 60_000_000.0;

    pub fn from_bpm(tempo_bpm: f64) -> Tempo {
        let micros = (Self::MICROS_PER_MINUTE / tempo_bpm).round();
        Tempo(micros.clamp(1.0, Self::MAX_MICROS as f64) as u32)
    }

    pub fn to_bpm(&self) -> f64 {
        Self::MICROS_PER_MINUTE / self.0 as f64
    }

    pub fn micros_per_quarter(&self) -> u32 {
        self.0
    }
}

impl Default for Tempo {
    fn default() -> Self {
        Tempo::from_bpm(120.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSignature {
    pub numerator: u8,
    /// A power of two: 4 means the beat is a quarter note.
    pub denominator: u8,
}

impl TimeSignature {
    pub fn beat_len(&self) -> Tick {
        Tick::new(PPQ * 4 / self.denominator.max(1) as i64)
    }

    pub fn bar_len(&self) -> Tick {
        self.beat_len() * self.numerator.max(1) as i64
    }
}

impl Default for TimeSignature {
    fn default() -> Self {
        TimeSignature {
            numerator: 4,
            denominator: 4,
        }
    }
}

/// Maps musical time to real time. One fixed tempo and meter for now.
///
/// This is the only place that converts ticks to seconds or samples, so tempo
/// changes and tempo automation only ever have to touch this type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TempoMap {
    pub tempo: Tempo,
    pub time_signature: TimeSignature,
}

impl TempoMap {
    pub fn new(bpm: f64) -> TempoMap {
        TempoMap {
            tempo: Tempo::from_bpm(bpm),
            ..TempoMap::default()
        }
    }

    pub fn ticks_to_seconds(&self, ticks: f64) -> f64 {
        ticks / PPQ as f64 * self.tempo.micros_per_quarter() as f64 / 1e6
    }

    pub fn seconds_to_ticks(&self, seconds: f64) -> f64 {
        seconds * 1e6 / self.tempo.micros_per_quarter() as f64 * PPQ as f64
    }

    /// The sample a tick lands on, rounded to the nearest. Exact integer math,
    /// so a clip at bar 1000 is not a sample off from where it'd be alone.
    pub fn tick_to_sample(&self, tick: Tick, sample_rate: u32) -> i64 {
        let numerator =
            tick.get() as i128 * self.tempo.micros_per_quarter() as i128 * sample_rate as i128;
        let denominator = PPQ as i128 * 1_000_000;
        numerator.div_euclid(denominator) as i64
            + (2 * numerator.rem_euclid(denominator) >= denominator) as i64
    }

    /// The tick at or before `sample`.
    pub fn sample_to_tick(&self, sample: i64, sample_rate: u32) -> Tick {
        let numerator = sample as i128 * PPQ as i128 * 1_000_000;
        let denominator = self.tempo.micros_per_quarter() as i128 * sample_rate.max(1) as i128;
        Tick::new(numerator.div_euclid(denominator) as i64)
    }

    /// How many frames at `sample_rate` a span of `ticks` lasts.
    pub fn ticks_to_frames(&self, ticks: f64, sample_rate: u32) -> f64 {
        self.ticks_to_seconds(ticks) * sample_rate as f64
    }

    pub fn frames_to_ticks(&self, frames: f64, sample_rate: u32) -> f64 {
        self.seconds_to_ticks(frames / sample_rate.max(1) as f64)
    }

    pub fn beat_len(&self) -> Tick {
        self.time_signature.beat_len()
    }

    pub fn bar_len(&self) -> Tick {
        self.time_signature.bar_len()
    }

    /// Where bar `bar` starts. Bars count from 1, like on screen.
    pub fn bar_start(&self, bar: i64) -> Tick {
        self.bar_len() * (bar - 1)
    }

    pub fn bar_beat(&self, tick: Tick) -> BarBeat {
        let bar_len = self.bar_len().get();
        let beat_len = self.beat_len().get();
        let sixteenth_len = (PPQ / 4).max(1);
        let t = tick.get();
        let bar = t.div_euclid(bar_len);
        let in_bar = t.rem_euclid(bar_len);
        let beat = in_bar / beat_len;
        let in_beat = in_bar % beat_len;
        BarBeat {
            bar: bar + 1,
            beat: beat + 1,
            sixteenth: in_beat / sixteenth_len + 1,
            ticks: in_beat % sixteenth_len,
        }
    }
}

/// Rounds a fractional tick count up, ignoring floating-point noise: a
/// length converted to frames and back can come out a hair above a whole
/// tick, and must not then count as one more.
pub(crate) fn ticks_up(ticks: f64) -> Tick {
    Tick::new((ticks - 1e-6).ceil() as i64)
}

/// A position as bars.beats.sixteenths, all counted from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarBeat {
    pub bar: i64,
    pub beat: i64,
    pub sixteenth: i64,
    /// Leftover ticks past the sixteenth.
    pub ticks: i64,
}

impl fmt::Display for BarBeat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.bar, self.beat, self.sixteenth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpm_round_trip() {
        assert_eq!(Tempo::from_bpm(120.0).micros_per_quarter(), 500_000);
        assert!((Tempo::from_bpm(93.5).to_bpm() - 93.5).abs() < 1e-3);
    }

    #[test]
    fn ticks_and_samples() {
        let map = TempoMap::new(120.0);
        // A quarter at 120 BPM is half a second.
        assert_eq!(map.tick_to_sample(Tick::QUARTER, 48_000), 24_000);
        assert_eq!(map.tick_to_sample(map.bar_start(2), 44_100), 88_200);
        assert_eq!(map.sample_to_tick(24_000, 48_000), Tick::QUARTER);
        assert_eq!(map.sample_to_tick(23_999, 48_000), Tick::new(959));
        assert_eq!(map.tick_to_sample(-Tick::QUARTER, 48_000), -24_000);
        assert!((map.ticks_to_seconds(1920.0) - 1.0).abs() < 1e-12);
        assert!((map.seconds_to_ticks(1.0) - 1920.0).abs() < 1e-9);
    }

    #[test]
    fn far_positions_stay_exact() {
        let map = TempoMap::new(127.0);
        let far = map.bar_start(10_001);
        let direct = map.ticks_to_frames(far.get() as f64, 44_100).round() as i64;
        assert_eq!(map.tick_to_sample(far, 44_100), direct);
    }

    #[test]
    fn bar_beat_display() {
        let map = TempoMap::default();
        assert_eq!(map.bar_beat(Tick::ZERO).to_string(), "1.1.1");
        assert_eq!(map.bar_beat(Tick::new(960 * 5 + 240)).to_string(), "2.2.2");
        let three_four = TempoMap {
            time_signature: TimeSignature {
                numerator: 3,
                denominator: 4,
            },
            ..map
        };
        assert_eq!(three_four.bar_beat(Tick::new(960 * 3)).to_string(), "2.1.1");
    }
}
