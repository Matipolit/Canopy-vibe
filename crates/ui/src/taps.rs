//! The recent sound of effects that tap it (an analyzer, the EQ), kept on
//! the UI thread for scopes and spectra to draw from.

use std::time::{Duration, Instant};

use engine::TapFrame;

/// About 1.4 s at 48 kHz: enough for the largest spectrum and a slow
/// oscilloscope. Longer views (a waveform's history) keep their own
/// summaries of what [`Tap::since`] hands them.
const FRAMES: usize = 1 << 16;

/// Anything above this counts as sound, for deciding to keep repainting.
const SOUND: f32 = 1e-5;

/// How long after the last sound displays keep animating, so they can fall
/// back to rest.
const SETTLE: Duration = Duration::from_millis(1500);

pub struct Tap {
    /// A ring of the newest frames; `next` is where the next one goes.
    frames: Box<[TapFrame]>,
    next: usize,
    /// Frames ever received, for readers that keep track of what's new.
    total: u64,
    last_sound: Option<Instant>,
}

impl Default for Tap {
    fn default() -> Self {
        Tap {
            frames: vec![[0.0; 4]; FRAMES].into_boxed_slice(),
            next: 0,
            total: 0,
            last_sound: None,
        }
    }
}

impl Tap {
    pub fn push(&mut self, frame: TapFrame) {
        if frame.iter().any(|s| s.abs() > SOUND) {
            self.last_sound = Some(Instant::now());
        }
        self.frames[self.next] = frame;
        self.next = (self.next + 1) % FRAMES;
        self.total += 1;
    }

    /// Frames received so far.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The newest `count` frames (fewer if there aren't that many), oldest
    /// first.
    pub fn latest(&self, count: usize) -> impl Iterator<Item = TapFrame> + '_ {
        let count = count.min(FRAMES).min(self.total as usize);
        let start = (self.next + FRAMES - count) % FRAMES;
        (0..count).map(move |i| self.frames[(start + i) % FRAMES])
    }

    /// The frames that arrived after the first `seen` (as far back as the
    /// ring reaches), oldest first.
    pub fn since(&self, seen: u64) -> impl Iterator<Item = TapFrame> + '_ {
        let new = self.total.saturating_sub(seen).min(FRAMES as u64) as usize;
        self.latest(new)
    }

    /// Whether there's been sound lately, so displays should keep moving.
    pub fn active(&self) -> bool {
        self.last_sound.is_some_and(|t| t.elapsed() < SETTLE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_frames_in_order() {
        let mut tap = Tap::default();
        assert_eq!(tap.latest(10).count(), 0);
        for i in 0..(FRAMES + 10) {
            tap.push([i as f32, 0.0, 0.0, 0.0]);
        }
        let newest: Vec<f32> = tap.latest(3).map(|f| f[0]).collect();
        let last = (FRAMES + 9) as f32;
        assert_eq!(newest, [last - 2.0, last - 1.0, last]);
        assert_eq!(tap.latest(usize::MAX).count(), FRAMES);
        let seen = tap.total();
        tap.push([1.0; 4]);
        tap.push([2.0; 4]);
        let new: Vec<f32> = tap.since(seen).map(|f| f[0]).collect();
        assert_eq!(new, [1.0, 2.0]);
        assert!(tap.active());
    }
}
