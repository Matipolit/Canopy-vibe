//! An envelope with delay and hold, drawn as its actual shape and edited by
//! dragging it, with a playhead where a voice is in it now.
//!
//! The six stages read left to right: delay (flat at zero), attack, hold
//! (flat at the top), decay, sustain (a plateau of no set length) and
//! release. Timed stages are as wide as their knob is turned, so the
//! shape shows the proportions a knob would.
//!
//! - The attack peak, the hold's end, the decay corner (also up and down,
//!   for the sustain) and the release end drag sideways for their times;
//!   the delay's end drags for the delay.
//! - The dot in the middle of each sloped stage drags up or down to bend it.
//! - Double-click a handle to reset what it controls.

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Id, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    pos2, vec2,
};
use instruments::{EnvelopeParams, ParamDescriptor};

const HANDLE_RADIUS: f32 = 4.0;
const PAD: f32 = 7.0;
/// Each stage's share of the width at its longest: delay and hold are
/// usually short, so they get less.
const DELAY_SHARE: f32 = 0.1;
const STAGE_SHARE: f32 = 0.22;
const HOLD_SHARE: f32 = 0.1;
const SUSTAIN_SHARE: f32 = 0.12;
/// Pixels of vertical drag per unit of curve.
const CURVE_DRAG: f32 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    Delay,
    Attack,
    Hold,
    Decay,
    Release,
    AttackCurve,
    DecayCurve,
    ReleaseCurve,
}

/// What changed this frame, for the caller to apply.
#[derive(Debug, Default)]
pub struct DahdsrResponse {
    /// `(parameter index, new value)` pairs.
    pub changes: Vec<(usize, f32)>,
    /// A drag ended or a reset happened: the change is done.
    pub finished: bool,
    /// The editor as a whole, for a context menu.
    pub response: Option<egui::Response>,
}

pub struct DahdsrEditor<'a> {
    id: Id,
    layout: EnvelopeParams,
    /// The delay's and hold's parameters.
    delay: usize,
    hold: usize,
    params: &'a [ParamDescriptor],
    value: &'a dyn Fn(usize) -> f32,
    color: Color32,
    size: egui::Vec2,
    /// Where a voice is: its stage number and progress (as the Wavetable's
    /// `METER_ENV_STAGE` holds it) and its level.
    playhead: Option<(f32, f32)>,
}

/// The value a drag started from, so it follows the pointer's movement.
#[derive(Debug, Clone, Copy)]
struct DragStart {
    handle: Handle,
    value: f32,
}

impl<'a> DahdsrEditor<'a> {
    pub fn new(
        id: Id,
        layout: EnvelopeParams,
        delay: usize,
        hold: usize,
        params: &'a [ParamDescriptor],
        value: &'a dyn Fn(usize) -> f32,
    ) -> Self {
        DahdsrEditor {
            id,
            layout,
            delay,
            hold,
            params,
            value,
            color: Color32::from_rgb(120, 190, 255),
            size: vec2(300.0, 110.0),
            playhead: None,
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

    /// Where a voice is in the envelope: `(stage + progress, level)`, the
    /// stages numbered 1 (delay) to 6 (release) as the synth reports them.
    pub fn playhead(mut self, playhead: Option<(f32, f32)>) -> Self {
        self.playhead = playhead;
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

    fn curve(&self, stage: usize) -> f32 {
        self.layout.curves.map_or(0.0, |c| self.get(c[stage]))
    }

    /// The parameter a handle moves, and whether it moves its curve.
    fn param_of(&self, handle: Handle) -> Option<usize> {
        let curves = self.layout.curves;
        Some(match handle {
            Handle::Delay => self.delay,
            Handle::Attack => self.layout.attack,
            Handle::Hold => self.hold,
            Handle::Decay => self.layout.decay,
            Handle::Release => self.layout.release,
            Handle::AttackCurve => curves?[0],
            Handle::DecayCurve => curves?[1],
            Handle::ReleaseCurve => curves?[2],
        })
    }

    pub fn show(self, ui: &mut egui::Ui) -> DahdsrResponse {
        let mut out = DahdsrResponse::default();
        let (rect, editor) = ui.allocate_exact_size(self.size, Sense::click());
        #[cfg(test)]
        ui.ctx()
            .data_mut(|d| d.insert_temp(self.id.with("rect"), rect));
        out.response = Some(editor);
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
            FontId::proportional(10.0),
            visuals.weak_text_color(),
        );

        let area = rect.shrink(PAD);
        let span = |share: f32, n: f32| area.width() * share * n;
        let timed = |n: f32| span(STAGE_SHARE, 0.04 + 0.96 * n);
        let y_of = |level: f32| area.bottom() - level.clamp(0.0, 1.0) * area.height();
        let sustain = self.get(self.layout.sustain);
        let start = pos2(area.left(), area.bottom());
        let delayed = pos2(
            start.x + span(DELAY_SHARE, self.normalized(self.delay)),
            start.y,
        );
        let peak = pos2(
            delayed.x + timed(self.normalized(self.layout.attack)),
            area.top(),
        );
        let held = pos2(
            peak.x + span(HOLD_SHARE, self.normalized(self.hold)),
            area.top(),
        );
        let corner = pos2(
            held.x + timed(self.normalized(self.layout.decay)),
            y_of(sustain),
        );
        let released = pos2(corner.x + area.width() * SUSTAIN_SHARE, corner.y);
        let end = pos2(
            released.x + timed(self.normalized(self.layout.release)),
            area.bottom(),
        );

        // The shape, sampled with the same curve math the envelope plays.
        let (attack_curve, decay_curve, release_curve) =
            (self.curve(0), self.curve(1), self.curve(2));
        let samples = 24;
        let mut line = vec![start, delayed];
        for i in 1..=samples {
            let t = i as f32 / samples as f32;
            line.push(pos2(
                lerp(delayed.x, peak.x, t),
                y_of(dsp::stage_shape(t, attack_curve)),
            ));
        }
        line.push(held);
        for i in 1..=samples {
            let t = i as f32 / samples as f32;
            let level = 1.0 + (sustain - 1.0) * dsp::stage_shape(t, decay_curve);
            line.push(pos2(lerp(held.x, corner.x, t), y_of(level)));
        }
        line.push(released);
        for i in 1..=samples {
            let t = i as f32 / samples as f32;
            let level = sustain * (1.0 - dsp::stage_shape(t, release_curve));
            line.push(pos2(lerp(released.x, end.x, t), y_of(level)));
        }
        for pair in line.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            painter.add(Shape::convex_polygon(
                vec![a, b, pos2(b.x, area.bottom()), pos2(a.x, area.bottom())],
                self.color.gamma_multiply(0.16),
                Stroke::NONE,
            ));
        }
        painter.add(Shape::line(line, Stroke::new(1.8, self.color)));
        let faint = Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.25));
        for x in [delayed.x, held.x, released.x] {
            painter.vline(x, area.y_range(), faint);
        }

        // Where a voice is now.
        if let Some((stage, level)) = self.playhead {
            let index = stage.floor() as i32;
            let t = stage - stage.floor();
            let x = match index {
                1 => lerp(start.x, delayed.x, t),
                2 => lerp(delayed.x, peak.x, t),
                3 => lerp(peak.x, held.x, t),
                4 => lerp(held.x, corner.x, t),
                5 => lerp(corner.x, released.x, 0.5),
                6 => lerp(released.x, end.x, t),
                _ => f32::NAN,
            };
            if x.is_finite() {
                let at = pos2(x, y_of(level));
                painter.vline(
                    x,
                    area.y_range(),
                    Stroke::new(1.0, visuals.strong_text_color().gamma_multiply(0.4)),
                );
                painter.circle_filled(at, 4.0, self.color.gamma_multiply(0.4));
                painter.circle_filled(at, 2.5, visuals.strong_text_color());
            }
        }

        let mid = |a: Pos2, b: Pos2, from: f32, to: f32, curve: f32| {
            pos2(
                lerp(a.x, b.x, 0.5),
                y_of(from + (to - from) * dsp::stage_shape(0.5, curve)),
            )
        };
        let mut handles = vec![
            (Handle::Delay, delayed),
            (Handle::Attack, peak),
            (Handle::Hold, held),
            (Handle::Decay, corner),
            (Handle::Release, end),
        ];
        if self.layout.curves.is_some() {
            handles.push((
                Handle::AttackCurve,
                mid(delayed, peak, 0.0, 1.0, attack_curve),
            ));
            handles.push((
                Handle::DecayCurve,
                mid(held, corner, 1.0, sustain, decay_curve),
            ));
            handles.push((
                Handle::ReleaseCurve,
                mid(released, end, sustain, 0.0, release_curve),
            ));
        }
        let drag_id = self.id.with("drag start");
        for (handle, pos) in handles {
            let hit = Rect::from_center_size(pos, vec2(14.0, 14.0));
            let response = ui.interact(hit, self.id.with(handle as u8), Sense::click_and_drag());
            let Some(index) = self.param_of(handle) else {
                continue;
            };
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
            if response.drag_started() {
                let value = if curve_handle {
                    self.get(index)
                } else {
                    self.normalized(index)
                };
                ui.data_mut(|d| d.insert_temp(drag_id, DragStart { handle, value }));
            }
            if response.dragged()
                && let Some(start) = ui.data(|d| d.get_temp::<DragStart>(drag_id))
                && start.handle == handle
            {
                let total = response.total_drag_delta().unwrap_or_default();
                let param = &self.params[index];
                if curve_handle {
                    // Down bends a rising stage to start slow and a falling
                    // one to start fast: the line follows the dot, as in the
                    // shared envelope editor.
                    let rising = handle == Handle::AttackCurve;
                    let sign = if rising { 1.0 } else { -1.0 };
                    let curve = (start.value + sign * total.y / CURVE_DRAG).clamp(-1.0, 1.0);
                    out.changes.push((index, curve));
                } else {
                    let share = match handle {
                        Handle::Delay => DELAY_SHARE,
                        Handle::Hold => HOLD_SHARE,
                        _ => STAGE_SHARE * 0.96,
                    };
                    let normalized = start.value + total.x / (area.width() * share);
                    out.changes
                        .push((index, param.from_normalized(normalized.clamp(0.0, 1.0))));
                    if handle == Handle::Decay {
                        let sustain = self.layout.sustain;
                        let level = ((area.bottom()
                            - response.interact_pointer_pos().map_or(corner.y, |p| p.y))
                            / area.height())
                        .clamp(0.0, 1.0);
                        out.changes.push((sustain, level));
                    }
                }
            }
            // What the drag sets, beside the handle.
            if response.dragged()
                && let Some(&(changed, value)) = out.changes.first()
            {
                let text = format!(
                    "{} {}",
                    self.params[changed].name,
                    self.params[changed].format(value)
                );
                painter.text(
                    pos + vec2(8.0, -8.0),
                    if pos.x > rect.right() - 100.0 {
                        Align2::RIGHT_BOTTOM
                    } else {
                        Align2::LEFT_BOTTOM
                    },
                    text,
                    FontId::proportional(11.0),
                    visuals.strong_text_color(),
                );
            }
            if response.drag_stopped() {
                ui.data_mut(|d| d.remove::<DragStart>(drag_id));
                out.finished = true;
            }
            if response.double_clicked() {
                out.changes
                    .push((index, self.params[index].default_value()));
                if handle == Handle::Decay {
                    let sustain = self.layout.sustain;
                    out.changes
                        .push((sustain, self.params[sustain].default_value()));
                }
                out.finished = true;
            }
            let color = if active {
                visuals.strong_text_color()
            } else {
                self.color
            };
            if curve_handle {
                painter.circle_stroke(pos, 3.0, Stroke::new(1.3, color));
            } else {
                painter.circle(
                    pos,
                    HANDLE_RADIUS,
                    color,
                    Stroke::new(1.0, visuals.extreme_bg_color),
                );
            }
        }
        out
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
