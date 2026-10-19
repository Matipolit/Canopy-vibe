//! Onset detection: where hits and notes start.
//!
//! Finds candidates with log spectral flux (how much louder each frequency
//! got than shortly before), picks peaks that stand out from the local
//! median, then moves each to the actual attack in the waveform.

use std::collections::BTreeSet;

use realfft::RealFftPlanner;

use crate::AudioBuffer;

/// Analysis window in seconds, rounded up to a power of two in frames: 1024
/// at 44.1 and 48 kHz. Short enough to separate fast hits, long enough to
/// resolve bass.
const WINDOW: f64 = 0.02;

/// Analysis frames per window, so a hop is about 5 ms.
const OVERLAP: usize = 4;

/// Flux compares each frame with the one this many hops back, and with the
/// louder of each bin's neighbours there ("SuperFlux"). Looking two hops back
/// catches attacks that take a few milliseconds; the neighbours keep vibrato
/// and slides from counting as new energy.
const LAG: usize = 2;

/// Magnitudes (1.0 for a full-scale sine) go through `ln(1 + COMPRESSION * m)`
/// before differencing, so a quiet hat after a loud kick counts about as much
/// as the kick, while bins near silence (below -60 dB) barely count at all.
const COMPRESSION: f32 = 1000.0;

/// The threshold follows the median flux over this much before and after
/// each frame, so steady noise or a busy mix raises the bar.
const MEDIAN_BEFORE: f64 = 0.1;
const MEDIAN_AFTER: f64 = 0.05;

/// How much the median counts towards the threshold.
const MEDIAN_WEIGHT: f32 = 1.5;

/// A peak must be the highest flux within this many seconds either way.
const PEAK_RADIUS: f64 = 0.015;

/// Onsets closer than this are merged, keeping the stronger.
const MIN_GAP: f64 = 0.04;

/// How far the threshold sits above the median, in mean log-flux per band,
/// at sensitivity 0 and 1 (interpolated geometrically in between). Out of
/// silence a noise burst scores about 1.3 at -20 dB and 0.26 at -40 dB, a
/// hat about half that; steady noise averages 0.06 and peaks around 0.14.
const DELTA_LEAST: f32 = 1.5;
const DELTA_MOST: f32 = 0.03;

/// Attacks are found by comparing the energy in windows this long, in
/// seconds, before and after points this far apart.
const ATTACK_BEFORE: f64 = 0.02;
const ATTACK_AFTER: f64 = 0.001;
const ATTACK_STEP: f64 = 0.00025;

/// Reported onsets sit this long before the first sample of the attack, so
/// a cut there (with a fade-in of about this length) keeps the whole hit.
const LEAD: f64 = 0.001;

pub(crate) fn detect(audio: &AudioBuffer, start: u64, end: u64, sensitivity: f32) -> Vec<u64> {
    let end = (end as usize).min(audio.frames());
    let start = start as usize;
    if start >= end {
        return Vec::new();
    }
    let rate = audio.sample_rate() as f64;
    let size = ((WINDOW * rate) as usize).next_power_of_two().max(64);
    let hop = size / OVERLAP;
    let seconds_to_hops = |seconds: f64| (seconds * rate / hop as f64).round() as usize;

    // Mono audio from far enough before `start` and after `end` that the
    // flux and its median there are the real thing, not an edge effect.
    // Before the audio is silence, so a sound that starts right away counts
    // as an onset. After it the audio is mirrored: stopping dead there would
    // click, and report an onset that isn't in the audio.
    let before = (MEDIAN_BEFORE * rate) as usize + (LAG + 1) * hop + size;
    let after = (MEDIAN_AFTER * rate) as usize + size;
    let first = start as i64 - before as i64;
    let (left, right) = (audio.left(), audio.right());
    let frames = left.len() as i64;
    let mono: Vec<f32> = (first..(end + after) as i64)
        .map(|i| {
            let i = if i >= frames { 2 * frames - 1 - i } else { i };
            usize::try_from(i).map_or(0.0, |i| 0.5 * (left[i] + right[i]))
        })
        .collect();

    let flux = spectral_flux(&mono, size, hop, rate);
    // Frame `t` is centred on mono sample `t * hop + size / 2`.
    let centre = |t: usize| t * hop + size / 2;

    let sensitivity = sensitivity.clamp(0.0, 1.0);
    let delta = DELTA_LEAST * (DELTA_MOST / DELTA_LEAST).powf(sensitivity);
    let (median_before, median_after) = (
        seconds_to_hops(MEDIAN_BEFORE),
        seconds_to_hops(MEDIAN_AFTER),
    );
    let radius = seconds_to_hops(PEAK_RADIUS).max(1);
    // Frames that reach past the end of the audio only see the mirroring.
    let last = (frames - first) as usize;
    let usable = flux.len().min(last.saturating_sub(size) / hop + 1);
    let mut window = Vec::new();
    let mut candidates = Vec::new();
    for t in 1..usable {
        let value = flux[t];
        let neighbours = &flux[t.saturating_sub(radius)..(t + radius + 1).min(flux.len())];
        // Strictly above the frame before, so a plateau counts once.
        if value <= flux[t - 1] || neighbours.iter().any(|&v| v > value) {
            continue;
        }
        window.clear();
        window.extend_from_slice(
            &flux[t.saturating_sub(median_before)..(t + median_after + 1).min(flux.len())],
        );
        let middle = window.len() / 2;
        let median = *window.select_nth_unstable_by(middle, f32::total_cmp).1;
        if value >= MEDIAN_WEIGHT * median + delta {
            candidates.push((value, centre(t)));
        }
    }

    // Strongest first, so a weak false onset can't push out a real one.
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    let gap = ((MIN_GAP * rate) as usize).max(1);
    let lead = (LEAD * rate).round() as usize;
    let to_frames = |seconds: f64| ((seconds * rate).round() as usize).max(1);
    let windows = Attack {
        before: to_frames(ATTACK_BEFORE),
        after: to_frames(ATTACK_AFTER),
        step: to_frames(ATTACK_STEP),
    };
    let mut onsets = BTreeSet::new();
    for (_, centre) in candidates {
        // Without a clear attack, the flux itself is within a few milliseconds.
        let attack = find_attack(&mono, centre, size, &windows).unwrap_or(centre - hop);
        let attack = first + attack as i64;
        let onset = attack - lead as i64;
        // An attack just after `start` is reported at `start`.
        if attack < start as i64 || onset >= end as i64 {
            continue;
        }
        let onset = onset.max(start as i64) as usize;
        if onsets
            .range(onset.saturating_sub(gap - 1)..onset + gap)
            .next()
            .is_none()
        {
            onsets.insert(onset);
        }
    }
    onsets.into_iter().map(|o| o as u64).collect()
}

/// Mean over bands of the rectified rise in log magnitude, one value per hop.
fn spectral_flux(mono: &[f32], size: usize, hop: usize, rate: f64) -> Vec<f32> {
    if mono.len() < size {
        return Vec::new();
    }
    let frames = (mono.len() - size) / hop + 1;
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(size);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let window: Vec<f32> = (0..size)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size as f32).cos())
        .collect();
    // A full-scale sine at a bin's centre has magnitude size / 4 through a Hann window.
    let scale = 4.0 / size as f32;
    let bands = Band::semitones(size, rate);

    // The last LAG frames' log spectra, max-filtered across neighbouring bands.
    let mut history = vec![vec![0.0f32; bands.len()]; LAG];
    let mut magnitude = vec![0.0f32; spectrum.len()];
    let mut log = vec![0.0f32; bands.len()];
    let mut flux = Vec::with_capacity(frames);
    for t in 0..frames {
        for ((x, s), w) in input.iter_mut().zip(&mono[t * hop..]).zip(&window) {
            *x = s * w;
        }
        fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch)
            .expect("buffer sizes match the plan");
        for (m, c) in magnitude.iter_mut().zip(&spectrum) {
            *m = scale * c.norm();
        }
        for (l, band) in log.iter_mut().zip(&bands) {
            *l = (COMPRESSION * band.apply(&magnitude)).ln_1p();
        }
        let reference = &mut history[t % LAG];
        let rise: f32 = log
            .iter()
            .zip(reference.iter())
            .map(|(l, r)| (l - r).max(0.0))
            .sum();
        flux.push(rise / bands.len().max(1) as f32);

        for (k, r) in reference.iter_mut().enumerate() {
            let neighbours = &log[k.saturating_sub(1)..(k + 2).min(log.len())];
            *r = neighbours.iter().copied().fold(0.0, f32::max);
        }
    }
    flux
}

/// Lowest and highest frequencies the flux looks at. Below are rumble and
/// offsets, above mostly air that hats and cymbals already show lower down.
const LOWEST: f64 = 30.0;
const HIGHEST: f64 = 16_000.0;

/// A triangular filter over FFT bins: one band of a log-frequency filterbank.
///
/// Summing linear bins would let the hundreds of treble bins drown out a new
/// note, whose partials touch only a few; bands a semitone wide (or one bin
/// wide in the bass, where bins are coarser) weigh every octave about the same.
struct Band {
    first: usize,
    weights: Vec<f32>,
}

impl Band {
    fn semitones(size: usize, rate: f64) -> Vec<Band> {
        let highest = HIGHEST.min(0.45 * rate);
        let mut edges: Vec<usize> = (0..)
            .map(|i| LOWEST * 2f64.powf(i as f64 / 12.0))
            .take_while(|&f| f <= highest)
            .map(|f| ((f * size as f64 / rate).round() as usize).max(1))
            .collect();
        edges.dedup();
        edges
            .windows(3)
            .map(|e| {
                let (lo, mid, hi) = (e[0], e[1], e[2]);
                let weights: Vec<f32> = (lo..=hi)
                    .map(|bin| {
                        if bin < mid {
                            (bin - lo) as f32 / (mid - lo) as f32
                        } else {
                            (hi - bin) as f32 / (hi - mid) as f32
                        }
                    })
                    .collect();
                // Unit sum, so a band reads as the average magnitude in it.
                let sum: f32 = weights.iter().sum();
                Band {
                    first: lo,
                    weights: weights.iter().map(|w| w / sum).collect(),
                }
            })
            .collect()
    }

    fn apply(&self, magnitude: &[f32]) -> f32 {
        self.weights
            .iter()
            .zip(&magnitude[self.first..])
            .map(|(w, m)| w * m)
            .sum()
    }
}

/// The first sample of the attack that the flux frame centred on `centre`
/// responded to, or `None` if nothing there clearly gets louder.
///
/// Works on the first difference, so bass doesn't mask the click of a hit.
/// The attack is where the energy over the next millisecond jumps above the
/// loudest millisecond in the 20 before. Comparing with the loudest rather
/// than the average matters for tones: their waveforms (and more so their
/// differences) can be spiky, a pulse per cycle, and every pulse would look
/// like an attack against the quiet stretch before it. Then it steps back to
/// the first sample that stands out.
fn find_attack(mono: &[f32], centre: usize, size: usize, attack: &Attack) -> Option<usize> {
    let Attack {
        before,
        after,
        step,
    } = *attack;
    // The flux peaks with the attack somewhere in its window.
    let from = centre.saturating_sub(size / 2).max(before + 1);
    let to = (centre + size / 2).min(mono.len().saturating_sub(after));
    if to <= from {
        return None;
    }
    // Running sums of the squared difference over the search area.
    let base = from - before;
    let mut energy = vec![0.0f64; to + after - base + 1];
    for i in base..to + after {
        let d = (mono[i] - mono[i - 1]) as f64;
        energy[i - base + 1] = energy[i - base] + d * d;
    }
    let mean = |a: usize, b: usize| (energy[b - base] - energy[a - base]) / (b - a) as f64;
    let loudest = mean(base, to + after);
    if loudest < 1e-12 {
        return None;
    }
    // Energy of the millisecond from each point, on a grid starting `before`
    // ahead of the search.
    let grid: Vec<f64> = (base..to)
        .step_by(step)
        .map(|p| mean(p, p + after))
        .collect();
    let lookback = before / step;
    let skip = after.div_ceil(step);
    // Relative to the overall level, so the score doesn't depend on it and
    // wobbles in near-silence before a hit don't count.
    let floor = loudest * 1e-4;
    let ratios: Vec<f64> = (lookback..grid.len())
        .map(|g| {
            let earlier = grid[g - lookback..=g - skip]
                .iter()
                .copied()
                .fold(0.0, f64::max);
            (grid[g] + floor) / (earlier + floor)
        })
        .collect();
    // At least four times the energy (+6 dB).
    let highest = ratios.iter().copied().fold(0.0, f64::max);
    if highest < 4.0 {
        return None;
    }
    // The first rise that comes close to the highest, climbed to its top.
    let mut k = ratios.iter().position(|&r| r >= 0.5 * highest)?;
    while ratios.get(k + 1).is_some_and(|&next| next > ratios[k]) {
        k += 1;
    }
    let best = base + (lookback + k) * step;
    let quiet = grid[k..=k + lookback - skip]
        .iter()
        .copied()
        .fold(0.0, f64::max)
        .sqrt() as f32;
    let loud = grid[lookback + k].sqrt() as f32;
    let threshold = (2.0 * quiet).max(0.1 * loud);
    (best.saturating_sub(step)..best + step)
        .find(|&i| (mono[i] - mono[i - 1]).abs() > threshold)
        .or(Some(best))
}

/// Window lengths for `find_attack`, in frames.
#[derive(Clone, Copy)]
struct Attack {
    before: usize,
    after: usize,
    step: usize,
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;
    use crate::test_util::{Drum, Rng, drums, sine};

    const RATE: u32 = 48_000;

    fn stereo(samples: Vec<f32>) -> AudioBuffer {
        AudioBuffer::new(RATE, samples.clone(), samples)
    }

    fn ms(ms: f64) -> i64 {
        (ms * RATE as f64 / 1000.0).round() as i64
    }

    /// How many of `expected` have an onset within `early..=late` frames of
    /// them, and how many onsets match nothing.
    fn score(found: &[u64], expected: &[usize], early: i64, late: i64) -> (usize, usize) {
        let matches = |e: usize, f: u64| (-early..=late).contains(&(f as i64 - e as i64));
        let hits = expected
            .iter()
            .filter(|&&e| found.iter().any(|&f| matches(e, f)))
            .count();
        let extra = found
            .iter()
            .filter(|&&f| !expected.iter().any(|&e| matches(e, f)))
            .count();
        (hits, extra)
    }

    /// Two bars of 16ths at 120 BPM: kicks, snares, hats and ghost snares
    /// (-20 dB) over a held bass note. Returns the audio and the hit frames.
    fn groove(seed: u64) -> (Vec<f32>, Vec<usize>) {
        let mut rng = Rng::new(seed);
        let step = RATE as usize / 8;
        let mut hits = Vec::new();
        for s in 0..32 {
            let start = ms(100.0) as usize + s * step + (rng.next_u64() % 200) as usize;
            let hit = match s % 16 {
                0 | 8 | 10 => Some((Drum::Kick, 0.8)),
                4 | 12 => Some((Drum::Snare, 0.6)),
                7 | 15 => Some((Drum::Snare, 0.08)),
                _ if s % 2 == 0 => Some((Drum::Hat, 0.3)),
                _ => None,
            };
            if let Some((drum, gain)) = hit {
                hits.push((start, drum, gain * rng.range(0.7, 1.0) as f32));
            }
        }
        let mut audio = drums(RATE, 32 * step + RATE as usize, &hits, seed);
        for (n, s) in audio.iter_mut().enumerate() {
            *s += 0.2 * (TAU * 55.0 * n as f64 / RATE as f64).sin() as f32;
        }
        (audio, hits.iter().map(|h| h.0).collect())
    }

    /// `audio` with a noise floor (-40 dB) and a wobbling pad chord under it.
    fn under_a_pad(audio: &[f32], seed: u64) -> Vec<f32> {
        let mut rng = Rng::new(seed);
        audio
            .iter()
            .enumerate()
            .map(|(n, s)| {
                let t = n as f64 / RATE as f64;
                let chord: f64 = [220.0, 261.6, 329.6]
                    .iter()
                    .map(|f| (TAU * f * t).sin())
                    .sum();
                let pad = 0.08 * chord * (1.0 + 0.3 * (TAU * 3.0 * t).sin());
                s + pad as f32 + rng.range(-0.01, 0.01) as f32
            })
            .collect()
    }

    /// Plucked notes (8 partials, 1 ms attack, long decay) at irregular
    /// times, ringing into each other.
    fn plucks(seed: u64) -> (Vec<f32>, Vec<usize>) {
        let mut rng = Rng::new(seed);
        let mut starts = Vec::new();
        let mut t = ms(100.0) as usize;
        for _ in 0..20 {
            starts.push(t);
            t += ms(120.0) as usize + (rng.next_u64() % ms(400.0) as u64) as usize;
        }
        let mut audio = vec![0.0f32; t + RATE as usize];
        for &start in &starts {
            let f0 = 110.0 * 2f64.powf((rng.next_u64() % 24) as f64 / 12.0);
            let gain = rng.range(0.1, 0.4);
            for (n, out) in audio[start..].iter_mut().enumerate() {
                let time = n as f64 / RATE as f64;
                let envelope = (time / 0.001).min(1.0) * (-time / 0.6).exp();
                let x: f64 = (1..=8)
                    .map(|h| (TAU * f0 * h as f64 * time).sin() / h as f64)
                    .sum();
                *out += (gain * envelope * x) as f32;
            }
        }
        (audio, starts)
    }

    #[test]
    fn finds_every_drum_hit_just_before_its_attack() {
        for seed in 1..4 {
            let (audio, hits) = groove(seed);
            let found = detect(&stereo(audio), ms(50.0) as u64, u64::MAX, 0.5);
            // Within a millisecond and a half before the first sample of the hit.
            assert_eq!(
                score(&found, &hits, ms(1.5), 0),
                (hits.len(), 0),
                "seed {seed}: {found:?}"
            );
        }
    }

    #[test]
    fn finds_drum_hits_under_a_pad_and_noise() {
        for seed in 1..4 {
            let (audio, hits) = groove(seed);
            let found = detect(
                &stereo(under_a_pad(&audio, seed)),
                ms(50.0) as u64,
                u64::MAX,
                0.5,
            );
            assert_eq!(
                score(&found, &hits, ms(1.5), 0),
                (hits.len(), 0),
                "seed {seed}: {found:?}"
            );
        }
    }

    #[test]
    fn finds_bursts_at_any_level() {
        let mut rng = Rng::new(5);
        let mut hits = Vec::new();
        let mut t = ms(30.0) as usize;
        for k in 0..12 {
            // -6 dB down to -36 dB.
            let gain = 0.5 / 1.4f32.powi(2 * (k % 6));
            hits.push((t, Drum::Burst, gain));
            t += ms(60.0) as usize + (rng.next_u64() % ms(300.0) as u64) as usize;
        }
        let audio = drums(RATE, t + RATE as usize, &hits, 7);
        let expected: Vec<usize> = hits.iter().map(|h| h.0).collect();
        let found = detect(&stereo(audio), 0, u64::MAX, 0.5);
        assert_eq!(
            score(&found, &expected, ms(1.5), 0),
            (expected.len(), 0),
            "{found:?}"
        );
    }

    #[test]
    fn finds_most_plucked_notes_roughly() {
        let (mut hits, mut extra, mut total) = (0, 0, 0);
        for seed in 1..4 {
            let (audio, notes) = plucks(seed);
            let found = detect(&stereo(audio), 0, u64::MAX, 0.75);
            let (h, e) = score(&found, &notes, ms(10.0), ms(10.0));
            (hits, extra, total) = (hits + h, extra + e, total + notes.len());
        }
        // Notes over ringing notes are hard; the clear ones are found.
        assert!(hits * 10 >= total * 6, "found {hits} of {total}");
        assert!(extra <= 1, "{extra} false onsets");
    }

    #[test]
    fn steady_sounds_have_no_onsets() {
        let frames = 5 * RATE as usize;
        let at = |n: usize| n as f64 / RATE as f64;
        let mut rng = Rng::new(3);
        let sounds: [(&str, Vec<f32>); 4] = [
            ("sine", sine(RATE, 440.0, frames)),
            (
                "vibrato",
                (0..frames)
                    .map(|n| {
                        (0.5 * (TAU * 440.0 * at(n) + 2.2 * (TAU * 6.0 * at(n)).sin()).sin()) as f32
                    })
                    .collect(),
            ),
            (
                "noise",
                (0..frames).map(|_| rng.range(-0.1, 0.1) as f32).collect(),
            ),
            (
                "tremolo chord",
                (0..frames)
                    .map(|n| {
                        let chord: f64 = [220.0, 277.2, 329.6]
                            .iter()
                            .map(|f| (TAU * f * at(n)).sin())
                            .sum();
                        (0.2 * chord * (1.0 + 0.4 * (TAU * 4.0 * at(n)).sin())) as f32
                    })
                    .collect(),
            ),
        ];
        for (name, audio) in sounds {
            // Skipping the start, where the sound itself begins.
            let found = detect(&stereo(audio), RATE as u64 / 2, frames as u64, 0.6);
            assert!(found.is_empty(), "{name}: {found:?}");
        }
    }

    #[test]
    fn a_steady_tone_starts_once_even_at_full_sensitivity() {
        let tone = stereo(sine(RATE, 440.0, 5 * RATE as usize));
        assert_eq!(detect(&tone, 0, u64::MAX, 1.0), vec![0]);
    }

    #[test]
    fn a_sound_at_the_very_start_is_an_onset() {
        let audio = drums(RATE, RATE as usize, &[(0, Drum::Snare, 0.5)], 1);
        assert_eq!(detect(&stereo(audio), 0, u64::MAX, 0.5), vec![0]);
    }

    #[test]
    fn sensitivity_decides_about_quiet_hits() {
        let (audio, hits) = groove(1);
        let audio = stereo(audio);
        let counts: Vec<usize> = [0.0, 0.25, 0.5]
            .iter()
            .map(|&s| detect(&audio, 0, u64::MAX, s).len())
            .collect();
        assert!(counts[0] < counts[1] && counts[1] < counts[2], "{counts:?}");
        assert!(counts[2] >= hits.len());
    }

    #[test]
    fn reports_only_the_range_asked_for() {
        let (audio, _) = groove(2);
        let audio = stereo(audio);
        let all = detect(&audio, 0, u64::MAX, 0.5);
        let (start, end) = (all[3], all[9] + 1);
        let some = detect(&audio, start, end, 0.5);
        assert_eq!(some, all[3..=9]);
        assert!(detect(&audio, end, start, 0.5).is_empty());
        assert!(detect(&stereo(Vec::new()), 0, 100, 0.5).is_empty());
    }

    /// `cargo test -p media --release -- --ignored --nocapture` to see the time.
    #[test]
    #[ignore]
    fn three_minutes_stereo_timing() {
        let frames = 180 * RATE as usize;
        let mut hits = Vec::new();
        for (k, start) in (0..frames).step_by(RATE as usize / 8).enumerate() {
            hits.push((
                start,
                [Drum::Kick, Drum::Hat, Drum::Snare, Drum::Hat][k % 4],
                0.5,
            ));
        }
        let audio = stereo(drums(RATE, frames, &hits, 1));
        let start = std::time::Instant::now();
        let found = detect(&audio, 0, u64::MAX, 0.5);
        eprintln!(
            "3 min stereo 48k: {:?}, {} onsets",
            start.elapsed(),
            found.len()
        );
        assert_eq!(found.len(), hits.len());
    }
}
