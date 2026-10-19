//! The small painted controls that every view shares, on the theme's
//! sizes: fold arrows, icon buttons, toggles, title bars, menu items and
//! shortcut tooltips with key caps, dialog buttons, the hint shown where a
//! view is empty, the labels by the pointer while dragging
//! ([`drop_label`], [`drag_label`]) and the keyboard focus ring
//! ([`paint_keyboard_focus`]; a widget with a ring of its own shape, as a
//! knob has, says so with [`rings_own_focus`]). (The UI font lacks most
//! symbols, so icons are drawn.)
//!
//! They animate their hover and state changes briefly, and only ask for
//! frames while something is changing.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Response, RichText, Sense,
    Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use crate::theme::{self, anim, color, radius, size, space, stroke, text};

/// A triangle pointing right when folded and down when open, turning
/// between the two.
pub fn fold_arrow(ui: &mut Ui, folded: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(14.0, size::CONTROL), Sense::click());
    let open =
        ui.ctx()
            .animate_bool_with_time(response.id.with("fold turn"), !folded, anim::NORMAL);
    let hover = theme::hover(ui, &response);
    let visuals = ui.visuals();
    let color = visuals
        .text_color()
        .lerp_to_gamma(visuals.strong_text_color(), hover);
    let angle = open * std::f32::consts::FRAC_PI_2;
    let (sin, cos) = angle.sin_cos();
    let c = rect.center();
    let turn = |x: f32, y: f32| c + vec2(x * cos - y * sin, x * sin + y * cos);
    let points = vec![turn(-3.0, -4.5), turn(4.0, 0.0), turn(-3.0, 4.5)];
    ui.painter()
        .add(Shape::convex_polygon(points, color, Stroke::NONE));
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            if folded { "Unfold" } else { "Fold" },
        )
    });
    response.on_hover_text(if folded { "Unfold" } else { "Fold" })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Duplicate,
    Menu,
    Remove,
    /// Fit something in view: two brackets.
    Fit,
}

/// A small icon button, `label` for screen readers and tests, `hover` as
/// its tooltip.
pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str, hover: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size::ICON), Sense::click());
    let t = theme::hover(ui, &response);
    let visuals = ui.visuals();
    let pressed = response.is_pointer_button_down_on();
    let fill = visuals
        .widgets
        .hovered
        .weak_bg_fill
        .gamma_multiply(if pressed { 1.0 } else { 0.75 * t });
    if t > 0.0 {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::CONTROL), fill);
    }
    let ink = visuals
        .text_color()
        .gamma_multiply(0.75)
        .lerp_to_gamma(visuals.strong_text_color(), t);
    paint_icon(
        ui.painter(),
        rect,
        icon,
        Stroke::new(1.3, ink),
        visuals.panel_fill,
    );
    let label = label.to_owned();
    response.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    if hover.is_empty() {
        response
    } else {
        response.on_hover_text(hover)
    }
}

/// A menu item with its shortcut at the right (`""` for none).
pub fn menu_item(ui: &mut Ui, label: &str, shortcut: &str) -> Response {
    menu_item_enabled(ui, true, label, shortcut)
}

/// [`menu_item`], greyed out unless `enabled`.
pub fn menu_item_enabled(ui: &mut Ui, enabled: bool, label: &str, shortcut: &str) -> Response {
    let button = if shortcut.is_empty() {
        egui::Button::new(label)
    } else {
        egui::Button::new(label).shortcut_text(shortcut)
    };
    ui.add_enabled(enabled, button)
}

/// A framed on/off button (Loop, Snap, Metronome): framed whether on or
/// off, so it reads as a button either way. `keys` is its shortcut, shown
/// in the tooltip ("" for none).
pub fn switch(ui: &mut Ui, on: bool, text: &str, what: &str, keys: &str) -> Response {
    let response = ui.add(egui::Button::new(text).selected(on));
    if keys.is_empty() {
        response.on_hover_text(what)
    } else {
        shortcut_tip(response, what, keys)
    }
}

/// A dialog's buttons, as clicked.
pub struct DialogButtons {
    pub primary: Response,
    pub secondary: Option<Response>,
    pub cancel: Response,
}

/// The row of buttons along a dialog's bottom, the same in every dialog:
/// at the right, the main action (`primary`, highlighted, and greyed out
/// unless `enabled`), then `secondary` if there is one, then `cancel`
/// nearest the edge. All as wide as each other, with a gap above.
pub fn dialog_buttons(
    ui: &mut Ui,
    primary: &str,
    enabled: bool,
    secondary: Option<&str>,
    cancel: &str,
) -> DialogButtons {
    ui.add_space(space::S);
    let width = size::DIALOG_BUTTON;
    let button = |text: &str| egui::Button::new(text).min_size(vec2(width, size::CONTROL));
    egui::Sides::new()
        .show(
            ui,
            |_| {},
            |ui| {
                let cancel = ui.add(button(cancel));
                let secondary = secondary.map(|text| ui.add(button(text)));
                let primary = ui.add_enabled(enabled, button(primary).selected(true));
                DialogButtons {
                    primary,
                    secondary,
                    cancel,
                }
            },
        )
        .1
}

/// Draws `icon` in `rect` with `stroke`; `ground` fills shapes that overlap.
pub fn paint_icon(painter: &Painter, rect: Rect, icon: Icon, stroke: Stroke, ground: Color32) {
    let c = rect.center();
    match icon {
        Icon::Duplicate => {
            let back = Rect::from_center_size(c + vec2(-1.5, -1.5), vec2(8.0, 8.0));
            let front = back.translate(vec2(3.0, 3.0));
            painter.rect_stroke(back, 1.0, stroke, StrokeKind::Middle);
            painter.rect_filled(front, 1.0, ground);
            painter.rect_stroke(front, 1.0, stroke, StrokeKind::Middle);
        }
        Icon::Menu => {
            for dy in [-3.5, 0.0, 3.5] {
                painter.hline(c.x - 4.5..=c.x + 4.5, c.y + dy, stroke);
            }
        }
        Icon::Remove => {
            let d = 3.5;
            painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], stroke);
            painter.line_segment([c + vec2(-d, d), c + vec2(d, -d)], stroke);
        }
        Icon::Fit => {
            // A double arrow stretched between two walls: |<->|.
            let (w, h) = (6.0, 4.5);
            for side in [-1.0, 1.0] {
                let wall = c.x + side * w;
                painter.vline(wall, c.y - h..=c.y + h, stroke);
                let tip = pos2(wall - side * 1.8, c.y);
                painter.add(Shape::line(
                    vec![
                        tip + vec2(-side * 2.2, -2.2),
                        tip,
                        tip + vec2(-side * 2.2, 2.2),
                    ],
                    stroke,
                ));
            }
            painter.hline(c.x - w + 1.8..=c.x + w - 1.8, c.y, stroke);
        }
    }
}

/// A small lit toggle (a track's mute and solo): `color` when on, fading
/// between the two.
pub fn toggle(ui: &mut Ui, label: &str, on: bool, lit: Color32, hover: &str) -> Response {
    let font = FontId::proportional(text::SMALL);
    let width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let size = vec2(
        (width + 2.0 * space::S + 2.0).max(size::SMALL_CONTROL + 2.0),
        size::SMALL_CONTROL,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let t = ui
        .ctx()
        .animate_bool_with_time(response.id.with("lit"), on, anim::NORMAL);
    let h = theme::hover(ui, &response);
    let visuals = ui.visuals();
    let off = visuals
        .widgets
        .inactive
        .weak_bg_fill
        .lerp_to_gamma(visuals.widgets.hovered.weak_bg_fill, h);
    let fill = off.lerp_to_gamma(lit, t);
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(radius::CONTROL), fill);
    let ink = visuals.text_color().lerp_to_gamma(Color32::BLACK, t);
    painter.text(rect.center(), Align2::CENTER_CENTER, label, font, ink);
    focus_ring(ui, &response, rect);
    let label = label.to_owned();
    response.widget_info(move || {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, &label)
    });
    response.on_hover_text(hover)
}

/// Paints the accent outline around `rect` while `response` has the
/// keyboard focus. The app already rings whatever has the focus
/// ([`paint_keyboard_focus`]), in this same style, so a widget only needs
/// this to show its focus on a rect other than its response's.
pub fn focus_ring(ui: &Ui, response: &Response, rect: Rect) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(1.0),
            CornerRadius::same(radius::CONTROL),
            Stroke::new(stroke::EMPHASIS, theme::accent(ui.visuals())),
            StrokeKind::Outside,
        );
    }
}

/// The widget with the keyboard focus (Tab moves it), where a ring should
/// show it: none while text is being typed (a text field shows its own) or
/// nothing has the focus. Clicks don't give egui's widgets the focus, so
/// this is the keyboard's.
pub fn keyboard_focus(ctx: &egui::Context) -> Option<(Rect, Rect, egui::LayerId)> {
    if ctx.text_edit_focused() {
        return None;
    }
    let id = ctx.memory(|m| m.focused())?;
    let response = ctx.read_response(id)?;
    Some((response.rect, response.interact_rect, response.layer_id))
}

fn own_ring_id() -> egui::Id {
    egui::Id::new("own focus ring")
}

/// Says that widget `id`, which has the keyboard focus, draws a ring of
/// its own shape this frame (a knob's is round), so
/// [`paint_keyboard_focus`] leaves it be.
pub fn rings_own_focus(ctx: &egui::Context, id: egui::Id) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(own_ring_id(), (id, pass)));
}

/// Whether the widget with the keyboard focus draws its own ring
/// ([`rings_own_focus`]), as of this frame or the one before.
pub fn focus_rings_itself(ctx: &egui::Context) -> bool {
    let pass = ctx.cumulative_pass_nr();
    let focused = ctx.memory(|m| m.focused());
    ctx.data(|d| d.get_temp::<(egui::Id, u64)>(own_ring_id()))
        .is_some_and(|(id, at)| (at == pass || at + 1 == pass) && Some(id) == focused)
}

/// Rings the widget with the keyboard focus in the accent, the same on
/// every control (egui's own and the painted ones), gliding from one to
/// the next as Tab moves it; a widget that draws its own ring
/// ([`rings_own_focus`]) is left to it. The app calls it once everything
/// is drawn.
pub fn paint_keyboard_focus(ctx: &egui::Context) {
    let Some((rect, visible, layer)) = keyboard_focus(ctx) else {
        return;
    };
    if focus_rings_itself(ctx) {
        return;
    }
    let id = egui::Id::new("keyboard focus ring");
    let glide = |n: u32, v: f32| theme::glide_marker(ctx, id.with(n), v);
    let rect = Rect::from_min_max(
        pos2(glide(0, rect.left()), glide(1, rect.top())),
        pos2(glide(2, rect.right()), glide(3, rect.bottom())),
    );
    let visuals = ctx.global_style().visuals.clone();
    let painter = ctx.layer_painter(layer).with_clip_rect(visible.expand(4.0));
    painter.rect_stroke(
        rect.expand(1.0),
        CornerRadius::same(radius::CONTROL),
        Stroke::new(stroke::EMPHASIS, theme::accent(&visuals)),
        StrokeKind::Outside,
    );
}

/// The background of a title bar (an effect's, an instrument's): brighter
/// when hovered, the selection's color when selected, fading between.
pub fn bar_fill(ui: &Ui, id: egui::Id, hovered: bool, selected: bool) -> Color32 {
    let ctx = ui.ctx();
    let h = ctx.animate_bool_with_time(id.with("bar hover"), hovered, anim::FAST);
    let s = ctx.animate_bool_with_time(id.with("bar selected"), selected, anim::NORMAL);
    let rest = color::BAR.lerp_to_gamma(color::BAR_HOVER, h);
    rest.lerp_to_gamma(ui.visuals().selection.bg_fill.gamma_multiply(0.6), s)
}

/// A tooltip saying what a control does and, in key caps, its shortcut:
/// beside a short description, under a long one.
pub fn shortcut_tip(response: Response, what: &str, keys: &str) -> Response {
    response.on_hover_ui(|ui| {
        if what.chars().count() > 48 {
            ui.label(what);
            ui.add_space(space::XS);
            ui.horizontal(|ui| keycaps(ui, keys));
        } else {
            ui.horizontal(|ui| {
                ui.label(what);
                ui.add_space(space::M);
                keycaps(ui, keys);
            });
        }
    })
}

/// `keys` as key caps: "Ctrl+Shift+Z" is three, "Ctrl+1 / Ctrl+2" two pairs.
pub fn keycaps(ui: &mut Ui, keys: &str) {
    ui.spacing_mut().item_spacing.x = space::XS;
    for (n, alternative) in keys.split(" / ").enumerate() {
        if n > 0 {
            ui.weak("/");
        }
        for (i, key) in alternative.split('+').enumerate() {
            if i > 0 {
                ui.weak("+");
            }
            keycap(ui, key);
        }
    }
}

/// One key cap.
pub fn keycap(ui: &mut Ui, key: &str) {
    let visuals = ui.visuals();
    egui::Frame::new()
        .fill(visuals.widgets.inactive.weak_bg_fill)
        .stroke(Stroke::new(
            stroke::HAIRLINE,
            visuals.widgets.noninteractive.bg_stroke.color,
        ))
        .corner_radius(CornerRadius::same(radius::SMALL))
        .inner_margin(egui::Margin::symmetric(4, 0))
        .show(ui, |ui| {
            ui.label(RichText::new(key).size(text::SMALL).strong());
        });
}

/// A group's caption, over its controls: small and weak, so the controls
/// stay what stands out.
pub fn caption(ui: &mut Ui, label: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(label)
                .size(text::SMALL)
                .color(ui.visuals().weak_text_color()),
        )
        .selectable(false),
    );
}

/// What an empty view says: what it would show (`title`) and how to get
/// there (`hint`), centered in `rect`.
pub fn empty_hint(painter: &Painter, rect: Rect, title: &str, hint: &str) {
    let visuals = painter.ctx().global_style().visuals.clone();
    let center = rect.center();
    // Both on two lines around the middle; either alone in the middle.
    let gap = if title.is_empty() || hint.is_empty() {
        0.0
    } else {
        9.0
    };
    if !title.is_empty() {
        painter.text(
            center - vec2(0.0, gap),
            Align2::CENTER_CENTER,
            title,
            FontId::proportional(text::BODY),
            visuals.weak_text_color(),
        );
    }
    if !hint.is_empty() {
        painter.text(
            center + vec2(0.0, gap + if gap > 0.0 { 2.0 } else { 0.0 }),
            Align2::CENTER_CENTER,
            hint,
            FontId::proportional(text::SMALL),
            visuals.weak_text_color().gamma_multiply(0.7),
        );
    }
}

fn drop_label_pass_id() -> egui::Id {
    egui::Id::new("drop label pass")
}

fn drag_label_id() -> egui::Id {
    egui::Id::new("drag label")
}

/// What's being dragged, by the pointer (`at`): shown at the end of the
/// frame ([`finish_drag_label`]) unless a drop target has said what a drop
/// on it would do ([`drop_label`]), which says more.
pub fn drag_label(ctx: &egui::Context, at: Pos2, label: &str) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(drag_label_id(), (pass, at, label.to_owned())));
}

/// Draws this frame's [`drag_label`], if no drop target drew its own. The
/// app calls it once everything else is drawn.
pub fn finish_drag_label(ctx: &egui::Context) {
    let pass = ctx.cumulative_pass_nr();
    let Some((at_pass, at, label)) =
        ctx.data_mut(|d| d.remove_temp::<(u64, Pos2, String)>(drag_label_id()))
    else {
        return;
    };
    let labelled = ctx.data(|d| d.get_temp::<u64>(drop_label_pass_id())) == Some(pass);
    if at_pass == pass && !labelled {
        drop_label(ctx, at, &label);
    }
}

/// A label by the pointer saying what a drop there will do.
pub fn drop_label(ctx: &egui::Context, at: Pos2, label: &str) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(drop_label_pass_id(), pass));
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("drop label"),
    ));
    let visuals = ctx.global_style().visuals.clone();
    let galley = painter.layout_no_wrap(
        label.to_owned(),
        FontId::proportional(text::BODY - 1.0),
        visuals.strong_text_color(),
    );
    let rect = Rect::from_min_size(at + vec2(14.0, 12.0), galley.size())
        .expand2(vec2(space::S + 2.0, space::XS + 1.0));
    painter.rect_filled(
        rect,
        CornerRadius::same(radius::CONTROL),
        visuals.window_fill,
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius::CONTROL),
        Stroke::new(stroke::HAIRLINE, theme::accent(&visuals)),
        StrokeKind::Outside,
    );
    painter.galley(
        rect.min + vec2(space::S + 2.0, space::XS + 1.0),
        galley,
        visuals.strong_text_color(),
    );
}
