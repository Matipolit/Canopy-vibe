//! Delay-line pieces: a ring buffer that clears in constant time, and taps
//! that crossfade to a new delay instead of jumping (and, for a modulator,
//! glide there instead).

use dsp::simd::F32x4;

/// Moves a read position `tap` towards `wanted` by at most `max_slope`
/// samples a sample, or straight there if it hasn't been `placed` yet;
/// returns it. A delay read through it bends in pitch, never jumps.
#[inline]
pub(crate) fn slew(tap: &mut f32, wanted: f32, placed: bool, max_slope: f32) -> f32 {
    *tap = if placed {
        *tap + (wanted - *tap).clamp(-max_slope, max_slope)
    } else {
        wanted
    };
    *tap
}

/// `x` if it's within `ceiling`; louder, bent smoothly towards twice that:
/// keeps a feedback loop a modulator pushes about from running away. NaN
/// and infinity come out NaN, for `dsp::flush` or `settle` to catch.
#[inline]
pub(crate) fn contain(x: f32, ceiling: f32) -> f32 {
    let size = x.abs();
    if size <= ceiling {
        return x;
    }
    let over = size - ceiling;
    (ceiling + over / (1.0 + over / ceiling)).copysign(x)
}

/// A circular buffer of past samples.
#[derive(Debug, Clone, Default)]
pub(crate) struct Ring {
    buffer: Vec<f32>,
    /// Where the next sample goes.
    write: usize,
    /// How many samples were pushed since the last clear, up to the length.
    /// Older ones read as silence, which makes clearing constant-time even
    /// for buffers many seconds long.
    filled: usize,
}

impl Ring {
    /// Room for `len` samples, all silent. Allocates: not for the audio thread.
    pub fn allocate(&mut self, len: usize) {
        self.buffer = vec![0.0; len.max(1)];
        self.write = 0;
        self.filled = 0;
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn clear(&mut self) {
        self.filled = 0;
    }

    #[inline]
    pub fn push(&mut self, x: f32) {
        self.buffer[self.write] = x;
        self.write += 1;
        if self.write == self.buffer.len() {
            self.write = 0;
        }
        if self.filled < self.buffer.len() {
            self.filled += 1;
        }
    }

    /// Pushes `x`, and into `out` (as long) what [`Ring::get_between`] at
    /// `age` gives after each push: what a tap holding still reads over a
    /// block. When the block lands in one piece, the ring has filled and
    /// every read stays clear of both ends, the pushes are one copy and the
    /// reads interpolate four samples at a time from the run they lie in;
    /// otherwise it goes a sample at a time. The same samples and the same
    /// arithmetic either way.
    pub fn push_and_read(&mut self, x: &[f32], age: f32, out: &mut [f32]) {
        let m = x.len().min(out.len());
        let (whole, floor) = dsp::split_index(age);
        let t = age - floor;
        let w = self.write;
        let len = self.buffer.len();
        // The four samples around `whole` pushes before sample `n` sit at
        // `w + n - whole - 2 ..= w + n - whole + 1`.
        let in_one_piece = self.filled == len && w + m <= len && whole >= 1 && w >= whole + 2;
        if !in_one_piece || age.is_nan() {
            for n in 0..m {
                self.push(x[n]);
                out[n] = self.get_between(age);
            }
            return;
        }
        self.buffer[w..w + m].copy_from_slice(&x[..m]);
        self.write = if w + m == len { 0 } else { w + m };
        if t <= 0.0 {
            out[..m].copy_from_slice(&self.buffer[w - whole..w - whole + m]);
            return;
        }
        let window = &self.buffer[w - whole - 2..w - whole + m + 1];
        let ts = F32x4::splat(t);
        let mut n = 0;
        while n + 4 <= m {
            // Oldest first in the buffer: `catmull_rom` takes the newest first.
            let y = catmull_rom_x4(
                F32x4::load(window, n + 3),
                F32x4::load(window, n + 2),
                F32x4::load(window, n + 1),
                F32x4::load(window, n),
                ts,
            );
            y.store(out, n);
            n += 4;
        }
        for n in n..m {
            out[n] = dsp::catmull_rom(window[n + 3], window[n + 2], window[n + 1], window[n], t);
        }
    }

    /// The sample pushed `age` pushes before the latest one (0 is the latest).
    #[inline]
    pub fn get(&self, age: usize) -> f32 {
        if age >= self.filled {
            return 0.0;
        }
        let index = if age < self.write {
            self.write - 1 - age
        } else {
            self.write + self.buffer.len() - 1 - age
        };
        self.buffer[index]
    }
}

/// Which samples to read this sample, from a [`Crossfade`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct Taps {
    from: usize,
    to: usize,
    fade: f32,
}

impl Taps {
    /// The sample at these ages in `ring`.
    #[inline]
    pub fn read(self, ring: &Ring) -> f32 {
        let a = ring.get(self.from);
        if self.from == self.to {
            a
        } else {
            a + (ring.get(self.to) - a) * self.fade
        }
    }
}

/// Where to read a delay line: an age that, when it changes, crossfades from
/// the old tap to the new one over a fixed time rather than jumping (a click)
/// or gliding (a pitch bend). A change that arrives mid-fade waits for it to
/// finish, so any sequence of changes is a sequence of clean fades.
#[derive(Debug, Clone)]
pub(crate) struct Crossfade {
    current: usize,
    next: usize,
    target: usize,
    fade: f32,
    step: f32,
}

impl Crossfade {
    pub fn new(age: usize) -> Crossfade {
        Crossfade {
            current: age,
            next: age,
            target: age,
            fade: 0.0,
            step: 1.0,
        }
    }

    pub fn set_fade_length(&mut self, samples: usize) {
        self.step = 1.0 / samples.max(1) as f32;
    }

    pub fn set_target(&mut self, age: usize) {
        self.target = age;
    }

    /// Go straight to `age`, abandoning any fade.
    pub fn jump(&mut self, age: usize) {
        self.current = age;
        self.next = age;
        self.target = age;
        self.fade = 0.0;
    }

    /// The taps for this sample; call once per sample.
    #[inline]
    pub fn advance(&mut self) -> Taps {
        if self.current == self.next {
            if self.target == self.current {
                return Taps {
                    from: self.current,
                    to: self.current,
                    fade: 0.0,
                };
            }
            self.next = self.target;
            self.fade = 0.0;
        }
        self.fade += self.step;
        if self.fade >= 1.0 {
            self.current = self.next;
            self.fade = 0.0;
            return Taps {
                from: self.current,
                to: self.current,
                fade: 0.0,
            };
        }
        Taps {
            from: self.current,
            to: self.next,
            fade: self.fade,
        }
    }
}

impl Ring {
    /// What was pushed `delay` samples before the sample about to be
    /// pushed, between samples: a delay line read before it is written.
    #[inline]
    pub fn delayed(&self, delay: f32) -> f32 {
        self.get_between(delay - 1.0)
    }

    /// The sample `age` pushes before the latest one, between samples when
    /// `age` isn't whole (a cubic through the four nearest), and exactly
    /// the sample when it is.
    #[inline]
    pub fn get_between(&self, age: f32) -> f32 {
        let (whole, floor) = dsp::split_index(age);
        let t = age - floor;
        if t <= 0.0 {
            return self.get(whole);
        }
        // Usually the four samples sit together in the buffer, short of
        // the wrap and all pushed: one check, then four plain reads,
        // rather than the branches of four `get`s.
        if whole >= 1 && whole + 2 < self.filled && whole + 3 <= self.write {
            let oldest = self.write - 3 - whole;
            let &[x3, x2, x1, x0] = &self.buffer[oldest..oldest + 4] else {
                unreachable!("four samples");
            };
            return dsp::catmull_rom(x0, x1, x2, x3, t);
        }
        let before = if whole > 0 {
            self.get(whole - 1)
        } else {
            self.get(0)
        };
        dsp::catmull_rom(
            before,
            self.get(whole),
            self.get(whole + 1),
            self.get(whole + 2),
            t,
        )
    }
}

/// Where to read a delay line this sample, from a [`Tap`]: two ages and how
/// far the fade from the first to the second has got.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TapRead {
    from: f32,
    to: f32,
    fade: f32,
}

impl TapRead {
    #[inline]
    pub fn read(self, ring: &Ring) -> f32 {
        let to = ring.get_between(self.to);
        if self.fade >= 1.0 {
            to
        } else {
            let from = ring.get_between(self.from);
            from + (to - from) * self.fade
        }
    }

    /// The age heard most.
    #[cfg(test)]
    pub fn age(self) -> f32 {
        if self.fade < 0.5 { self.from } else { self.to }
    }
}

/// Where to read a delay line, which moves two ways. A knob's change (see
/// [`crate::Effect::set_param`]) crossfades from the old age to the new one
/// over a fixed time, as [`Crossfade`] does: no click, no pitch bend; a
/// change that arrives mid-move waits for it to end. A modulator's change
/// ([`crate::Effect::modulate`]) glides the tap there instead, reading
/// between samples, as a tape delay's head would move: the pitch bends,
/// never faster than `max_slope` samples a sample, so it never stops or
/// runs backwards, and a fast modulator's ages are joined by straight
/// lines rather than stepped between.
#[derive(Debug, Clone)]
pub(crate) struct Tap {
    /// The age fading out, and the one fading in or heard alone.
    from: f32,
    to: f32,
    /// How far the fade has got (1 when there's none) and its step.
    fade: f32,
    fade_step: f32,
    /// A knob's age waiting for the move under way to end.
    waiting: Option<f32>,
    /// Where `to` glides, the samples it has left to get there, and the
    /// most it moves a sample.
    glide_target: f32,
    glide_left: u32,
    max_slope: f32,
}

impl Tap {
    pub fn new(age: f32) -> Tap {
        Tap {
            from: age,
            to: age,
            fade: 1.0,
            fade_step: 1.0,
            waiting: None,
            glide_target: age,
            glide_left: 0,
            max_slope: 0.5,
        }
    }

    pub fn set_fade_length(&mut self, samples: usize) {
        self.fade_step = 1.0 / samples.max(1) as f32;
    }

    /// The most a glide moves the tap a sample; under 1, so the sound
    /// never stops or plays backwards.
    pub fn set_max_slope(&mut self, slope: f32) {
        self.max_slope = slope.clamp(1e-3, 0.99);
    }

    fn gliding(&self) -> bool {
        self.to != self.glide_target
    }

    /// A knob's new age: crossfade there, from the next sample or once the
    /// move under way ends. Of several changes before then, the last wins.
    pub fn set_target(&mut self, age: f32) {
        self.waiting = Some(age);
    }

    /// A modulator's new age: glide there within `samples` samples (or as
    /// soon as the slope allows).
    pub fn glide_to(&mut self, age: f32, samples: usize) {
        self.waiting = None;
        self.glide_target = age;
        self.glide_left = samples.max(1) as u32;
    }

    /// Go straight to `age`, abandoning any move.
    pub fn jump(&mut self, age: f32) {
        self.from = age;
        self.to = age;
        self.fade = 1.0;
        self.waiting = None;
        self.glide_target = age;
        self.glide_left = 0;
    }

    /// Whether it's still moving, or has a move waiting.
    pub fn is_moving(&self) -> bool {
        self.fade < 1.0 || self.gliding() || self.waiting.is_some()
    }

    /// The age it reads at while it holds still, when [`Tap::advance`]
    /// would change nothing and read only there.
    pub fn still_at(&self) -> Option<f32> {
        (!self.is_moving()).then_some(self.to)
    }

    /// Where to read this sample; call once per sample.
    #[inline]
    pub fn advance(&mut self) -> TapRead {
        if self.gliding() {
            let left = self.glide_target - self.to;
            let step =
                (left / self.glide_left.max(1) as f32).clamp(-self.max_slope, self.max_slope);
            self.glide_left = self.glide_left.saturating_sub(1);
            if step.abs() >= left.abs() {
                self.to = self.glide_target;
            } else {
                self.to += step;
            }
        }
        if self.fade >= 1.0
            && !self.gliding()
            && let Some(age) = self.waiting.take()
            && age != self.to
        {
            self.from = self.to;
            self.to = age;
            self.glide_target = age;
            self.fade = 0.0;
        }
        if self.fade < 1.0 {
            self.fade += self.fade_step;
            if self.fade >= 1.0 {
                self.fade = 1.0;
                self.from = self.to;
            }
        }
        TapRead {
            from: self.from,
            to: self.to,
            fade: self.fade,
        }
    }
}

/// [`dsp::catmull_rom`] four at a time, the same operations in the same
/// order, so each lane is what it would give.
#[inline(always)]
pub(crate) fn catmull_rom_x4(x0: F32x4, x1: F32x4, x2: F32x4, x3: F32x4, t: F32x4) -> F32x4 {
    let half = F32x4::splat(0.5);
    let c1 = half * (x2 - x0);
    let c2 = x0 - F32x4::splat(2.5) * x1 + F32x4::splat(2.0) * x2 - half * x3;
    let c3 = half * (x3 - x0) + F32x4::splat(1.5) * (x1 - x2);
    ((c3 * t + c2) * t + c1) * t + x1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_still_tap_read_a_block_at_a_time_reads_as_sample_by_sample() {
        let mut state = 3u32;
        let mut noise = || dsp::xorshift32(&mut state) as f32 / u32::MAX as f32 - 0.5;
        let signal: Vec<f32> = (0..3000).map(|_| noise()).collect();
        for age in [0.0, 0.5, 1.0, 1.25, 7.0, 40.75, 95.5, 97.0, 98.999] {
            for block in [1, 3, 4, 7, 32, 33] {
                let (mut one, mut many) = (Ring::default(), Ring::default());
                one.allocate(100 + 32);
                many.allocate(100 + 32);
                let expected: Vec<f32> = signal
                    .iter()
                    .map(|&x| {
                        one.push(x);
                        one.get_between(age)
                    })
                    .collect();
                let mut got = vec![0.0; signal.len()];
                for (x, out) in signal.chunks(block).zip(got.chunks_mut(block)) {
                    many.push_and_read(x, age, out);
                }
                assert!(
                    expected.iter().map(|x| x.to_bits()).eq(got.iter().map(|x| x.to_bits())),
                    "age {age}, blocks of {block}"
                );
            }
        }
    }

    #[test]
    fn ring_reads_back_by_age_and_clears_instantly() {
        let mut ring = Ring::default();
        ring.allocate(4);
        for x in 1..=6 {
            ring.push(x as f32);
        }
        assert_eq!(
            [ring.get(0), ring.get(1), ring.get(2), ring.get(3)],
            [6.0, 5.0, 4.0, 3.0]
        );
        assert_eq!(ring.get(4), 0.0, "beyond the buffer");
        ring.clear();
        assert_eq!(ring.get(0), 0.0);
        ring.push(7.0);
        assert_eq!([ring.get(0), ring.get(1)], [7.0, 0.0]);
    }

    #[test]
    fn crossfade_moves_monotonically_to_the_new_tap() {
        let mut ring = Ring::default();
        ring.allocate(16);
        for x in 0..16 {
            ring.push(x as f32);
        }
        let mut tap = Crossfade::new(0);
        tap.set_fade_length(4);
        tap.set_target(8);
        let values: Vec<f32> = (0..6).map(|_| tap.advance().read(&ring)).collect();
        assert_eq!(values, [13.0, 11.0, 9.0, 7.0, 7.0, 7.0]);
    }

    #[test]
    fn a_tap_crossfades_for_a_knob_and_glides_for_a_modulator() {
        let mut ring = Ring::default();
        ring.allocate(64);
        for x in 0..64 {
            ring.push(x as f32);
        }
        // Whole ages read exactly; between them, the line through a ramp.
        assert_eq!(ring.get_between(3.0), 60.0);
        assert!((ring.get_between(3.25) - 59.75).abs() < 1e-5);

        // A knob: a crossfade over four samples, as Crossfade does.
        let mut tap = Tap::new(0.0);
        tap.set_fade_length(4);
        tap.set_target(8.0);
        let values: Vec<f32> = (0..6).map(|_| tap.advance().read(&ring)).collect();
        assert_eq!(values, [61.0, 59.0, 57.0, 55.0, 55.0, 55.0]);

        // A modulator: a glide over four samples, a straight line of
        // ages, never more than the slope allows.
        tap.glide_to(10.0, 4);
        let ages: Vec<f32> = (0..4).map(|_| tap.advance().age()).collect();
        assert_eq!(ages, [8.5, 9.0, 9.5, 10.0]);
        tap.set_max_slope(0.5);
        tap.glide_to(20.0, 4);
        let ages: Vec<f32> = (0..6).map(|_| tap.advance().age()).collect();
        assert_eq!(ages, [10.5, 11.0, 11.5, 12.0, 12.5, 13.0]);
        assert!(tap.is_moving());
        // A knob's change waits for the glide to arrive, then crossfades.
        tap.set_target(2.0);
        let reads: Vec<TapRead> = (0..16).map(|_| tap.advance()).collect();
        let fading = reads.iter().position(|r| r.fade < 1.0).unwrap();
        assert!(reads[..fading].windows(2).all(|w| w[1].to > w[0].to));
        let read = reads[fading];
        assert_eq!((read.from, read.to, read.fade), (20.0, 2.0, 0.25));
    }
}

/// A Schroeder allpass on a [`Ring`], `delay` samples long (between samples
/// if need be), for lengths that move: `w = x + g·w[n-D]`, `y = w[n-D] - g·w`.
#[inline]
pub fn ring_allpass(ring: &mut Ring, x: f32, gain: f32, delay: f32) -> f32 {
    let delayed = ring.delayed(delay);
    let w = x + gain * delayed;
    ring.push(dsp::flush(w));
    delayed - gain * w
}
