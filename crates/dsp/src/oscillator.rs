use std::f32::consts::TAU;

use crate::Smoothed;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Waveform {
    #[default]
    Sine,
    Saw,
    Square,
}

/// How quickly a pitch wheel move takes effect: enough to keep the
/// controller's steps from zippering.
const BEND_RAMP_MS: f32 = 5.0;

/// Band-limited oscillator. Saw and square use PolyBLEP, which removes most
/// of the aliasing a naive saw produces on high notes.
#[derive(Debug, Clone)]
pub struct Oscillator {
    waveform: Waveform,
    /// Position in the current cycle, 0..1.
    phase: f32,
    frequency: Smoothed,
    /// A multiplier on the frequency, for pitch bends.
    ratio: Smoothed,
    sample_rate: f32,
}

impl Oscillator {
    pub fn new(waveform: Waveform, frequency: f32) -> Self {
        Self {
            waveform,
            phase: 0.0,
            frequency: Smoothed::new(frequency, 20.0),
            ratio: Smoothed::new(1.0, BEND_RAMP_MS),
            sample_rate: 44_100.0,
        }
    }

    pub fn prepare(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.frequency.prepare(sample_rate);
        self.ratio.prepare(sample_rate);
    }

    /// Multiplies the frequency by `ratio`, smoothly: a pitch wheel. Kept
    /// apart from the frequency, so notes and glides don't reset it.
    pub fn set_pitch_ratio(&mut self, ratio: f32) {
        self.ratio.set_target(ratio.clamp(0.01, 100.0));
    }

    /// Like [`Self::set_pitch_ratio`] but at once, for a note starting.
    pub fn set_pitch_ratio_immediate(&mut self, ratio: f32) {
        self.ratio.set_immediate(ratio.clamp(0.01, 100.0));
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    pub fn set_frequency(&mut self, hz: f32) {
        // Above Nyquist the phase would advance more than a full cycle per sample.
        self.frequency
            .set_target(hz.clamp(0.0, self.sample_rate * 0.49));
    }

    /// How long `set_frequency` takes to glide to a new pitch. 20 ms by
    /// default, just enough to avoid clicks.
    pub fn set_glide(&mut self, ms: f32) {
        self.frequency.set_ramp(ms, self.sample_rate);
    }

    /// Jump to `hz` with no glide, e.g. when a note starts from silence.
    pub fn set_frequency_immediate(&mut self, hz: f32) {
        self.frequency
            .set_immediate(hz.clamp(0.0, self.sample_rate * 0.49));
    }

    /// Jump to `phase` in the cycle (`0..1`, wrapped). Lets detuned or
    /// polyphonic oscillators start out of step, as free-running analog ones do.
    pub fn set_phase(&mut self, phase: f32) {
        self.phase = phase.rem_euclid(1.0);
    }

    pub fn set_waveform(&mut self, waveform: Waveform) {
        self.waveform = waveform;
    }

    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let hz = (self.frequency.tick() * self.ratio.tick()).min(self.sample_rate * 0.49);
        let dt = hz / self.sample_rate;
        let t = self.phase;
        let out = match self.waveform {
            Waveform::Sine => (t * TAU).sin(),
            Waveform::Saw => 2.0 * t - 1.0 - poly_blep(t, dt),
            Waveform::Square => {
                let naive = if t < 0.5 { 1.0 } else { -1.0 };
                naive + poly_blep(t, dt) - poly_blep((t + 0.5).fract(), dt)
            }
        };
        self.phase += dt;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        out
    }

    pub fn fill(&mut self, out: &mut [f32]) {
        for sample in out {
            *sample = self.next_sample();
        }
    }
}

/// PolyBLEP correction for a step of +2 in a naive waveform at phase 0 (a
/// saw's or square's jump), applied within one sample of the step, which
/// keeps its harmonics from folding back below Nyquist as audible hash.
#[inline]
pub fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let t = t / dt;
        2.0 * t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + 2.0 * t + 1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(waveform: Waveform, hz: f32, samples: usize) -> Vec<f32> {
        let mut osc = Oscillator::new(waveform, hz);
        osc.prepare(44_100.0);
        let mut out = vec![0.0; samples];
        osc.fill(&mut out);
        out
    }

    #[test]
    fn sine_has_expected_frequency() {
        // One second of 1 kHz crosses zero going upward 1000 times.
        let out = render(Waveform::Sine, 1000.0, 44_100);
        let upward_crossings = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!(
            (999..=1001).contains(&upward_crossings),
            "got {upward_crossings}"
        );
    }

    #[test]
    fn waveforms_stay_in_range_and_centered() {
        for waveform in [Waveform::Sine, Waveform::Saw, Waveform::Square] {
            let out = render(waveform, 440.0, 44_100);
            let max = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            let mean = out.iter().sum::<f32>() / out.len() as f32;
            assert!(max <= 1.1, "{waveform:?} peaks at {max}");
            assert!(mean.abs() < 0.01, "{waveform:?} has DC offset {mean}");
        }
    }

    #[test]
    fn set_phase_jumps_within_the_cycle() {
        let mut osc = Oscillator::new(Waveform::Sine, 100.0);
        osc.prepare(44_100.0);
        osc.set_phase(1.25);
        assert!((osc.next_sample() - 1.0).abs() < 1e-6, "a quarter cycle in");
        osc.set_phase(-0.25);
        assert!((osc.next_sample() + 1.0).abs() < 1e-6, "three quarters in");
    }
}
