//! How measures are written down: levels in dB with a floor for silence,
//! positions as "bar.beat", pitches by name, and the rounding that keeps the
//! JSON short (dB to a tenth, Hz to whole numbers).

use midi::{PPQ, Tick};
use project::TempoMap;
use serde_json::{Map, Number, Value};

/// What a level reads for silence, as in the Analyzer's meters: never
/// -infinity.
pub const FLOOR_DB: f64 = effects::analyzer::SILENCE_DB as f64;

/// `10·log10(power)`, or [`FLOOR_DB`] if lower.
pub fn power_db(power: f64) -> f64 {
    (10.0 * power.max(1e-30).log10()).max(FLOOR_DB)
}

/// `20·log10(amplitude)`, or [`FLOOR_DB`] if lower.
pub fn amplitude_db(amplitude: f64) -> f64 {
    (20.0 * amplitude.max(1e-30).log10()).max(FLOOR_DB)
}

/// Rounds every number in `value` to what is worth reading, chosen by the
/// key it sits under (or, for a key without a rule of its own such as an
/// array's elements or `low` inside `width_db`, by the nearest key above it
/// that has one):
///
/// - Hz (`hz`, `peak_hz`) and cents: whole numbers;
/// - dB, LUFS, LU, milliseconds and BPM: a tenth;
/// - beats: a thousandth;
/// - anything else (correlations, shares): a thousandth.
///
/// Integers stay as they are; a number that isn't finite becomes `null`.
pub fn round(value: Value) -> Value {
    round_with(value, DEFAULT_DECIMALS)
}

const DEFAULT_DECIMALS: u32 = 3;

fn decimals(key: &str) -> Option<u32> {
    let key = key.to_ascii_lowercase();
    if key.contains("hz") || key.ends_with("cents") {
        Some(0)
    } else if key.contains("db")
        || key.contains("lufs")
        || key == "lu"
        || key.ends_with("_lu")
        || key == "ms"
        || key.ends_with("_ms")
        || key.contains("bpm")
    {
        Some(1)
    } else if key.contains("beat") {
        Some(3)
    } else {
        None
    }
}

fn round_with(value: Value, places: u32) -> Value {
    match value {
        Value::Number(n) if n.is_f64() => {
            let x = n.as_f64().unwrap_or(f64::NAN);
            if !x.is_finite() {
                Value::Null
            } else if places == 0 {
                Value::from(x.round() as i64)
            } else {
                let scale = 10f64.powi(places as i32);
                // Adding 0.0 turns a rounded -0.0 into 0.0.
                Number::from_f64((x * scale).round() / scale + 0.0)
                    .map_or(Value::Null, Value::Number)
            }
        }
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|v| round_with(v, places)).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let places = decimals(&k).unwrap_or(places);
                    (k, round_with(v, places))
                })
                .collect::<Map<_, _>>(),
        ),
        other => other,
    }
}

/// A position in beats (quarter notes from the song's start) as
/// "bar.beat", both counted from 1 in `tempo`'s meter: "5.3" for bar 5's
/// third beat, "5.3.50" when halfway past it (the fraction in hundredths of
/// a beat, given only when off the beat).
pub fn bar_beat(beats: f64, tempo: &TempoMap) -> String {
    let ticks = (beats * PPQ as f64).max(0.0);
    let beat_len = tempo.beat_len().get().max(1) as f64;
    // Rounded to hundredths of a beat first, so 4.999 reads "2.1", not "1.4.100".
    let hundredths = (ticks / beat_len * 100.0).round();
    let tick = Tick::new((hundredths / 100.0 * beat_len).round() as i64);
    let position = tempo.bar_beat(tick);
    let fraction = (hundredths as i64).rem_euclid(100);
    if fraction == 0 {
        format!("{}.{}", position.bar, position.beat)
    } else {
        format!("{}.{}.{fraction:02}", position.bar, position.beat)
    }
}

/// A run of bars: "9" or "9–12".
pub fn bar_span(first: i64, last: i64) -> String {
    if first == last {
        first.to_string()
    } else {
        format!("{first}–{last}")
    }
}

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// A MIDI note's name with its octave, MIDI 60 being "C3" as in the piano
/// roll.
pub fn pitch_name(midi: i32) -> String {
    format!(
        "{}{}",
        NOTE_NAMES[midi.rem_euclid(12) as usize],
        midi.div_euclid(12) - 2
    )
}

/// The value `share` of the way through `values` (0.5 for the median),
/// interpolating between neighbours; `None` for no values.
pub fn percentile(values: &[f64], share: f64) -> Option<f64> {
    let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let position = share.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let below = position.floor() as usize;
    let above = position.ceil() as usize;
    let t = position - below as f64;
    Some(sorted[below] + (sorted[above] - sorted[below]) * t)
}

pub fn median(values: &[f64]) -> Option<f64> {
    percentile(values, 0.5)
}

/// A level for a sentence, to a tenth, or "n/a".
pub(crate) fn tenth(value: &Value) -> String {
    value
        .as_f64()
        .map_or_else(|| "n/a".to_string(), |x| format!("{x:.1}"))
}

/// A whole number for a sentence, or "n/a".
pub(crate) fn whole(value: &Value) -> String {
    value
        .as_f64()
        .map_or_else(|| "n/a".to_string(), |x| format!("{}", x.round() as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rounding_follows_the_keys() {
        let rounded = round(json!({
            "peak_hz": 1000.4,
            "level_db": -3.04,
            "width_db": { "low": -12.345 },
            "bands": { "hz": [19.6, 25.2], "db": [-0.04, -10.06] },
            "beat": 1.23456,
            "correlation": 0.99951,
            "count": 3,
            "missing": f64::NAN,
        }));
        assert_eq!(
            rounded,
            json!({
                "peak_hz": 1000,
                "level_db": -3.0,
                "width_db": { "low": -12.3 },
                "bands": { "hz": [20, 25], "db": [0.0, -10.1] },
                "beat": 1.235,
                "correlation": 1.0,
                "count": 3,
                "missing": null,
            })
        );
    }

    #[test]
    fn bar_beat_counts_from_one_and_shows_fractions_off_the_beat() {
        let tempo = TempoMap::new(120.0);
        assert_eq!(bar_beat(0.0, &tempo), "1.1");
        assert_eq!(bar_beat(4.0, &tempo), "2.1");
        assert_eq!(bar_beat(18.5, &tempo), "5.3.50");
        assert_eq!(bar_beat(3.9999, &tempo), "2.1");
    }

    #[test]
    fn pitch_names_put_middle_c_in_octave_three() {
        assert_eq!(pitch_name(60), "C3");
        assert_eq!(pitch_name(57), "A2");
        assert_eq!(pitch_name(0), "C-2");
    }

    #[test]
    fn percentiles_interpolate() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(percentile(&[0.0, 10.0], 0.95), Some(9.5));
        assert_eq!(median(&[]), None);
    }
}
