//! The two filters: tabs choosing whose controls show, one display with
//! both curves where modulation (and key tracking, for the newest note)
//! has them now, and which source goes into which filter.

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, RichText, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2,
};
use instruments::builtin::wavetable as wt;

use super::super::Panel;
use super::super::panel::display;
use super::knobs::{self, Live, attach_choice, knob_row, mod_knob};
use crate::theme::{self, space, stroke, text};
use crate::widgets::spectrum::Axes;
use crate::widgets::{chrome, knob};

/// Filter 2's color in the filter display; filter 1 has the accent.
const SECOND_FILTER: Color32 = Color32::from_rgb(235, 150, 60);

/// The filter types by family, as the type menu lists them.
const FAMILIES: [(&str, &[usize]); 5] = [
    (
        "Low pass",
        &[wt::LP12, wt::LP24, wt::LADDER, wt::ACID, wt::DIRTY],
    ),
    ("High pass", &[wt::HP12, wt::HP24]),
    ("Band", &[wt::BP12, wt::BP24, wt::NOTCH, wt::MORPH]),
    ("EQ", &[wt::PEAK, wt::LOW_SHELF, wt::HIGH_SHELF]),
    (
        "Effects",
        &[
            wt::COMB_POS,
            wt::COMB_NEG,
            wt::FLANGER,
            wt::PHASER,
            wt::FORMANT,
        ],
    ),
];

/// What the blend knob does for filter type `kind`, as its name; `None`
/// when the type doesn't use it.
fn blend_name(kind: usize) -> Option<&'static str> {
    match kind {
        wt::PEAK | wt::LOW_SHELF | wt::HIGH_SHELF => Some("Gain"),
        wt::MORPH => Some("Morph"),
        wt::PHASER => Some("Spread"),
        wt::FLANGER => Some("Polarity"),
        _ => None,
    }
}

/// The filters' card: which filter it shows and whether it's on, its type
/// and the routing, both curves as large as the height allows, and the
/// selected one's knobs three to a row.
pub(super) fn section(ui: &mut Ui, panel: &mut Panel, live: &Live, height: f32) {
    let selected_id = panel.id("filter");
    let mut selected: usize = ui.data(|d| d.get_temp(selected_id)).unwrap_or(0);
    let width = 3.0 * knob::compact_width(knob::COMPACT);
    chrome::section(ui, theme::section::FILTER, |ui| {
        ui.set_width(width);
        chrome::section_header(ui, "Filter", theme::section::FILTER, |ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            for filter in 0..2 {
                let on = panel.values()[wt::filter_param(filter, wt::FILTER_ON)] >= 0.5;
                let text = RichText::new(format!("{}", filter + 1));
                let text = if on { text.strong() } else { text.weak() };
                let name = format!("Filter {}", filter + 1);
                let tab = ui.selectable_label(selected == filter, text);
                tab.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::SelectableLabel,
                        true,
                        selected == filter,
                        &name,
                    )
                });
                if tab.on_hover_text(&name).clicked() {
                    selected = filter;
                }
            }
            knobs::switch(ui, panel, wt::filter_param(selected, wt::FILTER_ON), "On");
        });
        {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::XS;
                type_menu(ui, panel, wt::filter_param(selected, wt::FILTER_TYPE));
                routing_menu(ui, panel);
            });
            let f = |index: usize| wt::filter_param(selected, index);
            let display = super::display_height(height, 1, 2);
            filter_display(ui, panel, live, selected, vec2(width, display));
            let kind = panel.values()[f(wt::FILTER_TYPE)] as usize;
            let compact = std::mem::replace(&mut panel.compact, true);
            knob_row(
                ui,
                panel,
                live,
                &[
                    (f(wt::FILTER_CUTOFF), "Cutoff"),
                    (f(wt::FILTER_RESONANCE), "Res"),
                    (f(wt::FILTER_DRIVE), "Drive"),
                ],
            );
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                mod_knob(ui, panel, live, f(wt::FILTER_KEYTRACK), "Key");
                mod_knob(ui, panel, live, f(wt::FILTER_MIX), "Mix");
                let name = blend_name(kind);
                ui.add_enabled_ui(name.is_some(), |ui| {
                    mod_knob(
                        ui,
                        panel,
                        live,
                        f(wt::FILTER_BLEND),
                        name.unwrap_or("Blend"),
                    );
                });
            });
            panel.compact = compact;
        }
    });
    ui.data_mut(|d| d.insert_temp(selected_id, selected));
}

/// The filter type as a menu of families, in columns.
fn type_menu(ui: &mut Ui, panel: &mut Panel, index: usize) {
    let current = panel.values()[index] as usize;
    let name = wt::FILTER_TYPES.get(current).copied().unwrap_or("?");
    let text = match panel.name_color(index) {
        Some(color) => RichText::new(name).color(color),
        None => RichText::new(name),
    };
    let mut chosen = None;
    let mut response = ui
        .menu_button(text, |ui| {
            ui.horizontal_top(|ui| {
                for (family, kinds) in FAMILIES {
                    ui.vertical(|ui| {
                        chrome::caption(ui, family);
                        for &kind in kinds {
                            if ui
                                .selectable_label(current == kind, wt::FILTER_TYPES[kind])
                                .clicked()
                            {
                                chosen = Some(kind);
                                ui.close();
                            }
                        }
                    });
                }
            });
        })
        .response
        .on_hover_text(panel.params[index].name);
    if let Some(kind) = chosen {
        panel.set(index, kind as f32);
        response.mark_changed();
    }
    attach_choice(panel, &response, index);
}

/// Filter `filter`'s settings where modulation has them now, as parameter
/// values the response math reads: the cutoff with key tracking for the
/// newest note, the resonance, mix and blend.
fn live_values(panel: &Panel, live: &Live, filter: usize) -> Vec<f32> {
    let mut values = panel.values().to_vec();
    let p = |index: usize| wt::filter_param(filter, index);
    for index in [
        wt::FILTER_CUTOFF,
        wt::FILTER_RESONANCE,
        wt::FILTER_MIX,
        wt::FILTER_BLEND,
    ] {
        values[p(index)] = live.param(panel, p(index));
    }
    let keytrack = values[p(wt::FILTER_KEYTRACK)];
    let octaves = keytrack * (live.pitch() - 60.0) / 12.0;
    values[p(wt::FILTER_CUTOFF)] =
        (values[p(wt::FILTER_CUTOFF)] * octaves.exp2()).clamp(20.0, 20_000.0);
    values
}

/// The filters' two curves, filter 1's in the accent color and filter 2's
/// in orange, the selected one in front and brighter; a faint copy shows
/// where the selected one is set when modulation has moved it. Drag
/// sideways for the selected filter's cutoff, up or down for its resonance.
fn filter_display(ui: &mut Ui, panel: &mut Panel, live: &Live, selected: usize, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    let axes = Axes {
        rect: response.rect.shrink2(vec2(space::S, space::S + space::XS)),
        low_hz: 20.0,
        high_hz: 20_000.0,
        floor_db: -36.0,
        ceiling_db: 24.0,
    };
    let visuals = ui.visuals().clone();
    axes.paint_grid(&painter, &visuals, 0.0);
    painter.hline(
        axes.rect.x_range(),
        axes.y(0.0),
        Stroke::new(
            stroke::HAIRLINE,
            visuals.weak_text_color().gamma_multiply(0.3),
        ),
    );
    let rate = panel.sample_rate;
    let curve = |filter: usize, values: &[f32]| -> Vec<Pos2> {
        let mut x = axes.rect.left();
        let mut points = Vec::new();
        while x <= axes.rect.right() {
            let db = wt::filter_curve_db(filter, values, axes.hz(x), rate);
            points.push(pos2(
                x,
                axes.y(db).clamp(axes.rect.top(), axes.rect.bottom()),
            ));
            x += 2.0;
        }
        points
    };
    let values = panel.values().to_vec();
    let colors = [theme::accent(&visuals), SECOND_FILTER];
    for filter in [1 - selected, selected] {
        let on = values[wt::filter_param(filter, wt::FILTER_ON)] >= 0.5;
        let front = filter == selected;
        if !on && !front {
            continue;
        }
        let moved = live_values(panel, live, filter);
        let cutoff = wt::filter_param(filter, wt::FILTER_CUTOFF);
        if front && (moved[cutoff] / values[cutoff] - 1.0).abs() > 0.01 {
            painter.add(Shape::line(
                curve(filter, &values),
                Stroke::new(stroke::HAIRLINE, colors[filter].gamma_multiply(0.35)),
            ));
        }
        let color = match (on, front) {
            (false, _) => visuals.weak_text_color(),
            (true, true) => colors[filter],
            (true, false) => colors[filter].gamma_multiply(0.55),
        };
        let line = curve(filter, &moved);
        if on && front {
            // Filled down to the floor, so the curve reads as a shape.
            for pair in line.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                painter.add(Shape::convex_polygon(
                    vec![
                        a,
                        b,
                        pos2(b.x, axes.rect.bottom()),
                        pos2(a.x, axes.rect.bottom()),
                    ],
                    color.gamma_multiply(0.1),
                    Stroke::NONE,
                ));
            }
        }
        painter.add(Shape::line(
            line,
            Stroke::new(if front { 1.8 } else { 1.2 }, color),
        ));
        if on {
            // A numbered dot where the filter's cutoff is.
            let x = axes.x(moved[cutoff]);
            let y = axes.y(wt::filter_curve_db(filter, &moved, moved[cutoff], rate));
            let dot = pos2(x, y.clamp(axes.rect.top() + 7.0, axes.rect.bottom() - 2.0));
            painter.circle_filled(dot, 5.5, color);
            painter.text(
                dot,
                Align2::CENTER_CENTER,
                format!("{}", filter + 1),
                FontId::proportional(text::TINY),
                visuals.extreme_bg_color,
            );
        }
    }
    if response.dragged() {
        let delta = response.drag_delta();
        let cutoff = wt::filter_param(selected, wt::FILTER_CUTOFF);
        panel.set(cutoff, values[cutoff] * (delta.x / 60.0).exp2());
        let resonance = wt::filter_param(selected, wt::FILTER_RESONANCE);
        panel.set(resonance, values[resonance] - delta.y / 150.0);
    }
    if response.drag_stopped() {
        panel.finish();
    }
    response.on_hover_text("Drag sideways for the cutoff, up or down for resonance");
}

/// Which filter each source goes into: a row per filter, a switch per
/// source. A source is in one filter or neither, so switching it on in
/// one row takes it out of the other; switching it off sends it past both.
/// Beside them, whether filter 1 feeds filter 2 or the two run side by side.
/// The routing in a menu that stays open while sources are switched, until
/// a click outside it.
fn routing_menu(ui: &mut Ui, panel: &mut Panel) {
    let config = egui::containers::menu::MenuConfig::new()
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    egui::containers::menu::MenuButton::new("Routing")
        .config(config)
        .ui(ui, |ui| routing(ui, panel))
        .0
        .on_hover_text("Which sources go through which filter, and how the two connect");
}

fn routing(ui: &mut Ui, panel: &mut Panel) {
    /// In the order shown, with each one's place in `SOURCE_ROUTES`.
    const SOURCES: [(&str, usize); 5] = [
        ("A", wt::OSC_ROUTES[0]),
        ("B", wt::OSC_ROUTES[1]),
        ("C", wt::OSC_ROUTES[2]),
        ("Sub", wt::SUB_ROUTE),
        ("Noise", wt::NOISE_ROUTE),
    ];
    ui.horizontal_top(|ui| {
        egui::Grid::new(panel.id("routing"))
            .spacing([space::XS, space::XS])
            .show(ui, |ui| {
                for filter in 0..2 {
                    ui.weak(format!("F{}", filter + 1));
                    for (name, route) in SOURCES {
                        let (switch, target) = wt::SOURCE_ROUTES[route];
                        let values = panel.values();
                        let here = values[switch] >= 0.5 && values[target] as usize == filter;
                        let response = ui.selectable_label(here, name).on_hover_text(format!(
                            "{} into filter {}",
                            panel.params[switch].name.trim_start_matches("Filter "),
                            filter + 1
                        ));
                        if response.clicked() {
                            if here {
                                panel.set(switch, 0.0);
                            } else {
                                panel.set(switch, 1.0);
                                panel.set(target, filter as f32);
                            }
                            panel.finish();
                        }
                    }
                    ui.end_row();
                }
            });
        ui.vertical(|ui| {
            let routing = panel.params[wt::FILTER_ROUTING].id;
            panel.choice(ui, routing);
        });
    });
}
