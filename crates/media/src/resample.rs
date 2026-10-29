use std::f64::consts::PI;

use crate::AudioBuffer;

/// Zero crossings of the sinc on each side of its centre. More gives a
/// steeper cutoff at the cost of more work per output sample.
const ZERO_CROSSINGS: f64 = 32.0;

/// Fractional positions the kernel is precomputed at. Positions in between are
/// linearly interpolated; a converted 1 kHz sine stays within -95 dB of the
/// ideal one at this density.
const PHASES: usize = 256;

/// Kaiser window shape. 8 gives about 80 dB of stopband attenuation (measured:
/// a 23 kHz tone going from 48 kHz to 44.1 kHz comes out 87 dB down).
const KAISER_BETA: f64 = 8.0;

/// Cutoff as a fraction of the lower Nyquist frequency. With 32 zero crossings
/// and this window the transition band spans about ±8% of the cutoff, so 0.92
/// puts the start of the stopband just below Nyquist: nothing aliases, and the
/// passband stays flat to about 85% of Nyquist (18.7 kHz at 44.1 kHz).
const ROLLOFF: f64 = 0.92;

/// At most this many threads convert one file, and each gets at least
/// [`MIN_FRAMES_PER_THREAD`] frames (fewer aren't worth a thread).
const MAX_THREADS: usize = 4;
const MIN_FRAMES_PER_THREAD: usize = 1 << 16;

/// Width of the manual SIMD accumulators. Taps are padded to a multiple of it.
const LANES: usize = 8;

/// Convert `buffer` to `target_rate` with a windowed-sinc filter. Offline only:
/// it allocates and takes a moment on long files. Returns a clone if the rates
/// already match. Samples before the start and after the end count as silence.
///
/// Panics if either sample rate is zero.
pub fn resample(buffer: &AudioBuffer, target_rate: u32) -> AudioBuffer {
    let source_rate = buffer.sample_rate();
    assert!(
        source_rate > 0 && target_rate > 0,
        "sample rates must be non-zero"
    );
    if source_rate == target_rate {
        return buffer.clone();
    }

    let frames = buffer.frames() as u128;
    let out_len =
        ((frames * target_rate as u128 + source_rate as u128 / 2) / source_rate as u128) as usize;
    if out_len == 0 {
        return AudioBuffer::new(target_rate, Vec::new(), Vec::new());
    }

    let kernel = Kernel::new(target_rate as f64 / source_rate as f64);
    let left = kernel.pad(buffer.left());
    let right = kernel.pad(buffer.right());
    let (left, right) = kernel.run(source_rate, target_rate, &left, &right, out_len);
    AudioBuffer::new(target_rate, left, right)
}

/// Varispeed: output frame `n` is the material at position `n * speed`,
/// interpolated with the same filter as `resample`. Speeding up lowers the
/// cutoff so nothing folds back from above the new Nyquist frequency.
///
/// `material(start, len)` returns `len` frames of both channels from frame
/// `start`, which may be negative: the kernel reaches about 35 frames (times
/// the speed when speeding up) past either end of what it plays, and reading
/// the audio that really surrounds it there, rather than silence, means a clip
/// split in two plays back seamlessly.
pub(crate) fn varispeed(
    speed: f64,
    frames: usize,
    material: impl FnOnce(i64, usize) -> (Vec<f32>, Vec<f32>),
) -> (Vec<f32>, Vec<f32>) {
    assert!(speed > 0.0 && speed.is_finite(), "speed must be positive");
    if frames == 0 {
        return (Vec::new(), Vec::new());
    }
    let kernel = Kernel::new(1.0 / speed);
    let last = ((frames - 1) as f64 * speed) as usize;
    let (left, right) = material(-(kernel.half as i64 - 1), last + kernel.taps);

    let mut out_left = Vec::with_capacity(frames);
    let mut out_right = Vec::with_capacity(frames);
    for n in 0..frames {
        // Computed afresh each frame rather than accumulated, so long clips
        // don't drift.
        let position = n as f64 * speed;
        let index = position as usize;
        let (l, r) = kernel.at(
            &left[index..][..kernel.taps],
            &right[index..][..kernel.taps],
            position - index as f64,
        );
        out_left.push(l);
        out_right.push(r);
    }
    (out_left, out_right)
}

/// A lowpass windowed-sinc kernel sampled at `PHASES` fractional offsets.
struct Kernel {
    /// Taps on each side of the centre, in input samples.
    half: usize,
    /// `2 * half` rounded up to a multiple of `LANES`; the extra taps are zero.
    taps: usize,
    /// For each phase, `taps` coefficients followed by `taps` differences to
    /// the next phase, so interpolating is one multiply-add per tap.
    table: Vec<f32>,
}

impl Kernel {
    /// `ratio` is output samples per input sample.
    fn new(ratio: f64) -> Self {
        // Cutoff in cycles per input sample. When downsampling it has to sit
        // below the output's Nyquist, which stretches the kernel in time.
        let cutoff = 0.5 * ratio.min(1.0) * ROLLOFF;
        let half = (ZERO_CROSSINGS / (2.0 * cutoff)).ceil() as usize;
        let taps = (2 * half).div_ceil(LANES) * LANES;

        let window_norm = bessel_i0(KAISER_BETA);
        let h = |t: f64| -> f64 {
            let u = t / half as f64;
            if u.abs() >= 1.0 {
                return 0.0;
            }
            let x = 2.0 * cutoff * t;
            let sinc = if x == 0.0 {
                1.0
            } else {
                (PI * x).sin() / (PI * x)
            };
            let window = bessel_i0(KAISER_BETA * (1.0 - u * u).sqrt()) / window_norm;
            2.0 * cutoff * sinc * window
        };

        // Row p holds the kernel for an output that lies p / PHASES of the way
        // past input sample i, with tap j lining up with input i + j - (half - 1).
        // One extra row so the last phase has a neighbour to interpolate towards.
        let rows: Vec<Vec<f64>> = (0..=PHASES)
            .map(|p| {
                let offset = p as f64 / PHASES as f64;
                let mut row: Vec<f64> = (0..taps)
                    .map(|j| h(j as f64 - (half as f64 - 1.0) - offset))
                    .collect();
                // Normalize each phase to unity gain at DC, so a constant
                // signal comes out constant instead of with a faint ripple.
                let sum: f64 = row.iter().sum();
                row.iter_mut().for_each(|c| *c /= sum);
                row
            })
            .collect();

        let mut table = Vec::with_capacity(PHASES * 2 * taps);
        for pair in rows.windows(2) {
            table.extend(pair[0].iter().map(|&c| c as f32));
            table.extend(pair[0].iter().zip(&pair[1]).map(|(&a, &b)| (b - a) as f32));
        }
        Self { half, taps, table }
    }

    /// Copy a channel with enough silence on both sides that every kernel
    /// window is a plain slice, so the inner loop has no edge cases.
    fn pad(&self, input: &[f32]) -> Vec<f32> {
        let mut padded = Vec::with_capacity(self.half - 1 + input.len() + self.taps);
        padded.resize(self.half - 1, 0.0);
        padded.extend_from_slice(input);
        padded.resize(padded.len() + self.taps, 0.0);
        padded
    }

    fn run(
        &self,
        source_rate: u32,
        target_rate: u32,
        left: &[f32],
        right: &[f32],
        out_len: usize,
    ) -> (Vec<f32>, Vec<f32>) {
        // Track the input position as an exact fraction, so long files don't
        // drift the way an accumulated floating-point step would.
        let gcd = gcd(source_rate, target_rate);
        let step = (source_rate / gcd) as u64;
        let den = (target_rate / gcd) as u64;

        let mut out_left = vec![0.0; out_len];
        let mut out_right = vec![0.0; out_len];
        // Every output frame depends only on where it falls in the input,
        // so a long file is cut into stretches converted side by side, each
        // starting exactly where the one before it would have got to.
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .clamp(1, MAX_THREADS)
            .min(out_len.div_ceil(MIN_FRAMES_PER_THREAD));
        let chunk = out_len.div_ceil(threads);
        std::thread::scope(|scope| {
            for (n, (out_l, out_r)) in out_left
                .chunks_mut(chunk)
                .zip(out_right.chunks_mut(chunk))
                .enumerate()
            {
                let first = (n * chunk) as u64;
                let mut job = move || self.run_from(first, step, den, left, right, out_l, out_r);
                if n + 1 == threads {
                    job();
                } else {
                    scope.spawn(job);
                }
            }
        });
        (out_left, out_right)
    }

    /// Output frames from `first` on, into `out_left` and `out_right`: input
    /// `step / den` frames apart.
    #[allow(clippy::too_many_arguments)]
    fn run_from(
        &self,
        first: u64,
        step: u64,
        den: u64,
        left: &[f32],
        right: &[f32],
        out_left: &mut [f32],
        out_right: &mut [f32],
    ) {
        let (whole_step, frac_step) = (step / den, step % den);
        let start = first as u128 * step as u128;
        let mut index = (start / den as u128) as usize;
        let mut rem = (start % den as u128) as u64;
        for (l_out, r_out) in out_left.iter_mut().zip(out_right.iter_mut()) {
            let (l, r) = self.at(
                &left[index..][..self.taps],
                &right[index..][..self.taps],
                rem as f64 / den as f64,
            );
            *l_out = l;
            *r_out = r;

            index += whole_step as usize;
            rem += frac_step;
            if rem >= den {
                rem -= den;
                index += 1;
            }
        }
    }

    /// One output frame from `taps` input frames, for an output lying
    /// `fraction` (0..1) of the way past input frame `half - 1` of them.
    #[inline]
    fn at(&self, left: &[f32], right: &[f32], fraction: f64) -> (f32, f32) {
        let phase = fraction * PHASES as f64;
        let row = (phase as usize).min(PHASES - 1);
        let t = (phase - row as f64) as f32;
        let row = &self.table[row * 2 * self.taps..][..2 * self.taps];
        let (coefs, deltas) = row.split_at(self.taps);
        dot2(coefs, deltas, t, left, right)
    }
}

/// Interpolate the kernel at `t` between two phases and apply it to both
/// channels. Accumulates in `LANES` independent lanes so the compiler can
/// vectorize it; a single running sum would force strict sequential order.
#[inline]
fn dot2(coefs: &[f32], deltas: &[f32], t: f32, left: &[f32], right: &[f32]) -> (f32, f32) {
    let (coefs, _) = coefs.as_chunks::<LANES>();
    let (deltas, _) = deltas.as_chunks::<LANES>();
    let (left, _) = left.as_chunks::<LANES>();
    let (right, _) = right.as_chunks::<LANES>();
    let mut acc_l = [0.0f32; LANES];
    let mut acc_r = [0.0f32; LANES];
    for (((c, d), l), r) in coefs.iter().zip(deltas).zip(left).zip(right) {
        for k in 0..LANES {
            let w = c[k] + t * d[k];
            acc_l[k] += w * l[k];
            acc_r[k] += w * r[k];
        }
    }
    (acc_l.iter().sum(), acc_r.iter().sum())
}

/// Modified Bessel function of the first kind, order zero, by its power series.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..64 {
        term *= half / k as f64;
        let add = term * term;
        sum += add;
        if add < sum * 1e-17 {
            break;
        }
    }
    sum
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{measure_frequency, sine};

    fn mono(rate: u32, samples: Vec<f32>) -> AudioBuffer {
        AudioBuffer::new(rate, samples.clone(), samples)
    }

    fn check_sine_conversion(from: u32, to: u32) {
        let frames = from as usize; // one second
        let input = mono(from, sine(from, 1000.0, frames));
        let output = resample(&input, to);

        assert_eq!(output.sample_rate(), to);
        assert_eq!(output.frames(), to as usize);
        assert_eq!(output.left(), output.right());

        // Skip the edges, where the abrupt start and end of the tone spread out.
        let skip = 200;
        let freq = measure_frequency(output.left(), to, skip);
        assert!((freq - 1000.0).abs() < 0.01, "measured {freq} Hz");

        // The filter has no delay, so the result should match a sine computed
        // directly at the new rate.
        let ideal = sine(to, 1000.0, output.frames());
        let middle = skip..output.frames() - skip;
        let worst = output.left()[middle.clone()]
            .iter()
            .zip(&ideal[middle.clone()])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "worst error {worst}");

        let peak = output.left()[middle]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((0.997..1.001).contains(&peak), "peak {peak}");
    }

    #[test]
    fn upsampling_keeps_frequency_and_amplitude() {
        check_sine_conversion(44100, 48000);
    }

    #[test]
    fn downsampling_keeps_frequency_and_amplitude() {
        check_sine_conversion(48000, 44100);
    }

    #[test]
    fn a_long_file_converted_in_stretches_matches_one_pass() {
        let frames = 3 * MIN_FRAMES_PER_THREAD + 1234;
        let left: Vec<f32> = (0..frames)
            .map(|i| ((i * 7919) % 1000) as f32 / 500.0 - 1.0)
            .collect();
        let right: Vec<f32> = left.iter().rev().copied().collect();
        let input = AudioBuffer::new(48000, left.clone(), right.clone());
        let output = resample(&input, 44100);

        let kernel = Kernel::new(44100.0 / 48000.0);
        let (left, right) = (kernel.pad(&left), kernel.pad(&right));
        let mut one_l = vec![0.0; output.frames()];
        let mut one_r = vec![0.0; output.frames()];
        kernel.run_from(0, 160, 147, &left, &right, &mut one_l, &mut one_r);
        assert_eq!(output.left(), &one_l[..]);
        assert_eq!(output.right(), &one_r[..]);
    }

    #[test]
    fn same_rate_is_identity() {
        let input = AudioBuffer::new(44100, vec![0.1, 0.2, 0.3], vec![-0.1, 0.5, 0.0]);
        assert_eq!(resample(&input, 44100), input);
    }

    #[test]
    fn length_is_rounded_ratio() {
        for (frames, from, to, expected) in [
            (1000, 44100, 48000, 1088), // 1088.43
            (1000, 48000, 44100, 919),  // 918.75
            (1, 44100, 48000, 1),
            (1, 48000, 8000, 0), // 0.17
            (0, 44100, 48000, 0),
            (3, 22050, 44100, 6),
        ] {
            let input = mono(from, vec![0.5; frames]);
            let output = resample(&input, to);
            assert_eq!(output.frames(), expected, "{frames} frames {from} -> {to}");
            assert_eq!(output.sample_rate(), to);
        }
    }

    #[test]
    fn constant_signal_stays_constant() {
        let input = mono(44100, vec![0.5; 10_000]);
        let output = resample(&input, 48000);
        let middle = &output.left()[100..output.frames() - 100];
        assert!(middle.iter().all(|s| (s - 0.5).abs() < 1e-4));
    }

    #[test]
    fn tone_above_target_nyquist_is_filtered_out() {
        // 23 kHz exists at 48 kHz but not at 44.1 kHz, where it would alias
        // down to 21.1 kHz.
        let input = mono(48000, sine(48000, 23000.0, 48000));
        let output = resample(&input, 44100);
        let skip = 200;
        let peak = output.left()[skip..output.frames() - skip]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak < 1e-3, "peak {peak}");
    }

    #[test]
    fn audible_tone_survives_downsampling() {
        // The counterpart to the aliasing check: 18 kHz is inside the passband.
        let input = mono(48000, sine(48000, 18000.0, 48000));
        let output = resample(&input, 44100);
        let peak = output.left()[200..output.frames() - 200]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.98, "peak {peak}");
    }

    #[test]
    fn edges_behave_like_silence() {
        // An impulse at either end gives the same response as one in the
        // middle, just cut off: nothing wraps around or gets lost.
        let impulse_at = |pos: usize| {
            let mut samples = vec![0.0; 400];
            samples[pos] = 1.0;
            resample(&mono(22050, samples), 44100).left().to_vec()
        };
        let first = impulse_at(0);
        let middle = impulse_at(200);
        let last = impulse_at(399);
        for k in 0..200 {
            assert!((first[k] - middle[400 + k]).abs() < 1e-6);
            assert!((last[798 - k] - middle[400 - k]).abs() < 1e-6);
        }
        assert!(first[0] > 0.9);
    }

    /// `cargo test -p media --release -- --ignored --nocapture` to see the time.
    #[test]
    #[ignore]
    fn three_minutes_stereo_timing() {
        let frames = 44100 * 180;
        let left = sine(44100, 440.0, frames);
        let right = sine(44100, 660.0, frames);
        let input = AudioBuffer::new(44100, left, right);
        let start = std::time::Instant::now();
        let output = resample(&input, 48000);
        let elapsed = start.elapsed();
        eprintln!("3 min stereo 44.1k -> 48k: {elapsed:?}");
        assert_eq!(output.frames(), 48000 * 180);
    }
}
