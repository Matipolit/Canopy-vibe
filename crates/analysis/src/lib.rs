//! Measures rendered audio: its spectrum, loudness, dynamics and stereo
//! image, its transients and tempo, sibilance, the pitch of a single voice
//! and the key; which tracks clash ([`clashes`]); and draws spectra and
//! spectrograms as PNG images ([`images`]). Pure functions on buffers: no
//! threads, no UI, no audio devices. The MCP server's `analyze`,
//! `find_clashes` and `plot` tools run these on renders the app makes; the
//! GUI's Analysis view is meant to use them too.
//!
//! Every measure takes a [`Signal`] (a stereo buffer and where it sits in
//! the song) and returns compact JSON, rounded by [`report::round`]: levels
//! in dB to a tenth, frequencies in whole Hz, positions in beats (quarter
//! notes from the song's start) and as "bar.beat". [`measure`] runs one by
//! name and adds a one-sentence `summary`.

pub mod clashes;
pub mod dynamics;
pub mod images;
pub mod key;
pub mod loudness;
pub mod pitch;
pub mod report;
pub mod sibilance;
pub mod spectrum;
pub mod stereo;
pub mod transients;

use std::ops::Range;

use midi::PPQ;
use project::TempoMap;
use serde_json::Value;

/// A stereo render and where it starts in the song, so times can be given
/// in beats and bars.
#[derive(Debug, Clone, Copy)]
pub struct Signal<'a> {
    pub left: &'a [f32],
    pub right: &'a [f32],
    pub rate: u32,
    /// Where the first frame falls, in beats from the song's start.
    pub start_beat: f64,
    pub tempo: &'a TempoMap,
}

impl<'a> Signal<'a> {
    pub fn new(
        left: &'a [f32],
        right: &'a [f32],
        rate: u32,
        start_beat: f64,
        tempo: &'a TempoMap,
    ) -> Signal<'a> {
        Signal {
            left,
            right,
            rate,
            start_beat,
            tempo,
        }
    }

    /// Frames in both channels (the shorter, should they differ).
    pub fn frames(&self) -> usize {
        self.left.len().min(self.right.len())
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate.max(1))
    }

    /// Both channels averaged: what the spectrum and pitch measures look at.
    pub fn mono(&self) -> Vec<f32> {
        let n = self.frames();
        self.left[..n]
            .iter()
            .zip(&self.right[..n])
            .map(|(l, r)| 0.5 * (l + r))
            .collect()
    }

    /// The song position of `frame` (fractional frames allowed), in beats.
    pub fn beat_at(&self, frame: f64) -> f64 {
        self.start_beat + self.tempo.frames_to_ticks(frame, self.rate) / PPQ as f64
    }

    /// The frame at song position `beat`, which may be outside the signal.
    pub fn frame_at(&self, beat: f64) -> f64 {
        self.tempo
            .ticks_to_frames((beat - self.start_beat) * PPQ as f64, self.rate)
    }

    /// How many beats a bar lasts in the song's meter.
    pub fn bar_beats(&self) -> f64 {
        self.tempo.bar_len().get().max(1) as f64 / PPQ as f64
    }

    /// The bar (counted from 1) that song position `beat` falls in.
    pub fn bar_of(&self, beat: f64) -> i64 {
        // A hair of slack so a bar line computed from frames isn't a bar early.
        ((beat + 1e-9) / self.bar_beats()).floor() as i64 + 1
    }

    /// The first and last bars the signal covers, even partly.
    pub fn bars(&self) -> (i64, i64) {
        let first = self.bar_of(self.start_beat);
        let end = self.beat_at(self.frames() as f64);
        let last = (self.bar_of(end - 1e-6)).max(first);
        (first, last)
    }

    /// The frames of bar `bar` that are in the signal.
    pub fn bar_frames(&self, bar: i64) -> Range<usize> {
        let at = |beat: f64| (self.frame_at(beat).round().max(0.0) as usize).min(self.frames());
        let start = (bar - 1) as f64 * self.bar_beats();
        at(start)..at(start + self.bar_beats())
    }

    /// Song position `beat` as "bar.beat".
    pub fn bar_beat(&self, beat: f64) -> String {
        report::bar_beat(beat, self.tempo)
    }
}

/// The measures [`measure`] knows, by name.
pub const MEASURES: [&str; 8] = [
    "spectrum",
    "loudness",
    "dynamics",
    "stereo",
    "transients",
    "sibilance",
    "pitch",
    "key",
];

/// What a measure may add when asked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// The spectrum gains eight bands' level per bar.
    pub bands_per_bar: bool,
}

/// Runs the measure called `name` (one of [`MEASURES`]) on `signal`: its
/// JSON, rounded, with a `summary` sentence added. An unknown name is an
/// error listing the known ones.
pub fn measure(name: &str, signal: &Signal, options: &Options) -> Result<Value, String> {
    let (mut value, summary): (Value, fn(&Value) -> String) = match name {
        "spectrum" => (
            spectrum::measure_with(signal, options.bands_per_bar),
            spectrum::summary,
        ),
        "loudness" => (loudness::measure(signal), loudness::summary),
        "dynamics" => (dynamics::measure(signal), dynamics::summary),
        "stereo" => (stereo::measure(signal), stereo::summary),
        "transients" => (transients::measure(signal), transients::summary),
        "sibilance" => (sibilance::measure(signal), sibilance::summary),
        "pitch" => (pitch::measure(signal), pitch::summary),
        "key" => (key::measure(signal), key::summary),
        _ => {
            return Err(format!(
                "no measure called `{name}`; the measures are {}",
                MEASURES.join(", ")
            ));
        }
    };
    let sentence = summary(&value);
    if let Value::Object(map) = &mut value {
        map.insert("summary".to_string(), Value::String(sentence));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{sine, vowel};

    #[test]
    fn every_measure_runs_by_name_with_a_summary() {
        let tempo = TempoMap::new(120.0);
        let wave = vowel(220.0, 0.3, 6000.0, 2.0, 48_000);
        let signal = Signal::new(&wave, &wave, 48_000, 0.0, &tempo);
        for name in MEASURES {
            let value = measure(name, &signal, &Options::default()).unwrap();
            let summary = value["summary"].as_str().unwrap();
            assert!(summary.ends_with('.'), "{name}: {summary}");
        }
        let error = measure("tempo", &signal, &Options::default()).unwrap_err();
        assert!(error.contains("spectrum, loudness"), "{error}");
    }

    #[test]
    fn band_levels_per_bar_come_only_when_asked() {
        let tempo = TempoMap::new(120.0);
        let wave = sine(100.0, 0.5, 4.0, 48_000);
        let signal = Signal::new(&wave, &wave, 48_000, 0.0, &tempo);
        let plain = measure("spectrum", &signal, &Options::default()).unwrap();
        assert!(plain.get("per_bar").is_none());
        let options = Options {
            bands_per_bar: true,
        };
        let with = measure("spectrum", &signal, &options).unwrap();
        assert_eq!(with["per_bar"]["bars"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn positions_convert_between_frames_beats_and_bars() {
        let tempo = TempoMap::new(120.0);
        let wave = vec![0.0f32; 48_000 * 3];
        // From beat 6: bar 2's third beat, 3 s = 6 beats, so into bar 3.
        let signal = Signal::new(&wave, &wave, 48_000, 6.0, &tempo);
        assert_eq!(signal.beat_at(24_000.0), 7.0);
        assert_eq!(signal.frame_at(8.0), 48_000.0);
        assert_eq!(signal.bars(), (2, 3));
        assert_eq!(signal.bar_frames(2), 0..48_000);
        assert_eq!(signal.bar_frames(3), 48_000..144_000);
        assert_eq!(signal.bar_beat(7.0), "2.4");
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! Synthetic signals with known answers.

    use std::f64::consts::TAU;

    /// A sine of `amplitude` at `hz`, `seconds` long.
    pub fn sine(hz: f64, amplitude: f64, seconds: f64, rate: u32) -> Vec<f32> {
        let n = (seconds * f64::from(rate)) as usize;
        (0..n)
            .map(|i| (amplitude * (TAU * hz * i as f64 / f64::from(rate)).sin()) as f32)
            .collect()
    }

    /// Harmonics of `hz` falling off as 1/k², like a sung vowel, up to
    /// `top` Hz, peaking near `amplitude`.
    pub fn vowel(hz: f64, amplitude: f64, top: f64, seconds: f64, rate: u32) -> Vec<f32> {
        let n = (seconds * f64::from(rate)) as usize;
        let harmonics = (top / hz) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / f64::from(rate);
                let sum: f64 = (1..=harmonics)
                    .map(|k| (TAU * hz * k as f64 * t + k as f64).sin() / (k * k) as f64)
                    .sum();
                (amplitude * sum / 1.6) as f32
            })
            .collect()
    }

    /// Noise between `low` and `high` Hz: many sines at random
    /// frequencies and phases.
    pub fn band_noise(low: f64, high: f64, len: usize, rate: u32, seed: u32) -> Vec<f32> {
        let random = noise(400, seed);
        let partials: Vec<(f64, f64)> = random
            .chunks(2)
            .map(|p| {
                let hz = low + (high - low) * (0.5 + 0.5 * f64::from(p[0]));
                (hz, TAU * f64::from(p[1]))
            })
            .collect();
        let scale = (2.0 / partials.len() as f64).sqrt();
        (0..len)
            .map(|i| {
                let t = i as f64 / f64::from(rate);
                let sum: f64 = partials
                    .iter()
                    .map(|(hz, phase)| (TAU * hz * t + phase).sin())
                    .sum();
                (sum * scale) as f32
            })
            .collect()
    }

    /// Deterministic white noise in -1..1.
    pub fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut state = seed.max(1);
        (0..len)
            .map(|_| dsp::noise::bipolar(dsp::noise::xorshift32(&mut state)))
            .collect()
    }
}
