//! Devices saved by an older version of an effect, and what points at them,
//! brought up to date when a project (or a rack preset) loads, so old
//! files sound as they did.
//!
//! The Reverb until version 2.0 had one `mix` (`0..=100`), a crossfade from
//! dry to wet; from 2.0 it has separate `dry` (`0..=100`) and `wet`
//! (`0..=200`) levels. A mix of `m` is a dry of `100 - m` and a wet of `m`.
//! Automation of the mix becomes a lane for each; a modulation, macro or
//! controller on it moves the wet.
//!
//! Other parameters' ranges have grown ([`RESCALES`]): the Reverb's low cut
//! reached 1000 Hz and from 2.0 reaches 2000 (on the same log scale from
//! 20 Hz); its width reached 100% and from 3.0 reaches 200%; the EQ's
//! bands' Q started at 0.1 and from 2.0 at 0.02 (on the same log scale up
//! to 18). Settings are in the parameters' own units and need nothing, but
//! automation, modulation and macros move a knob in its `0..=1`, so those
//! are mapped to keep the same values. A MIDI controller sweeps the whole
//! knob, now a wider range.

use std::collections::{BTreeMap, HashMap};

use crate::automation::{AutomationLane, AutomationPoint, AutomationTarget};
use crate::patch::walk_mut;
use crate::{Device, DeviceId, Project};

const REVERB: &str = "canopy.reverb";
const EQ: &str = "canopy.eq";
const DUCKER: &str = "canopy.ducker";
/// The Reverb's mix before 2.0, as the file said; 25 was its default.
const OLD_MIX: &str = "mix";
const OLD_MIX_DEFAULT: f32 = 25.0;
/// The wet level's top, against the old mix's 100: automation's and
/// modulation's normalized values halve.
const WET_TOP: f32 = 200.0;

/// A parameter whose range grew in `version` of `effect`: a knob position
/// `x` (`0..=1`) on the old range is `offset + scale · x` on the new one,
/// and a modulation's depth (a share of the range) `scale` times as much.
#[derive(Debug)]
pub struct Rescale {
    effect: &'static str,
    version: u32,
    param: fn(&str) -> bool,
    offset: f32,
    scale: f32,
}

impl Rescale {
    fn position(&self, x: f32) -> f32 {
        (self.offset + self.scale * x).clamp(0.0, 1.0)
    }
}

fn is_low_cut(param: &str) -> bool {
    param == "low_cut"
}

fn is_width(param: &str) -> bool {
    param == "width"
}

fn is_mode(param: &str) -> bool {
    param == "mode"
}

/// Any of the EQ's bands' Q (`b1_q` to `b24_q`).
fn is_band_q(param: &str) -> bool {
    param
        .strip_prefix('b')
        .and_then(|rest| rest.strip_suffix("_q"))
        .is_some_and(|n| n.parse::<u8>().is_ok())
}

/// Every range that has grown, oldest first (see the module's notes).
pub const RESCALES: &[Rescale] = &[
    // Three old trigger modes now occupy the first three of four positions.
    Rescale {
        effect: DUCKER,
        version: 2,
        param: is_mode,
        offset: 0.0,
        scale: 2.0 / 3.0,
    },
    // `ln(1000 / 20) / ln(2000 / 20)`.
    Rescale {
        effect: REVERB,
        version: 2,
        param: is_low_cut,
        offset: 0.0,
        scale: 0.849_485,
    },
    Rescale {
        effect: REVERB,
        version: 3,
        param: is_width,
        offset: 0.0,
        scale: 0.5,
    },
    // From `0.1..=18` to `0.02..=18`, log: `ln(0.1 / 0.02) / ln(900)` and
    // `ln(180) / ln(900)`.
    Rescale {
        effect: EQ,
        version: 2,
        param: is_band_q,
        offset: 0.236_601_4,
        scale: 0.763_398_6,
    },
];

fn major(version: &str) -> Option<u32> {
    version
        .split('.')
        .next()
        .and_then(|n| n.trim().parse::<u32>().ok())
}

fn version_below(version: &str, major_version: u32) -> bool {
    major(version).is_none_or(|n| n < major_version)
}

/// What changed on one device: the ranges it was saved with that have
/// grown since, and whether its mix became two levels.
#[derive(Debug, Default)]
struct Changed {
    rescales: Vec<&'static Rescale>,
    split_mix: bool,
}

impl Changed {
    fn rescale(&self, param: &str) -> Option<&'static Rescale> {
        self.rescales.iter().copied().find(|r| (r.param)(param))
    }
}

type ChangedDevices = HashMap<DeviceId, Changed>;

/// What's out of date on `device`, if anything, and brings its settings
/// and version up to date.
fn update(device: &mut Device) -> Option<Changed> {
    if !device.effect.format.is_builtin() {
        return None;
    }
    let id = device.effect.id.as_str();
    let current = effects::find_builtin(id)?.version;
    let saved = device.effect.version.as_str();
    let changed = Changed {
        rescales: RESCALES
            .iter()
            .filter(|r| r.effect == id && version_below(saved, r.version))
            .collect(),
        split_mix: id == REVERB && version_below(saved, 2),
    };
    if changed.rescales.is_empty() && !changed.split_mix {
        return None;
    }
    if changed.split_mix {
        reverb_params(&mut device.params);
    }
    device.effect.version = current.to_owned();
    Some(changed)
}

/// Brings devices (and what's inside them) up to date, and the macros of
/// Patchers among them on what changed. Returns what changed, for what
/// else points at those devices.
fn devices(devices: &mut [Device]) -> ChangedDevices {
    let mut changed = ChangedDevices::new();
    walk_mut(devices, &mut |device| {
        if let Some(c) = update(device) {
            changed.insert(device.id, c);
        }
    });
    if changed.is_empty() {
        return changed;
    }
    walk_mut(devices, &mut |device| {
        if let Some(patch) = &mut device.patch {
            links(patch, &changed);
        }
    });
    changed
}

/// A Patcher's effects, and its macros on their old parameters.
pub fn patch(patch: &mut crate::Patch) {
    let changed = devices(&mut patch.devices);
    links(patch, &changed);
}

/// A Patcher's macros on changed parameters.
fn links(patch: &mut crate::Patch, changed: &ChangedDevices) {
    for link in &mut patch.links {
        let Some(c) = changed.get(&link.device) else {
            continue;
        };
        if c.split_mix && link.param == OLD_MIX {
            link.param = "wet".to_owned();
            link.min /= 2.0;
            link.max /= 2.0;
        } else if let Some(r) = c.rescale(&link.param) {
            link.min = r.position(link.min);
            link.max = r.position(link.max);
        }
    }
}

/// An old Reverb's settings with its mix as the two levels.
pub fn reverb_params(params: &mut BTreeMap<String, f32>) {
    let mix = params
        .remove(OLD_MIX)
        .unwrap_or(OLD_MIX_DEFAULT)
        .clamp(0.0, 100.0);
    params.insert("dry".to_owned(), 100.0 - mix);
    params.insert("wet".to_owned(), mix);
}

/// A device preset's settings for `effect`, saved at `version`, brought up
/// to date. (Settings are in units, so only the Reverb's mix needs it.)
pub fn preset_params(effect: &str, version: Option<&str>, params: &mut BTreeMap<String, f32>) {
    let old = version.is_none_or(|v| version_below(v, 2));
    let levels = params.contains_key("dry") || params.contains_key("wet");
    if effect == REVERB && old && !levels {
        reverb_params(params);
    }
}

/// The version a rack preset's effect is brought up to: its current one,
/// for a built-in whose saved settings [`preset_params`] has updated.
pub fn preset_version(effect: &str, version: &mut String) {
    if (effect == REVERB || RESCALES.iter().any(|r| r.effect == effect))
        && let Some(descriptor) = effects::find_builtin(effect)
    {
        *version = descriptor.version.to_owned();
    }
}

/// Every device in `project`, and the lanes, routes and links on their
/// changed parameters.
pub(crate) fn project(project: &mut Project) {
    let mut changed = devices(&mut project.master_devices);
    project.visit_mut(|track| {
        let mine = devices(&mut track.devices);
        if mine.is_empty() {
            return;
        }
        lanes(&mut track.automation, &mine);
        for route in &mut track.modulations {
            let AutomationTarget::DeviceParam { device, param } = &route.target else {
                continue;
            };
            let Some(c) = mine.get(device) else {
                continue;
            };
            if c.split_mix && param == OLD_MIX {
                route.target = wet(&route.target);
                route.amount /= 2.0;
            } else if let Some(r) = c.rescale(param) {
                route.amount *= r.scale;
            }
        }
        changed.extend(mine);
    });
    if changed.is_empty() {
        return;
    }
    for link in &mut project.controller_links {
        if let AutomationTarget::DeviceParam { device, param } = &link.target
            && param == OLD_MIX
            && changed.get(device).is_some_and(|c| c.split_mix)
        {
            link.target = wet(&link.target);
        }
    }
}

fn wet(target: &AutomationTarget) -> AutomationTarget {
    match target {
        AutomationTarget::DeviceParam { device, .. } => AutomationTarget::DeviceParam {
            device: *device,
            param: "wet".to_owned(),
        },
        other => other.clone(),
    }
}

/// Lanes on changed parameters: rescaled, and a lane on an old mix split
/// into a dry lane and a wet lane.
fn lanes(automation: &mut Vec<AutomationLane>, changed: &ChangedDevices) {
    let mut added = Vec::new();
    for lane in automation.iter_mut() {
        let AutomationTarget::DeviceParam { device, param } = &lane.target else {
            continue;
        };
        let Some(c) = changed.get(device) else {
            continue;
        };
        if let Some(r) = c.rescale(param) {
            let points: Vec<AutomationPoint> = lane
                .points
                .iter()
                .map(|p| AutomationPoint {
                    value: r.position(p.value),
                    ..*p
                })
                .collect();
            lane.points = points.into();
            continue;
        }
        if param != OLD_MIX || !c.split_mix {
            continue;
        }
        let device = *device;
        let scaled = |f: &dyn Fn(f32) -> f32| -> Vec<AutomationPoint> {
            lane.points
                .iter()
                .map(|p| AutomationPoint {
                    value: f(p.value).clamp(0.0, 1.0),
                    ..*p
                })
                .collect()
        };
        let wet_points = scaled(&|v| v * 100.0 / WET_TOP);
        let dry_points = scaled(&|v| 1.0 - v);
        added.push(AutomationLane {
            target: AutomationTarget::DeviceParam {
                device,
                param: "wet".to_owned(),
            },
            points: wet_points.into(),
        });
        *lane = AutomationLane {
            target: AutomationTarget::DeviceParam {
                device,
                param: "dry".to_owned(),
            },
            points: dry_points.into(),
        };
    }
    automation.extend(added);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectRef, NewTrack, TrackPosition};
    use midi::Tick;

    fn old_reverb_project(mix: Option<f32>) -> (Project, DeviceId) {
        let mut p = Project::new();
        let track = p.add_track(NewTrack::Audio, TrackPosition::END).unwrap();
        let device = p
            .add_device(Some(track), EffectRef::builtin(REVERB), None)
            .unwrap();
        let t = p.track_mut(track).unwrap();
        let d = &mut t.devices[0];
        d.effect.version = "1.0".to_owned();
        d.params.clear();
        if let Some(mix) = mix {
            d.params.insert(OLD_MIX.to_owned(), mix);
        }
        t.automation.push(AutomationLane {
            target: AutomationTarget::DeviceParam {
                device,
                param: OLD_MIX.to_owned(),
            },
            points: vec![
                AutomationPoint {
                    tick: Tick::ZERO,
                    value: 0.2,
                    curve: 0.0,
                },
                AutomationPoint {
                    tick: Tick::new(960),
                    value: 0.8,
                    curve: 0.5,
                },
            ]
            .into(),
        });
        (p, device)
    }

    #[test]
    fn an_old_reverbs_mix_becomes_its_dry_and_wet_levels() {
        let (mut p, _) = old_reverb_project(Some(40.0));
        project(&mut p);
        let d = &p.tracks[0].devices[0];
        assert_eq!(d.effect.version, effects::reverb::DESCRIPTOR.version);
        assert_eq!(d.params.get("dry"), Some(&60.0));
        assert_eq!(d.params.get("wet"), Some(&40.0));
        assert!(!d.params.contains_key(OLD_MIX));
    }

    #[test]
    fn an_old_reverb_at_its_default_keeps_the_old_default_mix() {
        let (mut p, _) = old_reverb_project(None);
        project(&mut p);
        let d = &p.tracks[0].devices[0];
        assert_eq!(d.params.get("dry"), Some(&75.0));
        assert_eq!(d.params.get("wet"), Some(&25.0));
    }

    #[test]
    fn automation_of_the_old_mix_moves_both_levels() {
        let (mut p, device) = old_reverb_project(Some(40.0));
        project(&mut p);
        let lanes = &p.tracks[0].automation;
        let lane = |param: &str| {
            lanes
                .iter()
                .find(|l| {
                    l.target
                        == AutomationTarget::DeviceParam {
                            device,
                            param: param.to_owned(),
                        }
                })
                .unwrap_or_else(|| panic!("a {param} lane"))
        };
        let values = |param: &str| {
            lane(param)
                .points
                .iter()
                .map(|p| p.value)
                .collect::<Vec<_>>()
        };
        assert_eq!(values("wet"), [0.1, 0.4]);
        let dry = values("dry");
        assert!(
            (dry[0] - 0.8).abs() < 1e-6 && (dry[1] - 0.2).abs() < 1e-6,
            "{dry:?}"
        );
        assert_eq!(lane("dry").points[1].curve, 0.5);
        assert_eq!(lanes.len(), 2);
    }

    #[test]
    fn automation_of_the_old_low_cut_keeps_its_frequencies() {
        let (mut p, device) = old_reverb_project(Some(40.0));
        let target = AutomationTarget::DeviceParam {
            device,
            param: "low_cut".to_owned(),
        };
        let lane = AutomationLane {
            target: target.clone(),
            points: vec![AutomationPoint {
                tick: Tick::ZERO,
                value: 0.5,
                curve: 0.0,
            }]
            .into(),
        };
        p.tracks[0].automation.push(lane);
        project(&mut p);
        let value = p.tracks[0]
            .automation
            .iter()
            .find(|l| l.target == target)
            .unwrap()
            .points[0]
            .value;
        // Halfway up the old knob: 20 · 50^0.5, about 141 Hz.
        let hz = |v: f32, top: f32| 20.0 * (top / 20.0f32).powf(v);
        assert!((hz(value, 2000.0) - hz(0.5, 1000.0)).abs() < 0.01);
    }

    #[test]
    fn reverbs_already_up_to_date_are_left_alone() {
        let mut p = Project::new();
        let track = p.add_track(NewTrack::Audio, TrackPosition::END).unwrap();
        p.add_device(Some(track), EffectRef::builtin(REVERB), None)
            .unwrap();
        let before = p.clone();
        project(&mut p);
        assert_eq!(p, before);
    }

    /// A project with one `effect` saved at `version`, a lane on `param` at
    /// knob positions `values`, and an LFO on it at depth `amount`.
    fn saved_at(
        effect: &str,
        version: &str,
        param: &str,
        values: &[f32],
        amount: f32,
    ) -> (Project, AutomationTarget) {
        let mut p = Project::new();
        let track = p.add_track(NewTrack::Audio, TrackPosition::END).unwrap();
        let device = p
            .add_device(Some(track), EffectRef::builtin(effect), None)
            .unwrap();
        let target = AutomationTarget::DeviceParam {
            device,
            param: param.to_owned(),
        };
        let t = p.track_mut(track).unwrap();
        t.devices[0].effect.version = version.to_owned();
        t.automation.push(AutomationLane {
            target: target.clone(),
            points: values
                .iter()
                .enumerate()
                .map(|(i, &value)| AutomationPoint {
                    tick: Tick::new(960 * i as i64),
                    value,
                    curve: 0.0,
                })
                .collect::<Vec<_>>()
                .into(),
        });
        let id = p.new_mod_route_id();
        let lfo = p.new_modulator_id();
        p.track_mut(track)
            .unwrap()
            .modulations
            .push(crate::ModRoute {
                id,
                source: crate::ModSource::Modulator { id: lfo, output: 0 },
                target: target.clone(),
                amount,
                polarity: crate::Polarity::Source,
                curve: 0.0,
                scale_by: None,
                bypassed: false,
            });
        (p, target)
    }

    fn lane_values(p: &Project, target: &AutomationTarget) -> Vec<f32> {
        let lane = p.tracks[0]
            .automation
            .iter()
            .find(|l| &l.target == target)
            .unwrap();
        lane.points.iter().map(|p| p.value).collect()
    }

    #[test]
    fn automation_of_a_reverbs_width_keeps_its_values() {
        let (mut p, target) = saved_at(REVERB, "2.0", "width", &[0.0, 0.5, 1.0], 0.4);
        project(&mut p);
        // 0, 50 and 100% on a knob that now reaches 200%.
        assert_eq!(lane_values(&p, &target), [0.0, 0.25, 0.5]);
        assert!((p.tracks[0].modulations[0].amount - 0.2).abs() < 1e-6);
        let d = &p.tracks[0].devices[0];
        assert_eq!(d.effect.version, effects::reverb::DESCRIPTOR.version);
        // Already there: nothing moves.
        let before = p.clone();
        project(&mut p);
        assert_eq!(p, before);
    }

    #[test]
    fn automation_of_an_eq_bands_q_keeps_its_values() {
        let (mut p, target) = saved_at(EQ, "1.0", "b3_q", &[0.0, 0.5, 1.0], 0.5);
        project(&mut p);
        let q = |x: f32, low: f32| low * (18.0 / low).powf(x);
        let values = lane_values(&p, &target);
        for (old, new) in [0.0, 0.5, 1.0].iter().zip(&values) {
            let (before, after) = (q(*old, 0.1), q(*new, 0.02));
            assert!(
                (after / before - 1.0).abs() < 1e-4,
                "{before} became {after}"
            );
        }
        let amount = p.tracks[0].modulations[0].amount;
        assert!((amount - 0.5 * (180f32.ln() / 900f32.ln())).abs() < 1e-5);
        assert_eq!(p.tracks[0].devices[0].effect.version, "2.0");
        // Other parameters' lanes stay as they are.
        let (mut p, target) = saved_at(EQ, "1.0", "b3_gain", &[0.3], 0.5);
        project(&mut p);
        assert_eq!(lane_values(&p, &target), [0.3]);
        assert_eq!(p.tracks[0].modulations[0].amount, 0.5);
    }

    #[test]
    fn only_band_q_parameters_count_as_q() {
        assert!(is_band_q("b1_q") && is_band_q("b24_q"));
        assert!(!is_band_q("b1_gain") && !is_band_q("q") && !is_band_q("bx_q"));
    }
}
