//! A rotary knob for one parameter, as in FL Studio: drag up (or right) to
//! turn it up, with Shift or Ctrl held for fine moves; the mouse wheel steps
//! it. Its name sits above it and its value below. Typing a value on
//! double-click, resetting on Ctrl+click and the right-click menu are the
//! caller's (see [`crate::devices::Panel::attach`]), which knows what the
//! knob controls.
//!
//! With the keyboard's focus (Tab) it draws a round ring of its own rather
//! than the app's rectangle, and the arrow keys turn it a wheel notch a
//! press: Up and Right up, Down and Left down, finer with Shift (or Ctrl);
//! Home and End go to either end. A burst of presses (a key held down,
//! repeating) is one gesture: its changes reach the caller as they happen,
//! and the response reads as changed once the keys are let go, so the
//! caller commits it as one undo step, as it does a wheel step.
//!
//! A modulated knob can show each modulation as a ring around it
//! ([`Knob::rings`]), as in Serum and Vital: an arc from the value as far as
//! the modulation reaches (both ways for a bipolar one), in its source's
//! color. Dragging starting on the rings (or with Alt held) changes a
//! ring's depth instead of the value, and right-clicking there names the
//! ring for the caller's menu; the caller applies both
//! ([`KnobResponse::ring_changed`], [`KnobResponse::ring_menu`]).

use std::f32::consts::PI;

use eframe::egui::{
    self, Color32, EventFilter, FontId, Id, Key, Pos2, Rect, Response, Sense, Shape, Stroke, pos2,
    vec2,
};
use instruments::{ParamDescriptor, ParameterKind};

use crate::theme::{self, space, stroke, text};
use crate::widgets::chrome;

/// Where the arc starts (bottom left) and how far it sweeps (to the bottom
/// right), in screen angles (clockwise from the right).
const START: f32 = 0.75 * PI;
const SWEEP: f32 = 1.5 * PI;
/// Pixels of drag for the whole range.
const DRAG_RANGE: f32 = 220.0;
const FINE: f32 = 0.1;
/// How far one wheel notch (or arrow key press) turns a continuous knob.
const WHEEL_STEP: f32 = 0.02;
/// The keys that turn a knob with the keyboard's focus.
const KEYS: [Key; 6] = [
    Key::ArrowUp,
    Key::ArrowRight,
    Key::ArrowDown,
    Key::ArrowLeft,
    Key::Home,
    Key::End,
];

/// The name's row over the circle: the text, and the gap under it.
pub const LABEL_HEIGHT: f32 = text::SMALL + space::S;
/// The value's row under the circle, with the gap over it.
pub const VALUE_HEIGHT: f32 = text::SMALL + space::S + space::XS;

/// The track the value fills, and the line pointing at it.
const TRACK_WIDTH: f32 = 3.0;
const POINTER_WIDTH: f32 = 2.0;
/// The modulation rings' width.
const RING_WIDTH: f32 = 2.0;
/// How far outside the circle the drop highlight and the focus ring sit.
const HALO: f32 = 3.0;

/// How far outside the knob's track the first modulation ring sits, and
/// how much further each next one does.
const RING_GAP: f32 = 2.0;
const RING_STEP: f32 = 2.6;
/// Rings past this many share the outermost circle.
const RING_CIRCLES: usize = 3;

pub struct Knob<'a> {
    id: Id,
    param: &'a ParamDescriptor,
    value: f32,
    label: &'a str,
    diameter: f32,
    label_color: Option<Color32>,
    modulated: Option<f32>,
    rings: &'a [ModRing],
    drop_highlight: bool,
    text: Option<&'a str>,
    compact: bool,
}

/// One modulation of a knob's parameter, drawn as a ring (see [`Knob::rings`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModRing {
    /// How far it moves the value, in the knob's normalized units (`0..=1`
    /// is the whole range), signed.
    pub amount: f32,
    /// Swings both ways around the value rather than one.
    pub bipolar: bool,
    pub color: Color32,
}

pub struct KnobResponse {
    pub response: Response,
    /// The new value, when the user moved it.
    pub changed: Option<f32>,
    /// A ring dragged: its index in [`Knob::rings`] and its new amount
    /// (normalized, signed, `-1..=1`).
    pub ring_changed: Option<(usize, f32)>,
    /// A ring right-clicked: its index, for the caller's menu of it.
    pub ring_menu: Option<usize>,
}

impl<'a> Knob<'a> {
    pub fn new(id: Id, param: &'a ParamDescriptor, value: f32) -> Self {
        Knob {
            id,
            param,
            value,
            label: param.name,
            diameter: 34.0,
            label_color: None,
            modulated: None,
            rings: &[],
            drop_highlight: false,
            text: None,
            compact: false,
        }
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    pub fn diameter(mut self, diameter: f32) -> Self {
        self.diameter = diameter;
        self
    }

    /// The name's color (a track's color when the knob is automated).
    pub fn label_color(mut self, color: Option<Color32>) -> Self {
        self.label_color = color;
        self
    }

    /// Where something else (an LFO) has the value right now, from 0 to 1:
    /// drawn as a dot on the rim.
    pub fn modulated(mut self, normalized: Option<f32>) -> Self {
        self.modulated = normalized;
        self
    }

    /// [`Knob::modulated`]: the live value, normalized, as a dot on the rim.
    pub fn live(self, normalized: Option<f32>) -> Self {
        self.modulated(normalized)
    }

    /// The modulations moving this knob's parameter, drawn as rings
    /// around it, innermost first, and draggable (see the module docs).
    pub fn rings(mut self, rings: &'a [ModRing]) -> Self {
        self.rings = rings;
        self
    }

    /// Lights the knob up as a place to drop something (a modulation
    /// source being dragged over it).
    pub fn drop_highlight(mut self, highlight: bool) -> Self {
        self.drop_highlight = highlight;
        self
    }

    /// How the device itself shows the value (a plugin's own text, like
    /// "-6.0 dB" or "Saw"), in place of the number and unit.
    pub fn text(mut self, text: Option<&'a str>) -> Self {
        self.text = text;
        self
    }

    /// The small form for dense panels: no row for the value under the
    /// circle, which shows in the name's place instead while the knob is
    /// hovered, turned or focused (and in its tooltip), and only as wide as
    /// the circle needs.
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// What the knob is called: its label, or its parameter's name when it
    /// shows none (something beside it names it, as a macro's tile does).
    fn name(&self) -> &str {
        if self.label.is_empty() {
            self.param.name
        } else {
            self.label
        }
    }

    /// The value as the knob writes it: the device's own text, or the
    /// number and unit.
    fn value_text(&self, value: f32) -> String {
        match self.text {
            Some(text) => text.to_owned(),
            None => format_value(self.param, value),
        }
    }

    pub fn show(self, ui: &mut egui::Ui) -> KnobResponse {
        let size = if self.compact {
            vec2(compact_width(self.diameter), compact_height(self.diameter))
        } else {
            vec2(width(self.diameter), height(self.diameter))
        };
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let response = response.on_hover_cursor(egui::CursorIcon::ResizeVertical);
        let (ring_changed, ring_menu) = self.interact_rings(ui, rect, &response);
        let ringing = ring_changed.is_some() || self.ring_drag(ui).is_some();
        let changed = if ringing {
            None
        } else {
            self.interact(ui, &response)
        };
        // The keyboard's turns: shown at once, but committed (marked as
        // changed) only once the keys are let go.
        let (keyed, burst_over) = if ringing || changed.is_some() {
            (None, false)
        } else {
            self.keys(ui, &response)
        };
        let shown = changed.or(keyed).unwrap_or(self.value);
        if ui.is_rect_visible(rect) {
            self.paint(ui, rect, &response, shown, ring_changed);
        }
        let mut response = response;
        if changed.is_some() || burst_over {
            response.mark_changed();
        }
        let changed = changed.or(keyed);
        if self.compact {
            let text = format!("{}: {}", self.name(), self.value_text(shown));
            response = response.on_hover_text(text);
        }
        // Screen readers (and tests) see a slider with the knob's name.
        let label = self.name().to_owned();
        response.widget_info(|| {
            egui::WidgetInfo::slider(ui.is_enabled(), f64::from(shown), label.clone())
        });
        KnobResponse {
            response,
            changed,
            ring_changed,
            ring_menu,
        }
    }

    /// Where the knob's circle is within `rect`, and its radius.
    fn circle(&self, rect: Rect) -> (Pos2, f32) {
        let radius = self.diameter / 2.0;
        (
            pos2(rect.center().x, rect.top() + LABEL_HEIGHT + radius),
            radius,
        )
    }

    /// The arrow keys, Home and End, while the knob has the keyboard's
    /// focus: the value they turn it to, and whether a burst of them has
    /// just ended (its keys let go), which is when it's committed.
    fn keys(&self, ui: &mut egui::Ui, response: &Response) -> (Option<f32>, bool) {
        let burst_id = self.id.with("key burst");
        let mut value = None;
        if response.has_focus() {
            // The arrows turn the knob rather than move the focus on, and
            // the app leaves them (and Home and End) to it.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    },
                );
            });
            ui.data_mut(|d| d.insert_temp(keys_id(), response.id));
            value = self.key_presses(ui);
            if value.is_some() {
                ui.data_mut(|d| d.insert_temp(burst_id, true));
            }
        }
        let held = ui.input(|i| KEYS.iter().any(|key| i.key_down(*key)));
        let pending = ui.data(|d| d.get_temp::<bool>(burst_id)).unwrap_or(false);
        let over = pending && !held;
        if over {
            ui.data_mut(|d| d.remove::<bool>(burst_id));
        }
        (value.filter(|v| *v != self.value), over)
    }

    /// Where this frame's presses of [`KEYS`] take the value, if any were
    /// pressed.
    fn key_presses(&self, ui: &egui::Ui) -> Option<f32> {
        let param = self.param;
        let (steps, home, end, fine) = ui.input(|i| {
            let presses = |key| i.num_presses(key) as f32;
            (
                presses(Key::ArrowUp) + presses(Key::ArrowRight)
                    - presses(Key::ArrowDown)
                    - presses(Key::ArrowLeft),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.modifiers.shift || i.modifiers.command || i.modifiers.ctrl,
            )
        });
        let (low, high) = match param.kind {
            ParameterKind::Float { min, max, .. } => (min, max),
            ParameterKind::Enum { labels, .. } => (0.0, labels.len().saturating_sub(1) as f32),
        };
        let value = if end {
            high
        } else if home {
            low
        } else if steps != 0.0 {
            self.step(steps, fine)
        } else {
            return None;
        };
        Some(param.clamp(value))
    }

    /// The value `notches` wheel notches (or arrow presses) from here: a
    /// choice steps one option a notch.
    fn step(&self, notches: f32, fine: bool) -> f32 {
        let param = self.param;
        match param.kind {
            ParameterKind::Enum { .. } => param.clamp(self.value + notches.round()),
            ParameterKind::Float { .. } => {
                let step = if fine { WHEEL_STEP * FINE } else { WHEEL_STEP };
                let position = param.to_normalized(self.value) + notches * step;
                param.clamp(param.from_normalized(position.clamp(0.0, 1.0)))
            }
        }
    }

    /// The radius ring `i` is drawn at, for a knob of `radius`.
    fn ring_radius(radius: f32, i: usize) -> f32 {
        radius + RING_GAP + RING_STEP * i.min(RING_CIRCLES - 1) as f32
    }

    /// The ring nearest `pos`, if it's on the rings (outside the knob's
    /// body) or `alt` asks for the last one.
    fn ring_at(&self, rect: Rect, pos: Pos2, alt: bool) -> Option<usize> {
        if self.rings.is_empty() {
            return None;
        }
        if alt {
            return Some(self.rings.len() - 1);
        }
        let (center, radius) = self.circle(rect);
        let distance = pos.distance(center);
        if distance < radius - 3.0 || distance > Self::ring_radius(radius, RING_CIRCLES) + 3.0 {
            return None;
        }
        (0..self.rings.len()).min_by(|&a, &b| {
            let off = |i| (Self::ring_radius(radius, i) - distance).abs();
            off(a).total_cmp(&off(b))
        })
    }

    /// The ring being dragged, and the amount it had when the drag began.
    fn ring_drag(&self, ui: &egui::Ui) -> Option<(usize, f32)> {
        ui.data(|d| d.get_temp(self.id.with("ring drag")))
    }

    /// Drags and right-clicks on the rings: a new amount for the ring
    /// dragged, and the ring right-clicked.
    fn interact_rings(
        &self,
        ui: &mut egui::Ui,
        rect: Rect,
        response: &Response,
    ) -> (Option<(usize, f32)>, Option<usize>) {
        let drag_id = self.id.with("ring drag");
        let alt = ui.input(|i| i.modifiers.alt);
        if response.drag_started() {
            // Where the press was: a drag only starts once the pointer has
            // moved a little way from it.
            let origin = ui.input(|i| i.pointer.press_origin());
            let ring = origin
                .or(response.interact_pointer_pos())
                .and_then(|pos| self.ring_at(rect, pos, alt));
            ui.data_mut(|d| match ring {
                Some(i) => {
                    d.insert_temp(drag_id, (i, self.rings[i].amount));
                }
                None => d.remove::<(usize, f32)>(drag_id),
            });
        }
        let mut changed = None;
        if let Some((i, start)) = self.ring_drag(ui)
            && i < self.rings.len()
        {
            if response.dragged() {
                let fine =
                    ui.input(|i| i.modifiers.shift || i.modifiers.command || i.modifiers.ctrl);
                let delta = response.drag_delta();
                let mut amount = (delta.x - delta.y) / DRAG_RANGE;
                if fine {
                    amount *= FINE;
                }
                let moved = (start + amount).clamp(-1.0, 1.0);
                ui.data_mut(|d| d.insert_temp(drag_id, (i, moved)));
                if moved != self.rings[i].amount {
                    changed = Some((i, moved));
                }
            }
            if response.drag_stopped() {
                ui.data_mut(|d| d.remove::<(usize, f32)>(drag_id));
            }
        }
        let menu = response
            .secondary_clicked()
            .then(|| response.interact_pointer_pos())
            .flatten()
            .and_then(|pos| self.ring_at(rect, pos, alt));
        (changed, menu)
    }

    fn interact(&self, ui: &mut egui::Ui, response: &Response) -> Option<f32> {
        let param = self.param;
        let fine = ui.input(|i| i.modifiers.shift || i.modifiers.command || i.modifiers.ctrl);
        let drag_id = self.id.with("drag");
        if response.drag_started() {
            ui.data_mut(|d| d.insert_temp(drag_id, param.to_normalized(self.value)));
        }
        if response.dragged() {
            let delta = response.drag_delta();
            let mut amount = (delta.x - delta.y) / DRAG_RANGE;
            if fine {
                amount *= FINE;
            }
            // Accumulated unquantized, so slow drags still step an enum.
            let start = ui
                .data(|d| d.get_temp::<f32>(drag_id))
                .unwrap_or_else(|| param.to_normalized(self.value));
            let position = (start + amount).clamp(0.0, 1.0);
            ui.data_mut(|d| d.insert_temp(drag_id, position));
            let value = param.clamp(param.from_normalized(position));
            return (value != self.value).then_some(value);
        }
        if response.hovered() {
            // The wheel turns the knob, not the view it's in.
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            let notches = wheel_notches(ui);
            if notches != 0.0 {
                // A choice steps once a frame however far the wheel went.
                let notches = match param.kind {
                    ParameterKind::Enum { .. } => notches.signum(),
                    ParameterKind::Float { .. } => notches,
                };
                let value = self.step(notches, fine);
                return (value != self.value).then_some(value);
            }
        }
        None
    }

    fn paint(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        response: &Response,
        value: f32,
        ring_changed: Option<(usize, f32)>,
    ) {
        let visuals = ui.visuals();
        // The parent's painter rather than one clipped to the knob: a clip of
        // its own would cut the frame's shapes into a mesh per knob, and
        // everything drawn is inside the knob's rect anyway.
        let painter = ui.painter();
        // Hovered, dragged or focused, fading in and out.
        let lit = theme::hover(ui, response);
        let accent = theme::accent(visuals);

        let label_color = self.label_color.unwrap_or(
            visuals
                .text_color()
                .lerp_to_gamma(visuals.strong_text_color(), lit),
        );
        // Long names shrink a little, then end in an ellipsis, rather than
        // run into the next knob's. A compact knob shows its value there
        // while it's lit, having no row of its own for it.
        // Not when it shows no name: what names it is drawn over that row.
        let galley = if self.compact && lit > 0.5 && !self.label.is_empty() {
            let text = self.value_text(value);
            fitted(ui, &text, FontId::monospace, rect.width(), label_color)
        } else {
            fitted(
                ui,
                self.label,
                FontId::proportional,
                rect.width(),
                label_color,
            )
        };
        painter.galley(
            pos2(rect.center().x - galley.size().x / 2.0, rect.top()),
            galley,
            label_color,
        );

        let (center, radius) = self.circle(rect);
        let normalized = self.param.to_normalized(value);
        let origin = match self.param.kind {
            // Bipolar ranges (pan, gain) fill from their middle.
            ParameterKind::Float { min, max, .. } if min < 0.0 && max > 0.0 => {
                self.param.to_normalized(0.0)
            }
            _ => 0.0,
        };
        let track = visuals.widgets.inactive.bg_fill;
        let ring = radius - 2.0;
        painter.add(arc(center, ring, 0.0, 1.0, Stroke::new(TRACK_WIDTH, track)));
        painter.add(arc(
            center,
            ring,
            origin.min(normalized),
            origin.max(normalized),
            Stroke::new(TRACK_WIDTH, accent),
        ));
        let body = radius - 6.0;
        let fill = visuals
            .widgets
            .inactive
            .weak_bg_fill
            .lerp_to_gamma(visuals.widgets.hovered.bg_fill, lit);
        painter.circle(
            center,
            body,
            fill,
            Stroke::new(
                stroke::HAIRLINE,
                visuals.widgets.noninteractive.bg_stroke.color,
            ),
        );
        let angle = START + SWEEP * normalized;
        let direction = vec2(angle.cos(), angle.sin());
        painter.line_segment(
            [center + direction * body * 0.25, center + direction * body],
            Stroke::new(POINTER_WIDTH, visuals.strong_text_color()),
        );
        // Each modulation's reach, outside the track, in its color.
        for (i, mod_ring) in self.rings.iter().enumerate() {
            let amount = match ring_changed {
                Some((changed, amount)) if changed == i => amount,
                _ => mod_ring.amount,
            };
            let (from, to) = if mod_ring.bipolar {
                (normalized - amount.abs(), normalized + amount.abs())
            } else {
                (normalized, normalized + amount)
            };
            let (from, to) = (from.min(to).clamp(0.0, 1.0), from.max(to).clamp(0.0, 1.0));
            if to - from > 1e-4 {
                let radius = Self::ring_radius(self.diameter / 2.0, i);
                painter.add(arc(
                    center,
                    radius,
                    from,
                    to,
                    Stroke::new(RING_WIDTH, mod_ring.color),
                ));
            }
        }
        if let Some(modulated) = self.modulated {
            let angle = START + SWEEP * modulated.clamp(0.0, 1.0);
            let at = center + vec2(angle.cos(), angle.sin()) * ring;
            painter.circle_filled(at, 2.5, visuals.strong_text_color());
        }
        if self.drop_highlight {
            painter.circle_filled(center, radius + HALO, accent.gamma_multiply(0.15));
            painter.circle_stroke(
                center,
                radius + HALO,
                Stroke::new(stroke::INDICATOR, accent),
            );
        }
        // The keyboard's focus: a ring of the knob's own shape, fading in,
        // instead of the app's rectangle.
        let focused = response.has_focus();
        if focused {
            chrome::rings_own_focus(ui.ctx(), response.id);
        }
        let focus = theme::fade(ui.ctx(), response.id.with("focus ring"), focused);
        if focus > 0.0 {
            painter.circle_stroke(
                center,
                radius + HALO,
                Stroke::new(stroke::EMPHASIS, accent.gamma_multiply(focus)),
            );
        }

        if self.compact {
            return;
        }
        // Long values ("7.00 cents") shrink to fit rather than run into the
        // next knob's.
        let value_color = visuals
            .text_color()
            .gamma_multiply(0.8)
            .lerp_to_gamma(visuals.strong_text_color(), lit);
        let text = self.value_text(value);
        let galley = fitted(ui, &text, FontId::monospace, rect.width(), value_color);
        painter.galley(
            pos2(
                rect.center().x - galley.size().x / 2.0,
                rect.bottom() - galley.size().y,
            ),
            galley,
            value_color,
        );
    }
}

/// `text` laid out on one line no wider than `width`: at the caption size
/// if it fits, else at the smallest, ending in an ellipsis if even that
/// doesn't.
fn fitted(
    ui: &egui::Ui,
    text: &str,
    font: fn(f32) -> FontId,
    width: f32,
    color: Color32,
) -> std::sync::Arc<egui::Galley> {
    let whole = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font(text::SMALL), color));
    if whole.size().x <= width {
        return whole;
    }
    let size = text::TINY;
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat::simple(font(size), color),
    );
    job.wrap = egui::text::TextWrapping {
        max_width: width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.fonts_mut(|f| f.layout_job(job))
}

/// How wide a knob of `diameter` is: enough for most names (longer ones
/// shrink, then end in an ellipsis), and the same for every knob of that
/// size, so they line up in rows.
pub fn width(diameter: f32) -> f32 {
    (diameter + 22.0).max(58.0)
}

/// How tall a knob of `diameter` is, its name and value included.
pub fn height(diameter: f32) -> f32 {
    LABEL_HEIGHT + diameter + VALUE_HEIGHT
}

/// A compact knob's diameter (see [`Knob::compact`]).
pub const COMPACT: f32 = 24.0;

/// How wide a compact knob of `diameter` is: room for its modulation
/// rings and a short name.
pub const fn compact_width(diameter: f32) -> f32 {
    diameter + 22.0
}

/// How tall a compact knob of `diameter` is: its name (or value) and the
/// circle.
pub const fn compact_height(diameter: f32) -> f32 {
    LABEL_HEIGHT + diameter + space::XS
}

fn keys_id() -> Id {
    Id::new("knob with the keys")
}

/// Whether a knob has the keyboard's focus, so the arrow keys, Home and
/// End are its: shortcuts on them (nudging clips, moving the playhead)
/// leave them alone.
pub fn has_keys(ctx: &egui::Context) -> bool {
    let focused = ctx.memory(|m| m.focused());
    focused.is_some() && focused == ctx.data(|d| d.get_temp::<egui::Id>(keys_id()))
}

/// Whether `key` belongs to a knob with the keyboard's focus rather than
/// to a shortcut.
pub fn takes_key(ctx: &egui::Context, key: Key) -> bool {
    KEYS.contains(&key) && has_keys(ctx)
}

/// Mouse wheel notches this frame, up positive. Shift+wheel may arrive
/// sideways, so both directions count.
pub fn wheel_notches(ui: &egui::Ui) -> f32 {
    ui.input(|i| {
        i.events
            .iter()
            .map(|e| match e {
                egui::Event::MouseWheel { unit, delta, .. } => {
                    let amount = delta.y + delta.x;
                    match unit {
                        egui::MouseWheelUnit::Line => amount,
                        egui::MouseWheelUnit::Point => amount / 50.0,
                        egui::MouseWheelUnit::Page => amount * 4.0,
                    }
                }
                _ => 0.0,
            })
            .sum()
    })
}

/// An arc of the knob's sweep, from `from` to `to` (0 to 1).
fn arc(center: Pos2, radius: f32, from: f32, to: f32, stroke: Stroke) -> Shape {
    let segments = ((to - from) * 40.0).ceil().max(1.0) as usize;
    let points = (0..=segments)
        .map(|i| {
            let t = from + (to - from) * i as f32 / segments as f32;
            let angle = START + SWEEP * t;
            center + vec2(angle.cos(), angle.sin()) * radius
        })
        .collect();
    Shape::line(points, stroke)
}

/// A value as a knob shows it: short, with kHz and seconds where they read
/// better than Hz and ms.
pub fn format_value(param: &ParamDescriptor, value: f32) -> String {
    let value = param.clamp(value);
    if let ParameterKind::Float { unit, .. } = param.kind {
        match unit {
            "Hz" if value >= 1000.0 => {
                let khz = value / 1000.0;
                return if khz >= 10.0 {
                    format!("{khz:.1} kHz")
                } else {
                    format!("{khz:.2} kHz")
                };
            }
            "ms" if value >= 1000.0 => return format!("{:.2} s", value / 1000.0),
            "dB" => return format!("{value:+.1} dB"),
            _ => {}
        }
    }
    param.format(value)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    const LEVEL: ParamDescriptor = instruments::float("level", "Level", 0.0, 1.0, 0.5, "", false);

    fn compact_harness(size: Rc<Cell<egui::Vec2>>) -> Harness<'static> {
        Harness::new_ui(move |ui| {
            let knob = Knob::new(Id::new("level"), &LEVEL, 0.25)
                .diameter(COMPACT)
                .compact(true)
                .show(ui);
            size.set(knob.response.rect.size());
        })
    }

    #[test]
    fn a_compact_knob_is_shorter_and_narrower() {
        assert_eq!(compact_width(COMPACT), 46.0);
        // Against the 28 px knobs the dense panels had: a third shorter.
        assert!(compact_height(COMPACT) <= height(28.0) - 18.0);
        let size = Rc::new(Cell::new(egui::Vec2::ZERO));
        let mut harness = compact_harness(size.clone());
        harness.run();
        assert_eq!(
            size.get(),
            vec2(compact_width(COMPACT), compact_height(COMPACT))
        );
    }

    #[test]
    fn an_unnamed_compact_knob_names_its_parameter_in_its_tooltip() {
        // A macro's knob, named by the tile over it.
        let mut harness = Harness::new_ui(|ui| {
            Knob::new(Id::new("macro"), &LEVEL, 0.25)
                .diameter(COMPACT)
                .compact(true)
                .label("")
                .show(ui);
        });
        harness.run();
        harness.get_by_label("Level").hover();
        harness.run();
        assert!(harness.query_by_label_contains("Level: 0.25").is_some());
    }

    #[test]
    fn hovering_a_compact_knob_shows_its_value() {
        let mut harness = compact_harness(Rc::new(Cell::new(egui::Vec2::ZERO)));
        harness.run();
        assert!(harness.query_by_label_contains("0.25").is_none());
        harness.get_by_label("Level").hover();
        harness.run();
        assert!(harness.query_by_label_contains("0.25").is_some());
    }
}
