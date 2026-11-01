//! Sibilance: the "s", "sh" and "t" sounds of a voice, found as moments
//! when the 4–12 kHz band is at least as loud as the voice below it
//! (200 Hz–4 kHz) and 6 dB above its own usual level, for 20 ms or more.
//! Each event gives where it is, how long, the frequency it peaks at, and
//! how far it rises above the surrounding voice; together they give the
//! band a de-esser should work on and how hard.

use serde_json::{Value, json};

use crate::Signal;
use crate::report::{self, power_db};
use crate::spectrum::{Stft, band_power, each_frame};

const FFT: usize = 1024;
const HOP: usize = 256;

/// The sibilant band and the voice's band, in Hz.
pub const SIBILANT: (f32, f32) = (4000.0, 12_000.0);
pub const VOICE: (f32, f32) = (200.0, 4000.0);

/// How far the sibilant band must stand above the voice in the same frame,
/// and above its own median, in dB.
const OVER_VOICE_DB: f64 = 0.0;
const OVER_MEDIAN_DB: f64 = 6.0;
/// The shortest event, in seconds.
const SHORTEST: f64 = 0.02;
/// A gap this many frames long doesn't split an event.
const BRIDGE: usize = 1;
/// Frames quieter than this in both bands are silence, left out of the
/// medians.
const AUDIBLE_DB: f64 = -80.0;
/// The voice an event is compared with: the frames within this many
/// seconds either side that aren't sibilant.
const SURROUNDINGS: f64 = 0.5;
/// Events listed one by one.
const LISTED: usize = 200;

/// A sibilant moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Start and end, in frames of the signal.
    pub start: usize,
    pub end: usize,
    /// Where the sibilant band peaks, averaged over the event.
    pub peak_hz: f32,
    /// The sibilant band's loudest frame, in dB.
    pub db: f64,
    /// How far that is above the surrounding voice's median, in dB.
    pub above_voice: f64,
}

/// The sibilant events in `signal`, in order, and the voice's median level
/// in dB (`None` for silence).
pub fn events(signal: &Signal) -> (Vec<Event>, Option<f64>) {
    let mono = signal.mono();
    let rate = signal.rate.max(1);
    let bin_hz = rate as f32 / FFT as f32;
    let mut stft = Stft::new(FFT);
    let mut high = Vec::new();
    let mut low = Vec::new();
    each_frame(&mono, &mut stft, HOP, |_, power| {
        high.push(power_db(f64::from(band_power(
            power, bin_hz, SIBILANT.0, SIBILANT.1,
        ))));
        low.push(power_db(f64::from(band_power(
            power, bin_hz, VOICE.0, VOICE.1,
        ))));
    });
    let audible: Vec<bool> = (0..high.len())
        .map(|f| high[f] > AUDIBLE_DB || low[f] > AUDIBLE_DB)
        .collect();
    let of_audible = |values: &[f64]| -> Vec<f64> {
        (0..values.len())
            .filter(|&f| audible[f])
            .map(|f| values[f])
            .collect()
    };
    let Some(usual_high) = report::median(&of_audible(&high)) else {
        return (Vec::new(), None);
    };
    let voice = report::median(&of_audible(&low));

    let sibilant: Vec<bool> = (0..high.len())
        .map(|f| {
            high[f] > AUDIBLE_DB
                && high[f] >= low[f] + OVER_VOICE_DB
                && high[f] >= usual_high + OVER_MEDIAN_DB
        })
        .collect();
    // Runs of sibilant frames, bridging short gaps.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (f, &yes) in sibilant.iter().enumerate() {
        if !yes {
            continue;
        }
        match runs.last_mut() {
            Some(run) if f - run.1 <= BRIDGE + 1 => run.1 = f,
            _ => runs.push((f, f)),
        }
    }

    runs.retain(|&(a, b)| ((b - a + 1) * HOP) as f64 / f64::from(rate) >= SHORTEST);

    // A second pass sums each event's spectrum over the sibilant band, so
    // the frames' spectra needn't all be kept.
    let first_bin = (SIBILANT.0 / bin_hz).ceil() as usize;
    let last_bin = ((SIBILANT.1 / bin_hz).floor() as usize).min(FFT / 2);
    let mut sums = vec![vec![0.0f32; last_bin + 1 - first_bin]; runs.len()];
    if !runs.is_empty() {
        let mut run = 0;
        each_frame(&mono, &mut stft, HOP, |start, power| {
            let frame = start / HOP;
            while run < runs.len() && runs[run].1 < frame {
                run += 1;
            }
            if run < runs.len() && runs[run].0 <= frame {
                for (s, p) in sums[run].iter_mut().zip(&power[first_bin..=last_bin]) {
                    *s += p;
                }
            }
        });
    }

    let reach = (SURROUNDINGS * f64::from(rate) / HOP as f64).round() as usize;
    let found = runs
        .into_iter()
        .zip(sums)
        .map(|((a, b), sum)| {
            let top = (0..sum.len())
                .max_by(|&i, &j| sum[i].total_cmp(&sum[j]))
                .unwrap_or(0);
            let db = high[a..=b].iter().copied().fold(f64::MIN, f64::max);
            let around: Vec<f64> = (a.saturating_sub(reach)..(b + reach + 1).min(low.len()))
                .filter(|&f| audible[f] && !sibilant[f])
                .map(|f| low[f])
                .collect();
            let voice_here = report::median(&around)
                .or(voice)
                .unwrap_or(report::FLOOR_DB);
            // A frame's centre stands for it; an event spans its frames' hops.
            let centre = |f: usize| f * HOP + FFT / 2;
            Event {
                start: (centre(a) - HOP / 2).min(mono.len()),
                end: (centre(b) + HOP / 2).min(mono.len()),
                peak_hz: (first_bin + top) as f32 * bin_hz,
                db,
                above_voice: db - voice_here,
            }
        })
        .collect();
    (found, voice)
}

/// The sibilance measure.
pub fn measure(signal: &Signal) -> Value {
    let (found, voice) = events(signal);
    let rate = f64::from(signal.rate.max(1));
    let listed: Vec<Value> = found
        .iter()
        .take(LISTED)
        .map(|e| {
            let beat = signal.beat_at(e.start as f64);
            json!({
                "beat": beat,
                "at": signal.bar_beat(beat),
                "duration_ms": (e.end - e.start) as f64 / rate * 1000.0,
                "peak_hz": e.peak_hz,
                "db": e.db,
                "above_voice_db": e.above_voice,
            })
        })
        .collect();
    let hz: Vec<f64> = found.iter().map(|e| f64::from(e.peak_hz)).collect();
    let db: Vec<f64> = found.iter().map(|e| e.db).collect();
    let above: Vec<f64> = found.iter().map(|e| e.above_voice).collect();
    let typical = (!found.is_empty()).then(|| {
        json!({
            "band_hz": [report::percentile(&hz, 0.25), report::percentile(&hz, 0.75)],
            "peak_hz": report::median(&hz),
            "median_db": report::median(&db),
            "p95_db": report::percentile(&db, 0.95),
            "median_above_voice_db": report::median(&above),
            "p95_above_voice_db": report::percentile(&above, 0.95),
        })
    });
    report::round(json!({
        "count": found.len(),
        "events": listed,
        "voice_db": voice,
        "typical": typical,
    }))
}

/// One sentence: how many events, in which band, how far above the voice.
pub fn summary(value: &Value) -> String {
    let count = value["count"].as_u64().unwrap_or(0);
    if count == 0 {
        return "No sibilance found.".to_string();
    }
    let typical = &value["typical"];
    format!(
        "{count} sibilant event{} peaking around {} Hz (mostly {}–{} Hz), typically {} dB above the voice, {} dB at the 95th percentile.",
        if count == 1 { "" } else { "s" },
        report::whole(&typical["peak_hz"]),
        report::whole(&typical["band_hz"][0]),
        report::whole(&typical["band_hz"][1]),
        report::tenth(&typical["median_above_voice_db"]),
        report::tenth(&typical["p95_above_voice_db"]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{band_noise, vowel};
    use project::TempoMap;

    const RATE: u32 = 48_000;

    #[test]
    fn ess_bursts_are_sibilance_near_their_frequency() {
        let tempo = TempoMap::new(120.0);
        let mut wave = vowel(200.0, 0.3, 6000.0, 3.0, RATE);
        let burst_len = (0.12 * f64::from(RATE)) as usize;
        let ess = band_noise(6500.0, 7500.0, burst_len, RATE, 11);
        for second in [0.5, 1.5, 2.5] {
            let start = (second * f64::from(RATE)) as usize;
            for (i, s) in ess.iter().enumerate() {
                // Faded in and out over 5 ms.
                let edge = (i.min(burst_len - i) as f32 / (0.005 * RATE as f32)).min(1.0);
                wave[start + i] += 0.3 * s * edge;
            }
        }
        let signal = Signal::new(&wave, &wave, RATE, 0.0, &tempo);
        let value = measure(&signal);
        assert_eq!(value["count"], 3, "{value}");
        for (k, event) in value["events"].as_array().unwrap().iter().enumerate() {
            let hz = event["peak_hz"].as_f64().unwrap();
            assert!((6000.0..=8000.0).contains(&hz), "{value}");
            // Each burst starts a second apart: beats 1, 3, 5 at 120 BPM.
            let beat = event["beat"].as_f64().unwrap();
            assert!((beat - (1.0 + 2.0 * k as f64)).abs() < 0.05, "{value}");
            let ms = event["duration_ms"].as_f64().unwrap();
            assert!((90.0..=150.0).contains(&ms), "{value}");
            assert!(event["above_voice_db"].as_f64().unwrap() > 0.0);
        }
        assert!(summary(&value).starts_with("3 sibilant events"));
    }

    #[test]
    fn a_vowel_alone_has_no_sibilance() {
        let tempo = TempoMap::new(120.0);
        let wave = vowel(200.0, 0.3, 6000.0, 3.0, RATE);
        let signal = Signal::new(&wave, &wave, RATE, 0.0, &tempo);
        let value = measure(&signal);
        assert_eq!(value["count"], 0, "{value}");
        assert!(value["typical"].is_null());
        assert_eq!(summary(&value), "No sibilance found.");
    }
}
