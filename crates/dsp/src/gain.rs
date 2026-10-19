use crate::{Processor, Smoothed, db_to_gain};

/// Stereo gain stage with smoothed level changes.
#[derive(Debug, Clone)]
pub struct Gain {
    level: Smoothed,
}

impl Gain {
    pub fn new(db: f32) -> Self {
        Self {
            level: Smoothed::new(db_to_gain(db), 20.0),
        }
    }

    pub fn set_db(&mut self, db: f32) {
        self.level.set_target(db_to_gain(db));
    }
}

impl Processor for Gain {
    fn prepare(&mut self, sample_rate: f32) {
        self.level.prepare(sample_rate);
    }

    fn reset(&mut self) {
        self.level.settle();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let g = self.level.tick();
            *l *= g;
            *r *= g;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_gain_after_ramp() {
        let mut gain = Gain::new(0.0);
        gain.prepare(1000.0);
        gain.set_db(-6.0);
        let mut left = [1.0; 64];
        let mut right = [1.0; 64];
        gain.process(&mut left, &mut right);
        assert!((left[63] - 0.501).abs() < 1e-3);
        assert_eq!(left, right);
    }
}
