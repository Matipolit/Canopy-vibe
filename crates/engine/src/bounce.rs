//! Playing an arrangement from start to finish without a sound card, as
//! fast as it renders, for exporting audio.

use std::ops::Range;

use media::AudioBuffer;

use crate::arrangement::{MAX_TAIL_SECONDS, TAIL_SILENCE, TAIL_SILENT_SECONDS};
use crate::{Arrangement, Engine, EngineHandle, create};

pub struct Bounce {
    engine: Engine,
    /// Keeps the engine's queues open.
    _handle: EngineHandle,
    sample_rate: u32,
    /// Samples rendered so far.
    position: u64,
    /// How many samples at the start to leave out: as many as the mix
    /// plays late, so the audio lines up with the range.
    skip: u64,
    /// Where the range ends, counted like `position`.
    end: u64,
    /// Where rendering stops at the latest: the end, plus the longest tail.
    limit: u64,
    quiet_needed: u64,
    quiet: u64,
    left: Vec<f32>,
    right: Vec<f32>,
    interleaved: Vec<f32>,
}

impl Bounce {
    /// Renders `arrangement` from sample 0 to `end`, and with `tail` on past
    /// it until everything has rung out (up to ten seconds more).
    /// `arrangement` must come from a fresh [`crate::Compiler`], so every
    /// track has its own processors.
    pub fn new(arrangement: Box<Arrangement>, sample_rate: u32, end: u64, tail: bool) -> Bounce {
        Bounce::range(arrangement, sample_rate, 0..end, tail, 0)
    }

    /// Like [`Bounce::new`], over `range` (in samples). `latency` is how late
    /// the mix plays ([`crate::Compiler::latency`]): that much more is
    /// rendered and the start left out, so effects that look ahead don't
    /// shift the audio.
    pub fn range(
        arrangement: Box<Arrangement>,
        sample_rate: u32,
        range: Range<u64>,
        tail: bool,
        latency: usize,
    ) -> Bounce {
        let (handle, mut engine) = create(sample_rate as f32);
        engine.start_offline(arrangement, sample_rate as f32, range.start);
        let rate = sample_rate as f64;
        let skip = latency as u64;
        let end = range.end.saturating_sub(range.start).max(1) + skip;
        Bounce {
            engine,
            _handle: handle,
            sample_rate,
            position: 0,
            skip,
            end,
            limit: if tail {
                end + (MAX_TAIL_SECONDS * rate) as u64
            } else {
                end
            },
            quiet_needed: (TAIL_SILENT_SECONDS * rate) as u64,
            quiet: 0,
            left: Vec::new(),
            right: Vec::new(),
            interleaved: Vec::new(),
        }
    }

    /// Renders up to `frames` more. True once it's all rendered.
    pub fn step(&mut self, frames: usize) -> bool {
        if self.is_done() {
            return true;
        }
        let frames = frames.min((self.limit - self.position) as usize);
        self.interleaved.clear();
        self.interleaved.resize(frames * 2, 0.0);
        self.engine.process_interleaved(&mut self.interleaved, 2);
        for &[l, r] in self.interleaved.as_chunks::<2>().0 {
            if self.position >= self.skip {
                self.left.push(l);
                self.right.push(r);
            }
            // Past the end, count how long it's been quiet.
            if self.position >= self.end {
                if l.abs().max(r.abs()) < TAIL_SILENCE {
                    self.quiet += 1;
                } else {
                    self.quiet = 0;
                }
            }
            self.position += 1;
        }
        self.is_done()
    }

    fn is_done(&self) -> bool {
        self.position >= self.limit
            || (self.position >= self.end && self.quiet >= self.quiet_needed)
    }

    /// How far along it is, `0..=1`; the tail counts as the last bit.
    pub fn progress(&self) -> f32 {
        if self.is_done() {
            1.0
        } else {
            (self.position as f64 / self.end as f64).min(0.99) as f32
        }
    }

    /// The audio: the whole range, and the tail up to its last sound.
    pub fn finish(mut self) -> AudioBuffer {
        let end = (self.end - self.skip) as usize;
        let (mut left, mut right) = (
            std::mem::take(&mut self.left),
            std::mem::take(&mut self.right),
        );
        let last_sound = left
            .iter()
            .zip(&right)
            .rposition(|(l, r)| l.abs().max(r.abs()) >= TAIL_SILENCE)
            .map_or(0, |i| i + 1);
        let len = last_sound.max(end).min(left.len());
        left.truncate(len);
        right.truncate(len);
        AudioBuffer::new(self.sample_rate, left, right)
    }
}

impl Drop for Bounce {
    /// Finished or abandoned, what it rendered with stops on the thread
    /// that rendered it.
    fn drop(&mut self) {
        self.engine.retire_offline();
    }
}
