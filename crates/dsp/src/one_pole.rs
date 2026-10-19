//! First-order filters: the trapezoidal one-pole step that low-passes,
//! high-passes and all-passes ([`tpt_lowpass`]), and the DC blocker that
//! takes out what asymmetric curves and feedback loops leave below the
//! audio band ([`DcBlocker`]).

use std::f32::consts::TAU;

use crate::settle;

/// The pole `exp(-2π · hz / sample_rate)` of a one-pole filter with its
/// corner at `hz`.
#[inline]
pub fn pole(hz: f32, sample_rate: f32) -> f32 {
    (-TAU * hz / sample_rate).exp()
}

/// One step of Zavalishin's trapezoidal (TPT) one-pole low-pass, whose
/// integrator is `state`; `gain` is `g / (1 + g)`, `g` the prewarped
/// integrator gain. Returns the low-pass output: the high-pass is the input
/// minus it, the all-pass twice it minus the input.
#[inline(always)]
pub fn tpt_lowpass(state: &mut f32, gain: f32, x: f32) -> f32 {
    let v = (x - *state) * gain;
    let low = v + *state;
    *state = low + v;
    low
}

/// A one-pole DC blocker, `y[n] = x[n] - x[n-1] + pole · y[n-1]`: a
/// high-pass a few hertz up, its corner set by the [`pole`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DcBlocker {
    x1: f32,
    y1: f32,
}

impl DcBlocker {
    #[inline(always)]
    pub fn tick(&mut self, x: f32, pole: f32) -> f32 {
        let y = x - self.x1 + pole * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }

    /// Filters `samples` in place, then ends a decay in exact zeros rather
    /// than denormals.
    pub fn process(&mut self, samples: &mut [f32], pole: f32) {
        let (mut x1, mut y1) = (self.x1, self.y1);
        for sample in samples {
            let x = *sample;
            y1 = x - x1 + pole * y1;
            x1 = x;
            *sample = y1;
        }
        if y1.abs() < 1e-20 {
            y1 = 0.0;
        }
        (self.x1, self.y1) = (x1, y1);
    }

    /// For the end of a block: tiny values, NaN and infinity become zero.
    pub fn flush(&mut self) {
        self.x1 = settle(self.x1);
        self.y1 = settle(self.y1);
    }
}
