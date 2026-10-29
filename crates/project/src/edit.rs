//! Every change to a [`Project`]. Each method is one user-level edit.
//!
//! On error a method may leave the project partly changed; callers go
//! through [`crate::History::edit`], which rolls the project back.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::PathBuf;

use instruments::InstrumentRef;
use midi::Tick;

use crate::{
    AudioClip, AudioSource, AutomationLane, AutomationPoint, AutomationTarget, BendPoint,
    ChannelMode, ClipId, ClipRef, Color, DeviceId, MidiClip, Mixer, ModRouteId, ModSource,
    ModulatorId, NodeId, Note, NoteId, Project, RoutingError, Shared, SourceId, Stretch, Tempo,
    Track, TrackId, TrackKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    NoSuchTrack(TrackId),
    NoSuchClip(ClipId),
    NoSuchNote(NoteId),
    NoSuchSource(SourceId),
    /// The track can't hold this, e.g. an audio clip on a MIDI track.
    WrongTrackKind(TrackId),
    /// Grouping needs tracks that sit in the same group.
    NotSiblings,
    /// A group can't be moved into itself.
    IntoItself,
    EmptyClip,
    NothingSelected,
    NoSuchDevice(DeviceId),
    /// An effect this version of canopy doesn't have.
    UnknownEffect(String),
    /// A node that isn't in the chain's graph.
    NoSuchNode(NodeId),
    NoSuchModulator(ModulatorId),
    NoSuchRoute(ModRouteId),
    /// A modulator this version of canopy doesn't have.
    UnknownModulator(String),
    /// The edit would leave a graph that can't play.
    Routing(RoutingError),
    /// A view no screen can show: a zoom of zero, a number that isn't
    /// finite.
    InvalidView,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchTrack(id) => write!(f, "track {id} doesn't exist"),
            Self::NoSuchClip(id) => write!(f, "clip {id} doesn't exist"),
            Self::NoSuchNote(id) => write!(f, "note {id} doesn't exist"),
            Self::NoSuchSource(id) => write!(f, "audio source {id} doesn't exist"),
            Self::WrongTrackKind(_) => write!(f, "that doesn't go on this kind of track"),
            Self::NotSiblings => write!(f, "only tracks in the same group can be grouped"),
            Self::IntoItself => write!(f, "a group can't go inside itself"),
            Self::EmptyClip => write!(f, "a clip can't be shorter than a tick"),
            Self::NothingSelected => write!(f, "nothing selected"),
            Self::NoSuchDevice(id) => write!(f, "effect {id} doesn't exist"),
            Self::UnknownEffect(id) => write!(f, "there's no effect called {id}"),
            Self::NoSuchNode(id) => write!(f, "node {id} doesn't exist"),
            Self::NoSuchModulator(id) => write!(f, "modulator {id} doesn't exist"),
            Self::NoSuchRoute(id) => write!(f, "modulation {id} doesn't exist"),
            Self::UnknownModulator(id) => write!(f, "there's no modulator called {id}"),
            Self::Routing(err) => write!(f, "{err}"),
            Self::InvalidView => write!(f, "that view can't be shown"),
        }
    }
}

impl std::error::Error for EditError {}

impl From<RoutingError> for EditError {
    fn from(err: RoutingError) -> EditError {
        EditError::Routing(err)
    }
}

pub type EditResult<T = ()> = Result<T, EditError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewTrack {
    Midi,
    Audio,
    Group,
}

/// Where a track goes: inside `parent` (`None` for the top level), just
/// before `before` (`None` for the end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackPosition {
    pub parent: Option<TrackId>,
    pub before: Option<TrackId>,
}

impl TrackPosition {
    pub const END: TrackPosition = TrackPosition {
        parent: None,
        before: None,
    };

    /// Right after `id`, in the same group.
    pub fn after(project: &Project, id: TrackId) -> TrackPosition {
        let Some((parent, index)) = project.location(id) else {
            return TrackPosition::END;
        };
        let siblings = match parent {
            None => &project.tracks[..],
            Some(p) => project.track(p).map_or(&[][..], |t| t.children()),
        };
        TrackPosition {
            parent,
            before: siblings.get(index + 1).map(|t| t.id),
        }
    }
}

/// Put `clip` on `track` starting at `start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipMove {
    pub clip: ClipId,
    pub track: TrackId,
    pub start: Tick,
}

impl Project {
    pub fn add_track(&mut self, kind: NewTrack, position: TrackPosition) -> EditResult<TrackId> {
        let id = self.new_track_id();
        let count = self.track_ids().len() + 1;
        let (name, kind) = match kind {
            NewTrack::Midi => (
                format!("{count} MIDI"),
                TrackKind::Midi {
                    instrument: InstrumentRef::default(),
                    params: BTreeMap::new(),
                    clips: Vec::new().into(),
                    file: None,
                    tables: Vec::new(),
                    slices: Vec::new(),
                    plugin: None,
                },
            ),
            NewTrack::Audio => (
                format!("{count} Audio"),
                TrackKind::Audio {
                    clips: Vec::new().into(),
                },
            ),
            NewTrack::Group => (
                format!("{count} Group"),
                TrackKind::Group {
                    children: Vec::new(),
                    folded: false,
                },
            ),
        };
        let track = Track {
            id,
            name,
            color: Color::PALETTE[(count - 1) % Color::PALETTE.len()],
            mixer: Mixer::default(),
            devices: Vec::new(),
            routing: None,
            modulators: Vec::new(),
            modulations: Vec::new(),
            automation: Vec::new(),
            kind,
        };
        self.insert_tracks(vec![track], position)?;
        Ok(id)
    }

    fn insert_tracks(&mut self, tracks: Vec<Track>, position: TrackPosition) -> EditResult {
        let parent = position.parent;
        let siblings = self.siblings_mut(parent).ok_or_else(|| match parent {
            Some(p) => EditError::WrongTrackKind(p),
            None => unreachable!("the top level always exists"),
        })?;
        let index = match position.before {
            None => siblings.len(),
            Some(before) => siblings
                .iter()
                .position(|t| t.id == before)
                .ok_or(EditError::NoSuchTrack(before))?,
        };
        siblings.splice(index..index, tracks);
        Ok(())
    }

    /// Removes a track (and anything inside it) from wherever it is.
    fn take_track(&mut self, id: TrackId) -> EditResult<Track> {
        let (parent, index) = self.location(id).ok_or(EditError::NoSuchTrack(id))?;
        let siblings = self.siblings_mut(parent).expect("location found it");
        Ok(siblings.remove(index))
    }

    fn check_tracks(&self, ids: &[TrackId]) -> EditResult {
        if ids.is_empty() {
            return Err(EditError::NothingSelected);
        }
        match ids.iter().find(|id| self.track(**id).is_none()) {
            Some(missing) => Err(EditError::NoSuchTrack(*missing)),
            None => Ok(()),
        }
    }

    /// `ids` without duplicates or tracks whose group is also listed, in
    /// display order. Moving or deleting a group already takes its children.
    fn outermost(&self, ids: &[TrackId]) -> Vec<TrackId> {
        let set: HashSet<TrackId> = ids.iter().copied().collect();
        self.track_ids()
            .into_iter()
            .filter(|id| set.contains(id) && !set.iter().any(|other| self.is_inside(*id, *other)))
            .collect()
    }

    pub fn delete_tracks(&mut self, ids: &[TrackId]) -> EditResult {
        self.check_tracks(ids)?;
        for id in self.outermost(ids) {
            self.take_track(id)?;
        }
        self.prune_track_references();
        Ok(())
    }

    /// Moves tracks, keeping their order, to `position`. Used for drag and
    /// drop in the track list, including into and out of groups.
    pub fn move_tracks(&mut self, ids: &[TrackId], position: TrackPosition) -> EditResult {
        self.check_tracks(ids)?;
        let ids = self.outermost(ids);
        if let Some(parent) = position.parent {
            let parent_track = self.track(parent).ok_or(EditError::NoSuchTrack(parent))?;
            if !parent_track.is_group() {
                return Err(EditError::WrongTrackKind(parent));
            }
            if ids
                .iter()
                .any(|id| *id == parent || self.is_inside(parent, *id))
            {
                return Err(EditError::IntoItself);
            }
        }
        if let Some(before) = position.before
            && ids.contains(&before)
        {
            // Dropping a selection just before one of its own tracks: the
            // target is the first unmoved track after it.
            let next = self.next_unmoved_sibling(before, &ids);
            return self.move_tracks(
                &ids,
                TrackPosition {
                    before: next,
                    ..position
                },
            );
        }
        let mut tracks = Vec::with_capacity(ids.len());
        for id in &ids {
            tracks.push(self.take_track(*id)?);
        }
        self.insert_tracks(tracks, position)
    }

    fn next_unmoved_sibling(&self, id: TrackId, moved: &[TrackId]) -> Option<TrackId> {
        let (parent, index) = self.location(id)?;
        let siblings = match parent {
            None => &self.tracks[..],
            Some(p) => self.track(p)?.children(),
        };
        siblings[index..]
            .iter()
            .map(|t| t.id)
            .find(|id| !moved.contains(id))
    }

    /// Wraps tracks from one group (or the top level) in a new group, placed
    /// where the first of them was.
    pub fn group_tracks(&mut self, ids: &[TrackId]) -> EditResult<TrackId> {
        self.check_tracks(ids)?;
        let ids = self.outermost(ids);
        let parent = self.parent(ids[0]);
        if ids.iter().any(|id| self.parent(*id) != parent) {
            return Err(EditError::NotSiblings);
        }
        let group = self.add_track(
            NewTrack::Group,
            TrackPosition {
                parent,
                before: Some(ids[0]),
            },
        )?;
        let position = TrackPosition {
            parent: Some(group),
            before: None,
        };
        self.move_tracks(&ids, position)?;
        Ok(group)
    }

    /// Replaces a group with its children.
    pub fn ungroup(&mut self, id: TrackId) -> EditResult {
        let (parent, index) = self.location(id).ok_or(EditError::NoSuchTrack(id))?;
        let siblings = self.siblings_mut(parent).expect("location found it");
        let TrackKind::Group { children, .. } = &mut siblings[index].kind else {
            return Err(EditError::WrongTrackKind(id));
        };
        let children = std::mem::take(children);
        siblings.splice(index..=index, children);
        // The group is gone: nothing listens to it any more.
        self.prune_track_references();
        Ok(())
    }

    fn track_or_err(&mut self, id: TrackId) -> EditResult<&mut Track> {
        self.track_mut(id).ok_or(EditError::NoSuchTrack(id))
    }

    pub fn rename_track(&mut self, id: TrackId, name: &str) -> EditResult {
        self.track_or_err(id)?.name = name.to_owned();
        Ok(())
    }

    pub fn set_track_color(&mut self, id: TrackId, color: Color) -> EditResult {
        self.track_or_err(id)?.color = color;
        Ok(())
    }

    pub fn set_mixer(&mut self, id: TrackId, mixer: Mixer) -> EditResult {
        self.track_or_err(id)?.mixer = mixer;
        Ok(())
    }

    pub fn set_folded(&mut self, id: TrackId, fold: bool) -> EditResult {
        match &mut self.track_or_err(id)?.kind {
            TrackKind::Group { folded, .. } => {
                *folded = fold;
                Ok(())
            }
            _ => Err(EditError::WrongTrackKind(id)),
        }
    }

    pub fn set_tempo_bpm(&mut self, bpm: f64) {
        self.tempo.tempo = Tempo::from_bpm(bpm.clamp(Tempo::MIN_BPM, Tempo::MAX_BPM));
    }

    /// Swaps a MIDI track's instrument. Parameter values, their automation
    /// and modulation, and modulation by the instrument's own modulators
    /// belonged to the old one, so they go too, as does a plugin's state
    /// (set the new plugin's with [`Project::set_instrument_plugin`]).
    pub fn set_instrument(&mut self, id: TrackId, new: InstrumentRef) -> EditResult {
        let track = self.track_or_err(id)?;
        let TrackKind::Midi {
            instrument,
            params,
            plugin,
            ..
        } = &mut track.kind
        else {
            return Err(EditError::WrongTrackKind(id));
        };
        *instrument = new;
        params.clear();
        *plugin = None;
        let instrument_param =
            |t: &AutomationTarget| matches!(t, AutomationTarget::InstrumentParam(_));
        let from_instrument = |s: &ModSource| matches!(s, ModSource::Instrument(_));
        track
            .automation
            .retain(|lane| !instrument_param(&lane.target));
        track
            .modulations
            .retain(|r| !instrument_param(&r.target) && !from_instrument(&r.source));
        for route in &mut track.modulations {
            if route.scale_by.as_ref().is_some_and(from_instrument) {
                route.scale_by = None;
            }
        }
        Ok(())
    }

    /// Sets (or clears) what the project keeps about a track's plugin
    /// instrument: its name, vendor, where it was found and its state.
    pub fn set_instrument_plugin(
        &mut self,
        id: TrackId,
        data: Option<crate::PluginData>,
    ) -> EditResult {
        let TrackKind::Midi { plugin, .. } = &mut self.track_or_err(id)?.kind else {
            return Err(EditError::WrongTrackKind(id));
        };
        *plugin = data;
        Ok(())
    }

    /// Points a MIDI track's instrument at the file it plays from (a
    /// sampler's .sfz), or at none.
    pub fn set_instrument_file(&mut self, id: TrackId, path: Option<PathBuf>) -> EditResult {
        let TrackKind::Midi { file, .. } = &mut self.track_or_err(id)?.kind else {
            return Err(EditError::WrongTrackKind(id));
        };
        *file = path;
        Ok(())
    }

    /// Points oscillator `osc` (0 = A, 1 = B, 2 = C) of a MIDI track's
    /// Wavetable at a user wavetable, a WAV file, or at none. The file only
    /// plays while the oscillator's table parameter (`osc_a_table`,
    /// `osc_b_table`, `osc_c_table`) is "User", which this leaves alone:
    /// set it too (to `instruments::builtin::wavetable::USER_TABLE`) with
    /// [`Project::set_instrument_param`]. The path is stored as given, like
    /// [`Project::set_instrument_file`]'s. One edit, undone like any other;
    /// fails only for a track that isn't a MIDI track.
    pub fn set_wavetable_file(
        &mut self,
        id: TrackId,
        osc: usize,
        path: Option<PathBuf>,
    ) -> EditResult {
        let TrackKind::Midi { tables, .. } = &mut self.track_or_err(id)?.kind else {
            return Err(EditError::WrongTrackKind(id));
        };
        if tables.len() <= osc {
            if path.is_none() {
                return Ok(());
            }
            tables.resize(osc + 1, None);
        }
        tables[osc] = path;
        // No trailing `None`s, so a track without tables saves as before.
        while tables.last().is_some_and(Option::is_none) {
            tables.pop();
        }
        Ok(())
    }

    /// Sets where the Slicer on a MIDI track cuts its file: slice starts in
    /// the file's frames, in any order. They are sorted, duplicates
    /// dropped, the first moved to 0 and the list cut at
    /// [`crate::slicer::MAX_SLICES`] (see [`crate::slicer::normalize`]).
    /// Fails only for a track that isn't a MIDI track.
    pub fn set_slices(&mut self, id: TrackId, starts: Vec<u64>) -> EditResult {
        let TrackKind::Midi { slices, .. } = &mut self.track_or_err(id)?.kind else {
            return Err(EditError::WrongTrackKind(id));
        };
        *slices = crate::slicer::normalize(starts);
        Ok(())
    }

    /// Replaces all of a MIDI track's instrument settings (a preset): ids
    /// the instrument doesn't have are left out, and values are clamped.
    pub fn set_instrument_params(
        &mut self,
        id: TrackId,
        values: BTreeMap<String, f32>,
    ) -> EditResult {
        let TrackKind::Midi {
            instrument, params, ..
        } = &mut self.track_or_err(id)?.kind
        else {
            return Err(EditError::WrongTrackKind(id));
        };
        let Some(descriptor) = instrument.descriptor() else {
            return Ok(());
        };
        // By id once, rather than a search per value: a drum kit has thousands.
        let known: std::collections::HashMap<&str, _> =
            descriptor.params.iter().map(|p| (p.id, p)).collect();
        *params = values
            .into_iter()
            .filter_map(|(param, value)| {
                let known = known.get(param.as_str())?;
                value.is_finite().then(|| (param, known.clamp(value)))
            })
            .collect();
        Ok(())
    }

    pub fn set_instrument_param(&mut self, id: TrackId, param: &str, value: f32) -> EditResult {
        let TrackKind::Midi {
            instrument, params, ..
        } = &mut self.track_or_err(id)?.kind
        else {
            return Err(EditError::WrongTrackKind(id));
        };
        let value = instrument
            .descriptor()
            .and_then(|d| d.params.iter().find(|p| p.id == param))
            .map_or(value, |p| p.clamp(value));
        params.insert(param.to_owned(), value);
        Ok(())
    }

    /// Replaces the points of one automation lane. No points removes the lane.
    pub fn set_automation(
        &mut self,
        id: TrackId,
        target: AutomationTarget,
        points: impl Into<Shared<Vec<AutomationPoint>>>,
    ) -> EditResult {
        let track = self.track_or_err(id)?;
        // Points already in order and range stay shared with whoever
        // passed them (an undo step, the lane they came from).
        let mut points = points.into();
        if !points.is_sorted_by_key(|p| p.tick) {
            points.sort_by_key(|p| p.tick);
        }
        if points.iter().any(|p| !(0.0..=1.0).contains(&p.value)) {
            for point in &mut points {
                point.value = point.value.clamp(0.0, 1.0);
            }
        }
        let existing = track.automation.iter().position(|l| l.target == target);
        match (existing, points.is_empty()) {
            (Some(index), true) => {
                track.automation.remove(index);
            }
            (Some(index), false) => track.automation[index].points = points,
            (None, true) => {}
            (None, false) => track.automation.push(AutomationLane { target, points }),
        }
        Ok(())
    }

    /// Registers an audio file, or returns the id it already has.
    pub fn add_source(
        &mut self,
        path: PathBuf,
        sample_rate: u32,
        frames: u64,
        channels: u16,
    ) -> SourceId {
        if let Some(existing) = self
            .sources
            .iter()
            .find(|s| s.path == path && s.sample_rate == sample_rate && s.frames == frames)
        {
            return existing.id;
        }
        let id = SourceId(self.alloc_id());
        self.sources.push(AudioSource {
            id,
            path,
            sample_rate,
            frames,
            channels,
        });
        id
    }

    pub fn add_midi_clip(
        &mut self,
        track: TrackId,
        start: Tick,
        length: Tick,
    ) -> EditResult<ClipId> {
        if length <= Tick::ZERO {
            return Err(EditError::EmptyClip);
        }
        let id = self.new_clip_id();
        let name = self
            .track(track)
            .ok_or(EditError::NoSuchTrack(track))?
            .name
            .clone();
        let clip = OwnedClip::Midi(MidiClip {
            id,
            name,
            start,
            length,
            offset: Tick::ZERO,
            notes: Shared::default(),
            muted: false,
        });
        self.place_clips(vec![(track, clip)])?;
        Ok(id)
    }

    /// Adds a clip playing all of `source`.
    pub fn add_audio_clip(
        &mut self,
        track: TrackId,
        start: Tick,
        source: SourceId,
    ) -> EditResult<ClipId> {
        let audio = self.source(source).ok_or(EditError::NoSuchSource(source))?;
        let (name, frames) = (audio.name(), audio.frames);
        if frames == 0 {
            return Err(EditError::EmptyClip);
        }
        let id = self.new_clip_id();
        let clip = OwnedClip::Audio(AudioClip {
            id,
            name,
            start,
            source,
            source_offset: 0,
            source_len: frames,
            gain_db: 0.0,
            muted: false,
            fade_in: 0,
            fade_out: 0,
            fade_in_curve: 0.0,
            fade_out_curve: 0.0,
            crossfade_in: false,
            crossfade_out: false,
            reversed: false,
            looped: false,
            loop_phase: 0,
            play_len: frames,
            stretch: Stretch::default(),
            polarity_inverted: false,
            channels: ChannelMode::Stereo,
        });
        self.place_clips(vec![(track, clip)])?;
        Ok(id)
    }

    /// Moves clips, possibly to other tracks of the same kind. Clips already
    /// where they land get trimmed or removed to make room, as in Ableton.
    pub fn move_clips(&mut self, moves: &[ClipMove]) -> EditResult {
        if moves.is_empty() {
            return Err(EditError::NothingSelected);
        }
        let mut placed = Vec::with_capacity(moves.len());
        for m in moves {
            let (_, mut clip) = self.take_clip(m.clip)?;
            clip.set_start(m.start);
            placed.push((m.track, clip));
        }
        self.place_clips(placed)
    }

    /// Sets where a clip starts and ends on the timeline without moving its
    /// content, i.e. drags its edges. Audio clips stop at the ends of their file.
    pub fn set_clip_bounds(&mut self, id: ClipId, start: Tick, end: Tick) -> EditResult {
        if end <= start {
            return Err(EditError::EmptyClip);
        }
        let (track, mut clip) = self.take_clip(id)?;
        let original = clip.clone();
        let timing = Timing::new(self);
        clip.trim_start(start, &timing);
        clip.trim_end(end, &timing);
        // An audio clip can't reach before its file starts; if the requested
        // range lies entirely out there, nothing is left.
        if clip.end(&timing) <= clip.start() {
            self.put_clip(track, original)?;
            return Err(EditError::EmptyClip);
        }
        self.place_clips(vec![(track, clip)])
    }

    /// Cuts every listed clip that spans `at` in two. Returns the ids of the
    /// new right-hand halves.
    pub fn split_clips(&mut self, ids: &[ClipId], at: Tick) -> EditResult<Vec<ClipId>> {
        let mut new_ids = Vec::new();
        for id in ids {
            let (track, clip) = self.clip(*id).ok_or(EditError::NoSuchClip(*id))?;
            let (clip_start, end) = (clip.start(), self.clip_end(clip));
            if clip_start >= at || end <= at {
                continue;
            }
            let (_, mut left) = self.take_clip(*id)?;
            let mut right = left.clone();
            let timing = Timing::new(self);
            // Cut the fades first, so shrinking the clip doesn't clamp the
            // other fade to make room for them.
            left.cut_end(end - at, &timing);
            right.cut_start(at - clip_start, &timing);
            left.trim_end(at, &timing);
            right.trim_start(at, &timing);
            let right_id = self.new_clip_id();
            right.set_id(right_id);
            if let OwnedClip::Midi(midi) = &mut right {
                for note in &mut midi.notes {
                    note.id = self.new_note_id();
                }
            }
            new_ids.push(right_id);
            // The halves cover just what the clip did, so nothing else moves,
            // not even a clip it crossfades with.
            self.put_clip(track, left)?;
            self.put_clip(track, right)?;
            self.release_crossfades();
        }
        Ok(new_ids)
    }

    pub fn delete_clips(&mut self, ids: &[ClipId]) -> EditResult {
        if ids.is_empty() {
            return Err(EditError::NothingSelected);
        }
        for id in ids {
            self.take_clip(*id)?;
        }
        self.release_crossfades();
        Ok(())
    }

    /// Copies the clips to just after the span they cover together, keeping
    /// their layout. Returns the copies' ids, in the same order.
    pub fn duplicate_clips(&mut self, ids: &[ClipId]) -> EditResult<Vec<ClipId>> {
        if ids.is_empty() {
            return Err(EditError::NothingSelected);
        }
        let mut copies = Vec::with_capacity(ids.len());
        let (mut span_start, mut span_end) = (Tick::new(i64::MAX), Tick::new(i64::MIN));
        for id in ids {
            let (track, clip) = self.clip(*id).ok_or(EditError::NoSuchClip(*id))?;
            span_start = span_start.min(clip.start());
            span_end = span_end.max(self.clip_end(clip));
            copies.push((track, OwnedClip::from(clip)));
        }
        let shift = span_end - span_start;
        let mut new_ids = Vec::with_capacity(copies.len());
        for (_, clip) in &mut copies {
            let id = self.new_clip_id();
            clip.set_id(id);
            clip.set_start(clip.start() + shift);
            if let OwnedClip::Midi(midi) = clip {
                for note in &mut midi.notes {
                    note.id = self.new_note_id();
                }
            }
            new_ids.push(id);
        }
        self.place_clips(copies)?;
        Ok(new_ids)
    }

    /// Copies of clips with the tracks they're on, for the clipboard.
    pub fn copy_clips(&self, ids: &[ClipId]) -> Vec<(TrackId, ClipData)> {
        let mut seen = HashSet::new();
        ids.iter()
            .filter(|id| seen.insert(**id))
            .filter_map(|id| self.clip(*id))
            .map(|(track, clip)| (track, ClipData::from(clip)))
            .collect()
    }

    /// Pastes copied clips so the earliest starts at `at`, keeping their
    /// layout. They go back on the tracks they came from, or all onto `onto`
    /// if they came from one track and `onto` is the same kind. Clips whose
    /// track or audio file is gone are skipped. Returns the new clips' ids.
    pub fn paste_clips(
        &mut self,
        clips: &[(TrackId, ClipData)],
        at: Tick,
        onto: Option<TrackId>,
    ) -> EditResult<Vec<ClipId>> {
        let earliest = clips
            .iter()
            .map(|(_, c)| c.start())
            .min()
            .ok_or(EditError::NothingSelected)?;
        let single_track = clips.windows(2).all(|w| w[0].0 == w[1].0);
        let mut placed = Vec::new();
        let mut ids = Vec::new();
        for (track, clip) in clips {
            let mut target = *track;
            if single_track
                && let Some(onto) = onto
                && self.accepts(onto, clip)
            {
                target = onto;
            }
            if !self.accepts(target, clip) {
                continue;
            }
            if let ClipData::Audio(audio) = clip
                && self.source(audio.source).is_none()
            {
                continue;
            }
            let mut copy = clip.clone();
            let id = self.new_clip_id();
            copy.set_id(id);
            if let ClipData::Midi(midi) = &mut copy {
                for note in &mut midi.notes {
                    note.id = self.new_note_id();
                }
            }
            copy.set_start(at + (clip.start() - earliest));
            placed.push((target, copy));
            ids.push(id);
        }
        if placed.is_empty() {
            return Err(EditError::NothingSelected);
        }
        self.place_clips(placed)?;
        Ok(ids)
    }

    /// True if `clip` can go on `track`.
    fn accepts(&self, track: TrackId, clip: &ClipData) -> bool {
        matches!(
            (self.track(track).map(|t| &t.kind), clip),
            (Some(TrackKind::Midi { .. }), ClipData::Midi(_))
                | (Some(TrackKind::Audio { .. }), ClipData::Audio(_))
        )
    }

    pub fn rename_clip(&mut self, id: ClipId, name: &str) -> EditResult {
        let (track, mut clip) = self.take_clip(id)?;
        match &mut clip {
            OwnedClip::Midi(c) => c.name = name.to_owned(),
            OwnedClip::Audio(c) => c.name = name.to_owned(),
        }
        self.put_clip(track, clip)
    }

    pub fn set_clips_muted(&mut self, ids: &[ClipId], muted: bool) -> EditResult {
        for id in ids {
            let (track, mut clip) = self.take_clip(*id)?;
            match &mut clip {
                ClipData::Midi(c) => c.muted = muted,
                ClipData::Audio(c) => c.muted = muted,
            }
            self.put_clip(track, clip)?;
        }
        Ok(())
    }

    /// Sets an audio clip's fades, in frames at the source's rate, and their
    /// curves. Fades that wouldn't fit in the clip are shortened.
    pub fn set_clip_fades(
        &mut self,
        id: ClipId,
        fade_in: u64,
        fade_out: u64,
        fade_in_curve: f32,
        fade_out_curve: f32,
    ) -> EditResult {
        let (track, mut clip) = self.take_clip(id)?;
        let ClipData::Audio(audio) = &mut clip else {
            self.put_clip(track, clip)?;
            return Err(EditError::WrongTrackKind(track));
        };
        audio.fade_in = fade_in;
        audio.fade_out = fade_out;
        audio.fade_in_curve = fade_in_curve.clamp(-1.0, 1.0);
        audio.fade_out_curve = fade_out_curve.clamp(-1.0, 1.0);
        // Shaped by hand, they stay even if the clips are pulled apart.
        audio.crossfade_in = false;
        audio.crossfade_out = false;
        audio.clamp_fades();
        self.put_clip(track, clip)
    }

    /// Sets the song's key, or clears it.
    pub fn set_scale(&mut self, scale: Option<crate::Scale>) -> EditResult {
        self.scale = scale.map(|s| crate::Scale::new(s.root, s.mode));
        Ok(())
    }

    /// Sets the loop region between two points (either way round, never
    /// before the start), or clears it. An empty stretch clears it too.
    pub fn set_loop(&mut self, region: Option<crate::LoopRegion>) -> EditResult {
        self.loop_region = region.and_then(|r| {
            let (start, end) = (r.start.min(r.end).max(Tick::ZERO), r.start.max(r.end));
            (end > start).then_some(crate::LoopRegion {
                start,
                end,
                on: r.on,
            })
        });
        Ok(())
    }

    /// Keeps how the arrangement is scrolled and zoomed, to save with the
    /// project. A view setting, like a fold: callers keep it out of undo.
    /// A view with numbers no view can have (not finite, a zoom of zero) is
    /// refused, leaving the one there was.
    pub fn set_arrangement_view(&mut self, view: Option<crate::ArrangementView>) -> EditResult {
        if view.is_some_and(|v| !v.is_valid()) {
            return Err(EditError::InvalidView);
        }
        self.arrangement_view = view;
        Ok(())
    }

    /// Changes an audio clip through `change`, then keeps it consistent:
    /// fades inside it, loop state sane, and room made if it grew.
    pub fn edit_audio_clip(
        &mut self,
        id: ClipId,
        change: impl FnOnce(&mut AudioClip),
    ) -> EditResult {
        let (track, mut clip) = self.take_clip(id)?;
        let ClipData::Audio(audio) = &mut clip else {
            self.put_clip(track, clip)?;
            return Err(EditError::WrongTrackKind(track));
        };
        let was_looped = audio.looped;
        let old_span = {
            let timing = Timing::new(self);
            (audio.start, Clip::end(&*audio, &timing))
        };
        change(audio);
        let stretch = &mut audio.stretch;
        stretch.speed = stretch.speed.clamp(Stretch::MIN_SPEED, Stretch::MAX_SPEED);
        stretch.transpose = stretch.transpose.clamp(-48.0, 48.0);
        if stretch
            .original_bpm
            .is_some_and(|bpm| !(bpm > 0.0 && bpm.is_finite()))
        {
            stretch.original_bpm = None;
        }
        match (was_looped, audio.looped) {
            // Starting to loop: one pass, from the top.
            (false, true) => {
                audio.loop_phase = 0;
                audio.play_len = audio.source_len;
            }
            // Stopping: one pass again.
            (true, false) => audio.loop_phase = 0,
            _ => {}
        }
        audio.loop_phase = audio.loop_phase.min(audio.source_len.saturating_sub(1));
        audio.clamp_fades();
        let new_span = (audio.start, Clip::end(&*audio, &Timing::new(self)));
        if new_span == old_span {
            // Same place: leave the neighbours (and any crossfades) alone.
            self.put_clip(track, clip)
        } else {
            self.place_clips(vec![(track, clip)])
        }
    }

    /// Slides an audio clip's audio by `frames` of material (positive moves
    /// it later) without moving the clip. Unlooped clips stop at the ends of
    /// their file.
    pub fn slip_clip(&mut self, id: ClipId, frames: i64) -> EditResult {
        let file = match self.clip(id) {
            Some((_, crate::ClipRef::Audio(clip))) => {
                self.source(clip.source).map_or(0, |s| s.frames)
            }
            Some(_) => return Err(EditError::NothingSelected),
            None => return Err(EditError::NoSuchClip(id)),
        };
        self.edit_audio_clip(id, |clip| {
            if clip.looped {
                let len = clip.source_len.max(1) as i64;
                clip.loop_phase = (clip.loop_phase as i64 - frames).rem_euclid(len) as u64;
            } else {
                // Content moving later means reading from earlier in the
                // file, or later in it when the clip plays backwards.
                let shift = if clip.reversed { frames } else { -frames };
                let max = file.saturating_sub(clip.source_len) as i64;
                clip.source_offset = (clip.source_offset as i64 + shift).clamp(0, max) as u64;
            }
        })
    }

    /// Changes an audio clip's speed so the audio it plays spans
    /// `start..end`, as when dragging an edge with Shift. Speeds past the
    /// limits stop short of the requested length.
    pub fn stretch_clip(&mut self, id: ClipId, start: Tick, end: Tick) -> EditResult {
        let Some((_, crate::ClipRef::Audio(clip))) = self.clip(id) else {
            return Err(EditError::NothingSelected);
        };
        let old = self.clip_end(crate::ClipRef::Audio(clip)) - clip.start;
        let new = end - start;
        if new <= Tick::ZERO || old <= Tick::ZERO {
            return Err(EditError::EmptyClip);
        }
        let speed = clip.stretch.speed * old.get() as f64 / new.get() as f64;
        self.edit_audio_clip(id, |clip| {
            clip.start = start;
            clip.stretch.speed = speed;
        })
    }

    /// Like `set_clip_bounds`, but an audio clip dragged past the end of its
    /// file starts looping (its region running to the end of the file) and
    /// fills the rest with repeats.
    pub fn resize_clip_looping(&mut self, id: ClipId, start: Tick, end: Tick) -> EditResult {
        let limit = match self.clip(id) {
            Some((_, crate::ClipRef::Audio(clip))) if !clip.looped => {
                self.audio_clip_limits(clip).1
            }
            _ => return self.set_clip_bounds(id, start, end),
        };
        if end <= limit {
            return self.set_clip_bounds(id, start, end);
        }
        self.set_clip_bounds(id, start, limit)?;
        self.edit_audio_clip(id, |clip| clip.looped = true)?;
        self.set_clip_bounds(id, start, end)
    }

    /// Splits a clip at every position in `at` that falls inside it, in one
    /// go (chopping). Returns the ids of all the pieces, in order.
    pub fn chop_clip(&mut self, id: ClipId, at: &[Tick]) -> EditResult<Vec<ClipId>> {
        let mut at = at.to_vec();
        at.sort();
        at.dedup();
        let mut pieces = vec![id];
        for tick in at {
            let last = *pieces.last().expect("never empty");
            pieces.extend(self.split_clips(&[last], tick)?);
        }
        Ok(pieces)
    }

    pub fn set_clip_gain(&mut self, id: ClipId, gain_db: f32) -> EditResult {
        let (track, mut clip) = self.take_clip(id)?;
        let OwnedClip::Audio(audio) = &mut clip else {
            self.put_clip(track, clip)?;
            return Err(EditError::WrongTrackKind(track));
        };
        audio.gain_db = gain_db;
        self.put_clip(track, clip)
    }

    /// Replaces a MIDI clip's notes. The piano roll edits a copy of the notes
    /// and commits the result here, one undo step per gesture.
    pub fn set_notes(&mut self, id: ClipId, notes: impl Into<Shared<Vec<Note>>>) -> EditResult {
        let mut notes = notes.into();
        let (track, mut clip) = self.take_clip(id)?;
        let OwnedClip::Midi(midi) = &mut clip else {
            self.put_clip(track, clip)?;
            return Err(EditError::WrongTrackKind(track));
        };
        if !notes.is_sorted_by_key(|n| (n.start, n.pitch)) {
            notes.sort_by_key(|n| (n.start, n.pitch));
        }
        midi.notes = notes;
        self.put_clip(track, clip)
    }

    /// Sets note `note`'s pitch curve in clip `clip` (empty for none): the
    /// points sorted by time and held within [`BendPoint::MAX_SEMITONES`].
    pub fn set_note_bend(
        &mut self,
        clip: ClipId,
        note: NoteId,
        mut points: Vec<BendPoint>,
    ) -> EditResult {
        let limit = BendPoint::MAX_SEMITONES;
        for point in &mut points {
            point.semitones = point.semitones.clamp(-limit, limit);
            point.curve = point.curve.clamp(-1.0, 1.0);
            point.at = point.at.max(Tick::ZERO);
        }
        points.sort_by_key(|p| p.at);
        let Some((_, ClipRef::Midi(midi))) = self.clip(clip) else {
            return Err(EditError::NoSuchClip(clip));
        };
        let Some(index) = midi.notes.iter().position(|n| n.id == note) else {
            return Err(EditError::NoSuchNote(note));
        };
        let mut notes = midi.notes.clone();
        notes[index].bend = points;
        self.set_notes(clip, notes)
    }

    /// Removes a clip from its track, wherever it is.
    fn take_clip(&mut self, id: ClipId) -> EditResult<(TrackId, OwnedClip)> {
        let (track, _) = self.clip(id).ok_or(EditError::NoSuchClip(id))?;
        let owned = match &mut self.track_mut(track).expect("clip() found it").kind {
            TrackKind::Midi { clips, .. } => {
                let index = clips.iter().position(|c| c.id == id).expect("found above");
                OwnedClip::Midi(clips.remove(index))
            }
            TrackKind::Audio { clips } => {
                let index = clips.iter().position(|c| c.id == id).expect("found above");
                OwnedClip::Audio(clips.remove(index))
            }
            TrackKind::Group { .. } => unreachable!("groups hold no clips"),
        };
        Ok((track, owned))
    }

    /// Adds a clip to a track as-is, without making room.
    fn put_clip(&mut self, track: TrackId, clip: OwnedClip) -> EditResult {
        let target = self.track_or_err(track)?;
        match (&mut target.kind, clip) {
            (TrackKind::Midi { clips, .. }, OwnedClip::Midi(clip)) => {
                clips.push(clip);
                clips.sort_by_key(|c| c.start);
            }
            (TrackKind::Audio { clips }, OwnedClip::Audio(clip)) => {
                clips.push(clip);
                clips.sort_by_key(|c| c.start);
            }
            _ => return Err(EditError::WrongTrackKind(track)),
        }
        Ok(())
    }

    /// Adds clips to tracks, then trims whatever else they now overlap. The
    /// placed clips themselves are left alone, even if they overlap each other.
    fn place_clips(&mut self, placed: Vec<(TrackId, OwnedClip)>) -> EditResult {
        let placed_ids: HashSet<ClipId> = placed.iter().map(|(_, c)| c.id()).collect();
        let mut ranges = Vec::with_capacity(placed.len());
        {
            let timing = Timing::new(self);
            for (track, clip) in &placed {
                ranges.push((*track, clip.id(), clip.start(), clip.end(&timing)));
            }
        }
        for (track, clip) in placed {
            self.put_clip(track, clip)?;
        }
        // Decide every crossfade before trimming anything, so one placed
        // clip's trims can't hide a neighbour another one fades with.
        let keeps: Vec<HashSet<ClipId>> = ranges
            .iter()
            .map(|&(track, id, start, end)| {
                let mut keep = placed_ids.clone();
                self.crossfade(track, id, start, end, &mut keep);
                keep
            })
            .collect();
        for ((track, _, start, end), keep) in ranges.into_iter().zip(keeps) {
            self.make_room(track, start, end, &keep);
        }
        self.release_crossfades();
        Ok(())
    }

    /// Where the audio clip `placed` (spanning `start..end` on `track`)
    /// overlaps just the edge of another, both stay and fade over the
    /// overlap instead of one being trimmed. Those neighbours join `keep`.
    fn crossfade(
        &mut self,
        track: TrackId,
        placed: ClipId,
        start: Tick,
        end: Tick,
        keep: &mut HashSet<ClipId>,
    ) {
        let Project {
            tracks,
            tempo,
            sources,
            ..
        } = self;
        let timing = Timing { tempo, sources };
        let Some(TrackKind::Audio { clips }) =
            crate::track::find_mut(tracks, track).map(|t| &mut t.kind)
        else {
            return;
        };
        let Some(index) = clips.iter().position(|c| c.id == placed) else {
            return;
        };
        let spans: Vec<(ClipId, Tick, Tick)> = clips
            .iter()
            .map(|c| (c.id, c.start, Clip::end(c, &timing)))
            .collect();
        // Only a two-clip overlap fades; anything layered deeper is trimmed.
        let clear = |other: ClipId, from: Tick, to: Tick| {
            !spans
                .iter()
                .any(|&(id, a, b)| id != placed && id != other && a < to && b > from)
        };
        let mut placed_clip = clips[index].clone();
        for other in clips.iter_mut() {
            if keep.contains(&other.id) {
                continue;
            }
            let (a, b) = (other.start, Clip::end(other, &timing));
            if a < start && b > start && b < end && clear(other.id, start, b) {
                // Its tail under our head.
                let overlap = b - start;
                other.fade_out = timing.material(other, overlap).round() as u64;
                other.fade_out_curve = CROSSFADE_CURVE;
                other.crossfade_out = true;
                placed_clip.fade_in = timing.material(&placed_clip, overlap).round() as u64;
                placed_clip.fade_in_curve = CROSSFADE_CURVE;
                placed_clip.crossfade_in = true;
            } else if a > start && a < end && b > end && clear(other.id, a, end) {
                // Its head under our tail.
                let overlap = end - a;
                other.fade_in = timing.material(other, overlap).round() as u64;
                other.fade_in_curve = CROSSFADE_CURVE;
                other.crossfade_in = true;
                placed_clip.fade_out = timing.material(&placed_clip, overlap).round() as u64;
                placed_clip.fade_out_curve = CROSSFADE_CURVE;
                placed_clip.crossfade_out = true;
            } else {
                continue;
            }
            other.clamp_fades();
            keep.insert(other.id);
        }
        placed_clip.clamp_fades();
        if let Some(clip) = clips.iter_mut().find(|c| c.id == placed) {
            *clip = placed_clip;
        }
    }

    /// Drops crossfades whose other clip has gone: moved away, deleted, or
    /// trimmed so they no longer overlap.
    fn release_crossfades(&mut self) {
        fn walk(tracks: &mut [Track], timing: &Timing) {
            for track in tracks {
                match &mut track.kind {
                    TrackKind::Audio { clips } => {
                        let spans: Vec<(ClipId, Tick, Tick)> = clips
                            .iter()
                            .map(|c| (c.id, c.start, Clip::end(c, timing)))
                            .collect();
                        // Something still overlapping the fade keeps it.
                        let covered = |id: ClipId, from: Tick, to: Tick| {
                            spans
                                .iter()
                                .any(|&(other, a, b)| other != id && a < to && b > from)
                        };
                        for (clip, &(id, start, end)) in clips.iter_mut().zip(&spans) {
                            let fade_in = start + timing.duration(clip, clip.fade_in);
                            if clip.crossfade_in && !covered(id, start, fade_in) {
                                clip.crossfade_in = false;
                                clip.fade_in = 0;
                            }
                            let fade_out = end - timing.duration(clip, clip.fade_out);
                            if clip.crossfade_out && !covered(id, fade_out, end) {
                                clip.crossfade_out = false;
                                clip.fade_out = 0;
                            }
                        }
                    }
                    TrackKind::Group { children, .. } => walk(children, timing),
                    TrackKind::Midi { .. } => {}
                }
            }
        }
        let Project {
            tracks,
            tempo,
            sources,
            ..
        } = self;
        walk(tracks, &Timing { tempo, sources });
    }

    /// Trims, splits or removes the clips on `track` that overlap
    /// `start..end`, except those in `keep`.
    fn make_room(&mut self, track: TrackId, start: Tick, end: Tick, keep: &HashSet<ClipId>) {
        let Project {
            tracks,
            tempo,
            sources,
            next_id,
            ..
        } = self;
        let timing = Timing { tempo, sources };
        let mut new_id = || {
            let id = *next_id;
            *next_id += 1;
            id
        };
        let Some(track) = crate::track::find_mut(tracks, track) else {
            return;
        };
        match &mut track.kind {
            TrackKind::Midi { clips, .. } => {
                trim_overlaps(clips, start, end, keep, &timing, &mut new_id)
            }
            TrackKind::Audio { clips } => {
                trim_overlaps(clips, start, end, keep, &timing, &mut new_id)
            }
            TrackKind::Group { .. } => {}
        }
    }
}

/// The fade shape for crossfades: with both sides bent this much, each is at
/// 0.707 halfway, so the combined level holds steady (equal power).
const CROSSFADE_CURVE: f32 = -0.22;

/// What it takes to turn audio frames into ticks.
struct Timing<'a> {
    tempo: &'a crate::TempoMap,
    sources: &'a [AudioSource],
}

impl<'a> Timing<'a> {
    fn new(project: &'a Project) -> Timing<'a> {
        Timing {
            tempo: &project.tempo,
            sources: &project.sources,
        }
    }

    fn source(&self, id: SourceId) -> (u32, u64) {
        self.sources
            .iter()
            .find(|s| s.id == id)
            .map_or((48_000, u64::MAX), |s| (s.sample_rate, s.frames))
    }

    /// Frames of the clip's material that `ticks` of timeline cover: more
    /// than the plain conversion when the clip plays faster.
    fn material(&self, clip: &AudioClip, ticks: Tick) -> f64 {
        let rate = self.source(clip.source).0;
        self.tempo.ticks_to_frames(ticks.get() as f64, rate)
            * clip.stretch.effective_speed(self.tempo)
    }

    /// How long `frames` of the clip's material last on the timeline.
    /// Rounded up, so any audio at all lasts at least a tick.
    fn duration(&self, clip: &AudioClip, frames: u64) -> Tick {
        let rate = self.source(clip.source).0;
        let speed = clip.stretch.effective_speed(self.tempo);
        crate::tempo::ticks_up(self.tempo.frames_to_ticks(frames as f64 / speed, rate))
    }
}

/// A clip taken out of the project: while an edit moves it around, or on
/// the clipboard.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipData {
    Midi(MidiClip),
    Audio(AudioClip),
}

type OwnedClip = ClipData;

impl From<crate::ClipRef<'_>> for ClipData {
    fn from(clip: crate::ClipRef<'_>) -> Self {
        match clip {
            crate::ClipRef::Midi(c) => ClipData::Midi(c.clone()),
            crate::ClipRef::Audio(c) => ClipData::Audio(c.clone()),
        }
    }
}

macro_rules! dispatch {
    ($self:ident, $clip:ident => $body:expr) => {
        match $self {
            ClipData::Midi($clip) => $body,
            ClipData::Audio($clip) => $body,
        }
    };
}

impl ClipData {
    fn id(&self) -> ClipId {
        dispatch!(self, c => c.id())
    }
    fn set_id(&mut self, id: ClipId) {
        dispatch!(self, c => c.set_id(id))
    }
    fn start(&self) -> Tick {
        dispatch!(self, c => Clip::start(c))
    }
    fn set_start(&mut self, start: Tick) {
        dispatch!(self, c => c.set_start(start))
    }
    fn end(&self, timing: &Timing) -> Tick {
        dispatch!(self, c => Clip::end(c, timing))
    }
    fn trim_start(&mut self, to: Tick, timing: &Timing) {
        dispatch!(self, c => c.trim_start(to, timing))
    }
    fn trim_end(&mut self, to: Tick, timing: &Timing) {
        dispatch!(self, c => c.trim_end(to, timing))
    }
    fn cut_start(&mut self, removed: Tick, timing: &Timing) {
        dispatch!(self, c => c.cut_start(removed, timing))
    }
    fn cut_end(&mut self, removed: Tick, timing: &Timing) {
        dispatch!(self, c => c.cut_end(removed, timing))
    }
}

/// What the overlap rules need from either kind of clip.
trait Clip: Clone {
    fn id(&self) -> ClipId;
    fn set_id(&mut self, id: ClipId);
    fn start(&self) -> Tick;
    /// Moves the whole clip, content and all.
    fn set_start(&mut self, start: Tick);
    fn end(&self, timing: &Timing) -> Tick;
    /// Moves the left edge to `to`, leaving the content where it is.
    fn trim_start(&mut self, to: Tick, timing: &Timing);
    /// Moves the right edge to `to`.
    fn trim_end(&mut self, to: Tick, timing: &Timing);
    /// Gives anything inside the clip that has an id (notes) a fresh one,
    /// for a copy that must not share them.
    fn renumber_contents(&mut self, _new_id: &mut impl FnMut() -> u32) {}
    /// The first `removed` ticks are about to be cut away (by a split, or
    /// under another clip); a fade there loses that part, keeping its end
    /// where it was, so a crossfade still covers what's left of an overlap.
    fn cut_start(&mut self, _removed: Tick, _timing: &Timing) {}
    /// The last `removed` ticks are about to be cut away.
    fn cut_end(&mut self, _removed: Tick, _timing: &Timing) {}
}

impl Clip for MidiClip {
    fn id(&self) -> ClipId {
        self.id
    }
    fn set_id(&mut self, id: ClipId) {
        self.id = id;
    }
    fn start(&self) -> Tick {
        self.start
    }
    fn set_start(&mut self, start: Tick) {
        self.start = start;
    }
    fn end(&self, _: &Timing) -> Tick {
        MidiClip::end(self)
    }
    fn trim_start(&mut self, to: Tick, _: &Timing) {
        let delta = to - self.start;
        self.start = to;
        self.offset += delta;
        self.length = (self.length - delta).max(Tick::new(1));
    }
    fn trim_end(&mut self, to: Tick, _: &Timing) {
        self.length = (to - self.start).max(Tick::new(1));
    }
    fn renumber_contents(&mut self, new_id: &mut impl FnMut() -> u32) {
        for note in &mut self.notes {
            note.id = crate::NoteId(new_id());
        }
    }
}

impl Clip for AudioClip {
    fn id(&self) -> ClipId {
        self.id
    }
    fn set_id(&mut self, id: ClipId) {
        self.id = id;
    }
    fn start(&self) -> Tick {
        self.start
    }
    fn set_start(&mut self, start: Tick) {
        self.start = start;
    }
    fn end(&self, timing: &Timing) -> Tick {
        self.start + timing.duration(self, self.played())
    }
    fn trim_start(&mut self, to: Tick, timing: &Timing) {
        let old_end = Clip::end(self, timing);
        let delta = timing.material(self, to - self.start);
        if self.looped {
            // The loop region stays; where in it playback starts moves.
            let len = self.source_len.max(1) as i64;
            let delta = delta.round() as i64;
            self.loop_phase = (self.loop_phase as i64 + delta).rem_euclid(len) as u64;
            self.play_len = (self.play_len as i64 - delta).max(1) as u64;
            self.start = to;
        } else if !self.reversed {
            let end_frame = self.source_offset + self.source_len;
            let wanted = self.source_offset as f64 + delta;
            if wanted < 0.0 {
                // Can't reveal audio from before the file begins.
                self.start -= timing.duration(self, self.source_offset);
                self.source_offset = 0;
            } else {
                self.source_offset = (wanted.round() as u64).min(end_frame.saturating_sub(1));
                self.start = to;
            }
            self.source_len = end_frame - self.source_offset;
        } else {
            // Backwards, the clip's start is the end of its region in the file.
            let file = timing.source(self.source).1;
            let end_frame = (self.source_offset + self.source_len) as f64 - delta;
            let end_frame = if end_frame > file as f64 {
                let revealed = file - (self.source_offset + self.source_len);
                self.start -= timing.duration(self, revealed);
                file
            } else {
                self.start = to;
                (end_frame.round() as u64).max(self.source_offset + 1)
            };
            self.source_len = end_frame - self.source_offset;
        }
        // The end in ticks is rounded from the length in frames, so a new
        // start can nudge it a tick later, into whatever follows. Don't let it.
        let room = timing.material(self, old_end - self.start).floor().max(1.0) as u64;
        self.shorten_to(room);
        self.clamp_fades();
    }
    fn trim_end(&mut self, to: Tick, timing: &Timing) {
        // Round down so the clip never ends past `to`.
        let frames = timing.material(self, to - self.start).floor().max(1.0) as u64;
        if self.looped {
            self.play_len = frames;
        } else if !self.reversed {
            let available = timing
                .source(self.source)
                .1
                .saturating_sub(self.source_offset);
            self.source_len = frames.min(available).max(1);
        } else {
            // Backwards, playing longer reveals what comes before the region.
            let end_frame = self.source_offset + self.source_len;
            let len = frames.min(end_frame).max(1);
            self.source_offset = end_frame - len;
            self.source_len = len;
        }
        self.clamp_fades();
    }
    fn cut_start(&mut self, removed: Tick, timing: &Timing) {
        let frames = timing.material(self, removed).round() as u64;
        self.fade_in = self.fade_in.saturating_sub(frames);
    }
    fn cut_end(&mut self, removed: Tick, timing: &Timing) {
        let frames = timing.material(self, removed).round() as u64;
        self.fade_out = self.fade_out.saturating_sub(frames);
    }
}

impl AudioClip {
    /// Plays at most `frames` of material, cutting from the end.
    fn shorten_to(&mut self, frames: u64) {
        if self.looped {
            self.play_len = self.play_len.min(frames);
        } else if self.source_len > frames {
            if self.reversed {
                // The end of play is the start of the region.
                self.source_offset += self.source_len - frames;
            }
            self.source_len = frames;
        }
    }
}

fn trim_overlaps<C: Clip>(
    clips: &mut Vec<C>,
    start: Tick,
    end: Tick,
    keep: &HashSet<ClipId>,
    timing: &Timing,
    new_id: &mut impl FnMut() -> u32,
) {
    let mut split_off = Vec::new();
    clips.retain_mut(|clip| {
        if keep.contains(&clip.id()) {
            return true;
        }
        let (clip_start, clip_end) = (clip.start(), clip.end(timing));
        if clip_end <= start || clip_start >= end {
            return true;
        }
        let covers_start = clip_start < start;
        let covers_end = clip_end > end;
        match (covers_start, covers_end) {
            (false, false) => return false,
            (true, true) => {
                let mut right = clip.clone();
                right.set_id(ClipId(new_id()));
                right.renumber_contents(new_id);
                right.cut_start(end - clip_start, timing);
                right.trim_start(end, timing);
                split_off.push(right);
                clip.cut_end(clip_end - start, timing);
                clip.trim_end(start, timing);
            }
            (true, false) => {
                clip.cut_end(clip_end - start, timing);
                clip.trim_end(start, timing);
            }
            (false, true) => {
                clip.cut_start(end - clip_start, timing);
                clip.trim_start(end, timing);
            }
        }
        true
    });
    clips.extend(split_off);
    clips.sort_by_key(|c| c.start());
}

#[cfg(test)]
mod tests {
    use midi::{Pitch, Velocity};

    use super::*;
    use crate::{ClipRef, Row};

    const BAR: Tick = Tick::new(3840);

    fn ticks(t: i64) -> Tick {
        Tick::new(t)
    }

    fn names(project: &Project) -> Vec<String> {
        let mut names = Vec::new();
        project.visit(|t, depth| names.push(format!("{}{}", "-".repeat(depth), t.name)));
        names
    }

    /// (start, end) of each clip on a track, in order.
    fn spans(project: &Project, track: TrackId) -> Vec<(i64, i64)> {
        project
            .track(track)
            .unwrap()
            .clips()
            .into_iter()
            .map(|c| (c.start().get(), project.clip_end(c).get()))
            .collect()
    }

    fn midi_project() -> (Project, TrackId) {
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        (project, track)
    }

    fn note(project: &mut Project, start: i64, pitch: u8) -> Note {
        Note {
            id: project.new_note_id(),
            start: ticks(start),
            len: ticks(240),
            pitch: Pitch::new(pitch).unwrap(),
            velocity: Velocity::MAX,
            release_velocity: Velocity::CENTER,
            muted: false,
            bend: Vec::new(),
        }
    }

    #[test]
    fn set_note_bend_sorts_and_clamps() {
        let (mut project, track) = midi_project();
        let clip = project.add_midi_clip(track, Tick::ZERO, BAR).unwrap();
        let held = note(&mut project, 0, 60);
        let id = held.id;
        project.set_notes(clip, vec![held]).unwrap();
        let point = |at: i64, semitones: f32| crate::BendPoint {
            at: ticks(at),
            semitones,
            curve: 0.0,
        };
        project
            .set_note_bend(clip, id, vec![point(120, 99.0), point(0, -1.0)])
            .unwrap();
        let Some((_, ClipRef::Midi(midi))) = project.clip(clip) else {
            panic!("the clip");
        };
        let notes = &midi.notes;
        assert_eq!(
            notes[0].bend,
            [point(0, -1.0), point(120, crate::BendPoint::MAX_SEMITONES)]
        );
        assert!(
            project
                .set_note_bend(clip, crate::NoteId(9999), Vec::new())
                .is_err()
        );
    }

    #[test]
    fn set_slices_sorts_dedups_and_caps_at_64() {
        let (mut project, track) = midi_project();
        project.set_slices(track, vec![900, 300, 300, 600]).unwrap();
        assert_eq!(project.track(track).unwrap().slices(), [0, 300, 600, 900]);
        // More transients than slices: the first 64 are kept, so no key
        // past the root's 64th is ever played.
        let many: Vec<u64> = (0..500).map(|k| k * 100).collect();
        project.set_slices(track, many).unwrap();
        let slices = project.track(track).unwrap().slices();
        assert_eq!(slices.len(), crate::slicer::MAX_SLICES);
        assert_eq!(slices.last(), Some(&6300));
        let audio = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        assert!(project.set_slices(audio, vec![0]).is_err());
    }

    #[test]
    fn slices_stay_when_the_instrument_changes() {
        let (mut project, track) = midi_project();
        project
            .set_instrument_file(track, Some(PathBuf::from("/loops/amen.wav")))
            .unwrap();
        project.set_slices(track, vec![0, 1000, 2000]).unwrap();
        project
            .set_instrument(track, InstrumentRef::builtin("canopy.wavetable"))
            .unwrap();
        let t = project.track(track).unwrap();
        assert_eq!(t.slices(), [0, 1000, 2000]);
        assert_eq!(t.without_clips().slices(), [0, 1000, 2000]);
    }

    #[test]
    fn wavetable_files_are_set_per_oscillator_and_undone() {
        let (mut project, track) = midi_project();
        let mut history = crate::History::default();
        let file = |osc: usize| PathBuf::from(format!("/tables/osc {osc}.wav"));
        history
            .edit(&mut project, "Load Wavetable", |p| {
                p.set_wavetable_file(track, 2, Some(file(2)))
            })
            .unwrap();
        let t = project.track(track).unwrap();
        assert_eq!(t.wavetable_file(2), Some(file(2).as_path()));
        assert_eq!(t.wavetable_file(0), None);
        assert_eq!(t.wavetable_file(7), None);
        assert_eq!(t.wavetable_files(), &[None, None, Some(file(2))]);

        history
            .edit(&mut project, "Load Wavetable", |p| {
                p.set_wavetable_file(track, 0, Some(file(0)))
            })
            .unwrap();
        // Removing the last one leaves no trailing gaps.
        history
            .edit(&mut project, "Remove Wavetable", |p| {
                p.set_wavetable_file(track, 2, None)
            })
            .unwrap();
        assert_eq!(
            project.track(track).unwrap().wavetable_files(),
            &[Some(file(0))]
        );
        // Removing one that isn't there changes nothing: no undo step.
        let steps = history.generation();
        history
            .edit(&mut project, "Remove Wavetable", |p| {
                p.set_wavetable_file(track, 5, None)
            })
            .unwrap();
        assert_eq!(history.generation(), steps);

        assert_eq!(
            history.undo(&mut project).as_deref(),
            Some("Remove Wavetable")
        );
        assert_eq!(
            project.track(track).unwrap().wavetable_files(),
            &[Some(file(0)), None, Some(file(2))]
        );
        history.undo(&mut project);
        history.undo(&mut project);
        assert!(project.track(track).unwrap().wavetable_files().is_empty());
        history.redo(&mut project);
        assert_eq!(
            project.track(track).unwrap().wavetable_file(2),
            Some(file(2).as_path())
        );

        // Only MIDI tracks have them; swapping the instrument keeps them.
        let audio = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        assert_eq!(
            project.set_wavetable_file(audio, 0, Some(file(0))),
            Err(EditError::WrongTrackKind(audio))
        );
        assert!(project.track(audio).unwrap().wavetable_file(0).is_none());
        project
            .set_instrument(track, InstrumentRef::builtin("canopy.sampler"))
            .unwrap();
        assert_eq!(
            project.track(track).unwrap().wavetable_file(2),
            Some(file(2).as_path())
        );
    }

    #[test]
    fn new_tracks_are_numbered_and_colored() {
        let project = Project::with_default_tracks();
        assert_eq!(names(&project), ["1 MIDI", "2 MIDI", "3 Audio", "4 Audio"]);
        assert_ne!(project.tracks[0].color, project.tracks[1].color);
    }

    #[test]
    fn grouping_keeps_order_and_position() {
        let mut project = Project::with_default_tracks();
        let ids = project.track_ids();
        let group = project.group_tracks(&[ids[2], ids[1]]).unwrap();
        assert_eq!(
            names(&project),
            ["1 MIDI", "5 Group", "-2 MIDI", "-3 Audio", "4 Audio"]
        );

        let nested = project.group_tracks(&[ids[2]]).unwrap();
        assert_eq!(project.parent(nested), Some(group));
        assert!(project.is_inside(ids[2], group));

        assert_eq!(
            project.group_tracks(&[ids[0], ids[2]]),
            Err(EditError::NotSiblings)
        );

        project.set_folded(group, true).unwrap();
        let rows: Vec<Row> = project.rows();
        assert_eq!(rows.len(), 3, "a folded group hides its children");
        assert_eq!(
            rows[1],
            Row {
                id: group,
                depth: 0
            }
        );

        project.ungroup(group).unwrap();
        assert_eq!(
            names(&project),
            ["1 MIDI", "2 MIDI", "6 Group", "-3 Audio", "4 Audio"]
        );
    }

    #[test]
    fn moving_tracks_in_and_out_of_groups() {
        let mut project = Project::with_default_tracks();
        let ids = project.track_ids();
        let group = project.group_tracks(&[ids[0]]).unwrap();

        let into = TrackPosition {
            parent: Some(group),
            before: None,
        };
        project.move_tracks(&[ids[3], ids[2]], into).unwrap();
        assert_eq!(
            names(&project),
            ["5 Group", "-1 MIDI", "-3 Audio", "-4 Audio", "2 MIDI"]
        );

        assert_eq!(
            project.move_tracks(&[group], into),
            Err(EditError::IntoItself)
        );

        project
            .move_tracks(&[ids[0]], TrackPosition::after(&project, group))
            .unwrap();
        assert_eq!(
            names(&project),
            ["5 Group", "-3 Audio", "-4 Audio", "1 MIDI", "2 MIDI"]
        );

        // Dropping a selection just before one of its own members is a no-op
        // that still resolves to a valid spot.
        let before_self = TrackPosition {
            parent: None,
            before: Some(ids[0]),
        };
        project.move_tracks(&[ids[0], ids[1]], before_self).unwrap();
        assert_eq!(
            names(&project),
            ["5 Group", "-3 Audio", "-4 Audio", "1 MIDI", "2 MIDI"]
        );

        project.delete_tracks(&[group, ids[2]]).unwrap();
        assert_eq!(names(&project), ["1 MIDI", "2 MIDI"]);
    }

    #[test]
    fn placing_a_clip_trims_what_it_overlaps() {
        let (mut project, track) = midi_project();
        let long = project.add_midi_clip(track, ticks(0), BAR * 4).unwrap();
        // Covering the middle splits the long clip in two.
        project.add_midi_clip(track, BAR, BAR).unwrap();
        assert_eq!(
            spans(&project, track),
            [(0, 3840), (3840, 7680), (7680, 15360)]
        );

        let right_half = project.track(track).unwrap().clips()[2];
        let ClipRef::Midi(right_half) = right_half else {
            unreachable!()
        };
        assert_ne!(right_half.id, long);
        assert_eq!(right_half.offset, BAR * 2, "content stays put");

        // Covering an end trims; covering everything removes.
        project
            .add_midi_clip(track, ticks(1920), ticks(3840))
            .unwrap();
        assert_eq!(
            spans(&project, track),
            [(0, 1920), (1920, 5760), (5760, 7680), (7680, 15360)]
        );
        project.add_midi_clip(track, ticks(0), BAR * 2).unwrap();
        assert_eq!(spans(&project, track), [(0, 7680), (7680, 15360)]);
    }

    #[test]
    fn moving_clips_between_tracks() {
        let mut project = Project::with_default_tracks();
        let ids = project.track_ids();
        let a = project.add_midi_clip(ids[0], ticks(0), BAR).unwrap();
        let b = project.add_midi_clip(ids[1], ticks(0), BAR * 2).unwrap();

        project
            .move_clips(&[ClipMove {
                clip: a,
                track: ids[1],
                start: ticks(1920),
            }])
            .unwrap();
        assert!(spans(&project, ids[0]).is_empty());
        assert_eq!(
            spans(&project, ids[1]),
            [(0, 1920), (1920, 5760), (5760, 7680)]
        );
        assert_eq!(project.clip(b).unwrap().0, ids[1]);

        let wrong_kind = project.move_clips(&[ClipMove {
            clip: a,
            track: ids[2],
            start: ticks(0),
        }]);
        assert_eq!(wrong_kind, Err(EditError::WrongTrackKind(ids[2])));
    }

    #[test]
    fn trimming_the_left_edge_reveals_or_hides_notes() {
        let (mut project, track) = midi_project();
        let clip = project.add_midi_clip(track, BAR, BAR).unwrap();
        let notes = vec![note(&mut project, 0, 60), note(&mut project, 1920, 62)];
        project.set_notes(clip, notes).unwrap();

        project
            .set_clip_bounds(clip, BAR + ticks(960), BAR * 2)
            .unwrap();
        let (_, ClipRef::Midi(midi)) = project.clip(clip).unwrap() else {
            unreachable!()
        };
        assert_eq!(midi.offset, ticks(960));
        let audible: Vec<u8> = midi.audible_notes().map(|n| n.pitch.get()).collect();
        assert_eq!(audible, [62]);

        project.set_clip_bounds(clip, BAR, BAR * 2).unwrap();
        let (_, ClipRef::Midi(midi)) = project.clip(clip).unwrap() else {
            unreachable!()
        };
        assert_eq!(midi.audible_notes().count(), 2, "dragging back restores it");
        assert_eq!(
            project.set_clip_bounds(clip, BAR, BAR),
            Err(EditError::EmptyClip)
        );
    }

    #[test]
    fn audio_clips_trim_in_frames_and_stop_at_the_file_ends() {
        let mut project = Project::new(); // 120 BPM: a quarter is 0.5 s
        let track = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let source = project.add_source("/tmp/loop.wav".into(), 48_000, 96_000, 2);
        let clip = project.add_audio_clip(track, BAR, source).unwrap();
        assert_eq!(spans(&project, track), [(3840, 7680)], "2 s is one bar");

        project
            .set_clip_bounds(clip, BAR + Tick::QUARTER, BAR * 2)
            .unwrap();
        let (_, ClipRef::Audio(audio)) = project.clip(clip).unwrap() else {
            unreachable!()
        };
        assert_eq!((audio.source_offset, audio.source_len), (24_000, 72_000));

        // Asking for more than the file has stops at its edges.
        project.set_clip_bounds(clip, ticks(0), BAR * 4).unwrap();
        let (_, ClipRef::Audio(audio)) = project.clip(clip).unwrap() else {
            unreachable!()
        };
        assert_eq!((audio.source_offset, audio.source_len), (0, 96_000));
        assert_eq!(audio.start, BAR);

        // Twice the tempo: the same audio covers twice the beats.
        project.set_tempo_bpm(240.0);
        assert_eq!(spans(&project, track), [(3840, 11520)]);
    }

    #[test]
    fn split_and_duplicate() {
        let mut project = Project::new();
        let audio = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let midi = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let source = project.add_source("/tmp/a.wav".into(), 48_000, 96_000, 1);
        let a = project.add_audio_clip(audio, ticks(0), source).unwrap();
        let m = project.add_midi_clip(midi, ticks(0), BAR).unwrap();

        let halves = project.split_clips(&[a, m], ticks(1920)).unwrap();
        assert_eq!(halves.len(), 2);
        assert_eq!(spans(&project, audio), [(0, 1920), (1920, 3840)]);
        assert_eq!(spans(&project, midi), [(0, 1920), (1920, 3840)]);
        let (_, ClipRef::Audio(right)) = project.clip(halves[0]).unwrap() else {
            unreachable!()
        };
        assert_eq!((right.source_offset, right.source_len), (48_000, 48_000));

        assert!(project.split_clips(&[a], ticks(5000)).unwrap().is_empty());

        let copies = project.duplicate_clips(&[a, halves[0]]).unwrap();
        assert_eq!(copies.len(), 2);
        assert_eq!(
            spans(&project, audio),
            [(0, 1920), (1920, 3840), (3840, 5760), (5760, 7680)]
        );
    }

    #[test]
    fn copy_and_paste() {
        let mut project = Project::with_default_tracks();
        let ids = project.track_ids();
        let a = project.add_midi_clip(ids[0], ticks(0), BAR).unwrap();
        let b = project.add_midi_clip(ids[1], BAR, BAR).unwrap();
        let notes = vec![note(&mut project, 0, 60)];
        project.set_notes(a, notes).unwrap();
        let copied = project.copy_clips(&[a, b]);

        // Two tracks: back onto their own tracks, layout kept.
        let pasted = project.paste_clips(&copied, BAR * 4, Some(ids[1])).unwrap();
        assert_eq!(spans(&project, ids[0]), [(0, 3840), (15360, 19200)]);
        assert_eq!(spans(&project, ids[1]), [(3840, 7680), (19200, 23040)]);
        let (_, ClipRef::Midi(copy)) = project.clip(pasted[0]).unwrap() else {
            unreachable!()
        };
        let (_, ClipRef::Midi(original)) = project.clip(a).unwrap() else {
            unreachable!()
        };
        assert_ne!(copy.notes[0].id, original.notes[0].id, "fresh note ids");

        // One track: onto the selected track, if it's the same kind.
        let one = project.copy_clips(&[a]);
        let pasted = project.paste_clips(&one, ticks(0), Some(ids[1])).unwrap();
        assert_eq!(project.clip(pasted[0]).unwrap().0, ids[1]);
        assert_eq!(
            project
                .paste_clips(&one, ticks(0), Some(ids[2]))
                .map(|v| v.len()),
            Ok(1),
            "audio track: back to its own"
        );
        assert_eq!(
            project.paste_clips(&[], ticks(0), None),
            Err(EditError::NothingSelected)
        );
    }

    #[test]
    fn instrument_changes_drop_its_params_and_automation() {
        let (mut project, track) = midi_project();
        project
            .set_instrument_param(track, "cutoff", 500.0)
            .unwrap();
        let param = AutomationTarget::InstrumentParam("cutoff".into());
        let point = AutomationPoint {
            tick: ticks(0),
            value: 2.0,
            curve: 0.0,
        };
        project
            .set_automation(track, param.clone(), vec![point])
            .unwrap();
        project
            .set_automation(track, AutomationTarget::Volume, vec![point])
            .unwrap();
        let lane = project
            .track(track)
            .unwrap()
            .automation_lane(&param)
            .unwrap();
        assert_eq!(lane.points[0].value, 1.0, "clamped to the 0..1 scale");

        project
            .set_instrument(track, InstrumentRef::builtin("canopy.simple_osc"))
            .unwrap();
        let track = project.track(track).unwrap();
        assert!(track.automation_lane(&param).is_none());
        assert!(track.automation_lane(&AutomationTarget::Volume).is_some());
        let TrackKind::Midi { params, .. } = &track.kind else {
            unreachable!()
        };
        assert!(params.is_empty());
    }
}
