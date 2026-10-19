//! Offline processing of a clip's audio: reversing, looping, stretching and
//! pitch shifting, and finding transients. Runs on worker threads, never on
//! the audio thread.

use crate::{AudioBuffer, beats, resample, stretch, transients};

/// How a clip changes speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StretchMode {
    /// Varispeed, like FL Studio's Resample: faster is higher pitched.
    #[default]
    Resample,
    /// Changes speed and pitch independently.
    Stretch,
    /// Cuts the audio at transients and moves the slices, each played at its
    /// original speed. For drums.
    Beats,
}

/// Everything `process_clip` needs to render one clip. Frame counts are in
/// the input buffer's frames.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipProcess {
    /// The region of the input the clip plays.
    pub offset: u64,
    pub len: u64,
    /// Play the region backwards.
    pub reversed: bool,
    /// Repeat the region to fill `play_len`. Playback starts `phase` frames
    /// into the (possibly reversed) region.
    pub looped: bool,
    pub phase: u64,
    /// Frames of material played, before stretching. `len` unless looped.
    pub play_len: u64,
    pub mode: StretchMode,
    /// 2.0 plays twice as fast, so half as long. Resample also raises the
    /// pitch by an octave; Stretch and Beats keep it.
    pub speed: f64,
    /// Semitones, fractional for cents. Stretch mode only.
    pub transpose: f64,
    /// The exact length to return, in frames: the clip's length on the
    /// timeline. The result is cut or padded with silence to fit.
    pub out_len: u64,
}

/// Speeds outside this range are clamped. Far beyond anything musical, but it
/// bounds the work: the Resample kernel grows with the speed.
const MIN_SPEED: f64 = 1.0 / 16.0;
const MAX_SPEED: f64 = 16.0;

/// Transposition is clamped to four octaves either way.
const MAX_TRANSPOSE: f64 = 48.0;

/// Renders a clip's audio at the input's sample rate. May take seconds for a
/// long Stretch render.
///
/// The region, reversed and looped as asked, is the clip's material; frame
/// `n` of the result plays material position `n * speed`. After the material
/// runs out (`play_len / speed`) it is silent. Speed 1 (with no transpose)
/// copies the material exactly in every mode.
///
/// Resample and Stretch see past the ends of the material, to the audio that
/// surrounds it in the input (or the previous and next repeat of a loop), the
/// way they would if the whole file were stretched and then trimmed. Without
/// that the filters would fade in and out at the edges, and a stretched clip
/// split in two would dip where the halves meet.
pub fn process_clip(input: &AudioBuffer, spec: &ClipProcess) -> AudioBuffer {
    let material = Material::new(input, spec);
    let speed = if spec.speed.is_finite() && spec.speed > 0.0 {
        spec.speed.clamp(MIN_SPEED, MAX_SPEED)
    } else {
        1.0
    };
    let transpose = match spec.mode {
        StretchMode::Stretch if spec.transpose.is_finite() => {
            spec.transpose.clamp(-MAX_TRANSPOSE, MAX_TRANSPOSE)
        }
        _ => 0.0,
    };
    let out_len = spec.out_len as usize;
    // Frames that play any material; the rest is silence.
    let frames = out_len.min((spec.play_len as f64 / speed).ceil() as usize);
    let read = |start, len| material.read(start, len);

    let (mut left, mut right) = if speed == 1.0 && transpose == 0.0 {
        material.read(0, frames)
    } else {
        match spec.mode {
            StretchMode::Resample => resample::varispeed(speed, frames, read),
            StretchMode::Stretch if transpose == 0.0 => {
                stretch::stretch(input.sample_rate(), speed, 0, frames, read)
            }
            StretchMode::Stretch => {
                // Stretch to `speed / ratio` at the original pitch, then play
                // that `ratio` times faster: back to `speed`, pitch moved.
                // Resampling keeps the pitch exact.
                let ratio = 2f64.powf(transpose / 12.0);
                resample::varispeed(ratio, frames, |start, len| {
                    stretch::stretch(input.sample_rate(), speed / ratio, start, len, read)
                })
            }
            StretchMode::Beats => {
                // Only as much material as gets played.
                let len = spec.play_len.min((frames as f64 * speed).ceil() as u64 + 1);
                let cuts = beat_cuts(&material, len as i64, input.sample_rate());
                let (left, right) = material.read(0, len as usize);
                beats::rearrange(input.sample_rate(), &left, &right, &cuts, speed, frames)
            }
        }
    };
    left.resize(out_len, 0.0);
    right.resize(out_len, 0.0);
    AudioBuffer::new(input.sample_rate(), left, right)
}

/// Transients (onsets) in `start..end`, as frame positions, sorted.
/// `sensitivity` is `0..=1`: higher finds more, quieter ones.
///
/// Each position is just before the attack it marks (within a millisecond or
/// so), so cutting there keeps the whole hit. Onsets are at least 40 ms apart.
pub fn detect_transients(audio: &AudioBuffer, start: u64, end: u64, sensitivity: f32) -> Vec<u64> {
    transients::detect(audio, start, end, sensitivity)
}

/// The highest absolute sample in `start..start + len`, over both channels.
pub fn peak_level(audio: &AudioBuffer, start: u64, len: u64) -> f32 {
    let start = (start as usize).min(audio.frames());
    let end = (start + len as usize).min(audio.frames());
    audio.left()[start..end]
        .iter()
        .chain(&audio.right()[start..end])
        .fold(0.0, |m, s| m.max(s.abs()))
}

/// How eagerly Beats mode cuts. A little above the middle: a missed hit gets
/// dragged along with the one before it, which is worse than an extra cut
/// in the middle of a sound.
const BEATS_SENSITIVITY: f32 = 0.6;

/// Where Beats mode cuts the first `play_len` frames of the material: 0 and
/// every transient, sorted.
///
/// A loop is analysed once around its cycle and the cuts repeated, so every
/// repeat is sliced the same way, including at the seam.
fn beat_cuts(material: &Material, play_len: i64, rate: u32) -> Vec<usize> {
    // Audio the detector looks at before and after the range it reports on.
    let context = rate as i64 / 4;
    let detect = |start: i64, len: i64| -> Vec<i64> {
        let (left, right) = material.read(start - context, (len + 2 * context) as usize);
        let audio = AudioBuffer::new(rate, left, right);
        let found = transients::detect(
            &audio,
            context as u64,
            (context + len) as u64,
            BEATS_SENSITIVITY,
        );
        found
            .into_iter()
            .map(|t| t as i64 - context + start)
            .collect()
    };

    let mut cuts = vec![0];
    if material.looped && material.len > 0 {
        let len = material.len as i64;
        // Material position -phase is the start of the region.
        let start = -(material.phase as i64 % len);
        let cycle = detect(start, len);
        let mut repeat = 0;
        while start + repeat < play_len {
            cuts.extend(
                cycle
                    .iter()
                    .map(|t| t + repeat)
                    .filter(|&t| t > 0 && t < play_len),
            );
            repeat += len;
        }
        cuts.sort_unstable();
        cuts.dedup();
    } else {
        cuts.extend(
            detect(0, play_len)
                .into_iter()
                .filter(|&t| t > 0 && t < play_len),
        );
    }
    cuts.into_iter().map(|t| t as usize).collect()
}

/// A clip's material: its region of the input, reversed and looped as asked,
/// readable at any position. Positions in `0..play_len` are what the clip
/// plays; positions outside are the audio around it, for filters to look at.
struct Material<'a> {
    input: &'a AudioBuffer,
    offset: u64,
    len: u64,
    reversed: bool,
    looped: bool,
    phase: u64,
    play_len: u64,
}

impl<'a> Material<'a> {
    fn new(input: &'a AudioBuffer, spec: &ClipProcess) -> Self {
        Self {
            input,
            offset: spec.offset,
            len: spec.len,
            reversed: spec.reversed,
            looped: spec.looped,
            phase: spec.phase,
            play_len: spec.play_len,
        }
    }

    /// The input frame heard at material position `position`, or `None` for
    /// silence.
    ///
    /// A loop repeats forever both ways. Otherwise the region continues into
    /// the input around it, in the direction it plays, except that a region
    /// padded with silence to a longer `play_len` stays silent after its end.
    fn frame(&self, position: i64) -> Option<usize> {
        let len = self.len as i64;
        let in_region = if self.looped {
            if len == 0 {
                return None;
            }
            (self.phase as i64 % len + position).rem_euclid(len)
        } else if position >= len && self.play_len > self.len {
            return None;
        } else {
            position
        };
        let frame = if self.reversed {
            self.offset as i64 + len - 1 - in_region
        } else {
            self.offset as i64 + in_region
        };
        (0..self.input.frames() as i64)
            .contains(&frame)
            .then_some(frame as usize)
    }

    /// `len` frames of both channels from material position `start`.
    fn read(&self, start: i64, len: usize) -> (Vec<f32>, Vec<f32>) {
        let (input_left, input_right) = (self.input.left(), self.input.right());
        let mut left = Vec::with_capacity(len);
        let mut right = Vec::with_capacity(len);
        for position in start..start + len as i64 {
            let (l, r) = match self.frame(position) {
                Some(frame) => (input_left[frame], input_right[frame]),
                None => (0.0, 0.0),
            };
            left.push(l);
            right.push(r);
        }
        (left, right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{Drum, Rng, drums, largest_step, measure_frequency, peak, sine};

    const RATE: u32 = 48_000;

    /// Left is the frame number plus one, right its negative, so every sample
    /// says where it came from and none looks like silence.
    fn ramp(frames: usize) -> AudioBuffer {
        let left: Vec<f32> = (0..frames).map(|i| i as f32 + 1.0).collect();
        let right = left.iter().map(|s| -s).collect();
        AudioBuffer::new(RATE, left, right)
    }

    fn mono(samples: Vec<f32>) -> AudioBuffer {
        AudioBuffer::new(RATE, samples.clone(), samples)
    }

    fn spec(offset: u64, len: u64) -> ClipProcess {
        ClipProcess {
            offset,
            len,
            reversed: false,
            looped: false,
            phase: 0,
            play_len: len,
            mode: StretchMode::Resample,
            speed: 1.0,
            transpose: 0.0,
            out_len: len,
        }
    }

    /// Renders at speed 1 in every mode, checks they agree, and returns the
    /// left channel.
    fn material(input: &AudioBuffer, spec: ClipProcess) -> Vec<f32> {
        let output = process_clip(input, &spec);
        assert_eq!(output.frames(), spec.out_len as usize);
        assert_eq!(output.sample_rate(), input.sample_rate());
        for (l, r) in output.left().iter().zip(output.right()) {
            assert_eq!(*r, -l);
        }
        for mode in [StretchMode::Stretch, StretchMode::Beats] {
            assert_eq!(
                process_clip(
                    input,
                    &ClipProcess {
                        mode,
                        ..spec.clone()
                    }
                ),
                output
            );
        }
        output.left().to_vec()
    }

    /// What `ramp` sounds like at these input frames; `SILENT` for silence.
    fn heard(frames: impl IntoIterator<Item = i64>) -> Vec<f32> {
        frames.into_iter().map(|f| (f + 1) as f32).collect()
    }

    const SILENT: i64 = -1;

    #[test]
    fn plays_the_region() {
        let input = ramp(100);
        assert_eq!(material(&input, spec(10, 5)), heard(10..15));
    }

    #[test]
    fn reversed_plays_the_region_backwards() {
        let input = ramp(100);
        let clip = ClipProcess {
            reversed: true,
            ..spec(10, 5)
        };
        assert_eq!(material(&input, clip), heard([14, 13, 12, 11, 10]));
    }

    #[test]
    fn looped_repeats_from_the_phase() {
        let input = ramp(100);
        let clip = ClipProcess {
            looped: true,
            phase: 2,
            play_len: 12,
            out_len: 12,
            ..spec(10, 5)
        };
        assert_eq!(
            material(&input, clip),
            heard([12, 13, 14, 10, 11, 12, 13, 14, 10, 11, 12, 13])
        );
        // A phase past the end wraps.
        let clip = ClipProcess {
            looped: true,
            phase: 7,
            play_len: 4,
            out_len: 4,
            ..spec(10, 5)
        };
        assert_eq!(material(&input, clip), heard([12, 13, 14, 10]));
    }

    #[test]
    fn looped_and_reversed_repeats_the_reversed_region() {
        let input = ramp(100);
        let clip = ClipProcess {
            reversed: true,
            looped: true,
            phase: 1,
            play_len: 7,
            out_len: 7,
            ..spec(10, 5)
        };
        assert_eq!(material(&input, clip), heard([13, 12, 11, 10, 14, 13, 12]));
    }

    #[test]
    fn out_of_range_reads_are_silence() {
        let input = ramp(100);
        let mut expected = heard(95..100);
        expected.resize(10, 0.0);
        assert_eq!(material(&input, spec(95, 10)), expected);
        assert_eq!(material(&input, spec(500, 4)), vec![0.0; 4]);

        // Reversed, the missing frames come first.
        let clip = ClipProcess {
            reversed: true,
            ..spec(97, 5)
        };
        assert_eq!(material(&input, clip), heard([SILENT, SILENT, 99, 98, 97]));

        // A loop over a region that runs off the end loops the silence too.
        let clip = ClipProcess {
            looped: true,
            play_len: 8,
            out_len: 8,
            ..spec(98, 4)
        };
        assert_eq!(
            material(&input, clip),
            heard([98, 99, SILENT, SILENT, 98, 99, SILENT, SILENT])
        );

        // An empty loop is silent.
        let clip = ClipProcess {
            looped: true,
            play_len: 3,
            out_len: 3,
            ..spec(10, 0)
        };
        assert_eq!(material(&input, clip), vec![0.0; 3]);
    }

    #[test]
    fn output_is_cut_or_padded_to_out_len() {
        let input = ramp(100);
        assert_eq!(
            material(
                &input,
                ClipProcess {
                    out_len: 3,
                    ..spec(10, 5)
                }
            ),
            heard(10..13)
        );
        let mut padded = heard(10..15);
        padded.resize(8, 0.0);
        assert_eq!(
            material(
                &input,
                ClipProcess {
                    out_len: 8,
                    ..spec(10, 5)
                }
            ),
            padded
        );
        // Material shorter than play_len is padded with silence too.
        assert_eq!(
            material(
                &input,
                ClipProcess {
                    play_len: 8,
                    out_len: 8,
                    ..spec(10, 5)
                }
            ),
            padded
        );
    }

    #[test]
    fn material_continues_into_the_audio_around_it() {
        let input = ramp(100);
        let at = |spec: ClipProcess, from: i64, len: usize| {
            Material::new(&input, &spec).read(from, len).0
        };
        assert_eq!(
            at(spec(10, 5), -2, 9),
            heard([8, 9, 10, 11, 12, 13, 14, 15, 16])
        );
        let reversed = ClipProcess {
            reversed: true,
            ..spec(10, 5)
        };
        assert_eq!(
            at(reversed, -2, 9),
            heard([16, 15, 14, 13, 12, 11, 10, 9, 8])
        );
        let looped = ClipProcess {
            looped: true,
            phase: 1,
            play_len: 6,
            ..spec(10, 3)
        };
        assert_eq!(
            at(looped, -2, 10),
            heard([12, 10, 11, 12, 10, 11, 12, 10, 11, 12])
        );
        // Silence padding stays silent past the region.
        let padded = ClipProcess {
            play_len: 7,
            ..spec(10, 5)
        };
        assert_eq!(
            at(padded, 3, 6),
            heard([13, 14, SILENT, SILENT, SILENT, SILENT])
        );
        // The start of the input is the start of everything.
        assert_eq!(at(spec(1, 3), -3, 4), heard([SILENT, SILENT, 0, 1]));
    }

    fn render(input: &AudioBuffer, mode: StretchMode, speed: f64, transpose: f64) -> AudioBuffer {
        let len = input.frames() as u64;
        let out_len = (len as f64 / speed).round() as u64;
        process_clip(
            input,
            &ClipProcess {
                mode,
                speed,
                transpose,
                out_len,
                ..spec(0, len)
            },
        )
    }

    #[test]
    fn resample_changes_pitch_with_speed() {
        let input = mono(sine(RATE, 1000.0, RATE as usize));
        for (speed, freq) in [(2.0, 2000.0), (0.5, 500.0), (1.25, 1250.0)] {
            let output = render(&input, StretchMode::Resample, speed, 0.0);
            assert_eq!(output.frames(), (RATE as f64 / speed).round() as usize);
            let measured = measure_frequency(output.left(), RATE, 200);
            assert!(
                (measured - freq).abs() < 0.05,
                "speed {speed}: {measured} Hz"
            );
            let level = peak(&output.left()[200..output.frames() - 200]);
            assert!(
                (0.995..1.005).contains(&level),
                "speed {speed}: peak {level}"
            );
        }
    }

    #[test]
    fn resample_filters_what_would_alias() {
        // Doubled, 15 kHz would be 30 kHz, which folds to 18 kHz at 48 kHz.
        let output = render(
            &mono(sine(RATE, 15_000.0, RATE as usize)),
            StretchMode::Resample,
            2.0,
            0.0,
        );
        assert!(peak(&output.left()[200..output.frames() - 200]) < 1e-3);
        // While 8 kHz becomes a full-level 16 kHz.
        let output = render(
            &mono(sine(RATE, 8_000.0, RATE as usize)),
            StretchMode::Resample,
            2.0,
            0.0,
        );
        let rms = rms(&output.left()[200..output.frames() - 200]);
        assert!((rms - 0.5f32.sqrt()).abs() < 0.005, "rms {rms}");
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn resample_is_seamless_across_a_split() {
        // Rendering the halves of a clip separately gives the same audio as
        // the whole, because each half sees the other across the cut.
        let input = mono(sine(RATE, 3000.0, 10_000));
        let whole = ClipProcess {
            speed: 1.5,
            out_len: 4000,
            ..spec(1000, 6000)
        };
        let first = ClipProcess {
            out_len: 2000,
            ..spec(1000, 3000)
        };
        let second = ClipProcess {
            out_len: 2000,
            ..spec(4000, 3000)
        };
        let whole = process_clip(&input, &whole);
        let first = process_clip(
            &input,
            &ClipProcess {
                speed: 1.5,
                ..first
            },
        );
        let second = process_clip(
            &input,
            &ClipProcess {
                speed: 1.5,
                ..second
            },
        );
        let joined: Vec<f32> = first.left().iter().chain(second.left()).copied().collect();
        for (a, b) in joined.iter().zip(whole.left()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn resample_survives_a_rough_loop_seam() {
        // 7.3 cycles of a full-scale tone: the loop jumps at every repeat.
        let input = mono(sine(RATE, 1000.0, 400));
        for speed in [0.3, 1.7, 6.0] {
            let clip = ClipProcess {
                looped: true,
                play_len: 5000,
                speed,
                out_len: (5000.0 / speed) as u64,
                ..spec(0, 350)
            };
            let output = process_clip(&input, &clip);
            assert!(output.left().iter().all(|s| s.is_finite()));
            assert!(peak(output.left()) < 1.2, "speed {speed}");
        }
    }

    #[test]
    fn silence_after_the_material() {
        let input = mono(vec![0.5; 1000]);
        let clip = ClipProcess {
            speed: 2.0,
            out_len: 800,
            ..spec(0, 1000)
        };
        let output = process_clip(&input, &clip);
        assert_eq!(output.frames(), 800);
        // (The input starts abruptly, which rings for a few frames.)
        assert!(
            output.left()[100..480]
                .iter()
                .all(|s| (s - 0.5).abs() < 1e-3)
        );
        assert!(output.left()[500..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn stretch_keeps_pitch() {
        let input = mono(sine(RATE, 440.0, RATE as usize));
        for speed in [0.5, 0.8, 1.5] {
            let output = render(&input, StretchMode::Stretch, speed, 0.0);
            assert_eq!(output.frames(), (RATE as f64 / speed).round() as usize);
            let middle = &output.left()[RATE as usize / 10..output.frames() - RATE as usize / 10];
            let measured = measure_frequency(middle, RATE, 0);
            assert!(
                (measured - 440.0).abs() < 0.5,
                "speed {speed}: {measured} Hz"
            );
            // Steady all the way, no dropouts or bumps.
            for chunk in middle.chunks(RATE as usize / 50) {
                let level = peak(chunk);
                assert!((0.9..1.1).contains(&level), "speed {speed}: peak {level}");
            }
        }
    }

    #[test]
    fn stretch_transposes_without_changing_length() {
        let input = mono(sine(RATE, 440.0, RATE as usize));
        for (semitones, freq) in [
            (12.0, 880.0),
            (-5.0, 440.0 * 2f64.powf(-5.0 / 12.0)),
            (0.5, 440.0 * 2f64.powf(0.5 / 12.0)),
        ] {
            let output = render(&input, StretchMode::Stretch, 1.0, semitones);
            assert_eq!(output.frames(), input.frames());
            let middle = &output.left()[RATE as usize / 10..output.frames() - RATE as usize / 10];
            let measured = measure_frequency(middle, RATE, 0);
            assert!(
                (measured - freq).abs() < 1.0,
                "{semitones} st: {measured} Hz, wanted {freq}"
            );
        }
        // Transpose means nothing to the other modes.
        let plain = render(&input, StretchMode::Resample, 1.0, 0.0);
        assert_eq!(render(&input, StretchMode::Resample, 1.0, 7.0), plain);
        assert_eq!(render(&input, StretchMode::Beats, 1.0, 7.0), plain);
    }

    /// When each hit in `audio` starts: the first sample above a third of its
    /// peak, searched from 30 ms before each `expected` frame.
    fn attacks(audio: &[f32], expected: &[usize]) -> Vec<usize> {
        let span = RATE as usize * 3 / 100;
        expected
            .iter()
            .map(|&e| {
                let window = &audio[e.saturating_sub(span)..(e + span).min(audio.len())];
                let threshold = peak(window) / 3.0;
                e.saturating_sub(span) + window.iter().position(|s| s.abs() > threshold).unwrap()
            })
            .collect()
    }

    fn assert_near(found: &[usize], expected: &[usize], tolerance: usize, what: &str) {
        for (f, e) in found.iter().zip(expected) {
            assert!(
                f.abs_diff(*e) <= tolerance,
                "{what}: hit at {f}, expected {e}"
            );
        }
    }

    #[test]
    fn stretch_keeps_hits_in_time() {
        let hits: Vec<(usize, Drum, f32)> = [0.0, 0.25, 0.5, 0.875, 1.3]
            .iter()
            .map(|t| ((t * RATE as f64) as usize, Drum::Burst, 0.5))
            .collect();
        let input = mono(drums(RATE, 2 * RATE as usize, &hits, 1));
        for speed in [0.5, 0.8, 1.25, 2.0] {
            let output = render(&input, StretchMode::Stretch, speed, 0.0);
            let expected: Vec<usize> = hits.iter().map(|h| (h.0 as f64 / speed) as usize).collect();
            // Within 2.5 ms. (At four times either way, the smearing before
            // a hit grows to around 10 ms.)
            let found = attacks(output.left(), &expected);
            assert_near(
                &found,
                &expected,
                RATE as usize / 400,
                &format!("speed {speed}"),
            );
        }
    }

    #[test]
    fn stretch_keeps_time_across_silence() {
        // Long enough for the stretcher to switch to its silence shortcut.
        let hits = [
            (4 * RATE as usize, Drum::Burst, 0.5),
            (6 * RATE as usize, Drum::Snare, 0.5),
        ];
        let input = mono(drums(RATE, 7 * RATE as usize, &hits, 1));
        for speed in [0.5, 2.0] {
            let output = render(&input, StretchMode::Stretch, speed, 0.0);
            let expected: Vec<usize> = hits.iter().map(|h| (h.0 as f64 / speed) as usize).collect();
            let found = attacks(output.left(), &expected);
            assert_near(
                &found,
                &expected,
                RATE as usize / 400,
                &format!("speed {speed}"),
            );
            // Silence stays silent, but for the smear of the blocks that
            // reach the first hit (below -40 dB 50 ms ahead of it).
            let before = &output.left()[..expected[0] - RATE as usize / 20];
            assert!(
                before[..before.len() - RATE as usize / 10]
                    .iter()
                    .all(|&s| s == 0.0)
            );
            assert!(peak(before) < 0.01);
        }
    }

    #[test]
    fn stretch_is_repeatable() {
        let input = mono(drums(RATE, RATE as usize, &[(1000, Drum::Snare, 0.5)], 2));
        let a = render(&input, StretchMode::Stretch, 0.3, 2.0);
        assert_eq!(render(&input, StretchMode::Stretch, 0.3, 2.0), a);
    }

    /// Checks a Beats render of `input` at `speed`: every hit in `hits` has
    /// a cut at most 2 ms before it, every cut lands at `cut / speed`, and the
    /// slice from there on is the input untouched (after its 1 ms fade-in)
    /// until the next slice or 200 frames, whichever is first.
    fn check_beats(input: &AudioBuffer, hits: &[usize], speed: f64) -> AudioBuffer {
        let output = render(input, StretchMode::Beats, speed, 0.0);
        let cuts = detect_transients(input, 0, input.frames() as u64, BEATS_SENSITIVITY);
        for &hit in hits {
            assert!(
                cuts.iter()
                    .any(|&c| (hit - 96..=hit).contains(&(c as usize))),
                "no cut before {hit}: {cuts:?}"
            );
        }
        let fade_in = RATE as usize / 1000;
        for pair in cuts.windows(2) {
            let (cut, next) = (pair[0] as usize, pair[1] as usize);
            let at = (cut as f64 / speed).round() as usize;
            let slot = (next as f64 / speed).round() as usize - at;
            let len = 200.min(slot - 4 * fade_in);
            assert_eq!(
                output.left()[at + fade_in..][..len],
                input.left()[cut + fade_in..][..len],
                "speed {speed}: slice from {cut}"
            );
        }
        output
    }

    #[test]
    fn beats_moves_drum_hits_intact() {
        let mut rng = Rng::new(4);
        let hits: Vec<(usize, Drum, f32)> = (0..16)
            .map(|k| {
                let drum = [Drum::Kick, Drum::Hat, Drum::Snare, Drum::Hat][k % 4];
                (
                    k * RATE as usize / 8 + 500,
                    drum,
                    rng.range(0.3, 0.8) as f32,
                )
            })
            .collect();
        let input = mono(drums(RATE, 2 * RATE as usize + 1000, &hits, 4));
        let starts: Vec<usize> = hits.iter().map(|h| h.0).collect();
        for speed in [0.8, 1.25, 2.0] {
            check_beats(&input, &starts, speed);
        }
    }

    #[test]
    fn beats_does_not_click() {
        // Tonal hits (300 Hz, decaying, starting at a zero crossing) over a
        // held bass note, so a cut anywhere without a fade would click.
        let starts: Vec<usize> = [0.05, 0.3, 0.55, 0.675, 0.8, 1.05, 1.3, 1.55, 1.8]
            .iter()
            .map(|t| (t * RATE as f64) as usize)
            .collect();
        let tau = std::f64::consts::TAU;
        let mut audio: Vec<f32> = (0..2 * RATE as usize)
            .map(|n| 0.3 * (tau * 50.0 * n as f64 / RATE as f64).sin() as f32)
            .collect();
        for &start in &starts {
            for (n, s) in audio[start..].iter_mut().enumerate() {
                let t = n as f64 / RATE as f64;
                *s += (0.6 * (-t / 0.04).exp() * (tau * 300.0 * t).sin()) as f32;
            }
        }
        let input = mono(audio);
        let input_step = largest_step(input.left());
        for speed in [0.8, 1.25] {
            let output = check_beats(&input, &starts, speed);
            let step = largest_step(output.left());
            assert!(
                step < 1.5 * input_step,
                "speed {speed}: step {step}, input's {input_step}"
            );
        }
    }

    #[test]
    fn beats_slices_every_repeat_of_a_loop_alike() {
        // A one-bar loop with hits at 0, 1/4 and 5/8 of it, played 2.5
        // times from 1/8 in and slowed to 0.8.
        let bar = RATE as usize;
        let hits = [
            (200, Drum::Kick, 0.7),
            (bar / 4 + 200, Drum::Snare, 0.6),
            (bar * 5 / 8 + 200, Drum::Hat, 0.5),
        ];
        let input = mono(drums(RATE, bar, &hits, 3));
        let phase = bar / 8;
        let play_len = bar * 5 / 2;
        let speed = 0.8;
        let clip = ClipProcess {
            looped: true,
            phase: phase as u64,
            play_len: play_len as u64,
            speed,
            mode: StretchMode::Beats,
            out_len: (play_len as f64 / speed) as u64,
            ..spec(0, bar as u64)
        };
        let output = process_clip(&input, &clip);
        let mut expected = Vec::new();
        for repeat in 0..3 {
            for h in &hits {
                let position = (h.0 + repeat * bar) as i64 - phase as i64;
                if position > 0 && (position as usize) < play_len {
                    expected.push((position as f64 / speed) as usize);
                }
            }
        }
        assert_eq!(expected.len(), 7);
        assert_near(
            &attacks(output.left(), &expected),
            &expected,
            RATE as usize / 1000,
            "looped",
        );
    }

    #[test]
    fn odd_settings_never_panic_and_fill_out_len() {
        let input = mono(sine(RATE, 220.0, 3000));
        let modes = [
            StretchMode::Resample,
            StretchMode::Stretch,
            StretchMode::Beats,
        ];
        for mode in modes {
            let transposes: &[f64] = match mode {
                StretchMode::Stretch => &[-100.0, 0.0, 7.0, f64::INFINITY],
                _ => &[0.0],
            };
            for speed in [f64::NAN, 0.0, 1e-9, 0.3, 1.0, 3.0, 1e9] {
                for &transpose in transposes {
                    for (len, play_len, looped) in [
                        (0, 0, false),
                        (1, 1, false),
                        (50, 50, true),
                        (2000, 7000, true),
                        (2000, 5000, false),
                    ] {
                        for out_len in [0, 1, 777] {
                            let clip = ClipProcess {
                                looped,
                                phase: 13,
                                play_len,
                                mode,
                                speed,
                                transpose,
                                out_len,
                                ..spec(900, len)
                            };
                            let output = process_clip(&input, &clip);
                            assert_eq!(output.frames(), out_len as usize, "{clip:?}");
                            assert!(
                                output.left().iter().all(|s| s.is_finite() && s.abs() < 4.0),
                                "{clip:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// `cargo test -p media --release -- --ignored --nocapture` to see the times.
    #[test]
    #[ignore]
    fn thirty_seconds_stereo_timing() {
        let frames = 30 * RATE as usize;
        let mut rng = Rng::new(1);
        let hits: Vec<(usize, Drum, f32)> = (0..frames)
            .step_by(RATE as usize / 4)
            .enumerate()
            .map(|(k, s)| {
                (
                    s,
                    [Drum::Kick, Drum::Hat, Drum::Snare, Drum::Hat][k % 4],
                    rng.range(0.3, 0.6) as f32,
                )
            })
            .collect();
        let beat = drums(RATE, frames, &hits, 1);
        let tone = sine(RATE, 330.0, frames);
        let left: Vec<f32> = beat.iter().zip(&tone).map(|(b, t)| b + 0.2 * t).collect();
        let input = AudioBuffer::new(RATE, left, beat);
        for (mode, speed, transpose) in [
            (StretchMode::Resample, 0.5, 0.0),
            (StretchMode::Resample, 0.8, 0.0),
            (StretchMode::Resample, 2.0, 0.0),
            (StretchMode::Stretch, 0.5, 0.0),
            (StretchMode::Stretch, 0.8, 0.0),
            (StretchMode::Stretch, 1.0, 3.0),
            (StretchMode::Stretch, 2.0, -12.0),
            (StretchMode::Beats, 0.8, 0.0),
            (StretchMode::Beats, 1.25, 0.0),
        ] {
            let start = std::time::Instant::now();
            let output = render(&input, mode, speed, transpose);
            eprintln!(
                "30 s stereo 48k, {mode:?} x{speed} {transpose:+} st: {:?}",
                start.elapsed()
            );
            assert!(output.frames() > 0);
        }
    }
}
