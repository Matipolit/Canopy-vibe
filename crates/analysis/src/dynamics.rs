//! Dynamics bar by bar: each bar's peak, RMS and crest factor, and the
//! loudest and quietest stretches of the song.

use serde_json::{Value, json};

use crate::Signal;
use crate::report::{self, FLOOR_DB, amplitude_db, power_db};

/// Bars quieter than this are silence, not the quietest section.
const AUDIBLE_DB: f64 = -80.0;

/// Neighbouring bars within this much of the loudest (or quietest) bar's
/// RMS belong to the same section.
const SECTION_DB: f64 = 1.5;

/// Each bar's sample peak and RMS (both channels), in dB.
pub fn bars(signal: &Signal) -> (i64, Vec<(f64, f64)>) {
    let (first, last) = signal.bars();
    let levels = (first..=last)
        .map(|bar| {
            let frames = signal.bar_frames(bar);
            let (mut peak, mut squares) = (0.0f32, 0.0f64);
            for i in frames.clone() {
                let (l, r) = (signal.left[i], signal.right[i]);
                peak = peak.max(l.abs()).max(r.abs());
                squares += f64::from(l) * f64::from(l) + f64::from(r) * f64::from(r);
            }
            let mean = squares / (2 * frames.len().max(1)) as f64;
            (amplitude_db(f64::from(peak)), power_db(mean))
        })
        .collect();
    (first, levels)
}

/// The bar at index `extreme`, widened over its neighbours within
/// [`SECTION_DB`] of it, as `{ bars: "9–12", rms_db }` (their mean power).
fn section(first: i64, rms: &[f64], extreme: usize) -> Value {
    let close = |i: usize| (rms[i] - rms[extreme]).abs() <= SECTION_DB;
    let mut from = extreme;
    while from > 0 && close(from - 1) {
        from -= 1;
    }
    let mut to = extreme;
    while to + 1 < rms.len() && close(to + 1) {
        to += 1;
    }
    let power = rms[from..=to]
        .iter()
        .map(|db| 10f64.powf(db / 10.0))
        .sum::<f64>()
        / (to - from + 1) as f64;
    json!({
        "bars": report::bar_span(first + from as i64, first + to as i64),
        "rms_db": power_db(power),
    })
}

/// The dynamics measure.
pub fn measure(signal: &Signal) -> Value {
    let (first, levels) = bars(signal);
    let peaks: Vec<f64> = levels.iter().map(|l| l.0).collect();
    let rms: Vec<f64> = levels.iter().map(|l| l.1).collect();
    let crest: Vec<f64> = levels
        .iter()
        .map(|&(peak, rms)| if rms > FLOOR_DB { peak - rms } else { 0.0 })
        .collect();
    let audible: Vec<usize> = (0..rms.len()).filter(|&i| rms[i] > AUDIBLE_DB).collect();
    let by_rms = |a: &&usize, b: &&usize| rms[**a].total_cmp(&rms[**b]);
    let loudest = audible
        .iter()
        .max_by(by_rms)
        .map(|&i| section(first, &rms, i));
    let quietest = audible
        .iter()
        .min_by(by_rms)
        .map(|&i| section(first, &rms, i));
    let spread = match (audible.iter().max_by(by_rms), audible.iter().min_by(by_rms)) {
        (Some(&loud), Some(&quiet)) => Some(rms[loud] - rms[quiet]),
        _ => None,
    };
    report::round(json!({
        "bars": {
            "first": first,
            "peak_db": peaks,
            "rms_db": rms,
            "crest_db": crest,
        },
        "loudest": loudest,
        "quietest": quietest,
        "bar_range_db": spread,
    }))
}

/// One sentence: the loudest and quietest sections and the spread between.
pub fn summary(value: &Value) -> String {
    if value["loudest"].is_null() {
        return "Silent throughout.".to_string();
    }
    let span = |key: &str| {
        let bars = value[key]["bars"].as_str().unwrap_or("?");
        let word = if bars.contains('–') { "bars" } else { "bar" };
        format!(
            "{word} {bars} ({} dB RMS)",
            report::tenth(&value[key]["rms_db"])
        )
    };
    format!(
        "Loudest at {}, quietest at {}: {} dB between bars.",
        span("loudest"),
        span("quietest"),
        report::tenth(&value["bar_range_db"]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sine;
    use project::TempoMap;

    #[test]
    fn dynamics_finds_the_loud_bar() {
        // Four bars at 120 BPM (2 s each); the third 20 dB louder.
        let tempo = TempoMap::new(120.0);
        let rate = 48_000;
        let bar = 2 * rate as usize;
        let mut wave = sine(220.0, 0.05, 8.0, rate);
        for s in &mut wave[2 * bar..3 * bar] {
            *s *= 10.0;
        }
        let signal = Signal::new(&wave, &wave, rate, 0.0, &tempo);
        let value = measure(&signal);
        assert_eq!(value["loudest"]["bars"], "3", "{value}");
        assert_eq!(value["quietest"]["bars"], "1–2", "{value}");
        assert_eq!(value["bars"]["first"], 1);
        assert_eq!(value["bars"]["rms_db"].as_array().unwrap().len(), 4);
        let range = value["bar_range_db"].as_f64().unwrap();
        assert!((range - 20.0).abs() < 0.2, "{value}");
        let crest = value["bars"]["crest_db"][0].as_f64().unwrap();
        assert!((crest - 3.0).abs() < 0.1, "{value}");
        assert!(summary(&value).contains("bar 3"));
    }

    #[test]
    fn bars_are_counted_from_where_the_signal_starts() {
        // Starting at beat 6 (bar 2, beat 3) and lasting 4 beats: bars 2 and 3.
        let tempo = TempoMap::new(120.0);
        let wave = sine(220.0, 0.5, 2.0, 48_000);
        let signal = Signal::new(&wave, &wave, 48_000, 6.0, &tempo);
        let value = measure(&signal);
        assert_eq!(value["bars"]["first"], 2);
        assert_eq!(value["bars"]["rms_db"].as_array().unwrap().len(), 2);
    }
}
