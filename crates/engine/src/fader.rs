use dsp::Smoothed;

/// Ramp time for fader, pan and mute changes.
const RAMP_MS: f32 = 20.0;

/// A channel strip's level, pan and mute, all smoothed so moving them while
/// audio plays doesn't crackle.
#[derive(Debug, Clone)]
pub(crate) struct Fader {
    gain: Smoothed,
    pan: Smoothed,
    /// 1 when the track should be heard, 0 when muted or silenced by a solo.
    gate: Smoothed,
    mute: bool,
    audible: bool,
}

/// The values a fader is set to from the project.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FaderValues {
    pub gain: f32,
    pub pan: f32,
    pub mute: bool,
    /// False when another track is soloed and this one isn't.
    pub audible: bool,
}

impl FaderValues {
    pub const UNITY: FaderValues = FaderValues {
        gain: 1.0,
        pan: 0.0,
        mute: false,
        audible: true,
    };

    pub fn from_mixer(mixer: &project::Mixer, audible: bool) -> FaderValues {
        FaderValues {
            gain: project::volume_gain(mixer.volume_db),
            pan: mixer.pan.clamp(-1.0, 1.0),
            mute: mixer.mute,
            audible,
        }
    }
}

impl Fader {
    /// Starts at `values` with no ramp. Called on the UI thread.
    pub fn new(values: FaderValues, sample_rate: f32) -> Fader {
        let mut fader = Fader {
            gain: Smoothed::new(values.gain, RAMP_MS),
            pan: Smoothed::new(values.pan, RAMP_MS),
            gate: Smoothed::new(0.0, RAMP_MS),
            mute: values.mute,
            audible: values.audible,
        };
        fader.gain.prepare(sample_rate);
        fader.pan.prepare(sample_rate);
        fader.gate.prepare(sample_rate);
        fader.gate.set_immediate(fader.gate_target());
        fader
    }

    fn gate_target(&self) -> f32 {
        if self.mute || !self.audible { 0.0 } else { 1.0 }
    }

    /// Retargets whatever differs from `values`. Anything already heading
    /// there keeps its ramp.
    pub fn set(&mut self, values: FaderValues) {
        self.set_gain(values.gain);
        self.set_pan(values.pan);
        self.mute = values.mute;
        self.audible = values.audible;
        self.update_gate();
    }

    fn update_gate(&mut self) {
        let target = self.gate_target();
        if target != self.gate.target() {
            self.gate.set_target(target);
        }
    }

    pub fn set_gain(&mut self, gain: f32) {
        if gain != self.gain.target() {
            self.gain.set_target(gain);
        }
    }

    pub fn set_pan(&mut self, pan: f32) {
        let pan = pan.clamp(-1.0, 1.0);
        if pan != self.pan.target() {
            self.pan.set_target(pan);
        }
    }

    pub fn gain_target(&self) -> f32 {
        self.gain.target()
    }

    pub fn pan_target(&self) -> f32 {
        self.pan.target()
    }

    pub fn set_mute(&mut self, mute: bool) {
        self.mute = mute;
        self.update_gate();
    }

    /// Applies level, pan and mute in place. Pan is a balance control: center
    /// leaves both sides at full level, and panning turns the other side down.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.gain.is_smoothing() && !self.pan.is_smoothing() && !self.gate.is_smoothing() {
            // Nothing moving, which is nearly always: one pair of gains for
            // the block, the same ones the loop below would find each sample.
            let gain = self.gain.current() * self.gate.current();
            let (pan_l, pan_r) = balance(self.pan.current());
            let (gain_l, gain_r) = (gain * pan_l, gain * pan_r);
            if gain_l == 1.0 && gain_r == 1.0 {
                return;
            }
            for l in left.iter_mut() {
                *l *= gain_l;
            }
            for r in right.iter_mut() {
                *r *= gain_r;
            }
            return;
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let gain = self.gain.tick() * self.gate.tick();
            let pan = self.pan.tick();
            let (pan_l, pan_r) = balance(pan);
            *l *= gain * pan_l;
            *r *= gain * pan_r;
        }
    }
}

/// Left and right levels for a balance position, `-1..=1`.
fn balance(pan: f32) -> (f32, f32) {
    use std::f32::consts::FRAC_PI_2;
    let left = (pan.max(0.0) * FRAC_PI_2).cos();
    let right = ((-pan).max(0.0) * FRAC_PI_2).cos();
    (left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_keeps_center_at_unity() {
        assert_eq!(balance(0.0), (1.0, 1.0));
        let (l, r) = balance(1.0);
        assert!(l.abs() < 1e-6 && r == 1.0);
        let (l, r) = balance(-0.5);
        assert!(l == 1.0 && (r - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
    }

    #[test]
    fn mute_and_solo_ramp_to_silence() {
        let mut fader = Fader::new(FaderValues::UNITY, 1000.0);
        let buf = || ([1.0f32; 40], [1.0f32; 40]);
        let (mut l, mut r) = buf();
        fader.process(&mut l, &mut r);
        assert_eq!(l[39], 1.0);

        fader.set_mute(true);
        let (mut l, mut r) = buf();
        fader.process(&mut l, &mut r);
        assert!(l[0] > 0.0, "ramps rather than cuts");
        assert_eq!(l[39], 0.0);

        fader.set(FaderValues {
            audible: false,
            ..FaderValues::UNITY
        });
        let (mut l, mut r) = buf();
        fader.process(&mut l, &mut r);
        assert_eq!(l[39], 0.0, "not soloed is silent too");
    }
}
