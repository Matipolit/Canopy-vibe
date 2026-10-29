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

/// A device's card: a raised ground around `rect` (a device's title bar
/// and panel) with its title bar, `bar` tall, filled with `bar_fill`
/// across the top. Painted into a shape reserved before the device was
/// laid out, so it goes under it.
pub fn card(visuals: &egui::Visuals, rect: Rect, bar: f32, bar_fill: Color32) -> Shape {
    let ground = rect.expand(space::S);
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), bar));
    Shape::Vec(vec![
        Shape::rect_filled(ground, radius::PANEL, visuals.faint_bg_color),
        Shape::rect_stroke(
            ground,
            radius::PANEL,
            visuals.widgets.noninteractive.bg_stroke,
            StrokeKind::Inside,
        ),
        Shape::rect_filled(bar, radius::CONTROL, bar_fill),
    ])
}

/// What a [`transport_button`] shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Play,
    Stop,
    Record,
}

/// A transport button (play, stop, record): a painted glyph on a control
/// [`size::TRANSPORT`] tall, filled with `lit` while `on`, fading between
/// the two. `label` is for screen readers and tests.
pub fn transport_button(
    ui: &mut Ui,
    glyph: Glyph,
    on: bool,
    lit: Color32,
    label: &str,
) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(size::TRANSPORT * 1.4, size::TRANSPORT), Sense::click());
    let t = ui
        .ctx()
        .animate_bool_with_time(response.id.with("lit"), on, anim::NORMAL);
    let h = theme::hover(ui, &response);
    let visuals = ui.visuals();
    let rest = visuals
        .widgets
        .inactive
        .weak_bg_fill
        .lerp_to_gamma(visuals.widgets.hovered.weak_bg_fill, h);
    let fill = rest.lerp_to_gamma(lit.gamma_multiply(0.35), t);
    let painter = ui.painter();
    let corner = CornerRadius::same(radius::CONTROL);
    painter.rect_filled(rect, corner, fill);
    if t > 0.0 {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(stroke::HAIRLINE, lit.gamma_multiply(0.8 * t)),
            StrokeKind::Inside,
        );
    }
    let ink = visuals
        .text_color()
        .lerp_to_gamma(visuals.strong_text_color(), h)
        .lerp_to_gamma(lit, t);
    let c = rect.center();
    match glyph {
        Glyph::Play => {
            let points = vec![
                c + vec2(-3.5, -5.5),
                c + vec2(5.5, 0.0),
                c + vec2(-3.5, 5.5),
            ];
            painter.add(Shape::convex_polygon(points, ink, Stroke::NONE));
        }
        Glyph::Stop => {
            painter.rect_filled(
                Rect::from_center_size(c, Vec2::splat(9.0)),
                CornerRadius::same(1),
                ink,
            );
        }
        Glyph::Record => {
            let dot = visuals
                .weak_text_color()
                .lerp_to_gamma(lit, t.max(0.35 * h));
            painter.circle_filled(c, 5.0, dot);
        }
    }
    focus_ring(ui, &response, rect);
    let label = label.to_owned();
    response.widget_info(move || {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, &label)
    });
    response
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
        .inner_margin(egui::Margin::symmetric(5, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(key).font(text::medium(text::SMALL)).strong());
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

/// A group of an instrument's controls on a card with a strip of `color`
/// along its top; `add` lays it out, starting with [`section_header`].
/// Returns what `add` returns, and the whole card's response.
pub fn section<R>(
    ui: &mut Ui,
    color: Color32,
    add: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    let background = ui.painter().add(Shape::Noop);
    let inner = egui::Frame::NONE
        .inner_margin(egui::Margin::same(space::S as i8))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = space::XS;
                add(ui)
            })
            .inner
        });
    let rect = inner.response.rect;
    let visuals = ui.visuals();
    let strip = Rect::from_min_size(rect.min, vec2(rect.width(), space::XS));
    ui.painter().set(
        background,
        Shape::Vec(vec![
            Shape::rect_filled(rect, radius::PANEL, visuals.faint_bg_color),
            Shape::rect_stroke(
                rect,
                radius::PANEL,
                visuals.widgets.noninteractive.bg_stroke,
                StrokeKind::Inside,
            ),
            Shape::rect_filled(
                strip,
                CornerRadius {
                    nw: radius::PANEL,
                    ne: radius::PANEL,
                    sw: 0,
                    se: 0,
                },
                color.gamma_multiply(0.8),
            ),
        ]),
    );
    egui::InnerResponse::new(inner.inner, inner.response)
}

/// A small selectable tile naming one of several (an envelope, an LFO, a
/// drum layer) by `text`, with `curve` (heights from 0 to 1, left to
/// right) drawn after it in `color`: what it is at a glance. `name` is what
/// screen readers (and tests) call it.
pub fn chip(
    ui: &mut Ui,
    name: &str,
    text: &str,
    selected: bool,
    color: Color32,
    curve: &[f32],
) -> Response {
    let font = FontId::proportional(text::SMALL);
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, color));
    let sketch = if curve.len() > 1 { CHIP_SKETCH } else { 0.0 };
    let width = galley.size().x + sketch + 2.0 * space::S + space::XS;
    let (rect, response) = ui.allocate_exact_size(vec2(width, size::CONTROL), Sense::click());
    let lit = theme::hover(ui, &response);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let corner = CornerRadius::same(radius::CONTROL);
        let fill = if selected { 0.32 } else { 0.10 + 0.08 * lit };
        painter.rect_filled(rect, corner, color.gamma_multiply(fill));
        if selected {
            painter.rect_stroke(
                rect,
                corner,
                Stroke::new(stroke::HAIRLINE, color),
                StrokeKind::Inside,
            );
        }
        let ink = if selected {
            ui.visuals().strong_text_color()
        } else {
            color
        };
        painter.galley(
            pos2(
                rect.left() + space::S,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            ink,
        );
        if sketch > 0.0 {
            let area = Rect::from_min_max(
                pos2(rect.right() - space::S - sketch, rect.top() + space::S),
                pos2(rect.right() - space::S, rect.bottom() - space::S),
            );
            let last = (curve.len() - 1) as f32;
            let points: Vec<Pos2> = curve
                .iter()
                .enumerate()
                .map(|(i, y)| {
                    pos2(
                        area.left() + i as f32 / last * area.width(),
                        area.bottom() - y.clamp(0.0, 1.0) * area.height(),
                    )
                })
                .collect();
            painter.add(Shape::line(points, Stroke::new(stroke::EMPHASIS, ink)));
        }
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, name)
    });
    response
}

/// How wide a [`chip`]'s curve is.
const CHIP_SKETCH: f32 = 18.0;

/// A [`section`]'s first row: its title in `color` (none if empty, for a
/// section whose switch names it), then `add`'s controls (an on switch, a
/// menu).
pub fn section_header(ui: &mut Ui, title: &str, color: Color32, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_height(size::CONTROL);
        ui.spacing_mut().item_spacing.x = space::S;
        if !title.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(title)
                        .font(text::semibold(text::SMALL))
                        .color(color),
                )
                .selectable(false),
            );
        }
        add(ui);
    });
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
        10.0
    };
    if !title.is_empty() {
        painter.text(
            center - vec2(0.0, gap),
            Align2::CENTER_CENTER,
            title,
            text::medium(text::BODY + 1.0),
            visuals.text_color().gamma_multiply(0.8),
        );
    }
    if !hint.is_empty() {
        painter.text(
            center + vec2(0.0, gap + if gap > 0.0 { 2.0 } else { 0.0 }),
            Align2::CENTER_CENTER,
            hint,
            FontId::proportional(text::SMALL + 0.5),
            visuals.weak_text_color(),
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    /// A harness drawing `add` once the theme's fonts are in (they take a
    /// pass to load, as in the app).
    fn themed(mut add: impl FnMut(&mut Ui) + 'static) -> Harness<'static> {
        let styled = Cell::new(false);
        Harness::new_ui(move |ui| {
            if !styled.replace(true) {
                theme::apply(ui.ctx());
                ui.ctx().request_discard("fonts");
                return;
            }
            add(ui);
        })
    }

    #[test]
    fn a_chip_is_named_and_clicks() {
        let clicked = Rc::new(Cell::new(false));
        let out = clicked.clone();
        let mut harness = themed(move |ui| {
            let curve = [0.0, 1.0, 0.5, 0.5, 0.0];
            if chip(ui, "LFO 3", "3", true, theme::section::LFO, &curve).clicked() {
                out.set(true);
            }
        });
        harness.run();
        harness.get_by_label("LFO 3").click();
        harness.run();
        assert!(clicked.get());
    }

    #[test]
    fn a_section_wraps_its_body_under_a_titled_header() {
        let rects = Rc::new(Cell::new((Rect::NOTHING, Rect::NOTHING)));
        let out = rects.clone();
        let mut harness = themed(move |ui| {
            let section = section(ui, theme::section::FILTER, |ui| {
                section_header(ui, "Filter", theme::section::FILTER, |_| {});
                ui.allocate_exact_size(vec2(100.0, 50.0), Sense::hover()).0
            });
            out.set((section.response.rect, section.inner));
        });
        harness.run();
        let (outer, body) = rects.get();
        assert!(outer.contains_rect(body), "{outer:?} holds {body:?}");
        assert!(outer.height() >= body.height() + size::CONTROL);
        assert!(body.top() - outer.top() >= size::CONTROL);
        harness.get_by_label("Filter");
    }
}
