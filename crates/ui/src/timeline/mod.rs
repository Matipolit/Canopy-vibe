//! The arrangement timeline: ruler, track headers and lanes.
//!
//! Rendering only reads the project. Interactions queue [`Action`]s, which
//! are applied once the frame is drawn, so nothing changes under the loop
//! that is painting it.

mod action;
mod automation;
pub(crate) mod clip;
mod grid;
mod interaction;
mod lane;
mod ruler;
mod track_header;
mod viewport;

use std::collections::HashMap;

use eframe::egui::{
    self, Color32, CursorIcon, Id, Pos2, Rect, ScrollArea, Sense, Stroke, StrokeKind, UiBuilder,
    pos2, vec2,
};
use midi::Tick;
use project::{AutomationTarget, ClipId, Project, TrackId, TrackKind, TrackPosition};

use crate::session::Session;
use crate::theme::{color, stroke};
use crate::widgets::chrome;
pub use action::{Action, AudioEdit, Chop, ClipChange, SelectMode};
pub use grid::{Grid, GridSize, paint_lines};
pub(crate) use interaction::DEFAULT_SENSITIVITY;
pub(crate) use ruler::{Ruler, RulerAction};
pub use track_header::{
    automate_menu, clicked_control_menu, clicked_control_menu_with, control_menu, format_pan,
    volume_drag,
};

/// What controls' menus show about MIDI controllers.
#[derive(Debug, Clone, Default)]
pub struct Controls {
    pub links: Vec<project::ControllerLink>,
    pub learning: Option<(Option<TrackId>, project::AutomationTarget)>,
}
pub use viewport::{Glide, Viewport, WheelZoom};

pub const HEADER_WIDTH: f32 = 240.0;
pub const RULER_HEIGHT: f32 = crate::theme::size::RULER;
const TRACK_HEIGHT: f32 = 54.0;
/// Alt+wheel zooms track heights within this range.
const TRACK_HEIGHTS: std::ops::RangeInclusive<f32> = 26.0..=200.0;
/// Room below the last track, where dropping a file makes a new track.
const TAIL_HEIGHT: f32 = 120.0;
const SCROLLBAR_HEIGHT: f32 = 10.0;
/// Dragging within this many pixels of the lanes' edge scrolls.
const EDGE_SCROLL_ZONE: f32 = 40.0;
/// Pixels per second at the zone's edge; faster beyond it.
const EDGE_SCROLL_SPEED: f32 = 900.0;

#[derive(Debug, Default, Clone)]
pub struct Selection {
    pub tracks: Vec<TrackId>,
    pub clips: Vec<ClipId>,
    /// The clip the piano roll shows.
    pub open_clip: Option<ClipId>,
    /// The master is selected (clicked in the meter panel): the device view
    /// shows its effects. Selecting a track or clip clears it.
    pub master: bool,
    /// Selected effects, in the chain the device view shows (others are
    /// dropped as it changes).
    pub devices: Vec<project::DeviceId>,
}

impl Selection {
    pub fn has_clip(&self, id: ClipId) -> bool {
        self.clips.contains(&id)
    }

    pub fn has_track(&self, id: TrackId) -> bool {
        self.tracks.contains(&id)
    }

    /// The track things like new clips and imports go to.
    pub fn primary_track(&self) -> Option<TrackId> {
        self.tracks.last().copied()
    }
}

/// A drag in progress. Nothing is committed until the button is released,
/// which makes each gesture one undo step and one engine rebuild.
#[derive(Debug, Clone)]
enum Gesture {
    MoveClips(interaction::MoveClips),
    Resize(interaction::Resize),
    RubberBand {
        origin: Pos2,
        current: Pos2,
        /// Clips selected before, kept when Ctrl or Shift is held.
        base: Vec<ClipId>,
    },
    MoveTracks {
        ids: Vec<TrackId>,
        target: Option<TrackDrop>,
    },
    Automation(automation::AutomationDrag),
    ClipHandle(interaction::HandleDrag),
    Slip(interaction::SlipDrag),
}

#[derive(Debug, Clone, Copy)]
struct TrackDrop {
    position: TrackPosition,
    /// Where to draw the insertion line, or the group to highlight.
    indicator: DropIndicator,
}

#[derive(Debug, Clone, Copy)]
enum DropIndicator {
    Line(f32),
    Into(Rect),
}

/// Where a row was drawn last frame, for drags that cross rows.
#[derive(Debug, Clone, Copy)]
struct RowRect {
    track: TrackId,
    rect: Rect,
    /// False for an automation lane under its track.
    is_track: bool,
    /// For an automation lane, the target it shows.
    lane: Option<usize>,
    kind: RowKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Midi,
    Audio,
    Group,
}

/// One row to draw this frame.
#[derive(Debug, Clone)]
struct RowSpec {
    track: TrackId,
    depth: usize,
    /// An automation lane: its index among the track's lanes, and target.
    automation: Option<(usize, AutomationTarget)>,
}

pub struct TimelineView {
    pub viewport: Viewport,
    pub grid: Grid,
    pub selection: Selection,
    /// Where Ctrl+E splits and the last click in a lane landed.
    pub marker: Option<Tick>,
    /// Keep the playhead in view while playing.
    pub follow: bool,
    /// The automation lanes each track shows, top to bottom.
    pub automation: HashMap<TrackId, Vec<AutomationTarget>>,
    /// Dragging on an automation lane paints points (B, or the lane's Draw button).
    pub automation_draw: bool,
    /// Set when a clip was opened for editing; the app shows the piano roll.
    pub open_requested: bool,
    /// Tracks to export, from a track's context menu; the app opens the
    /// Export dialog.
    pub export_requested: Option<Vec<TrackId>>,
    /// Controller links and the control waiting for one, refreshed by the
    /// app each frame, for controls' menus.
    pub controls: Controls,
    /// Selected automation points, in one lane.
    pub automation_selection: Option<automation::PointSelection>,
    /// The automation Generate window, while it's open.
    pub generator: Option<automation::GeneratorWindow>,
    /// Where files are being dragged over the window, when the app hears
    /// about it directly rather than through egui (Wayland).
    pub external_hover: Option<Pos2>,
    /// The playhead last frame, to notice it running off the right edge.
    last_playhead: f64,
    /// Row height, zoomed with Alt+wheel.
    pub track_height: f32,
    /// The rows' scroll position last frame, and where the scrolled area
    /// starts on screen, to keep the row under the pointer still while zooming.
    scroll_offset: f32,
    scroll_top: f32,
    pending_scroll: Option<f32>,
    gesture: Option<Gesture>,
    renaming: Option<(TrackId, String)>,
    rows: Vec<RowRect>,
    clip_rects: Vec<(ClipId, Rect)>,
    lanes_rect: Rect,
    scrollbar_rect: Rect,
    actions: Vec<Action>,
    /// A zoom to fit on its way there.
    glide: Option<Glide>,
    /// Ctrl+wheel zoom still easing in.
    wheel_zoom: WheelZoom,
    /// Rows growing open (an automation lane just shown, a group's tracks
    /// just unfolded), by when they started (egui time; NaN until first
    /// drawn).
    opening: HashMap<RowKey, f64>,
    /// Rows shrinking shut (a lane just hidden, a group's tracks just
    /// folded away), oldest first.
    closing: Vec<Closing>,
}

/// Which row a row is, for rows that grow open and shrink shut.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum RowKey {
    Track(TrackId),
    Lane(TrackId, AutomationTarget),
}

/// Rows that have just gone (hidden or folded away), still drawn where
/// they were while they shrink shut, the way rows grow open: over
/// [`anim::NORMAL`](crate::theme::anim::NORMAL), eased out.
#[derive(Debug, Clone)]
struct Closing {
    /// The row they were under.
    after: RowKey,
    rows: Vec<RowSpec>,
    /// When they started (egui time; NaN until first drawn).
    started: f64,
}

impl RowKey {
    fn of(spec: &RowSpec) -> RowKey {
        match &spec.automation {
            Some((_, target)) => RowKey::Lane(spec.track, target.clone()),
            None => RowKey::Track(spec.track),
        }
    }
}

impl Default for TimelineView {
    fn default() -> Self {
        TimelineView {
            viewport: Viewport::default(),
            grid: Grid::default(),
            selection: Selection::default(),
            marker: None,
            follow: true,
            automation: HashMap::new(),
            automation_draw: false,
            open_requested: false,
            export_requested: None,
            controls: Controls::default(),
            automation_selection: None,
            generator: None,
            external_hover: None,
            last_playhead: 0.0,
            track_height: TRACK_HEIGHT,
            scroll_offset: 0.0,
            scroll_top: 0.0,
            pending_scroll: None,
            gesture: None,
            renaming: None,
            rows: Vec::new(),
            clip_rects: Vec::new(),
            lanes_rect: Rect::NOTHING,
            scrollbar_rect: Rect::NOTHING,
            actions: Vec::new(),
            glide: None,
            wheel_zoom: WheelZoom::default(),
            opening: HashMap::new(),
            closing: Vec::new(),
        }
    }
}

pub fn color32(color: project::Color) -> Color32 {
    Color32::from_rgb(color.r, color.g, color.b)
}

impl TimelineView {
    pub fn push(&mut self, action: Action) {
        self.actions.push(action);
    }

    pub fn show(&mut self, ui: &mut egui::Ui, session: &mut Session) {
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, Sense::hover());
        ui.painter()
            .rect_filled(full, 0.0, ui.visuals().window_fill);
        let ruler_rect = Rect::from_min_max(
            pos2(full.left() + HEADER_WIDTH, full.top()),
            pos2(full.right(), full.top() + RULER_HEIGHT),
        );
        let corner = Rect::from_min_max(full.min, pos2(ruler_rect.left(), ruler_rect.bottom()));
        let scrollbar = Rect::from_min_max(
            pos2(ruler_rect.left(), full.bottom() - SCROLLBAR_HEIGHT),
            full.max,
        );
        let body = Rect::from_min_max(
            pos2(full.left(), ruler_rect.bottom()),
            pos2(full.right(), scrollbar.top()),
        );
        self.viewport
            .set_screen(ruler_rect.left(), ruler_rect.width());
        self.lanes_rect = Rect::from_min_max(pos2(ruler_rect.left(), body.top()), body.max);
        if let Some(glide) = self.glide {
            let t = glide.progress(ui.input(|i| i.time));
            glide.apply(&mut self.viewport, t);
            if t < 1.0 {
                ui.ctx().request_repaint();
            } else {
                self.glide = None;
            }
        }

        self.handle_wheel(ui, Rect::from_min_max(ruler_rect.min, body.max));
        if self
            .wheel_zoom
            .step(&mut self.viewport, ui.input(|i| i.stable_dt))
        {
            ui.ctx().request_repaint();
        }
        self.handle_vertical_zoom(ui, body);
        // Page when the playhead runs off the right edge, but don't fight a
        // user who scrolled away from it.
        let playhead = session.playhead();
        let was_visible = (self.viewport.start..=self.viewport.end()).contains(&self.last_playhead);
        if session.is_playing() && self.follow && self.gesture.is_none() && was_visible {
            self.viewport.follow(playhead);
        }
        self.last_playhead = playhead;

        let step = self.grid.step(&self.viewport, &session.project.tempo);
        self.corner(ui, corner, &session.project);
        ruler::show(self, ui, ruler_rect, session, step);

        let specs = self.row_specs(&session.project);
        let mut rows = Vec::with_capacity(specs.len());
        let mut clip_rects = Vec::new();
        let mut tail = Rect::NOTHING;
        let mut scroll = ScrollArea::vertical()
            .id_salt("timeline_rows")
            .auto_shrink([false, false]);
        if let Some(offset) = self.pending_scroll.take() {
            scroll = scroll.vertical_scroll_offset(offset.max(0.0));
        }
        let automation_height = (self.track_height * 1.2).max(48.0);
        let now = ui.input(|i| i.time);
        let drawn = self.with_closing(&specs, now);
        let output = ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            scroll.show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                for (spec, shutting) in &drawn {
                    let Some(track) = session.project.track(spec.track) else {
                        continue;
                    };
                    let full_height = match &spec.automation {
                        Some(_) => automation_height,
                        None => self.track_height,
                    };
                    let shown = match shutting {
                        Some(shown) => *shown,
                        None => self.growth(&RowKey::of(spec), now),
                    };
                    let height = full_height * shown;
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(body.width(), height), Sense::hover());
                    // A row growing open or shrinking shut shows only as
                    // much as is open, but is drawn at full height. Drags
                    // find rows growing open, not ones going away.
                    let clip = ui.clip_rect();
                    if height < full_height {
                        ui.set_clip_rect(clip.intersect(rect));
                    }
                    let full = rect.with_max_y(rect.top() + full_height);
                    let header = full.with_max_x(full.left() + HEADER_WIDTH);
                    let lane = full.with_min_x(full.left() + HEADER_WIDTH);
                    let mut gone = Vec::new();
                    let clips = if shutting.is_some() {
                        &mut gone
                    } else {
                        rows.push(RowRect {
                            track: track.id,
                            rect: full,
                            is_track: spec.automation.is_none(),
                            lane: spec.automation.as_ref().map(|(i, _)| *i),
                            kind: row_kind(track),
                        });
                        &mut clip_rects
                    };
                    match &spec.automation {
                        None => {
                            track_header::show(self, ui, session, track, spec.depth, header);
                            lane::show(self, ui, session, track, lane, step, clips);
                        }
                        Some((_, target)) => {
                            automation::show_header(self, ui, track, target, header);
                            automation::show_lane(self, ui, session, track, target, lane, step);
                        }
                    }
                    ui.set_clip_rect(clip);
                }
                let height = TAIL_HEIGHT.max(ui.available_height());
                let (rect, response) =
                    ui.allocate_exact_size(vec2(body.width(), height), Sense::click());
                tail = rect;
                if response.clicked() {
                    self.push(Action::ClearSelection);
                }
                if response.double_clicked() {
                    self.push(Action::AddTrack(
                        project::NewTrack::Midi,
                        TrackPosition::END,
                    ));
                }
            })
        });
        // Rows that weren't drawn (in a folded group) don't grow.
        self.opening.retain(|row, started| {
            !started.is_nan() && specs.iter().any(|spec| RowKey::of(spec) == *row)
        });
        if !self.opening.is_empty() || !self.closing.is_empty() {
            ui.ctx().request_repaint();
        }
        self.scroll_offset = output.inner.state.offset.y;
        self.scroll_top = output.inner.inner_rect.top();
        self.rows = rows;
        self.clip_rects = clip_rects;

        self.drive_gesture(ui, session, step);
        automation::show_generator(self, ui.ctx(), session);
        self.scrollbar(ui, scrollbar, session);
        self.paint_overlays(ui, session, tail);
        self.handle_file_hover(ui, session, step);

        let actions = std::mem::take(&mut self.actions);
        for action in actions {
            self.apply(session, action);
        }
        // Kept with the project, so it opens as it was left.
        session.keep_arrangement_view(self.arrangement_view());
    }

    /// How far the arrangement is scrolled and zoomed.
    pub fn arrangement_view(&self) -> project::ArrangementView {
        project::ArrangementView {
            start: self.viewport.start,
            px_per_tick: self.viewport.px_per_tick,
            track_height: self.track_height,
            scroll: self.scroll_offset,
        }
    }

    /// Scrolls and zooms to `view` (a project's, as it was saved), within
    /// what the arrangement allows.
    pub fn restore_view(&mut self, view: &project::ArrangementView) {
        self.viewport.show_from(view.start, view.px_per_tick);
        self.track_height = view
            .track_height
            .clamp(*TRACK_HEIGHTS.start(), *TRACK_HEIGHTS.end());
        self.scroll_offset = view.scroll.max(0.0);
        self.pending_scroll = Some(self.scroll_offset);
    }

    /// How far a row has grown open, 0 to 1 (eased): 1 unless it was just
    /// shown.
    fn growth(&mut self, row: &RowKey, now: f64) -> f32 {
        let Some(started) = self.opening.get_mut(row) else {
            return 1.0;
        };
        if started.is_nan() {
            *started = now;
        }
        let t = ((now - *started) / f64::from(crate::theme::anim::NORMAL)) as f32;
        if t >= 1.0 {
            self.opening.remove(row);
            return 1.0;
        }
        crate::theme::anim::ease_out(t)
    }

    /// Has `row` grow open from nothing when it's next drawn.
    fn grow_open(&mut self, row: RowKey) {
        self.opening.insert(row, f64::NAN);
    }

    /// Has `rows`, just gone from under the row `after`, shrink shut there
    /// from when they're next drawn.
    fn shrink_shut(&mut self, after: RowKey, rows: Vec<RowSpec>) {
        if rows.is_empty() {
            return;
        }
        for row in &rows {
            self.opening.remove(&RowKey::of(row));
        }
        self.closing.push(Closing {
            after,
            rows,
            started: f64::NAN,
        });
    }

    /// Has the rows under group `group` shrink shut, as it folds: its
    /// tracks (and their lanes) down to the next row at its depth.
    fn fold_shut(&mut self, project: &Project, group: TrackId) {
        let rows = project.rows();
        let Some(at) = rows.iter().position(|r| r.id == group) else {
            return;
        };
        let depth = rows[at].depth;
        let going = rows[at + 1..]
            .iter()
            .take_while(|r| r.depth > depth)
            .flat_map(|r| {
                let track = RowSpec {
                    track: r.id,
                    depth: r.depth,
                    automation: None,
                };
                std::iter::once(track).chain(self.lane_specs(r.id, r.depth))
            })
            .collect();
        let after = self
            .lane_specs(group, depth)
            .last()
            .map_or(RowKey::Track(group), RowKey::of);
        self.shrink_shut(after, going);
    }

    /// Has track `track`'s automation lane for `target` (or every lane it
    /// shows) shrink shut, as it's hidden.
    fn lane_shut(&mut self, project: &Project, track: TrackId, target: Option<&AutomationTarget>) {
        let Some(row) = project.rows().into_iter().find(|r| r.id == track) else {
            return;
        };
        let lanes = self.lane_specs(track, row.depth);
        let Some(first) = lanes.iter().position(|lane| {
            target.is_none_or(|t| lane.automation.as_ref().is_some_and(|(_, l)| l == t))
        }) else {
            return;
        };
        let after = match first {
            0 => RowKey::Track(track),
            n => RowKey::of(&lanes[n - 1]),
        };
        let going = match target {
            Some(_) => vec![lanes[first].clone()],
            None => lanes,
        };
        self.shrink_shut(after, going);
    }

    /// The rows to draw this frame: `specs`, with the rows still shrinking
    /// shut back where they were, each with how much of it is still open.
    /// Rows that have finished shrinking are forgotten.
    fn with_closing(&mut self, specs: &[RowSpec], now: f64) -> Vec<(RowSpec, Option<f32>)> {
        let shut = |started: f64| ((now - started) / f64::from(crate::theme::anim::NORMAL)) as f32;
        for closing in &mut self.closing {
            if closing.started.is_nan() {
                closing.started = now;
            }
        }
        self.closing.retain(|c| shut(c.started) < 1.0);
        let mut drawn: Vec<(RowSpec, Option<f32>)> =
            specs.iter().map(|spec| (spec.clone(), None)).collect();
        // Newest first, each right under the row it was under: rows hidden
        // one after another come back in their order.
        for closing in self.closing.iter().rev() {
            let Some(at) = drawn
                .iter()
                .position(|(spec, _)| RowKey::of(spec) == closing.after)
            else {
                continue;
            };
            let open = 1.0 - crate::theme::anim::ease_out(shut(closing.started));
            // A row shown again meanwhile is drawn as itself.
            let going = closing
                .rows
                .iter()
                .filter(|row| specs.iter().all(|s| RowKey::of(s) != RowKey::of(row)))
                .map(|row| (row.clone(), Some(open)));
            drawn.splice(at + 1..at + 1, going);
        }
        drawn
    }

    /// The rows of the track `track`'s automation lanes, as drawn, at
    /// `depth`.
    fn lane_specs(&self, track: TrackId, depth: usize) -> Vec<RowSpec> {
        self.automation
            .get(&track)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, target)| RowSpec {
                track,
                depth,
                automation: Some((index, target.clone())),
            })
            .collect()
    }

    /// Zooms to the selected clips, or with none selected (or `song`) to the
    /// whole song (at least eight bars), easing there.
    pub fn zoom_to_fit(&mut self, ctx: &egui::Context, project: &Project, song: bool) {
        let selected = self
            .selection
            .clips
            .iter()
            .filter(|_| !song)
            .filter_map(|id| project.clip(*id))
            .map(|(_, clip)| (clip.start(), project.clip_end(clip)))
            .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)));
        let (start, end) = selected.unwrap_or_else(|| {
            let end = project.end().max(project.tempo.bar_len() * 8);
            (Tick::ZERO, end)
        });
        let target = self
            .viewport
            .fitted(start.get() as f64, end.get().max(start.get() + 1) as f64);
        self.wheel_zoom.stop();
        self.glide = Some(Glide::new(&self.viewport, &target, ctx.input(|i| i.time)));
        ctx.request_repaint();
    }

    /// The grid step the timeline snaps to at its current zoom.
    pub fn grid_step(&self, session: &Session) -> Tick {
        self.grid.step(&self.viewport, &session.project.tempo)
    }

    /// Follows the pointer for the drag in progress and commits it when the
    /// button is released, wherever that happens. Escape cancels it.
    fn drive_gesture(&mut self, ui: &egui::Ui, session: &Session, step: Tick) {
        if self.gesture.is_none() {
            return;
        }
        let (down, pos, alt, escape) = ui.input(|i| {
            (
                i.pointer.primary_down(),
                i.pointer.latest_pos(),
                i.modifiers.alt,
                i.key_pressed(egui::Key::Escape),
            )
        });
        if escape {
            // Handle drags and slips are applied live; put the clip back.
            if let Some(Gesture::ClipHandle(_) | Gesture::Slip(_)) = self.gesture.take() {
                self.push(Action::CancelLive);
            }
            return;
        }
        if !down {
            let rows: Vec<TrackId> = self.track_rows().map(|r| r.track).collect();
            match self.gesture.take() {
                Some(Gesture::MoveClips(drag)) => {
                    if let Some(moves) = drag.moves(&rows) {
                        self.push(Action::MoveClips(moves));
                    }
                }
                Some(Gesture::Resize(resize)) => {
                    if resize.changed() {
                        self.push(resize.action());
                    }
                }
                Some(Gesture::Slip(_)) => self.push(Action::EndLive("Slip Audio")),
                Some(Gesture::MoveTracks {
                    ids,
                    target: Some(target),
                }) => self.push(Action::MoveTracks(ids, target.position)),
                Some(Gesture::Automation(drag)) => {
                    if let Some(action) = drag.finish() {
                        self.push(action);
                    }
                }
                Some(Gesture::ClipHandle(drag)) => self.push(Action::EndLive(drag.label())),
                _ => {}
            }
            return;
        }
        let Some(pos) = pos else {
            return;
        };
        // Near an edge, scroll that way, faster the closer the pointer gets.
        if matches!(
            self.gesture,
            Some(
                Gesture::MoveClips(_)
                    | Gesture::Resize(_)
                    | Gesture::RubberBand { .. }
                    | Gesture::Automation(_)
            )
        ) {
            let lanes = self.lanes_rect;
            let dt = ui.input(|i| i.stable_dt).min(0.1);
            let speed =
                |depth: f32| (depth / EDGE_SCROLL_ZONE).clamp(0.0, 2.0) * EDGE_SCROLL_SPEED * dt;
            let left = lanes.left() + EDGE_SCROLL_ZONE - pos.x;
            let right = pos.x - (lanes.right() - EDGE_SCROLL_ZONE);
            if left > 0.0 {
                self.viewport.scroll_by_px(speed(left));
                ui.ctx().request_repaint();
            } else if right > 0.0 {
                self.viewport.scroll_by_px(-speed(right));
                ui.ctx().request_repaint();
            }
        }
        let rows = self.rows.clone();
        let viewport = self.viewport;
        let grid = self.grid;
        let mut gesture = self.gesture.take();
        match &mut gesture {
            Some(Gesture::MoveClips(drag)) => {
                interaction::update_move(drag, &viewport, &grid, &rows, pos, step, alt);
            }
            Some(Gesture::Resize(resize)) => resize.update(&viewport, &grid, pos.x, step, alt),
            Some(Gesture::RubberBand {
                origin,
                current,
                base,
            }) => {
                *current = pos;
                let band = Rect::from_two_pos(*origin, pos);
                let mut clips = base.clone();
                for (id, rect) in &self.clip_rects {
                    if rect.intersects(band) && !clips.contains(id) {
                        clips.push(*id);
                    }
                }
                self.selection.clips = clips;
            }
            Some(Gesture::MoveTracks { ids, target }) => {
                *target = track_header::drop_target(self, &session.project, pos.y, ids);
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            }
            Some(Gesture::Automation(drag)) => {
                if let Some(lane) = self.lane_rect(drag.track(), drag.target()) {
                    drag.update(self, lane, pos, step, alt);
                }
            }
            Some(Gesture::ClipHandle(drag)) => {
                if let Some(change) = drag.update(&viewport, &session.project, pos) {
                    self.push(Action::LiveClip(drag.clip, change));
                }
            }
            Some(Gesture::Slip(drag)) => {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                if let Some(action) = drag.update(&viewport, &session.project, pos.x) {
                    self.push(action);
                }
            }
            None => {}
        }
        self.gesture = gesture;
    }

    /// A sideways scrollbar under the lanes, over the song plus some room.
    fn scrollbar(&mut self, ui: &egui::Ui, rect: Rect, session: &Session) {
        self.scrollbar_rect = rect;
        let visuals = ui.visuals();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
        let bar = session.project.tempo.bar_len().get() as f64;
        let visible = self.viewport.end() - self.viewport.start;
        let total = (session.project.end().get() as f64 + bar * 8.0)
            .max(self.viewport.end())
            .max(1.0);
        let x_of = |tick: f64| rect.left() + (tick / total) as f32 * rect.width();
        let thumb = Rect::from_min_max(
            pos2(x_of(self.viewport.start), rect.top() + 2.0),
            pos2(
                x_of(self.viewport.start + visible).max(x_of(self.viewport.start) + 12.0),
                rect.bottom() - 2.0,
            ),
        );
        let response = ui.interact(rect, id("scrollbar", 0), Sense::click_and_drag());
        let lit = ui.ctx().animate_bool_with_time(
            id("scrollbar lit", 0),
            response.hovered() || response.dragged(),
            crate::theme::anim::FAST,
        );
        let fill = visuals
            .widgets
            .inactive
            .bg_fill
            .lerp_to_gamma(visuals.widgets.hovered.bg_fill, lit);
        painter.rect_filled(thumb, crate::theme::radius::CONTROL, fill);
        if response.dragged() {
            self.glide = None;
            self.viewport
                .shift(response.drag_delta().x as f64 / rect.width() as f64 * total);
        } else if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            // Glide over to center the view where the bar was clicked.
            let tick = (pos.x - rect.left()) as f64 / rect.width() as f64 * total;
            let mut target = self.viewport;
            target.shift(tick - visible / 2.0 - self.viewport.start);
            self.wheel_zoom.stop();
            self.glide = Some(Glide::new(&self.viewport, &target, ui.input(|i| i.time)));
            ui.ctx().request_repaint();
        }
    }

    /// Alt+wheel over the tracks makes rows taller or shorter, keeping the
    /// row under the pointer where it is.
    fn handle_vertical_zoom(&mut self, ui: &egui::Ui, body: Rect) {
        let (alt, pointer, scroll) = ui.input(|i| {
            (
                i.modifiers.alt,
                i.pointer.hover_pos(),
                i.smooth_scroll_delta.y,
            )
        });
        let Some(pointer) = pointer.filter(|p| body.contains(*p)) else {
            return;
        };
        if !alt || scroll == 0.0 {
            return;
        }
        // egui turns Alt+wheel into a vertical scroll; take it before the
        // scroll area does.
        ui.ctx().input_mut(|i| i.smooth_scroll_delta.y = 0.0);
        let old = self.track_height;
        let new =
            (old * (scroll / 300.0).exp()).clamp(*TRACK_HEIGHTS.start(), *TRACK_HEIGHTS.end());
        if new == old {
            return;
        }
        self.track_height = new;
        let in_content = pointer.y - self.scroll_top + self.scroll_offset;
        self.pending_scroll = Some(in_content * new / old - (pointer.y - self.scroll_top));
    }

    /// Ctrl+wheel zooms around the pointer and Shift+wheel scrolls sideways.
    /// Plain wheel is left to the vertical scroll area.
    fn handle_wheel(&mut self, ui: &egui::Ui, area: Rect) {
        let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        if !area.contains(pointer) || pointer.x < self.viewport.left {
            return;
        }
        let (zoom, scroll, alt) =
            ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers.alt));
        if alt {
            return;
        }
        if zoom != 1.0 || scroll.x != 0.0 {
            // The wheel takes over from a zoom to fit.
            self.glide = None;
        }
        if zoom != 1.0 {
            self.wheel_zoom.add(zoom as f64, pointer.x);
        }
        if scroll.x != 0.0 {
            self.viewport.scroll_by_px(scroll.x);
            ui.ctx().input_mut(|i| i.smooth_scroll_delta.x = 0.0);
        }
    }

    /// The rows to draw: visible tracks, each followed by its automation
    /// lane if one is shown.
    fn row_specs(&self, project: &Project) -> Vec<RowSpec> {
        let mut specs = Vec::new();
        for row in project.rows() {
            specs.push(RowSpec {
                track: row.id,
                depth: row.depth,
                automation: None,
            });
            specs.extend(self.lane_specs(row.id, row.depth));
        }
        specs
    }

    fn corner(&mut self, ui: &mut egui::Ui, rect: Rect, project: &Project) {
        let position = self.new_track_position(project);
        ui.painter()
            .rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
        let ui = &mut ui.new_child(
            UiBuilder::new()
                .id_salt("timeline_corner")
                .max_rect(rect.shrink2(vec2(6.0, 2.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let midi = ui.small_button("+ MIDI");
        if chrome::shortcut_tip(midi, "New MIDI track", "Ctrl+Shift+T").clicked() {
            self.push(Action::AddTrack(project::NewTrack::Midi, position));
        }
        let audio = ui.small_button("+ Audio");
        if chrome::shortcut_tip(audio, "New audio track", "Ctrl+T").clicked() {
            self.push(Action::AddTrack(project::NewTrack::Audio, position));
        }
        if ui
            .small_button("+ Group")
            .on_hover_text("New empty group (Ctrl+G groups the selected tracks)")
            .clicked()
        {
            self.push(Action::AddTrack(project::NewTrack::Group, position));
        }
    }

    /// New tracks go after the selected one, like in Ableton.
    pub fn new_track_position(&self, project: &Project) -> TrackPosition {
        match self.selection.primary_track() {
            Some(id) => TrackPosition::after(project, id),
            None => TrackPosition::END,
        }
    }

    fn paint_overlays(&mut self, ui: &egui::Ui, session: &Session, tail: Rect) {
        let lanes = self.lanes_rect;
        let painter = ui.painter().with_clip_rect(lanes);
        let visuals = ui.visuals();

        if let Some(marker) = self.marker {
            let x = self.viewport.x(marker.get() as f64);
            painter.vline(
                x,
                lanes.y_range(),
                Stroke::new(1.0, visuals.text_color().gamma_multiply(0.6)),
            );
        }
        // The loop's bounds, while it's on.
        if let Some(region) = session.project.loop_region.filter(|r| r.on) {
            for tick in [region.start, region.end] {
                let x = self.viewport.x(tick.get() as f64);
                painter.vline(
                    x,
                    lanes.y_range(),
                    Stroke::new(1.0, color::LOOP.gamma_multiply(0.5)),
                );
            }
        }
        let x = self.viewport.x(session.playhead());
        painter.vline(
            x,
            lanes.y_range(),
            Stroke::new(stroke::EMPHASIS, color::PLAYHEAD),
        );

        match &self.gesture {
            Some(Gesture::MoveClips(drag)) => {
                interaction::paint_move(self, &painter, session, drag)
            }
            Some(Gesture::Resize(resize)) => {
                interaction::paint_resize(self, &painter, session, resize)
            }
            Some(Gesture::RubberBand {
                origin, current, ..
            }) => {
                let rect = Rect::from_two_pos(*origin, *current);
                painter.rect(
                    rect,
                    0.0,
                    visuals.selection.bg_fill.gamma_multiply(0.2),
                    Stroke::new(1.0, visuals.selection.stroke.color),
                    StrokeKind::Inside,
                );
            }
            Some(Gesture::MoveTracks {
                target: Some(target),
                ..
            }) => {
                let painter = ui
                    .painter()
                    .with_clip_rect(lanes.with_min_x(lanes.left() - HEADER_WIDTH));
                let stroke = Stroke::new(stroke::INDICATOR, crate::theme::accent(visuals));
                match target.indicator {
                    DropIndicator::Line(y) => {
                        // It slides from gap to gap as the pointer moves.
                        let y = crate::theme::glide_marker(ui.ctx(), id("track drop line", 0), y);
                        painter.hline(lanes.left() - HEADER_WIDTH..=lanes.right(), y, stroke);
                    }
                    DropIndicator::Into(rect) => {
                        painter.rect_stroke(
                            rect,
                            crate::theme::radius::CONTROL,
                            stroke,
                            StrokeKind::Inside,
                        );
                    }
                }
            }
            _ => {}
        }
        // What the room under the tracks is for, when there's room to say.
        if self.gesture.is_none() && tail.height() >= 48.0 && self.external_hover.is_none() {
            let area = tail.with_min_x(lanes.left());
            let (title, hint) = if session.project.tracks.is_empty() {
                (
                    "No tracks yet",
                    "Drop audio files here, double-click for a MIDI track, or use + MIDI / + Audio",
                )
            } else {
                (
                    "",
                    "Double-click here for a new MIDI track · drop audio files to add tracks",
                )
            };
            let painter = ui.painter().with_clip_rect(area);
            chrome::empty_hint(&painter, area.with_max_y(area.top() + 64.0), title, hint);
        }
    }

    /// Shows where dragged-in files will land.
    fn handle_file_hover(&mut self, ui: &egui::Ui, session: &Session, step: Tick) {
        let pos = match self.external_hover {
            Some(pos) => pos,
            None => {
                if ui.input(|i| i.raw.hovered_files.is_empty()) {
                    return;
                }
                let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else {
                    return;
                };
                pos
            }
        };
        if !self.lanes_rect.contains(pos) {
            return;
        }
        let (track, tick) = self.drop_target(pos, session, step);
        let painter = ui.painter().with_clip_rect(self.lanes_rect);
        let color = ui.visuals().selection.stroke.color;
        let x = self.viewport.x(tick.get() as f64);
        let rect = match track.and_then(|t| self.rows.iter().find(|r| r.track == t && r.is_track)) {
            Some(row) => row.rect.with_min_x(self.lanes_rect.left()),
            None => self.lanes_rect,
        };
        painter.rect_filled(rect, 0.0, color.gamma_multiply(0.08));
        painter.vline(x, rect.y_range(), Stroke::new(stroke::INDICATOR, color));
        ui.ctx().set_cursor_icon(CursorIcon::Copy);
        let label = match track.and_then(|t| session.project.track(t)) {
            Some(track) => format!("Onto {}", track.name),
            None => "New audio track".to_owned(),
        };
        chrome::drop_label(ui.ctx(), pos, &label);
    }

    /// The audio track and position a file dropped at `pos` goes to. `None`
    /// means a new track.
    pub fn drop_target(&self, pos: Pos2, session: &Session, step: Tick) -> (Option<TrackId>, Tick) {
        let tick = self
            .grid
            .snap_floor(self.viewport.tick_at(pos.x).max(0.0), step, false);
        let track = self
            .track_row_at(pos.y)
            .filter(|row| row.kind == RowKind::Audio)
            .map(|row| row.track);
        let _ = session;
        (track, tick)
    }

    /// Drops files at `pos`, or onto the selected track when there's no
    /// pointer position (some platforms don't report one during a drop).
    pub fn drop_files(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        pos: Option<Pos2>,
        session: &Session,
    ) {
        let step = self.grid.step(&self.viewport, &session.project.tempo);
        let inside = pos.filter(|p| self.lanes_rect.contains(*p));
        let (track, tick) = match inside {
            Some(pos) => self.drop_target(pos, session, step),
            None => (
                self.selection.primary_track(),
                self.marker.unwrap_or(Tick::ZERO),
            ),
        };
        let position = self.new_track_position(&session.project);
        // .sfz instruments go to a sampler on whatever track they land on;
        // the rest is audio.
        let (instruments, audio): (Vec<_>, Vec<_>) = paths
            .into_iter()
            .partition(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("sfz")));
        if !instruments.is_empty() {
            let under = match inside {
                Some(pos) => self.track_row_at(pos.y).map(|row| row.track),
                None => self.selection.primary_track(),
            };
            self.push(Action::LoadSfz(instruments, under, position));
        }
        if !audio.is_empty() {
            self.push(Action::Import(audio, track, tick, position));
        }
    }

    fn track_row_at(&self, y: f32) -> Option<&RowRect> {
        self.rows
            .iter()
            .find(|row| row.is_track && row.rect.y_range().contains(y))
    }

    /// The track rows from last frame, top to bottom.
    fn track_rows(&self) -> impl Iterator<Item = &RowRect> {
        self.rows.iter().filter(|row| row.is_track)
    }

    /// Where a track's row was drawn last frame, header included.
    #[cfg(test)]
    pub fn row_rect(&self, track: TrackId) -> Option<Rect> {
        self.track_rows().find(|r| r.track == track).map(|r| r.rect)
    }

    /// Where a clip was drawn last frame.
    #[cfg(test)]
    pub fn clip_rect(&self, clip: ClipId) -> Option<Rect> {
        self.clip_rects
            .iter()
            .find(|(id, _)| *id == clip)
            .map(|(_, r)| *r)
    }

    /// A point on the ruler at `tick`: in the loop brace along its top, or
    /// below it.
    #[cfg(test)]
    pub fn ruler_point(&self, tick: f64, brace: bool) -> Pos2 {
        let top = self.lanes_rect.top() - RULER_HEIGHT;
        let y = if brace {
            crate::theme::size::LOOP_BRACE / 2.0
        } else {
            RULER_HEIGHT - 6.0
        };
        pos2(self.viewport.x(tick), top + y)
    }

    #[cfg(test)]
    pub fn scrollbar_rect(&self) -> Rect {
        self.scrollbar_rect
    }

    /// The first automation lane under a track, if any is shown.
    #[cfg(test)]
    pub fn automation_rect(&self, track: TrackId) -> Option<Rect> {
        let target = self.automation.get(&track)?.first()?.clone();
        self.lane_rect(track, &target)
    }

    /// Where the lane for `target` under `track` was drawn last frame.
    fn lane_rect(&self, track: TrackId, target: &AutomationTarget) -> Option<Rect> {
        let index = self
            .automation
            .get(&track)?
            .iter()
            .position(|t| t == target)?;
        self.rows
            .iter()
            .find(|r| r.track == track && r.lane == Some(index))
            .map(|r| r.rect.with_min_x(self.lanes_rect.left()))
    }

    /// Keeps the selection pointing at things that still exist, e.g. after undo.
    pub fn prune(&mut self, project: &Project) {
        self.selection
            .tracks
            .retain(|id| project.track(*id).is_some());
        self.selection
            .clips
            .retain(|id| project.clip(*id).is_some());
        if let Some(open) = self.selection.open_clip
            && project.clip(open).is_none()
        {
            self.selection.open_clip = None;
        }
        self.automation.retain(|id, _| project.track(*id).is_some());
        // A selection of points that have since changed some other way.
        if let Some(selection) = &self.automation_selection {
            let lane = project
                .track(selection.track)
                .and_then(|t| t.automation_lane(&selection.target))
                .map_or(&[][..], |l| &l.points[..]);
            if lane != &selection.points[..] {
                self.automation_selection = None;
            }
        }
    }
}

fn row_kind(track: &project::Track) -> RowKind {
    match track.kind {
        TrackKind::Midi { .. } => RowKind::Midi,
        TrackKind::Audio { .. } => RowKind::Audio,
        TrackKind::Group { .. } => RowKind::Group,
    }
}

/// A stable id for something drawn in the timeline.
fn id(kind: &str, n: u32) -> Id {
    Id::new(("timeline", kind, n))
}
