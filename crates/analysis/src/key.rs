//! The key, estimated as Krumhansl and Kessler's method does: how much of
//! each pitch class sounds (the chroma), correlated with their 24 key
//! profiles, the best fit winning. The chroma comes from the peaks of the
//! average spectrum, so a peak's main lobe doesn't spill into the
//! neighbouring semitones at low frequencies.

use serde_json::{Map, Value, json};

use crate::Signal;
use crate::report::FLOOR_DB;
use crate::spectrum::{self, FFT};

/// The frequencies whose peaks count, in Hz: below, a bin is wider than a
/// semitone; above, harmonics outnumber the notes.
const LOWEST_HZ: f32 = 60.0;
const HIGHEST_HZ: f32 = 5000.0;

/// Krumhansl and Kessler's probe-tone ratings, tonic first.
const MAJOR: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

/// Pitch classes as the chroma lists them, from C.
pub const CLASSES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
/// Tonics as keys are usually spelled.
const MAJOR_TONICS: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];
const MINOR_TONICS: [&str; 12] = [
    "C", "C#", "D", "Eb", "E", "F", "F#", "G", "G#", "A", "Bb", "B",
];

/// How much of each pitch class (C first) sounds in `signal`: each
/// spectral peak's magnitude added to its nearest semitone's class.
pub fn chroma(signal: &Signal) -> [f64; 12] {
    let spectrum = spectrum::average(signal, FFT);
    let bins = &spectrum.bins;
    let mut chroma = [0.0f64; 12];
    let first = (spectrum.bin(LOWEST_HZ).ceil() as usize).max(1);
    let last = (spectrum.bin(HIGHEST_HZ).floor() as usize).min(bins.len() - 2);
    for k in first..=last {
        let (a, b, c) = (bins[k - 1], bins[k], bins[k + 1]);
        if b <= a || b < c || b <= FLOOR_DB as f32 + 1.0 {
            continue;
        }
        let curve = a - 2.0 * b + c;
        let offset = if curve < 0.0 {
            (0.5 * (a - c) / curve).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        let hz = spectrum.hz(k as f32 + offset);
        let midi = crate::pitch::midi_of(f64::from(hz)).round() as i64;
        let magnitude = f64::from(spectrum.power[k]).sqrt();
        chroma[midi.rem_euclid(12) as usize] += magnitude;
    }
    chroma
}

fn correlation(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let mean = |x: &[f64; 12]| x.iter().sum::<f64>() / 12.0;
    let (ma, mb) = (mean(a), mean(b));
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        ab += (x - ma) * (y - mb);
        aa += (x - ma) * (x - ma);
        bb += (y - mb) * (y - mb);
    }
    if aa <= 0.0 || bb <= 0.0 {
        0.0
    } else {
        ab / (aa * bb).sqrt()
    }
}

/// Every key's name and how well the chroma fits it, best first.
pub fn keys(chroma: &[f64; 12]) -> Vec<(String, f64)> {
    let mut fits = Vec::with_capacity(24);
    for tonic in 0..12 {
        for (profile, names, mode) in [
            (&MAJOR, &MAJOR_TONICS, "major"),
            (&MINOR, &MINOR_TONICS, "minor"),
        ] {
            let mut rotated = [0.0; 12];
            for (i, r) in rotated.iter_mut().enumerate() {
                *r = profile[(i + 12 - tonic) % 12];
            }
            fits.push((
                format!("{} {mode}", names[tonic]),
                correlation(chroma, &rotated),
            ));
        }
    }
    fits.sort_by(|a, b| b.1.total_cmp(&a.1));
    fits
}

/// The key measure.
pub fn measure(signal: &Signal) -> Value {
    let chroma = chroma(signal);
    let loudest = chroma.iter().copied().fold(0.0, f64::max);
    if loudest <= 0.0 {
        return json!({ "key": null });
    }
    let fits = keys(&chroma);
    let shares: Map<String, Value> = CLASSES
        .iter()
        .zip(chroma)
        .map(|(name, c)| (name.to_string(), json!(c / loudest)))
        .collect();
    crate::report::round(json!({
        "key": fits[0].0,
        "correlation": fits[0].1,
        "runner_up": fits[1].0,
        "runner_up_correlation": fits[1].1,
        "confidence": fits[0].1 - fits[1].1,
        "chroma": shares,
    }))
}

/// One sentence: the key, the runner-up and how clear the choice is.
pub fn summary(value: &Value) -> String {
    let Some(key) = value["key"].as_str() else {
        return "No pitched content to find a key in.".to_string();
    };
    let confidence = value["confidence"].as_f64().unwrap_or(0.0);
    let clarity = if confidence >= 0.1 {
        "Clearly"
    } else if confidence >= 0.03 {
        "Probably"
    } else {
        "Perhaps"
    };
    format!(
        "{clarity} {key} (correlation {:.2}); next best {} ({:.2}).",
        value["correlation"].as_f64().unwrap_or(0.0),
        value["runner_up"].as_str().unwrap_or("?"),
        value["runner_up_correlation"].as_f64().unwrap_or(0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use project::TempoMap;
    use std::f64::consts::TAU;

    /// A chord of MIDI notes, each with three harmonics, `seconds` long.
    fn chord(notes: &[i32], seconds: f64, rate: u32) -> Vec<f32> {
        let n = (seconds * f64::from(rate)) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / f64::from(rate);
                let sum: f64 = notes
                    .iter()
                    .flat_map(|&note| {
                        let hz = 440.0 * 2f64.powf((f64::from(note) - 69.0) / 12.0);
                        (1..=3).map(move |k| (TAU * hz * k as f64 * t).sin() / (k * k) as f64)
                    })
                    .sum();
                (0.1 * sum) as f32
            })
            .collect()
    }

    #[test]
    fn a_c_major_progression_is_in_c_major() {
        // C, F, G, C, with a bass note under each.
        let rate = 48_000;
        let mut wave = Vec::new();
        for (bass, triad) in [
            (48, [60, 64, 67]),
            (53, [65, 69, 72]),
            (55, [67, 71, 74]),
            (48, [60, 64, 67]),
        ] {
            wave.extend(chord(&[bass, triad[0], triad[1], triad[2]], 1.0, rate));
        }
        let tempo = TempoMap::new(120.0);
        let signal = Signal::new(&wave, &wave, rate, 0.0, &tempo);
        let value = measure(&signal);
        assert_eq!(value["key"], "C major", "{value}");
        assert!(value["confidence"].as_f64().unwrap() > 0.0);
        assert!(summary(&value).contains("C major"));
    }

    #[test]
    fn an_a_minor_progression_is_in_a_minor() {
        // Am, Dm, E, Am.
        let rate = 48_000;
        let mut wave = Vec::new();
        for (bass, triad) in [
            (45, [57, 60, 64]),
            (50, [62, 65, 69]),
            (52, [64, 68, 71]),
            (45, [57, 60, 64]),
        ] {
            wave.extend(chord(&[bass, triad[0], triad[1], triad[2]], 1.0, rate));
        }
        let tempo = TempoMap::new(120.0);
        let signal = Signal::new(&wave, &wave, rate, 0.0, &tempo);
        assert_eq!(measure(&signal)["key"], "A minor");
    }

    #[test]
    fn silence_has_no_key() {
        let quiet = vec![0.0f32; 48_000];
        let tempo = TempoMap::new(120.0);
        let signal = Signal::new(&quiet, &quiet, 48_000, 0.0, &tempo);
        let value = measure(&signal);
        assert!(value["key"].is_null());
        assert!(summary(&value).starts_with("No pitched"));
    }
}
