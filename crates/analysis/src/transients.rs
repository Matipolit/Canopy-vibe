//! Transients: where hits and notes start (`media::detect_transients`, the
//! detector the clip menu and the Slicer use), how strong each is, how far
//! each sits from the sixteenth-note grid, and the tempo they suggest.

use media::{AudioBuffer, detect_transients};
use serde_json::{Value, json};

use crate::Signal;
use crate::report::{self, amplitude_db};

/// How eagerly onsets are found (0 to 1), as the clip menu's default.
pub const SENSITIVITY: f32 = 0.5;

/// A hit's strength is its peak within this long after its onset.
const STRENGTH_SECONDS: f64 = 0.05;

/// The grid offsets are measured against, in beats: sixteenth notes.
const GRID_BEATS: f64 = 0.25;

/// Onsets listed one by one; past this only the count says how many more.
const LISTED: usize = 500;

/// Tempos the estimate may land on, in BPM, the step it searches in, and
/// how far (in octaves, one standard deviation) it leans towards 120 when
/// half or double a tempo fit about as well.
const TEMPO_LOWEST: f64 = 70.0;
const TEMPO_HIGHEST: f64 = 180.0;
const TEMPO_STEP: f64 = 0.1;
const TEMPO_LEANING: f64 = 1.0;
/// Onsets this far from a multiple of the beat (in seconds, one standard
/// deviation) still count as on it.
const TEMPO_SLACK: f64 = 0.01;
/// Pairs of onsets further apart than this don't vote.
const TEMPO_HORIZON: f64 = 4.0;
/// Only this many of the strongest onsets vote, which keeps a long busy
/// render's estimate to a few million steps.
const TEMPO_VOTERS: usize = 400;

/// An onset: where (in frames) and how strong (linear peak after it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Onset {
    pub frame: u64,
    pub peak: f32,
}

/// The onsets in `signal`, in order.
pub fn onsets(signal: &Signal) -> Vec<Onset> {
    let n = signal.frames();
    let audio = AudioBuffer::new(
        signal.rate,
        signal.left[..n].to_vec(),
        signal.right[..n].to_vec(),
    );
    let after = (STRENGTH_SECONDS * f64::from(signal.rate)) as usize;
    detect_transients(&audio, 0, n as u64, SENSITIVITY)
        .into_iter()
        .map(|frame| {
            let from = (frame as usize).min(n);
            let to = (from + after).min(n);
            let peak = signal.left[from..to]
                .iter()
                .chain(&signal.right[from..to])
                .fold(0.0f32, |m, s| m.max(s.abs()));
            Onset { frame, peak }
        })
        .collect()
}

/// The tempo whose beats best line up the onsets (`seconds`, weight),
/// between 70 and 180 BPM: every pair of onsets within 4 s votes for the
/// tempos that put a whole number of beats between them, weighted by both
/// strengths. `None` for fewer than three onsets.
pub fn tempo(onsets: &[(f64, f64)]) -> Option<f64> {
    if onsets.len() < 3 {
        return None;
    }
    let mut onsets = onsets.to_vec();
    if onsets.len() > TEMPO_VOTERS {
        onsets.sort_by(|a, b| b.1.total_cmp(&a.1));
        onsets.truncate(TEMPO_VOTERS);
        onsets.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    let mut pairs = Vec::new();
    for (i, &(t, w)) in onsets.iter().enumerate() {
        for &(u, v) in &onsets[i + 1..] {
            if u - t > TEMPO_HORIZON {
                break;
            }
            pairs.push((u - t, w * v));
        }
    }
    let steps = ((TEMPO_HIGHEST - TEMPO_LOWEST) / TEMPO_STEP).round() as usize;
    let mut best = (f64::MIN, None);
    for step in 0..=steps {
        let bpm = TEMPO_LOWEST + step as f64 * TEMPO_STEP;
        let beat = 60.0 / bpm;
        let fit: f64 = pairs
            .iter()
            .map(|&(gap, weight)| {
                let beats = (gap / beat).round();
                if beats < 1.0 {
                    return 0.0;
                }
                let miss = (gap - beats * beat) / TEMPO_SLACK;
                weight * (-0.5 * miss * miss).exp()
            })
            .sum();
        let leaning = (bpm / 120.0).log2() / TEMPO_LEANING;
        let score = fit * (-0.5 * leaning * leaning).exp();
        if score > best.0 {
            best = (score, Some(bpm));
        }
    }
    best.1.filter(|_| best.0 > 0.0)
}

/// The transients measure.
pub fn measure(signal: &Signal) -> Value {
    let found = onsets(signal);
    let rate = f64::from(signal.rate.max(1));
    let tempo_bpm = tempo(
        &found
            .iter()
            .map(|o| (o.frame as f64 / rate, f64::from(o.peak)))
            .collect::<Vec<_>>(),
    );
    let seconds_per_beat = signal.tempo.ticks_to_seconds(midi::PPQ as f64);
    let mut offsets = Vec::with_capacity(found.len());
    let listed: Vec<Value> = found
        .iter()
        .map(|o| {
            let beat = signal.beat_at(o.frame as f64);
            let off = beat - (beat / GRID_BEATS).round() * GRID_BEATS;
            offsets.push(off * seconds_per_beat * 1000.0);
            json!({
                "beat": beat,
                "at": signal.bar_beat(beat),
                "db": amplitude_db(f64::from(o.peak)),
                "off_beats": off,
            })
        })
        .take(LISTED)
        .collect();
    let mean = |values: &[f64]| {
        (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
    };
    let absolute: Vec<f64> = offsets.iter().map(|o| o.abs()).collect();
    report::round(json!({
        "count": found.len(),
        "onsets": listed,
        "tempo_bpm": tempo_bpm,
        "mean_offset_ms": mean(&offsets),
        "mean_abs_offset_ms": mean(&absolute),
    }))
}

/// One sentence: how many onsets, the tempo they suggest, how tight.
pub fn summary(value: &Value) -> String {
    let count = value["count"].as_u64().unwrap_or(0);
    if count == 0 {
        return "No transients found.".to_string();
    }
    let tempo = match value["tempo_bpm"].as_f64() {
        Some(bpm) => format!(", suggesting {bpm:.1} BPM"),
        None => String::new(),
    };
    let offset = value["mean_offset_ms"].as_f64().unwrap_or(0.0);
    let lean = if offset > 0.5 {
        "late"
    } else if offset < -0.5 {
        "early"
    } else {
        "centred"
    };
    format!(
        "{count} onset{}{tempo}; on average {} ms from the sixteenth grid, {lean}.",
        if count == 1 { "" } else { "s" },
        report::tenth(&value["mean_abs_offset_ms"]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use project::TempoMap;

    /// Short decaying noise clicks at `beats`, at 120 BPM.
    fn clicks(beats: &[f64], seconds: f64, rate: u32) -> Vec<f32> {
        let mut wave = vec![0.0f32; (seconds * f64::from(rate)) as usize];
        let noise = crate::testing::noise(rate as usize / 20, 5);
        for &beat in beats {
            let start = (beat * 0.5 * f64::from(rate)) as usize;
            for (i, n) in noise.iter().enumerate() {
                if let Some(s) = wave.get_mut(start + i) {
                    *s = 0.8 * n * (-(i as f32) / (0.005 * rate as f32)).exp();
                }
            }
        }
        wave
    }

    #[test]
    fn clicks_are_found_where_they_are() {
        let tempo = TempoMap::new(120.0);
        let rate = 48_000;
        let wave = clicks(&[0.0, 1.0, 2.0, 3.0], 2.5, rate);
        let signal = Signal::new(&wave, &wave, rate, 0.0, &tempo);
        let value = measure(&signal);
        assert_eq!(value["count"], 4, "{value}");
        for (k, onset) in value["onsets"].as_array().unwrap().iter().enumerate() {
            let beat = onset["beat"].as_f64().unwrap();
            // 5 ms is a hundredth of a beat at 120 BPM.
            assert!((beat - k as f64).abs() <= 0.01, "{value}");
            assert!(onset["off_beats"].as_f64().unwrap().abs() <= 0.01);
        }
        assert_eq!(value["onsets"][2]["at"], "1.3");
        let tempo = value["tempo_bpm"].as_f64().unwrap();
        assert!((tempo - 120.0).abs() <= 1.0, "{value}");
        assert!(value["mean_abs_offset_ms"].as_f64().unwrap() <= 5.0);
    }

    #[test]
    fn eighth_notes_suggest_the_beat_not_a_triplet() {
        let tempo = TempoMap::new(120.0);
        let rate = 48_000;
        let beats: Vec<f64> = (0..16).map(|k| k as f64 * 0.5).collect();
        let wave = clicks(&beats, 8.5 * 0.5, rate);
        let signal = Signal::new(&wave, &wave, rate, 0.0, &tempo);
        let value = measure(&signal);
        assert_eq!(value["count"], 16, "{value}");
        let bpm = value["tempo_bpm"].as_f64().unwrap();
        assert!((bpm - 120.0).abs() <= 1.0, "{value}");
    }

    #[test]
    fn times_follow_the_signals_place_in_the_song() {
        let tempo = TempoMap::new(120.0);
        let rate = 48_000;
        let wave = clicks(&[1.0], 1.0, rate);
        let signal = Signal::new(&wave, &wave, rate, 8.0, &tempo);
        let value = measure(&signal);
        assert_eq!(value["onsets"][0]["at"], "3.2", "{value}");
    }
}
