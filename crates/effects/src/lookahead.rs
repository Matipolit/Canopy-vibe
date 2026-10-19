//! Lookahead gain smoothing, as the limiter and the clipper's true-peak
//! safety do it. The sound is delayed by the lookahead; the gain each
//! sample needs is held at the lowest of the coming `lookahead + hold`
//! samples' and then averaged over the lookahead, so it falls smoothly into
//! a peak and is all the way down by the time the peak arrives: nothing
//! gets through, and the gain never moves fast enough to distort.

use crate::truepeak;

/// Samples the gain is held flat beyond the lookahead, half before and
/// half after each peak, so the reconstruction around a peak sees a steady
/// gain.
const HOLD_EXTRA: usize = 16;

/// The most a gain goes down (-100 dB), which keeps the gains in a range
/// where the average's running sum is exact.
pub(crate) const MIN_GAIN: f32 = 1e-5;

/// A lookahead of `ms`, in samples at `sample_rate`.
pub(crate) fn samples(ms: f32, sample_rate: f32) -> usize {
    ((ms * 0.001 * sample_rate).round() as usize).max(1)
}

/// How many samples late a lookahead of `ms` at `sample_rate` makes the
/// sound, with the true-peak detector's delay.
pub(crate) fn latency(ms: f32, sample_rate: f32) -> usize {
    samples(ms, sample_rate) - 1 + truepeak::DELAY + HOLD_EXTRA / 2
}

/// The gain smoothing itself: the hold, then the average.
#[derive(Debug, Clone, Default)]
pub(crate) struct Lookahead {
    hold: SlidingMin,
    average: BoxAverage,
}

impl Lookahead {
    /// For a lookahead of `lookahead` samples, holding each reduction for
    /// `hold` samples more. Allocates: not for the audio thread.
    pub fn allocate(&mut self, lookahead: usize, hold: usize) {
        self.hold.allocate(lookahead + HOLD_EXTRA + hold);
        self.average.allocate(lookahead);
    }

    /// Back to unity gain.
    pub fn clear(&mut self) {
        self.hold.clear();
        self.average.clear();
    }

    /// The gain to apply now, given the gain the newest sample needs.
    #[inline]
    pub fn push(&mut self, needed: f32) -> f32 {
        self.average.push(self.hold.push(needed))
    }
}

/// The smallest of the last `window` values pushed, in constant time: a
/// queue of the values that could still be the smallest, oldest first,
/// each smaller than the one before.
#[derive(Debug, Clone, Default)]
struct SlidingMin {
    values: Vec<f32>,
    times: Vec<u64>,
    head: usize,
    len: usize,
    window: u64,
    now: u64,
}

impl SlidingMin {
    /// Allocates: not for the audio thread.
    fn allocate(&mut self, window: usize) {
        self.values = vec![0.0; window + 1];
        self.times = vec![0; window + 1];
        self.window = window as u64;
        self.clear();
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.now = 0;
    }

    fn wrap(&self, index: usize) -> usize {
        if index >= self.values.len() {
            index - self.values.len()
        } else {
            index
        }
    }

    #[inline]
    fn push(&mut self, value: f32) -> f32 {
        while self.len > 0 && self.values[self.wrap(self.head + self.len - 1)] >= value {
            self.len -= 1;
        }
        let slot = self.wrap(self.head + self.len);
        self.values[slot] = value;
        self.times[slot] = self.now;
        self.len += 1;
        while self.times[self.head] + self.window <= self.now {
            self.head = self.wrap(self.head + 1);
            self.len -= 1;
        }
        self.now += 1;
        self.values[self.head]
    }
}

/// The mean of the last `len` values pushed. The running sum is exact:
/// every value is a gain between [`MIN_GAIN`] and 1, whose bits all fit in
/// a double's mantissa alongside the sum's, so it never drifts and all ones
/// average to exactly 1.
#[derive(Debug, Clone, Default)]
struct BoxAverage {
    values: Vec<f32>,
    position: usize,
    sum: f64,
}

impl BoxAverage {
    /// Allocates: not for the audio thread.
    fn allocate(&mut self, len: usize) {
        self.values = vec![1.0; len.max(1)];
        self.clear();
    }

    /// Back to all ones.
    fn clear(&mut self) {
        self.values.fill(1.0);
        self.position = 0;
        self.sum = self.values.len() as f64;
    }

    #[inline]
    fn push(&mut self, value: f32) -> f32 {
        let old = std::mem::replace(&mut self.values[self.position], value);
        self.position += 1;
        if self.position == self.values.len() {
            self.position = 0;
        }
        self.sum += f64::from(value) - f64::from(old);
        (self.sum / self.values.len() as f64) as f32
    }
}
