//! Whole tracks copied, pasted and duplicated, with everything on them: the
//! instrument and its settings, effects and their graph, modulators,
//! automation, clips and notes.

use std::collections::{HashMap, HashSet};

use crate::edit::EditResult;
use crate::{
    AudioSource, AutomationTarget, DeviceId, EditError, Listen, ModSource, ModulatorId, NodeId,
    NodeKind, Project, SourceId, Track, TrackId, TrackKind, TrackPosition,
};

/// A track as copied, with the audio files its clips (and its children's)
/// play, so it can be pasted into another project.
#[derive(Debug, Clone, PartialEq)]
pub struct CopiedTrack {
    pub track: Track,
    pub sources: Vec<AudioSource>,
}

/// The new id of everything in the tracks being pasted. Track ids are
/// known up front, so references to tracks outside the paste can be told
/// apart; the rest are given out as they're met, references included.
#[derive(Default)]
struct Renumber {
    tracks: HashMap<TrackId, TrackId>,
    sources: HashMap<SourceId, SourceId>,
    devices: HashMap<DeviceId, DeviceId>,
    nodes: HashMap<NodeId, NodeId>,
    modulators: HashMap<ModulatorId, ModulatorId>,
    /// Whether the copies came from this project, so their references to
    /// tracks outside the paste still mean those tracks.
    from_here: bool,
}

impl Renumber {
    fn device(&mut self, p: &mut Project, old: DeviceId) -> DeviceId {
        *self.devices.entry(old).or_insert_with(|| p.new_device_id())
    }

    fn node(&mut self, p: &mut Project, old: NodeId) -> NodeId {
        *self.nodes.entry(old).or_insert_with(|| p.new_node_id())
    }

    fn modulator(&mut self, p: &mut Project, old: ModulatorId) -> ModulatorId {
        *self
            .modulators
            .entry(old)
            .or_insert_with(|| p.new_modulator_id())
    }

    /// What a reference to another track becomes: its copy if it was
    /// pasted too, itself if it's in this project, otherwise nothing.
    fn other_track(&self, old: TrackId) -> Option<TrackId> {
        match self.tracks.get(&old) {
            Some(new) => Some(*new),
            None => self.from_here.then_some(old),
        }
    }

    fn mod_source(&mut self, p: &mut Project, source: &mut ModSource) {
        if let ModSource::Modulator { id, .. } = source {
            *id = self.modulator(p, *id);
        }
    }

    fn target(&mut self, p: &mut Project, target: &mut AutomationTarget) {
        match target {
            AutomationTarget::DeviceParam { device, .. } => *device = self.device(p, *device),
            AutomationTarget::ModulatorParam { modulator, .. } => {
                *modulator = self.modulator(p, *modulator)
            }
            AutomationTarget::Volume
            | AutomationTarget::Pan
            | AutomationTarget::InstrumentParam(_) => {}
        }
    }
}

fn walk(track: &Track, f: &mut impl FnMut(&Track)) {
    f(track);
    for child in track.children() {
        walk(child, f);
    }
}

impl Project {
    /// Copies tracks (a group with what's in it), in display order, for the
    /// clipboard. A track inside another one listed is copied with it, once.
    pub fn copy_tracks(&self, ids: &[TrackId]) -> Vec<CopiedTrack> {
        self.outermost(ids)
            .into_iter()
            .filter_map(|id| {
                let track = self.track(id)?.clone();
                let mut used = HashSet::new();
                walk(&track, &mut |t| {
                    if let TrackKind::Audio { clips } = &t.kind {
                        used.extend(clips.iter().map(|c| c.source));
                    }
                });
                let sources = self
                    .sources
                    .iter()
                    .filter(|s| used.contains(&s.id))
                    .cloned()
                    .collect();
                Some(CopiedTrack { track, sources })
            })
            .collect()
    }

    /// Puts copies of `copied` at `position`, with new ids throughout, and
    /// adds the audio files they play (or finds them, if the project has
    /// them already). `from_here` says the copies came from this project:
    /// then a sidechain or follower listening to another track keeps it;
    /// from elsewhere, only if that track was pasted too. MIDI controller
    /// links aren't part of a track, so they stay with the original.
    /// Returns the new tracks' ids, outermost only.
    pub fn paste_tracks(
        &mut self,
        copied: &[CopiedTrack],
        position: TrackPosition,
        from_here: bool,
    ) -> EditResult<Vec<TrackId>> {
        if copied.is_empty() {
            return Err(EditError::NothingSelected);
        }
        let mut map = Renumber {
            from_here,
            ..Renumber::default()
        };
        for c in copied {
            walk(&c.track, &mut |t| {
                map.tracks.insert(t.id, self.new_track_id());
            });
        }
        for source in copied.iter().flat_map(|c| &c.sources) {
            if let std::collections::hash_map::Entry::Vacant(entry) = map.sources.entry(source.id) {
                entry.insert(self.add_source(
                    source.path.clone(),
                    source.sample_rate,
                    source.frames,
                    source.channels,
                ));
            }
        }
        let mut tracks = Vec::with_capacity(copied.len());
        for c in copied {
            let mut track = c.track.clone();
            self.renumber(&mut track, &mut map);
            tracks.push(track);
        }
        let ids: Vec<TrackId> = tracks.iter().map(|t| t.id).collect();
        let mut all = Vec::new();
        for track in &tracks {
            walk(track, &mut |t| all.push(t.id));
        }
        self.insert_tracks(tracks, position)?;
        for id in all {
            self.normalize_routing(Some(id))?;
        }
        Ok(ids)
    }

    /// Copies each track (and what's in it) to just below itself, named
    /// "<name> 2" (or the next number free). Returns the copies' ids.
    pub fn duplicate_tracks(&mut self, ids: &[TrackId]) -> EditResult<Vec<TrackId>> {
        self.check_tracks(ids)?;
        let mut copies = Vec::new();
        for id in self.outermost(ids) {
            let mut copied = self.copy_tracks(&[id]);
            for c in &mut copied {
                c.track.name = self.copy_name(&c.track.name);
            }
            let position = TrackPosition::after(self, id);
            copies.extend(self.paste_tracks(&copied, position, true)?);
        }
        Ok(copies)
    }

    /// "Bass" → "Bass 2", "Bass 2" → "Bass 3", skipping names in use.
    fn copy_name(&self, name: &str) -> String {
        let mut names = HashSet::new();
        self.visit(|t, _| {
            names.insert(t.name.clone());
        });
        let (base, first) = match name.rsplit_once(' ') {
            Some((base, n)) if let Ok(n) = n.parse::<u32>() => (base, n + 1),
            _ => (name, 2),
        };
        (first..)
            .map(|n| format!("{base} {n}"))
            .find(|candidate| !names.contains(candidate))
            .expect("some number is free")
    }

    /// Gives a chain's devices (and everything inside Patchers among them)
    /// new ids from `map`, with its graph following.
    fn renumber_chain(
        &mut self,
        devices: &mut [crate::Device],
        routing: Option<&mut crate::Routing>,
        map: &mut Renumber,
    ) {
        for device in devices.iter_mut() {
            device.id = map.device(self, device.id);
            device.sidechain = device.sidechain.and_then(|t| map.other_track(t));
            if let Some(patch) = &mut device.patch {
                self.renumber_chain(&mut patch.devices, patch.routing.as_mut(), map);
                for link in &mut patch.links {
                    link.device = map.device(self, link.device);
                }
            }
        }
        if let Some(routing) = routing {
            for node in &mut routing.nodes {
                node.id = map.node(self, node.id);
                if let NodeKind::Device(device) = &mut node.kind {
                    *device = map.device(self, *device);
                }
            }
            for cable in &mut routing.cables {
                cable.from.node = map.node(self, cable.from.node);
                cable.to.node = map.node(self, cable.to.node);
            }
        }
    }

    /// Gives `track` and everything on it new ids from `map`, and points
    /// its references at them.
    fn renumber(&mut self, track: &mut Track, map: &mut Renumber) {
        track.id = map.tracks[&track.id];
        self.renumber_chain(&mut track.devices, track.routing.as_mut(), map);
        for modulator in &mut track.modulators {
            modulator.id = map.modulator(self, modulator.id);
            modulator.listen = match modulator.listen {
                Some(Listen::Node(mut port)) => {
                    port.node = map.node(self, port.node);
                    Some(Listen::Node(port))
                }
                Some(Listen::Track(t)) => map.other_track(t).map(Listen::Track),
                listen => listen,
            };
        }
        for route in &mut track.modulations {
            route.id = self.new_mod_route_id();
            map.mod_source(self, &mut route.source);
            if let Some(scale_by) = &mut route.scale_by {
                map.mod_source(self, scale_by);
            }
            map.target(self, &mut route.target);
        }
        for lane in &mut track.automation {
            map.target(self, &mut lane.target);
        }
        match &mut track.kind {
            TrackKind::Audio { clips } => {
                for clip in clips.iter_mut() {
                    clip.id = self.new_clip_id();
                    if let Some(source) = map.sources.get(&clip.source) {
                        clip.source = *source;
                    }
                }
            }
            TrackKind::Midi { clips, .. } => {
                for clip in clips.iter_mut() {
                    clip.id = self.new_clip_id();
                    for note in clip.notes.iter_mut() {
                        note.id = self.new_note_id();
                    }
                }
            }
            TrackKind::Group { children, .. } => {
                for child in children {
                    self.renumber(child, map);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use effects::EffectRef;
    use midi::{Pitch, Tick, Velocity};

    use super::*;
    use crate::{AutomationPoint, ClipRef, ModRoute, Modulator, ModulatorRef, Note, Polarity};

    fn ids(track: &Track) -> HashSet<u32> {
        let mut ids = HashSet::new();
        walk(track, &mut |t| {
            ids.insert(t.id.0);
            ids.extend(t.devices.iter().map(|d| d.id.0));
            ids.extend(t.modulators.iter().map(|m| m.id.0));
            ids.extend(t.modulations.iter().map(|r| r.id.0));
            for clip in t.clips() {
                ids.insert(clip.id().0);
                if let ClipRef::Midi(midi) = clip {
                    ids.extend(midi.notes.iter().map(|n| n.id.0));
                }
            }
        });
        ids
    }

    /// The first MIDI track with an EQ, an LFO moving the EQ, automation of
    /// the EQ, and a clip with a note.
    fn busy_track(p: &mut Project) -> (TrackId, DeviceId) {
        let track = p.track_ids()[0];
        let eq = p
            .add_device(Some(track), EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        let target = AutomationTarget::DeviceParam {
            device: eq,
            param: "gain1".into(),
        };
        let (lfo, route) = (p.new_modulator_id(), p.new_mod_route_id());
        let t = p.track_mut(track).unwrap();
        t.modulators.push(Modulator {
            id: lfo,
            kind: ModulatorRef {
                id: "canopy.lfo".into(),
                version: "1".into(),
            },
            params: Default::default(),
            name: None,
            listen: None,
            folded: false,
        });
        t.modulations.push(ModRoute {
            id: route,
            source: ModSource::Modulator { id: lfo, output: 0 },
            target: target.clone(),
            amount: 0.5,
            polarity: Polarity::Source,
            curve: 0.0,
            scale_by: None,
            bypassed: false,
        });
        let point = AutomationPoint {
            tick: Tick::ZERO,
            value: 0.25,
            curve: 0.0,
        };
        p.set_automation(track, target, vec![point]).unwrap();
        let clip = p.add_midi_clip(track, Tick::ZERO, Tick::new(3840)).unwrap();
        let note = Note {
            id: p.new_note_id(),
            start: Tick::ZERO,
            len: Tick::new(960),
            pitch: Pitch::new(60).unwrap(),
            velocity: Velocity::MAX,
            release_velocity: Velocity::CENTER,
            muted: false,
            bend: Vec::new(),
        };
        p.set_notes(clip, vec![note]).unwrap();
        (track, eq)
    }

    #[test]
    fn a_duplicate_has_its_own_ids_and_points_at_its_own_devices() {
        let mut p = Project::with_default_tracks();
        let (track, eq) = busy_track(&mut p);
        let copies = p.duplicate_tracks(&[track]).unwrap();
        let copy = copies[0];
        assert_eq!(p.track_ids()[..2], [track, copy], "right below it");

        let (original, duplicate) = (p.track(track).unwrap(), p.track(copy).unwrap());
        assert_eq!(duplicate.name, format!("{} 2", original.name));
        assert!(ids(original).is_disjoint(&ids(duplicate)));
        let new_eq = duplicate.devices[0].id;
        assert_ne!(new_eq, eq);
        let lfo = duplicate.modulators[0].id;
        let route = &duplicate.modulations[0];
        assert_eq!(route.source, ModSource::Modulator { id: lfo, output: 0 });
        let expected = AutomationTarget::DeviceParam {
            device: new_eq,
            param: "gain1".into(),
        };
        assert_eq!(route.target, expected);
        assert_eq!(duplicate.automation[0].target, expected);
        assert_eq!(duplicate.clips().len(), 1);

        // They're apart from now on.
        p.set_device_param(new_eq, "gain1", 0.9).unwrap();
        assert_ne!(
            p.track(track).unwrap().devices[0].params.get("gain1"),
            Some(&0.9)
        );
    }

    #[test]
    fn duplicating_again_counts_up() {
        let mut p = Project::with_default_tracks();
        let track = p.track_ids()[0];
        p.track_mut(track).unwrap().name = "Bass".into();
        let two = p.duplicate_tracks(&[track]).unwrap()[0];
        let three = p.duplicate_tracks(&[track]).unwrap()[0];
        let four = p.duplicate_tracks(&[two]).unwrap()[0];
        let names: Vec<_> = [two, three, four]
            .iter()
            .map(|id| p.track(*id).unwrap().name.clone())
            .collect();
        assert_eq!(names, ["Bass 2", "Bass 3", "Bass 4"]);
    }

    #[test]
    fn a_group_copies_with_whats_in_it_once() {
        let mut p = Project::with_default_tracks();
        let tracks = p.track_ids();
        let group = p.group_tracks(&tracks[..2]).unwrap();
        let copied = p.copy_tracks(&[tracks[0], group]);
        assert_eq!(copied.len(), 1, "the child goes with its group");
        let pasted = p.paste_tracks(&copied, TrackPosition::END, true).unwrap();
        let children = p.track(pasted[0]).unwrap().children();
        assert_eq!(children.len(), 2);
        assert!(children.iter().all(|c| !tracks.contains(&c.id)));
    }

    #[test]
    fn pasting_into_another_project_brings_the_audio_and_drops_outside_tracks() {
        let mut p = Project::with_default_tracks();
        let tracks = p.track_ids();
        let (midi, audio) = (tracks[0], tracks[2]);
        let source = p.add_source("/music/loop.wav".into(), 48_000, 96_000, 2);
        p.add_audio_clip(audio, Tick::ZERO, source).unwrap();
        let comp = p
            .add_device(Some(audio), EffectRef::builtin("canopy.compressor"), None)
            .unwrap();
        p.track_mut(audio).unwrap().devices[0].sidechain = Some(midi);

        // Here, the sidechain still means the MIDI track.
        let copied = p.copy_tracks(&[audio]);
        let here = p.paste_tracks(&copied, TrackPosition::END, true).unwrap()[0];
        let pasted = p.track(here).unwrap();
        assert_eq!(pasted.devices[0].sidechain, Some(midi));
        assert_ne!(pasted.devices[0].id, comp);
        assert_eq!(p.sources.len(), 1, "the same file");

        let mut other = Project::new();
        other.add_source("/music/other.wav".into(), 44_100, 10, 1);
        let there = other
            .paste_tracks(&copied, TrackPosition::END, false)
            .unwrap()[0];
        let pasted = other.track(there).unwrap();
        assert_eq!(pasted.devices[0].sidechain, None);
        let ClipRef::Audio(clip) = pasted.clips()[0] else {
            unreachable!()
        };
        let file = other.source(clip.source).unwrap();
        assert_eq!(file.path, std::path::PathBuf::from("/music/loop.wav"));
        assert_eq!(other.sources.len(), 2);
    }

    #[test]
    fn pasting_nothing_or_nowhere_is_an_error() {
        let mut p = Project::with_default_tracks();
        assert_eq!(
            p.paste_tracks(&[], TrackPosition::END, true),
            Err(EditError::NothingSelected)
        );
        let copied = p.copy_tracks(&[p.track_ids()[0]]);
        let audio = p.track_ids()[2];
        let inside_audio = TrackPosition {
            parent: Some(audio),
            before: None,
        };
        assert!(p.paste_tracks(&copied, inside_audio, true).is_err());
    }
}
