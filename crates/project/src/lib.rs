//! The document: tracks, groups, clips, notes and tempo, plus every edit
//! and the undo history. Plain data with no threads, audio or I/O.
//!
//! The UI thread owns the [`Project`] and edits it; the engine never sees it
//! directly, only a compiled snapshot built from it.

mod automation;
mod clip;
mod color;
mod controller;
mod devices;
mod edit;
mod file;
mod generate;
mod history;
mod ids;
mod mixer;
mod modulation;
mod note;
pub mod note_tools;
mod plugin;
mod preset;
mod routing;
mod scale;
mod shared;
pub mod slicer;
mod source;
mod tempo;
mod track;

use midi::Tick;
use serde::{Deserialize, Serialize};

pub use automation::{AutomationLane, AutomationPoint, AutomationTarget, ParamId, segment_value};
pub use clip::{AudioClip, ChannelMode, ClipRef, MidiClip, Stretch, StretchMode};
pub use color::Color;
pub use controller::ControllerLink;
pub use devices::CopiedDevice;
pub use edit::{ClipData, ClipMove, EditError, NewTrack, TrackPosition};
pub use effects::{EffectRef, Format};
pub use file::{FILE_EXTENSION, LoadError, SaveError};
pub use generate::{Generate, Wave, replace_range};
pub use history::History;
pub use ids::{ClipId, DeviceId, ModRouteId, ModulatorId, NodeId, NoteId, SourceId, TrackId};
pub use mixer::{
    MAX_VOLUME_DB, Mixer, SILENCE_DB, normalized_to_pan, normalized_to_volume, pan_to_normalized,
    volume_gain, volume_to_normalized,
};
pub use modulation::{Listen, ModRoute, ModSource, Modulator, ModulatorRef, Polarity};
pub use note::{BendPoint, Note};
pub use plugin::{Blob, PluginData};
pub use preset::PresetFile;
pub use routing::{
    Cable, CableKey, GraphNode, Join, MAX_CROSSOVERS, NODE_SPACING, NodeKind, PortRef, Routing,
    RoutingError, Slope, Split,
};
pub use scale::{NOTE_NAMES, Scale, ScaleMode};
pub use shared::Shared;
pub use source::AudioSource;
pub use tempo::{BarBeat, Tempo, TempoMap, TimeSignature};
pub use track::{Device, Track, TrackKind};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub tempo: TempoMap,
    /// Top level, in display order.
    pub tracks: Vec<Track>,
    pub master: Mixer,
    /// The master's effects, before its fader.
    #[serde(default)]
    pub master_devices: Vec<Device>,
    /// How the sound flows through the master's effects when it isn't a
    /// straight line (see `routing.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_routing: Option<Routing>,
    pub sources: Vec<AudioSource>,
    /// MIDI controllers linked to controls.
    #[serde(default)]
    pub controller_links: Vec<ControllerLink>,
    /// The song's key, if set: shown in the piano roll, and what chords fit.
    #[serde(default)]
    pub scale: Option<Scale>,
    /// The stretch playback goes round, if one is set.
    #[serde(default)]
    pub loop_region: Option<LoopRegion>,
    /// Where the arrangement was last scrolled and zoomed to, so the
    /// project opens as it was left. A view setting, like a fold: not an
    /// undo step (see [`Project::set_arrangement_view`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrangement_view: Option<ArrangementView>,
    /// One counter for every kind of id, so ids are unique project-wide.
    next_id: u32,
}

/// A stretch of the song that playback can go round and round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRegion {
    pub start: Tick,
    pub end: Tick,
    /// Playback goes round it; off, it's just marked.
    pub on: bool,
}

/// How the arrangement is looked at: how far it's scrolled and zoomed
/// (see [`Project::arrangement_view`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ArrangementView {
    /// The tick at the left edge.
    pub start: f64,
    /// The zoom: how wide a tick is on screen, in points.
    pub px_per_tick: f64,
    /// How tall a track's row is, in points.
    pub track_height: f32,
    /// How far the rows are scrolled down, in points.
    pub scroll: f32,
}

impl ArrangementView {
    /// Whether every number is one a view can have: finite, the zoom and
    /// the rows' height above zero, and nothing before the start.
    pub fn is_valid(&self) -> bool {
        self.start.is_finite()
            && self.start >= 0.0
            && self.px_per_tick.is_finite()
            && self.px_per_tick > 0.0
            && self.track_height.is_finite()
            && self.track_height > 0.0
            && self.scroll.is_finite()
            && self.scroll >= 0.0
    }
}

/// A track as it appears on screen: what's visible once folded groups hide
/// their children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub id: TrackId,
    /// 0 for top-level tracks, 1 inside one group, and so on.
    pub depth: usize,
}

impl Default for Project {
    fn default() -> Self {
        Project::new()
    }
}

impl Project {
    /// An empty project at 120 BPM.
    pub fn new() -> Project {
        Project {
            tempo: TempoMap::default(),
            tracks: Vec::new(),
            master: Mixer::default(),
            master_devices: Vec::new(),
            master_routing: None,
            sources: Vec::new(),
            controller_links: Vec::new(),
            scale: None,
            loop_region: None,
            arrangement_view: None,
            next_id: 1,
        }
    }

    /// What a new session starts with: two MIDI tracks and two audio tracks.
    pub fn with_default_tracks() -> Project {
        let mut project = Project::new();
        for kind in [
            NewTrack::Midi,
            NewTrack::Midi,
            NewTrack::Audio,
            NewTrack::Audio,
        ] {
            project
                .add_track(kind, TrackPosition::END)
                .expect("adding at the end always works");
        }
        project
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn new_note_id(&mut self) -> NoteId {
        NoteId(self.alloc_id())
    }

    pub(crate) fn new_clip_id(&mut self) -> ClipId {
        ClipId(self.alloc_id())
    }

    pub(crate) fn new_device_id(&mut self) -> DeviceId {
        DeviceId(self.alloc_id())
    }

    pub(crate) fn new_track_id(&mut self) -> TrackId {
        TrackId(self.alloc_id())
    }

    pub fn new_node_id(&mut self) -> NodeId {
        NodeId(self.alloc_id())
    }

    pub fn new_modulator_id(&mut self) -> ModulatorId {
        ModulatorId(self.alloc_id())
    }

    pub fn new_mod_route_id(&mut self) -> ModRouteId {
        ModRouteId(self.alloc_id())
    }

    /// Visits every track, parents before children, with its depth.
    pub fn visit<'a>(&'a self, mut f: impl FnMut(&'a Track, usize)) {
        fn go<'a>(tracks: &'a [Track], depth: usize, f: &mut impl FnMut(&'a Track, usize)) {
            for track in tracks {
                f(track, depth);
                go(track.children(), depth + 1, f);
            }
        }
        go(&self.tracks, 0, &mut f);
    }

    /// Visits every track mutably, parents before children.
    pub(crate) fn visit_mut(&mut self, mut f: impl FnMut(&mut Track)) {
        fn go(tracks: &mut [Track], f: &mut impl FnMut(&mut Track)) {
            for track in tracks {
                f(track);
                if let Some(children) = track.children_mut() {
                    go(children, f);
                }
            }
        }
        go(&mut self.tracks, &mut f);
    }

    /// Every track id in display order, including inside folded groups.
    pub fn track_ids(&self) -> Vec<TrackId> {
        let mut ids = Vec::new();
        self.visit(|t, _| ids.push(t.id));
        ids
    }

    /// The tracks on screen, top to bottom.
    pub fn rows(&self) -> Vec<Row> {
        fn go(tracks: &[Track], depth: usize, rows: &mut Vec<Row>) {
            for track in tracks {
                rows.push(Row {
                    id: track.id,
                    depth,
                });
                if !track.is_folded() {
                    go(track.children(), depth + 1, rows);
                }
            }
        }
        let mut rows = Vec::new();
        go(&self.tracks, 0, &mut rows);
        rows
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        fn find(tracks: &[Track], id: TrackId) -> Option<&Track> {
            tracks.iter().find_map(|t| {
                if t.id == id {
                    Some(t)
                } else {
                    find(t.children(), id)
                }
            })
        }
        find(&self.tracks, id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        track::find_mut(&mut self.tracks, id)
    }

    /// The group containing `id` (`None` at the top level) and its index
    /// there. `None` if there's no such track.
    pub fn location(&self, id: TrackId) -> Option<(Option<TrackId>, usize)> {
        fn find(
            tracks: &[Track],
            parent: Option<TrackId>,
            id: TrackId,
        ) -> Option<(Option<TrackId>, usize)> {
            for (index, track) in tracks.iter().enumerate() {
                if track.id == id {
                    return Some((parent, index));
                }
                if let Some(found) = find(track.children(), Some(track.id), id) {
                    return Some(found);
                }
            }
            None
        }
        find(&self.tracks, None, id)
    }

    pub fn parent(&self, id: TrackId) -> Option<TrackId> {
        self.location(id).and_then(|(parent, _)| parent)
    }

    /// True if `id` is inside `ancestor`, at any depth. A track is not its own
    /// ancestor.
    pub fn is_inside(&self, id: TrackId, ancestor: TrackId) -> bool {
        let mut current = self.parent(id);
        while let Some(parent) = current {
            if parent == ancestor {
                return true;
            }
            current = self.parent(parent);
        }
        false
    }

    pub(crate) fn siblings_mut(&mut self, parent: Option<TrackId>) -> Option<&mut Vec<Track>> {
        match parent {
            None => Some(&mut self.tracks),
            Some(id) => self.track_mut(id)?.children_mut(),
        }
    }

    pub fn source(&self, id: SourceId) -> Option<&AudioSource> {
        self.sources.iter().find(|s| s.id == id)
    }

    /// The clip with this id and the track it's on.
    pub fn clip(&self, id: ClipId) -> Option<(TrackId, ClipRef<'_>)> {
        let mut found = None;
        self.visit(|track, _| {
            if found.is_none()
                && let Some(clip) = track.clips().into_iter().find(|c| c.id() == id)
            {
                found = Some((track.id, clip));
            }
        });
        found
    }

    /// Where a clip ends on the timeline. An audio clip's length in ticks
    /// depends on the tempo, so it's computed here rather than stored.
    pub fn clip_end(&self, clip: ClipRef<'_>) -> Tick {
        match clip {
            ClipRef::Midi(c) => c.end(),
            ClipRef::Audio(c) => c.start + self.audio_duration(c, c.played()),
        }
    }

    /// How long `frames` of an audio clip's material last on the timeline,
    /// at its speed. Rounded up, so any audio at all lasts at least a tick
    /// and never looks like it ends before it does.
    pub fn audio_duration(&self, clip: &AudioClip, frames: u64) -> Tick {
        tempo::ticks_up(self.audio_ticks(clip, frames as f64))
    }

    /// The same, unrounded, for drawing.
    pub fn audio_ticks(&self, clip: &AudioClip, frames: f64) -> f64 {
        let rate = self.source(clip.source).map_or(48_000, |s| s.sample_rate);
        let speed = clip.stretch.effective_speed(&self.tempo);
        self.tempo.frames_to_ticks(frames / speed, rate)
    }

    /// Material frames of an audio clip that `ticks` of timeline cover.
    pub fn audio_material(&self, clip: &AudioClip, ticks: f64) -> f64 {
        let rate = self.source(clip.source).map_or(48_000, |s| s.sample_rate);
        self.tempo.ticks_to_frames(ticks, rate) * clip.stretch.effective_speed(&self.tempo)
    }

    /// How far an audio clip's edges can be dragged: unlooped clips stop at
    /// the ends of their file.
    pub fn audio_clip_limits(&self, clip: &AudioClip) -> (Tick, Tick) {
        if clip.looped {
            return (Tick::new(i64::MIN / 4), Tick::new(i64::MAX / 4));
        }
        let file = self.source(clip.source).map_or(u64::MAX / 4, |s| s.frames);
        let floor = |frames: u64| Tick::new(self.audio_ticks(clip, frames as f64).floor() as i64);
        let region_end = clip.source_offset + clip.source_len;
        // Material before the clip's start, and after its end.
        let (before, after) = if clip.reversed {
            (file.saturating_sub(region_end), region_end)
        } else {
            (clip.source_offset, file.saturating_sub(clip.source_offset))
        };
        (clip.start - floor(before), clip.start + floor(after))
    }

    /// Where the last clip ends, or zero for an empty project.
    pub fn end(&self) -> Tick {
        let mut end = Tick::ZERO;
        self.visit(|track, _| {
            for clip in track.clips() {
                end = end.max(self.clip_end(clip));
            }
        });
        end
    }

    /// True if any track is soloed, which silences every track that isn't.
    pub fn any_solo(&self) -> bool {
        let mut any = false;
        self.visit(|t, _| any |= t.mixer.solo);
        any
    }
}
