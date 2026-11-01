//! The pitch of a single voice (a singer, a bass line, a lead), by YIN (de
//! Cheveigné and Kawahara, 2002): its median and range, the notes it sings
//! over time, and how far it sits from equal temperament. Chords and mixes
//! have no one pitch; for those it finds little or nothing.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use serde_json::{Value, json};

use crate::Signal;
use crate::report::{self, pitch_name};

/// The lowest and highest pitch looked for, in Hz.
pub const LOWEST_HZ: f64 = 50.0;
pub const HIGHEST_HZ: f64 = 1000.0;
/// YIN's threshold on the normalized difference: lower is stricter.
const THRESHOLD: f32 = 0.15;
const HOP: usize = 256;
/// Frames quieter than this (RMS, dB) have no pitch.
const QUIET_DB: f64 = -50.0;
/// A note is a run of frames on the same semitone at least this long.
const SHORTEST_NOTE: f64 = 0.08;
/// Notes listed one by one.
const LISTED: usize = 500;

/// YIN over one signal: the difference function from FFT correlations, so
/// a frame costs a few transforms rather than a window's length squared.
struct Yin {
    /// Lags compared: up to this many frames, also the length summed over.
    window: usize,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    frame: Vec<f32>,
    head: Vec<f32>,
    frame_spectrum: Vec<Complex<f32>>,
    head_spectrum: Vec<Complex<f32>>,
    correlation: Vec<f32>,
    difference: Vec<f32>,
}

impl Yin {
    fn new(rate: u32) -> Yin {
        // 1024 at 44.1 and 48 kHz: room for 50 Hz.
        let window = ((f64::from(rate) / LOWEST_HZ).ceil() as usize)
            .next_power_of_two()
            .max(1024);
        let size = 4 * window;
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(size);
        let inverse = planner.plan_fft_inverse(size);
        Yin {
            window,
            frame: forward.make_input_vec(),
            head: forward.make_input_vec(),
            frame_spectrum: forward.make_output_vec(),
            head_spectrum: forward.make_output_vec(),
            correlation: inverse.make_output_vec(),
            difference: vec![0.0; window + 1],
            forward,
            inverse,
        }
    }

    /// Samples one estimate looks at: twice the window.
    fn span(&self) -> usize {
        2 * self.window
    }

    /// The period of `samples` (`span` long) in frames, or `None` if it has
    /// none clear enough between `shortest` and `longest`.
    fn period(&mut self, samples: &[f32], shortest: usize, longest: usize) -> Option<f32> {
        let w = self.window;
        self.frame.fill(0.0);
        self.head.fill(0.0);
        self.frame[..samples.len()].copy_from_slice(samples);
        self.head[..w].copy_from_slice(&samples[..w]);
        // Plans and buffers always match, so these cannot fail.
        let _ = self
            .forward
            .process(&mut self.frame, &mut self.frame_spectrum);
        let _ = self
            .forward
            .process(&mut self.head, &mut self.head_spectrum);
        for (x, y) in self.frame_spectrum.iter_mut().zip(&self.head_spectrum) {
            *x *= y.conj();
        }
        let last = self.frame_spectrum.len() - 1;
        self.frame_spectrum[0].im = 0.0;
        self.frame_spectrum[last].im = 0.0;
        let _ = self
            .inverse
            .process(&mut self.frame_spectrum, &mut self.correlation);
        let scale = 1.0 / self.correlation.len() as f32;

        // d(τ) = Σ x[j]² + Σ x[j+τ]² − 2 Σ x[j]·x[j+τ], j over the window,
        // the moving energy kept as a running sum.
        let energy = |range: std::ops::Range<usize>| -> f64 {
            samples[range]
                .iter()
                .map(|&x| f64::from(x) * f64::from(x))
                .sum()
        };
        let first = energy(0..w);
        let mut moving = first;
        self.difference[0] = 0.0;
        for tau in 1..=w.min(longest + 1) {
            let (out, into) = (samples[tau - 1], samples[tau - 1 + w]);
            moving += f64::from(into) * f64::from(into) - f64::from(out) * f64::from(out);
            let d = first + moving - 2.0 * f64::from(self.correlation[tau] * scale);
            self.difference[tau] = d.max(0.0) as f32;
        }

        // The cumulative mean normalized difference: d'(τ) = d(τ)·τ / Σ d.
        let mut sum = 0.0f32;
        let top = w.min(longest + 1);
        for tau in 1..=top {
            sum += self.difference[tau];
            self.difference[tau] = if sum > 0.0 {
                self.difference[tau] * tau as f32 / sum
            } else {
                1.0
            };
        }
        let d = &self.difference;
        let mut tau = shortest.max(2);
        while tau < top {
            if d[tau] < THRESHOLD {
                while tau + 1 < top && d[tau + 1] < d[tau] {
                    tau += 1;
                }
                let (a, b, c) = (d[tau - 1], d[tau], d[tau + 1]);
                let curve = a - 2.0 * b + c;
                let offset = if curve > 0.0 {
                    (0.5 * (a - c) / curve).clamp(-0.5, 0.5)
                } else {
                    0.0
                };
                return Some(tau as f32 + offset);
            }
            tau += 1;
        }
        None
    }
}

/// The pitch of each frame `HOP` apart, in Hz (`None` where there is none),
/// and the frame (in samples) each estimate stands for.
pub fn track(signal: &Signal) -> Vec<(usize, Option<f64>)> {
    let mono = signal.mono();
    let rate = f64::from(signal.rate.max(1));
    let mut yin = Yin::new(signal.rate);
    let span = yin.span();
    let shortest = (rate / HIGHEST_HZ).floor() as usize;
    let longest = (rate / LOWEST_HZ).ceil() as usize;
    let quiet = 10f64.powf(QUIET_DB / 10.0);
    let mut out = Vec::new();
    let mut start = 0;
    while start + span <= mono.len() {
        let samples = &mono[start..start + span];
        let power = samples[..yin.window]
            .iter()
            .map(|&x| f64::from(x) * f64::from(x))
            .sum::<f64>()
            / yin.window as f64;
        let hz = (power > quiet)
            .then(|| yin.period(samples, shortest, longest))
            .flatten()
            .map(|period| rate / f64::from(period))
            .filter(|hz| (LOWEST_HZ..=HIGHEST_HZ).contains(hz));
        // The window's middle: where the periods it compares lie.
        out.push((start + yin.window, hz));
        start += HOP;
    }
    out
}

/// A frequency as a MIDI note, fractional.
pub fn midi_of(hz: f64) -> f64 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

fn hz_of(midi: f64) -> f64 {
    440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

/// The pitch measure.
pub fn measure(signal: &Signal) -> Value {
    let frames = track(signal);
    let rate = f64::from(signal.rate.max(1));
    let voiced: Vec<f64> = frames.iter().filter_map(|f| f.1).collect();
    if voiced.is_empty() {
        return json!({ "voiced_share": 0.0, "median_hz": null, "notes": [] });
    }
    let midis: Vec<f64> = voiced.iter().map(|&hz| midi_of(hz)).collect();
    let cents: Vec<f64> = midis.iter().map(|m| (m - m.round()) * 100.0).collect();
    let median = report::median(&voiced).unwrap_or(0.0);
    let (low, high) = (
        report::percentile(&voiced, 0.05).unwrap_or(median),
        report::percentile(&voiced, 0.95).unwrap_or(median),
    );
    let named = |hz: f64| pitch_name(midi_of(hz).round() as i32);

    // Runs of frames on the same semitone.
    let mut runs: Vec<(usize, usize, i32)> = Vec::new();
    for (i, (_, hz)) in frames.iter().enumerate() {
        let Some(hz) = hz else { continue };
        let note = midi_of(*hz).round() as i32;
        match runs.last_mut() {
            Some(run) if run.1 + 1 == i && run.2 == note => run.1 = i,
            _ => runs.push((i, i, note)),
        }
    }
    let notes: Vec<Value> = runs
        .into_iter()
        .filter(|&(a, b, _)| ((b - a + 1) * HOP) as f64 / rate >= SHORTEST_NOTE)
        .take(LISTED)
        .map(|(a, b, note)| {
            let hz: Vec<f64> = frames[a..=b].iter().filter_map(|f| f.1).collect();
            let hz = report::median(&hz).unwrap_or_else(|| hz_of(f64::from(note)));
            let start = frames[a].0.saturating_sub(HOP / 2) as f64;
            let end = (frames[b].0 + HOP / 2) as f64;
            let beat = signal.beat_at(start);
            json!({
                "beat": beat,
                "at": signal.bar_beat(beat),
                "length_beats": signal.beat_at(end) - beat,
                "note": pitch_name(note),
                "midi": note,
                "hz": hz,
                "cents": (midi_of(hz) - f64::from(note)) * 100.0,
            })
        })
        .collect();

    report::round(json!({
        "voiced_share": voiced.len() as f64 / frames.len() as f64,
        "median_hz": median,
        "median_note": named(median),
        "range": {
            "low_hz": low,
            "high_hz": high,
            "low_note": named(low),
            "high_note": named(high),
        },
        "median_cents": report::median(&cents),
        "notes": notes,
    }))
}

/// One sentence: the median pitch, range and tuning.
pub fn summary(value: &Value) -> String {
    if value["median_hz"].is_null() {
        return "No clear single pitch.".to_string();
    }
    let range = &value["range"];
    format!(
        "Median pitch {} ({} Hz), from {} to {}, {} cents from equal temperament, pitched {}% of the time.",
        value["median_note"].as_str().unwrap_or("?"),
        report::whole(&value["median_hz"]),
        range["low_note"].as_str().unwrap_or("?"),
        range["high_note"].as_str().unwrap_or("?"),
        report::whole(&value["median_cents"]),
        value["voiced_share"]
            .as_f64()
            .map_or(0, |s| (s * 100.0).round() as i64),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::vowel;
    use project::TempoMap;

    #[test]
    fn yin_tracks_a_sung_note() {
        let tempo = TempoMap::new(120.0);
        let rate = 48_000;
        let wave = vowel(220.0, 0.3, 4000.0, 2.0, rate);
        let signal = Signal::new(&wave, &wave, rate, 0.0, &tempo);
        let value = measure(&signal);
        assert_eq!(value["median_note"], "A2", "{value}");
        assert_eq!(value["median_hz"], 220, "{value}");
        let notes = value["notes"].as_array().unwrap();
        assert_eq!(notes.len(), 1, "{value}");
        assert_eq!(notes[0]["midi"], 57);
        assert_eq!(notes[0]["note"], "A2");
        assert!(notes[0]["cents"].as_i64().unwrap().abs() <= 2, "{value}");
        assert!(value["voiced_share"].as_f64().unwrap() > 0.95, "{value}");
    }

    #[test]
    fn a_melody_comes_out_as_its_notes() {
        // A2, C3, E3, half a second each, at 44.1 kHz.
        let tempo = TempoMap::new(120.0);
        let rate = 44_100;
        let mut wave = Vec::new();
        for midi in [57.0, 60.0, 64.0] {
            wave.extend(vowel(hz_of(midi), 0.3, 4000.0, 0.5, rate));
        }
        let signal = Signal::new(&wave, &wave, rate, 0.0, &tempo);
        let value = measure(&signal);
        let notes: Vec<&str> = value["notes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["note"].as_str().unwrap())
            .collect();
        assert_eq!(notes, ["A2", "C3", "E3"], "{value}");
        // Each half a second: a beat at 120 BPM.
        let second = value["notes"][1]["beat"].as_f64().unwrap();
        assert!((second - 1.0).abs() < 0.1, "{value}");
    }

    #[test]
    fn noise_has_no_pitch() {
        let tempo = TempoMap::new(120.0);
        let wave = crate::testing::noise(48_000, 9);
        let signal = Signal::new(&wave, &wave, 48_000, 0.0, &tempo);
        let value = measure(&signal);
        assert!(value["voiced_share"].as_f64().unwrap() < 0.1, "{value}");
    }
}
