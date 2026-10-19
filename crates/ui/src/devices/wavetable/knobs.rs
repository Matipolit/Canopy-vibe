//! What every page of the Wavetable's panel draws with: the live
//! modulation read once a frame, the colors of the modulation sources,
//! their draggable tiles, and the knobs that show modulation as rings and
//! take a dropped source as a new route.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Rect, Response, RichText, Sense, Stroke, Ui, pos2,
    vec2,
};
use instruments::ParameterKind;
use instruments::builtin::wavetable as wt;
use project::AutomationTarget;

use super::super::{Owner, Panel};
use crate::theme::{radius, size, space, stroke, text};
use crate::timeline::clicked_control_menu_with;
use crate::widgets::Knob;
use crate::widgets::knob::ModRing;
use crate::widgets::value_entry;

/// A modulation source being dragged from one of its tiles, onto a knob of
/// the same synth (`owner`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SourceDrag {
    pub owner: Owner,
    pub source: usize,
}

/// Where modulation has everything this frame, as the voices have it.
pub(super) struct Live {
    pub meters: [f32; instruments::MAX_METERS],
    pub modulation: wt::LiveModulation,
}

impl Live {
    pub fn new(panel: &Panel) -> Live {
        Live {
            meters: panel.meters,
            modulation: wt::live_modulation(panel.values(), &panel.meters),
        }
    }

    /// Destination `dest`'s modulation now, in its own unit.
    pub fn dest(&self, dest: usize) -> f32 {
        self.modulation
            .destinations
            .get(dest)
            .copied()
            .unwrap_or(0.0)
    }

    /// Source `source` now (macros as the matrix moves them).
    pub fn source(&self, source: usize) -> f32 {
        self.modulation.sources.get(source).copied().unwrap_or(0.0)
    }

    /// Parameter `index` where modulation has it now, in its own unit.
    pub fn param(&self, panel: &Panel, index: usize) -> f32 {
        let base = panel.values()[index];
        match wt::param_dest(index) {
            Some(dest) => wt::modulated_param(index, base, self.dest(dest)),
            None => base,
        }
    }

    /// The newest note's sounding pitch, for key-tracked displays.
    pub fn pitch(&self) -> f32 {
        self.meters[wt::METER_PITCH]
    }
}

/// Each source group's hue, as in the tiles, rings and matrix rows.
fn group_color(group: usize) -> Color32 {
    match group {
        0 => Color32::from_rgb(110, 210, 150),
        1 => Color32::from_rgb(110, 170, 255),
        2 => Color32::from_rgb(245, 165, 80),
        3 => Color32::from_rgb(235, 120, 190),
        _ => Color32::from_rgb(225, 210, 100),
    }
}

/// Source `source`'s color: its group's (envelopes green, LFOs blue,
/// macros orange, note pink, controllers yellow), a little lighter for
/// each later one in the group so neighbours' rings tell apart.
pub(super) fn source_color(source: usize) -> Color32 {
    for (group, (_, sources)) in wt::SOURCE_GROUPS.iter().enumerate() {
        if let Some(place) = sources.iter().position(|s| *s == source) {
            let base = group_color(group);
            let lift = place as f32 / sources.len().max(1) as f32 * 0.35;
            let mix = |c: u8| (c as f32 + (255.0 - c as f32) * lift) as u8;
            return Color32::from_rgb(mix(base.r()), mix(base.g()), mix(base.b()));
        }
    }
    Color32::GRAY
}

/// A short name for source `source`, for tiles.
pub(super) fn short_name(source: usize) -> String {
    if let Some(env) = wt::source_env(source) {
        return if env == 0 {
            "Amp".to_owned()
        } else {
            format!("Env {}", env + 1)
        };
    }
    if let Some(lfo) = wt::source_lfo(source) {
        return format!("LFO {}", lfo + 1);
    }
    if let Some(m) = wt::source_macro(source) {
        return format!("M{}", m + 1);
    }
    match source {
        wt::SRC_VELOCITY => "Vel",
        wt::SRC_NOTE => "Key",
        wt::SRC_RANDOM => "Rnd 1",
        wt::SRC_RANDOM2 => "Rnd 2",
        wt::SRC_LIFT => "Lift",
        wt::SRC_MOD_WHEEL => "Wheel",
        wt::SRC_PITCH_BEND => "Bend",
        wt::SRC_AFTERTOUCH => "AT",
        wt::SRC_SLIDE => "Slide",
        wt::SRC_FIXED => "Fixed",
        _ => "?",
    }
    .to_owned()
}

/// The matrix slots in use that read source `source`.
pub(super) fn slots_reading(panel: &Panel, source: usize) -> usize {
    (0..wt::MOD_SLOTS)
        .filter(|&slot| {
            let v = |o| panel.values()[wt::slot_param(slot, o)] as usize;
            v(wt::SLOT_SOURCE) == source && v(wt::SLOT_DEST) != wt::DEST_NONE
        })
        .count()
}

/// The handle to drag source `source` from onto the track's other devices
/// (see `crate::modulation`), if the synth exports it.
pub(super) fn export_handle(ui: &mut Ui, panel: &mut Panel, source: usize) {
    if let (Some(track), Some(output)) = (panel.track, wt::source_output(source)) {
        let source = project::ModSource::Instrument(output.id.to_owned());
        crate::modulation::source_handle(ui, panel.session, track, source);
    }
}

/// A draggable tile for source `source`: its name in its color, a bar of
/// where it is now, and how many slots read it. Drag it onto a knob to
/// route it there; `clicked` on the response is the caller's.
pub(super) fn source_tile(ui: &mut Ui, panel: &Panel, live: &Live, source: usize) -> Response {
    let name = short_name(source);
    let color = source_color(source);
    let id = panel.id(("source tile", source));
    let payload = SourceDrag {
        owner: panel.owner,
        source,
    };
    let used = slots_reading(panel, source);
    let value = live.source(source);
    let bipolar = wt::is_bipolar(source);
    let inner = ui.dnd_drag_source(id, payload, |ui| {
        let font = FontId::proportional(text::SMALL);
        let text_width =
            ui.fonts_mut(|f| f.layout_no_wrap(name.clone(), font.clone(), color).size().x);
        // Room for the count of slots reading it, at the top right.
        let count = if used > 0 { space::M } else { 0.0 };
        let size = vec2(text_width + 2.0 * space::S + count, size::CONTROL);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let painter = ui.painter_at(rect);
        let corner = CornerRadius::same(radius::CONTROL);
        painter.rect_filled(rect, corner, color.gamma_multiply(0.16));
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(stroke::HAIRLINE, color.gamma_multiply(0.55)),
            egui::StrokeKind::Inside,
        );
        painter.text(
            pos2(rect.left() + space::S, rect.center().y - 1.0),
            Align2::LEFT_CENTER,
            &name,
            font,
            color,
        );
        if used > 0 {
            painter.text(
                rect.right_top() + vec2(-space::S + 1.0, 0.0),
                Align2::RIGHT_TOP,
                format!("{used}"),
                FontId::proportional(text::TINY),
                ui.visuals().strong_text_color(),
            );
        }
        // Where the source is now, along the bottom edge.
        let bar = Rect::from_min_max(
            pos2(rect.left() + space::XS, rect.bottom() - 3.0),
            pos2(rect.right() - space::XS, rect.bottom() - 1.5),
        );
        let (from, to) = if bipolar {
            let middle = bar.center().x;
            let x = middle + value.clamp(-1.0, 1.0) * bar.width() / 2.0;
            (middle.min(x), middle.max(x))
        } else {
            (bar.left(), bar.left() + value.clamp(0.0, 1.0) * bar.width())
        };
        if to > from {
            painter.rect_filled(Rect::from_x_y_ranges(from..=to, bar.y_range()), 0.0, color);
        }
        response
    });
    let response = inner.inner | inner.response;
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
    response.on_hover_text(format!(
        "{}: drag onto a knob to modulate it{}",
        wt::SOURCES[source],
        if used > 0 {
            format!(" ({used} in use)")
        } else {
            String::new()
        }
    ))
}

/// Whether a source of this synth is being dragged.
pub(super) fn dragging_source(ui: &Ui, panel: &Panel) -> bool {
    egui::DragAndDrop::payload::<SourceDrag>(ui.ctx()).is_some_and(|d| d.owner == panel.owner)
}

/// The slots in use aimed at destination `dest`, bypassed ones included.
pub(super) fn slots_at(panel: &Panel, dest: usize) -> Vec<usize> {
    (0..wt::MOD_SLOTS)
        .filter(|&slot| {
            let v = |o| panel.values()[wt::slot_param(slot, o)] as usize;
            v(wt::SLOT_DEST) == dest && v(wt::SLOT_SOURCE) != wt::SRC_NONE
        })
        .collect()
}

/// Whether slot `slot` swings its destination both ways.
pub(super) fn slot_bipolar(panel: &Panel, slot: usize) -> bool {
    let v = |o| panel.values()[wt::slot_param(slot, o)] as usize;
    match v(wt::SLOT_POLARITY) {
        wt::POLARITY_UNIPOLAR => false,
        wt::POLARITY_BIPOLAR => true,
        _ => wt::is_bipolar(v(wt::SLOT_SOURCE)),
    }
}

/// The first slot with nothing in it, if there is one.
fn free_slot(panel: &Panel) -> Option<usize> {
    (0..wt::MOD_SLOTS).find(|&slot| {
        let v = |o| panel.values()[wt::slot_param(slot, o)] as usize;
        v(wt::SLOT_SOURCE) == wt::SRC_NONE || v(wt::SLOT_DEST) == wt::DEST_NONE
    })
}

/// Routes `source` to `dest` in a free slot, a quarter deep, as one undo
/// step; says so if every slot is taken or the route is there already.
pub(super) fn add_route(panel: &mut Panel, source: usize, dest: usize) {
    let exists = slots_at(panel, dest)
        .into_iter()
        .any(|slot| panel.values()[wt::slot_param(slot, wt::SLOT_SOURCE)] as usize == source);
    if exists {
        panel.session.info(format!(
            "{} already modulates {}",
            wt::SOURCES[source],
            wt::DESTINATIONS[dest]
        ));
        return;
    }
    let Some(slot) = free_slot(panel) else {
        panel
            .session
            .error("Every modulation slot is in use: free one in the Matrix");
        return;
    };
    let p = |offset| wt::slot_param(slot, offset);
    panel.set(p(wt::SLOT_SOURCE), source as f32);
    panel.set(p(wt::SLOT_DEST), dest as f32);
    panel.set(p(wt::SLOT_AMOUNT), 0.25);
    panel.set(p(wt::SLOT_AUX), wt::SRC_NONE as f32);
    panel.set(p(wt::SLOT_CURVE), 0.0);
    panel.set(p(wt::SLOT_POLARITY), wt::POLARITY_SOURCE as f32);
    panel.set(p(wt::SLOT_BYPASS), 0.0);
    panel.finish();
}

/// What a knob's menu can do to one of the slots aimed at it.
#[derive(Debug, Clone, Copy)]
enum SlotAction {
    Bypass(usize, bool),
    Polarity(usize, usize),
    Invert(usize),
    Remove(usize),
}

/// A knob for parameter `index`, named `label`, that shows the slots aimed
/// at it as rings (drag one to set its depth), where modulation has it now,
/// lights up while a source is dragged and takes a source dropped on it.
pub(super) fn mod_knob(
    ui: &mut Ui,
    panel: &mut Panel,
    live: &Live,
    index: usize,
    label: &str,
) -> Response {
    let param = panel.params[index];
    let value = panel.values()[index];
    let dest = wt::param_dest(index);
    let slots = dest.map(|d| slots_at(panel, d)).unwrap_or_default();
    let unit = wt::ring_unit(index);
    let rings: Vec<ModRing> = slots
        .iter()
        .map(|&slot| {
            let v = |o| panel.values()[wt::slot_param(slot, o)];
            let bypassed = v(wt::SLOT_BYPASS) >= 0.5;
            let color = source_color(v(wt::SLOT_SOURCE) as usize);
            ModRing {
                amount: v(wt::SLOT_AMOUNT) * unit,
                bipolar: slot_bipolar(panel, slot),
                color: if bypassed {
                    color.gamma_multiply(0.3)
                } else {
                    color
                },
            }
        })
        .collect();
    let normalize = |v: f32| match param.kind {
        ParameterKind::Enum { labels, .. } if labels.len() > 1 => v / (labels.len() - 1) as f32,
        _ => param.to_normalized(v),
    };
    let moved = live.param(panel, index);
    let live_dot = (!slots.is_empty() && (moved - value).abs() > 1e-5).then(|| normalize(moved));
    let droppable = dest.is_some() && dragging_source(ui, panel);
    let out = Knob::new(panel.id(("knob", index)), &param, value)
        .diameter(panel.knob_size)
        .label(label)
        .label_color(panel.name_color(index))
        .rings(&rings)
        .live(live_dot)
        .drop_highlight(droppable)
        .show(ui);
    if let Some(changed) = out.changed {
        panel.set(index, changed);
    }
    if let Some((ring, amount)) = out.ring_changed
        && let Some(&slot) = slots.get(ring)
        && unit != 0.0
    {
        panel.set(wt::slot_param(slot, wt::SLOT_AMOUNT), amount / unit);
    }
    if let (Some(dest), Some(drop)) = (dest, out.response.dnd_release_payload::<SourceDrag>())
        && drop.owner == panel.owner
    {
        add_route(panel, drop.source, dest);
    }
    attach(panel, &out.response, index, &slots, out.ring_menu);
    let response = out.response;
    if slots.is_empty() {
        response
    } else {
        let lines: Vec<String> = slots
            .iter()
            .map(|&slot| slot_summary(panel, slot))
            .collect();
        response.on_hover_text(lines.join("\n"))
    }
}

/// "LFO 1 +25%" for slot `slot`, with its scaling and state.
fn slot_summary(panel: &Panel, slot: usize) -> String {
    let v = |o| panel.values()[wt::slot_param(slot, o)];
    let source = wt::SOURCES[(v(wt::SLOT_SOURCE) as usize).min(wt::SOURCES.len() - 1)];
    let mut text = format!("{source} {:+.0}%", v(wt::SLOT_AMOUNT) * 100.0);
    let aux = v(wt::SLOT_AUX) as usize;
    if aux != wt::SRC_NONE {
        text += &format!(" × {}", wt::SOURCES[aux.min(wt::SOURCES.len() - 1)]);
    }
    if v(wt::SLOT_BYPASS) >= 0.5 {
        text += " (bypassed)";
    }
    text
}

/// What a knob's right-click menu, double-click and Ctrl+click do (as
/// `Panel::attach`), plus, for each slot aimed at it, a submenu to bypass,
/// invert, change the polarity of or remove it; the ring right-clicked
/// (`focus`) first.
fn attach(
    panel: &mut Panel,
    response: &Response,
    index: usize,
    slots: &[usize],
    focus: Option<usize>,
) {
    let param = panel.params[index];
    let mut menu = response.clone();
    menu.id = response.id.with("control menu");
    let (mut type_in, mut reset) = (false, false);
    let mut action = None;
    let mut order: Vec<usize> = slots.to_vec();
    if let Some(focus) = focus.filter(|f| *f < order.len()) {
        let first = order.remove(focus);
        order.insert(0, first);
    }
    let entries: Vec<(usize, String, bool, usize)> = order
        .iter()
        .map(|&slot| {
            let v = |o| panel.values()[wt::slot_param(slot, o)];
            (
                slot,
                slot_summary(panel, slot),
                v(wt::SLOT_BYPASS) >= 0.5,
                v(wt::SLOT_POLARITY) as usize,
            )
        })
        .collect();
    let target = AutomationTarget::InstrumentParam(param.id.to_owned());
    clicked_control_menu_with(panel.timeline, &menu, panel.track, target, |ui| {
        type_in = ui.button("Type Value…").clicked();
        reset = ui.button("Reset to Default").clicked();
        if type_in || reset {
            ui.close();
        }
        if !entries.is_empty() {
            ui.separator();
            crate::widgets::chrome::caption(ui, "Modulation");
            for (slot, summary, bypassed, polarity) in &entries {
                ui.menu_button(format!("Mod {}: {summary}", slot + 1), |ui| {
                    let bypass = if *bypassed { "Turn On" } else { "Bypass" };
                    if ui.button(bypass).clicked() {
                        action = Some(SlotAction::Bypass(*slot, !bypassed));
                        ui.close();
                    }
                    if ui.button("Invert").clicked() {
                        action = Some(SlotAction::Invert(*slot));
                        ui.close();
                    }
                    for (i, name) in wt::POLARITIES.iter().enumerate() {
                        if ui.selectable_label(*polarity == i, *name).clicked() {
                            action = Some(SlotAction::Polarity(*slot, i));
                            ui.close();
                        }
                    }
                    if ui.button("Remove").clicked() {
                        action = Some(SlotAction::Remove(*slot));
                        ui.close();
                    }
                });
            }
        }
        ui.separator();
    });
    if let Some(action) = action {
        match action {
            SlotAction::Bypass(slot, on) => {
                panel.set(
                    wt::slot_param(slot, wt::SLOT_BYPASS),
                    if on { 1.0 } else { 0.0 },
                );
            }
            SlotAction::Polarity(slot, polarity) => {
                panel.set(wt::slot_param(slot, wt::SLOT_POLARITY), polarity as f32);
            }
            SlotAction::Invert(slot) => {
                let amount = wt::slot_param(slot, wt::SLOT_AMOUNT);
                panel.set(amount, -panel.values()[amount]);
            }
            SlotAction::Remove(slot) => {
                panel.set(wt::slot_param(slot, wt::SLOT_SOURCE), wt::SRC_NONE as f32);
                panel.set(wt::slot_param(slot, wt::SLOT_DEST), wt::DEST_NONE as f32);
            }
        }
        panel.session.end_live("Change Modulation");
    }
    let value = panel.values()[index];
    let ended = response.drag_stopped() || (response.changed() && !response.dragged());
    let command_click = response.clicked() && response.ctx.input(|i| i.modifiers.command_only());
    if reset || command_click {
        panel.set(index, param.default_value());
        panel.session.end_live("Reset Parameter");
        return;
    }
    if type_in {
        value_entry::open(response, &param, value);
    }
    if let Some(typed) = value_entry::show(response, &param, value) {
        panel.set(index, typed);
        panel.finish();
    } else if ended {
        panel.finish();
    }
}

/// Knobs for these parameters, by index, with short names.
pub(super) fn knob_row(ui: &mut Ui, panel: &mut Panel, live: &Live, knobs: &[(usize, &str)]) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (index, label) in knobs {
            mod_knob(ui, panel, live, *index, label);
        }
    });
}

/// An Off/On parameter as a switch labelled `label`.
pub(super) fn switch(ui: &mut Ui, panel: &mut Panel, index: usize, label: &str) -> Response {
    panel.toggle_at(ui, index, label)
}

/// A small light as bright as `level` (0 to 1), in `color`.
pub(super) fn level_dot(ui: &mut Ui, level: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(space::M), Sense::hover());
    ui.painter().circle_filled(
        rect.center(),
        3.5,
        color.gamma_multiply(0.15 + 0.85 * level.clamp(0.0, 1.0)),
    );
}

/// What a choice control does besides choosing, for choices the panel
/// draws itself: the right-click menu (automate, link a controller) and
/// ending the gesture.
pub(super) fn attach_choice(panel: &mut Panel, response: &Response, index: usize) {
    let mut menu = response.clone();
    menu.id = response.id.with("control menu");
    let target = AutomationTarget::InstrumentParam(panel.params[index].id.to_owned());
    crate::timeline::clicked_control_menu(panel.timeline, &menu, panel.track, target);
    if response.changed() {
        panel.finish();
    }
}

/// A choice as a menu button showing `names` (one per label of the
/// parameter, or fewer, in which case the rest show the labels), in
/// columns of `per_column` when long.
pub(super) fn choice_menu(
    ui: &mut Ui,
    panel: &mut Panel,
    index: usize,
    names: &[&str],
    per_column: usize,
) -> Response {
    let ParameterKind::Enum { labels, .. } = panel.params[index].kind else {
        return ui.label("?");
    };
    let current = panel.values()[index] as usize;
    let name_of = |i: usize| names.get(i).or(labels.get(i)).copied().unwrap_or("?");
    let text = match panel.name_color(index) {
        Some(color) => RichText::new(name_of(current)).color(color),
        None => RichText::new(name_of(current)),
    };
    let mut chosen = None;
    let mut response = ui
        .menu_button(text, |ui| {
            ui.horizontal_top(|ui| {
                for column in (0..labels.len())
                    .collect::<Vec<_>>()
                    .chunks(per_column.max(1))
                {
                    ui.vertical(|ui| {
                        for &i in column {
                            if ui.selectable_label(current == i, name_of(i)).clicked() {
                                chosen = Some(i);
                                ui.close();
                            }
                        }
                    });
                }
            });
        })
        .response
        .on_hover_text(panel.params[index].name);
    if let Some(i) = chosen {
        panel.set(index, i as f32);
        response.mark_changed();
    }
    attach_choice(panel, &response, index);
    response
}
