//! Analyzer: meters and scopes, MiniMeters-style. It leaves the sound alone,
//! bit for bit; the UI draws the spectrum, waveform, oscilloscope and
//! vectorscope from its tap, and the loudness and level readouts from its
//! meters. Its settings only change what the UI shows.
//!
//! Loudness follows ITU-R BS.1770-4 and EBU R128: both channels are
//! K-weighted (see `creative_loudness.rs`) and their mean squares summed.
//! That is gathered in 100 ms pieces: the last 4 make the momentary
//! loudness (400 ms), the last 30 the short-term (3 s), so both update ten
//! times a second. Every momentary block also goes to the integrated
//! loudness, gated (blocks below -70 LUFS, then those 10 LU below the mean,
//! are left out), and every short-term value to the loudness range (EBU
//! Tech 3342: 10th to 95th percentile, gated at -70 LUFS and 20 LU below the
//! mean). Integrated loudness, loudness range and the highest true peak
//! start again whenever playback starts.
//!
//! True peak is measured four times oversampled (BS.1770-4 Annex 2), so it
//! catches the peaks between samples that a converter or a lossy encoder
//! would make. Peak meters jump up at once and fall back 20 dB a second;
//! RMS levels, stereo correlation and balance are averaged over 300 ms.
//!
//! Everything in dB or LUFS reads -100 for silence (or for "not measured
//! yet"), never -infinity.

mod loudness;

use instruments::ParamDescriptor;

use self::loudness::{Biquad, BiquadState, Gate, TruePeak, k_weighting, lufs};
use crate::meter::PeakMeter;
use crate::{Category, Context, Effect, EffectDescriptor, MAX_METERS, OFF_ON, Pace, choice, float};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.analyzer",
    name: "Analyzer",
    description: "Spectrum, waveform, loudness and stereo meters",
    category: Category::Analysis,
    params: &[],
    settings: SETTINGS,
    meters: &[
        "Momentary loudness (LUFS, 400 ms)",
        "Short-term loudness (LUFS, 3 s)",
        "Integrated loudness (LUFS, since playback started)",
        "Loudness range (LU, since playback started)",
        "True peak left (dBTP)",
        "True peak right (dBTP)",
        "RMS left (dBFS, 300 ms)",
        "RMS right (dBFS, 300 ms)",
        "Stereo correlation (-1 to 1)",
        "Balance (-1 left to 1 right)",
        "Sample peak left (dBFS)",
        "Sample peak right (dBFS)",
        "Highest true peak (dBTP, since playback started)",
    ],
    tap: true,
    create: || Box::new(Analyzer::new()),
    ..EffectDescriptor::BASE
};

/// What the UI shows and how; never sent to the audio thread.
const SETTINGS: &[ParamDescriptor] = &[
    choice("view_spectrum", "Spectrum", OFF_ON, 1),
    choice("view_waveform", "Waveform", OFF_ON, 1),
    choice("view_scope", "Oscilloscope", OFF_ON, 0),
    choice("view_stereo", "Stereo", OFF_ON, 1),
    choice("view_loudness", "Loudness", OFF_ON, 1),
    choice("view_levels", "Levels", OFF_ON, 1),
    choice(
        "spectrum_size",
        "Resolution",
        &["2048", "4096", "8192", "16384"],
        2,
    ),
    float("spectrum_slope", "Slope", 0.0, 6.0, 4.5, "dB/oct", false),
    float("spectrum_floor", "Floor", -120.0, -30.0, -84.0, "dB", false),
    float("spectrum_fall", "Fall", 5.0, 120.0, 40.0, "dB/s", false),
    choice("spectrum_hold", "Peak hold", OFF_ON, 0),
    float("spectrum_low", "Lowest", 10.0, 200.0, 20.0, "Hz", true),
    float(
        "spectrum_high",
        "Highest",
        2000.0,
        24_000.0,
        20_000.0,
        "Hz",
        true,
    ),
    choice(
        "spectrum_channel",
        "Channel",
        &["Mid", "Side", "Left", "Right"],
        0,
    ),
    float(
        "waveform_seconds",
        "Waveform length",
        1.0,
        30.0,
        5.0,
        "s",
        true,
    ),
    choice("waveform_colors", "Waveform colors", &["Bands", "Plain"], 0),
    float("scope_ms", "Scope window", 2.0, 200.0, 20.0, "ms", true),
    float(
        "loudness_target",
        "Loudness target",
        -24.0,
        -6.0,
        -14.0,
        "LUFS",
        false,
    ),
    choice("colors", "Colors", &["Default", "Warm", "Cool", "Mono"], 0),
];

/// What dB and LUFS readouts show for silence.
pub const SILENCE_DB: f32 = -100.0;

/// Meter slots, in [`DESCRIPTOR`]'s order.
pub const MOMENTARY: usize = 0;
pub const SHORT_TERM: usize = 1;
pub const INTEGRATED: usize = 2;
pub const RANGE: usize = 3;
pub const TRUE_PEAK_L: usize = 4;
pub const TRUE_PEAK_R: usize = 5;
pub const RMS_L: usize = 6;
pub const RMS_R: usize = 7;
pub const CORRELATION: usize = 8;
pub const BALANCE: usize = 9;
pub const PEAK_L: usize = 10;
pub const PEAK_R: usize = 11;
pub const MAX_TRUE_PEAK: usize = 12;
const READOUTS: usize = 13;

/// Loudness is gathered in pieces this long...
const PIECE_SECONDS: f64 = 0.1;
/// ...the momentary loudness is this many of them...
const MOMENTARY_PIECES: usize = 4;
/// ...and the short-term loudness this many.
const SHORT_TERM_PIECES: usize = 30;
/// Averaging time of RMS, correlation and balance.
const AVERAGE_SECONDS: f64 = 0.3;

/// `20·log10(amplitude)`, or [`SILENCE_DB`] if lower.
fn amplitude_db(amplitude: f32) -> f32 {
    (20.0 * amplitude.max(1e-12).log10()).max(SILENCE_DB)
}

/// `10·log10(power)`, or [`SILENCE_DB`] if lower.
fn power_db(power: f64) -> f32 {
    ((10.0 * power.max(1e-30).log10()) as f32).max(SILENCE_DB)
}

fn loudness_readout(power: f64) -> f32 {
    (lufs(power) as f32).max(SILENCE_DB)
}

pub struct Analyzer {
    sample_rate: f32,
    k_weighting: [Biquad; 2],
    /// Per channel, the two K-weighting stages.
    k_states: [[BiquadState; 2]; 2],
    /// Samples per loudness piece, and how far the current one has got.
    piece_len: usize,
    piece_done: usize,
    /// The K-weighted power summed over the current piece.
    piece_sum: f64,
    /// The last `SHORT_TERM_PIECES` pieces' sums, oldest overwritten.
    pieces: [f64; SHORT_TERM_PIECES],
    next_piece: usize,
    /// Pieces completed since playback started (or `reset`).
    pieces_since_start: usize,
    momentary: f64,
    short_term: f64,
    integrated_gate: Gate,
    range_gate: Gate,
    integrated: Option<f64>,
    range: Option<f64>,
    true_peak: TruePeak,
    /// Peak meters, falling back between peaks.
    true_peaks: [PeakMeter; 2],
    sample_peaks: [PeakMeter; 2],
    highest_true_peak: f32,
    /// Samples until the true-peak interpolator has forgotten what played
    /// before playback started; until then the highest true peak counts
    /// the samples alone.
    settling: usize,
    /// Running means of left², right² and left·right.
    squares: [f64; 3],
    average: f64,
    was_playing: bool,
    readouts: [f32; READOUTS],
}

impl Analyzer {
    pub fn new() -> Analyzer {
        let mut analyzer = Analyzer {
            sample_rate: 48_000.0,
            k_weighting: k_weighting(48_000.0),
            k_states: [[BiquadState::default(); 2]; 2],
            piece_len: 4800,
            piece_done: 0,
            piece_sum: 0.0,
            pieces: [0.0; SHORT_TERM_PIECES],
            next_piece: 0,
            pieces_since_start: 0,
            momentary: 0.0,
            short_term: 0.0,
            integrated_gate: Gate::new(),
            range_gate: Gate::new(),
            integrated: None,
            range: None,
            true_peak: TruePeak::new(),
            true_peaks: Default::default(),
            sample_peaks: Default::default(),
            highest_true_peak: 0.0,
            settling: 0,
            squares: [0.0; 3],
            average: 1.0,
            was_playing: false,
            readouts: [0.0; READOUTS],
        };
        analyzer.prepare(48_000.0, 0);
        analyzer
    }

    /// Integrated loudness, loudness range and the highest true peak start
    /// again, from the next piece on.
    fn restart_measurement(&mut self) {
        self.integrated_gate.clear();
        self.range_gate.clear();
        self.integrated = None;
        self.range = None;
        self.highest_true_peak = 0.0;
        self.settling = loudness::TAPS;
        self.pieces_since_start = 0;
        self.piece_sum = 0.0;
        self.piece_done = 0;
    }

    /// The mean K-weighted power over the last `count` pieces.
    fn mean_power(&self, count: usize) -> f64 {
        let sum: f64 = (1..=count)
            .map(|back| {
                self.pieces[(self.next_piece + SHORT_TERM_PIECES - back) % SHORT_TERM_PIECES]
            })
            .sum();
        sum / (count * self.piece_len) as f64
    }

    fn finish_piece(&mut self) {
        self.pieces[self.next_piece] = self.piece_sum;
        self.next_piece = (self.next_piece + 1) % SHORT_TERM_PIECES;
        self.piece_sum = 0.0;
        self.piece_done = 0;
        self.pieces_since_start += 1;
        self.momentary = self.mean_power(MOMENTARY_PIECES);
        self.short_term = self.mean_power(SHORT_TERM_PIECES);
        if self.pieces_since_start >= MOMENTARY_PIECES {
            self.integrated_gate.add(self.momentary);
            self.integrated = self.integrated_gate.integrated();
        }
        if self.pieces_since_start >= SHORT_TERM_PIECES {
            self.range_gate.add(self.short_term);
            self.range = self.range_gate.range();
        }
    }

    fn update_readouts(&mut self) {
        let r = &mut self.readouts;
        r[MOMENTARY] = loudness_readout(self.momentary);
        r[SHORT_TERM] = loudness_readout(self.short_term);
        r[INTEGRATED] = self
            .integrated
            .map_or(SILENCE_DB, |l| (l as f32).max(SILENCE_DB));
        r[RANGE] = self.range.map_or(0.0, |lu| lu as f32);
        r[TRUE_PEAK_L] = amplitude_db(self.true_peaks[0].level());
        r[TRUE_PEAK_R] = amplitude_db(self.true_peaks[1].level());
        let [left, right, product] = self.squares;
        r[RMS_L] = power_db(left);
        r[RMS_R] = power_db(right);
        // Below -100 dB there's nothing to compare.
        let quiet = 1e-10;
        r[CORRELATION] = if left > quiet && right > quiet {
            (product / (left * right).sqrt()).clamp(-1.0, 1.0) as f32
        } else {
            0.0
        };
        r[BALANCE] = if left + right > quiet {
            ((right - left) / (right + left)) as f32
        } else {
            0.0
        };
        r[PEAK_L] = amplitude_db(self.sample_peaks[0].level());
        r[PEAK_R] = amplitude_db(self.sample_peaks[1].level());
        r[MAX_TRUE_PEAK] = amplitude_db(self.highest_true_peak);
    }
}

impl Default for Analyzer {
    fn default() -> Self {
        Analyzer::new()
    }
}

impl Effect for Analyzer {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        let rate = f64::from(sample_rate);
        self.k_weighting = k_weighting(rate);
        self.piece_len = (PIECE_SECONDS * rate).round().max(1.0) as usize;
        self.average = 1.0 - (-1.0 / (AVERAGE_SECONDS * rate)).exp();
        for meter in self.true_peaks.iter_mut().chain(&mut self.sample_peaks) {
            meter.prepare(sample_rate);
        }
        self.reset();
    }

    fn reset(&mut self) {
        self.k_states = [[BiquadState::default(); 2]; 2];
        self.pieces = [0.0; SHORT_TERM_PIECES];
        self.next_piece = 0;
        self.momentary = 0.0;
        self.short_term = 0.0;
        self.restart_measurement();
        self.true_peak.clear();
        for meter in self.true_peaks.iter_mut().chain(&mut self.sample_peaks) {
            meter.reset();
        }
        self.squares = [0.0; 3];
        self.was_playing = false;
        self.update_readouts();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: &Context) {
        if context.playing && !self.was_playing {
            self.restart_measurement();
        }
        self.was_playing = context.playing;

        let len = left.len().min(right.len());
        let [shelf, high_pass] = self.k_weighting;
        let mut true_peaks = [0.0f32; 2];
        let mut sample_peaks = [0.0f32; 2];
        for (&l, &r) in left[..len].iter().zip(&right[..len]) {
            // A non-finite sample would stick in the filters forever.
            let clean = |x: f32| if x.is_finite() { x } else { 0.0 };
            let (l, r) = (clean(l), clean(r));
            let mut weighted = 0.0;
            for ([first, second], x) in self.k_states.iter_mut().zip([l, r]) {
                let y = second.tick(&high_pass, first.tick(&shelf, f64::from(x)));
                weighted += y * y;
            }
            self.piece_sum += weighted;
            self.piece_done += 1;
            if self.piece_done == self.piece_len {
                self.finish_piece();
            }

            let (l64, r64) = (f64::from(l), f64::from(r));
            for (mean, value) in self
                .squares
                .iter_mut()
                .zip([l64 * l64, r64 * r64, l64 * r64])
            {
                *mean += self.average * (value - *mean);
            }

            let peaks = self.true_peak.push(l, r);
            for channel in 0..2 {
                true_peaks[channel] = true_peaks[channel].max(peaks[channel]);
            }
            let highest = if self.settling > 0 {
                // Only the samples themselves until then.
                self.settling -= 1;
                l.abs().max(r.abs())
            } else {
                peaks[0].max(peaks[1])
            };
            self.highest_true_peak = self.highest_true_peak.max(highest);
            sample_peaks[0] = sample_peaks[0].max(l.abs());
            sample_peaks[1] = sample_peaks[1].max(r.abs());
        }

        for channel in 0..2 {
            self.true_peaks[channel].update(true_peaks[channel], len);
            self.sample_peaks[channel].update(sample_peaks[channel], len);
        }
        for state in self.k_states.iter_mut().flatten() {
            state.flush();
        }
        for mean in &mut self.squares {
            if mean.abs() < 1e-30 {
                *mean = 0.0;
            }
        }
        self.true_peak.flush();
        self.update_readouts();
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        &[]
    }

    fn receive(&mut self, _index: usize, _value: f32, _pace: Pace) {}

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out[..READOUTS].copy_from_slice(&self.readouts);
    }

    /// The momentary loudness as a power ratio (`10^(LUFS / 10)`) rather
    /// than in LUFS, so that silence reads 0 as the [`Effect::meter`]
    /// contract expects; the UI reads [`Effect::meters`].
    fn meter(&self) -> f32 {
        let power = 10f32.powf(self.readouts[MOMENTARY] / 10.0);
        if self.readouts[MOMENTARY] <= SILENCE_DB {
            0.0
        } else {
            power
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, noise, sine};

    fn analyzer() -> Analyzer {
        let mut analyzer = Analyzer::new();
        analyzer.prepare(RATE, 512);
        analyzer
    }

    /// A 1 kHz sine whose peak is `dbfs`, for `seconds`.
    fn tone(dbfs: f32, seconds: f64) -> Vec<f32> {
        sine(
            1000.0,
            dsp::db_to_gain(dbfs),
            (seconds * f64::from(RATE)) as usize,
            RATE,
        )
    }

    /// Feeds `left` and `right` through in 512-sample blocks; returns the
    /// meters after the last one, and checks nothing was changed.
    fn measure(
        analyzer: &mut Analyzer,
        left: &[f32],
        right: &[f32],
        playing: bool,
    ) -> [f32; MAX_METERS] {
        let (mut l, mut r) = (left.to_vec(), right.to_vec());
        let context = Context {
            playing,
            ..Context::DEFAULT
        };
        for (a, b) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            analyzer.process(a, b, &context);
        }
        assert!(l == left && r == right, "the analyzer changed the sound");
        let mut meters = [0.0; MAX_METERS];
        analyzer.meters(&mut meters);
        meters
    }

    fn stereo(segments: &[(f32, f64)]) -> Vec<f32> {
        segments
            .iter()
            .flat_map(|&(dbfs, seconds)| tone(dbfs, seconds))
            .collect()
    }

    #[test]
    fn a_sine_reads_its_level_in_lufs() {
        // BS.1770: a 0 dBFS 1 kHz sine in one channel reads -3.01 LUFS, so
        // in both it reads its own peak level.
        for dbfs in [-18.0, -23.0, -33.0] {
            let signal = tone(dbfs, 5.0);
            let meters = measure(&mut analyzer(), &signal, &signal, true);
            for slot in [MOMENTARY, SHORT_TERM, INTEGRATED] {
                assert!(
                    (meters[slot] - dbfs).abs() < 0.1,
                    "{dbfs} dBFS: slot {slot} reads {}",
                    meters[slot]
                );
            }
            let silent = vec![0.0; signal.len()];
            let one = measure(&mut analyzer(), &signal, &silent, true);
            assert!(
                (one[INTEGRATED] - (dbfs - 3.01)).abs() < 0.1,
                "{}",
                one[INTEGRATED]
            );
        }
    }

    #[test]
    fn integrated_loudness_passes_ebu_tech_3341() {
        // Tech 3341's cases 1-5: stereo 1 kHz sines, expected -23 ± 0.1 LUFS
        // (case 2: -33).
        let cases: [(&[(f32, f64)], f32); 5] = [
            (&[(-23.0, 20.0)], -23.0),
            (&[(-33.0, 20.0)], -33.0),
            (&[(-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0)], -23.0),
            (
                &[
                    (-72.0, 10.0),
                    (-36.0, 10.0),
                    (-23.0, 60.0),
                    (-36.0, 10.0),
                    (-72.0, 10.0),
                ],
                -23.0,
            ),
            (&[(-26.0, 20.0), (-20.0, 20.1), (-26.0, 20.0)], -23.0),
        ];
        for (number, (segments, expected)) in cases.iter().enumerate() {
            let signal = stereo(segments);
            let meters = measure(&mut analyzer(), &signal, &signal, true);
            println!(
                "Tech 3341 case {}: {:.2} LUFS (expected {expected})",
                number + 1,
                meters[INTEGRATED]
            );
            assert!(
                (meters[INTEGRATED] - expected).abs() <= 0.1,
                "case {}: {}",
                number + 1,
                meters[INTEGRATED]
            );
        }
    }

    #[test]
    fn loudness_range_passes_ebu_tech_3342() {
        // Tech 3342's cases 1-4: expected LRA ± 1 LU.
        let cases: [(&[(f32, f64)], f32); 4] = [
            (&[(-20.0, 20.0), (-30.0, 20.0)], 10.0),
            (&[(-20.0, 20.0), (-15.0, 20.0)], 5.0),
            (&[(-40.0, 20.0), (-20.0, 20.0)], 20.0),
            (
                &[
                    (-50.0, 20.0),
                    (-35.0, 20.0),
                    (-20.0, 20.0),
                    (-35.0, 20.0),
                    (-50.0, 20.0),
                ],
                15.0,
            ),
        ];
        for (number, (segments, expected)) in cases.iter().enumerate() {
            let signal = stereo(segments);
            let meters = measure(&mut analyzer(), &signal, &signal, true);
            println!(
                "Tech 3342 case {}: LRA {:.2} LU (expected {expected})",
                number + 1,
                meters[RANGE]
            );
            assert!(
                (meters[RANGE] - expected).abs() <= 1.0,
                "case {}: {}",
                number + 1,
                meters[RANGE]
            );
        }
    }

    #[test]
    fn true_peak_finds_the_peaks_between_samples() {
        // A quarter of the sample rate at 45°: every sample lands at ±0.707
        // while the wave itself reaches ±1.
        let len = 24_000;
        let signal: Vec<f32> = (0..len)
            .map(|n| {
                (std::f64::consts::FRAC_PI_2 * n as f64 + std::f64::consts::FRAC_PI_4).sin() as f32
            })
            .collect();
        let meters = measure(&mut analyzer(), &signal, &signal, false);
        println!(
            "fs/4 at 45°: sample peak {:.2} dBFS, true peak {:.2} dBTP",
            meters[PEAK_L], meters[TRUE_PEAK_L]
        );
        assert!((meters[PEAK_L] + 3.01).abs() < 0.01);
        assert!(meters[TRUE_PEAK_L].abs() < 0.1, "{}", meters[TRUE_PEAK_L]);
        assert!(meters[MAX_TRUE_PEAK].abs() < 0.1);

        // And a plain sine reads its own level, near the top of the band too.
        for freq in [997.0, 5000.0, 15_000.0, 19_000.0] {
            let signal = sine(freq, 0.5, len, RATE);
            let meters = measure(&mut analyzer(), &signal, &signal, false);
            assert!(
                (meters[TRUE_PEAK_R] + 6.02).abs() < 0.2,
                "{freq} Hz: {}",
                meters[TRUE_PEAK_R]
            );
        }
    }

    #[test]
    fn levels_correlation_and_balance() {
        let signal = tone(-6.0, 2.0);
        let inverted: Vec<f32> = signal.iter().map(|s| -s).collect();
        let half: Vec<f32> = signal.iter().map(|s| 0.5 * s).collect();
        let uncorrelated = noise(signal.len(), 77);
        let other = noise(signal.len(), 78);

        let same = measure(&mut analyzer(), &signal, &signal, false);
        assert!((same[RMS_L] - (-6.0 - 3.01)).abs() < 0.1, "{}", same[RMS_L]);
        assert!((same[CORRELATION] - 1.0).abs() < 1e-3);
        assert!(same[BALANCE].abs() < 1e-3);

        let opposite = measure(&mut analyzer(), &signal, &inverted, false);
        assert!((opposite[CORRELATION] + 1.0).abs() < 1e-3);

        let left_heavy = measure(&mut analyzer(), &signal, &half, false);
        // Right is 6 dB down: (0.25 - 1) / (0.25 + 1).
        assert!(
            (left_heavy[BALANCE] + 0.6).abs() < 0.01,
            "{}",
            left_heavy[BALANCE]
        );

        let wide = measure(&mut analyzer(), &uncorrelated, &other, false);
        assert!(wide[CORRELATION].abs() < 0.1, "{}", wide[CORRELATION]);

        let silent = vec![0.0; 4800];
        let mut quiet = analyzer();
        let meters = measure(&mut quiet, &silent, &silent, false);
        for slot in [
            MOMENTARY,
            SHORT_TERM,
            INTEGRATED,
            TRUE_PEAK_L,
            RMS_R,
            PEAK_L,
            MAX_TRUE_PEAK,
        ] {
            assert_eq!(meters[slot], SILENCE_DB, "slot {slot}");
        }
        assert_eq!(meters[CORRELATION], 0.0);
        assert_eq!(quiet.meter(), 0.0);
    }

    #[test]
    fn starting_playback_starts_the_integrated_measurement_again() {
        let mut analyzer = analyzer();
        let loud = tone(-10.0, 10.0);
        measure(&mut analyzer, &loud, &loud, false);
        let meters = measure(&mut analyzer, &loud, &loud, true);
        assert!((meters[INTEGRATED] + 10.0).abs() < 0.1);
        // Stop, then play something quieter: only that counts.
        let quiet = tone(-30.0, 10.0);
        measure(&mut analyzer, &loud, &loud, false);
        let meters = measure(&mut analyzer, &quiet, &quiet, true);
        assert!(
            (meters[INTEGRATED] + 30.0).abs() < 0.1,
            "{}",
            meters[INTEGRATED]
        );
        assert!(
            (meters[MAX_TRUE_PEAK] + 30.0).abs() < 0.1,
            "{}",
            meters[MAX_TRUE_PEAK]
        );
        // While still playing, it keeps integrating: now the quiet part is
        // more than 10 LU under the mean, so the relative gate drops it.
        let meters = measure(&mut analyzer, &loud, &loud, true);
        assert!(
            (meters[INTEGRATED] + 10.0).abs() < 0.2,
            "{}",
            meters[INTEGRATED]
        );
    }

    #[test]
    fn settings_and_meters_are_as_the_ui_expects() {
        let ids: Vec<&str> = SETTINGS.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            [
                "view_spectrum",
                "view_waveform",
                "view_scope",
                "view_stereo",
                "view_loudness",
                "view_levels",
                "spectrum_size",
                "spectrum_slope",
                "spectrum_floor",
                "spectrum_fall",
                "spectrum_hold",
                "spectrum_low",
                "spectrum_high",
                "spectrum_channel",
                "waveform_seconds",
                "waveform_colors",
                "scope_ms",
                "loudness_target",
                "colors",
            ]
        );
        assert_eq!(DESCRIPTOR.meters.len(), READOUTS);
        assert!(DESCRIPTOR.params.is_empty() && DESCRIPTOR.tap);
    }
}
