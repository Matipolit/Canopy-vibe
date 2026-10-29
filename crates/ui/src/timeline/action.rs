//! Everything the timeline can ask for, applied after the frame is drawn.

use std::path::PathBuf;

use midi::Tick;
use project::{
    AudioClip, AutomationPoint, AutomationTarget, ChannelMode, ClipId, ClipMove, ClipRef, Color,
    Mixer, NewTrack, TrackId, TrackPosition,
};

use super::TimelineView;
use crate::session::Session;

/// How a click changes the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMode {
    Replace,
    /// Ctrl-click.
    Toggle,
    /// Shift-click.
    Extend,
}

impl SelectMode {
    pub fn from_modifiers(modifiers: eframe::egui::Modifiers) -> SelectMode {
        if modifiers.command {
            SelectMode::Toggle
        } else if modifiers.shift {
            SelectMode::Extend
        } else {
            SelectMode::Replace
        }
    }
}

/// A change made by dragging one of an audio clip's handles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipChange {
    /// Fade lengths in source frames, and curves.
    Fades {
        fade_in: u64,
        fade_out: u64,
        fade_in_curve: f32,
        fade_out_curve: f32,
    },
    Gain(f32),
}

/// A switch flipped on audio clips from the context menu or the clip panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioEdit {
    Reverse(bool),
    Loop(bool),
    InvertPolarity(bool),
    Channels(ChannelMode),
}

impl AudioEdit {
    pub fn label(self) -> &'static str {
        match self {
            AudioEdit::Reverse(_) => "Reverse",
            AudioEdit::Loop(_) => "Loop",
            AudioEdit::InvertPolarity(_) => "Invert Polarity",
            AudioEdit::Channels(_) => "Channels",
        }
    }

    pub fn apply(self, clip: &mut AudioClip) {
        match self {
            AudioEdit::Reverse(on) => clip.reversed = on,
            AudioEdit::Loop(on) => clip.looped = on,
            AudioEdit::InvertPolarity(on) => clip.polarity_inverted = on,
            AudioEdit::Channels(mode) => clip.channels = mode,
        }
    }
}

/// Where to chop an audio clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Chop {
    /// At every transient; the sensitivity is 0..=1.
    Transients(f32),
    /// At every grid line of this size.
    Grid(Tick),
}

#[derive(Debug, Clone)]
pub enum Action {
    SelectClip {
        clip: ClipId,
        track: TrackId,
        mode: SelectMode,
    },
    SelectTrack {
        track: TrackId,
        mode: SelectMode,
    },
    ClearSelection,
    SetMarker(Tick),
    Seek(Tick),
    /// Sets or clears the loop region (not an undo step).
    SetLoop(Option<project::LoopRegion>),
    /// Loops this stretch and starts the playhead inside it.
    LoopSpan(Tick, Tick),
    /// Show a clip in the piano roll; `focus` also brings the piano roll up.
    OpenClip {
        clip: ClipId,
        focus: bool,
    },
    AddTrack(NewTrack, TrackPosition),
    AddMidiClip {
        track: TrackId,
        start: Tick,
        length: Tick,
    },
    MoveClips(Vec<ClipMove>),
    SetClipBounds {
        clip: ClipId,
        start: Tick,
        end: Tick,
    },
    /// Shift-dragging an edge: the audio speeds up or slows down to fill
    /// `start..end`.
    StretchClip {
        clip: ClipId,
        start: Tick,
        end: Tick,
    },
    DeleteClips(Vec<ClipId>),
    MuteClips(Vec<ClipId>, bool),
    FreezeClip(ClipId),
    EditAudioClips(Vec<ClipId>, AudioEdit),
    NormalizeClips(Vec<ClipId>),
    ChopClip(ClipId, Chop),
    ConsolidateClips(Vec<ClipId>),
    /// An Alt-drag in progress: the clip's audio slid `frames` (of material)
    /// later than it was when the drag began.
    LiveSlip {
        original: Box<AudioClip>,
        frames: i64,
    },
    /// A clip handle drag in progress: applied live, one undo step at the end.
    LiveClip(ClipId, ClipChange),
    CancelLive,
    DuplicateClips(Vec<ClipId>),
    SplitClips(Vec<ClipId>, Tick),
    SetFolded(TrackId, bool),
    SetMixer {
        track: TrackId,
        mixer: Mixer,
        label: &'static str,
    },
    /// A fader drag in progress; `None` is the master.
    LiveMixer {
        track: Option<TrackId>,
        mixer: Mixer,
    },
    EndLive(&'static str),
    RenameTrack(TrackId, String),
    SetTrackColor(TrackId, Color),
    DeleteTracks(Vec<TrackId>),
    GroupTracks(Vec<TrackId>),
    Ungroup(TrackId),
    MoveTracks(Vec<TrackId>, TrackPosition),
    /// Shows a lane for the target (if it isn't shown already).
    ShowAutomation(TrackId, AutomationTarget),
    HideAutomation(TrackId, AutomationTarget),
    /// The A button: hide all lanes, or show the ones with points (volume if none).
    ToggleAutomation(TrackId),
    ReplaceAutomationLane {
        track: TrackId,
        from: AutomationTarget,
        to: AutomationTarget,
    },
    SetAutomation {
        track: TrackId,
        target: AutomationTarget,
        points: Vec<AutomationPoint>,
    },
    Import(Vec<PathBuf>, Option<TrackId>, Tick, TrackPosition),
    /// .sfz instruments: onto that track if it's a MIDI one, else onto new
    /// tracks at that position.
    LoadSfz(Vec<PathBuf>, Option<TrackId>, TrackPosition),
    /// Audio files to slice, each on a Slicer on a new track at the
    /// position.
    LoadSlicer(Vec<PathBuf>, TrackPosition),
    /// Link the next MIDI controller moved to this control (`None`: the
    /// master).
    LearnController(Option<TrackId>, AutomationTarget),
    SelectPoints(super::automation::PointSelection),
    /// The Generate window's preview: applied live, one undo step on Apply.
    LiveAutomation {
        track: TrackId,
        target: AutomationTarget,
        points: Vec<AutomationPoint>,
    },
    UnlinkController(Option<TrackId>, AutomationTarget),
    /// Effects dragged from the device view onto a track's header (or the
    /// master): moved to the end of its chain, or copied there.
    DropDevices {
        ids: Vec<project::DeviceId>,
        to: Option<TrackId>,
        copy: bool,
    },
}

impl TimelineView {
    pub fn apply(&mut self, session: &mut Session, action: Action) {
        match action {
            Action::SelectClip { clip, track, mode } => {
                self.automation_selection = None;
                self.selection.master = false;
                let selection = &mut self.selection;
                match mode {
                    SelectMode::Replace => selection.clips = vec![clip],
                    SelectMode::Toggle => {
                        if let Some(i) = selection.clips.iter().position(|c| *c == clip) {
                            selection.clips.remove(i);
                        } else {
                            selection.clips.push(clip);
                        }
                    }
                    SelectMode::Extend => {
                        if !selection.has_clip(clip) {
                            selection.clips.push(clip);
                        }
                    }
                }
                selection.tracks = vec![track];
                if session.project.clip(clip).is_some() {
                    selection.open_clip = Some(clip);
                }
            }
            Action::SelectTrack { track, mode } => {
                self.automation_selection = None;
                self.selection.master = false;
                self.select_track(session, track, mode);
            }
            Action::ClearSelection => {
                self.selection.clips.clear();
                self.automation_selection = None;
            }
            Action::SetMarker(tick) => self.marker = Some(tick.max(Tick::ZERO)),
            Action::Seek(tick) => session.seek(tick),
            Action::SetLoop(region) => session.set_loop(region),
            Action::LoopSpan(start, end) => session.loop_span(start, end),
            Action::OpenClip { clip, focus } => {
                self.selection.open_clip = Some(clip);
                self.open_requested |= focus;
            }
            Action::AddTrack(kind, position) => {
                if let Some(id) = session.edit("Add Track", |p| p.add_track(kind, position)) {
                    self.selection.tracks = vec![id];
                    self.selection.clips.clear();
                }
            }
            Action::AddMidiClip {
                track,
                start,
                length,
            } => {
                let added =
                    session.edit("Add MIDI Clip", |p| p.add_midi_clip(track, start, length));
                if let Some(clip) = added {
                    self.selection.clips = vec![clip];
                    self.selection.open_clip = Some(clip);
                    self.open_requested = true;
                }
            }
            Action::MoveClips(moves) => {
                session.edit("Move Clips", |p| p.move_clips(&moves));
            }
            Action::SetClipBounds { clip, start, end } => {
                session.edit("Resize Clip", |p| p.resize_clip_looping(clip, start, end));
            }
            Action::StretchClip { clip, start, end } => {
                session.edit("Stretch Clip", |p| p.stretch_clip(clip, start, end));
            }
            Action::EditAudioClips(clips, edit) => {
                session.edit(edit.label(), |p| {
                    for id in &clips {
                        if let Some((_, ClipRef::Audio(_))) = p.clip(*id) {
                            p.edit_audio_clip(*id, |c| edit.apply(c))?;
                        }
                    }
                    Ok(())
                });
            }
            Action::NormalizeClips(clips) => session.normalize_clips(&clips),
            Action::ChopClip(clip, chop) => {
                let pieces = match chop {
                    Chop::Transients(sensitivity) => session.chop_at_transients(clip, sensitivity),
                    Chop::Grid(step) => {
                        let project = &session.project;
                        let span = project
                            .clip(clip)
                            .map(|(_, c)| (c.start(), project.clip_end(c)));
                        match span {
                            Some((start, end)) => {
                                let step = step.get().max(1);
                                let first = start.get().div_euclid(step) * step + step;
                                let at: Vec<Tick> = (first..end.get())
                                    .step_by(step as usize)
                                    .map(Tick::new)
                                    .collect();
                                session.edit("Chop", |p| p.chop_clip(clip, &at))
                            }
                            None => None,
                        }
                    }
                };
                if let Some(pieces) = pieces {
                    self.selection.clips = pieces;
                }
            }
            Action::ConsolidateClips(clips) => {
                if let Some(clip) = session.consolidate_clips(&clips) {
                    self.selection.clips = vec![clip];
                    self.selection.open_clip = Some(clip);
                }
            }
            Action::LiveSlip { original, frames } => session.live_edit(|p| {
                p.edit_audio_clip(original.id, |c| {
                    c.source_offset = original.source_offset;
                    c.loop_phase = original.loop_phase;
                })?;
                p.slip_clip(original.id, frames)
            }),
            Action::DeleteClips(clips) => {
                session.edit("Delete Clips", |p| p.delete_clips(&clips));
            }
            Action::MuteClips(clips, muted) => {
                let label = if muted { "Mute Clips" } else { "Unmute Clips" };
                session.edit(label, |p| p.set_clips_muted(&clips, muted));
            }
            Action::FreezeClip(clip) => {
                if let Some(audio) = session.freeze_clip(clip) {
                    self.selection.clips = vec![audio];
                }
            }
            Action::LiveClip(clip, change) => session.live_edit(|p| match change {
                ClipChange::Fades {
                    fade_in,
                    fade_out,
                    fade_in_curve,
                    fade_out_curve,
                } => p.set_clip_fades(clip, fade_in, fade_out, fade_in_curve, fade_out_curve),
                ClipChange::Gain(db) => p.set_clip_gain(clip, db),
            }),
            Action::CancelLive => session.cancel_live(),
            Action::DuplicateClips(clips) => {
                if let Some(copies) = session.edit("Duplicate", |p| p.duplicate_clips(&clips)) {
                    self.selection.clips = copies;
                }
            }
            Action::SplitClips(clips, at) => {
                if let Some(right) = session.edit("Split", |p| p.split_clips(&clips, at)) {
                    if right.is_empty() {
                        session.info("Nothing to split at the marker");
                    }
                    self.selection.clips.extend(right);
                }
            }
            Action::SetFolded(track, folded) => {
                if folded {
                    self.fold_shut(&session.project, track);
                }
                session.view_edit(|p| p.set_folded(track, folded));
                if !folded {
                    // What unfolding shows grows open: the rows under the
                    // group, down to the next one at its depth.
                    let rows = session.project.rows();
                    if let Some(at) = rows.iter().position(|r| r.id == track) {
                        let depth = rows[at].depth;
                        for row in rows[at + 1..].iter().take_while(|r| r.depth > depth) {
                            self.grow_open(super::RowKey::Track(row.id));
                            for target in self.automation.get(&row.id).cloned().unwrap_or_default()
                            {
                                self.grow_open(super::RowKey::Lane(row.id, target));
                            }
                        }
                    }
                }
            }
            Action::SetMixer {
                track,
                mixer,
                label,
            } => {
                session.edit(label, |p| p.set_mixer(track, mixer));
            }
            Action::LiveMixer { track, mixer } => session.live_mixer(track, mixer),
            Action::EndLive(label) => session.end_live(label),
            Action::RenameTrack(track, name) => {
                session.edit("Rename Track", |p| p.rename_track(track, &name));
            }
            Action::SetTrackColor(track, color) => {
                session.edit("Track Color", |p| p.set_track_color(track, color));
            }
            Action::DeleteTracks(tracks) => {
                session.edit("Delete Tracks", |p| p.delete_tracks(&tracks));
            }
            Action::GroupTracks(tracks) => {
                if let Some(group) = session.edit("Group Tracks", |p| p.group_tracks(&tracks)) {
                    self.selection.tracks = vec![group];
                }
            }
            Action::Ungroup(track) => {
                session.edit("Ungroup", |p| p.ungroup(track));
            }
            Action::MoveTracks(tracks, position) => {
                session.edit("Move Tracks", |p| p.move_tracks(&tracks, position));
            }
            Action::ShowAutomation(track, target) => {
                let lanes = self.automation.entry(track).or_default();
                if !lanes.contains(&target) {
                    lanes.push(target.clone());
                    self.grow_open(super::RowKey::Lane(track, target));
                }
            }
            Action::HideAutomation(track, target) => {
                self.lane_shut(&session.project, track, Some(&target));
                if let Some(lanes) = self.automation.get_mut(&track) {
                    lanes.retain(|t| *t != target);
                }
            }
            Action::ToggleAutomation(track) => {
                let shown = self.automation.get(&track).is_some_and(|l| !l.is_empty());
                if shown {
                    self.lane_shut(&session.project, track, None);
                    self.automation.remove(&track);
                } else if let Some(t) = session.project.track(track) {
                    let mut lanes: Vec<_> = t.automation.iter().map(|l| l.target.clone()).collect();
                    if lanes.is_empty() {
                        lanes.push(AutomationTarget::Volume);
                    }
                    for target in &lanes {
                        self.grow_open(super::RowKey::Lane(track, target.clone()));
                    }
                    self.automation.insert(track, lanes);
                }
            }
            Action::ReplaceAutomationLane { track, from, to } => {
                if let Some(lanes) = self.automation.get_mut(&track)
                    && let Some(slot) = lanes.iter_mut().find(|t| **t == from)
                {
                    *slot = to;
                }
            }
            Action::SetAutomation {
                track,
                target,
                points,
            } => {
                // Moving or bending keeps the selection; anything that adds
                // or removes points drops it.
                if let Some(selection) = &mut self.automation_selection
                    && selection.is_lane(track, &target)
                {
                    if selection.points.len() == points.len() {
                        selection.points = points.clone();
                    } else {
                        self.automation_selection = None;
                    }
                }
                session.edit("Edit Automation", |p| {
                    p.set_automation(track, target, points)
                });
            }
            Action::SelectPoints(selection) => self.automation_selection = Some(selection),
            Action::LiveAutomation {
                track,
                target,
                points,
            } => session.live_edit(|p| p.set_automation(track, target, points)),
            Action::Import(paths, track, start, position) => {
                session.import_files(paths, track, start, position);
            }
            Action::LoadSfz(paths, track, position) => {
                for path in paths {
                    if let Some(loaded) = session.load_sfz(path, track, position) {
                        self.selection.tracks = vec![loaded];
                    }
                }
            }
            Action::LoadSlicer(paths, position) => {
                for path in paths {
                    session.load_slicer(path, None, position);
                }
            }
            Action::LearnController(track, target) => session.learn_controller(track, target),
            Action::UnlinkController(track, target) => {
                session.edit("Unlink Controller", |p| p.unlink_controller(track, &target));
            }
            Action::DropDevices { ids, to, copy } => {
                crate::effect_rack::drop_devices(session, self, &ids, to, None, copy);
            }
        }
        self.prune(&session.project);
    }

    fn select_track(&mut self, session: &Session, track: TrackId, mode: SelectMode) {
        let selection = &mut self.selection;
        match mode {
            SelectMode::Replace => {
                selection.tracks = vec![track];
                selection.clips.clear();
            }
            SelectMode::Toggle => {
                if let Some(i) = selection.tracks.iter().position(|t| *t == track) {
                    selection.tracks.remove(i);
                } else {
                    selection.tracks.push(track);
                }
            }
            SelectMode::Extend => {
                // Everything on screen between the last selected track and this one.
                let rows: Vec<TrackId> = session.project.rows().iter().map(|r| r.id).collect();
                let anchor = selection
                    .primary_track()
                    .and_then(|a| rows.iter().position(|id| *id == a));
                let Some(end) = rows.iter().position(|id| *id == track) else {
                    return;
                };
                let start = anchor.unwrap_or(end);
                let (lo, hi) = (start.min(end), start.max(end));
                for id in &rows[lo..=hi] {
                    if !selection.tracks.contains(id) {
                        selection.tracks.push(*id);
                    }
                }
                // Keep the clicked track last, so it's the primary one.
                selection.tracks.retain(|t| *t != track);
                selection.tracks.push(track);
            }
        }
    }
}
