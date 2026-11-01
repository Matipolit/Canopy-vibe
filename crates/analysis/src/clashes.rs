//! Which tracks fight over the same frequencies. Every track (and the mix
//! they make) is split into the 24 critical bands of hearing (the Bark
//! scale) over windows a tenth of a second apart. Two tracks clash in a band
//! and window when both are within 40 dB of the mix there, so both are part
//! of what is heard, and within 6 dB of each other, so neither clearly masks
//! the other: the ear can't tell which of them it hears.
//!
//! Pairs are ranked by how long they clash, each clashing band and window
//! weighted by the pair's level there, so a loud, lasting fight comes before
//! a faint or brief one. Fixing a clash (EQ, ducking, panning, arranging) is
//! left to whoever reads the report.

use serde_json::{Value, json};

use crate::Signal;
use crate::report::{self, power_db};
use crate::spectrum::{self, HANN_BANDWIDTH, Stft, band_power};

/// The edges of Zwicker's 24 critical bands, in Hz.
pub const BARK_EDGES: [f32; BANDS + 1] = [
    20.0, 100.0, 200.0, 300.0, 400.0, 510.0, 630.0, 770.0, 920.0, 1080.0, 1270.0, 1480.0, 1720.0,
    2000.0, 2320.0, 2700.0, 3150.0, 3700.0, 4400.0, 5300.0, 6400.0, 7700.0, 9500.0, 12_000.0,
    15_500.0,
];

pub const BANDS: usize = 24;

/// How far apart windows are.
pub const WINDOW_SECONDS: f64 = 0.1;

/// A track this far under the mix in a band is not part of what is heard
/// there.
pub const MIX_RANGE_DB: f32 = 40.0;

/// Two tracks closer than this in a band are a clash; further apart, the
/// louder masks the quieter.
pub const CLOSE_DB: f32 = 6.0;

/// Below this a band is silent, whatever the mix does.
const AUDIBLE_DB: f32 = -80.0;

/// A pair clashing in fewer than this share of the windows isn't reported.
const MIN_TIME_SHARE: f64 = 0.01;

/// A band is named for a pair when it clashes at least this share as often
/// as the pair's busiest band.
const BAND_SHARE_OF_BUSIEST: f64 = 0.2;

/// A bar is among the worst when its clashing weighs at least this share of
/// the worst bar's.
const BAR_SHARE_OF_WORST: f64 = 0.5;

/// At most this many band ranges and bar spans per pair.
const MOST_RANGES: usize = 3;

/// Each band's level in each window: the dB, and the amplitude the
/// weighting uses.
struct Levels {
    db: Vec<[f32; BANDS]>,
    amplitude: Vec<[f32; BANDS]>,
}

/// Window spacing and length: windows [`WINDOW_SECONDS`] apart, each the
/// longest power of two that fits in that, so they don't overlap.
fn window(rate: u32) -> (usize, usize) {
    let hop = ((f64::from(rate) * WINDOW_SECONDS).round() as usize).max(64);
    let size = 1usize << (usize::BITS - 1 - hop.leading_zeros());
    (hop, size)
}

fn levels(signal: &Signal, windows: usize, hop: usize, stft: &mut Stft) -> Levels {
    let mono = signal.mono();
    let bin_hz = signal.rate as f32 / stft.size() as f32;
    // `band_power` assumes Hann's bandwidth.
    let bandwidth = HANN_BANDWIDTH / stft.bandwidth();
    let mut power = vec![0.0; stft.bins()];
    let mut db = Vec::with_capacity(windows);
    let mut amplitude = Vec::with_capacity(windows);
    for w in 0..windows {
        let start = (w * hop).min(mono.len());
        let end = (start + stft.size()).min(mono.len());
        stft.power(&mono[start..end], &mut power);
        let mut levels = [0.0; BANDS];
        let mut amplitudes = [0.0; BANDS];
        for (b, (level, amplitude)) in levels.iter_mut().zip(&mut amplitudes).enumerate() {
            let p = band_power(&power, bin_hz, BARK_EDGES[b], BARK_EDGES[b + 1]) * bandwidth;
            *level = power_db(f64::from(p)) as f32;
            *amplitude = p.sqrt();
        }
        db.push(levels);
        amplitude.push(amplitudes);
    }
    Levels { db, amplitude }
}

/// Calls `each(window, band, a_louder, weight)` for every band and window
/// in which `a` and `b` clash under `mix`.
fn each_clash(a: &Levels, b: &Levels, mix: &Levels, mut each: impl FnMut(usize, usize, bool, f32)) {
    for (w, ((la, lb), lm)) in a.db.iter().zip(&b.db).zip(&mix.db).enumerate() {
        for band in 0..BANDS {
            let (x, y, heard) = (la[band], lb[band], lm[band] - MIX_RANGE_DB);
            if x > AUDIBLE_DB
                && y > AUDIBLE_DB
                && x >= heard
                && y >= heard
                && (x - y).abs() <= CLOSE_DB
            {
                let weight = 0.5 * (a.amplitude[w][band] + b.amplitude[w][band]);
                each(w, band, x > y, weight);
            }
        }
    }
}

/// The pairs of `tracks` (name, id, render) that clash under `mix`, the
/// `top` worst first. All renders start where the mix does, at its rate; a
/// shorter one is silent past its end.
///
/// Each pair gives its tracks, a `score` (its clashing weighed against the
/// worst pair's, which scores 1), the share of the time it clashes in any
/// band, the band ranges it clashes in (with the share of the time each
/// does), its worst bars ("9–12") and which track is usually the louder
/// where they clash. `clashing_pairs` counts every pair that clashes, those
/// past `top` included. The JSON is rounded and has a `summary` sentence.
pub fn clashes(tracks: &[(String, u32, Signal)], mix: &Signal, top: usize) -> Value {
    let (hop, size) = window(mix.rate);
    let windows = mix.frames().div_ceil(hop);
    // Hann's side lobes would leak a loud tone into the next band, where
    // the leaks of two tones a band apart look like a clash.
    let mut stft = Stft::with_window(spectrum::blackman_harris(size));
    let mix_levels = levels(mix, windows, hop, &mut stft);
    let track_levels: Vec<Levels> = tracks
        .iter()
        .map(|(_, _, signal)| levels(signal, windows, hop, &mut stft))
        .collect();

    // Ranked by score first; only the reported pairs are looked at closely.
    let least = ((windows as f64 * MIN_TIME_SHARE).ceil() as usize).max(1);
    let mut ranked: Vec<(usize, usize, f64)> = Vec::new();
    for i in 0..tracks.len() {
        for j in i + 1..tracks.len() {
            let (mut score, mut last, mut clashing) = (0.0f64, usize::MAX, 0usize);
            each_clash(
                &track_levels[i],
                &track_levels[j],
                &mix_levels,
                |w, _, _, weight| {
                    score += f64::from(weight);
                    if w != last {
                        clashing += 1;
                        last = w;
                    }
                },
            );
            if clashing >= least && score > 0.0 {
                ranked.push((i, j, score));
            }
        }
    }
    ranked.sort_by(|a, b| b.2.total_cmp(&a.2));
    let found = ranked.len();
    ranked.truncate(top);
    let worst = ranked.first().map_or(1.0, |r| r.2);

    // The bar each window's centre falls in.
    let (first_bar, last_bar) = mix.bars();
    let bar_of_window: Vec<usize> = (0..windows)
        .map(|w| {
            let centre = ((w * hop + size / 2).min(mix.frames().max(1) - 1)) as f64;
            let bar = mix.bar_of(mix.beat_at(centre)) - first_bar;
            bar.clamp(0, last_bar - first_bar) as usize
        })
        .collect();
    let bar_count = (last_bar - first_bar + 1).max(1) as usize;

    let pairs: Vec<Value> = ranked
        .iter()
        .map(|&(i, j, score)| {
            let mut per_window = vec![0u32; windows];
            let mut per_bar = vec![0.0f64; bar_count];
            let mut per_band = [0usize; BANDS];
            let (mut cells, mut a_louder) = (0usize, 0usize);
            each_clash(
                &track_levels[i],
                &track_levels[j],
                &mix_levels,
                |w, band, louder, weight| {
                    per_window[w] |= 1 << band;
                    per_bar[bar_of_window[w]] += f64::from(weight);
                    per_band[band] += 1;
                    cells += 1;
                    a_louder += usize::from(louder);
                },
            );
            let clashing = per_window.iter().filter(|&&mask| mask != 0).count();
            let share = |count: usize| count as f64 / windows.max(1) as f64;
            let (louder, louder_cells) = if 2 * a_louder >= cells {
                (&tracks[i].0, a_louder)
            } else {
                (&tracks[j].0, cells - a_louder)
            };
            let bands: Vec<Value> = band_ranges(&per_band)
                .into_iter()
                .map(|(from, to)| {
                    let mask = ((1u64 << (to + 1)) - (1u64 << from)) as u32;
                    let count = per_window.iter().filter(|&&m| m & mask != 0).count();
                    json!({
                        "from_hz": BARK_EDGES[from],
                        "to_hz": BARK_EDGES[to + 1],
                        "time_share": share(count),
                    })
                })
                .collect();
            json!({
                "tracks": [
                    { "name": tracks[i].0, "id": tracks[i].1 },
                    { "name": tracks[j].0, "id": tracks[j].1 },
                ],
                "score": score / worst,
                "time_share": share(clashing),
                "bands": bands,
                "worst_bars": worst_bars(&per_bar, first_bar),
                "louder": { "name": louder, "share": louder_cells as f64 / cells.max(1) as f64 },
            })
        })
        .collect();

    let mut value = report::round(json!({
        "tracks": tracks.len(),
        "window_ms": (WINDOW_SECONDS * 1000.0).round() as i64,
        "windows": windows,
        "clashing_pairs": found,
        "pairs": pairs,
    }));
    let sentence = summary(&value);
    value["summary"] = Value::String(sentence);
    value
}

/// The runs of neighbouring bands a pair clashes in often enough to name,
/// as first and last band, the busiest run first.
fn band_ranges(per_band: &[usize; BANDS]) -> Vec<(usize, usize)> {
    let busiest = per_band.iter().copied().max().unwrap_or(0);
    if busiest == 0 {
        return Vec::new();
    }
    let named = |b: usize| per_band[b] as f64 >= BAND_SHARE_OF_BUSIEST * busiest as f64;
    let mut runs: Vec<(usize, usize, usize)> = Vec::new();
    let mut b = 0;
    while b < BANDS {
        if !named(b) {
            b += 1;
            continue;
        }
        let from = b;
        while b + 1 < BANDS && named(b + 1) {
            b += 1;
        }
        runs.push((from, b, per_band[from..=b].iter().sum()));
        b += 1;
    }
    runs.sort_by_key(|run| std::cmp::Reverse(run.2));
    runs.truncate(MOST_RANGES);
    runs.into_iter().map(|(from, to, _)| (from, to)).collect()
}

/// The spans of neighbouring bars where a pair clashes worst ("9–12"),
/// the heaviest first.
fn worst_bars(per_bar: &[f64], first_bar: i64) -> Vec<String> {
    let worst = per_bar.iter().copied().fold(0.0, f64::max);
    if worst <= 0.0 {
        return Vec::new();
    }
    let bad = |i: usize| per_bar[i] >= BAR_SHARE_OF_WORST * worst;
    let mut spans: Vec<(usize, usize, f64)> = Vec::new();
    let mut i = 0;
    while i < per_bar.len() {
        if !bad(i) {
            i += 1;
            continue;
        }
        let from = i;
        while i + 1 < per_bar.len() && bad(i + 1) {
            i += 1;
        }
        spans.push((from, i, per_bar[from..=i].iter().sum()));
        i += 1;
    }
    spans.sort_by(|x, y| y.2.total_cmp(&x.2));
    spans.truncate(MOST_RANGES);
    spans
        .into_iter()
        .map(|(from, to, _)| report::bar_span(first_bar + from as i64, first_bar + to as i64))
        .collect()
}

/// One sentence: the worst pair, where and when, and how many pairs clash.
pub fn summary(value: &Value) -> String {
    let pairs = value["pairs"].as_array().map_or(&[][..], Vec::as_slice);
    if value["tracks"].as_u64().unwrap_or(0) < 2 {
        return "Fewer than two tracks: nothing can clash.".to_string();
    }
    let Some(worst) = pairs.first() else {
        return format!(
            "No two of the {} tracks clash: wherever they share a band, one is clearly louder or \
             both sit well under the mix.",
            value["tracks"]
        );
    };
    let name = |k: usize| {
        worst["tracks"][k]["name"]
            .as_str()
            .unwrap_or("?")
            .to_string()
    };
    let percent = |share: &Value| format!("{}%", (share.as_f64().unwrap_or(0.0) * 100.0).round());
    let mut sentence = format!(
        "{} and {} clash most, {} of the time",
        name(0),
        name(1),
        percent(&worst["time_share"]),
    );
    if let Some(band) = worst["bands"].get(0) {
        sentence += &format!(
            ", mostly {}–{} Hz",
            report::whole(&band["from_hz"]),
            report::whole(&band["to_hz"])
        );
    }
    if let Some(bars) = worst["worst_bars"].get(0).and_then(Value::as_str) {
        sentence += &format!(", worst in bars {bars}");
    }
    let others = value["clashing_pairs"]
        .as_u64()
        .map_or(pairs.len() - 1, |n| n.saturating_sub(1) as usize);
    match others {
        0 => sentence += "; no other pair clashes.",
        1 => sentence += "; one other pair clashes.",
        n => sentence += &format!("; {n} other pairs clash."),
    }
    sentence
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sine;
    use project::TempoMap;

    const RATE: u32 = 48_000;

    fn mixed(tracks: &[&[f32]]) -> Vec<f32> {
        let len = tracks.iter().map(|t| t.len()).max().unwrap_or(0);
        (0..len)
            .map(|i| {
                tracks
                    .iter()
                    .map(|t| t.get(i).copied().unwrap_or(0.0))
                    .sum()
            })
            .collect()
    }

    fn run(waves: &[(&str, &[f32])], tempo: &TempoMap) -> Value {
        let mix = mixed(&waves.iter().map(|w| w.1).collect::<Vec<_>>());
        let tracks: Vec<(String, u32, Signal)> = waves
            .iter()
            .enumerate()
            .map(|(i, (name, wave))| {
                let signal = Signal::new(wave, wave, RATE, 0.0, tempo);
                (name.to_string(), i as u32 + 1, signal)
            })
            .collect();
        clashes(&tracks, &Signal::new(&mix, &mix, RATE, 0.0, tempo), 10)
    }

    #[test]
    fn two_tracks_in_one_band_clash() {
        let tempo = TempoMap::new(120.0);
        let bass = sine(440.0, 0.3, 4.0, RATE);
        let keys = sine(470.0, 0.25, 4.0, RATE);
        let value = run(&[("Bass", &bass), ("Keys", &keys)], &tempo);
        let pairs = value["pairs"].as_array().unwrap();
        assert_eq!(pairs.len(), 1, "{value}");
        let pair = &pairs[0];
        assert_eq!(pair["tracks"][0]["name"], "Bass");
        assert_eq!(pair["tracks"][1]["id"], 2);
        assert_eq!(pair["score"], 1.0);
        assert!(pair["time_share"].as_f64().unwrap() > 0.95, "{pair}");
        assert_eq!(pair["bands"][0]["from_hz"], 400);
        assert_eq!(pair["bands"][0]["to_hz"], 510);
        assert_eq!(pair["louder"]["name"], "Bass");
        assert_eq!(pair["louder"]["share"], 1.0);
        assert!(
            value["summary"]
                .as_str()
                .unwrap()
                .starts_with("Bass and Keys clash most, 100% of the time, mostly 400–510 Hz"),
            "{value}"
        );
    }

    #[test]
    fn tracks_in_separate_bands_dont() {
        let tempo = TempoMap::new(120.0);
        let bass = sine(150.0, 0.3, 4.0, RATE);
        let hats = sine(3000.0, 0.3, 4.0, RATE);
        let value = run(&[("Bass", &bass), ("Hats", &hats)], &tempo);
        assert_eq!(value["pairs"], json!([]), "{value}");
        assert!(
            value["summary"]
                .as_str()
                .unwrap()
                .starts_with("No two of the 2 tracks clash")
        );
    }

    #[test]
    fn a_much_louder_track_masks_rather_than_clashes() {
        let tempo = TempoMap::new(120.0);
        let loud = sine(440.0, 0.5, 4.0, RATE);
        let quiet = sine(470.0, 0.125, 4.0, RATE);
        let value = run(&[("Loud", &loud), ("Quiet", &quiet)], &tempo);
        assert_eq!(value["pairs"], json!([]), "{value}");
    }

    #[test]
    fn tracks_far_under_the_mix_dont_clash() {
        let tempo = TempoMap::new(120.0);
        let lead = sine(450.0, 0.5, 4.0, RATE);
        let pad = sine(420.0, 0.003, 4.0, RATE);
        let choir = sine(490.0, 0.003, 4.0, RATE);
        let value = run(&[("Lead", &lead), ("Pad", &pad), ("Choir", &choir)], &tempo);
        assert_eq!(value["pairs"], json!([]), "{value}");
    }

    #[test]
    fn the_worst_bars_are_reported() {
        // At 120 BPM a bar lasts 2 s: the strings join for bars 5 and 6.
        let tempo = TempoMap::new(120.0);
        let piano = sine(440.0, 0.3, 16.0, RATE);
        let mut strings = vec![0.0; 8 * RATE as usize];
        strings.extend(sine(470.0, 0.2, 4.0, RATE));
        strings.resize(16 * RATE as usize, 0.0);
        let bass = sine(100.0, 0.3, 16.0, RATE);
        let value = run(
            &[("Piano", &piano), ("Strings", &strings), ("Bass", &bass)],
            &tempo,
        );
        let pairs = value["pairs"].as_array().unwrap();
        assert_eq!(pairs.len(), 1, "{value}");
        let pair = &pairs[0];
        assert_eq!(pair["worst_bars"], json!(["5–6"]), "{pair}");
        let share = pair["time_share"].as_f64().unwrap();
        assert!((share - 0.25).abs() < 0.02, "{share}");
        assert_eq!(pair["louder"]["name"], "Piano");
        assert!(
            value["summary"]
                .as_str()
                .unwrap()
                .contains("worst in bars 5–6")
        );
    }

    #[test]
    fn pairs_are_ranked_and_cut_to_the_top() {
        let tempo = TempoMap::new(120.0);
        let a = sine(440.0, 0.3, 4.0, RATE);
        let b = sine(470.0, 0.3, 4.0, RATE);
        let c = sine(1100.0, 0.05, 4.0, RATE);
        let d = sine(1150.0, 0.05, 4.0, RATE);
        let waves = [("A", &a[..]), ("B", &b[..]), ("C", &c[..]), ("D", &d[..])];
        let all = run(&waves, &tempo);
        let pairs = all["pairs"].as_array().unwrap();
        assert_eq!(pairs.len(), 2, "{all}");
        assert_eq!(pairs[0]["tracks"][0]["name"], "A");
        assert!(pairs[1]["score"].as_f64().unwrap() < 0.5, "{all}");

        let mix = mixed(&[&a, &b, &c, &d]);
        let tracks: Vec<(String, u32, Signal)> = waves
            .iter()
            .map(|(name, wave)| {
                (
                    name.to_string(),
                    0,
                    Signal::new(wave, wave, RATE, 0.0, &tempo),
                )
            })
            .collect();
        let one = clashes(&tracks, &Signal::new(&mix, &mix, RATE, 0.0, &tempo), 1);
        assert_eq!(one["pairs"].as_array().unwrap().len(), 1);
        assert_eq!(one["clashing_pairs"], 2);
        assert!(
            one["summary"]
                .as_str()
                .unwrap()
                .ends_with("one other pair clashes.")
        );
    }

    #[test]
    fn silence_and_a_lone_track_report_nothing() {
        let tempo = TempoMap::new(120.0);
        let empty: Vec<f32> = Vec::new();
        let value = run(&[("A", &empty), ("B", &empty)], &tempo);
        assert_eq!(value["pairs"], json!([]));
        assert_eq!(value["windows"], 0);
        let quiet = vec![0.0; RATE as usize];
        let value = run(&[("A", &quiet), ("B", &quiet)], &tempo);
        assert_eq!(value["pairs"], json!([]));
        let lone = sine(440.0, 0.3, 1.0, RATE);
        let value = run(&[("A", &lone)], &tempo);
        assert_eq!(value["pairs"], json!([]));
        assert_eq!(
            value["summary"],
            "Fewer than two tracks: nothing can clash."
        );
    }
}
