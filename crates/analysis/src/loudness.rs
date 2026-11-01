//! Loudness as the Analyzer effect measures it, over a whole render:
//! integrated loudness, the momentary and short-term maxima and loudness
//! range (ITU-R BS.1770-4, EBU R128 and Tech 3342), true peak (four times
//! oversampled), sample peak, RMS and crest factor. The filters, gates and
//! true-peak interpolator are the Analyzer's own
//! (`effects::analyzer::loudness`), so both read the same.

use effects::analyzer::loudness::{BiquadState, Gate, TAPS, TruePeak, k_weighting, lufs};
use serde_json::{Value, json};

use crate::Signal;
use crate::report::{self, amplitude_db, power_db};

/// Loudness is gathered in 100 ms pieces; momentary loudness spans 4 of
/// them, short-term 30, as in the Analyzer.
const PIECE_SECONDS: f64 = 0.1;
const MOMENTARY_PIECES: usize = 4;
const SHORT_TERM_PIECES: usize = 30;

/// Everything the loudness measure reads, before rounding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    /// `None` when nothing rose above the absolute gate (-70 LUFS).
    pub integrated: Option<f64>,
    /// `None` for a render shorter than 400 ms.
    pub momentary_max: Option<f64>,
    /// `None` for a render shorter than 3 s.
    pub short_term_max: Option<f64>,
    /// `None` until there are short-term values above the gates.
    pub range: Option<f64>,
    /// Linear, the higher of the two channels.
    pub true_peak: f32,
    pub sample_peak: f32,
    /// Mean square of both channels together.
    pub mean_square: f64,
}

/// Measures `signal` from start to end. A last piece shorter than 100 ms
/// doesn't count towards loudness, as it wouldn't on the meter yet.
pub fn loudness(signal: &Signal) -> Loudness {
    let rate = f64::from(signal.rate.max(1));
    let [shelf, high_pass] = k_weighting(rate);
    let mut filters = [[BiquadState::default(); 2]; 2];
    let piece_len = (PIECE_SECONDS * rate).round().max(1.0) as usize;
    let mut pieces = [0.0f64; SHORT_TERM_PIECES];
    let (mut piece_sum, mut piece_done, mut done) = (0.0f64, 0usize, 0usize);
    let mean = |pieces: &[f64; SHORT_TERM_PIECES], done: usize, count: usize| {
        let sum: f64 = (1..=count)
            .map(|back| pieces[(done + SHORT_TERM_PIECES - back) % SHORT_TERM_PIECES])
            .sum();
        sum / (count * piece_len) as f64
    };
    let mut integrated_gate = Gate::new();
    let mut range_gate = Gate::new();
    let (mut momentary_max, mut short_term_max) = (None::<f64>, None::<f64>);

    let mut true_peak = TruePeak::new();
    let (mut highest_true, mut highest_sample) = (0.0f32, 0.0f32);
    let mut squares = 0.0f64;

    let n = signal.frames();
    for (&l, &r) in signal.left[..n].iter().zip(&signal.right[..n]) {
        // A non-finite sample would stick in the filters forever.
        let clean = |x: f32| if x.is_finite() { x } else { 0.0 };
        let (l, r) = (clean(l), clean(r));
        let mut weighted = 0.0;
        for ([first, second], x) in filters.iter_mut().zip([l, r]) {
            let y = second.tick(&high_pass, first.tick(&shelf, f64::from(x)));
            weighted += y * y;
        }
        piece_sum += weighted;
        piece_done += 1;
        if piece_done == piece_len {
            pieces[done % SHORT_TERM_PIECES] = piece_sum;
            done += 1;
            piece_sum = 0.0;
            piece_done = 0;
            if done >= MOMENTARY_PIECES {
                let power = mean(&pieces, done, MOMENTARY_PIECES);
                integrated_gate.add(power);
                momentary_max = Some(momentary_max.unwrap_or(f64::MIN).max(lufs(power)));
            }
            if done >= SHORT_TERM_PIECES {
                let power = mean(&pieces, done, SHORT_TERM_PIECES);
                range_gate.add(power);
                short_term_max = Some(short_term_max.unwrap_or(f64::MIN).max(lufs(power)));
            }
        }

        let peaks = true_peak.push(l, r);
        highest_true = highest_true.max(peaks[0]).max(peaks[1]);
        highest_sample = highest_sample.max(l.abs()).max(r.abs());
        let (l, r) = (f64::from(l), f64::from(r));
        squares += l * l + r * r;
    }
    // The interpolator lags by half its length: silence after the end lets
    // it reach the last samples' in-between points, as playback would.
    for _ in 0..TAPS {
        let peaks = true_peak.push(0.0, 0.0);
        highest_true = highest_true.max(peaks[0]).max(peaks[1]);
    }

    Loudness {
        integrated: integrated_gate.integrated(),
        momentary_max,
        short_term_max,
        range: range_gate.range(),
        true_peak: highest_true,
        sample_peak: highest_sample,
        mean_square: squares / (2 * n.max(1)) as f64,
    }
}

/// The loudness measure.
pub fn measure(signal: &Signal) -> Value {
    let l = loudness(signal);
    let floor = |lufs: Option<f64>| lufs.map(|x| x.max(report::FLOOR_DB));
    let rms = power_db(l.mean_square);
    let peak = amplitude_db(f64::from(l.sample_peak));
    report::round(json!({
        "integrated_lufs": floor(l.integrated),
        "momentary_max_lufs": floor(l.momentary_max),
        "short_term_max_lufs": floor(l.short_term_max),
        "range_lu": l.range,
        "true_peak_dbtp": amplitude_db(f64::from(l.true_peak)),
        "sample_peak_db": peak,
        "rms_db": rms,
        "crest_db": if l.sample_peak > 0.0 { peak - rms } else { 0.0 },
    }))
}

/// One sentence: integrated loudness, range and true peak.
pub fn summary(value: &Value) -> String {
    if value["integrated_lufs"].is_null() {
        return format!(
            "Too quiet or short for integrated loudness; true peak {} dBTP, RMS {} dB.",
            report::tenth(&value["true_peak_dbtp"]),
            report::tenth(&value["rms_db"]),
        );
    }
    let range = if value["range_lu"].is_null() {
        String::new()
    } else {
        format!(", loudness range {} LU", report::tenth(&value["range_lu"]))
    };
    format!(
        "{} LUFS integrated{range}, true peak {} dBTP, crest factor {} dB.",
        report::tenth(&value["integrated_lufs"]),
        report::tenth(&value["true_peak_dbtp"]),
        report::tenth(&value["crest_db"]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sine;
    use project::TempoMap;

    #[test]
    fn loudness_matches_the_ebu_reference() {
        // EBU Tech 3341, as for case 1: a 1 kHz sine on both channels reads
        // its level in dBFS as LUFS.
        let tempo = TempoMap::new(120.0);
        for rate in [44_100, 48_000] {
            let wave = sine(1000.0, 0.1, 20.0, rate);
            let signal = Signal::new(&wave, &wave, rate, 0.0, &tempo);
            let value = measure(&signal);
            let integrated = value["integrated_lufs"].as_f64().unwrap();
            assert!((integrated + 20.0).abs() <= 0.1, "{rate}: {value}");
            for key in ["momentary_max_lufs", "short_term_max_lufs"] {
                let lufs = value[key].as_f64().unwrap();
                assert!((lufs + 20.0).abs() <= 0.1, "{key}: {value}");
            }
            assert!(value["range_lu"].as_f64().unwrap() <= 0.2, "{value}");
            assert_eq!(value["sample_peak_db"], json!(-20.0));
            assert_eq!(value["rms_db"], json!(-23.0));
            assert_eq!(value["crest_db"], json!(3.0));
        }
    }

    #[test]
    fn true_peak_finds_what_falls_between_samples() {
        // A quarter of the rate, sampled at ±45°: samples at 0.707 of the
        // wave's true peak of 1.
        let tempo = TempoMap::new(120.0);
        let rate = 48_000;
        let wave: Vec<f32> = (0..rate as usize)
            .map(|i| {
                (std::f64::consts::FRAC_PI_2 * i as f64 + std::f64::consts::FRAC_PI_4).sin() as f32
            })
            .collect();
        let signal = Signal::new(&wave, &wave, rate, 0.0, &tempo);
        let value = measure(&signal);
        assert_eq!(value["sample_peak_db"], json!(-3.0));
        let true_peak = value["true_peak_dbtp"].as_f64().unwrap();
        assert!(true_peak.abs() <= 0.3, "{value}");
    }

    #[test]
    fn silence_has_no_integrated_loudness() {
        let tempo = TempoMap::new(120.0);
        let quiet = vec![0.0f32; 48_000 * 4];
        let signal = Signal::new(&quiet, &quiet, 48_000, 0.0, &tempo);
        let value = measure(&signal);
        assert!(value["integrated_lufs"].is_null());
        assert_eq!(value["rms_db"], json!(-100.0));
        assert!(summary(&value).starts_with("Too quiet"));
    }
}
