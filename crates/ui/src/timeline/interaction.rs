//! Clicking, moving and resizing clips. A drag shows a ghost of the result
//! and commits one edit when released.

use eframe::egui::{self, CursorIcon, Painter, Rect, Sense, pos2, vec2};
use midi::Tick;
use project::{
    AudioClip, ChannelMode, ClipId, ClipMove, ClipRef, Project, Stretch, Track, TrackId,
};

use super::clip::{self, ClipLook};
use super::{
    Action, AudioEdit, Chop, ClipChange, Gesture, RowKind, SelectMode, TimelineView, color32, id,
    row_kind,
};
use crate::session::Session;
use crate::theme;
use crate::widgets::chrome;

/// Width of the grab zone at each end of a clip.
const EDGE: f32 = 6.0;

#[derive(Debug, Clone)]
pub(super) struct MoveClips {
    grabbed_start: Tick,
    anchor_tick: f64,
    anchor_row: usize,
    pub clips: Vec<Moving>,
    pub delta: Tick,
    pub rows: isize,
}

impl MoveClips {
    pub fn has_moved(&self) -> bool {
        self.delta != Tick::ZERO || self.rows != 0
    }
}

#[derive(Debug, Clone)]
pub(super) struct Moving {
    pub id: ClipId,
    /// Index among the visible track rows.
    row: usize,
    kind: RowKind,
    start: Tick,
    end: Tick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Edge {
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub(super) struct Resize {
    pub clip: ClipId,
    track: TrackId,
    edge: Edge,
    pub start: Tick,
    pub end: Tick,
    /// How far each edge can go: audio clips stop at the ends of their file
    /// (unless the right edge is going to loop), or at the speed limits when
    /// stretching.
    earliest_start: Tick,
    latest_end: Tick,
    min_len: Tick,
    /// Shift was held when grabbed: the audio changes speed to fit instead
    /// of being trimmed.
    pub stretch: bool,
    /// An unlooped audio clip's right edge: past the end of the file, the
    /// clip starts looping.
    loops: bool,
    /// Pointer distance from the edge when grabbed, so the edge doesn't jump.
    grab_offset: f64,
    original: (Tick, Tick),
}

pub(super) fn clip(
    view: &mut TimelineView,
    ui: &egui::Ui,
    session: &Session,
    track: &Track,
    clip: ClipRef<'_>,
    rect: Rect,
    step: Tick,
) {
    let id_number = clip.id().0;
    let body = ui.interact(rect, id("clip", id_number), Sense::click_and_drag());
    let edge = EDGE.min(rect.width() / 4.0);
    let left_rect = Rect::from_min_max(rect.min, pos2(rect.left() + edge, rect.bottom()));
    let right_rect = Rect::from_min_max(pos2(rect.right() - edge, rect.top()), rect.max);
    let left = ui.interact(left_rect, id("clip_left", id_number), Sense::drag());
    let right = ui.interact(right_rect, id("clip_right", id_number), Sense::drag());
    if left.hovered() || right.hovered() || left.dragged() || right.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }

    let modifiers = ui.input(|i| i.modifiers);
    if body.clicked()
        && let Some(pos) = body.interact_pointer_pos()
    {
        view.push(Action::SelectClip {
            clip: clip.id(),
            track: track.id,
            mode: SelectMode::from_modifiers(modifiers),
        });
        let tick = view
            .grid
            .snap(view.viewport.tick_at(pos.x), step, modifiers.alt);
        view.push(Action::SetMarker(tick));
        if !session.is_playing() {
            view.push(Action::Seek(tick));
        }
    }
    if body.double_clicked() {
        view.push(Action::OpenClip {
            clip: clip.id(),
            focus: true,
        });
    }
    body.context_menu(|ui| context_menu(view, ui, session, clip.id(), step));

    // Widgets only start drags. `TimelineView::drive_gesture` follows the
    // pointer from then on and commits on release, even if this clip has
    // scrolled out of view by then.
    if body.drag_started() {
        match clip {
            ClipRef::Audio(audio) if modifiers.alt => {
                let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or_default();
                view.gesture = Some(Gesture::Slip(SlipDrag {
                    original: audio.clone(),
                    anchor_x: origin.x,
                    frames: 0,
                }));
            }
            _ => start_move(view, ui, session, track, clip),
        }
    }
    for (response, edge) in [(&left, Edge::Left), (&right, Edge::Right)] {
        if response.drag_started()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            view.gesture = Some(Gesture::Resize(start_resize(
                view,
                session,
                track.id,
                clip,
                edge,
                origin.x,
                modifiers.shift,
            )));
        }
    }
    if let ClipRef::Audio(audio) = clip {
        audio_handles(view, ui, session, audio, rect);
    }
}

/// What an audio clip's handles drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClipHandle {
    FadeIn,
    FadeOut,
    FadeInCurve,
    FadeOutCurve,
    Volume,
}

impl ClipHandle {
    fn label(self) -> &'static str {
        match self {
            ClipHandle::Volume => "Clip Volume",
            _ => "Clip Fade",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct HandleDrag {
    pub clip: ClipId,
    handle: ClipHandle,
    /// The clip as it was when the drag began.
    original: project::AudioClip,
    anchor: egui::Pos2,
    /// The last change sent, so standing still doesn't rebuild every frame.
    last: Option<ClipChange>,
}

impl HandleDrag {
    pub fn label(&self) -> &'static str {
        self.handle.label()
    }

    /// The change the pointer at `pos` asks for, if it differs from the last.
    pub fn update(
        &mut self,
        viewport: &super::Viewport,
        project: &Project,
        pos: egui::Pos2,
    ) -> Option<ClipChange> {
        let clip = &self.original;
        let len = clip.played();
        let frames_at = |tick: f64| project.audio_material(clip, tick).max(0.0).round() as u64;
        let start = clip.start.get() as f64;
        let end = start + project.audio_ticks(clip, len as f64);
        let bend = |curve: f32| (curve + (pos.y - self.anchor.y) / 60.0).clamp(-1.0, 1.0);
        // The fade being dragged gives way to the other one, not vice versa.
        let fades = |fade_in: u64, fade_out: u64, fade_in_curve: f32, fade_out_curve: f32| {
            let fade_in = fade_in.min(len);
            ClipChange::Fades {
                fade_in,
                fade_out: fade_out.min(len - fade_in),
                fade_in_curve,
                fade_out_curve,
            }
        };
        let tick = viewport.tick_at(pos.x);
        let change = match self.handle {
            ClipHandle::FadeIn => fades(
                frames_at(tick - start).min(len - clip.fade_out),
                clip.fade_out,
                clip.fade_in_curve,
                clip.fade_out_curve,
            ),
            ClipHandle::FadeOut => fades(
                clip.fade_in,
                frames_at(end - tick).min(len - clip.fade_in),
                clip.fade_in_curve,
                clip.fade_out_curve,
            ),
            ClipHandle::FadeInCurve => fades(
                clip.fade_in,
                clip.fade_out,
                bend(clip.fade_in_curve),
                clip.fade_out_curve,
            ),
            ClipHandle::FadeOutCurve => fades(
                clip.fade_in,
                clip.fade_out,
                clip.fade_in_curve,
                bend(clip.fade_out_curve),
            ),
            ClipHandle::Volume => {
                let db = clip.gain_db - (pos.y - self.anchor.y) / VOLUME_DRAG_PX_PER_DB;
                ClipChange::Gain(db.clamp(project::SILENCE_DB, MAX_CLIP_GAIN_DB))
            }
        };
        (self.last != Some(change)).then(|| {
            self.last = Some(change);
            change
        })
    }
}

const VOLUME_DRAG_PX_PER_DB: f32 = 3.0;
const MAX_CLIP_GAIN_DB: f32 = 12.0;

/// The fade and volume handles on an audio clip, FL Studio style.
fn audio_handles(
    view: &mut TimelineView,
    ui: &egui::Ui,
    session: &Session,
    audio: &project::AudioClip,
    rect: Rect,
) {
    let Some(handles) = clip::AudioHandles::new(rect, audio, &session.project, &view.viewport)
    else {
        return;
    };
    let grab = |center: egui::Pos2| Rect::from_center_size(center, vec2(12.0, 12.0));
    let mut spots = vec![
        (ClipHandle::FadeIn, grab(handles.fade_in_handle())),
        (ClipHandle::FadeOut, grab(handles.fade_out_handle())),
        (ClipHandle::Volume, grab(handles.volume)),
    ];
    if let Some(dot) = handles.fade_in_curve {
        spots.push((ClipHandle::FadeInCurve, grab(dot)));
    }
    if let Some(dot) = handles.fade_out_curve {
        spots.push((ClipHandle::FadeOutCurve, grab(dot)));
    }
    for (handle, spot) in spots {
        let handle_id = id("clip_handle", audio.id.0).with(handle as u8);
        let response = ui.interact(spot, handle_id, Sense::click_and_drag());
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(match handle {
                ClipHandle::FadeIn | ClipHandle::FadeOut => CursorIcon::ResizeHorizontal,
                _ => CursorIcon::ResizeVertical,
            });
        }
        if handle == ClipHandle::Volume {
            response
                .clone()
                .on_hover_text(format!("{:.1} dB", audio.gain_db));
        }
        if response.drag_started()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            view.gesture = Some(Gesture::ClipHandle(HandleDrag {
                clip: audio.id,
                handle,
                original: audio.clone(),
                anchor: origin,
                last: None,
            }));
        }
        if response.double_clicked() {
            let reset = |h: ClipHandle| handle == h;
            let change = match handle {
                ClipHandle::Volume => ClipChange::Gain(0.0),
                _ => ClipChange::Fades {
                    fade_in: if reset(ClipHandle::FadeIn) {
                        0
                    } else {
                        audio.fade_in
                    },
                    fade_out: if reset(ClipHandle::FadeOut) {
                        0
                    } else {
                        audio.fade_out
                    },
                    fade_in_curve: if reset(ClipHandle::FadeInCurve) {
                        0.0
                    } else {
                        audio.fade_in_curve
                    },
                    fade_out_curve: if reset(ClipHandle::FadeOutCurve) {
                        0.0
                    } else {
                        audio.fade_out_curve
                    },
                },
            };
            view.push(Action::LiveClip(audio.id, change));
            view.push(Action::EndLive(handle.label()));
        }
    }
}

impl MoveClips {
    /// The moves to commit, or `None` if nothing moved or the target rows
    /// are gone (e.g. an undo during the drag).
    pub fn moves(&self, rows: &[TrackId]) -> Option<Vec<ClipMove>> {
        if !self.has_moved() {
            return None;
        }
        self.clips
            .iter()
            .map(|c| {
                let row = usize::try_from(c.row as isize + self.rows).ok()?;
                Some(ClipMove {
                    clip: c.id,
                    track: *rows.get(row)?,
                    start: c.start + self.delta,
                })
            })
            .collect()
    }
}

impl Resize {
    pub fn update(
        &mut self,
        viewport: &super::Viewport,
        grid: &super::Grid,
        x: f32,
        step: Tick,
        bypass_snap: bool,
    ) {
        let tick = viewport.tick_at(x) - self.grab_offset;
        let snapped = grid.snap(tick, step, bypass_snap);
        let min_len = if grid.snap && !bypass_snap {
            step.min(Tick::new(60))
        } else {
            Tick::new(1)
        }
        .max(self.min_len);
        let latest_end = if self.loops {
            Tick::new(i64::MAX / 4)
        } else {
            self.latest_end
        };
        match self.edge {
            Edge::Left => self.start = snapped.max(self.earliest_start).min(self.end - min_len),
            Edge::Right => self.end = snapped.min(latest_end).max(self.start + min_len),
        }
    }

    pub fn changed(&self) -> bool {
        (self.start, self.end) != self.original
    }

    /// The action that commits the resize.
    pub fn action(&self) -> Action {
        let (clip, start, end) = (self.clip, self.start, self.end);
        if self.stretch {
            Action::StretchClip { clip, start, end }
        } else {
            Action::SetClipBounds { clip, start, end }
        }
    }

    /// How an audio clip will look once resized, where that differs from
    /// trimming it: stretched, or looping past the end of its file.
    fn preview(&self, project: &Project, clip: &AudioClip) -> Option<AudioClip> {
        let mut preview = clip.clone();
        if self.stretch {
            let (old, new) = (self.original.1 - self.original.0, self.end - self.start);
            preview.start = self.start;
            preview.stretch.speed *= old.get() as f64 / new.get().max(1) as f64;
            return Some(preview);
        }
        if !self.loops || self.end <= self.latest_end {
            return None;
        }
        // As `Project::resize_clip_looping` will: the region runs to the end
        // of the file, and repeats.
        let file = project.source(clip.source)?.frames;
        if clip.reversed {
            preview.source_len += preview.source_offset;
            preview.source_offset = 0;
        } else {
            preview.source_len = file - preview.source_offset;
        }
        preview.looped = true;
        preview.loop_phase = 0;
        preview.play_len = u64::MAX / 4;
        Some(preview)
    }
}

/// Alt-dragging inside an audio clip slides its audio along.
#[derive(Debug, Clone)]
pub(super) struct SlipDrag {
    original: AudioClip,
    anchor_x: f32,
    frames: i64,
}

impl SlipDrag {
    /// The live edit for the pointer at `x`, if it moved the audio.
    pub fn update(
        &mut self,
        viewport: &super::Viewport,
        project: &Project,
        x: f32,
    ) -> Option<Action> {
        let ticks = viewport.tick_at(x) - viewport.tick_at(self.anchor_x);
        let frames = project.audio_material(&self.original, ticks).round() as i64;
        (frames != self.frames).then(|| {
            self.frames = frames;
            Action::LiveSlip {
                original: Box::new(self.original.clone()),
                frames,
            }
        })
    }
}

fn context_menu(
    view: &mut TimelineView,
    ui: &mut egui::Ui,
    session: &Session,
    clip: ClipId,
    step: Tick,
) {
    let clips = if view.selection.has_clip(clip) {
        view.selection.clips.clone()
    } else {
        vec![clip]
    };
    if chrome::menu_item(ui, "Duplicate", "Ctrl+D").clicked() {
        view.push(Action::DuplicateClips(clips.clone()));
        ui.close();
    }
    if let Some(marker) = view.marker
        && chrome::menu_item(ui, "Split at Marker", "Ctrl+E").clicked()
    {
        view.push(Action::SplitClips(clips.clone(), marker));
        ui.close();
    }
    let all_muted = clips
        .iter()
        .all(|id| session.project.clip(*id).is_some_and(|(_, c)| c.muted()));
    let label = if all_muted { "Unmute" } else { "Mute" };
    if chrome::menu_item(ui, label, "0").clicked() {
        view.push(Action::MuteClips(clips.clone(), !all_muted));
        ui.close();
    }
    if chrome::menu_item(ui, "Loop Selection", "Shift+L").clicked() {
        let project = &session.project;
        let span = clips
            .iter()
            .filter_map(|id| project.clip(*id))
            .map(|(_, c)| (c.start(), project.clip_end(c)))
            .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)));
        if let Some((start, end)) = span {
            view.push(Action::LoopSpan(start, end));
        }
        ui.close();
    }
    if matches!(session.project.clip(clip), Some((_, ClipRef::Midi(_))))
        && ui.button("Freeze to Audio").clicked()
    {
        view.push(Action::FreezeClip(clip));
        ui.close();
    }
    if let Some((_, ClipRef::Audio(audio))) = session.project.clip(clip) {
        audio_menu(view, ui, &clips, audio, step);
    }
    ui.separator();
    if chrome::menu_item(ui, "Delete", "Del").clicked() {
        view.push(Action::DeleteClips(clips));
        ui.close();
    }
}

/// The audio part of a clip's context menu. Switches show the clicked
/// clip's state and set every selected audio clip to the opposite.
fn audio_menu(
    view: &mut TimelineView,
    ui: &mut egui::Ui,
    clips: &[ClipId],
    audio: &AudioClip,
    step: Tick,
) {
    ui.separator();
    let mut toggle = |ui: &mut egui::Ui, on: bool, label: &str, edit: AudioEdit| {
        let mut checked = on;
        if ui.checkbox(&mut checked, label).clicked() {
            view.push(Action::EditAudioClips(clips.to_vec(), edit));
            ui.close();
        }
    };
    toggle(
        ui,
        audio.reversed,
        "Reverse",
        AudioEdit::Reverse(!audio.reversed),
    );
    toggle(ui, audio.looped, "Loop", AudioEdit::Loop(!audio.looped));
    toggle(
        ui,
        audio.polarity_inverted,
        "Invert Polarity",
        AudioEdit::InvertPolarity(!audio.polarity_inverted),
    );
    ui.menu_button("Channels", |ui| {
        for (mode, label) in [
            (ChannelMode::Stereo, "Stereo"),
            (ChannelMode::Mono, "Mono"),
            (ChannelMode::Swap, "Swap Left/Right"),
        ] {
            if ui.radio(audio.channels == mode, label).clicked() {
                view.push(Action::EditAudioClips(
                    clips.to_vec(),
                    AudioEdit::Channels(mode),
                ));
                ui.close();
            }
        }
    });
    if ui.button("Normalize").clicked() {
        view.push(Action::NormalizeClips(clips.to_vec()));
        ui.close();
    }
    ui.menu_button("Chop", |ui| {
        if ui.button("At Transients").clicked() {
            view.push(Action::ChopClip(
                audio.id,
                Chop::Transients(DEFAULT_SENSITIVITY),
            ));
            ui.close();
        }
        if ui.button("To Grid").clicked() {
            view.push(Action::ChopClip(audio.id, Chop::Grid(step)));
            ui.close();
        }
    });
    if ui.button("Consolidate").clicked() {
        view.push(Action::ConsolidateClips(clips.to_vec()));
        ui.close();
    }
}

/// Transient detection sensitivity used when chopping from the menu.
pub(crate) const DEFAULT_SENSITIVITY: f32 = 0.5;

fn start_move(
    view: &mut TimelineView,
    ui: &egui::Ui,
    session: &Session,
    track: &Track,
    grabbed: ClipRef<'_>,
) {
    let project = &session.project;
    if !view.selection.has_clip(grabbed.id()) {
        view.selection.clips = vec![grabbed.id()];
        view.selection.tracks = vec![track.id];
    }
    let rows: Vec<_> = view.track_rows().map(|r| r.track).collect();
    let mut clips = Vec::new();
    for id in &view.selection.clips {
        let Some((track, clip)) = project.clip(*id) else {
            continue;
        };
        // Clips inside folded groups aren't on screen and stay put.
        let Some(row) = rows.iter().position(|t| *t == track) else {
            continue;
        };
        clips.push(Moving {
            id: *id,
            row,
            kind: project.track(track).map_or(RowKind::Group, row_kind),
            start: clip.start(),
            end: project.clip_end(clip),
        });
    }
    let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or_default();
    let anchor_row = rows.iter().position(|t| *t == track.id).unwrap_or(0);
    view.gesture = Some(Gesture::MoveClips(MoveClips {
        grabbed_start: grabbed.start(),
        anchor_tick: view.viewport.tick_at(origin.x),
        anchor_row,
        clips,
        delta: Tick::ZERO,
        rows: 0,
    }));
}

pub(super) fn update_move(
    drag: &mut MoveClips,
    viewport: &super::Viewport,
    grid: &super::Grid,
    rows: &[super::RowRect],
    pos: egui::Pos2,
    step: Tick,
    bypass_snap: bool,
) {
    // Snap the grabbed clip's start to the grid; everything else follows.
    let raw = viewport.tick_at(pos.x) - drag.anchor_tick;
    let target = grid.snap(drag.grabbed_start.get() as f64 + raw, step, bypass_snap);
    let earliest = drag
        .clips
        .iter()
        .map(|c| c.start)
        .min()
        .unwrap_or(Tick::ZERO);
    drag.delta = (target - drag.grabbed_start).max(-earliest);

    let track_rows: Vec<_> = rows.iter().filter(|r| r.is_track).collect();
    let pointer_row = track_rows
        .iter()
        .position(|r| r.rect.y_range().contains(pos.y))
        .or_else(|| {
            let last = track_rows.last()?;
            (pos.y > last.rect.bottom()).then_some(track_rows.len() - 1)
        })
        .unwrap_or(drag.anchor_row);
    let delta_rows = pointer_row as isize - drag.anchor_row as isize;
    // Only move across tracks if every clip lands on a track of its kind.
    let fits = drag.clips.iter().all(|c| {
        let row = c.row as isize + delta_rows;
        row >= 0 && (row as usize) < track_rows.len() && track_rows[row as usize].kind == c.kind
    });
    if fits {
        drag.rows = delta_rows;
    }
}

fn start_resize(
    view: &TimelineView,
    session: &Session,
    track: TrackId,
    clip: ClipRef<'_>,
    edge: Edge,
    pointer_x: f32,
    shift: bool,
) -> Resize {
    let project = &session.project;
    let (start, end) = (clip.start(), project.clip_end(clip));
    let unlimited = (Tick::ZERO, Tick::new(i64::MAX / 4));
    let (mut earliest_start, mut latest_end) = unlimited;
    let (mut min_len, mut stretch, mut loops) = (Tick::new(1), false, false);
    if let ClipRef::Audio(audio) = clip {
        if shift {
            // As far as the speed limits allow.
            stretch = true;
            let len = (end - start).get() as f64;
            let speed = audio.stretch.speed;
            let longest = Tick::new((len * speed / Stretch::MIN_SPEED).floor() as i64);
            min_len = Tick::new((len * speed / Stretch::MAX_SPEED).ceil() as i64).max(min_len);
            (earliest_start, latest_end) = (end - longest, start + longest);
        } else {
            (earliest_start, latest_end) = project.audio_clip_limits(audio);
            loops = edge == Edge::Right && !audio.looped;
        }
    }
    let edge_tick = match edge {
        Edge::Left => start,
        Edge::Right => end,
    };
    Resize {
        clip: clip.id(),
        track,
        edge,
        start,
        end,
        earliest_start: earliest_start.max(Tick::ZERO),
        latest_end,
        min_len,
        stretch,
        loops,
        grab_offset: view.viewport.tick_at(pointer_x) - edge_tick.get() as f64,
        original: (start, end),
    }
}

/// Ghosts of the clips being moved, where they'd land.
pub(super) fn paint_move(
    view: &TimelineView,
    painter: &Painter,
    session: &Session,
    drag: &MoveClips,
) {
    if !drag.has_moved() {
        return;
    }
    let ctx = painter.ctx();
    // The preview glides from one snapped place to the next rather than
    // jumping; keyed to this drag, so the next one starts where it is.
    let this_drag = drag.anchor_tick.to_bits();
    let delta = f64::from(theme::glide(
        ctx,
        id("ghost delta", 0).with(this_drag),
        drag.delta.get() as f32,
    ));
    let rows: Vec<_> = view.track_rows().copied().collect();
    for moving in &drag.clips {
        let row = (moving.row as isize + drag.rows) as usize;
        let (Some(row), Some((track, clip))) = (rows.get(row), session.project.clip(moving.id))
        else {
            continue;
        };
        let color = session
            .project
            .track(track)
            .map_or(egui::Color32::GRAY, |t| color32(t.color));
        let top = theme::glide(
            ctx,
            id("ghost top", moving.id.0).with(this_drag),
            row.rect.top(),
        );
        let rect = Rect::from_min_max(
            pos2(
                view.viewport.x(moving.start.get() as f64 + delta),
                top + 2.0,
            ),
            pos2(
                view.viewport.x(moving.end.get() as f64 + delta),
                top + row.rect.height() - 2.0,
            ),
        );
        // Draw the content shifted along with the clip.
        let mut shifted = view.viewport;
        shifted.start -= delta;
        clip::paint(
            painter,
            rect,
            clip,
            color,
            ClipLook::Ghost,
            false,
            session,
            &shifted,
        );
    }
    // Where the grabbed clip will start, and on which track if not its own.
    if let Some(pointer) = ctx.pointer_latest_pos() {
        let at = session
            .project
            .tempo
            .bar_beat(drag.grabbed_start + drag.delta);
        let track = rows
            .get((drag.anchor_row as isize + drag.rows) as usize)
            .filter(|_| drag.rows != 0)
            .and_then(|row| session.project.track(row.track));
        let label = match track {
            Some(track) => format!("{at} · {}", track.name),
            None => at.to_string(),
        };
        chrome::drop_label(ctx, pointer, &label);
    }
}

/// The clip being resized, at its new size.
pub(super) fn paint_resize(
    view: &TimelineView,
    painter: &Painter,
    session: &Session,
    resize: &Resize,
) {
    let Some(row) = view.track_rows().find(|r| r.track == resize.track) else {
        return;
    };
    let Some((track, clip)) = session.project.clip(resize.clip) else {
        return;
    };
    let preview = match clip {
        ClipRef::Audio(audio) => resize.preview(&session.project, audio),
        ClipRef::Midi(_) => None,
    };
    let clip = preview.as_ref().map_or(clip, ClipRef::Audio);
    let color = session
        .project
        .track(track)
        .map_or(egui::Color32::GRAY, |t| color32(t.color));
    let rect = Rect::from_min_max(
        pos2(
            view.viewport.x(resize.start.get() as f64),
            row.rect.top() + 2.0,
        ),
        pos2(
            view.viewport.x(resize.end.get() as f64),
            row.rect.bottom() - 2.0,
        ),
    );
    clip::paint(
        painter,
        rect,
        clip,
        color,
        ClipLook::Ghost,
        false,
        session,
        &view.viewport,
    );
}
