//! Patchers: effects patched together inside one effect, as FL Studio's
//! Patcher holds them. A Patcher device (`canopy.patcher`) carries a
//! [`Patch`]: a chain of its own (effects, and a graph when the sound
//! isn't a straight line through them) and links from its eight macro
//! knobs to knobs inside, each over a range of its own.
//!
//! **Chains.** A Patch's chain works as a track's does: everything that
//! edits a chain takes a [`Chain`], which is a track's, the master's, or a
//! Patcher's, and a Patcher may hold another. The automation, controller
//! links and modulation of the effects inside live where those of the
//! effects around them do: on the track the outermost Patcher is on.
//!
//! **Macros.** A macro is the Patcher's own parameter (`macro_1` …
//! `macro_8`, 0–100%), so it automates, links to a controller and takes
//! presets as any knob does. Each link puts its knob at `min` with the
//! macro at 0% and at `max` with it at 100%, in between in a straight line;
//! both are fractions of the knob's range as its slider shows it (log for
//! a frequency), and `min` above `max` turns the knob down as the macro
//! goes up. Setting a macro sets the knobs it drives, through the project,
//! so what they show is what plays; the engine follows a macro's
//! automation the same way, lane by lane (see `engine::arrangement`).
//! Macros drive knobs of the effects directly inside, which can be
//! another Patcher's macros.

use serde::{Deserialize, Serialize};

use crate::edit::EditResult;
use crate::{Device, DeviceId, EditError, Project, Routing, TrackId};

/// The effect id of a Patcher.
pub const PATCHER: &str = "canopy.patcher";

/// How many macros a Patcher has.
pub const MACROS: usize = 8;

/// The id of macro `index` (from 0) as a parameter of its Patcher.
pub fn macro_param(index: usize) -> &'static str {
    const IDS: [&str; MACROS] = [
        "macro_1", "macro_2", "macro_3", "macro_4", "macro_5", "macro_6", "macro_7", "macro_8",
    ];
    IDS[index.min(MACROS - 1)]
}

/// Which macro parameter `id` is, if it's one.
pub fn macro_index(id: &str) -> Option<usize> {
    (0..MACROS).find(|i| macro_param(*i) == id)
}

/// What a Patcher holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    /// Its effects: in sound order without `routing`, else the pool the
    /// graph patches (as a track's `devices`).
    #[serde(default)]
    pub devices: Vec<Device>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<Routing>,
    /// What the macros drive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<MacroLink>,
    /// The macros' names, by index; a missing or empty one is "Macro n".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

impl Patch {
    /// Macro `index`'s name as shown.
    pub fn macro_name(&self, index: usize) -> String {
        match self.names.get(index) {
            Some(name) if !name.trim().is_empty() => name.clone(),
            _ => format!("Macro {}", index + 1),
        }
    }

    /// The macros that drive something, in order.
    pub fn used_macros(&self) -> Vec<usize> {
        (0..MACROS)
            .filter(|i| self.links.iter().any(|l| usize::from(l.index) == *i))
            .collect()
    }
}

/// One knob a macro drives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MacroLink {
    /// Which macro, from 0.
    pub index: u8,
    /// An effect directly in the Patch.
    pub device: DeviceId,
    pub param: String,
    /// Where the knob is, as a fraction of its range, with the macro at 0%.
    pub min: f32,
    /// … and at 100%.
    pub max: f32,
}

impl MacroLink {
    /// Where the knob is, as a fraction of its range, with the macro at
    /// `amount` (0–1).
    pub fn normalized(&self, amount: f32) -> f32 {
        (self.min + amount.clamp(0.0, 1.0) * (self.max - self.min)).clamp(0.0, 1.0)
    }
}

/// An effect chain: a track's, the master's, or the one inside a Patcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Chain {
    Master,
    Track(TrackId),
    /// Inside the Patcher with this id.
    Patch(DeviceId),
}

impl From<Option<TrackId>> for Chain {
    fn from(track: Option<TrackId>) -> Chain {
        match track {
            Some(id) => Chain::Track(id),
            None => Chain::Master,
        }
    }
}

impl From<TrackId> for Chain {
    fn from(track: TrackId) -> Chain {
        Chain::Track(track)
    }
}

/// `devices` and every effect inside the Patchers among them, outer first.
pub fn walk<'a>(devices: &'a [Device], f: &mut impl FnMut(&'a Device)) {
    for device in devices {
        f(device);
        if let Some(patch) = &device.patch {
            walk(&patch.devices, f);
        }
    }
}

/// [`walk`], mutably.
pub fn walk_mut(devices: &mut [Device], f: &mut impl FnMut(&mut Device)) {
    for device in devices {
        f(device);
        if let Some(patch) = &mut device.patch {
            walk_mut(&mut patch.devices, f);
        }
    }
}

/// The ids of `device` and of every effect inside it.
pub fn ids_within(device: &Device) -> Vec<DeviceId> {
    let mut ids = Vec::new();
    walk(std::slice::from_ref(device), &mut |d| ids.push(d.id));
    ids
}

/// Where `id` is among `devices` and inside them: its chain and index.
fn locate_in(devices: &[Device], chain: Chain, id: DeviceId) -> Option<(Chain, usize)> {
    if let Some(index) = devices.iter().position(|d| d.id == id) {
        return Some((chain, index));
    }
    devices.iter().find_map(|d| {
        let patch = d.patch.as_ref()?;
        locate_in(&patch.devices, Chain::Patch(d.id), id)
    })
}

impl crate::Track {
    /// One of the track's effects, inside a Patcher or not.
    pub fn device(&self, id: DeviceId) -> Option<&Device> {
        let mut found = None;
        walk(&self.devices, &mut |d| {
            if d.id == id {
                found = Some(d);
            }
        });
        found
    }
}

impl Project {
    /// Which chain an effect is in, wherever it is, and where in it.
    pub fn locate_device(&self, id: DeviceId) -> Option<(Chain, usize)> {
        if let Some(found) = locate_in(&self.master_devices, Chain::Master, id) {
            return Some(found);
        }
        let mut found = None;
        self.visit(|track, _| {
            if found.is_none() {
                found = locate_in(&track.devices, Chain::Track(track.id), id);
            }
        });
        found
    }

    /// The track (or, `None`, the master) a chain is on, through any
    /// Patchers around it: where its effects' automation and links live.
    /// `None` outside if the chain isn't there.
    pub fn chain_owner(&self, chain: Chain) -> Option<Option<TrackId>> {
        match chain {
            Chain::Master => Some(None),
            Chain::Track(id) => self.track(id).map(|t| Some(t.id)),
            Chain::Patch(patcher) => self.chain_owner(self.locate_device(patcher)?.0),
        }
    }

    /// A chain's effects.
    pub fn chain_devices(&self, chain: Chain) -> Option<&[Device]> {
        match chain {
            Chain::Master => Some(&self.master_devices),
            Chain::Track(id) => self.track(id).map(|t| &t.devices[..]),
            Chain::Patch(patcher) => self.device(patcher)?.patch.as_ref().map(|p| &p.devices[..]),
        }
    }

    /// A Patcher's Patch.
    pub fn patch(&self, patcher: DeviceId) -> Option<&Patch> {
        self.device(patcher)?.patch.as_deref()
    }

    fn patch_mut(&mut self, patcher: DeviceId) -> EditResult<&mut Patch> {
        self.device_mut(patcher)?
            .patch
            .as_deref_mut()
            .ok_or(EditError::NotAPatcher(patcher))
    }

    /// Links macro `index` of a Patcher to parameter `param` of an effect
    /// directly inside it, from `min` to `max` (fractions of the knob's
    /// range), replacing a link between the two, and puts the knob where
    /// the macro says.
    pub fn link_macro(
        &mut self,
        patcher: DeviceId,
        index: usize,
        device: DeviceId,
        param: &str,
        min: f32,
        max: f32,
    ) -> EditResult {
        if index >= MACROS {
            return Err(EditError::Invalid(format!("a Patcher has {MACROS} macros")));
        }
        let inside = self
            .patch(patcher)
            .ok_or(EditError::NotAPatcher(patcher))?
            .devices
            .iter()
            .any(|d| d.id == device);
        if !inside {
            return Err(EditError::NoSuchDevice(device));
        }
        let known = self
            .device(device)
            .and_then(|d| d.effect.descriptor())
            .is_none_or(|d| d.params.iter().any(|p| p.id == param));
        if !known {
            return Err(EditError::Invalid(format!("the effect has no {param}")));
        }
        let link = MacroLink {
            index: index as u8,
            device,
            param: param.to_owned(),
            min: finite(min).clamp(0.0, 1.0),
            max: finite(max).clamp(0.0, 1.0),
        };
        let links = &mut self.patch_mut(patcher)?.links;
        links
            .retain(|l| !(usize::from(l.index) == index && l.device == device && l.param == param));
        links.push(link);
        self.apply_macros(patcher)
    }

    /// Takes the link between macro `index` and a knob away; the knob stays
    /// where it is.
    pub fn unlink_macro(
        &mut self,
        patcher: DeviceId,
        index: usize,
        device: DeviceId,
        param: &str,
    ) -> EditResult {
        self.patch_mut(patcher)?
            .links
            .retain(|l| !(usize::from(l.index) == index && l.device == device && l.param == param));
        Ok(())
    }

    /// Names macro `index`; an empty name goes back to "Macro n".
    pub fn rename_macro(&mut self, patcher: DeviceId, index: usize, name: &str) -> EditResult {
        if index >= MACROS {
            return Err(EditError::Invalid(format!("a Patcher has {MACROS} macros")));
        }
        let names = &mut self.patch_mut(patcher)?.names;
        if names.len() <= index {
            names.resize(index + 1, String::new());
        }
        names[index] = name.trim().to_owned();
        while names.last().is_some_and(String::is_empty) {
            names.pop();
        }
        Ok(())
    }

    /// Puts every knob a Patcher's macros drive where the macros say. Run
    /// after anything that sets a macro, or links one.
    pub(crate) fn apply_macros(&mut self, patcher: DeviceId) -> EditResult {
        let Some(device) = self.device(patcher) else {
            return Err(EditError::NoSuchDevice(patcher));
        };
        let Some(patch) = &device.patch else {
            return Ok(());
        };
        let targets: Vec<(DeviceId, String, f32)> = patch
            .links
            .iter()
            .filter_map(|link| {
                let amount = device.param(macro_param(link.index.into()))? / 100.0;
                Some((link.device, link.param.clone(), link.normalized(amount)))
            })
            .collect();
        for (target, param, normalized) in targets {
            let Some(descriptor) = self.device(target).and_then(|d| d.effect.descriptor()) else {
                continue;
            };
            let Some(knob) = descriptor.params.iter().find(|p| p.id == param) else {
                continue;
            };
            // Through `set_device_param`, so a Patcher's macro inside
            // drives its own knobs in turn.
            self.set_device_param(target, &param, knob.from_normalized(normalized))?;
        }
        Ok(())
    }

    /// Every knob a Patcher's macros drive, through Patchers inside too, as
    /// it's set now: the effect, the knob's index among its parameters and
    /// its value. What a live macro gesture sends the engine, which plays
    /// the effects inside a Patcher rather than the Patcher.
    pub fn driven_knobs(&self, patcher: DeviceId) -> Vec<(DeviceId, usize, f32)> {
        let mut out = Vec::new();
        let Some(patch) = self.patch(patcher) else {
            return out;
        };
        for link in &patch.links {
            let Some(device) = self.device(link.device) else {
                continue;
            };
            let Some(descriptor) = device.effect.descriptor() else {
                continue;
            };
            let Some(index) = descriptor.params.iter().position(|p| p.id == link.param) else {
                continue;
            };
            if let Some(value) = device.param(&link.param)
                && !out.iter().any(|(d, i, _)| *d == link.device && *i == index)
            {
                out.push((link.device, index, value));
            }
            if device.patch.is_some() {
                out.extend(self.driven_knobs(link.device));
            }
        }
        out
    }

    /// Puts effects that share a chain into a new Patcher, in their order,
    /// where the first of them was: several effects become one. Their
    /// automation and links go with them. Returns the Patcher.
    pub fn wrap_in_patcher(&mut self, ids: &[DeviceId]) -> EditResult<DeviceId> {
        let mut found: Vec<(Chain, usize)> = Vec::new();
        for id in ids {
            found.push(
                self.locate_device(*id)
                    .ok_or(EditError::NoSuchDevice(*id))?,
            );
        }
        found.sort();
        let Some(&(chain, first)) = found.first() else {
            return Err(EditError::NothingSelected);
        };
        if found.iter().any(|(c, _)| *c != chain) {
            return Err(EditError::Invalid(
                "the effects must be in one chain".into(),
            ));
        }
        let patcher = self.add_device(chain, effects::EffectRef::builtin(PATCHER), Some(first))?;
        let in_order: Vec<DeviceId> = found
            .iter()
            .filter_map(|(c, i)| Some(self.chain_devices(*c)?.get(*i + 1)?.id))
            .collect();
        self.move_devices(&in_order, Chain::Patch(patcher), 0)?;
        Ok(patcher)
    }

    /// Takes a Patcher's effects out into the chain it's in, where it was,
    /// and removes it with its macros. A graph inside is lost: they come
    /// out in the order the sound reached them.
    pub fn unpack_patcher(&mut self, patcher: DeviceId) -> EditResult<Vec<DeviceId>> {
        let (chain, index) = self
            .locate_device(patcher)
            .ok_or(EditError::NoSuchDevice(patcher))?;
        let inside: Vec<DeviceId> = self
            .patch(patcher)
            .ok_or(EditError::NotAPatcher(patcher))?
            .devices
            .iter()
            .map(|d| d.id)
            .collect();
        self.move_devices(&inside, chain, index)?;
        self.remove_device(patcher)?;
        Ok(inside)
    }

    /// Drops links to effects no longer directly inside their Patcher,
    /// everywhere.
    pub(crate) fn prune_macro_links(&mut self) {
        let prune = |devices: &mut [Device]| {
            walk_mut(devices, &mut |d| {
                if let Some(patch) = &mut d.patch {
                    let inside: Vec<DeviceId> = patch.devices.iter().map(|d| d.id).collect();
                    patch.links.retain(|l| inside.contains(&l.device));
                }
            });
        };
        prune(&mut self.master_devices);
        self.visit_mut(|track| prune(&mut track.devices));
    }
}

/// A new Patch for a device, if it's a Patcher.
pub(crate) fn patch_for(effect: &effects::EffectRef) -> Option<Box<Patch>> {
    (effect.format.is_builtin() && effect.id == PATCHER).then(Box::default)
}

fn finite(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use effects::EffectRef;

    use super::*;
    use crate::{
        AutomationLane, AutomationPoint, AutomationTarget, ControllerLink, NodeKind, PortRef,
    };

    /// A track with a Patcher holding a utility and a reverb.
    fn patched() -> (Project, TrackId, DeviceId, [DeviceId; 2]) {
        let mut p = Project::with_default_tracks();
        let track = p.track_ids()[0];
        let patcher = p
            .add_device(Some(track), EffectRef::builtin(PATCHER), None)
            .unwrap();
        let inside = ["canopy.utility", "canopy.reverb"].map(|id| {
            p.add_device(Chain::Patch(patcher), EffectRef::builtin(id), None)
                .unwrap()
        });
        (p, track, patcher, inside)
    }

    fn param(p: &Project, device: DeviceId, id: &str) -> f32 {
        p.device(device).unwrap().param(id).unwrap()
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn lane(device: DeviceId, param: &str) -> AutomationLane {
        AutomationLane {
            target: AutomationTarget::DeviceParam {
                device,
                param: param.into(),
            },
            points: vec![AutomationPoint {
                tick: midi::Tick::ZERO,
                value: 0.5,
                curve: 0.0,
            }]
            .into(),
        }
    }

    #[test]
    fn a_patcher_holds_a_chain_of_its_own() {
        let (mut p, track, patcher, [utility, reverb]) = patched();
        assert_eq!(p.devices(Some(track)).unwrap().len(), 1);
        let inside: Vec<DeviceId> = p
            .devices(Chain::Patch(patcher))
            .unwrap()
            .iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(inside, [utility, reverb]);
        assert_eq!(p.device(reverb).unwrap().name(), "Reverb");
        assert_eq!(p.locate_device(reverb), Some((Chain::Patch(patcher), 1)));
        // Where it is on the track: where its Patcher is.
        assert_eq!(p.find_device(reverb), Some((Some(track), 0)));
        assert_eq!(p.chain_owner(Chain::Patch(patcher)), Some(Some(track)));
        assert!(matches!(
            p.add_device(Chain::Patch(reverb), EffectRef::builtin("canopy.eq"), None),
            Err(EditError::NotAPatcher(_))
        ));
    }

    #[test]
    fn a_macro_moves_the_knobs_it_drives_over_their_ranges() {
        let (mut p, _, patcher, [utility, reverb]) = patched();
        p.link_macro(patcher, 0, reverb, "dry", 0.2, 0.6).unwrap();
        // Turned down as the macro goes up.
        p.link_macro(patcher, 0, utility, "gain", 1.0, 0.0).unwrap();
        // Linking puts the knob where the macro (at 0%) says.
        assert!(near(param(&p, reverb, "dry"), 20.0));
        assert!(near(param(&p, utility, "gain"), 24.0));
        p.set_device_param(patcher, "macro_1", 50.0).unwrap();
        assert!(near(param(&p, reverb, "dry"), 40.0));
        assert!(near(param(&p, utility, "gain"), -18.0));
        // A preset of the Patcher's macros moves them too.
        let preset = [("macro_1".to_owned(), 100.0)].into();
        p.set_device_params(patcher, preset).unwrap();
        assert!(near(param(&p, reverb, "dry"), 60.0));
        assert_eq!(p.patch(patcher).unwrap().used_macros(), [0]);

        // Unlinked, a knob stays where it was; removed, its links go.
        p.unlink_macro(patcher, 0, reverb, "dry").unwrap();
        p.set_device_param(patcher, "macro_1", 0.0).unwrap();
        assert!(near(param(&p, reverb, "dry"), 60.0));
        p.remove_device(utility).unwrap();
        assert!(p.patch(patcher).unwrap().links.is_empty());
        assert!(p.link_macro(patcher, 8, reverb, "dry", 0.0, 1.0).is_err());
        assert!(p.link_macro(patcher, 0, reverb, "nope", 0.0, 1.0).is_err());
    }

    #[test]
    fn a_macro_can_drive_a_patcher_inside() {
        let (mut p, _, outer, [_, reverb]) = patched();
        let inner = p
            .add_device(Chain::Patch(outer), EffectRef::builtin(PATCHER), None)
            .unwrap();
        p.move_devices(&[reverb], Chain::Patch(inner), 0).unwrap();
        p.link_macro(inner, 2, reverb, "dry", 0.0, 1.0).unwrap();
        p.link_macro(outer, 0, inner, "macro_3", 0.0, 1.0).unwrap();
        p.set_device_param(outer, "macro_1", 25.0).unwrap();
        assert!(near(param(&p, inner, "macro_3"), 25.0));
        assert!(near(param(&p, reverb, "dry"), 25.0));
        // Not into itself, nor into a Patcher inside it.
        assert_eq!(
            p.move_devices(&[outer], Chain::Patch(inner), 0),
            Err(EditError::IntoItself)
        );
    }

    #[test]
    fn removing_a_patcher_takes_what_points_inside_it() {
        let (mut p, track, patcher, [utility, _]) = patched();
        p.track_mut(track)
            .unwrap()
            .automation
            .push(lane(utility, "gain"));
        let link = ControllerLink {
            channel: 0,
            controller: 1,
            track: Some(track),
            target: AutomationTarget::DeviceParam {
                device: utility,
                param: "gain".into(),
            },
        };
        p.link_controller(link).unwrap();
        p.remove_device(patcher).unwrap();
        assert!(p.track(track).unwrap().automation.is_empty());
        assert!(p.controller_links.is_empty());
        assert!(p.device(utility).is_none());
    }

    #[test]
    fn a_copied_patcher_has_new_ids_all_the_way_in() {
        let (mut p, track, patcher, [utility, reverb]) = patched();
        p.link_macro(patcher, 0, reverb, "dry", 0.0, 1.0).unwrap();
        p.track_mut(track)
            .unwrap()
            .automation
            .push(lane(utility, "gain"));
        // A parallel path inside: the utility straight to Output too.
        let graph = p.ensure_routing(Chain::Patch(patcher)).unwrap().clone();
        let node = graph.device_node(utility).unwrap();
        let out = graph.output().unwrap();
        p.connect(
            Chain::Patch(patcher),
            PortRef { node, port: 0 },
            PortRef { node: out, port: 0 },
        )
        .unwrap();
        let copied = p.copy_devices(&[patcher]);
        let other = p.track_ids()[1];
        let [copy] = p.insert_devices(Some(other), None, &copied).unwrap()[..] else {
            panic!("one copy");
        };
        let patch = p.patch(copy).unwrap();
        let (new_utility, new_reverb) = (patch.devices[0].id, patch.devices[1].id);
        assert!(![utility, reverb, patcher].contains(&new_utility));
        assert_eq!(patch.links[0].device, new_reverb);
        let routing = patch.routing.as_ref().unwrap();
        assert_eq!(routing.validate(&patch.devices), Ok(()));
        assert!(routing.device_node(new_utility).is_some());
        let original = p.routing(Chain::Patch(patcher)).unwrap();
        assert!(routing.nodes.iter().all(|n| original.node(n.id).is_none()));
        // Its inside's automation came along, on its own ids.
        assert_eq!(
            p.track(other).unwrap().automation,
            [lane(new_utility, "gain")]
        );
        // And its macros still drive its own knobs only.
        p.set_device_param(copy, "macro_1", 100.0).unwrap();
        assert!(near(param(&p, new_reverb, "dry"), 100.0));
        assert!(near(param(&p, reverb, "dry"), 0.0));
    }

    #[test]
    fn effects_move_in_and_out_of_a_patcher_with_their_automation() {
        let (mut p, track, patcher, [utility, _]) = patched();
        let eq = p
            .add_device(Some(track), EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        p.track_mut(track)
            .unwrap()
            .automation
            .push(lane(eq, "output"));
        p.move_devices(&[eq], Chain::Patch(patcher), 0).unwrap();
        assert_eq!(p.locate_device(eq), Some((Chain::Patch(patcher), 0)));
        assert_eq!(p.track(track).unwrap().automation, [lane(eq, "output")]);
        p.link_macro(patcher, 0, utility, "gain", 0.0, 1.0).unwrap();
        let other = p.track_ids()[1];
        p.move_devices(&[utility], Some(other), 0).unwrap();
        // Out of the Patcher, nothing inside it drives it any more.
        assert!(p.patch(patcher).unwrap().links.is_empty());
        assert_eq!(p.locate_device(utility), Some((Chain::Track(other), 0)));
    }

    #[test]
    fn a_patchers_graph_refuses_a_loop_and_goes_linear_again() {
        let (mut p, _, patcher, [utility, reverb]) = patched();
        let chain = Chain::Patch(patcher);
        let graph = p.ensure_routing(chain).unwrap().clone();
        let (u, r) = (
            graph.device_node(utility).unwrap(),
            graph.device_node(reverb).unwrap(),
        );
        let input = graph.input().unwrap();
        // Input straight into the reverb too: a parallel path.
        let key = p
            .connect(
                chain,
                PortRef {
                    node: input,
                    port: 0,
                },
                PortRef { node: r, port: 0 },
            )
            .unwrap();
        assert!(p.routing(chain).is_some());
        let before = p.clone();
        let looped = p.connect(
            chain,
            PortRef { node: r, port: 0 },
            PortRef { node: u, port: 0 },
        );
        assert!(looped.is_err());
        assert_eq!(p, before);
        p.set_cable(chain, key, -6.0, false).unwrap();
        assert_eq!(p.routing(chain).unwrap().cable(key).unwrap().gain_db, -6.0);
        p.disconnect(chain, key).unwrap();
        assert!(p.routing(chain).is_none(), "a straight line again");
        let split = p
            .add_node(chain, NodeKind::Split(crate::Split::MidSide), [0.0, 100.0])
            .unwrap();
        assert!(p.routing(chain).unwrap().node(split).is_some());
        p.remove_node(chain, split).unwrap();
        assert!(p.routing(chain).is_none());
    }

    #[test]
    fn a_project_with_a_patcher_saves_as_format_3_and_loads_the_same() {
        let (mut p, _, patcher, [_, reverb]) = patched();
        p.link_macro(patcher, 1, reverb, "dry", 0.1, 0.9).unwrap();
        p.rename_macro(patcher, 1, "Space").unwrap();
        assert_eq!(p.file_format(), 3);
        let text = p.to_file_string().unwrap();
        assert!(text.contains("format: 3"));
        let loaded = Project::from_file_string(&text).unwrap();
        assert_eq!(loaded, p);
        assert_eq!(loaded.patch(patcher).unwrap().macro_name(1), "Space");
        assert_eq!(loaded.patch(patcher).unwrap().macro_name(0), "Macro 1");
    }

    #[test]
    fn repair_gives_a_patcher_a_patch_and_drops_links_to_effects_not_inside() {
        let (mut p, track, patcher, [utility, _]) = patched();
        p.link_macro(patcher, 0, utility, "gain", 0.0, 1.0).unwrap();
        let mut text = p.to_file_string().unwrap();
        // A link to an effect that isn't inside, by hand.
        text = text.replacen(&format!("device: {},", utility.0), "device: 9999,", 1);
        let loaded = Project::from_file_string(&text).unwrap();
        assert!(loaded.patch(patcher).unwrap().links.is_empty());
        // A Patcher saved without its Patch gets an empty one.
        let mut bare = p.clone();
        bare.track_mut(track).unwrap().devices[0].patch = None;
        let loaded = Project::from_file_string(&bare.to_file_string().unwrap()).unwrap();
        assert_eq!(loaded.patch(patcher), Some(&Patch::default()));
    }

    #[test]
    fn effects_wrap_into_a_patcher_and_unpack_again() {
        let mut p = Project::with_default_tracks();
        let track = p.track_ids()[0];
        let ids = [
            "canopy.eq",
            "canopy.compressor",
            "canopy.reverb",
            "canopy.delay",
        ]
        .map(|id| {
            p.add_device(Some(track), EffectRef::builtin(id), None)
                .unwrap()
        });
        p.track_mut(track)
            .unwrap()
            .automation
            .push(lane(ids[2], "dry"));
        // Picked out of order: they keep the chain's.
        let patcher = p.wrap_in_patcher(&[ids[2], ids[1]]).unwrap();
        let top: Vec<DeviceId> = p
            .devices(Some(track))
            .unwrap()
            .iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(top, [ids[0], patcher, ids[3]]);
        let inside: Vec<DeviceId> = p
            .devices(Chain::Patch(patcher))
            .unwrap()
            .iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(inside, [ids[1], ids[2]]);
        assert_eq!(p.track(track).unwrap().automation, [lane(ids[2], "dry")]);
        let other = p.track_ids()[1];
        let elsewhere = p
            .add_device(Some(other), EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        assert!(p.wrap_in_patcher(&[ids[0], elsewhere]).is_err());

        p.unpack_patcher(patcher).unwrap();
        let top: Vec<DeviceId> = p
            .devices(Some(track))
            .unwrap()
            .iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(top, ids);
        assert_eq!(p.track(track).unwrap().automation, [lane(ids[2], "dry")]);
    }

    #[test]
    fn a_rack_keeps_its_patchers_inside() {
        let (mut p, track, patcher, [_, reverb]) = patched();
        p.link_macro(patcher, 0, reverb, "dry", 0.0, 1.0).unwrap();
        let rack = p.rack(Some(track)).unwrap();
        let other = p.track_ids()[1];
        let [copy] = p.load_rack(Some(other), &rack).unwrap()[..] else {
            panic!("one effect");
        };
        let patch = p.patch(copy).unwrap();
        assert_eq!(patch.devices.len(), 2);
        assert_ne!(patch.devices[1].id, reverb);
        assert_eq!(patch.links[0].device, patch.devices[1].id);
    }
}
