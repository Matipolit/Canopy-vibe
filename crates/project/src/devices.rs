//! Effect chains: adding, removing, reordering and bypassing effects, and
//! setting their parameters. A chain belongs to a track, to the master
//! (`None`), or to a Patcher ([`Chain`]); functions taking a chain take any
//! of those, an `Option<TrackId>` included.
//!
//! What refers to an effect goes where it goes: its automation lanes,
//! controller links and modulation routes (the master has none of those,
//! so they're dropped there), and its node in the chain's graph, if the
//! chain has one (see `routing.rs`): a removed effect's node is spliced
//! out, joining what fed it to what it fed, and an added one is spliced in
//! just before the graph's Output.

use std::collections::{BTreeMap, HashMap};

use effects::EffectRef;

use crate::edit::EditResult;
use crate::patch::{Chain, ids_within, macro_index, patch_for, walk, walk_mut};
use crate::{
    AutomationLane, AutomationTarget, Device, DeviceId, EditError, Listen, ModRoute, NodeKind,
    PluginData, Project, TrackId,
};

/// An effect as copied: its settings, and the automation and modulation of
/// them on its track (and of the effects inside it, for a Patcher), so a
/// paste sounds and moves the same.
#[derive(Debug, Clone, PartialEq)]
pub struct CopiedDevice {
    pub device: Device,
    pub automation: Vec<AutomationLane>,
    /// Routes modulating its parameters. A paste keeps the ones whose
    /// source is on the track it lands on.
    pub routes: Vec<ModRoute>,
}

/// Whether `target` is a parameter of one of `devices`.
fn targets_any(target: &AutomationTarget, devices: &[DeviceId]) -> bool {
    matches!(target, AutomationTarget::DeviceParam { device, .. } if devices.contains(device))
}

impl Project {
    /// A chain's effects: a track's, the master's (`None`) or a Patcher's.
    pub fn devices(&self, chain: impl Into<Chain>) -> Option<&[Device]> {
        self.chain_devices(chain.into())
    }

    fn devices_mut(&mut self, chain: Chain) -> EditResult<&mut Vec<Device>> {
        Ok(self.chain_mut(chain)?.0)
    }

    /// Which track's chain (or, `None`, the master's) an effect is in, and
    /// where; for one inside a Patcher, where the outermost Patcher is.
    /// [`Project::locate_device`] says which chain exactly.
    pub fn find_device(&self, id: DeviceId) -> Option<(Option<TrackId>, usize)> {
        let (mut chain, mut index) = self.locate_device(id)?;
        loop {
            match chain {
                Chain::Master => return Some((None, index)),
                Chain::Track(track) => return Some((Some(track), index)),
                Chain::Patch(patcher) => (chain, index) = self.locate_device(patcher)?,
            }
        }
    }

    /// An effect, wherever it is.
    pub fn device(&self, id: DeviceId) -> Option<&Device> {
        let (chain, index) = self.locate_device(id)?;
        self.chain_devices(chain)?.get(index)
    }

    pub(crate) fn device_mut(&mut self, id: DeviceId) -> EditResult<&mut Device> {
        let (chain, index) = self.locate_device(id).ok_or(EditError::NoSuchDevice(id))?;
        self.devices_mut(chain)?
            .get_mut(index)
            .ok_or(EditError::NoSuchDevice(id))
    }

    /// The track whose automation and links hold a chain's (`None` for the
    /// master's), or an error if there's no such chain.
    fn owner(&self, chain: Chain) -> EditResult<Option<TrackId>> {
        self.chain_owner(chain).ok_or(match chain {
            Chain::Track(id) => EditError::NoSuchTrack(id),
            Chain::Patch(id) => EditError::NoSuchDevice(id),
            Chain::Master => EditError::NothingSelected,
        })
    }

    /// Adds `effect` to a chain at `index` (the end if `None` or past it).
    /// A built-in must be one this build has; a plugin needn't be loaded
    /// yet (an import names plugins the host loads afterwards), and until
    /// it is the device keeps what it's given, as one opened from a file
    /// does. A Patcher starts empty.
    pub fn add_device(
        &mut self,
        chain: impl Into<Chain>,
        effect: EffectRef,
        index: Option<usize>,
    ) -> EditResult<DeviceId> {
        let chain = chain.into();
        if effect.format.is_builtin() && effect.descriptor().is_none() {
            return Err(EditError::UnknownEffect(effect.id));
        }
        self.devices_mut(chain)?;
        let id = self.new_device_id();
        let devices = self.devices_mut(chain)?;
        let index = index.unwrap_or(devices.len()).min(devices.len());
        let patch = patch_for(&effect);
        devices.insert(
            index,
            Device {
                id,
                effect,
                params: BTreeMap::new(),
                bypassed: false,
                sidechain: None,
                folded: false,
                plugin: None,
                patch,
            },
        );
        self.normalize_routing(chain)?;
        Ok(id)
    }

    /// Removes an effect (with everything inside it, for a Patcher), with
    /// its automation, controller links and the macro links to it.
    pub fn remove_device(&mut self, id: DeviceId) -> EditResult {
        let (chain, index) = self.locate_device(id).ok_or(EditError::NoSuchDevice(id))?;
        let owner = self.owner(chain)?;
        let removed = self.devices_mut(chain)?.remove(index);
        let gone = ids_within(&removed);
        self.normalize_routing(chain)?;
        if let Some(track) = owner.and_then(|t| self.track_mut(t)) {
            track
                .automation
                .retain(|lane| !targets_any(&lane.target, &gone));
            track.prune_modulation();
        }
        self.controller_links
            .retain(|l| !targets_any(&l.target, &gone));
        self.prune_macro_links();
        Ok(())
    }

    /// Removes several effects (see [`Project::remove_device`]).
    pub fn remove_devices(&mut self, ids: &[DeviceId]) -> EditResult {
        for id in self.outermost_devices(ids) {
            self.remove_device(id)?;
        }
        Ok(())
    }

    /// `ids` without those inside a Patcher that's among them too: what a
    /// copy, move or removal of them all acts on.
    fn outermost_devices(&self, ids: &[DeviceId]) -> Vec<DeviceId> {
        let mut inner = Vec::new();
        for id in ids {
            if let Some(device) = self.device(*id) {
                inner.extend(ids_within(device).into_iter().skip(1));
            }
        }
        let mut kept: Vec<DeviceId> = Vec::new();
        for id in ids {
            if !inner.contains(id) && !kept.contains(id) {
                kept.push(*id);
            }
        }
        kept
    }

    /// Copies effects, in the order they're in their chains, with their
    /// automation (and, for a Patcher, everything inside it with theirs).
    /// Ids that aren't there are left out.
    pub fn copy_devices(&self, ids: &[DeviceId]) -> Vec<CopiedDevice> {
        let mut found: Vec<(Chain, usize)> = self
            .outermost_devices(ids)
            .iter()
            .filter_map(|id| self.locate_device(*id))
            .collect();
        found.sort();
        found.dedup();
        found
            .into_iter()
            .filter_map(|(chain, index)| {
                let device = self.chain_devices(chain)?.get(index)?.clone();
                let within = ids_within(&device);
                let owner = self
                    .chain_owner(chain)
                    .flatten()
                    .and_then(|t| self.track(t));
                let automation = owner
                    .map(|t| {
                        t.automation
                            .iter()
                            .filter(|lane| targets_any(&lane.target, &within))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                let routes = owner
                    .map(|t| {
                        t.modulations
                            .iter()
                            .filter(|route| targets_any(&route.target, &within))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                Some(CopiedDevice {
                    device,
                    automation,
                    routes,
                })
            })
            .collect()
    }

    /// Gives `device`, and everything inside it, new ids, with the graphs
    /// and macro links inside following. Returns the new id of each old one.
    fn renew_ids(&mut self, device: &mut Device) -> HashMap<DeviceId, DeviceId> {
        let mut map = HashMap::new();
        self.renew(device, &mut map);
        map
    }

    fn renew(&mut self, device: &mut Device, map: &mut HashMap<DeviceId, DeviceId>) {
        let new = self.new_device_id();
        map.insert(device.id, new);
        device.id = new;
        let Some(patch) = &mut device.patch else {
            return;
        };
        for inner in &mut patch.devices {
            self.renew(inner, map);
        }
        if let Some(routing) = &mut patch.routing {
            let mut nodes = HashMap::new();
            for node in &mut routing.nodes {
                let id = self.new_node_id();
                nodes.insert(node.id, id);
                node.id = id;
                if let NodeKind::Device(inner) = &mut node.kind
                    && let Some(new) = map.get(inner)
                {
                    *inner = *new;
                }
            }
            for cable in &mut routing.cables {
                for end in [&mut cable.from, &mut cable.to] {
                    if let Some(new) = nodes.get(&end.node) {
                        end.node = *new;
                    }
                }
            }
        }
        for link in &mut patch.links {
            if let Some(new) = map.get(&link.device) {
                link.device = *new;
            }
        }
    }

    /// Puts copies of `devices` into a chain at `index` (the end if `None`
    /// or past it), with new ids (inside Patchers too), their automation on
    /// that chain's track, and their modulation where its source is on that
    /// track too. A copy that would listen to its own track loses its
    /// sidechain. In a graph, they go just before Output, in order. Returns
    /// the new ids.
    pub fn insert_devices(
        &mut self,
        chain: impl Into<Chain>,
        index: Option<usize>,
        devices: &[CopiedDevice],
    ) -> EditResult<Vec<DeviceId>> {
        let chain = chain.into();
        let owner = self.owner(chain)?;
        let len = self.devices_mut(chain)?.len();
        let start = index.unwrap_or(len).min(len);
        let mut ids = Vec::new();
        for (at, copied) in (start..).zip(devices) {
            let mut device = copied.device.clone();
            let old = device.id;
            let map = self.renew_ids(&mut device);
            let tracks: Vec<TrackId> = {
                let mut tracks = Vec::new();
                walk(std::slice::from_ref(&device), &mut |d| {
                    tracks.extend(d.sidechain)
                });
                tracks.retain(|t| self.track(*t).is_some());
                tracks
            };
            walk_mut(std::slice::from_mut(&mut device), &mut |d| {
                if d.sidechain == owner || d.sidechain.is_some_and(|t| !tracks.contains(&t)) {
                    d.sidechain = None;
                }
            });
            let renamed = |target: &AutomationTarget| match target {
                AutomationTarget::DeviceParam { device, param } => {
                    map.get(device).map(|new| AutomationTarget::DeviceParam {
                        device: *new,
                        param: param.clone(),
                    })
                }
                _ => None,
            };
            let route_ids: Vec<_> = copied
                .routes
                .iter()
                .map(|_| self.new_mod_route_id())
                .collect();
            if let Some(track) = owner.and_then(|t| self.track_mut(t)) {
                for lane in &copied.automation {
                    if let Some(target) = renamed(&lane.target) {
                        let mut lane = lane.clone();
                        lane.target = target;
                        track.automation.push(lane);
                    }
                }
                for (route, new_id) in copied.routes.iter().zip(route_ids) {
                    if let Some(target) = renamed(&route.target)
                        && track.has_source(&route.source)
                    {
                        let mut route = route.clone();
                        route.id = new_id;
                        route.target = target;
                        if route
                            .scale_by
                            .as_ref()
                            .is_some_and(|s| !track.has_source(s))
                        {
                            route.scale_by = None;
                        }
                        track.modulations.push(route);
                    }
                }
            }
            self.devices_mut(chain)?.insert(at, device);
            ids.push(map[&old]);
        }
        self.normalize_routing(chain)?;
        Ok(ids)
    }

    /// Copies effects into their own chains, the copies of each chain's
    /// together after the last of its originals. Returns the copies' ids.
    pub fn duplicate_devices(&mut self, ids: &[DeviceId]) -> EditResult<Vec<DeviceId>> {
        let ids = self.outermost_devices(ids);
        let mut chains: Vec<Chain> = Vec::new();
        for id in &ids {
            let (chain, _) = self
                .locate_device(*id)
                .ok_or(EditError::NoSuchDevice(*id))?;
            if !chains.contains(&chain) {
                chains.push(chain);
            }
        }
        let mut copies = Vec::new();
        for chain in chains {
            let in_chain: Vec<DeviceId> = ids
                .iter()
                .copied()
                .filter(|id| self.locate_device(*id).is_some_and(|(c, _)| c == chain))
                .collect();
            let last = in_chain
                .iter()
                .filter_map(|id| self.locate_device(*id))
                .map(|(_, index)| index)
                .max()
                .unwrap_or(0);
            let copied = self.copy_devices(&in_chain);
            copies.extend(self.insert_devices(chain, Some(last + 1), &copied)?);
        }
        Ok(copies)
    }

    /// Moves effects, keeping their order, into a chain (their own or
    /// another track's, the master's, or a Patcher's) at `index`, counted
    /// in that chain as it is before they leave. Their automation,
    /// controller links and modulation go with them (modulation only where
    /// its source is on the track they go to); the master has none of
    /// those, so they're dropped there. Graph nodes are spliced out of the
    /// chains they leave and in before Output in the one they join; moving
    /// within a graph's own pool changes nothing, since the graph sets the
    /// order. A Patcher can't go inside itself.
    pub fn move_devices(
        &mut self,
        ids: &[DeviceId],
        chain: impl Into<Chain>,
        index: usize,
    ) -> EditResult {
        let chain = chain.into();
        let ids = self.outermost_devices(ids);
        for id in &ids {
            let device = self.device(*id).ok_or(EditError::NoSuchDevice(*id))?;
            if let Chain::Patch(patcher) = chain
                && ids_within(device).contains(&patcher)
            {
                return Err(EditError::IntoItself);
            }
        }
        let to = self.owner(chain)?;
        let target_len = self.devices_mut(chain)?.len();
        let index = index.min(target_len);
        let moving = self.copy_devices(&ids);
        // Places before `index` that the moving ones free up.
        let freed = moving
            .iter()
            .filter_map(|c| self.locate_device(c.device.id))
            .filter(|(c, i)| *c == chain && *i < index)
            .count();
        let mut chains = vec![chain];
        for copied in &moving {
            let id = copied.device.id;
            let within = ids_within(&copied.device);
            let (from, at) = self.locate_device(id).ok_or(EditError::NoSuchDevice(id))?;
            let from_owner = self.owner(from)?;
            self.devices_mut(from)?.remove(at);
            if let Some(track) = from_owner.and_then(|t| self.track_mut(t)) {
                track
                    .automation
                    .retain(|lane| !targets_any(&lane.target, &within));
                track
                    .modulations
                    .retain(|route| !targets_any(&route.target, &within));
            }
            if !chains.contains(&from) {
                chains.push(from);
            }
        }
        for (at, copied) in (index - freed..).zip(moving) {
            let mut device = copied.device;
            let within = ids_within(&device);
            walk_mut(std::slice::from_mut(&mut device), &mut |d| {
                if d.sidechain.is_some() && d.sidechain == to {
                    d.sidechain = None;
                }
            });
            match to.and_then(|t| self.track_mut(t)) {
                Some(track) => {
                    track.automation.extend(copied.automation);
                    let arriving: Vec<ModRoute> = copied
                        .routes
                        .into_iter()
                        .filter(|r| track.has_source(&r.source))
                        .collect();
                    track.modulations.extend(arriving);
                    for link in &mut self.controller_links {
                        if targets_any(&link.target, &within) {
                            link.track = to;
                        }
                    }
                }
                None => self
                    .controller_links
                    .retain(|l| !targets_any(&l.target, &within)),
            }
            self.devices_mut(chain)?.insert(at, device);
        }
        for chain in chains {
            self.normalize_routing(chain)?;
            if let Some(track) = self
                .chain_owner(chain)
                .flatten()
                .and_then(|t| self.track_mut(t))
            {
                track.prune_modulation();
            }
        }
        self.prune_macro_links();
        Ok(())
    }

    /// Moves an effect to `index` in its chain. In a graph, the graph sets
    /// the order, so this changes nothing.
    pub fn move_device(&mut self, id: DeviceId, index: usize) -> EditResult {
        let (chain, from) = self.locate_device(id).ok_or(EditError::NoSuchDevice(id))?;
        let devices = self.devices_mut(chain)?;
        let device = devices.remove(from);
        let index = index.min(devices.len());
        devices.insert(index, device);
        self.normalize_routing(chain)
    }

    pub fn set_device_bypassed(&mut self, id: DeviceId, bypassed: bool) -> EditResult {
        self.device_mut(id)?.bypassed = bypassed;
        Ok(())
    }

    /// Shows an effect as just its name, or in full.
    pub fn set_device_folded(&mut self, id: DeviceId, folded: bool) -> EditResult {
        self.device_mut(id)?.folded = folded;
        Ok(())
    }

    /// Every effect's fold, inside Patchers too, for carrying it over undo
    /// and redo.
    pub fn device_folds(&self) -> Vec<(DeviceId, bool)> {
        let mut folds = Vec::new();
        let mut add = |devices: &[Device]| walk(devices, &mut |d| folds.push((d.id, d.folded)));
        add(&self.master_devices);
        self.visit(|track, _| add(&track.devices));
        folds
    }

    /// Sets an effect parameter, or one of its UI settings, clamped to its
    /// range. A Patcher's macro sets the knobs it drives too.
    pub fn set_device_param(&mut self, id: DeviceId, param: &str, value: f32) -> EditResult {
        let device = self.device_mut(id)?;
        let value = device
            .effect
            .descriptor()
            .and_then(|d| d.params.iter().chain(d.settings).find(|p| p.id == param))
            .map_or(value, |p| p.clamp(value));
        device.params.insert(param.to_owned(), value);
        if device.patch.is_some() && macro_index(param).is_some() {
            self.apply_macros(id)?;
        }
        Ok(())
    }

    /// Replaces all of an effect's settings (a preset): ids it doesn't have
    /// are left out, and values are clamped to their ranges. A Patcher's
    /// macros set the knobs they drive.
    pub fn set_device_params(&mut self, id: DeviceId, params: BTreeMap<String, f32>) -> EditResult {
        let device = self.device_mut(id)?;
        let Some(descriptor) = device.effect.descriptor() else {
            return Ok(());
        };
        device.params = params
            .into_iter()
            .filter_map(|(param, value)| {
                let known = descriptor
                    .params
                    .iter()
                    .chain(descriptor.settings)
                    .find(|p| p.id == param)?;
                value.is_finite().then(|| (param, known.clamp(value)))
            })
            .collect();
        if device.patch.is_some() {
            self.apply_macros(id)?;
        }
        Ok(())
    }

    /// Routes `source`'s sound into an effect's sidechain, or (`None`)
    /// disconnects it.
    pub fn set_device_sidechain(&mut self, id: DeviceId, source: Option<TrackId>) -> EditResult {
        if let Some(track) = source
            && self.track(track).is_none()
        {
            return Err(EditError::NoSuchTrack(track));
        }
        self.device_mut(id)?.sidechain = source;
        Ok(())
    }

    /// Sets (or clears) what the project keeps about a plugin effect: its
    /// name, vendor, where it was found and its state.
    pub fn set_device_plugin(&mut self, id: DeviceId, data: Option<PluginData>) -> EditResult {
        self.device_mut(id)?.plugin = data;
        Ok(())
    }

    /// Disconnects sidechains, and followers, that listen to a track that's
    /// gone.
    pub(crate) fn prune_track_references(&mut self) {
        let mut tracks = std::collections::HashSet::new();
        self.visit(|t, _| {
            tracks.insert(t.id);
        });
        let prune = |devices: &mut Vec<Device>| {
            walk_mut(devices, &mut |device| {
                if device.sidechain.is_some_and(|t| !tracks.contains(&t)) {
                    device.sidechain = None;
                }
            });
        };
        prune(&mut self.master_devices);
        self.visit_mut(|track| {
            prune(&mut track.devices);
            for modulator in &mut track.modulators {
                if let Some(Listen::Track(t)) = modulator.listen
                    && !tracks.contains(&t)
                {
                    modulator.listen = None;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AutomationPoint, ControllerLink};

    #[test]
    fn chains_on_tracks_and_the_master() {
        let mut p = Project::with_default_tracks();
        let track = p.track_ids()[0];
        let eq = p
            .add_device(Some(track), EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        let reverb = p
            .add_device(Some(track), EffectRef::builtin("canopy.reverb"), None)
            .unwrap();
        let first = p
            .add_device(
                Some(track),
                EffectRef::builtin("canopy.compressor"),
                Some(0),
            )
            .unwrap();
        let names: Vec<&str> = p
            .devices(Some(track))
            .unwrap()
            .iter()
            .map(Device::name)
            .collect();
        assert_eq!(names, ["Compressor", "EQ", "Reverb"]);
        p.move_device(first, 9).unwrap();
        assert_eq!(p.find_device(first), Some((Some(track), 2)));
        assert_eq!(p.find_device(eq), Some((Some(track), 0)));

        let master = p
            .add_device(None, EffectRef::builtin("canopy.compressor"), None)
            .unwrap();
        assert_eq!(p.find_device(master), Some((None, 0)));

        p.set_device_param(reverb, "mix", 250.0).unwrap();
        assert_eq!(
            p.device(reverb).unwrap().param("mix"),
            Some(100.0),
            "clamped"
        );
        assert_eq!(
            p.device(reverb).unwrap().param("decay"),
            Some(2.0),
            "default"
        );
        p.set_device_bypassed(reverb, true).unwrap();
        assert!(p.device(reverb).unwrap().bypassed);

        assert!(matches!(
            p.add_device(None, EffectRef::builtin("nope"), None),
            Err(EditError::UnknownEffect(_))
        ));
        // A plugin no host has loaded yet is kept, for when one does.
        let plugin = EffectRef {
            format: crate::Format::Clap,
            id: "com.example.unloaded".into(),
            version: String::new(),
        };
        let unloaded = p.add_device(None, plugin, None).unwrap();
        assert!(p.device(unloaded).unwrap().values().is_empty());
        assert!(
            p.add_device(Some(TrackId(9999)), EffectRef::builtin("canopy.eq"), None)
                .is_err()
        );

        // Removing one takes its automation and controller links with it.
        let target = AutomationTarget::DeviceParam {
            device: reverb,
            param: "mix".into(),
        };
        let point = AutomationPoint {
            tick: midi::Tick::ZERO,
            value: 0.5,
            curve: 0.0,
        };
        p.set_automation(track, target.clone(), vec![point])
            .unwrap();
        p.link_controller(ControllerLink {
            channel: 0,
            controller: 1,
            track: Some(track),
            target: target.clone(),
        })
        .unwrap();
        p.remove_device(reverb).unwrap();
        assert!(p.device(reverb).is_none());
        assert!(p.track(track).unwrap().automation_lane(&target).is_none());
        assert!(p.controller_links.is_empty());
    }

    #[test]
    fn sidechains_follow_their_track() {
        let mut p = Project::with_default_tracks();
        let ids = p.track_ids();
        let (kick, bass) = (ids[0], ids[1]);
        let ducker = p
            .add_device(Some(bass), EffectRef::builtin("canopy.compressor"), None)
            .unwrap();
        assert!(p.set_device_sidechain(ducker, Some(TrackId(9999))).is_err());
        p.set_device_sidechain(ducker, Some(kick)).unwrap();
        let text = p.to_file_string().unwrap();
        let loaded = Project::from_file_string(&text).unwrap();
        assert_eq!(loaded.device(ducker).unwrap().sidechain, Some(kick));

        p.delete_tracks(&[kick]).unwrap();
        assert_eq!(
            p.device(ducker).unwrap().sidechain,
            None,
            "disconnected with the track"
        );
    }

    fn names(p: &Project, track: Option<TrackId>) -> Vec<&'static str> {
        p.devices(track)
            .unwrap()
            .iter()
            .map(|d| d.effect.descriptor().unwrap().name)
            .collect()
    }

    /// Three effects on the first track: EQ, Compressor, Reverb.
    fn chain() -> (Project, TrackId, [DeviceId; 3]) {
        let mut p = Project::with_default_tracks();
        let track = p.track_ids()[0];
        let ids = ["canopy.eq", "canopy.compressor", "canopy.reverb"].map(|id| {
            p.add_device(Some(track), EffectRef::builtin(id), None)
                .unwrap()
        });
        (p, track, ids)
    }

    fn mix_lane(device: DeviceId) -> AutomationTarget {
        AutomationTarget::DeviceParam {
            device,
            param: "mix".into(),
        }
    }

    fn point(value: f32) -> AutomationPoint {
        AutomationPoint {
            tick: midi::Tick::ZERO,
            value,
            curve: 0.0,
        }
    }

    #[test]
    fn duplicating_copies_settings_and_automation_after_the_originals() {
        let (mut p, track, [eq, comp, reverb]) = chain();
        p.set_device_param(comp, "ratio", 8.0).unwrap();
        p.set_device_bypassed(comp, true).unwrap();
        p.set_automation(track, mix_lane(comp), vec![point(0.25)])
            .unwrap();
        let copies = p.duplicate_devices(&[comp, eq]).unwrap();
        assert_eq!(copies.len(), 2);
        assert_eq!(
            names(&p, Some(track)),
            ["EQ", "Compressor", "EQ", "Compressor", "Reverb"],
            "in chain order, after the last original"
        );
        let copy = p.device(copies[1]).unwrap();
        assert_eq!(copy.param("ratio"), Some(8.0));
        assert!(copy.bypassed);
        let lane = p
            .track(track)
            .unwrap()
            .automation_lane(&mix_lane(copies[1]))
            .unwrap();
        assert_eq!(*lane.points, [point(0.25)]);
        // The originals are untouched.
        assert!(p.device(comp).is_some() && p.device(reverb).is_some());
        assert!(p.duplicate_devices(&[DeviceId(9999)]).is_err());
    }

    #[test]
    fn moving_within_a_chain_counts_places_before_they_leave() {
        let (mut p, track, [eq, comp, reverb]) = chain();
        // Before the reverb: EQ, Compressor stay in front of it.
        p.move_devices(&[eq], Some(track), 2).unwrap();
        assert_eq!(names(&p, Some(track)), ["Compressor", "EQ", "Reverb"]);
        // To the end.
        p.move_devices(&[comp, eq], Some(track), 3).unwrap();
        assert_eq!(names(&p, Some(track)), ["Reverb", "Compressor", "EQ"]);
        // To the front, keeping their order.
        p.move_devices(&[eq, comp], Some(track), 0).unwrap();
        assert_eq!(names(&p, Some(track)), ["Compressor", "EQ", "Reverb"]);
        // Where they already are.
        p.move_devices(&[reverb], Some(track), 3).unwrap();
        assert_eq!(names(&p, Some(track)), ["Compressor", "EQ", "Reverb"]);
    }

    #[test]
    fn moving_to_another_track_takes_automation_links_and_sidechain_rules() {
        let (mut p, track, [_, comp, _]) = chain();
        let other = p.track_ids()[1];
        p.set_automation(track, mix_lane(comp), vec![point(0.5)])
            .unwrap();
        p.link_controller(ControllerLink {
            channel: 0,
            controller: 7,
            track: Some(track),
            target: mix_lane(comp),
        })
        .unwrap();
        // It listens to the track it's going to: that would be itself.
        p.set_device_sidechain(comp, Some(other)).unwrap();
        p.move_devices(&[comp], Some(other), 0).unwrap();
        assert_eq!(p.find_device(comp), Some((Some(other), 0)));
        assert!(
            p.track(track)
                .unwrap()
                .automation_lane(&mix_lane(comp))
                .is_none()
        );
        assert!(
            p.track(other)
                .unwrap()
                .automation_lane(&mix_lane(comp))
                .is_some()
        );
        assert_eq!(p.controller_links[0].track, Some(other));
        assert_eq!(p.device(comp).unwrap().sidechain, None);

        // The master has no automation: it's dropped there.
        p.move_devices(&[comp], None, 0).unwrap();
        assert_eq!(p.find_device(comp), Some((None, 0)));
        assert!(
            p.track(other)
                .unwrap()
                .automation_lane(&mix_lane(comp))
                .is_none()
        );
        assert!(p.controller_links.is_empty());
    }

    #[test]
    fn pasting_makes_new_devices_with_their_automation() {
        let (mut p, track, [eq, comp, _]) = chain();
        let other = p.track_ids()[1];
        p.set_automation(track, mix_lane(comp), vec![point(0.75)])
            .unwrap();
        p.set_device_sidechain(comp, Some(other)).unwrap();
        let copied = p.copy_devices(&[comp, eq]);
        assert_eq!(copied.len(), 2);
        assert_eq!(copied[0].device.id, eq, "in chain order");
        let pasted = p.insert_devices(Some(other), None, &copied).unwrap();
        assert_eq!(names(&p, Some(other)), ["EQ", "Compressor"]);
        assert!(!pasted.contains(&eq) && !pasted.contains(&comp));
        let lane = p
            .track(other)
            .unwrap()
            .automation_lane(&mix_lane(pasted[1]))
            .unwrap();
        assert_eq!(*lane.points, [point(0.75)]);
        assert_eq!(p.device(pasted[1]).unwrap().sidechain, None, "not itself");
        // Pasted twice: different ids again.
        let again = p.insert_devices(Some(other), Some(0), &copied).unwrap();
        assert_eq!(
            names(&p, Some(other)),
            ["EQ", "Compressor", "EQ", "Compressor"]
        );
        assert!(again.iter().all(|id| !pasted.contains(id)));
        p.remove_devices(&again).unwrap();
        assert_eq!(names(&p, Some(other)), ["EQ", "Compressor"]);
    }

    #[test]
    fn chains_survive_saving() {
        let mut p = Project::with_default_tracks();
        let track = p.track_ids()[2];
        let eq = p
            .add_device(Some(track), EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        p.set_device_param(eq, "b3_gain", 6.0).unwrap();
        p.add_device(None, EffectRef::builtin("canopy.delay"), None)
            .unwrap();
        let text = p.to_file_string().unwrap();
        let loaded = Project::from_file_string(&text).unwrap();
        assert_eq!(loaded, p);
    }
}
