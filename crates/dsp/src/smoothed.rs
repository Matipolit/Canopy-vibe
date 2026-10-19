//! [`Smoothed`]: a parameter that glides to new values instead of jumping,
//! which avoids the "zipper" crackle you'd hear when a knob moves while
//! audio plays. A linear ramp, so it always arrives exactly on time.
//!
//! A value glides either at its own pace ([`Smoothed::set_target`], the
//! knob's, set when it's made) or within [`MAX_BLOCK`] samples
//! ([`Smoothed::modulate`]), as a modulator's change must. Retargeted once
//! a block, the second follows the modulator as straight lines between the
//! values it sends, one block behind: no stair steps, however fast it
//! moves.

/// Frames in the engine's longest block, and so the longest a modulated
/// value may take to arrive.
pub const MAX_BLOCK: usize = 64;

/// How fast a parameter change should land: at the knob's pace, or within
/// a block, as a modulator's must.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    Knob,
    Modulation,
}

#[derive(Debug, Clone)]
pub struct Smoothed {
    current: f32,
    target: f32,
    step: f32,
    steps_left: u32,
    ramp_ms: f32,
    /// The knob's ramp, and a modulator's, in ticks.
    ramp_ticks: u32,
    quick_ticks: u32,
}

impl Smoothed {
    pub fn new(value: f32, ramp_ms: f32) -> Self {
        Self {
            current: value,
            target: value,
            step: 0.0,
            steps_left: 0,
            ramp_ms,
            ramp_ticks: 1,
            quick_ticks: 1,
        }
    }

    /// For a value ticked once a sample at `sample_rate`.
    pub fn prepare(&mut self, sample_rate: f32) {
        self.prepare_every(sample_rate, 1);
    }

    /// For a value ticked once every `chunk` samples at `sample_rate`.
    pub fn prepare_every(&mut self, sample_rate: f32, chunk: usize) {
        let chunk = chunk.max(1);
        let rate = sample_rate / chunk as f32;
        self.ramp_ticks = (rate * self.ramp_ms / 1000.0).round().max(1.0) as u32;
        self.quick_ticks = ((MAX_BLOCK / chunk).max(1) as u32).min(self.ramp_ticks);
    }

    /// Changes how long a ramp takes, from the next `set_target` on.
    pub fn set_ramp(&mut self, ramp_ms: f32, sample_rate: f32) {
        self.ramp_ms = ramp_ms.max(0.0);
        self.prepare(sample_rate);
    }

    /// Glide to `target` at the knob's pace.
    #[inline]
    pub fn set_target(&mut self, target: f32) {
        self.ramp(target, self.ramp_ticks);
    }

    /// Glide to `target` within a block (or sooner, if the knob's own
    /// glide is shorter still).
    #[inline]
    pub fn modulate(&mut self, target: f32) {
        self.ramp(target, self.quick_ticks);
    }

    /// [`Self::set_target`] or [`Self::modulate`], as `pace` says.
    #[inline]
    pub fn move_to(&mut self, target: f32, pace: Pace) {
        match pace {
            Pace::Knob => self.set_target(target),
            Pace::Modulation => self.modulate(target),
        }
    }

    #[inline]
    fn ramp(&mut self, target: f32, ticks: u32) {
        self.target = target;
        self.steps_left = ticks;
        self.step = (target - self.current) / ticks as f32;
    }

    /// Jump straight to `value` with no ramp.
    #[inline]
    pub fn set_immediate(&mut self, value: f32) {
        self.current = value;
        self.target = value;
        self.steps_left = 0;
    }

    /// Jump straight to the target, finishing any glide.
    #[inline]
    pub fn settle(&mut self) {
        self.set_immediate(self.target);
    }

    /// Advance one tick and return the new value.
    #[inline]
    pub fn tick(&mut self) -> f32 {
        if self.steps_left > 0 {
            self.steps_left -= 1;
            self.current = if self.steps_left == 0 {
                self.target
            } else {
                self.current + self.step
            };
        }
        self.current
    }

    #[inline]
    pub fn current(&self) -> f32 {
        self.current
    }

    #[inline]
    pub fn target(&self) -> f32 {
        self.target
    }

    #[inline]
    pub fn is_smoothing(&self) -> bool {
        self.steps_left > 0
    }

    /// Whether it is moving, or resting anywhere but at `rest`: whether
    /// whatever it controls has anything to do.
    #[inline]
    pub fn is_away_from(&self, rest: f32) -> bool {
        self.is_smoothing() || self.target != rest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaches_target_exactly_after_ramp() {
        let mut s = Smoothed::new(0.0, 10.0);
        s.prepare(1000.0); // 10 ms at 1 kHz = 10 samples
        s.set_target(1.0);
        let values: Vec<f32> = (0..10).map(|_| s.tick()).collect();
        assert!(
            values.windows(2).all(|w| w[1] > w[0]),
            "ramp should rise monotonically"
        );
        assert_eq!(values[9], 1.0);
        assert!(!s.is_smoothing());
        assert_eq!(s.tick(), 1.0);
    }

    #[test]
    fn retargeting_mid_ramp_starts_from_current_value() {
        let mut s = Smoothed::new(0.0, 10.0);
        s.prepare(1000.0);
        s.set_target(1.0);
        for _ in 0..5 {
            s.tick();
        }
        let midway = s.current();
        s.set_target(0.0);
        assert!(s.tick() < midway);
    }

    #[test]
    fn the_knob_takes_its_time_and_a_modulator_a_block() {
        let mut s = Smoothed::new(0.0, 20.0);
        s.prepare(48_000.0);
        s.set_target(1.0);
        let knob: Vec<f32> = (0..960).map(|_| s.tick()).collect();
        assert!(knob.windows(2).all(|w| w[1] > w[0]));
        assert_eq!(knob[959], 1.0);
        assert!(knob[958] < 1.0);

        s.modulate(0.0);
        let quick: Vec<f32> = (0..MAX_BLOCK).map(|_| s.tick()).collect();
        assert_eq!(quick[MAX_BLOCK - 1], 0.0);
        assert!(
            quick[MAX_BLOCK - 2] > 0.0,
            "arrives at the block's end, not before"
        );
        assert!(!s.is_smoothing());

        // Ticked once a chunk, a block is fewer ticks.
        let mut chunked = Smoothed::new(0.0, 20.0);
        chunked.prepare_every(48_000.0, 16);
        chunked.modulate(1.0);
        let ticks = (0..).take_while(|_| chunked.tick() < 1.0).count() + 1;
        assert_eq!(ticks, MAX_BLOCK / 16);

        // Never slower than the knob's own glide.
        let mut short = Smoothed::new(0.0, 0.5);
        short.prepare(48_000.0);
        short.modulate(1.0);
        assert_eq!((0..24).map(|_| short.tick()).last(), Some(1.0));
    }

    #[test]
    fn retargeting_every_block_draws_straight_lines() {
        // A modulator's steps, one a block: the value passes through each
        // one a block later, in a straight line from the one before.
        let mut s = Smoothed::new(0.0, 20.0);
        s.prepare(48_000.0);
        let steps = [0.25, 1.0, 0.5, -0.5];
        let mut previous = 0.0;
        for &step in &steps {
            s.modulate(step);
            for i in 1..=MAX_BLOCK {
                let expected = previous + (step - previous) * i as f32 / MAX_BLOCK as f32;
                assert!((s.tick() - expected).abs() < 1e-5);
            }
            previous = step;
        }
    }
}
