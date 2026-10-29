//! Bar numbers across the top of the arrangement and of the piano roll: one
//! ruler for both, so they look and behave the same.
//!
//! - Click or drag to move the playhead.
//! - Drag along the top strip (the loop brace), or Shift+drag anywhere, to
//!   set the loop; drag inside the brace to move it, and its ends to change
//!   it. Click the brace to turn looping on or off.
//!
//! The loop is the project's: the piano roll shows the same ticks as the
//! arrangement, so its brace is the same stretch of the song.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, Id, Rect, Sense, Shape, Stroke, pos2,
};
use midi::Tick;
use project::{LoopRegion, TempoMap};

use super::{Action, Grid, TimelineView, Viewport, id};
use crate::session::Session;
use crate::theme::{anim, color, radius, size, text};

/// Bar numbers are at least this far apart.
const MIN_LABEL_SPACING: f32 = 40.0;
/// How close to a loop end a drag grabs it.
const EDGE_GRAB: f32 = 5.0;

/// What a ruler shows and snaps to.
pub(crate) struct Ruler<'a> {
    /// Keeps the drag's state apart from another ruler's.
    pub id: Id,
    pub viewport: &'a Viewport,
    pub grid: &'a Grid,
    pub step: Tick,
    pub tempo: &'a TempoMap,
    pub region: Option<LoopRegion>,
    pub playhead: f64,
    /// The arrangement's marker (where Ctrl+E splits), if shown here.
    pub marker: Option<Tick>,
}

/// What a click or drag on a ruler asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RulerAction {
    Seek(Tick),
    /// A new loop, or the old one moved, resized or turned on or off.
    SetLoop(LoopRegion),
    /// A drag that set the loop just ended.
    LoopSet,
}

/// What a drag on the ruler moves.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Grip {
    Playhead,
    /// A new loop, from where the drag started.
    Loop(Tick),
    Start,
    End,
    /// The whole loop, grabbed this far after its start.
    Move(Tick),
}

/// The arrangement's ruler.
pub(super) fn show(
    view: &mut TimelineView,
    ui: &mut egui::Ui,
    rect: Rect,
    session: &Session,
    step: Tick,
) {
    let ruler = Ruler {
        id: id("ruler", 0),
        viewport: &view.viewport,
        grid: &view.grid,
        step,
        tempo: &session.project.tempo,
        region: session.project.loop_region,
        playhead: session.playhead(),
        marker: view.marker,
    };
    let action = ruler.show(ui, rect);
    match action {
        Some(RulerAction::Seek(tick)) => view.push(Action::Seek(tick)),
        Some(RulerAction::SetLoop(region)) => view.push(Action::SetLoop(Some(region))),
        Some(RulerAction::LoopSet) | None => {}
    }
}

impl Ruler<'_> {
    /// Paints the ruler in `rect` and returns what a click or drag on it
    /// asks for.
    pub fn show(&self, ui: &mut egui::Ui, rect: Rect) -> Option<RulerAction> {
        self.paint(ui, rect);
        self.interact(ui, rect)
    }

    fn brace(rect: Rect) -> Rect {
        rect.with_max_y(rect.top() + size::LOOP_BRACE)
    }

    fn paint(&self, ui: &egui::Ui, rect: Rect) {
        let viewport = self.viewport;
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        painter.rect_filled(rect, 0.0, visuals.faint_bg_color);
        // The brace's strip, a shade darker: where dragging sets the loop.
        // It lightens under the pointer, to say it does something.
        let over_brace = ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|p| Self::brace(rect).contains(p));
        let lit =
            ui.ctx()
                .animate_bool_with_time(self.id.with("brace hover"), over_brace, anim::FAST);
        let strip = Color32::from_black_alpha(40).lerp_to_gamma(Color32::from_white_alpha(14), lit);
        painter.rect_filled(Self::brace(rect), 0.0, strip);
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            visuals.widgets.noninteractive.bg_stroke,
        );

        let weak = visuals.weak_text_color();
        let label_top = rect.top() + size::LOOP_BRACE + 1.0;
        let bar = self.tempo.bar_len().get().max(1);
        let bar_px = viewport.width_of(bar as f64);
        let mut every = 1;
        while bar_px * (every as f32) < MIN_LABEL_SPACING {
            every *= 2;
        }
        let first_bar = ((viewport.start / bar as f64).floor() as i64).max(0);
        let last_bar = (viewport.end() / bar as f64).ceil() as i64;
        for index in first_bar..=last_bar {
            let x = viewport.x((index * bar) as f64).round() + 0.5;
            if index % every == 0 {
                painter.vline(
                    x,
                    label_top..=rect.bottom(),
                    Stroke::new(1.0, weak.gamma_multiply(0.7)),
                );
                painter.text(
                    pos2(x + 4.0, label_top),
                    Align2::LEFT_TOP,
                    (index + 1).to_string(),
                    text::medium(text::SMALL),
                    visuals.text_color(),
                );
            } else {
                painter.vline(
                    x,
                    rect.bottom() - 8.0..=rect.bottom(),
                    Stroke::new(1.0, weak),
                );
            }
        }
        // Beat ticks once there's room for them.
        let beat = self.tempo.beat_len().get().max(1);
        if viewport.width_of(beat as f64) >= 8.0 {
            let first = ((viewport.start / beat as f64).floor() as i64).max(0);
            let last = (viewport.end() / beat as f64).ceil() as i64;
            for index in first..=last {
                if (index * beat) % bar != 0 {
                    let x = viewport.x((index * beat) as f64).round() + 0.5;
                    painter.vline(
                        x,
                        rect.bottom() - 4.0..=rect.bottom(),
                        Stroke::new(1.0, weak),
                    );
                }
            }
        }

        if let Some(region) = self.region {
            let span = viewport.x(region.start.get() as f64)..=viewport.x(region.end.get() as f64);
            // On and off fade into each other.
            let on =
                ui.ctx()
                    .animate_bool_with_time(self.id.with("loop on"), region.on, anim::NORMAL);
            let fill = weak.gamma_multiply(0.6).lerp_to_gamma(color::LOOP, on);
            if on > 0.0 {
                painter.rect_filled(
                    Rect::from_x_y_ranges(span.clone(), rect.y_range()),
                    0.0,
                    color::LOOP.gamma_multiply(0.12 * on),
                );
            }
            let brace =
                Rect::from_x_y_ranges(span, Self::brace(rect).y_range()).shrink2([0.0, 1.0].into());
            painter.rect_filled(brace, CornerRadius::same(radius::SMALL), fill);
            // Grips at its ends.
            for x in [brace.left() + 1.5, brace.right() - 1.5] {
                painter.vline(
                    x,
                    brace.y_range(),
                    Stroke::new(1.0, Color32::from_black_alpha(90)),
                );
            }
        }

        if let Some(marker) = self.marker {
            let x = viewport.x(marker.get() as f64);
            painter.vline(
                x,
                rect.bottom() - 10.0..=rect.bottom(),
                Stroke::new(2.0, visuals.text_color()),
            );
        }
        let x = viewport.x(self.playhead);
        painter.add(Shape::convex_polygon(
            vec![
                pos2(x - 5.0, rect.bottom() - 9.0),
                pos2(x + 5.0, rect.bottom() - 9.0),
                pos2(x, rect.bottom()),
            ],
            color::PLAYHEAD,
            Stroke::NONE,
        ));
    }

    fn interact(&self, ui: &mut egui::Ui, rect: Rect) -> Option<RulerAction> {
        let viewport = self.viewport;
        let region = self.region;
        let response = ui.interact(rect, self.id, Sense::click_and_drag());
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        let (bypass, shift) = ui.input(|i| (i.modifiers.alt, i.modifiers.shift));
        let tick_at = |x: f32| {
            self.grid
                .snap(viewport.tick_at(x).max(0.0), self.step, bypass)
        };
        // Near one of the loop's ends, a drag moves that end; inside the
        // brace, the whole loop; elsewhere in the brace's strip, it draws a
        // new one.
        let brace = Self::brace(rect);
        let grip_at = |pos: egui::Pos2| {
            let near = |tick: Tick| (viewport.x(tick.get() as f64) - pos.x).abs() <= EDGE_GRAB;
            if let Some(region) = region {
                if near(region.end) {
                    return Some(Grip::End);
                }
                if near(region.start) {
                    return Some(Grip::Start);
                }
            }
            if !brace.contains(pos) {
                return None;
            }
            let tick = viewport.tick_at(pos.x);
            Some(match region {
                Some(r) if (r.start.get() as f64..r.end.get() as f64).contains(&tick) => {
                    Grip::Move(Tick::new(tick.round() as i64) - r.start)
                }
                _ => Grip::Loop(tick_at(pos.x)),
            })
        };
        if let Some(pos) = response.hover_pos() {
            let cursor = match grip_at(pos) {
                Some(Grip::Start | Grip::End) => Some(CursorIcon::ResizeHorizontal),
                Some(Grip::Move(_)) => Some(if response.dragged() {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::Grab
                }),
                _ => None,
            };
            if let Some(cursor) = cursor {
                ui.ctx().set_cursor_icon(cursor);
            }
        }
        let grip_id = self.id.with("grip");
        // Where the press began: by the time it counts as a drag, the
        // pointer has moved on.
        let pressed = ui.input(|i| i.pointer.press_origin()).or(pointer);
        if response.drag_started()
            && let Some(pos) = pressed
        {
            let grip = if shift {
                Grip::Loop(tick_at(pos.x))
            } else {
                grip_at(pos).unwrap_or(Grip::Playhead)
            };
            ui.data_mut(|d| d.insert_temp(grip_id, grip));
        }
        let grip = ui
            .data(|d| d.get_temp::<Grip>(grip_id))
            .unwrap_or(Grip::Playhead);
        let mut action = None;
        if response.dragged()
            && let Some(pos) = pointer
        {
            let tick = tick_at(pos.x);
            let on = region.is_none_or(|r| r.on);
            action = match (grip, region) {
                (Grip::Loop(anchor), _) => Some(RulerAction::SetLoop(LoopRegion {
                    start: anchor,
                    end: tick,
                    on: true,
                })),
                (Grip::Start, Some(r)) if tick < r.end => Some(RulerAction::SetLoop(LoopRegion {
                    start: tick,
                    on,
                    ..r
                })),
                (Grip::End, Some(r)) if tick > r.start => {
                    Some(RulerAction::SetLoop(LoopRegion { end: tick, on, ..r }))
                }
                (Grip::Move(offset), Some(r)) => {
                    let len = r.end - r.start;
                    let raw = viewport.tick_at(pos.x) - offset.get() as f64;
                    let start = self.grid.snap(raw.max(0.0), self.step, bypass);
                    Some(RulerAction::SetLoop(LoopRegion {
                        start,
                        end: start + len,
                        ..r
                    }))
                }
                (Grip::Playhead, _) => Some(RulerAction::Seek(tick)),
                _ => None,
            };
        }
        if response.clicked()
            && !shift
            && let Some(pos) = pointer
        {
            // A click on the loop's brace turns it on or off; anywhere else
            // it moves the playhead.
            action = match (grip_at(pos), region) {
                (Some(Grip::Move(_)), Some(r)) => {
                    Some(RulerAction::SetLoop(LoopRegion { on: !r.on, ..r }))
                }
                _ => Some(RulerAction::Seek(tick_at(pos.x))),
            };
        }
        if response.drag_stopped() && !matches!(grip, Grip::Playhead) {
            action = Some(RulerAction::LoopSet);
        }
        crate::widgets::chrome::shortcut_tip(
            response,
            "Click to move the playhead. Drag along the top strip (or Shift+drag) to set the \
             loop; drag the loop to move it, its ends to change it, click it to turn it on or off",
            "L",
        );
        // Unchanged regions aren't worth a rebuild.
        match action {
            Some(RulerAction::SetLoop(new)) if Some(new) == region => None,
            other => other,
        }
    }
}
