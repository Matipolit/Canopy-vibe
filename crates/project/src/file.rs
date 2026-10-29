//! Project files: the [`Project`] as RON text. Reading and writing the file
//! itself is the caller's job; this crate does no I/O.
//!
//! A project that uses nothing newer than format 1 (no effect graphs, no
//! modulation, no plugins) is written as format 1, exactly as before those
//! existed, so older canopy still opens it. One that does is written as
//! format 2, which older canopy refuses rather than silently dropping what
//! it can't read.

use std::collections::HashSet;
use std::fmt;

use midi::Tick;
use serde::{Deserialize, Serialize};

use crate::routing::{normalize, sort_devices};
use crate::{
    AudioSource, AutomationTarget, ClipId, ClipRef, Device, DeviceId, Listen, Mixer, ModRouteId,
    ModulatorId, NodeId, NodeKind, NoteId, Project, Routing, Split, Stretch, Tempo, Track, TrackId,
    TrackKind,
};

pub const FILE_EXTENSION: &str = "canopy";

/// The newest format this build reads and writes. Bumped whenever a change
/// would make old files load wrongly.
const FORMAT_VERSION: u32 = 2;

/// What projects that need nothing newer are written as.
const PLAIN_FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    format: u32,
    project: Project,
}

/// Why a project couldn't be turned into text.
#[derive(Debug, Clone, PartialEq)]
pub struct SaveError(pub String);

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SaveError {}

#[derive(Debug, Clone, PartialEq)]
pub enum LoadError {
    Parse(String),
    /// Written by a newer canopy.
    TooNew(u32),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(err) => write!(f, "not a readable project file: {err}"),
            Self::TooNew(version) => write!(
                f,
                "this project was saved by a newer canopy (format {version}, this one reads {FORMAT_VERSION})"
            ),
        }
    }
}

impl std::error::Error for LoadError {}

/// Just enough of a file to read its version before trusting the rest.
#[derive(Deserialize)]
struct Header {
    format: u32,
}

impl Project {
    /// Fails only for things RON can't represent, such as a file path that
    /// isn't valid UTF-8.
    pub fn to_file_string(&self) -> Result<String, SaveError> {
        let file = ProjectFile {
            format: self.file_format(),
            project: self.clone(),
        };
        let config = ron::ser::PrettyConfig::new().indentor("  ".to_owned());
        ron::ser::to_string_pretty(&file, config).map_err(|err| SaveError(err.to_string()))
    }

    /// The format the project is saved as: [`PLAIN_FORMAT`] unless it uses
    /// effect graphs, modulation or plugins.
    pub fn file_format(&self) -> u32 {
        let is_plugin = |d: &Device| d.plugin.is_some() || !d.effect.format.is_builtin();
        let modulator_param =
            |t: &AutomationTarget| matches!(t, AutomationTarget::ModulatorParam { .. });
        let mut newer = self.master_routing.is_some()
            || self.master_devices.iter().any(is_plugin)
            || self
                .controller_links
                .iter()
                .any(|l| modulator_param(&l.target));
        self.visit(|track, _| {
            newer |= track.routing.is_some()
                || !track.modulators.is_empty()
                || !track.modulations.is_empty()
                || track.devices.iter().any(is_plugin)
                || track.automation.iter().any(|l| modulator_param(&l.target))
                || matches!(
                    &track.kind,
                    TrackKind::Midi { instrument, plugin, .. }
                        if plugin.is_some() || !instrument.format.is_builtin()
                );
        });
        if newer { FORMAT_VERSION } else { PLAIN_FORMAT }
    }

    pub fn from_file_string(text: &str) -> Result<Project, LoadError> {
        let options = ron::Options::default();
        // The whole file first: reading just the header skips the project
        // in a way that takes time quadratic in its size (35 s for 4 MB).
        // Only a file that fails gets its header read, to tell one from a
        // newer canopy apart from a broken one.
        let file: ProjectFile = match options.from_str(text) {
            Ok(file) => file,
            Err(err) => {
                return Err(match options.from_str::<Header>(text) {
                    Ok(header) if header.format > FORMAT_VERSION => {
                        LoadError::TooNew(header.format)
                    }
                    _ => LoadError::Parse(err.to_string()),
                });
            }
        };
        if file.format > FORMAT_VERSION {
            return Err(LoadError::TooNew(file.format));
        }
        let mut project = file.project;
        project.repair();
        Ok(project)
    }

    /// Puts a hand-edited or damaged file back into a state every edit and
    /// the engine can rely on: ids unique, lengths positive, audio clips
    /// inside their files, values finite, everything sorted.
    fn repair(&mut self) {
        let bpm = self.tempo.tempo.to_bpm();
        let bpm = if bpm.is_finite() { bpm } else { 120.0 };
        self.tempo.tempo = Tempo::from_bpm(bpm.clamp(Tempo::MIN_BPM, Tempo::MAX_BPM));
        let signature = &mut self.tempo.time_signature;
        signature.numerator = signature.numerator.clamp(1, 32);
        if !matches!(signature.denominator, 1 | 2 | 4 | 8 | 16 | 32) {
            signature.denominator = 4;
        }
        repair_mixer(&mut self.master);
        if let Some(scale) = &mut self.scale {
            scale.root %= 12;
        }
        // In order and not before the start; an empty one goes.
        let region = self.loop_region.take();
        let _ = self.set_loop(region);
        // A view no screen can show goes; the arrangement opens as new.
        let view = self.arrangement_view.take();
        let _ = self.set_arrangement_view(view);
        // Links to tracks that aren't there, or out-of-range numbers.
        let mut tracks = HashSet::new();
        self.visit(|t, _| {
            tracks.insert(t.id);
        });
        self.controller_links.retain(|l| {
            l.channel < 16 && l.controller < 128 && l.track.is_none_or(|t| tracks.contains(&t))
        });

        // Keep the first of any duplicated source; clips can't say which
        // copy they meant.
        let mut seen_sources = HashSet::new();
        self.sources.retain(|s| seen_sources.insert(s.id));

        let mut max = self.sources.iter().map(|s| s.id.0).max().unwrap_or(0);
        for device in &self.master_devices {
            max = max.max(device.id.0);
        }
        let node_ids = |routing: &Option<Routing>| {
            routing
                .iter()
                .flat_map(|r| r.nodes.iter().map(|n| n.id.0))
                .max()
                .unwrap_or(0)
        };
        max = max.max(node_ids(&self.master_routing));
        self.visit(|track, _| {
            max = max.max(track.id.0).max(node_ids(&track.routing));
            for device in &track.devices {
                max = max.max(device.id.0);
            }
            for modulator in &track.modulators {
                max = max.max(modulator.id.0);
            }
            for route in &track.modulations {
                max = max.max(route.id.0);
            }
            for clip in track.clips() {
                max = max.max(clip.id().0);
                if let ClipRef::Midi(midi) = clip {
                    for note in &midi.notes {
                        max = max.max(note.id.0);
                    }
                }
            }
        });
        self.next_id = self.next_id.max(max + 1);

        let mut ids = Ids {
            seen: seen_sources.iter().map(|s| s.0).collect(),
            next: &mut self.next_id,
        };
        repair_devices(&mut self.master_devices, &mut ids);
        repair_routing(&mut self.master_devices, &mut self.master_routing, &mut ids);
        let sources = &self.sources;
        for track in &mut self.tracks {
            repair_track(track, sources, &mut ids);
        }
        self.prune_track_references();
        // Links to modulators that aren't there (on the track they name).
        let mut modulators = HashSet::new();
        self.visit(|t, _| modulators.extend(t.modulators.iter().map(|m| (t.id, m.id))));
        self.controller_links
            .retain(|l| match (&l.target, l.track) {
                (AutomationTarget::ModulatorParam { modulator, .. }, Some(track)) => {
                    modulators.contains(&(track, *modulator))
                }
                (AutomationTarget::ModulatorParam { .. }, None) => false,
                _ => true,
            });
    }
}

/// A graph that can't play goes, leaving the chain linear in the order the
/// graph had (as near as a broken graph says). One that can gets unique
/// node ids, finite values and canonical form. Returns the node ids that
/// changed, old and new, for followers listening to them.
fn repair_routing(
    devices: &mut [Device],
    routing: &mut Option<Routing>,
    ids: &mut Ids,
) -> Vec<(NodeId, NodeId)> {
    let Some(graph) = routing else {
        return Vec::new();
    };
    for node in &mut graph.nodes {
        if !node.pos.iter().all(|v| v.is_finite()) {
            node.pos = [0.0; 2];
        }
        if let NodeKind::Split(Split::Bands { crossovers, .. }) = &mut node.kind {
            for hz in crossovers.iter_mut() {
                *hz = finite_or(*hz, 1000.0).clamp(20.0, 20_000.0);
            }
            crossovers.sort_by(f32::total_cmp);
        }
    }
    for cable in &mut graph.cables {
        cable.gain_db =
            finite_or(cable.gain_db, 0.0).clamp(crate::SILENCE_DB, crate::MAX_VOLUME_DB);
    }
    if graph.validate(devices).is_err() {
        let order: Vec<DeviceId> = graph
            .order_breaking_loops()
            .into_iter()
            .filter_map(|id| match graph.node(id)?.kind {
                NodeKind::Device(device) => Some(device),
                _ => None,
            })
            .collect();
        sort_devices(devices, &order);
        *routing = None;
        return Vec::new();
    }
    let mut changed = Vec::new();
    for node in &mut graph.nodes {
        let new = NodeId(ids.unique(node.id.0));
        if new != node.id {
            changed.push((node.id, new));
            node.id = new;
        }
    }
    let renumber = |id: &mut NodeId| {
        if let Some((_, new)) = changed.iter().find(|(old, _)| old == id) {
            *id = *new;
        }
    };
    for cable in &mut graph.cables {
        renumber(&mut cable.from.node);
        renumber(&mut cable.to.node);
    }
    normalize(devices, routing, || NodeId(ids.fresh()));
    changed
}

/// Unique ids and sane values for a track's modulators and routes, and no
/// routes whose source or target isn't on the track. `nodes` are the graph
/// node ids that changed, for followers listening to them.
fn repair_modulation(track: &mut Track, ids: &mut Ids, nodes: &[(NodeId, NodeId)]) {
    for modulator in &mut track.modulators {
        modulator.id = ModulatorId(ids.unique(modulator.id.0));
        modulator.params.retain(|_, v| v.is_finite());
        if let Some(Listen::Node(port)) = &mut modulator.listen
            && let Some((_, new)) = nodes.iter().find(|(old, _)| *old == port.node)
        {
            port.node = *new;
        }
    }
    for route in &mut track.modulations {
        route.id = ModRouteId(ids.unique(route.id.0));
        route.amount = finite_or(route.amount, 0.0).clamp(-1.0, 1.0);
        route.curve = finite_or(route.curve, 0.0).clamp(-1.0, 1.0);
    }
    track.prune_modulation();
    let modulators: HashSet<ModulatorId> = track.modulators.iter().map(|m| m.id).collect();
    track.automation.retain(|lane| match &lane.target {
        AutomationTarget::ModulatorParam { modulator, .. } => modulators.contains(modulator),
        _ => true,
    });
}

/// Unique ids and finite values. Effects this version doesn't know stay
/// (and pass the sound through), so saving doesn't lose them.
fn repair_devices(devices: &mut [crate::Device], ids: &mut Ids) {
    for device in devices {
        device.id = crate::DeviceId(ids.unique(device.id.0));
        device.params.retain(|_, v| v.is_finite());
    }
}

/// Hands out ids, replacing any that were already used.
struct Ids<'a> {
    seen: HashSet<u32>,
    next: &'a mut u32,
}

impl Ids<'_> {
    fn unique(&mut self, id: u32) -> u32 {
        if self.seen.insert(id) {
            return id;
        }
        self.fresh()
    }

    fn fresh(&mut self) -> u32 {
        let fresh = *self.next;
        *self.next += 1;
        self.seen.insert(fresh);
        fresh
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn repair_mixer(mixer: &mut Mixer) {
    mixer.volume_db =
        finite_or(mixer.volume_db, 0.0).clamp(crate::SILENCE_DB, crate::MAX_VOLUME_DB);
    mixer.pan = finite_or(mixer.pan, 0.0).clamp(-1.0, 1.0);
}

fn repair_track(track: &mut Track, sources: &[AudioSource], ids: &mut Ids) {
    track.id = TrackId(ids.unique(track.id.0));
    repair_mixer(&mut track.mixer);
    repair_devices(&mut track.devices, ids);
    let nodes = repair_routing(&mut track.devices, &mut track.routing, ids);
    repair_modulation(track, ids, &nodes);
    for lane in &mut track.automation {
        lane.points.retain(|p| p.value.is_finite());
        for point in &mut lane.points {
            point.value = point.value.clamp(0.0, 1.0);
            point.curve = finite_or(point.curve, 0.0).clamp(-1.0, 1.0);
        }
        lane.sort();
    }
    match &mut track.kind {
        TrackKind::Midi {
            clips,
            params,
            tables,
            slices,
            ..
        } => {
            params.retain(|_, v| v.is_finite());
            if !slices.is_empty() {
                *slices = crate::slicer::normalize(std::mem::take(slices));
            }
            while tables.last().is_some_and(Option::is_none) {
                tables.pop();
            }
            clips.retain(|c| c.length > Tick::ZERO);
            for clip in clips.iter_mut() {
                clip.id = ClipId(ids.unique(clip.id.0));
                for note in &mut clip.notes {
                    note.id = NoteId(ids.unique(note.id.0));
                    note.len = note.len.max(Tick::new(1));
                }
                clip.notes.sort_by_key(|n| (n.start, n.pitch));
            }
            clips.sort_by_key(|c| c.start);
        }
        TrackKind::Audio { clips } => {
            clips.retain_mut(|clip| {
                let Some(source) = sources.iter().find(|s| s.id == clip.source) else {
                    return false;
                };
                if clip.source_offset >= source.frames {
                    return false;
                }
                clip.source_len = clip.source_len.min(source.frames - clip.source_offset);
                clip.gain_db = finite_or(clip.gain_db, 0.0);
                clip.fade_in_curve = finite_or(clip.fade_in_curve, 0.0).clamp(-1.0, 1.0);
                clip.fade_out_curve = finite_or(clip.fade_out_curve, 0.0).clamp(-1.0, 1.0);
                let stretch = &mut clip.stretch;
                let finite = |x: f64, or: f64| if x.is_finite() { x } else { or };
                stretch.speed =
                    finite(stretch.speed, 1.0).clamp(Stretch::MIN_SPEED, Stretch::MAX_SPEED);
                stretch.transpose = finite(stretch.transpose, 0.0).clamp(-48.0, 48.0);
                if stretch
                    .original_bpm
                    .is_some_and(|bpm| !(bpm.is_finite() && bpm > 0.0))
                {
                    stretch.original_bpm = None;
                }
                if clip.looped && clip.play_len == 0 {
                    clip.play_len = clip.source_len;
                }
                clip.loop_phase = clip.loop_phase.min(clip.source_len.saturating_sub(1));
                clip.clamp_fades();
                clip.source_len > 0
            });
            for clip in clips.iter_mut() {
                clip.id = ClipId(ids.unique(clip.id.0));
            }
            clips.sort_by_key(|c| c.start);
        }
        TrackKind::Group { children, .. } => {
            for child in children {
                repair_track(child, sources, ids);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use midi::{Pitch, Tick, Velocity};

    use super::*;
    use crate::{AutomationPoint, AutomationTarget, EditError, NewTrack, Note, TrackPosition};

    #[test]
    fn round_trips_through_text() {
        let mut project = Project::with_default_tracks();
        project.set_tempo_bpm(97.0);
        let midi = project.tracks[0].id;
        let clip = project
            .add_midi_clip(midi, Tick::ZERO, Tick::new(3840))
            .unwrap();
        let note = Note {
            id: project.new_note_id(),
            start: Tick::new(480),
            len: Tick::new(240),
            pitch: Pitch::new(64).unwrap(),
            velocity: Velocity::from_midi1(100),
            release_velocity: Velocity::CENTER,
            muted: false,
            bend: Vec::new(),
        };
        project.set_notes(clip, vec![note]).unwrap();
        project
            .set_instrument_param(midi, "cutoff", 1234.0)
            .unwrap();
        project
            .set_automation(
                midi,
                AutomationTarget::Volume,
                vec![AutomationPoint {
                    tick: Tick::ZERO,
                    value: 0.5,
                    curve: 0.0,
                }],
            )
            .unwrap();
        let group = project.group_tracks(&[project.tracks[2].id]).unwrap();
        let source = project.add_source("/tmp/kick.wav".into(), 44_100, 44_100, 2);
        let audio = project.track(group).unwrap().children()[0].id;
        project
            .add_audio_clip(audio, Tick::QUARTER, source)
            .unwrap();

        let text = project.to_file_string().unwrap();
        let loaded = Project::from_file_string(&text).unwrap();
        assert_eq!(loaded, project);

        let mut loaded = loaded;
        let fresh = loaded
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        assert!(project.track(fresh).is_none(), "new ids don't collide");
    }

    #[test]
    fn slices_save_only_when_set() {
        let mut project = Project::with_default_tracks();
        let midi = project.tracks[0].id;
        let plain = project.to_file_string().unwrap();
        assert!(!plain.contains("slices"), "no slices, nothing written");

        project.set_slices(midi, vec![0, 22_050, 44_100]).unwrap();
        let text = project.to_file_string().unwrap();
        assert!(text.contains("slices"), "{text}");
        let loaded = Project::from_file_string(&text).unwrap();
        assert_eq!(loaded, project);
        assert_eq!(loaded.track(midi).unwrap().slices(), [0, 22_050, 44_100]);

        // Cleared, the track saves as it did before it had any.
        project.set_slices(midi, Vec::new()).unwrap();
        assert_eq!(project.to_file_string().unwrap(), plain);
        // Out of order by hand, they're put right on load.
        let messy = text.replacen(
            "            0,\n            22050,",
            "            44100,\n            22050,\n            22050,",
            1,
        );
        assert_ne!(messy, text);
        let repaired = Project::from_file_string(&messy).unwrap();
        assert_eq!(repaired.track(midi).unwrap().slices(), [0, 22_050, 44_100]);
    }

    #[test]
    fn wavetable_files_round_trip_and_old_files_load_without_them() {
        let mut project = Project::with_default_tracks();
        let midi = project.tracks[0].id;
        let plain = project.to_file_string().unwrap();
        assert!(!plain.contains("tables"), "no tables, nothing written");

        project
            .set_wavetable_file(midi, 2, Some("/tmp/vital/osc 3.wav".into()))
            .unwrap();
        project
            .set_wavetable_file(midi, 0, Some("/tmp/vital/osc 1.wav".into()))
            .unwrap();
        let text = project.to_file_string().unwrap();
        assert!(text.contains("tables"), "{text}");
        let loaded = Project::from_file_string(&text).unwrap();
        assert_eq!(loaded, project);
        let track = loaded.track(midi).unwrap();
        assert_eq!(
            track.wavetable_file(0),
            Some(std::path::Path::new("/tmp/vital/osc 1.wav"))
        );
        assert_eq!(track.wavetable_file(1), None);
        assert_eq!(
            track.wavetable_file(2),
            Some(std::path::Path::new("/tmp/vital/osc 3.wav"))
        );

        // A file from before tables existed has no field at all: none.
        let old = Project::from_file_string(&plain).unwrap();
        assert!(old.track(midi).unwrap().wavetable_files().is_empty());
        // Trailing "none"s written by hand are dropped.
        let padded = text.replacen(
            "Some(\"/tmp/vital/osc 3.wav\")",
            "Some(\"/tmp/vital/osc 3.wav\"), None, None",
            1,
        );
        assert_ne!(padded, text);
        let repaired = Project::from_file_string(&padded).unwrap();
        assert_eq!(repaired.track(midi).unwrap().wavetable_files().len(), 3);
    }

    #[test]
    fn the_arrangement_view_is_saved_and_old_files_open_without_one() {
        let mut project = Project::with_default_tracks();
        let plain = project.to_file_string().unwrap();
        assert!(!plain.contains("arrangement_view"), "none, nothing written");

        let view = crate::ArrangementView {
            start: 7680.5,
            px_per_tick: 0.0625,
            track_height: 80.0,
            scroll: 120.0,
        };
        project.set_arrangement_view(Some(view)).unwrap();
        let text = project.to_file_string().unwrap();
        assert!(text.contains("arrangement_view"), "{text}");
        assert_eq!(project.file_format(), PLAIN_FORMAT, "older canopy reads it");
        let loaded = Project::from_file_string(&text).unwrap();
        assert_eq!(loaded.arrangement_view, Some(view));

        // A file from before views were saved opens with none.
        assert_eq!(
            Project::from_file_string(&plain).unwrap().arrangement_view,
            None
        );

        // A view no screen could show is refused, and dropped on loading.
        let zero = crate::ArrangementView {
            px_per_tick: 0.0,
            ..view
        };
        assert_eq!(
            project.set_arrangement_view(Some(zero)),
            Err(EditError::InvalidView)
        );
        assert_eq!(project.arrangement_view, Some(view), "the old one stays");
        let broken = text.replacen("px_per_tick: 0.0625", "px_per_tick: -1.0", 1);
        assert_ne!(broken, text);
        assert_eq!(
            Project::from_file_string(&broken).unwrap().arrangement_view,
            None
        );
    }

    #[test]
    fn rejects_garbage_and_newer_files() {
        assert!(matches!(
            Project::from_file_string("not ron at all"),
            Err(LoadError::Parse(_))
        ));
        let newer = Project::new()
            .to_file_string()
            .unwrap()
            .replacen("format: 1", "format: 99", 1);
        assert_eq!(
            Project::from_file_string(&newer),
            Err(LoadError::TooNew(99))
        );
    }
}
