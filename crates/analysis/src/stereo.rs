//! The stereo image: correlation and balance of the two channels, how wide
//! the low, middle and high frequencies are (side against mid), and how
//! much level is lost when the mix is summed to mono.

use serde_json::{Value, json};

use crate::Signal;
use crate::report::{self, FLOOR_DB, power_db};
use crate::spectrum::{self, FFT};

/// The three bands whose width is given, in Hz.
pub const BANDS: [(&str, f32, f32); 3] = [
    ("low", 20.0, 250.0),
    ("mid", 250.0, 4000.0),
    ("high", 4000.0, 20_000.0),
];

/// Power this small is silence.
const QUIET: f64 = 1e-10;

/// The stereo measure.
pub fn measure(signal: &Signal) -> Value {
    let n = signal.frames();
    let (left, right) = (&signal.left[..n], &signal.right[..n]);
    let (mut ll, mut rr, mut lr, mut mm) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for (&l, &r) in left.iter().zip(right) {
        let (l, r) = (f64::from(l), f64::from(r));
        ll += l * l;
        rr += r * r;
        lr += l * r;
        let m = 0.5 * (l + r);
        mm += m * m;
    }
    let count = n.max(1) as f64;
    let (ll, rr, lr, mm) = (ll / count, rr / count, lr / count, mm / count);
    let silent = ll + rr <= QUIET;
    let correlation = (ll > QUIET && rr > QUIET).then(|| (lr / (ll * rr).sqrt()).clamp(-1.0, 1.0));
    let balance = (!silent).then(|| (rr - ll) / (rr + ll));
    // Mono as (L + R) / 2 against the channels' mean power: 0 dB lost when
    // they are the same, 3 dB for unrelated channels, everything when
    // they cancel.
    let mono_loss = (!silent).then(|| -power_db(mm / (0.5 * (ll + rr))).max(FLOOR_DB));

    let (mid, side): (Vec<f32>, Vec<f32>) = left
        .iter()
        .zip(right)
        .map(|(l, r)| (0.5 * (l + r), 0.5 * (l - r)))
        .unzip();
    let mid = spectrum::average_of(&mid, signal.rate, FFT);
    let side = spectrum::average_of(&side, signal.rate, FFT);
    let mut width = serde_json::Map::new();
    for (name, low, high) in BANDS {
        let (m, s) = (
            f64::from(mid.band_power(low, high)),
            f64::from(side.band_power(low, high)),
        );
        let value = if m + s <= QUIET {
            Value::Null
        } else if m <= QUIET {
            json!(-FLOOR_DB)
        } else {
            json!(power_db(s / m))
        };
        width.insert(name.to_string(), value);
    }

    report::round(json!({
        "correlation": correlation,
        "balance": balance,
        "mono_loss_db": mono_loss,
        "width_db": width,
    }))
}

/// One sentence: correlation, width and what mono costs.
pub fn summary(value: &Value) -> String {
    if value["balance"].is_null() {
        return "Silent: no stereo image.".to_string();
    }
    let width = |band: &str| report::tenth(&value["width_db"][band]);
    format!(
        "Correlation {}, side against mid {} / {} / {} dB (low / mid / high), {} dB lost in mono.",
        value["correlation"]
            .as_f64()
            .map_or_else(|| "n/a".to_string(), |c| format!("{c:.2}")),
        width("low"),
        width("mid"),
        width("high"),
        report::tenth(&value["mono_loss_db"]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{noise, sine};
    use project::TempoMap;

    #[test]
    fn a_mono_signal_has_correlation_one_and_no_loss() {
        let tempo = TempoMap::new(120.0);
        let wave = sine(440.0, 0.5, 2.0, 48_000);
        let signal = Signal::new(&wave, &wave, 48_000, 0.0, &tempo);
        let value = measure(&signal);
        assert_eq!(value["correlation"], json!(1.0));
        assert_eq!(value["mono_loss_db"], json!(0.0));
        assert_eq!(value["balance"], json!(0.0));
        assert_eq!(value["width_db"]["mid"], json!(-100.0));
    }

    #[test]
    fn unrelated_channels_are_wide_and_lose_three_db() {
        let tempo = TempoMap::new(120.0);
        let (left, right) = (noise(96_000, 1), noise(96_000, 99));
        let signal = Signal::new(&left, &right, 48_000, 0.0, &tempo);
        let value = measure(&signal);
        assert!(
            value["correlation"].as_f64().unwrap().abs() < 0.05,
            "{value}"
        );
        let loss = value["mono_loss_db"].as_f64().unwrap();
        assert!((loss - 3.0).abs() < 0.1, "{value}");
        for band in ["mid", "high"] {
            let width = value["width_db"][band].as_f64().unwrap();
            assert!(width.abs() < 0.5, "{band}: {value}");
        }
    }

    #[test]
    fn opposite_channels_cancel_in_mono() {
        let tempo = TempoMap::new(120.0);
        let left = sine(440.0, 0.5, 1.0, 48_000);
        let right: Vec<f32> = left.iter().map(|x| -x).collect();
        let signal = Signal::new(&left, &right, 48_000, 0.0, &tempo);
        let value = measure(&signal);
        assert_eq!(value["correlation"], json!(-1.0));
        assert_eq!(value["mono_loss_db"], json!(100.0));
        assert_eq!(value["width_db"]["mid"], json!(100.0));
    }
}
