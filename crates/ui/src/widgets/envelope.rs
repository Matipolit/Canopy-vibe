//! An ADSR envelope drawn as its actual shape, edited by dragging it.
//!
//! Works for any instrument's envelope: it only needs the parameter
//! descriptors and an [`EnvelopeParams`] saying which ones make the envelope.
//!
//! - The attack peak drags sideways (attack time).
//! - The decay corner drags sideways (decay time) and up/down (sustain).
//! - The release end drags sideways (release time).
//! - The dot in the middle of each sloped stage drags up/down to bend it.
//! - Double-click any handle to reset what it controls.

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Id, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    pos2, vec2,
};
use instruments::{EnvelopeParams, ParamDescriptor};

const HANDLE_RADIUS: f32 = 4.5;
/// Each timed stage takes up to this share of the width at its longest.
pub(crate) const STAGE_SHARE: f32 = 0.29;
/// The sustain plateau's share of the width, which has no time of its own.
const SUSTAIN_SHARE: f32 = 0.13;
/// Pixels of vertical drag per unit of curve.
const CURVE_DRAG: f32 = 60.0;
pub(crate) const PAD: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    Attack,
    Decay,
    Release,
    AttackCurve,
    DecayCurve,
    ReleaseCurve,
}

/// What changed this frame, for the caller to apply.
#[derive(Debug, Default)]
pub struct EnvelopeResponse {
    /// `(parameter index, new value)` pairs.
    pub changes: Vec<(usize, f32)>,
    /// A drag ended (or a double-click reset happened): the change is done.
    pub finished: bool,
    /// The editor as a whole, for a context menu.
    pub response: Option<egui::Response>,
}

pub struct EnvelopeEditor<'a> {
    layout: EnvelopeParams,
    params: &'a [ParamDescriptor],
    value: &'a dyn Fn(usize) -> f32,
    color: Color32,
    id: Id,
    size: egui::Vec2,
}

/// The state a drag started from, so a stage being bent follows the
/// pointer's movement rather than jumping to it.
#[derive(Debug, Clone, Copy)]
struct DragStart {
    handle: Handle,
    curve: f32,
}

impl<'a> EnvelopeEditor<'a> {
    pub fn new(
        id: Id,
        layout: EnvelopeParams,
        params: &'a [ParamDescriptor],
        value: &'a dyn Fn(usize) -> f32,
    ) -> Self {
        EnvelopeEditor {
            layout,
            params,
            value,
            color: Color32::from_rgb(120, 190, 255),
            id,
            size: vec2(300.0, 110.0),
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

    fn get(&self, index: usize) -> f32 {
        self.params
            .get(index)
            .map_or(0.0, |p| p.clamp((self.value)(index)))
    }

    fn normalized(&self, index: usize) -> f32 {
        self.params
            .get(index)
            .map_or(0.0, |p| p.to_normalized(self.get(index)))
    }

    fn value_at(&self, index: usize, normalized: f32) -> f32 {
        self.params
            .get(index)
            .map_or(0.0, |p| p.from_normalized(normalized))
    }

    fn curve(&self, stage: usize) -> f32 {
        self.layout.curves.map_or(0.0, |c| self.get(c[stage]))
    }

    pub fn show(self, ui: &mut egui::Ui) -> EnvelopeResponse {
        let mut out = EnvelopeResponse::default();
        let (rect, editor) = ui.allocate_exact_size(self.size, Sense::click());
        out.response = Some(editor);
        #[cfg(test)]
        ui.ctx()
            .data_mut(|d| d.insert_temp(self.id.with("rect"), rect));
        let visuals = ui.visuals().clone();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
        painter.rect_stroke(
            rect,
            4.0,
            visuals.widgets.noninteractive.bg_stroke,
            StrokeKind::Inside,
        );
        painter.text(
            rect.right_top() + vec2(-6.0, 4.0),
            Align2::RIGHT_TOP,
            self.layout.name,
            FontId::proportional(crate::theme::text::SMALL),
            visuals.weak_text_color(),
        );

        let area = rect.shrink(PAD);
        let width_of = |n: f32| area.width() * STAGE_SHARE * (0.04 + 0.96 * n);
        let y_of = |level: f32| area.bottom() - level.clamp(0.0, 1.0) * area.height();
        let sustain = self.get(self.layout.sustain);
        let start = pos2(area.left(), area.bottom());
        let peak = pos2(
            start.x + width_of(self.normalized(self.layout.attack)),
            area.top(),
        );
        let corner = pos2(
            peak.x + width_of(self.normalized(self.layout.decay)),
            y_of(sustain),
        );
        let held = pos2(corner.x + area.width() * SUSTAIN_SHARE, corner.y);
        let end = pos2(
            held.x + width_of(self.normalized(self.layout.release)),
            area.bottom(),
        );

        // The shape, sampled with the same curve math the envelope plays.
        let (attack_curve, decay_curve, release_curve) =
            (self.curve(0), self.curve(1), self.curve(2));
        let mut line = Vec::with_capacity(80);
        let samples = 24;
        for i in 0..=samples {
            let t = i as f32 / samples as f32;
            line.push(pos2(
                lerp(start.x, peak.x, t),
                y_of(dsp::stage_shape(t, attack_curve)),
            ));
        }
        for i in 1..=samples {
            let t = i as f32 / samples as f32;
            let level = 1.0 + (sustain - 1.0) * dsp::stage_shape(t, decay_curve);
            line.push(pos2(lerp(peak.x, corner.x, t), y_of(level)));
        }
        line.push(held);
        for i in 1..=samples {
            let t = i as f32 / samples as f32;
            let level = sustain * (1.0 - dsp::stage_shape(t, release_curve));
            line.push(pos2(lerp(held.x, end.x, t), y_of(level)));
        }
        let mut fill = line.clone();
        fill.push(pos2(end.x, area.bottom()));
        fill.push(start);
        for pair in fill.windows(2) {
            // Filled as thin quads: the outline isn't convex.
            let (a, b) = (pair[0], pair[1]);
            painter.add(Shape::convex_polygon(
                vec![a, b, pos2(b.x, area.bottom()), pos2(a.x, area.bottom())],
                self.color.gamma_multiply(0.18),
                Stroke::NONE,
            ));
        }
        painter.add(Shape::line(line, Stroke::new(1.8, self.color)));
        painter.vline(
            held.x,
            area.y_range(),
            Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.3)),
        );

        let mid = |a: Pos2, b: Pos2, from: f32, to: f32, curve: f32| {
            pos2(
                lerp(a.x, b.x, 0.5),
                y_of(from + (to - from) * dsp::stage_shape(0.5, curve)),
            )
        };
        let mut handles = vec![
            (Handle::Attack, peak),
            (Handle::Decay, corner),
            (Handle::Release, end),
        ];
        if self.layout.curves.is_some() {
            handles.push((
                Handle::AttackCurve,
                mid(start, peak, 0.0, 1.0, attack_curve),
            ));
            handles.push((
                Handle::DecayCurve,
                mid(peak, corner, 1.0, sustain, decay_curve),
            ));
            handles.push((
                Handle::ReleaseCurve,
                mid(held, end, sustain, 0.0, release_curve),
            ));
        }

        for (handle, pos) in handles {
            let hit = Rect::from_center_size(pos, vec2(14.0, 14.0));
            let response = ui.interact(hit, self.id.with(handle as u8), Sense::click_and_drag());
            let curve_handle = matches!(
                handle,
                Handle::AttackCurve | Handle::DecayCurve | Handle::ReleaseCurve
            );
            let active = response.hovered() || response.dragged();
            if active {
                ui.ctx().set_cursor_icon(match handle {
                    Handle::Decay => CursorIcon::Move,
                    _ if curve_handle => CursorIcon::ResizeVertical,
                    _ => CursorIcon::ResizeHorizontal,
                });
            }
            if curve_handle {
                painter.circle(
                    pos,
                    HANDLE_RADIUS - 1.0,
                    visuals.extreme_bg_color,
                    Stroke::new(1.5, self.color),
                );
            } else {
                painter.circle_filled(
                    pos,
                    if active {
                        HANDLE_RADIUS + 1.5
                    } else {
                        HANDLE_RADIUS
                    },
                    self.color,
                );
            }

            if response.drag_started() {
                let curve = match handle {
                    Handle::AttackCurve => attack_curve,
                    Handle::DecayCurve => decay_curve,
                    Handle::ReleaseCurve => release_curve,
                    _ => 0.0,
                };
                ui.ctx()
                    .data_mut(|d| d.insert_temp(self.id, DragStart { handle, curve }));
            }
            if response.dragged()
                && let Some(pointer) = response.interact_pointer_pos()
            {
                let start_state: Option<DragStart> = ui.ctx().data(|d| d.get_temp(self.id));
                let total = response.total_drag_delta().unwrap_or_default();
                let to_normalized =
                    |w: f32| ((w / (area.width() * STAGE_SHARE) - 0.04) / 0.96).clamp(0.0, 1.0);
                let layout = self.layout;
                let curves = layout.curves.unwrap_or([0; 3]);
                match handle {
                    Handle::Attack => out.changes.push((
                        layout.attack,
                        self.value_at(layout.attack, to_normalized(pointer.x - start.x)),
                    )),
                    Handle::Decay => {
                        out.changes.push((
                            layout.decay,
                            self.value_at(layout.decay, to_normalized(pointer.x - peak.x)),
                        ));
                        let level = ((area.bottom() - pointer.y) / area.height()).clamp(0.0, 1.0);
                        out.changes
                            .push((layout.sustain, self.value_at(layout.sustain, level)));
                    }
                    Handle::Release => out.changes.push((
                        layout.release,
                        self.value_at(layout.release, to_normalized(pointer.x - held.x)),
                    )),
                    curve => {
                        let Some(start_state) = start_state.filter(|s| s.handle == handle) else {
                            continue;
                        };
                        // Down bends a rising stage to start slow and a
                        // falling one to start fast: the line follows the dot.
                        let (index, sign) = match curve {
                            Handle::AttackCurve => (curves[0], 1.0),
                            Handle::DecayCurve => (curves[1], -1.0),
                            _ => (curves[2], -1.0),
                        };
                        let value =
                            (start_state.curve + sign * total.y / CURVE_DRAG).clamp(-1.0, 1.0);
                        out.changes.push((index, value));
                    }
                }
                let text = self.describe(handle, &out.changes);
                painter.text(
                    pos + vec2(8.0, -8.0),
                    if pos.x > rect.right() - 90.0 {
                        Align2::RIGHT_BOTTOM
                    } else {
                        Align2::LEFT_BOTTOM
                    },
                    text,
                    FontId::proportional(crate::theme::text::SMALL),
                    visuals.strong_text_color(),
                );
            }
            if response.drag_stopped() {
                out.finished = true;
            }
            if response.double_clicked() {
                for index in self.reset_indices(handle) {
                    if let Some(param) = self.params.get(index) {
                        out.changes.push((index, param.default_value()));
                    }
                }
                out.finished = true;
            }
        }
        out
    }

    /// The values a handle is changing, as shown while dragging.
    fn describe(&self, handle: Handle, changes: &[(usize, f32)]) -> String {
        let format = |index: usize| {
            let value = changes
                .iter()
                .rev()
                .find(|(i, _)| *i == index)
                .map_or(self.get(index), |(_, v)| *v);
            self.params
                .get(index)
                .map_or_else(String::new, |p| p.format(value))
        };
        let layout = self.layout;
        match handle {
            Handle::Attack => format(layout.attack),
            Handle::Decay => format!("{} · {}", format(layout.decay), format(layout.sustain)),
            Handle::Release => format(layout.release),
            _ => {
                let curves = layout.curves.unwrap_or([0; 3]);
                let index = match handle {
                    Handle::AttackCurve => curves[0],
                    Handle::DecayCurve => curves[1],
                    _ => curves[2],
                };
                format(index)
            }
        }
    }

    fn reset_indices(&self, handle: Handle) -> Vec<usize> {
        let layout = self.layout;
        let curves = layout.curves.unwrap_or([usize::MAX; 3]);
        match handle {
            Handle::Attack => vec![layout.attack],
            Handle::Decay => vec![layout.decay, layout.sustain],
            Handle::Release => vec![layout.release],
            Handle::AttackCurve => vec![curves[0]],
            Handle::DecayCurve => vec![curves[1]],
            Handle::ReleaseCurve => vec![curves[2]],
        }
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
