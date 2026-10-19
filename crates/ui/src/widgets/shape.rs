//! A drawable shape over one cycle, as in Cableguys ShaperBox: a row of
//! points from 0 to 1, drawn by dragging across them, with the curve
//! between them as the effect plays it and a playhead where it is now.
//! Right-click for ready-made shapes.

use eframe::egui::{self, Color32, Id, Pos2, Rect, Sense, Shape, Stroke, pos2, vec2};

/// What changed this frame, for the caller to apply.
#[derive(Debug, Default)]
pub struct ShapeResponse {
    /// `(point, new value)` pairs.
    pub changes: Vec<(usize, f32)>,
    /// A drag ended, or a preset was picked.
    pub finished: bool,
}

pub struct ShapeEditor<'a> {
    id: Id,
    points: &'a [f32],
    /// The value the effect plays at a phase (0 to 1), smoothing included.
    curve: &'a dyn Fn(f32) -> f32,
    playhead: Option<f32>,
    size: egui::Vec2,
    color: Color32,
    /// Beat lines across the cycle, if it spans several.
    divisions: usize,
}

/// A shape as a function of the phase, 0 to 1 both ways.
type ShapeFn = fn(f32) -> f32;

/// Ready-made shapes, by name.
pub const PRESETS: &[(&str, ShapeFn)] = &[
    ("Pump", |t| (t * 4.0).min(1.0).powf(0.6)),
    ("Sine", |t| 0.5 - 0.5 * (std::f32::consts::TAU * t).cos()),
    ("Triangle", |t| 1.0 - (2.0 * t - 1.0).abs()),
    ("Saw down", |t| 1.0 - t),
    ("Saw up", |t| t),
    ("Square", |t| if t < 0.5 { 1.0 } else { 0.0 }),
    (
        "Gate 1/8",
        |t| if (t * 8.0).fract() < 0.5 { 1.0 } else { 0.0 },
    ),
    ("Flat", |_| 1.0),
];

impl<'a> ShapeEditor<'a> {
    pub fn new(id: Id, points: &'a [f32], curve: &'a dyn Fn(f32) -> f32) -> Self {
        ShapeEditor {
            id,
            points,
            curve,
            playhead: None,
            size: vec2(300.0, 100.0),
            color: Color32::LIGHT_BLUE,
            divisions: 4,
        }
    }

    pub fn size(mut self, size: egui::Vec2) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Color32) -> Self {
        self.color = color;
        self
    }

    pub fn playhead(mut self, phase: Option<f32>) -> Self {
        self.playhead = phase;
        self
    }

    pub fn divisions(mut self, divisions: usize) -> Self {
        self.divisions = divisions.max(1);
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> ShapeResponse {
        let mut out = ShapeResponse::default();
        let (rect, response) = ui.allocate_exact_size(self.size, Sense::click_and_drag());
        #[cfg(test)]
        ui.ctx()
            .data_mut(|d| d.insert_temp(self.id.with("rect"), rect));
        let area = rect.shrink(6.0);
        let visuals = ui.visuals().clone();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
        painter.rect_stroke(
            rect,
            4.0,
            visuals.widgets.noninteractive.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let n = self.points.len().max(1);
        let x_of = |t: f32| area.left() + t * area.width();
        let y_of = |v: f32| area.bottom() - v.clamp(0.0, 1.0) * area.height();
        let grid = visuals.weak_text_color().gamma_multiply(0.15);
        for i in 1..self.divisions {
            painter.vline(
                x_of(i as f32 / self.divisions as f32),
                area.y_range(),
                Stroke::new(1.0, grid.gamma_multiply(2.0)),
            );
        }
        painter.hline(area.x_range(), y_of(0.5), Stroke::new(1.0, grid));

        // The curve as played, filled underneath.
        let steps = area.width().max(2.0) as usize;
        let line: Vec<Pos2> = (0..=steps)
            .map(|i| {
                let t = i as f32 / steps as f32;
                pos2(x_of(t), y_of((self.curve)(t.min(0.9999))))
            })
            .collect();
        let mut fill = egui::Mesh::default();
        for pair in line.windows(2) {
            fill.add_colored_rect(
                Rect::from_min_max(pair[0], pos2(pair[1].x, area.bottom())),
                self.color.gamma_multiply(0.18),
            );
        }
        painter.add(fill);
        painter.add(Shape::line(line, Stroke::new(1.8, self.color)));
        for (i, value) in self.points.iter().enumerate() {
            let at = pos2(x_of(i as f32 / n as f32), y_of(*value));
            painter.circle_filled(at, 2.0, self.color.gamma_multiply(0.8));
        }
        if let Some(phase) = self.playhead {
            let x = x_of(phase.clamp(0.0, 1.0));
            painter.vline(
                x,
                area.y_range(),
                Stroke::new(1.0, visuals.strong_text_color()),
            );
            painter.circle_filled(
                pos2(x, y_of((self.curve)(phase.clamp(0.0, 0.9999)))),
                3.5,
                visuals.strong_text_color(),
            );
        }

        // Drawing: every point the pointer passed over since last frame
        // takes the height along the way.
        let last_id = self.id.with("last");
        let point_at = |pos: Pos2| {
            let t = ((pos.x - area.left()) / area.width()).clamp(0.0, 0.9999);
            let value = ((area.bottom() - pos.y) / area.height()).clamp(0.0, 1.0);
            ((t * n as f32) as usize, value)
        };
        if (response.dragged() || response.clicked())
            && let Some(pos) = response.interact_pointer_pos()
        {
            let (to, to_value) = point_at(pos);
            let (from, from_value) = ui
                .data(|d| d.get_temp::<(usize, f32)>(last_id))
                .filter(|_| !response.drag_started() && !response.clicked())
                .unwrap_or((to, to_value));
            for i in from.min(to)..=from.max(to) {
                let t = if from == to {
                    1.0
                } else {
                    (i as f32 - from as f32) / (to as f32 - from as f32)
                };
                let value = from_value + (to_value - from_value) * t;
                if self.points.get(i) != Some(&value) {
                    out.changes.push((i, value));
                }
            }
            ui.data_mut(|d| d.insert_temp(last_id, (to, to_value)));
        }
        if response.drag_stopped() || response.clicked() {
            out.finished = true;
        }
        response
            .on_hover_text("Drag to draw; right-click for shapes")
            .context_menu(|ui| {
                for (name, shape) in PRESETS {
                    if ui.button(*name).clicked() {
                        for i in 0..n {
                            out.changes.push((i, shape(i as f32 / n as f32)));
                        }
                        out.finished = true;
                        ui.close();
                    }
                }
            });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_stay_in_range() {
        for (name, shape) in PRESETS {
            for i in 0..64 {
                let v = shape(i as f32 / 64.0);
                assert!((0.0..=1.0).contains(&v), "{name} at {i}: {v}");
            }
        }
    }
}
