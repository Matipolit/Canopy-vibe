use std::fmt;

use crate::AudioBuffer;

/// Frames per block at the finest mipmap level, as a power of two.
const BASE_SHIFT: u32 = 6;
const BASE_BLOCK: usize = 1 << BASE_SHIFT;

/// A min/max summary of a file's audio for drawing waveforms, answering "what
/// is the lowest and highest sample between these two frames" in about
/// constant time however long the range is.
///
/// Mono: each frame is the average of left and right, the same thing you'd
/// hear from a mono downmix.
///
/// Memory vs speed: besides the mipmap (8 bytes per 64 frames, plus as much
/// again for the coarser levels, so about 0.25 bytes per frame) this keeps the
/// raw mono samples, 4 bytes per frame (half the size of the stereo audio).
/// Without them a zoomed-in view could only draw whole 64-frame blocks and
/// would look blocky, and a sub-frame query couldn't return the actual sample.
/// With them a query reads at most 63 raw samples at each ragged end plus two
/// blocks per level, which is cheap enough to run once per pixel column.
/// Storing them as `i16` would halve the memory if that ever matters.
pub struct Peaks {
    samples: Box<[f32]>,
    /// `levels[k][j]` is the (min, max) of frames `[j << (6 + k), (j + 1) << (6 + k))`,
    /// cut off at the end of the audio. Each level halves the previous one,
    /// down to a single block.
    levels: Vec<Box<[(f32, f32)]>>,
}

impl Peaks {
    pub fn new(buffer: &AudioBuffer) -> Self {
        let samples: Box<[f32]> = buffer
            .left()
            .iter()
            .zip(buffer.right())
            .map(|(l, r)| 0.5 * (l + r))
            .collect();

        let mut levels = Vec::new();
        let mut level: Box<[(f32, f32)]> = samples.chunks(BASE_BLOCK).map(scan).collect();
        while level.len() > 1 {
            let coarser = level
                .chunks(2)
                .map(|pair| pair.iter().copied().fold(EMPTY, merge))
                .collect();
            levels.push(level);
            level = coarser;
        }
        if !level.is_empty() {
            levels.push(level);
        }
        Self { samples, levels }
    }

    /// Number of frames of the audio it summarizes.
    pub fn frames(&self) -> u64 {
        self.samples.len() as u64
    }

    /// The (min, max) over all frames overlapping `[start, end)`. Positions are
    /// fractional because a pixel column rarely lands on whole frames.
    ///
    /// Clamped to the audio; (0.0, 0.0) if the range misses it entirely. A
    /// range narrower than one frame returns the sample at `floor(start)` as
    /// both min and max, so a zoomed-in view draws individual samples.
    pub fn min_max(&self, start: f64, end: f64) -> (f32, f32) {
        if start.is_nan() || end.is_nan() {
            return (0.0, 0.0);
        }
        let len = self.samples.len() as f64;
        if end - start < 1.0 {
            let frame = start.floor();
            return if frame >= 0.0 && frame < len {
                let sample = self.samples[frame as usize];
                (sample, sample)
            } else {
                (0.0, 0.0)
            };
        }
        let first = start.floor().max(0.0);
        let last = end.ceil().min(len);
        if first >= last {
            return (0.0, 0.0);
        }
        self.frame_range(first as usize, last as usize)
    }

    /// (min, max) over frames `[first, last)`, which must be non-empty and in bounds.
    fn frame_range(&self, first: usize, last: usize) -> (f32, f32) {
        // Whole base blocks inside the range.
        let mut lo = first.div_ceil(BASE_BLOCK);
        let mut hi = last >> BASE_SHIFT;
        if lo >= hi {
            // Less than two blocks long: just scan it.
            return scan(&self.samples[first..last]);
        }
        let mut acc = merge(
            scan(&self.samples[first..lo << BASE_SHIFT]),
            scan(&self.samples[hi << BASE_SHIFT..last]),
        );
        // Walk up the levels, taking the odd block off either end of [lo, hi)
        // so the rest pairs up exactly into blocks of the next level.
        for level in &self.levels {
            if lo >= hi {
                break;
            }
            if lo % 2 == 1 {
                acc = merge(acc, level[lo]);
                lo += 1;
            }
            if hi % 2 == 1 {
                hi -= 1;
                acc = merge(acc, level[hi]);
            }
            lo /= 2;
            hi /= 2;
        }
        acc
    }
}

impl fmt::Debug for Peaks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Peaks")
            .field("frames", &self.samples.len())
            .field("levels", &self.levels.len())
            .finish()
    }
}

/// The identity for `merge`: what an empty range folds to.
const EMPTY: (f32, f32) = (f32::INFINITY, f32::NEG_INFINITY);

fn merge(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    (a.0.min(b.0), a.1.max(b.1))
}

/// (min, max) of a slice, `EMPTY` if it has none. Keeps eight independent
/// lanes so the compiler can vectorize it.
fn scan(samples: &[f32]) -> (f32, f32) {
    const LANES: usize = 8;
    let (chunks, rest) = samples.as_chunks::<LANES>();
    let mut lo = [f32::INFINITY; LANES];
    let mut hi = [f32::NEG_INFINITY; LANES];
    for chunk in chunks {
        for k in 0..LANES {
            lo[k] = lo[k].min(chunk[k]);
            hi[k] = hi[k].max(chunk[k]);
        }
    }
    let lanes = lo.into_iter().zip(hi).fold(EMPTY, merge);
    rest.iter().map(|&s| (s, s)).fold(lanes, merge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::Rng;

    /// Straight from the definition: every frame whose span [i, i + 1)
    /// overlaps [start, end).
    fn brute_force(samples: &[f32], start: f64, end: f64) -> (f32, f32) {
        if end - start < 1.0 {
            let frame = start.floor();
            return if frame >= 0.0 && frame < samples.len() as f64 {
                (samples[frame as usize], samples[frame as usize])
            } else {
                (0.0, 0.0)
            };
        }
        let mut result: Option<(f32, f32)> = None;
        for (i, &s) in samples.iter().enumerate() {
            if (i as f64) < end && (i + 1) as f64 > start {
                result = Some(result.map_or((s, s), |r| merge(r, (s, s))));
            }
        }
        result.unwrap_or((0.0, 0.0))
    }

    fn random_buffer(frames: usize, seed: u64) -> AudioBuffer {
        let mut rng = Rng::new(seed);
        let mut channel =
            || -> Vec<f32> { (0..frames).map(|_| rng.range(-1.0, 1.0) as f32).collect() };
        let left = channel();
        let right = channel();
        AudioBuffer::new(44100, left, right)
    }

    #[test]
    fn matches_brute_force_on_random_ranges() {
        // Not a multiple of 64, so there's a partial block at the end.
        let frames = 50_003;
        let buffer = random_buffer(frames, 1);
        let peaks = Peaks::new(&buffer);
        let mono: Vec<f32> = buffer
            .left()
            .iter()
            .zip(buffer.right())
            .map(|(l, r)| 0.5 * (l + r))
            .collect();
        assert_eq!(peaks.frames(), frames as u64);

        let n = frames as f64;
        let mut rng = Rng::new(2);
        for round in 0..3000 {
            let (start, end) = match round % 7 {
                // Narrower than a frame, including just off either end.
                0 => {
                    let start = rng.range(-3.0, n + 3.0);
                    (start, start + rng.range(0.0, 1.0))
                }
                // A few frames to a few blocks.
                1 => {
                    let start = rng.range(-10.0, n);
                    (start, start + rng.range(1.0, 300.0))
                }
                // A typical zoomed-out pixel.
                2 => {
                    let start = rng.range(0.0, n);
                    (start, start + rng.range(300.0, 20_000.0))
                }
                // Huge, often covering everything.
                3 => {
                    let start = rng.range(-n, n);
                    (start, start + rng.range(0.0, 3.0 * n))
                }
                // Straddling the end.
                4 => (rng.range(n - 500.0, n), rng.range(n, n + 500.0)),
                // Whole frames, which line up with block boundaries now and then.
                5 => {
                    let start = (rng.next_u64() % frames as u64) as f64;
                    let len = (rng.next_u64() % 5000) as f64;
                    (start, start + len)
                }
                // Exactly on block boundaries.
                _ => {
                    let blocks = (frames / 64) as u64;
                    let start = (rng.next_u64() % blocks * 64) as f64;
                    let end = start + ((rng.next_u64() % 200 + 1) * 64) as f64;
                    (start, end)
                }
            };
            assert_eq!(
                peaks.min_max(start, end),
                brute_force(&mono, start, end),
                "range {start}..{end}"
            );
        }
    }

    #[test]
    fn whole_range_and_edges() {
        let value = |i: usize| i as f32 / 1000.0 - 0.5;
        let ramp: Vec<f32> = (0..1000).map(value).collect();
        let peaks = Peaks::new(&AudioBuffer::new(48000, ramp.clone(), ramp));
        let all = (value(0), value(999));

        assert_eq!(peaks.min_max(0.0, 1000.0), all);
        assert_eq!(peaks.min_max(-1e12, 1e12), all);
        assert_eq!(peaks.min_max(f64::NEG_INFINITY, f64::INFINITY), all);
        // Overlaps frames 10 and 11 only.
        assert_eq!(peaks.min_max(10.5, 11.5), (value(10), value(11)));
        // Sub-frame: the sample at floor(start), even if it reaches into the next frame.
        assert_eq!(peaks.min_max(10.7, 11.2), (value(10), value(10)));
        assert_eq!(peaks.min_max(10.0, 10.0), (value(10), value(10)));
        // Clamped at the end.
        assert_eq!(peaks.min_max(990.0, 5000.0), (value(990), value(999)));
        // Outside.
        assert_eq!(peaks.min_max(-50.0, -10.0), (0.0, 0.0));
        assert_eq!(peaks.min_max(1000.0, 2000.0), (0.0, 0.0));
        assert_eq!(peaks.min_max(-0.5, -0.1), (0.0, 0.0));
        assert_eq!(peaks.min_max(f64::NAN, 10.0), (0.0, 0.0));
    }

    #[test]
    fn mono_is_the_average_of_both_channels() {
        let buffer = AudioBuffer::new(44100, vec![1.0, -1.0, 0.5], vec![0.0, -1.0, -0.5]);
        let peaks = Peaks::new(&buffer);
        assert_eq!(peaks.min_max(0.0, 1.0), (0.5, 0.5));
        assert_eq!(peaks.min_max(0.0, 3.0), (-1.0, 0.5));
        assert_eq!(peaks.min_max(2.0, 3.0), (0.0, 0.0));
    }

    #[test]
    fn empty_and_tiny_buffers() {
        let empty = Peaks::new(&AudioBuffer::new(44100, Vec::new(), Vec::new()));
        assert_eq!(empty.frames(), 0);
        assert_eq!(empty.min_max(0.0, 100.0), (0.0, 0.0));
        assert_eq!(empty.min_max(0.0, 0.5), (0.0, 0.0));

        let one = Peaks::new(&AudioBuffer::new(44100, vec![0.25], vec![0.25]));
        assert_eq!(one.min_max(-5.0, 5.0), (0.25, 0.25));
        assert_eq!(one.min_max(0.2, 0.4), (0.25, 0.25));
    }
}
