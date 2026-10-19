//! Plugin parameters as the host sees them, the same for every format.
//!
//! Values are in each format's host-side units, between [`ParamInfo::min`]
//! and [`ParamInfo::max`]: a CLAP parameter's plain value ("-6" for -6 dB),
//! a VST3 parameter's normalized one (always 0 to 1; VST3 hosts never see
//! plain values), or a VST2 parameter's normalized one (0 to 1 too, kept
//! by the plugin as a 32-bit float, so values read back are only that
//! precise). Stepped parameters only take [`ParamInfo::step_count`] + 1
//! values. The plugin turns values into text:
//! [`crate::PluginInstance::param_text`] (a VST2 plugin only its current
//! value).

use serde::{Deserialize, Serialize};

/// A parameter's id, stable across sessions (save it in projects).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ParamId(pub u32);

/// What kind of control a parameter is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ParamFlags {
    /// Only takes whole steps (a switch, a choice, a semitone count).
    pub stepped: bool,
    /// The plugin names its steps (an enum or a list).
    pub enumerated: bool,
    /// The host may automate it.
    pub automatable: bool,
    /// The host may modulate it without moving it (CLAP's `PARAM_MOD`; no
    /// VST3 or VST2 parameter is).
    pub modulatable: bool,
    /// Wraps around, like a phase.
    pub periodic: bool,
    /// The plugin shows it but the host can't change it (a meter).
    pub read_only: bool,
    /// Not to be shown in lists of parameters.
    pub hidden: bool,
    /// The plugin's own bypass switch.
    pub bypass: bool,
}

/// A parameter's description.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    pub id: ParamId,
    pub name: String,
    /// Where the plugin groups it, outermost first, separated by "/"
    /// ("Osc 1/Shape"): a CLAP parameter's module, a VST3 parameter's unit,
    /// a VST2 parameter's category. Empty for the top level.
    pub module: String,
    /// The unit the plugin names, if it does (VST3 and VST2: "dB"; CLAP has
    /// none and puts units in [`crate::PluginInstance::param_text`]).
    pub unit: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// How many steps a stepped parameter's range has (a switch: 1, four
    /// choices: 3); 0 for a continuous one.
    pub step_count: u32,
    pub flags: ParamFlags,
}

impl ParamInfo {
    /// `value` in range, on a step for stepped parameters; the default for
    /// NaN.
    pub fn clamp(&self, value: f64) -> f64 {
        if value.is_nan() {
            return self.default;
        }
        let (low, high) = (self.min.min(self.max), self.max.max(self.min));
        let value = value.clamp(low, high);
        if self.step_count > 0 && high > low {
            let step = (high - low) / f64::from(self.step_count);
            low + ((value - low) / step).round() * step
        } else {
            value
        }
    }

    /// Maps a value onto `0..=1`, linearly.
    pub fn to_normalized(&self, value: f64) -> f64 {
        if self.max <= self.min {
            return 0.0;
        }
        (self.clamp(value) - self.min) / (self.max - self.min)
    }

    /// The inverse of [`Self::to_normalized`].
    pub fn from_normalized(&self, normalized: f64) -> f64 {
        let t = if normalized.is_nan() {
            0.0
        } else {
            normalized.clamp(0.0, 1.0)
        };
        self.clamp(self.min + (self.max - self.min) * t)
    }

    /// The value of step `index` of a stepped parameter (the first is
    /// `min`), for listing its choices.
    pub fn step_value(&self, index: u32) -> f64 {
        if self.step_count == 0 {
            return self.min;
        }
        let t = f64::from(index.min(self.step_count)) / f64::from(self.step_count);
        self.min + (self.max - self.min) * t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(min: f64, max: f64, step_count: u32) -> ParamInfo {
        ParamInfo {
            id: ParamId(1),
            name: "P".into(),
            module: String::new(),
            unit: String::new(),
            min,
            max,
            default: min,
            step_count,
            flags: ParamFlags {
                stepped: step_count > 0,
                ..ParamFlags::default()
            },
        }
    }

    #[test]
    fn normalizes_linearly_and_snaps_to_steps() {
        let gain = param(-60.0, 12.0, 0);
        assert_eq!(gain.to_normalized(-60.0), 0.0);
        assert_eq!(gain.to_normalized(12.0), 1.0);
        assert!((gain.from_normalized(gain.to_normalized(-6.0)) + 6.0).abs() < 1e-12);
        assert_eq!(gain.clamp(100.0), 12.0);
        assert_eq!(gain.clamp(f64::NAN), -60.0);

        // A CLAP choice: plain values 0 to 3.
        let choice = param(0.0, 3.0, 3);
        assert_eq!(choice.from_normalized(0.4), 1.0);
        assert_eq!(choice.clamp(2.6), 3.0);
        assert_eq!(choice.step_value(2), 2.0);

        // A VST3 choice: normalized, steps a third apart.
        let list = param(0.0, 1.0, 3);
        assert!((list.clamp(0.3) - 1.0 / 3.0).abs() < 1e-12);
        assert!((list.step_value(2) - 2.0 / 3.0).abs() < 1e-12);
    }
}
