use midi::Tick;
use serde::{Deserialize, Serialize};

use crate::{DeviceId, ModulatorId, Shared, TrackId};

/// What an automation lane drives on its track.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AutomationTarget {
    Volume,
    Pan,
    /// An instrument parameter, by its stable id.
    InstrumentParam(String),
    /// A parameter of one of the track's effects.
    DeviceParam {
        device: DeviceId,
        param: String,
    },
    /// A parameter of one of the track's modulators (an LFO's rate, a
    /// macro's value).
    ModulatorParam {
        modulator: ModulatorId,
        param: String,
    },
}

/// Names any automatable value in the project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParamId {
    pub track: TrackId,
    pub target: AutomationTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AutomationPoint {
    pub tick: Tick,
    /// On the `0..=1` scale, whatever the target's own units are.
    pub value: f32,
    /// How the segment from this point to the next bends, `-1..=1`: zero is a
    /// straight line, negative moves fast first, positive slow first. The
    /// same curve as envelope stages and clip fades (`dsp::stage_shape`).
    #[serde(default)]
    pub curve: f32,
}

/// The value `t` of the way (`0..=1`) along a segment from `from` to `to`.
pub fn segment_value(from: f32, to: f32, curve: f32, t: f32) -> f32 {
    from + (to - from) * dsp::stage_shape(t, curve)
}

/// A breakpoint envelope. Values are interpolated linearly between points
/// and held flat before the first point and after the last.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutomationLane {
    pub target: AutomationTarget,
    /// Sorted by tick. Two points may share a tick, making a step.
    pub points: Shared<Vec<AutomationPoint>>,
}

impl AutomationLane {
    pub fn new(target: AutomationTarget) -> AutomationLane {
        AutomationLane {
            target,
            points: Shared::default(),
        }
    }

    pub fn value_at(&self, tick: Tick) -> Option<f32> {
        let after = self.points.partition_point(|p| p.tick <= tick);
        let before = after.checked_sub(1).map(|i| self.points[i]);
        match (before, self.points.get(after).copied()) {
            (None, None) => None,
            (Some(p), None) | (None, Some(p)) => Some(p.value),
            (Some(a), Some(b)) => {
                let span = (b.tick - a.tick).get() as f32;
                let t = (tick - a.tick).get() as f32 / span;
                Some(segment_value(a.value, b.value, a.curve, t))
            }
        }
    }

    /// Restores the sorted order after points were moved, keeping the
    /// relative order of points that share a tick.
    pub fn sort(&mut self) {
        self.points.sort_by_key(|p| p.tick);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(tick: i64, value: f32) -> AutomationPoint {
        AutomationPoint {
            tick: Tick::new(tick),
            value,
            curve: 0.0,
        }
    }

    #[test]
    fn curved_segments_bend() {
        let mut lane = AutomationLane::new(AutomationTarget::Volume);
        lane.points = vec![
            AutomationPoint {
                tick: Tick::ZERO,
                value: 0.0,
                curve: -1.0,
            },
            AutomationPoint {
                tick: Tick::new(100),
                value: 1.0,
                curve: 0.0,
            },
        ]
        .into();
        assert!(lane.value_at(Tick::new(50)).unwrap() > 0.9, "fast first");
        lane.points[0].curve = 1.0;
        assert!(lane.value_at(Tick::new(50)).unwrap() < 0.1, "slow first");
    }

    #[test]
    fn interpolates_and_holds_the_ends() {
        let mut lane = AutomationLane::new(AutomationTarget::Volume);
        assert_eq!(lane.value_at(Tick::ZERO), None);
        lane.points = vec![point(100, 0.0), point(200, 1.0), point(200, 0.5)].into();
        assert_eq!(lane.value_at(Tick::ZERO), Some(0.0));
        assert_eq!(lane.value_at(Tick::new(150)), Some(0.5));
        assert_eq!(
            lane.value_at(Tick::new(200)),
            Some(0.5),
            "a step lands on the later point"
        );
        assert_eq!(lane.value_at(Tick::new(1000)), Some(0.5));
    }
}
