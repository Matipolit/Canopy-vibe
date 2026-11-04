//! Delay compensation. Some effects hand back their sound late (a
//! limiter's lookahead), so a track through one would play behind the
//! others. The compiler works out how late each track arrives at the bus it
//! plays into, and delays the rest to match: everything meets in time, the
//! whole mix later by the slowest path.

/// The most a track can be delayed: about 170 ms at 48 kHz, far more than
/// any built-in effect's lookahead.
pub(crate) const MAX_DELAY: usize = 8192;

/// When the delay changes, the old and new delays crossfade over this many
/// samples instead of jumping, which would click.
const FADE: usize = 256;

/// One track's compensation delay, or one graph cable's. Lives with the
/// track's processors (or the graph's state), so it carries on through
/// snapshot swaps; allocated on the UI thread.
///
/// The ring's length is a power of two past the longest delay it takes, so
/// wrapping an index is a mask rather than a division per sample.
pub(crate) struct Compensation {
    left: Box<[f32]>,
    right: Box<[f32]>,
    /// The ring's length less one.
    mask: usize,
    /// The longest delay it takes.
    max: usize,
    /// Where the next sample goes.
    write: usize,
    current: usize,
    target: usize,
    /// Samples into a crossfade from `current` to `target`.
    fade: usize,
}

impl Compensation {
    /// A track's: up to [`MAX_DELAY`].
    pub fn new(delay: usize) -> Compensation {
        Compensation::sized(delay, MAX_DELAY)
    }

    /// One that delays by up to `max` samples.
    pub fn sized(delay: usize, max: usize) -> Compensation {
        let ring = (max + 1).next_power_of_two();
        let delay = delay.min(max);
        Compensation {
            left: vec![0.0; ring].into_boxed_slice(),
            right: vec![0.0; ring].into_boxed_slice(),
            mask: ring - 1,
            max,
            write: 0,
            current: delay,
            target: delay,
            fade: 0,
        }
    }

    /// Delays by `delay` samples from now on, fading over from the old one.
    pub fn set(&mut self, delay: usize) {
        self.target = delay.min(self.max);
    }

    /// Carries on from `old`, a line this one replaces (one too short for
    /// its new delay): the sound it holds, and the delay it plays at, from
    /// which this one fades to its own. Copies only: real-time safe.
    pub fn take_over(&mut self, old: &Compensation) {
        let kept = old.left.len().min(self.left.len());
        for back in 1..=kept {
            let from = old.write.wrapping_sub(back) & old.mask;
            let to = self.write.wrapping_sub(back) & self.mask;
            self.left[to] = old.left[from];
            self.right[to] = old.right[from];
        }
        self.current = old.current.min(self.max);
        self.fade = 0;
    }

    /// Delays by `delay` samples straight away.
    pub fn jump(&mut self, delay: usize) {
        self.set(delay);
        self.current = self.target;
        self.fade = 0;
    }

    /// Delays a block in place.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.current == 0 && self.target == 0 {
            // Most tracks aren't delayed at all. The block passes through
            // untouched, but still goes into the ring: a delay that starts
            // later fades in from what was just played, not from stale sound.
            for (l, r) in left.iter().zip(right.iter()) {
                self.left[self.write] = *l;
                self.right[self.write] = *r;
                self.write = (self.write + 1) & self.mask;
            }
            return;
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            self.left[self.write] = *l;
            self.right[self.write] = *r;
            let mask = self.mask;
            let read = |delay: usize| self.write.wrapping_sub(delay) & mask;
            let at = read(self.current);
            let (mut out_l, mut out_r) = (self.left[at], self.right[at]);
            if self.target != self.current {
                let to = read(self.target);
                let t = self.fade as f32 / FADE as f32;
                out_l += (self.left[to] - out_l) * t;
                out_r += (self.right[to] - out_r) * t;
                self.fade += 1;
                if self.fade >= FADE {
                    self.current = self.target;
                    self.fade = 0;
                }
            }
            *l = out_l;
            *r = out_r;
            self.write = (self.write + 1) & self.mask;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_by_exactly_the_amount_and_fades_between_amounts() {
        let mut line = Compensation::new(100);
        let mut left = vec![0.0; 512];
        left[10] = 1.0;
        let mut right = left.clone();
        line.process(&mut left, &mut right);
        assert_eq!(left[110], 1.0);
        assert_eq!(left.iter().filter(|s| **s != 0.0).count(), 1);

        // A change crossfades: a steady input stays steady.
        let mut line = Compensation::new(0);
        let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
        line.process(&mut l, &mut r);
        line.set(40);
        for _ in 0..8 {
            let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
            line.process(&mut l, &mut r);
            assert!(l.iter().all(|s| (*s - 0.5).abs() < 1e-6));
        }
        assert_eq!(line.current, 40);
    }

    #[test]
    fn a_longer_line_takes_over_the_sound_and_delay_of_the_one_it_replaces() {
        let mut short = Compensation::sized(10, 15);
        let ramp: Vec<f32> = (0..64).map(|i| i as f32).collect();
        let (mut l, mut r) = (ramp.clone(), ramp.clone());
        short.process(&mut l, &mut r);
        assert_eq!(l[10], 0.0);

        let mut long = Compensation::sized(100, 1023);
        long.take_over(&short);
        long.set(100);
        // Straight on from where the short line was: 10 samples late.
        let (mut l, mut r) = (vec![1000.0; 4], vec![1000.0; 4]);
        long.process(&mut l, &mut r);
        assert_eq!(l[0], 54.0, "the sound from 10 samples ago");
        assert_eq!(long.current, 10);
        // Then it fades to its own delay.
        for _ in 0..FADE / 4 {
            let (mut l, mut r) = (vec![0.0; 4], vec![0.0; 4]);
            long.process(&mut l, &mut r);
        }
        assert_eq!(long.current, 100);
    }

    #[test]
    fn no_delay_passes_through_and_keeps_history_for_a_later_one() {
        let mut line = Compensation::new(0);
        let ramp: Vec<f32> = (0..64).map(|i| i as f32).collect();
        let (mut l, mut r) = (ramp.clone(), ramp.clone());
        line.process(&mut l, &mut r);
        assert_eq!(l, ramp, "untouched");
        assert_eq!(r, ramp);
        // Delaying by 32 from now fades from the samples just played: at
        // the end of the fade the output is the input 32 samples ago,
        // which for a ramp of 64 + i is 32 + i.
        line.set(32);
        for _ in 0..FADE / 64 {
            let (mut l, mut r) = (ramp.clone(), ramp.clone());
            line.process(&mut l, &mut r);
        }
        let (mut l, mut r) = (vec![1000.0; 64], vec![1000.0; 64]);
        line.process(&mut l, &mut r);
        assert_eq!(line.current, 32);
        assert_eq!(l[0], 32.0, "the ramp from 32 samples ago");
        assert_eq!(l[31], 63.0);
        assert_eq!(l[32], 1000.0);
    }
}
