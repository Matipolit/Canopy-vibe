//! The average spectrum (Welch's method: Hann windows of 4096 frames, half
//! overlapping, their power averaged) of both channels summed to mono, and
//! what is read from it: ⅓-octave band levels, resonant peaks, centroid,
//! roll-off and tilt, and optionally eight bands' level bar by bar.
//!
//! Levels are in dB where a full-scale sine reads 0 (a sine of amplitude
//! 0.5 reads -6.0 however its frequency falls between bins). A band's
//! level is the power within it, so pink noise reads flat across the
//! ⅓-octave bands; the tilt is that of the power density, so white noise
//! tilts 0 dB per octave and pink noise -3.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use serde_json::{Value, json};

use crate::Signal;
use crate::report::{self, FLOOR_DB, power_db};

/// Frames per analysis window.
pub const FFT: usize = 4096;

/// A Hann window's equivalent noise bandwidth, in bins: a sine's power
/// summed over its main lobe is this many times its amplitude squared.
pub const HANN_BANDWIDTH: f32 = 1.5;

/// The range every summary looks at.
pub const LOWEST_HZ: f32 = 20.0;
pub const HIGHEST_HZ: f32 = 20_000.0;

/// A short-time Fourier transform of one size: a periodic Hann window and
/// its buffers, reused frame after frame.
pub struct Stft {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Turns |X|² into amplitude² for a sine centred on a bin.
    scale: f32,
}

impl Stft {
    pub fn new(size: usize) -> Stft {
        Stft::with_window(hann(size))
    }

    /// A transform as long as `window`, through that window.
    pub fn with_window(window: Vec<f32>) -> Stft {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(window.len());
        let sum: f32 = window.iter().sum();
        Stft {
            input: fft.make_input_vec(),
            output: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            window,
            scale: (2.0 / sum).powi(2),
            fft,
        }
    }

    pub fn size(&self) -> usize {
        self.window.len()
    }

    /// Bins from 0 Hz to Nyquist.
    pub fn bins(&self) -> usize {
        self.size() / 2 + 1
    }

    /// The window's equivalent noise bandwidth in bins ([`HANN_BANDWIDTH`]
    /// for the Hann window).
    pub fn bandwidth(&self) -> f32 {
        let sum: f64 = self.window.iter().map(|&w| f64::from(w)).sum();
        let squares: f64 = self.window.iter().map(|&w| f64::from(w * w)).sum();
        (self.size() as f64 * squares / (sum * sum)) as f32
    }

    /// The windowed power of `frame` (zero-padded if shorter) per bin, into
    /// `out` ([`Stft::bins`] long), scaled so a sine of amplitude A centred
    /// on a bin reads A² there.
    pub fn power(&mut self, frame: &[f32], out: &mut [f32]) {
        for (i, (x, w)) in self.input.iter_mut().zip(&self.window).enumerate() {
            *x = frame.get(i).map_or(0.0, |s| s * w);
        }
        // Lengths always match the plan, so this cannot fail.
        let _ = self
            .fft
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch);
        for (p, c) in out.iter_mut().zip(&self.output) {
            *p = c.norm_sqr() * self.scale;
        }
    }
}

/// A periodic Hann window: its overlapping halves sum to a constant.
pub fn hann(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| {
            let x = std::f64::consts::TAU * i as f64 / size as f64;
            (0.5 - 0.5 * x.cos()) as f32
        })
        .collect()
}

/// A periodic four-term Blackman-Harris window: side lobes 92 dB down,
/// for when a loud tone must not leak into the bands beside it.
pub fn blackman_harris(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| {
            let x = std::f64::consts::TAU * i as f64 / size as f64;
            (0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos() - 0.01168 * (3.0 * x).cos())
                as f32
        })
        .collect()
}

/// Calls `each(start, power)` for the frames of `samples`, `stft`'s size
/// long every `hop`: every frame that fits, or one zero-padded frame if the
/// samples are shorter than one. Nothing for no samples.
pub fn each_frame(
    samples: &[f32],
    stft: &mut Stft,
    hop: usize,
    mut each: impl FnMut(usize, &[f32]),
) {
    if samples.is_empty() {
        return;
    }
    let size = stft.size();
    let mut power = vec![0.0; stft.bins()];
    let mut start = 0;
    loop {
        let end = (start + size).min(samples.len());
        stft.power(&samples[start..end], &mut power);
        each(start, &power);
        start += hop.max(1);
        if start + size > samples.len() {
            break;
        }
    }
}

/// An average spectrum.
#[derive(Debug, Clone, PartialEq)]
pub struct Spectrum {
    /// Mean power per bin, 0 Hz to Nyquist, in dB (floored at
    /// [`FLOOR_DB`]).
    pub bins: Vec<f32>,
    /// The loudest each bin got in any one window, in dB.
    pub peak_hold: Vec<f32>,
    /// Mean power per bin, linear (amplitude² for a centred sine).
    pub power: Vec<f32>,
    pub rate: u32,
    pub fft: usize,
}

impl Spectrum {
    /// Hz per bin.
    pub fn bin_hz(&self) -> f32 {
        self.rate as f32 / self.fft as f32
    }

    pub fn hz(&self, bin: f32) -> f32 {
        bin * self.bin_hz()
    }

    pub fn bin(&self, hz: f32) -> f32 {
        hz / self.bin_hz()
    }

    /// The power from `low` to `high` Hz, counting the part of each bin
    /// inside, as amplitude² (a sine within the band reads its own).
    pub fn band_power(&self, low: f32, high: f32) -> f32 {
        band_power(&self.power, self.bin_hz(), low, high)
    }

    pub fn band_db(&self, low: f32, high: f32) -> f32 {
        power_db(f64::from(self.band_power(low, high))) as f32
    }
}

/// [`Spectrum::band_power`] on a power spectrum `bin_hz` apart.
pub fn band_power(power: &[f32], bin_hz: f32, low: f32, high: f32) -> f32 {
    let nyquist = (power.len().saturating_sub(1)) as f32;
    let (a, b) = ((low / bin_hz).max(0.0), (high / bin_hz).min(nyquist + 0.5));
    if b <= a {
        return 0.0;
    }
    let first = (a + 0.5).floor() as usize;
    let last = ((b + 0.5).ceil() as usize).min(power.len());
    let mut sum = 0.0;
    for (k, p) in power.iter().enumerate().take(last).skip(first) {
        let k = k as f32;
        let overlap = (b.min(k + 0.5) - a.max(k - 0.5)).max(0.0);
        sum += p * overlap;
    }
    sum / HANN_BANDWIDTH
}

/// The average spectrum of `signal`'s two channels summed to mono, from
/// windows `fft` frames long, half overlapping.
pub fn average(signal: &Signal, fft: usize) -> Spectrum {
    average_of(&signal.mono(), signal.rate, fft)
}

/// [`average`] of any one channel.
pub fn average_of(samples: &[f32], rate: u32, fft: usize) -> Spectrum {
    let mut stft = Stft::new(fft);
    let bins = stft.bins();
    let mut sum = vec![0.0f64; bins];
    let mut hold = vec![0.0f32; bins];
    let mut count = 0usize;
    each_frame(samples, &mut stft, fft / 2, |_, power| {
        for ((s, h), &p) in sum.iter_mut().zip(&mut hold).zip(power) {
            *s += f64::from(p);
            *h = h.max(p);
        }
        count += 1;
    });
    let power: Vec<f32> = sum
        .iter()
        .map(|s| (s / count.max(1) as f64) as f32)
        .collect();
    let db = |p: &f32| power_db(f64::from(*p)) as f32;
    Spectrum {
        bins: power.iter().map(db).collect(),
        peak_hold: hold.iter().map(db).collect(),
        power,
        rate,
        fft,
    }
}

/// The 31 ⅓-octave bands from 20 Hz to 20 kHz: nominal centre, and the
/// exact edges (a sixth of an octave either side of 1 kHz·2^(n/3)).
pub fn third_octave_bands() -> Vec<(f32, f32, f32)> {
    const NOMINAL: [f32; 31] = [
        20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0,
        500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0,
        6300.0, 8000.0, 10_000.0, 12_500.0, 16_000.0, 20_000.0,
    ];
    NOMINAL
        .iter()
        .enumerate()
        .map(|(i, &nominal)| {
            let centre = 1000.0 * 2f32.powf((i as f32 - 17.0) / 3.0);
            let edge = 2f32.powf(1.0 / 6.0);
            (nominal, centre / edge, centre * edge)
        })
        .collect()
}

/// Each ⅓-octave band's centre (Hz) and level (dB), 20 Hz to 20 kHz.
pub fn third_octaves(spectrum: &Spectrum) -> Vec<(f32, f32)> {
    third_octave_bands()
        .into_iter()
        .map(|(centre, low, high)| (centre, spectrum.band_db(low, high)))
        .collect()
}

/// A peak in a spectrum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    pub hz: f32,
    /// Its level, the power of its main lobe: a sine's amplitude in dB.
    pub db: f32,
    /// How far it stands above the median of the bins within a sixth of an
    /// octave either side, its own lobe left out.
    pub prominence: f32,
}

/// Peaks standing out at least this far are resonances worth naming.
const PROMINENT_DB: f32 = 6.0;
/// Peaks this far below the loudest are left out.
const PEAK_RANGE_DB: f32 = 60.0;
/// A main lobe's half width, in bins.
const LOBE: usize = 2;
/// The neighbourhood a peak is compared with reaches at least this many
/// bins past its lobe, however low the frequency.
const MIN_REACH: usize = 4;

/// The `count` strongest resonant peaks between 20 Hz and 20 kHz (those
/// standing at least 6 dB above their neighbourhood), strongest first. A
/// peak is the strongest within the sixth of an octave either side, so a
/// sine's side lobes don't count as peaks.
pub fn peaks(spectrum: &Spectrum, count: usize) -> Vec<Peak> {
    let bins = &spectrum.bins;
    let n = bins.len();
    if n < 2 * (LOBE + MIN_REACH) + 1 {
        return Vec::new();
    }
    let span = |k: usize| {
        let hz = spectrum.hz(k as f32);
        let reach = LOBE + MIN_REACH;
        let low = (spectrum.bin(hz / 2f32.powf(1.0 / 6.0)).floor() as usize).min(k - reach);
        let high = (spectrum.bin(hz * 2f32.powf(1.0 / 6.0)).ceil() as usize).max(k + reach);
        low..=high.min(n - 1)
    };
    let first = (spectrum.bin(LOWEST_HZ).ceil() as usize).max(LOBE + MIN_REACH);
    let last = (spectrum.bin(HIGHEST_HZ).floor() as usize).min(n - 1 - LOBE - MIN_REACH);

    let mut peaks: Vec<Peak> = Vec::new();
    for k in first..=last {
        let here = bins[k];
        if here <= FLOOR_DB as f32 + 1.0 {
            continue;
        }
        let around = span(k);
        // Strictly above what follows, at least what comes before: a flat
        // top counts once.
        if bins[*around.start()..k].iter().any(|&b| b > here)
            || bins[k + 1..=*around.end()].iter().any(|&b| b >= here)
        {
            continue;
        }
        let neighbours: Vec<f64> = around
            .clone()
            .filter(|&j| j.abs_diff(k) > LOBE)
            .map(|j| f64::from(bins[j]))
            .collect();
        let prominence = here - report::median(&neighbours).unwrap_or(f64::from(here)) as f32;
        if prominence < PROMINENT_DB {
            continue;
        }
        let lobe: f32 = spectrum.power[k - LOBE..=k + LOBE].iter().sum();
        let (a, c) = (bins[k - 1], bins[k + 1]);
        let curve = a - 2.0 * here + c;
        let offset = if curve < 0.0 {
            (0.5 * (a - c) / curve).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        peaks.push(Peak {
            hz: spectrum.hz(k as f32 + offset),
            db: power_db(f64::from(lobe / HANN_BANDWIDTH)) as f32,
            prominence,
        });
    }
    peaks.sort_by(|a, b| b.db.total_cmp(&a.db));
    let loudest = peaks.first().map_or(0.0, |p| p.db);
    peaks.retain(|p| p.db >= loudest - PEAK_RANGE_DB);
    peaks.truncate(count);
    peaks
}

/// The power-weighted mean frequency from 20 Hz to 20 kHz, or `None` for
/// silence.
pub fn centroid(spectrum: &Spectrum) -> Option<f32> {
    let (mut weighted, mut total) = (0.0f64, 0.0f64);
    for (k, &p) in spectrum.power.iter().enumerate() {
        let hz = spectrum.hz(k as f32);
        if (LOWEST_HZ..=HIGHEST_HZ).contains(&hz) {
            weighted += f64::from(hz) * f64::from(p);
            total += f64::from(p);
        }
    }
    (total > 1e-20).then(|| (weighted / total) as f32)
}

/// The frequency below which `share` of the power from 20 Hz to 20 kHz
/// lies, or `None` for silence.
pub fn rolloff(spectrum: &Spectrum, share: f32) -> Option<f32> {
    let in_range = |k: usize| (LOWEST_HZ..=HIGHEST_HZ).contains(&spectrum.hz(k as f32));
    let total: f64 = (0..spectrum.power.len())
        .filter(|&k| in_range(k))
        .map(|k| f64::from(spectrum.power[k]))
        .sum();
    if total <= 1e-20 {
        return None;
    }
    let mut sum = 0.0;
    for k in (0..spectrum.power.len()).filter(|&k| in_range(k)) {
        sum += f64::from(spectrum.power[k]);
        if sum >= f64::from(share) * total {
            return Some(spectrum.hz(k as f32));
        }
    }
    Some(HIGHEST_HZ)
}

/// The bands the tilt is fitted over: below 50 Hz a window this long
/// smears too much, above 16 kHz many renders have already rolled off.
const TILT_LOW_HZ: f32 = 50.0;
const TILT_HIGH_HZ: f32 = 16_000.0;

/// The slope of the power density in dB per octave (white noise 0, pink
/// noise -3), fitted by least squares over the ⅓-octave bands from 50 Hz
/// to 16 kHz that aren't silent. `None` if fewer than three are.
pub fn tilt(spectrum: &Spectrum) -> Option<f32> {
    let points: Vec<(f64, f64)> = third_octave_bands()
        .into_iter()
        .filter(|&(centre, ..)| (TILT_LOW_HZ..=TILT_HIGH_HZ).contains(&centre))
        .filter_map(|(_, low, high)| {
            let power = spectrum.band_power(low, high);
            let width = spectrum.bin(high) - spectrum.bin(low);
            let density = f64::from(power / width);
            let centre = (f64::from(low) * f64::from(high)).sqrt();
            (power_db(density) > FLOOR_DB).then(|| (centre.log2(), power_db(density)))
        })
        .collect();
    if points.len() < 3 {
        return None;
    }
    let n = points.len() as f64;
    let mean_x = points.iter().map(|p| p.0).sum::<f64>() / n;
    let mean_y = points.iter().map(|p| p.1).sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (x, y) in &points {
        sxy += (x - mean_x) * (y - mean_y);
        sxx += (x - mean_x) * (x - mean_x);
    }
    Some((sxy / sxx) as f32)
}

/// The eight bands whose level is given bar by bar: name and edges in Hz.
pub const BAR_BANDS: [(&str, f32, f32); 8] = [
    ("sub", 20.0, 60.0),
    ("bass", 60.0, 150.0),
    ("low_mid", 150.0, 400.0),
    ("mid", 400.0, 1000.0),
    ("high_mid", 1000.0, 2500.0),
    ("presence", 2500.0, 6000.0),
    ("brilliance", 6000.0, 12_000.0),
    ("air", 12_000.0, 20_000.0),
];

/// Each bar's level in the eight [`BAR_BANDS`]: the mean power of the
/// windows centred in it.
pub fn per_bar(signal: &Signal) -> Value {
    let (first, last) = signal.bars();
    let bars = (last - first + 1).max(0) as usize;
    let mut sums = vec![[0.0f64; 8]; bars];
    let mut counts = vec![0usize; bars];
    let mut stft = Stft::new(FFT);
    let bin_hz = signal.rate as f32 / FFT as f32;
    let mono = signal.mono();
    each_frame(&mono, &mut stft, FFT / 2, |start, power| {
        let centre = (start + FFT / 2).min(mono.len().saturating_sub(1));
        let bar = signal.bar_of(signal.beat_at(centre as f64)) - first;
        if let Some(sum) = usize::try_from(bar).ok().and_then(|b| sums.get_mut(b)) {
            for (s, (_, low, high)) in sum.iter_mut().zip(BAR_BANDS) {
                *s += f64::from(band_power(power, bin_hz, low, high));
            }
            counts[bar as usize] += 1;
        }
    });
    let rows: Vec<Value> = sums
        .iter()
        .zip(&counts)
        .enumerate()
        .filter(|(_, (_, count))| **count > 0)
        .map(|(i, (sum, &count))| {
            let db: Vec<f64> = sum.iter().map(|s| power_db(s / count as f64)).collect();
            json!({ "bar": first + i as i64, "db": db })
        })
        .collect();
    json!({
        "bands": BAR_BANDS.iter().map(|(name, low, high)| json!({ "name": name, "from_hz": low, "to_hz": high })).collect::<Vec<_>>(),
        "bars": rows,
    })
}

/// Peaks named in a measure.
const PEAKS: usize = 8;

/// The spectrum measure: ⅓-octave bands, peaks, centroid, roll-off, tilt.
pub fn measure(signal: &Signal) -> Value {
    measure_with(signal, false)
}

/// [`measure`], with eight bands' level per bar when `bands_per_bar`.
pub fn measure_with(signal: &Signal, bands_per_bar: bool) -> Value {
    let spectrum = average(signal, FFT);
    let bands = third_octaves(&spectrum);
    let mut value = json!({
        "bands": {
            "hz": bands.iter().map(|b| b.0).collect::<Vec<_>>(),
            "db": bands.iter().map(|b| b.1).collect::<Vec<_>>(),
        },
        "peaks": peaks(&spectrum, PEAKS).iter().map(|p| json!({
            "hz": p.hz, "db": p.db, "prominence_db": p.prominence,
        })).collect::<Vec<_>>(),
        "centroid_hz": centroid(&spectrum),
        "rolloff_hz": rolloff(&spectrum, 0.85),
        "tilt_db_per_octave": tilt(&spectrum),
    });
    if bands_per_bar {
        value["per_bar"] = per_bar(signal);
    }
    report::round(value)
}

/// One sentence: where the energy sits and the strongest resonance.
pub fn summary(value: &Value) -> String {
    if value["centroid_hz"].is_null() {
        return "Silent: no spectrum to speak of.".to_string();
    }
    let mut sentence = format!(
        "Centroid {} Hz, 85% of the power below {} Hz, tilting {} dB per octave",
        report::whole(&value["centroid_hz"]),
        report::whole(&value["rolloff_hz"]),
        report::tenth(&value["tilt_db_per_octave"]),
    );
    match value["peaks"].get(0) {
        Some(peak) => {
            sentence += &format!(
                "; strongest peak {} Hz at {} dB, {} dB above its surroundings.",
                report::whole(&peak["hz"]),
                report::tenth(&peak["db"]),
                report::tenth(&peak["prominence_db"]),
            )
        }
        None => sentence += "; no resonant peaks stand out.",
    }
    sentence
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{noise, sine};
    use project::TempoMap;

    const RATE: u32 = 48_000;

    #[test]
    fn a_sine_peaks_at_its_frequency() {
        let tempo = TempoMap::new(120.0);
        let wave = sine(1000.0, 0.707, 3.0, RATE);
        let signal = Signal::new(&wave, &wave, RATE, 0.0, &tempo);
        let spectrum = average(&signal, FFT);
        let peaks = peaks(&spectrum, 8);
        let strongest = peaks[0];
        let bin = spectrum.bin_hz();
        assert!((strongest.hz - 1000.0).abs() <= bin, "{strongest:?}");
        assert!((strongest.db + 3.0).abs() <= 0.2, "{strongest:?}");
        assert_eq!(peaks.len(), 1, "side lobes aren't peaks: {peaks:?}");

        let value = measure(&signal);
        assert_eq!(value["peaks"][0]["hz"], 1000);
        // The 1 kHz band holds the sine.
        assert_eq!(value["bands"]["hz"][17], 1000);
        let band = value["bands"]["db"][17].as_f64().unwrap();
        assert!((band + 3.0).abs() <= 0.2, "{band}");
        let centroid = value["centroid_hz"].as_f64().unwrap();
        assert!((centroid - 1000.0).abs() < 20.0, "{centroid}");
    }

    #[test]
    fn a_sine_reads_its_level_wherever_it_falls_between_bins() {
        let tempo = TempoMap::new(120.0);
        for hz in [440.0, 1000.0, 1005.86, 5333.0] {
            let wave = sine(hz, 0.5, 2.0, RATE);
            let signal = Signal::new(&wave, &wave, RATE, 0.0, &tempo);
            let peak = peaks(&average(&signal, FFT), 1)[0];
            assert!((peak.db + 6.02).abs() < 0.05, "{hz} Hz: {peak:?}");
        }
    }

    /// Paul Kellet's pink noise filter (accurate to ±0.05 dB above 9 Hz at
    /// 44.1 kHz) over white noise.
    fn pink(len: usize) -> Vec<f32> {
        let mut b = [0.0f32; 7];
        noise(len, 7)
            .into_iter()
            .map(|white| {
                b[0] = 0.99886 * b[0] + white * 0.0555179;
                b[1] = 0.99332 * b[1] + white * 0.0750759;
                b[2] = 0.96900 * b[2] + white * 0.153_852;
                b[3] = 0.86650 * b[3] + white * 0.3104856;
                b[4] = 0.55000 * b[4] + white * 0.5329522;
                b[5] = -0.7616 * b[5] - white * 0.0168980;
                let out = b[..6].iter().sum::<f32>() + b[6] + white * 0.5362;
                b[6] = white * 0.115926;
                out * 0.05
            })
            .collect()
    }

    #[test]
    fn pink_noise_tilts_minus_three_db_per_octave() {
        let tempo = TempoMap::new(120.0);
        let rate = 44_100;
        let wave = pink(rate as usize * 20);
        let signal = Signal::new(&wave, &wave, rate, 0.0, &tempo);
        let tilt = tilt(&average(&signal, FFT)).unwrap();
        assert!((tilt + 3.0).abs() <= 0.5, "{tilt}");

        let white = noise(rate as usize * 20, 3);
        let signal = Signal::new(&white, &white, rate, 0.0, &tempo);
        let tilt = super::tilt(&average(&signal, FFT)).unwrap();
        assert!(tilt.abs() <= 0.5, "{tilt}");
    }

    #[test]
    fn bars_get_their_own_band_levels() {
        let tempo = TempoMap::new(120.0);
        // Bar 1 a low sine, bar 2 a high one (2 s each at 120 BPM).
        let mut wave = sine(100.0, 0.5, 2.0, RATE);
        wave.extend(sine(8000.0, 0.5, 2.0, RATE));
        let signal = Signal::new(&wave, &wave, RATE, 0.0, &tempo);
        let value = report::round(per_bar(&signal));
        let bars = value["bars"].as_array().unwrap();
        assert_eq!(bars.len(), 2);
        let level = |bar: usize, band: usize| bars[bar]["db"][band].as_f64().unwrap();
        assert!(level(0, 1) > level(0, 6) + 40.0, "{value}");
        // Windows straddling the bar line carry a little of bar 1 over.
        assert!(level(1, 6) > level(1, 1) + 15.0, "{value}");
    }

    #[test]
    fn silence_has_no_peaks_and_says_so() {
        let tempo = TempoMap::new(120.0);
        let quiet = vec![0.0f32; RATE as usize];
        let signal = Signal::new(&quiet, &quiet, RATE, 0.0, &tempo);
        let value = crate::measure("spectrum", &signal, &crate::Options::default()).unwrap();
        assert_eq!(value["peaks"], json!([]));
        assert!(value["centroid_hz"].is_null());
        assert!(value["summary"].as_str().unwrap().starts_with("Silent"));
    }
}
