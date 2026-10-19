//! The modulation matrix: all 64 slots as one list, as in Serum, scrolled
//! when the view is too short for them. Each row: the slot's number, its
//! switch (bypass), source, destination, amount (with a dot where the slot
//! is sending now), curve, polarity, the source that scales it, and a
//! button to clear it.

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Response, RichText, Sense, Shape, Stroke, Ui, pos2,
    vec2,
};
use instruments::builtin::wavetable as wt;

use super::super::Panel;
use super::knobs::{Live, attach_choice, source_color};
use crate::theme::{self, radius, size, space, stroke, text};
use crate::widgets::chrome::{self, Icon};

/// Oscillator `osc`'s destinations, in the destination menu's order.
const fn osc_destinations(osc: usize) -> [usize; 13] {
    let d = wt::osc_dest(osc);
    [
        d + wt::DEST_POSITION,
        d + wt::DEST_WARP,
        wt::warp2_dest(osc),
        wt::DEST_MORPH_A + osc,
        d + wt::DEST_LEVEL,
        d + wt::DEST_PAN,
        d + wt::DEST_PITCH,
        d + wt::DEST_FINE,
        d + wt::DEST_DETUNE,
        d + wt::DEST_BLEND,
        wt::DEST_WIDTH_A + osc,
        wt::DEST_WARP_SPREAD_A + osc,
        wt::DEST_POSITION_SPREAD_A + osc,
    ]
}

/// Every envelope's destinations, envelope by envelope: attack, decay,
/// sustain, release.
const ENVELOPE_DESTINATIONS: [usize; wt::ENV_COUNT * wt::ENV_DESTS] = {
    let mut out = [0; wt::ENV_COUNT * wt::ENV_DESTS];
    let mut i = 0;
    while i < out.len() {
        out[i] = wt::env_dest(i / wt::ENV_DESTS, i % wt::ENV_DESTS);
        i += 1;
    }
    out
};

/// An LFO's destinations: its rate, then phase, smooth, rise and delay.
const LFO_ROW: usize = 1 + wt::LFO_DESTS;

/// Every LFO's destinations, LFO by LFO (see [`LFO_ROW`]).
const LFO_DESTINATIONS: [usize; wt::LFO_COUNT * LFO_ROW] = {
    let mut out = [0; wt::LFO_COUNT * LFO_ROW];
    let mut i = 0;
    while i < out.len() {
        let (lfo, offset) = (i / LFO_ROW, i % LFO_ROW);
        out[i] = if offset == 0 {
            wt::lfo_rate_dest(lfo)
        } else {
            wt::lfo_dest(lfo, offset - 1)
        };
        i += 1;
    }
    out
};

/// The destination menu's groups: every destination once, grouped the way
/// Serum groups them, with each oscillator's second warp beside its first.
/// The groups in [`GRIDS`] lay out as grids, a row per envelope or LFO;
/// the rest are columns.
pub(super) const DESTINATION_GROUPS: [(&str, &[usize]); 8] = [
    ("Osc A", &osc_destinations(0)),
    ("Osc B", &osc_destinations(1)),
    ("Osc C", &osc_destinations(2)),
    (
        "Filters",
        &[
            wt::DEST_CUTOFF,
            wt::DEST_RESONANCE,
            wt::DEST_DRIVE,
            wt::DEST_FILTER_MIX,
            wt::DEST_FILTER_BLEND,
            wt::DEST_KEYTRACK,
            wt::DEST_CUTOFF2,
            wt::DEST_RESONANCE2,
            wt::DEST_DRIVE2,
            wt::DEST_FILTER2_MIX,
            wt::DEST_FILTER2_BLEND,
            wt::DEST_KEYTRACK2,
        ],
    ),
    (
        "Voice",
        &[
            wt::DEST_NONE,
            wt::DEST_GLOBAL_PITCH,
            wt::DEST_AMP,
            wt::DEST_VOLUME,
            wt::DEST_VOICE_PAN,
            wt::DEST_SUB_LEVEL,
            wt::DEST_NOISE_LEVEL,
        ],
    ),
    (
        "Macros",
        &[
            wt::DEST_MACRO1,
            wt::DEST_MACRO1 + 1,
            wt::DEST_MACRO1 + 2,
            wt::DEST_MACRO1 + 3,
            wt::DEST_MACRO1 + 4,
            wt::DEST_MACRO1 + 5,
            wt::DEST_MACRO1 + 6,
            wt::DEST_MACRO1 + 7,
        ],
    ),
    ("Envelopes", &ENVELOPE_DESTINATIONS),
    ("LFOs", &LFO_DESTINATIONS),
];

/// The groups of [`DESTINATION_GROUPS`] drawn as grids, and how many
/// destinations each of their rows holds.
const GRIDS: [(&str, usize); 2] = [("Envelopes", wt::ENV_DESTS), ("LFOs", LFO_ROW)];

/// Bipolar amounts below zero draw in this color; above, in the source's.
const NEGATIVE: Color32 = Color32::from_rgb(235, 150, 60);

pub(super) fn page(ui: &mut Ui, panel: &mut Panel, live: &Live) {
    let height = ui.available_height();
    egui::ScrollArea::vertical()
        .id_salt(panel.id("matrix scroll"))
        .max_height(height)
        .show(ui, |ui| {
            egui::Grid::new(panel.id("matrix"))
                .num_columns(10)
                .spacing([space::S, space::XS])
                .show(ui, |ui| {
                    for heading in [
                        "",
                        "",
                        "Source",
                        "Destination",
                        "Amount",
                        "Curve",
                        "",
                        "Scaled by",
                        "",
                    ] {
                        chrome::caption(ui, heading);
                    }
                    ui.end_row();
                    for slot in 0..wt::MOD_SLOTS {
                        row(ui, panel, live, slot);
                        ui.end_row();
                    }
                });
        });
}

fn row(ui: &mut Ui, panel: &mut Panel, live: &Live, slot: usize) {
    let p = |offset: usize| wt::slot_param(slot, offset);
    let source = panel.values()[p(wt::SLOT_SOURCE)] as usize;
    let used = source != wt::SRC_NONE && panel.values()[p(wt::SLOT_DEST)] as usize != wt::DEST_NONE;
    let number = RichText::new(format!("{}", slot + 1));
    ui.label(if used { number } else { number.weak() });
    bypass_button(ui, panel, slot);
    source_menu(ui, panel, p(wt::SLOT_SOURCE));
    destination_menu(ui, panel, slot);
    amount_bar(ui, panel, live, slot);
    curve_box(ui, panel, slot);
    polarity_button(ui, panel, slot);
    source_menu(ui, panel, p(wt::SLOT_AUX));
    let clear = ui
        .add_enabled_ui(used, |ui| {
            chrome::icon_button(ui, Icon::Remove, "Clear", "Clear the slot")
        })
        .inner;
    if clear.clicked() {
        for (offset, value) in [
            (wt::SLOT_SOURCE, 0.0),
            (wt::SLOT_DEST, 0.0),
            (wt::SLOT_AMOUNT, 0.0),
            (wt::SLOT_AUX, 0.0),
            (wt::SLOT_CURVE, 0.0),
            (wt::SLOT_POLARITY, 0.0),
            (wt::SLOT_BYPASS, 0.0),
        ] {
            panel.set(p(offset), value);
        }
        panel.session.end_live("Clear Modulation");
    }
}

/// The slot's switch: a light, lit while the slot works, dark while
/// bypassed.
fn bypass_button(ui: &mut Ui, panel: &mut Panel, slot: usize) {
    let index = wt::slot_param(slot, wt::SLOT_BYPASS);
    let bypassed = panel.values()[index] >= 0.5;
    let source = panel.values()[wt::slot_param(slot, wt::SLOT_SOURCE)] as usize;
    let (rect, response) = ui.allocate_exact_size(
        vec2(size::SMALL_CONTROL, size::SMALL_CONTROL),
        Sense::click(),
    );
    let color = if source == wt::SRC_NONE {
        ui.visuals().weak_text_color()
    } else {
        source_color(source)
    };
    let center = rect.center();
    if bypassed {
        ui.painter().circle_stroke(
            center,
            4.0,
            Stroke::new(stroke::HAIRLINE, color.gamma_multiply(0.6)),
        );
    } else {
        ui.painter().circle_filled(center, 4.0, color);
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, !bypassed, "Slot on")
    });
    let mut response = response.on_hover_text(if bypassed {
        "Bypassed: click to turn it back on"
    } else {
        "On: click to bypass"
    });
    if response.clicked() {
        panel.set(index, if bypassed { 0.0 } else { 1.0 });
        response.mark_changed();
    }
    attach_choice(panel, &response, index);
}

/// A source choice (a slot's source or its scaling) as a menu of the
/// source groups in columns, the name in the source's color.
fn source_menu(ui: &mut Ui, panel: &mut Panel, index: usize) {
    let current = panel.values()[index] as usize;
    let name = wt::SOURCES.get(current).copied().unwrap_or("?");
    let text = match (panel.name_color(index), current) {
        (Some(color), _) => RichText::new(name).color(color),
        (None, wt::SRC_NONE) => RichText::new(name),
        (None, source) => RichText::new(name).color(source_color(source)),
    };
    let mut chosen = None;
    let mut response = ui
        .menu_button(text, |ui| {
            if ui
                .selectable_label(current == wt::SRC_NONE, "None")
                .clicked()
            {
                chosen = Some(wt::SRC_NONE);
                ui.close();
            }
            ui.separator();
            ui.horizontal_top(|ui| {
                for (group, sources) in wt::SOURCE_GROUPS {
                    ui.vertical(|ui| {
                        chrome::caption(ui, group);
                        for &source in sources {
                            let label =
                                RichText::new(wt::SOURCES[source]).color(source_color(source));
                            if ui.selectable_label(current == source, label).clicked() {
                                chosen = Some(source);
                                ui.close();
                            }
                        }
                    });
                }
            });
        })
        .response
        .on_hover_text(panel.params[index].name);
    if let Some(source) = chosen {
        panel.set(index, source as f32);
        response.mark_changed();
    }
    attach_choice(panel, &response, index);
}

/// A slot's destination as a menu: a column per group, then the
/// envelopes' and LFOs' destinations as grids under them.
fn destination_menu(ui: &mut Ui, panel: &mut Panel, slot: usize) {
    let index = wt::slot_param(slot, wt::SLOT_DEST);
    let current = panel.values()[index] as usize;
    let name = wt::DESTINATIONS.get(current).copied().unwrap_or("?");
    let text = match panel.name_color(index) {
        Some(color) => RichText::new(name).color(color),
        None => RichText::new(name),
    };
    let mut chosen = None;
    let grid = |group: &str| GRIDS.iter().find(|(name, _)| *name == group).map(|g| g.1);
    let mut response = ui
        .menu_button(text, |ui| {
            ui.horizontal_top(|ui| {
                for (group, destinations) in DESTINATION_GROUPS {
                    if grid(group).is_some() {
                        continue;
                    }
                    ui.vertical(|ui| {
                        chrome::caption(ui, group);
                        for &dest in destinations {
                            let label = wt::DESTINATIONS[dest];
                            if ui.selectable_label(current == dest, label).clicked() {
                                chosen = Some(dest);
                                ui.close();
                            }
                        }
                    });
                }
            });
            ui.separator();
            ui.horizontal_top(|ui| {
                for (group, destinations) in DESTINATION_GROUPS {
                    let Some(width) = grid(group) else {
                        continue;
                    };
                    ui.vertical(|ui| {
                        chrome::caption(ui, group);
                        if let Some(dest) =
                            destination_grid(ui, group, destinations, width, current)
                        {
                            chosen = Some(dest);
                            ui.close();
                        }
                    });
                    ui.add_space(space::L);
                }
            });
        })
        .response
        .on_hover_text(panel.params[index].name);
    if let Some(dest) = chosen {
        panel.set(index, dest as f32);
        response.mark_changed();
    }
    attach_choice(panel, &response, index);
}

/// `destinations` as a grid of `width` to a row, each row named for its
/// envelope or LFO ("Amp", "LFO 3") and each cell for what it moves there
/// ("Decay", "Phase"); the cell's accessible name is the destination's
/// whole name. Returns the destination clicked, if any.
fn destination_grid(
    ui: &mut Ui,
    group: &str,
    destinations: &[usize],
    width: usize,
    current: usize,
) -> Option<usize> {
    let mut chosen = None;
    egui::Grid::new(("destination grid", group))
        .spacing([space::M, space::XS])
        .show(ui, |ui| {
            for row in destinations.chunks(width.max(1)) {
                let split = |dest: usize| {
                    let name = wt::DESTINATIONS[dest];
                    name.rsplit_once(' ').unwrap_or(("", name))
                };
                chrome::caption(ui, split(row[0]).0);
                for &dest in row {
                    let selected = current == dest;
                    let response = ui.selectable_label(selected, split(dest).1);
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            wt::DESTINATIONS[dest],
                        )
                    });
                    if response.clicked() {
                        chosen = Some(dest);
                    }
                }
                ui.end_row();
            }
        });
    chosen
}

/// A slot's amount as a bar from the middle, dragged sideways; the dot is
/// what the slot sends right now.
fn amount_bar(ui: &mut Ui, panel: &mut Panel, live: &Live, slot: usize) {
    let index = wt::slot_param(slot, wt::SLOT_AMOUNT);
    let amount = panel.values()[index];
    let source = panel.values()[wt::slot_param(slot, wt::SLOT_SOURCE)] as usize;
    let bypassed = panel.values()[wt::slot_param(slot, wt::SLOT_BYPASS)] >= 0.5;
    let (rect, response) =
        ui.allocate_exact_size(vec2(96.0, size::SMALL_CONTROL), Sense::click_and_drag());
    let visuals = ui.visuals().clone();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, radius::CONTROL, visuals.extreme_bg_color);
    let center = rect.center().x;
    let to_x = |v: f32| center + v.clamp(-1.0, 1.0) * rect.width() / 2.0;
    let color = if amount >= 0.0 {
        source_color(source)
    } else {
        NEGATIVE
    };
    let color = if bypassed {
        color.gamma_multiply(0.3)
    } else {
        color
    };
    painter.rect_filled(
        Rect::from_x_y_ranges(
            center.min(to_x(amount))..=center.max(to_x(amount)),
            rect.y_range(),
        ),
        radius::SMALL,
        color.gamma_multiply(0.7),
    );
    painter.vline(
        center,
        rect.y_range(),
        Stroke::new(stroke::HAIRLINE, visuals.weak_text_color()),
    );
    let now = live.modulation.slots[slot];
    if now != 0.0 && !bypassed {
        painter.circle_filled(
            pos2(to_x(now), rect.center().y),
            3.0,
            visuals.strong_text_color(),
        );
    }
    painter.text(
        rect.right_center() - vec2(space::S, 0.0),
        Align2::RIGHT_CENTER,
        format!("{:+.0}%", amount * 100.0),
        FontId::monospace(text::TINY),
        visuals.text_color(),
    );
    let mut response = response;
    if response.dragged() {
        let fine = ui.input(|i| i.modifiers.shift || i.modifiers.ctrl);
        let scale = if fine { 0.2 } else { 2.0 };
        let moved = amount + response.drag_delta().x / rect.width() * scale;
        panel.set(index, moved);
        response.mark_changed();
    }
    panel.attach(&response, index);
}

/// A slot's curve, drawn as the response it gives from 0 to full; drag up
/// to bend it up (fast start), down to bend it down (slow start).
fn curve_box(ui: &mut Ui, panel: &mut Panel, slot: usize) {
    let index = wt::slot_param(slot, wt::SLOT_CURVE);
    let curve = panel.values()[index];
    let (rect, response) =
        ui.allocate_exact_size(vec2(30.0, size::SMALL_CONTROL), Sense::click_and_drag());
    #[cfg(test)]
    ui.ctx()
        .data_mut(|d| d.insert_temp(panel.id(("curve", slot)).with("rect"), rect));
    let visuals = ui.visuals().clone();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, radius::CONTROL, visuals.extreme_bg_color);
    let area = rect.shrink2(vec2(space::S, space::XS));
    let points: Vec<Pos2> = (0..=16)
        .map(|i| {
            let x = i as f32 / 16.0;
            let y = wt::matrix_curve(x, curve);
            pos2(
                area.left() + x * area.width(),
                area.bottom() - y * area.height(),
            )
        })
        .collect();
    let color = if curve == 0.0 {
        visuals.weak_text_color()
    } else {
        theme::accent(&visuals)
    };
    painter.add(Shape::line(points, Stroke::new(1.4, color)));
    let mut response = response.on_hover_text(format!("Curve {:+.0}%", curve * 100.0));
    if response.dragged() {
        let fine = ui.input(|i| i.modifiers.shift || i.modifiers.ctrl);
        let scale = if fine { 0.002 } else { 0.02 };
        panel.set(index, curve + response.drag_delta().y * scale);
        response.mark_changed();
    }
    panel.attach(&response, index);
}

/// A slot's polarity as a button that steps through the choices: `±` for
/// a source swinging both ways, `+` for one from 0 up, dimmed while it is
/// just the source's own.
fn polarity_button(ui: &mut Ui, panel: &mut Panel, slot: usize) -> Response {
    let index = wt::slot_param(slot, wt::SLOT_POLARITY);
    let values = panel.values();
    let polarity = values[index] as usize;
    let source = values[wt::slot_param(slot, wt::SLOT_SOURCE)] as usize;
    let bipolar = match polarity {
        wt::POLARITY_UNIPOLAR => false,
        wt::POLARITY_BIPOLAR => true,
        _ => wt::is_bipolar(source),
    };
    let text = RichText::new(if bipolar { "±" } else { "+" }).monospace();
    let text = if polarity == wt::POLARITY_SOURCE {
        text.weak()
    } else {
        text.strong()
    };
    let mut response = ui
        .selectable_label(polarity != wt::POLARITY_SOURCE, text)
        .on_hover_text(format!(
            "Polarity: {}",
            wt::POLARITIES[polarity.min(wt::POLARITIES.len() - 1)]
        ));
    if response.clicked() {
        let next = (polarity + 1) % wt::POLARITIES.len();
        panel.set(index, next as f32);
        response.mark_changed();
    }
    panel.attach(&response, index);
    response
}
