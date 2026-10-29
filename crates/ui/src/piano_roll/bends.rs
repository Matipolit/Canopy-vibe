//! Bend mode: drawing a note's own pitch curve (`project::Note::bend`)
//! right in the roll, where its path shows the pitch it plays.
//!
//! A drag across a note draws: its curve holds where the drag began, then
//! follows the pointer, a point at each grid step (Alt: anywhere), each
//! snapped to a semitone (Shift: to the cent). A drag on one of its points
//! moves the point; Alt-dragging up or down on a segment bends it; a
//! double-click on a point removes it, and a right-click on the note clears
//! its bend. Each gesture is one undo step. Outside Bend mode bent notes
//! still show their path, thinner.

use eframe::egui::{self, Pos2, Rect, Sense, Shape, Stroke};
use midi::Tick;
use project::{BendPoint, Note, NoteId};

use super::{Gesture, PianoRoll, Shown};
use crate::session::Session;
use crate::theme::stroke;

/// How near the pointer must be to a point to take it, in points.
const GRAB: f32 = 6.0;

/// How far apart a path's samples are drawn, in points.
const PATH_STEP: f32 = 3.0;

/// How much an Alt-drag of a point's height bends its segment.
const CURVE_PER_POINT: f32 = 1.0 / 60.0;

/// What a bend gesture does as the pointer moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum BendDrag {
    /// Freehand: a point wherever the pointer goes, replacing those it
    /// passes over since `last`; the point at `hold`, where the drag began,
    /// keeps the value the curve had there.
    Draw { last: Tick, hold: Tick },
    /// Moves point `index`.
    Point(usize),
    /// Bends the segment leading into point `index`, from `curve` where
    /// the drag began at `anchor_y`.
    Curve {
        index: usize,
        anchor_y: f32,
        curve: f32,
    },
}

/// What the pointer is over in Bend mode.
enum Target<'a> {
    Point(&'a Note, usize),
    Note(&'a Note),
}

impl PianoRoll {
    /// The y of the middle of the row `pitch` (fractional) would be on.
    fn y_of_pitch(&self, area: Rect, pitch: f32) -> f32 {
        area.top() + (127.5 - pitch) * self.key_height - self.scroll_y
    }

    /// The (fractional) pitch at height `y`: the inverse of
    /// [`Self::y_of_pitch`].
    fn pitch_of_y(&self, area: Rect, y: f32) -> f32 {
        127.5 - (y - area.top() + self.scroll_y) / self.key_height
    }

    /// Where `note`'s point `point` is on screen.
    fn bend_point_pos(&self, area: Rect, origin: Tick, note: &Note, point: &BendPoint) -> Pos2 {
        let x = self
            .viewport
            .x((origin + note.start + point.at.min(note.len)).get() as f64);
        let y = self.y_of_pitch(area, f32::from(note.pitch.get()) + point.semitones);
        Pos2::new(x, y)
    }

    /// Each bent note's path over it, from its start to its end: what it
    /// plays. In Bend mode brighter, with its points.
    pub(super) fn paint_bends(&self, ui: &egui::Ui, area: Rect, shown: &Shown, notes: &[Note]) {
        let painter = ui.painter_at(area);
        let origin = shown.origin();
        let ink = ui.visuals().strong_text_color();
        let (width, ink) = if self.bend_mode {
            (stroke::EMPHASIS, ink)
        } else {
            (stroke::HAIRLINE + 0.2, ink.gamma_multiply(0.6))
        };
        for note in notes.iter().filter(|n| !n.bend.is_empty()) {
            let left = self.viewport.x((origin + note.start).get() as f64);
            let right = self.viewport.x((origin + note.end()).get() as f64);
            if right < area.left() || left > area.right() {
                continue;
            }
            let steps = ((right - left) / PATH_STEP).ceil().max(1.0) as usize;
            let base = f32::from(note.pitch.get());
            let path: Vec<Pos2> = (0..=steps)
                .map(|i| {
                    let x = left + (right - left) * i as f32 / steps as f32;
                    let at = Tick::new((note.len.get() as f32 * i as f32 / steps as f32) as i64);
                    Pos2::new(x, self.y_of_pitch(area, base + note.bend_at(at)))
                })
                .collect();
            painter.add(Shape::line(path, Stroke::new(width, ink)));
            if self.bend_mode {
                for point in note.bend.iter().filter(|p| p.at <= note.len) {
                    let pos = self.bend_point_pos(area, origin, note, point);
                    painter.circle(pos, 3.0, shown.color, Stroke::new(stroke::HAIRLINE, ink));
                }
            }
        }
    }

    /// What's under `pos` in Bend mode: a point of a bent note's curve, or
    /// a note (its body, or anywhere near its path).
    fn bend_target<'a>(
        &self,
        area: Rect,
        shown: &Shown,
        notes: &'a [Note],
        pos: Pos2,
    ) -> Option<Target<'a>> {
        let origin = shown.origin();
        for note in notes {
            for (i, point) in note.bend.iter().enumerate() {
                if point.at <= note.len
                    && self.bend_point_pos(area, origin, note, point).distance(pos) <= GRAB
                {
                    return Some(Target::Point(note, i));
                }
            }
        }
        let tick = self.viewport.tick_at(pos.x) as i64;
        notes
            .iter()
            .filter(|n| (origin + n.start).get() <= tick && tick < (origin + n.end()).get())
            .find(|n| {
                let at = Tick::new(tick) - origin - n.start;
                let path = self.y_of_pitch(area, f32::from(n.pitch.get()) + n.bend_at(at));
                let body = self.y_of_pitch(area, f32::from(n.pitch.get()));
                let reach = self.key_height.max(GRAB);
                (pos.y - path).abs() <= reach || (pos.y - body).abs() <= self.key_height / 2.0
            })
            .map(Target::Note)
    }

    /// Bend mode's clicks and the start of its drags, over the whole notes
    /// area (it takes the place of the notes' own widgets);
    /// [`Self::drive_bend`] follows the drag.
    pub(super) fn bend_input(
        &mut self,
        ui: &egui::Ui,
        area: Rect,
        session: &mut Session,
        shown: &Shown,
        notes: &[Note],
        step: Tick,
    ) {
        let response = ui.interact(area, egui::Id::new("bend layer"), Sense::click_and_drag());
        if response.is_pointer_button_down_on() {
            self.focused = true;
        }
        let alt = ui.input(|i| i.modifiers.alt);
        if response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(Target::Point(note, index)) = self.bend_target(area, shown, notes, pos)
        {
            let mut points = note.bend.clone();
            points.remove(index);
            let id = note.id;
            session.edit("Bend Note", |p| p.set_note_bend(shown.id, id, points));
            return;
        }
        if response.secondary_clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(Target::Note(note) | Target::Point(note, _)) =
                self.bend_target(area, shown, notes, pos)
        {
            let id = note.id;
            session.edit("Clear Bend", |p| p.set_note_bend(shown.id, id, Vec::new()));
            return;
        }
        if !response.drag_started() {
            return;
        }
        let Some(press) = ui.input(|i| i.pointer.press_origin()) else {
            return;
        };
        let origin = shown.origin();
        let gesture = match self.bend_target(area, shown, notes, press) {
            Some(Target::Point(note, index)) if alt && index > 0 => Some((
                note,
                BendDrag::Curve {
                    index,
                    anchor_y: press.y,
                    curve: note.bend[index].curve,
                },
            )),
            Some(Target::Point(note, index)) => Some((note, BendDrag::Point(index))),
            Some(Target::Note(note)) => {
                let at = Tick::new(self.viewport.tick_at(press.x) as i64) - origin - note.start;
                let next = note.bend.iter().position(|p| p.at > at);
                match next {
                    Some(index) if alt && index > 0 => Some((
                        note,
                        BendDrag::Curve {
                            index,
                            anchor_y: press.y,
                            curve: note.bend[index].curve,
                        },
                    )),
                    _ => Some((
                        note,
                        BendDrag::Draw {
                            last: Tick::ZERO,
                            hold: Tick::ZERO,
                        },
                    )),
                }
            }
            None => None,
        };
        let Some((note, drag)) = gesture else {
            return;
        };
        let mut points = note.bend.clone();
        let drag = match drag {
            BendDrag::Draw { .. } => {
                // The curve holds what it was until where the drag began.
                let at = self.bend_tick(note, origin, press.x, alt, step);
                let held = BendPoint {
                    at,
                    semitones: note.bend_at(at),
                    curve: 0.0,
                };
                points.retain(|p| p.at != at);
                points.push(held);
                points.sort_by_key(|p| p.at);
                BendDrag::Draw { last: at, hold: at }
            }
            other => other,
        };
        self.gesture = Some(Gesture::Bend {
            note: note.id,
            original: note.bend.clone(),
            points,
            drag,
        });
    }

    /// The tick (from `note`'s start, within it) at screen `x`: on the
    /// grid of `step` unless `free`.
    fn bend_tick(&self, note: &Note, origin: Tick, x: f32, free: bool, step: Tick) -> Tick {
        let snapped = self.grid.snap(self.viewport.tick_at(x), step, free);
        (snapped - origin - note.start).clamp(Tick::ZERO, note.len)
    }

    /// Follows the pointer for a bend gesture: `pos` is where it is, `step`
    /// the grid's step.
    pub(super) fn drive_bend(
        &mut self,
        shown: &Shown,
        pos: Pos2,
        modifiers: egui::Modifiers,
        step: Tick,
    ) {
        let area = self.notes_area;
        let origin = shown.origin();
        let Some(Gesture::Bend { note, .. }) = &self.gesture else {
            return;
        };
        let Some(note) = shown.clip.notes.iter().find(|n| n.id == *note) else {
            return;
        };
        let at = self.bend_tick(note, origin, pos.x, modifiers.alt, step);
        let raw = self.pitch_of_y(area, pos.y) - f32::from(note.pitch.get());
        let semitones = if modifiers.shift {
            (raw * 100.0).round() / 100.0
        } else {
            raw.round()
        }
        .clamp(-BendPoint::MAX_SEMITONES, BendPoint::MAX_SEMITONES);
        let Some(Gesture::Bend { points, drag, .. }) = &mut self.gesture else {
            return;
        };
        match drag {
            BendDrag::Draw { last, hold } => {
                // What the pointer passed over is redrawn; where it began
                // stays as it was.
                if at != *hold {
                    let (from, to) = ((*last).min(at), (*last).max(at));
                    let keep = |p: &BendPoint| {
                        p.at == *hold || (p.at != at && (p.at < from || p.at > to || p.at == *last))
                    };
                    points.retain(keep);
                    points.push(BendPoint {
                        at,
                        semitones,
                        curve: 0.0,
                    });
                    points.sort_by_key(|p| p.at);
                    *last = at;
                }
            }
            BendDrag::Point(index) => {
                let index = *index;
                let low = index.checked_sub(1).map_or(Tick::ZERO, |i| points[i].at);
                let high = points.get(index + 1).map_or(note.len, |p| p.at);
                if let Some(point) = points.get_mut(index) {
                    point.at = at.clamp(low, high);
                    point.semitones = semitones;
                }
            }
            BendDrag::Curve {
                index,
                anchor_y,
                curve,
            } => {
                if let Some(point) = points.get_mut(*index) {
                    point.curve = (*curve + (*anchor_y - pos.y) * CURVE_PER_POINT).clamp(-1.0, 1.0);
                }
            }
        }
    }

    /// The edit a finished bend gesture makes, if it changed anything.
    pub(super) fn finish_bend(
        session: &mut Session,
        shown: &Shown,
        note: NoteId,
        original: &[BendPoint],
        points: Vec<BendPoint>,
    ) {
        if points != original {
            session.edit("Bend Note", |p| p.set_note_bend(shown.id, note, points));
        }
    }
}
