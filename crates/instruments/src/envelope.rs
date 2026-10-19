//! ADSR envelopes as instrument parameters.
//!
//! An instrument lists an envelope's settings among its parameters with the
//! constructors here, so every envelope has the same ranges and names, and
//! says where they are with an [`EnvelopeParams`]. The UI draws any such
//! envelope with one editor, and [`EnvelopeParams::apply`] turns parameter
//! changes into an [`AdsrDesc`] for `dsp::Adsr`.

use dsp::AdsrDesc;

use crate::ParamDescriptor;
use crate::float;

/// Where one envelope's settings sit among an instrument's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeParams {
    /// Shown above the editor, e.g. "Amp" or "Filter".
    pub name: &'static str,
    pub attack: usize,
    pub decay: usize,
    pub sustain: usize,
    pub release: usize,
    /// Attack, decay and release curves, if the envelope has them.
    pub curves: Option<[usize; 3]>,
}

impl EnvelopeParams {
    /// Seven parameters in a row from `first`: attack, decay, sustain,
    /// release, then the attack, decay and release curves.
    pub const fn starting_at(name: &'static str, first: usize) -> EnvelopeParams {
        EnvelopeParams {
            name,
            attack: first,
            decay: first + 1,
            sustain: first + 2,
            release: first + 3,
            curves: Some([first + 4, first + 5, first + 6]),
        }
    }

    /// Every parameter index this envelope uses.
    pub fn indices(&self) -> Vec<usize> {
        let mut indices = vec![self.attack, self.decay, self.sustain, self.release];
        indices.extend(self.curves.iter().flatten());
        indices
    }

    pub fn contains(&self, index: usize) -> bool {
        self.indices().contains(&index)
    }

    /// If `index` is one of this envelope's parameters, sets it in `desc`
    /// and returns true. `value` must already be clamped. Real-time safe.
    pub fn apply(&self, index: usize, value: f32, desc: &mut AdsrDesc) -> bool {
        let curves = self.curves.unwrap_or([usize::MAX; 3]);
        match index {
            i if i == self.attack => desc.attack.time_ms = value,
            i if i == self.decay => desc.decay.time_ms = value,
            i if i == self.sustain => desc.sustain = value,
            i if i == self.release => desc.release.time_ms = value,
            i if i == curves[0] => desc.attack.curve = value,
            i if i == curves[1] => desc.decay.curve = value,
            i if i == curves[2] => desc.release.curve = value,
            _ => return false,
        }
        true
    }

    /// The envelope described by the current parameter values.
    pub fn desc(&self, value: impl Fn(usize) -> f32) -> AdsrDesc {
        let mut desc = AdsrDesc::default();
        for index in self.indices() {
            self.apply(index, value(index), &mut desc);
        }
        desc
    }
}

pub const fn attack(id: &'static str, default_ms: f32) -> ParamDescriptor {
    float(id, "Attack", 0.5, 10_000.0, default_ms, "ms", true)
}

pub const fn decay(id: &'static str, default_ms: f32) -> ParamDescriptor {
    float(id, "Decay", 1.0, 10_000.0, default_ms, "ms", true)
}

pub const fn sustain(id: &'static str, default: f32) -> ParamDescriptor {
    float(id, "Sustain", 0.0, 1.0, default, "", false)
}

pub const fn release(id: &'static str, default_ms: f32) -> ParamDescriptor {
    float(id, "Release", 1.0, 20_000.0, default_ms, "ms", true)
}

/// A stage's bend, `-1..=1`: negative starts fast, positive starts slow.
pub const fn curve(id: &'static str, name: &'static str, default: f32) -> ParamDescriptor {
    float(id, name, -1.0, 1.0, default, "", false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_and_read_back() {
        let layout = EnvelopeParams::starting_at("Amp", 3);
        let values = [0.0, 0.0, 0.0, 12.0, 340.0, 0.25, 900.0, -0.2, 0.4, 0.9];
        let desc = layout.desc(|i| values[i]);
        assert_eq!(desc.attack.time_ms, 12.0);
        assert_eq!(desc.decay.time_ms, 340.0);
        assert_eq!(desc.sustain, 0.25);
        assert_eq!(desc.release.time_ms, 900.0);
        assert_eq!(
            (desc.attack.curve, desc.decay.curve, desc.release.curve),
            (-0.2, 0.4, 0.9)
        );
        let mut other = AdsrDesc::default();
        assert!(
            !layout.apply(1, 5.0, &mut other),
            "not an envelope parameter"
        );
        assert_eq!(layout.indices(), [3, 4, 5, 6, 7, 8, 9]);
    }
}
