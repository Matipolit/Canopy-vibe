//! Automation lanes: breakpoint envelopes under a track, as many per track as
//! there are things to automate.
//!
//! - Double-click to add a point; drag a point to move it; double-click a
//!   point to remove it.
//! - Click points to select them (Ctrl or Shift to add), or drag a box on
//!   empty space; dragging a selected point moves them all, Delete removes
//!   them, and the lane's menu inverts or reverses them.
//! - The lane's menu → Generate fills the selected span (or the bars in
//!   view) with a shape or random values, previewed live.
//! - Drag the dot in the middle of a segment up or down to bend it;
//!   double-click it to straighten it.
//! - In draw mode (the Draw button, B, or Ctrl+drag) dragging paints a point
//!   per grid step.

use std::collections::BTreeMap;

use eframe::egui::{
    self, Align, Align2, ComboBox, FontId, Layout, Pos2, Rect, Sense, Shape, Stroke, UiBuilder,
    pos2, vec2,
};
use midi::Tick;
use project::{
    AutomationPoint, AutomationTarget, Generate, Track, TrackId, TrackKind, Wave, replace_range,
    segment_value,
};

use super::{Action, Gesture, TimelineView, color32, paint_lines, track_header};
use crate::session::Session;
use crate::theme::space;
use crate::widgets::chrome::{self, Icon};

const PAD: f32 = 6.0;
const POINT_RADIUS: f32 = 4.0;
/// Curve dots only show on segments at least this wide.
const MIN_CURVE_DOT_GAP: f32 = 18.0;
/// Pixels of vertical drag per unit of curve.
const CURVE_DRAG: f32 = 60.0;

/// A drag on a lane. The lane is drawn from `points` until release.
#[derive(Debug, Clone)]
pub(super) struct AutomationDrag {
    track: TrackId,
    target: AutomationTarget,
    points: Vec<AutomationPoint>,
    original: Vec<AutomationPoint>,
    what: DragKind,
}

/// Selected points, all in one lane.
#[derive(Debug, Clone, PartialEq)]
pub struct PointSelection {
    pub track: TrackId,
    pub target: AutomationTarget,
    /// Into the lane's points, sorted.
    pub indices: Vec<usize>,
    /// The lane's points when selected. If they change some other way (an
    /// undo, say), the selection is dropped.
    pub points: Vec<AutomationPoint>,
    /// The span a box selection covered.
    pub range: Option<(Tick, Tick)>,
}

impl PointSelection {
    pub fn is_lane(&self, track: TrackId, target: &AutomationTarget) -> bool {
        self.track == track && &self.target == target
    }

    /// The lane's points without the selected ones.
    pub fn without(&self) -> Vec<AutomationPoint> {
        self.points
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.indices.contains(i))
            .map(|(_, p)| *p)
            .collect()
    }

    /// The span to work on: the box, or from the first selected point to
    /// the last.
    fn span(&self) -> Option<(Tick, Tick)> {
        self.range.or_else(|| {
            let ticks = self
                .indices
                .iter()
                .filter_map(|i| self.points.get(*i))
                .map(|p| p.tick);
            let (first, last) = (ticks.clone().min()?, ticks.max()?);
            (last > first).then_some((first, last))
        })
    }

    /// The lane with the selected points' values flipped top to bottom.
    fn inverted(&self) -> Vec<AutomationPoint> {
        let mut points = self.points.clone();
        for &i in &self.indices {
            if let Some(p) = points.get_mut(i) {
                p.value = 1.0 - p.value;
            }
        }
        points
    }

    /// The lane with the selected points mirrored in time across their span.
    fn reversed(&self) -> Vec<AutomationPoint> {
        let Some((first, last)) = self.span() else {
            return self.points.clone();
        };
        let selected: Vec<AutomationPoint> = self
            .indices
            .iter()
            .filter_map(|i| self.points.get(*i))
            .map(|p| AutomationPoint {
                tick: first + (last - p.tick),
                ..*p
            })
            .collect();
        // A segment's curve belongs to its first point; reversed, it
        // belongs to the other end and bends the other way.
        let mut selected = selected;
        selected.sort_by_key(|p| p.tick);
        let curves: Vec<f32> = selected.iter().map(|p| p.curve).collect();
        let n = selected.len();
        for (i, p) in selected.iter_mut().enumerate() {
            p.curve = if i + 1 < n { -curves[n - 2 - i] } else { 0.0 };
        }
        let mut points = self.without();
        points.extend(selected);
        points.sort_by_key(|p| p.tick);
        points
    }
}

#[derive(Debug, Clone)]
enum DragKind {
    /// Moving the selected points (`indices`) by the grabbed one.
    Points { grabbed: usize, indices: Vec<usize> },
    /// A box on empty space, selecting what it covers.
    Select {
        origin: Pos2,
        current: Pos2,
        /// Already selected (with Ctrl or Shift held).
        base: Vec<usize>,
        indices: Vec<usize>,
        range: (Tick, Tick),
    },
    Curve {
        index: usize,
        start: f32,
        anchor_y: f32,
        /// Bending follows the pointer, which means opposite signs for
        /// rising and falling segments.
        falling: bool,
    },
    /// Painted values by grid cell start.
    Draw {
        cells: BTreeMap<i64, f32>,
        last: Option<(i64, f32)>,
    },
}

impl AutomationDrag {
    pub fn track(&self) -> TrackId {
        self.track
    }

    pub fn target(&self) -> &AutomationTarget {
        &self.target
    }

    /// Follows the pointer. `lane` is the lane's rect this frame.
    pub fn update(
        &mut self,
        view: &TimelineView,
        lane: Rect,
        pos: Pos2,
        step: Tick,
        bypass_snap: bool,
    ) {
        let value = value_at_y(lane, pos.y);
        match &mut self.what {
            DragKind::Points { grabbed, indices } => {
                let original = &self.original;
                let Some(anchor) = original.get(*grabbed).copied() else {
                    return;
                };
                let tick = view
                    .grid
                    .snap(view.viewport.tick_at(pos.x), step, bypass_snap);
                // Points keep their order: none passes an unselected
                // neighbour, or goes before zero.
                let (mut low, mut high) = (i64::MIN, i64::MAX);
                for &i in indices.iter() {
                    let t = original[i].tick.get();
                    low = low.max(-t);
                    if let Some(left) = (0..i).rev().find(|j| !indices.contains(j)) {
                        low = low.max(original[left].tick.get() - t);
                    }
                    if let Some(right) = (i + 1..original.len()).find(|j| !indices.contains(j)) {
                        high = high.min(original[right].tick.get() - t);
                    }
                }
                let delta = (tick - anchor.tick).get().clamp(low.min(0), high.max(0));
                let lift = value - anchor.value;
                let mut points = original.clone();
                for &i in indices.iter() {
                    points[i].tick = Tick::new(original[i].tick.get() + delta);
                    points[i].value = (original[i].value + lift).clamp(0.0, 1.0);
                }
                self.points = points;
            }
            DragKind::Select {
                origin,
                current,
                base,
                indices,
                range,
            } => {
                *current = pos;
                let band = Rect::from_two_pos(*origin, pos);
                *indices = base.clone();
                for (i, p) in self.original.iter().enumerate() {
                    let at = pos2(view.viewport.x(p.tick.get() as f64), y_of(lane, p.value));
                    if band.contains(at) && !indices.contains(&i) {
                        indices.push(i);
                    }
                }
                indices.sort_unstable();
                let tick = |x: f32| Tick::new(view.viewport.tick_at(x).max(0.0).round() as i64);
                *range = (tick(band.left()), tick(band.right()));
            }
            DragKind::Curve {
                index,
                start,
                anchor_y,
                falling,
            } => {
                let sign = if *falling { -1.0 } else { 1.0 };
                let curve = (*start + sign * (pos.y - *anchor_y) / CURVE_DRAG).clamp(-1.0, 1.0);
                if let Some(point) = self.points.get_mut(*index) {
                    point.curve = curve;
                }
            }
            DragKind::Draw { cells, last } => {
                let step = step.get().max(1);
                let tick = view.viewport.tick_at(pos.x).max(0.0) as i64;
                let cell = tick.div_euclid(step) * step;
                // Fill in cells skipped by a fast drag, ramping between them.
                if let Some((last_cell, last_value)) = *last {
                    let (from, to) = (last_cell.min(cell), last_cell.max(cell));
                    let mut c = from + step;
                    while c < to {
                        let t = (c - last_cell) as f32 / (cell - last_cell) as f32;
                        cells.insert(c, last_value + (value - last_value) * t);
                        c += step;
                    }
                }
                cells.insert(cell, value);
                *last = Some((cell, value));
                let (Some(&first), Some(&end)) = (cells.keys().next(), cells.keys().next_back())
                else {
                    return;
                };
                let end = end + step;
                let mut points: Vec<AutomationPoint> = self
                    .original
                    .iter()
                    .filter(|p| p.tick.get() < first || p.tick.get() >= end)
                    .copied()
                    .collect();
                points.extend(cells.iter().map(|(&tick, &value)| AutomationPoint {
                    tick: Tick::new(tick),
                    value,
                    curve: 0.0,
                }));
                points.sort_by_key(|p| p.tick);
                self.points = points;
            }
        }
    }

    /// The edit to commit, if anything changed, or the selection made.
    pub fn finish(self) -> Option<Action> {
        if let DragKind::Select { indices, range, .. } = self.what {
            return Some(Action::SelectPoints(PointSelection {
                track: self.track,
                target: self.target,
                indices,
                points: self.original,
                range: Some(range),
            }));
        }
        (self.points != self.original).then_some(Action::SetAutomation {
            track: self.track,
            target: self.target,
            points: self.points,
        })
    }
}

fn y_of(lane: Rect, value: f32) -> f32 {
    lane.bottom() - PAD - value.clamp(0.0, 1.0) * (lane.height() - 2.0 * PAD)
}

fn value_at_y(lane: Rect, y: f32) -> f32 {
    ((lane.bottom() - PAD - y) / (lane.height() - 2.0 * PAD)).clamp(0.0, 1.0)
}

pub(super) fn show_header(
    view: &mut TimelineView,
    ui: &mut egui::Ui,
    track: &Track,
    target: &AutomationTarget,
    rect: Rect,
) {
    let visuals = ui.visuals();
    ui.painter().rect_filled(rect, 0.0, visuals.faint_bg_color);
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        visuals.widgets.noninteractive.bg_stroke,
    );
    // The track's color, fainter, ties the lane to its track; the controls
    // start where the track header's do.
    let strip = rect.with_max_x(rect.left() + track_header::COLOR_STRIP);
    ui.painter()
        .rect_filled(strip, 0.0, color32(track.color).gamma_multiply(0.45));
    let content = Rect::from_min_max(
        pos2(strip.right() + space::S, rect.top() + space::S),
        pos2(rect.right() - space::M, rect.bottom() - space::S),
    );
    let ui = &mut ui.new_child(
        UiBuilder::new()
            .id_salt(("automation_header", track.id.0, format!("{target:?}")))
            .max_rect(content)
            .layout(Layout::left_to_right(Align::Min)),
    );
    let current = track_header::automation_target_name(track, target);
    let shown = view.automation.get(&track.id).cloned().unwrap_or_default();
    ComboBox::from_id_salt(("automation_target", track.id.0, format!("{target:?}")))
        .selected_text(current)
        // Room after it for Draw and the ×, a gap apart.
        .width(content.width() - 104.0)
        .show_ui(ui, |ui| {
            // Listed only while the menu is open: a drum kit has thousands.
            let targets: Vec<_> = track_header::automation_targets(track)
                .into_iter()
                // Each target gets one lane.
                .filter(|c| &c.target == target || !shown.contains(&c.target))
                .collect();
            let mut replace = None;
            track_header::grouped_menu(ui, &targets, &mut |ui, choice| {
                let has_points = track.automation_lane(&choice.target).is_some();
                let label = if has_points {
                    format!("{} •", choice.label)
                } else {
                    choice.label.clone()
                };
                if ui
                    .selectable_label(&choice.target == target, label)
                    .clicked()
                {
                    replace = Some(choice.target.clone());
                }
            });
            if let Some(to) = replace {
                view.push(Action::ReplaceAutomationLane {
                    track: track.id,
                    from: target.clone(),
                    to,
                });
            }
        });
    let draw = view.automation_draw;
    if chrome::switch(ui, draw, "Draw", "Drag to draw points (or hold Ctrl)", "B").clicked() {
        view.automation_draw = !draw;
    }
    if chrome::icon_button(ui, Icon::Remove, "Hide lane", "Hide this lane").clicked() {
        view.push(Action::HideAutomation(track.id, target.clone()));
    }
}

pub(super) fn show_lane(
    view: &mut TimelineView,
    ui: &mut egui::Ui,
    session: &Session,
    track: &Track,
    target: &AutomationTarget,
    rect: Rect,
    step: Tick,
) {
    let visuals = ui.visuals().clone();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, visuals.extreme_bg_color.gamma_multiply(0.85));
    paint_lines(
        &painter,
        rect,
        &view.viewport,
        &session.project.tempo,
        step,
        visuals.weak_text_color().gamma_multiply(0.15),
    );
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        visuals.widgets.noninteractive.bg_stroke,
    );

    let dragging = match &view.gesture {
        Some(Gesture::Automation(drag)) if drag.track == track.id && &drag.target == target => {
            Some(drag.clone())
        }
        _ => None,
    };
    let points = match &dragging {
        Some(drag) => drag.points.clone(),
        None => track
            .automation_lane(target)
            .map(|lane| lane.points.clone())
            .unwrap_or_default(),
    };
    let color = color32(track.color);
    // Copies, so the closures below don't hold on to `view`.
    let (viewport, grid) = (view.viewport, view.grid);
    let x_of = |tick: Tick| viewport.x(tick.get() as f64);

    // The envelope: curved segments, held flat before the first point and
    // after the last.
    if points.is_empty() {
        let value = static_value(track, target);
        painter.hline(
            rect.x_range(),
            y_of(rect, value),
            Stroke::new(1.0, color.gamma_multiply(0.5)),
        );
        // How to start, in what's in view.
        let visible = rect.intersect(ui.clip_rect());
        if visible.height() >= 30.0 && dragging.is_none() {
            chrome::empty_hint(
                &painter,
                visible,
                "",
                "Double-click to add a point, or drag with B (or Ctrl) held to draw",
            );
        }
    } else {
        let mut line = vec![pos2(rect.left(), y_of(rect, points[0].value))];
        for pair in points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (xa, xb) = (x_of(a.tick), x_of(b.tick));
            if xb < rect.left() || xa > rect.right() {
                line.push(pos2(xb, y_of(rect, b.value)));
                continue;
            }
            let steps = (((xb - xa) / 4.0).ceil() as usize).clamp(1, 64);
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                line.push(pos2(
                    xa + (xb - xa) * t,
                    y_of(rect, segment_value(a.value, b.value, a.curve, t)),
                ));
            }
        }
        let last = points[points.len() - 1];
        line.push(pos2(x_of(last.tick), y_of(rect, last.value)));
        line.push(pos2(rect.right(), y_of(rect, last.value)));
        painter.add(Shape::line(line, Stroke::new(1.5, color)));
    }

    // Empty space: add points, or paint them in draw mode.
    let lane_id = egui::Id::new(("automation_lane", track.id.0, format!("{target:?}")));
    let response = ui.interact(rect, lane_id, Sense::click_and_drag());
    let modifiers = ui.input(|i| i.modifiers);
    let drawing = view.automation_draw || modifiers.command;
    let add_point = |pos: Pos2| {
        let tick = grid.snap(viewport.tick_at(pos.x).max(0.0), step, modifiers.alt);
        let mut new_points = points.clone();
        let index = new_points.partition_point(|p| p.tick <= tick);
        new_points.insert(
            index,
            AutomationPoint {
                tick,
                value: value_at_y(rect, pos.y),
                curve: 0.0,
            },
        );
        new_points
    };
    let clicked_to_add = if drawing {
        response.clicked()
    } else {
        response.double_clicked()
    };
    if clicked_to_add && let Some(pos) = response.interact_pointer_pos() {
        view.push(Action::SetAutomation {
            track: track.id,
            target: target.clone(),
            points: add_point(pos),
        });
    }
    let selection = view
        .automation_selection
        .clone()
        .filter(|s| s.is_lane(track.id, target));
    let selected: Vec<usize> = match &dragging {
        Some(AutomationDrag {
            what: DragKind::Select { indices, .. },
            ..
        }) => indices.clone(),
        _ => selection
            .as_ref()
            .map(|s| s.indices.clone())
            .unwrap_or_default(),
    };
    if response.clicked() && !drawing && !modifiers.shift && selection.is_some() {
        view.automation_selection = None;
    }
    if response.drag_started() {
        let what = if drawing {
            DragKind::Draw {
                cells: BTreeMap::new(),
                last: None,
            }
        } else {
            let origin = ui
                .input(|i| i.pointer.press_origin())
                .unwrap_or_else(|| rect.center());
            let base = if modifiers.shift {
                selected.clone()
            } else {
                Vec::new()
            };
            DragKind::Select {
                origin,
                current: origin,
                indices: base.clone(),
                base,
                range: (Tick::ZERO, Tick::ZERO),
            }
        };
        view.gesture = Some(Gesture::Automation(AutomationDrag {
            track: track.id,
            target: target.clone(),
            points: points.clone(),
            original: points.clone(),
            what,
        }));
    }
    response.context_menu(|ui| {
        if ui.button("Generate…").clicked() {
            let span = selection.as_ref().and_then(PointSelection::span);
            let span = span.unwrap_or_else(|| visible_bars(&viewport, &session.project));
            let mut window = GeneratorWindow::new(track.id, target.clone(), span, points.clone());
            window.at = ui.min_rect().left_top();
            view.generator = Some(window);
            ui.close();
        }
        if let Some(selection) = &selection
            && !selection.indices.is_empty()
        {
            let edits = [
                ("Delete Points", "Del", selection.without()),
                ("Invert Points", "", selection.inverted()),
                ("Reverse Points", "", selection.reversed()),
            ];
            for (label, shortcut, new_points) in edits {
                if chrome::menu_item(ui, label, shortcut).clicked() {
                    view.push(Action::SetAutomation {
                        track: track.id,
                        target: target.clone(),
                        points: new_points,
                    });
                    ui.close();
                }
            }
        }
        ui.separator();
        if chrome::menu_item(ui, "Clear Automation", "").clicked() {
            view.push(Action::SetAutomation {
                track: track.id,
                target: target.clone(),
                points: Vec::new(),
            });
            ui.close();
        }
    });
    // The selection box.
    if let Some(AutomationDrag {
        what: DragKind::Select {
            origin, current, ..
        },
        ..
    }) = &dragging
    {
        let band = Rect::from_two_pos(*origin, *current).intersect(rect);
        painter.rect(
            band,
            0.0,
            visuals.selection.bg_fill.gamma_multiply(0.25),
            Stroke::new(1.0, visuals.selection.stroke.color),
            egui::StrokeKind::Inside,
        );
    }

    // Curve dots, then points on top of them.
    for (index, pair) in points.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        let (xa, xb) = (x_of(a.tick), x_of(b.tick));
        if xb - xa < MIN_CURVE_DOT_GAP || (b.value - a.value).abs() < 0.01 {
            continue;
        }
        let center = pos2(
            (xa + xb) / 2.0,
            y_of(rect, segment_value(a.value, b.value, a.curve, 0.5)),
        );
        if !rect.contains(center) {
            continue;
        }
        let hit = Rect::from_center_size(center, vec2(12.0, 12.0));
        let dot_id = egui::Id::new(("automation_curve", track.id.0, format!("{target:?}"), index));
        let dot = ui.interact(hit, dot_id, Sense::click_and_drag());
        let active = dot.hovered() || dot.dragged();
        painter.circle(
            center,
            if active { 4.5 } else { 3.5 },
            visuals.extreme_bg_color,
            Stroke::new(1.5, color),
        );
        if active {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if dot.drag_started() {
            view.gesture = Some(Gesture::Automation(AutomationDrag {
                track: track.id,
                target: target.clone(),
                points: points.clone(),
                original: points.clone(),
                what: DragKind::Curve {
                    index,
                    start: a.curve,
                    anchor_y: ui
                        .input(|i| i.pointer.press_origin())
                        .map_or(center.y, |p| p.y),
                    falling: b.value < a.value,
                },
            }));
        }
        if dot.double_clicked() {
            let mut straight = points.clone();
            straight[index].curve = 0.0;
            view.push(Action::SetAutomation {
                track: track.id,
                target: target.clone(),
                points: straight,
            });
        }
    }

    for (index, point) in points.iter().enumerate() {
        let center = pos2(x_of(point.tick), y_of(rect, point.value));
        if !rect.expand(POINT_RADIUS).contains(center) {
            continue;
        }
        let hit = Rect::from_center_size(center, vec2(POINT_RADIUS * 3.0, POINT_RADIUS * 3.0));
        let point_id =
            egui::Id::new(("automation_point", track.id.0, format!("{target:?}"), index));
        let point_response = ui.interact(hit, point_id, Sense::click_and_drag());
        let active = point_response.hovered() || point_response.dragged();
        let is_selected = selected.contains(&index);
        painter.circle_filled(
            center,
            if active || is_selected {
                POINT_RADIUS + 1.5
            } else {
                POINT_RADIUS
            },
            color,
        );
        painter.circle_stroke(
            center,
            if is_selected {
                POINT_RADIUS + 1.5
            } else {
                POINT_RADIUS
            },
            if is_selected {
                Stroke::new(1.5, egui::Color32::WHITE)
            } else {
                Stroke::new(1.0, egui::Color32::from_black_alpha(160))
            },
        );
        if point_response.clicked() {
            let mut indices = if modifiers.shift || modifiers.command {
                selected.clone()
            } else {
                Vec::new()
            };
            match indices.iter().position(|i| *i == index) {
                Some(at) if modifiers.command => {
                    indices.remove(at);
                }
                Some(_) => {}
                None => indices.push(index),
            }
            indices.sort_unstable();
            view.automation_selection = Some(PointSelection {
                track: track.id,
                target: target.clone(),
                indices,
                points: points.clone(),
                range: None,
            });
        }

        if point_response.double_clicked() {
            let mut new_points = points.clone();
            new_points.remove(index);
            view.push(Action::SetAutomation {
                track: track.id,
                target: target.clone(),
                points: new_points,
            });
        }
        if point_response.drag_started() {
            // Dragging a selected point moves the selection; any other
            // point becomes the selection.
            let indices = if selected.contains(&index) {
                selected.clone()
            } else {
                view.automation_selection = Some(PointSelection {
                    track: track.id,
                    target: target.clone(),
                    indices: vec![index],
                    points: points.clone(),
                    range: None,
                });
                vec![index]
            };
            view.gesture = Some(Gesture::Automation(AutomationDrag {
                track: track.id,
                target: target.clone(),
                points: points.clone(),
                original: points.clone(),
                what: DragKind::Points {
                    grabbed: index,
                    indices,
                },
            }));
        }
    }

    // While dragging a point or a curve, show its value.
    if let Some(drag) = &dragging {
        let shown = match drag.what {
            DragKind::Points { grabbed, .. } => {
                drag.points.get(grabbed).map(|p| (x_of(p.tick), p.value))
            }
            DragKind::Select { .. } => None,
            DragKind::Curve { index, .. } => drag.points.get(index..index + 2).map(|pair| {
                let (a, b) = (pair[0], pair[1]);
                (
                    (x_of(a.tick) + x_of(b.tick)) / 2.0,
                    segment_value(a.value, b.value, a.curve, 0.5),
                )
            }),
            DragKind::Draw { ref last, .. } => {
                last.map(|(tick, value)| (viewport.x(tick as f64), value))
            }
        };
        if let Some((x, value)) = shown {
            let at = pos2(x, y_of(rect, value));
            let align = if at.x > rect.right() - 80.0 {
                Align2::RIGHT_BOTTOM
            } else {
                Align2::LEFT_BOTTOM
            };
            painter.text(
                at + vec2(
                    if align == Align2::LEFT_BOTTOM {
                        8.0
                    } else {
                        -8.0
                    },
                    -4.0,
                ),
                align,
                format_value(track, target, value),
                FontId::proportional(crate::theme::text::SMALL),
                visuals.strong_text_color(),
            );
        }
    }
}

/// What the target is set to without automation, on the `0..=1` scale.
fn static_value(track: &Track, target: &AutomationTarget) -> f32 {
    match target {
        AutomationTarget::Volume => project::volume_to_normalized(track.mixer.volume_db),
        AutomationTarget::Pan => project::pan_to_normalized(track.mixer.pan),
        AutomationTarget::InstrumentParam(id) => param(track, id).map_or(0.5, |p| {
            p.to_normalized(track.instrument_param(id).unwrap_or(p.default_value()))
        }),
        AutomationTarget::DeviceParam { device, param } => {
            let device = track.devices.iter().find(|d| d.id == *device);
            let found =
                device.and_then(|d| Some((device_param(track, d.id, param)?, d.param(param)?)));
            found.map_or(0.5, |(p, value)| p.to_normalized(value))
        }
        // Modulators' parameters are described by the modulators crate.
        AutomationTarget::ModulatorParam { .. } => 0.5,
    }
}

fn param(track: &Track, id: &str) -> Option<instruments::ParamDescriptor> {
    let TrackKind::Midi { instrument, .. } = &track.kind else {
        return None;
    };
    instrument
        .descriptor()?
        .params
        .iter()
        .find(|p| p.id == id)
        .copied()
}

fn device_param(
    track: &Track,
    device: project::DeviceId,
    id: &str,
) -> Option<instruments::ParamDescriptor> {
    track
        .devices
        .iter()
        .find(|d| d.id == device)?
        .effect
        .descriptor()?
        .params
        .iter()
        .find(|p| p.id == id)
        .copied()
}

/// A lane value in the target's own units.
pub fn format_value(track: &Track, target: &AutomationTarget, normalized: f32) -> String {
    match target {
        AutomationTarget::Volume => {
            let db = project::normalized_to_volume(normalized);
            if db <= project::SILENCE_DB {
                "-inf dB".to_owned()
            } else {
                format!("{db:.1} dB")
            }
        }
        AutomationTarget::Pan => track_header::format_pan(project::normalized_to_pan(normalized)),
        AutomationTarget::InstrumentParam(id) => param(track, id).map_or_else(
            || format!("{normalized:.2}"),
            |p| p.format(p.from_normalized(normalized)),
        ),
        AutomationTarget::DeviceParam { device, param } => device_param(track, *device, param)
            .map_or_else(
                || format!("{normalized:.2}"),
                |p| p.format(p.from_normalized(normalized)),
            ),
        AutomationTarget::ModulatorParam { .. } => format!("{normalized:.2}"),
    }
}

/// Whole bars covering what's on screen.
fn visible_bars(viewport: &super::Viewport, project: &project::Project) -> (Tick, Tick) {
    let bar = project.tempo.bar_len().get().max(1) as f64;
    let first = (viewport.start.max(0.0) / bar).floor() * bar;
    let last = (viewport.end() / bar).ceil() * bar;
    (
        Tick::new(first as i64),
        Tick::new((last as i64).max(first as i64 + bar as i64)),
    )
}

/// The Generate window: a shape or random values over a span of a lane,
/// shown live while it's open.
#[derive(Debug, Clone)]
pub struct GeneratorWindow {
    pub track: TrackId,
    pub target: AutomationTarget,
    pub settings: Generate,
    /// The lane before generating, which everything outside the span keeps.
    original: Vec<AutomationPoint>,
    /// What was last sent to the lane.
    sent: Option<Vec<AutomationPoint>>,
    /// Where it opens: by the menu it came from.
    pub at: Pos2,
}

/// Period choices, as (label, quarter notes; negative counts bars).
const PERIODS: [(&str, f64); 8] = [
    ("4 bars", -4.0),
    ("2 bars", -2.0),
    ("1 bar", -1.0),
    ("1/2", 2.0),
    ("1/4", 1.0),
    ("1/8", 0.5),
    ("1/16", 0.25),
    ("1/32", 0.125),
];

fn period_ticks(choice: f64, project: &project::Project) -> Tick {
    if choice < 0.0 {
        project.tempo.bar_len() * (-choice) as i64
    } else {
        Tick::new((choice * midi::PPQ as f64) as i64)
    }
}

impl GeneratorWindow {
    pub fn new(
        track: TrackId,
        target: AutomationTarget,
        (start, end): (Tick, Tick),
        original: Vec<AutomationPoint>,
    ) -> GeneratorWindow {
        GeneratorWindow {
            track,
            target,
            settings: Generate {
                wave: Wave::Sine,
                start,
                end,
                period: Tick::new(3840),
                low: 0.0,
                high: 1.0,
                phase: 0.0,
                seed: 1,
            },
            original,
            sent: None,
            at: Pos2::ZERO,
        }
    }

    fn points(&self) -> Vec<AutomationPoint> {
        let s = &self.settings;
        replace_range(&self.original, s.start, s.end, s.points())
    }
}

/// Shows the Generate window, if open, and previews its settings live.
pub(super) fn show_generator(view: &mut TimelineView, ctx: &egui::Context, session: &Session) {
    let Some(mut window) = view.generator.take() else {
        return;
    };
    let project = &session.project;
    if project.track(window.track).is_none() {
        view.push(Action::CancelLive);
        return;
    }
    let mut outcome = None;
    egui::Window::new("Generate")
        .collapsible(false)
        .resizable(false)
        .default_pos(window.at)
        .show(ctx, |ui| {
            let s = &mut window.settings;
            let tempo = &project.tempo;
            ui.weak(format!(
                "{} to {}",
                tempo.bar_beat(s.start),
                tempo.bar_beat(s.end)
            ));
            egui::Grid::new("generate").num_columns(2).show(ui, |ui| {
                ui.label("Shape");
                ComboBox::from_id_salt("generate_wave")
                    .selected_text(s.wave.label())
                    .show_ui(ui, |ui| {
                        for wave in Wave::ALL {
                            ui.selectable_value(&mut s.wave, wave, wave.label());
                        }
                    });
                ui.end_row();
                ui.label(if s.wave.is_random() {
                    "Every"
                } else {
                    "Period"
                });
                let current = PERIODS
                    .iter()
                    .find(|(_, q)| period_ticks(*q, project) == s.period)
                    .map_or("?", |(label, _)| label);
                ComboBox::from_id_salt("generate_period")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (label, quarters) in PERIODS {
                            ui.selectable_value(
                                &mut s.period,
                                period_ticks(quarters, project),
                                label,
                            );
                        }
                    });
                ui.end_row();
                let percent = |ui: &mut egui::Ui, value: &mut f32| {
                    let mut p = *value * 100.0;
                    ui.add(
                        egui::DragValue::new(&mut p)
                            .range(0.0..=100.0)
                            .speed(0.5)
                            .max_decimals(0)
                            .suffix("%"),
                    );
                    *value = p / 100.0;
                };
                ui.label("High");
                percent(ui, &mut s.high);
                ui.end_row();
                ui.label("Low");
                percent(ui, &mut s.low);
                ui.end_row();
                if s.wave.is_random() {
                    ui.label("");
                    if ui.button("Reroll").clicked() {
                        s.seed = s.seed.wrapping_add(1);
                    }
                } else {
                    ui.label("Phase");
                    let mut degrees = s.phase * 360.0;
                    ui.add(
                        egui::DragValue::new(&mut degrees)
                            .range(0.0..=359.0)
                            .speed(1.0)
                            .max_decimals(0)
                            .suffix("°"),
                    );
                    s.phase = degrees / 360.0;
                }
                ui.end_row();
            });
            ui.separator();
            let buttons = chrome::dialog_buttons(ui, "Apply", true, None, "Cancel");
            if buttons.primary.clicked() {
                outcome = Some(true);
            }
            if buttons.cancel.clicked() {
                outcome = Some(false);
            }
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        outcome = Some(false);
    }
    match outcome {
        Some(true) => view.push(Action::EndLive("Generate Automation")),
        Some(false) => view.push(Action::CancelLive),
        None => {
            let points = window.points();
            if window.sent.as_ref() != Some(&points) {
                window.sent = Some(points.clone());
                view.push(Action::LiveAutomation {
                    track: window.track,
                    target: window.target.clone(),
                    points,
                });
            }
            view.generator = Some(window);
        }
    }
}
