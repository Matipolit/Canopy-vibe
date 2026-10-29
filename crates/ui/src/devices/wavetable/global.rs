//! What stays at the right on every page (the eight macros, each with a
//! tile to drag it onto a knob, the output's scope and level, the volume),
//! and the Voicing page: poly, mono or legato, voices, glide and how it
//! moves, the wheels and velocity, the quality, and each oscillator's
//! unison and key settings.

use eframe::egui::{self, Align2, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use instruments::builtin::wavetable as wt;

use super::super::Panel;
use super::super::panel::display;
use super::knobs::{self, Live, choice_menu, knob_row, mod_knob, source_tile};
use super::oscillators::{OSC_NAMES, accent};
use crate::theme::{self, radius, space, stroke, text};
use crate::widgets::{chrome, knob};

/// The macros, two to a row, and under them the output's scope and the
/// volume: a narrow column at the right of every page. Each macro's name
/// is a tile to drag it onto a knob.
pub(super) fn side(ui: &mut Ui, panel: &mut Panel, live: &Live, height: f32) {
    let compact = std::mem::replace(&mut panel.compact, true);
    let width = 2.0 * knob::compact_width(knob::COMPACT);
    chrome::section(ui, theme::section::MACRO, |ui| {
        ui.set_width(width);
        chrome::section_header(ui, "Macros", theme::section::MACRO, |_| {});
        {
            // Each macro's tile names it (and drags it onto a knob), in the
            // place of its knob's name; the knob's value is in its tooltip.
            for row in 0..wt::MACRO_COUNT / 2 {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for m in [2 * row, 2 * row + 1] {
                        let knob = mod_knob(ui, panel, live, wt::macro_param(m), "");
                        let name = Rect::from_min_size(
                            knob.rect.min,
                            vec2(knob.rect.width(), knob::LABEL_HEIGHT),
                        );
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(name)
                                .layout(egui::Layout::top_down(egui::Align::Center)),
                            |ui| source_tile(ui, panel, live, wt::macro_source(m)),
                        );
                    }
                });
            }
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if height >= SCOPE_ROOM {
                    scope(
                        ui,
                        panel,
                        vec2(
                            knob::compact_width(knob::COMPACT),
                            knob::compact_height(knob::COMPACT),
                        ),
                    );
                }
                mod_knob(ui, panel, live, wt::MASTER_VOLUME, "Volume");
            });
        }
    });
    panel.compact = compact;
}

/// How tall the page must be for the scope to show beside the volume.
const SCOPE_ROOM: f32 = 300.0;

/// One cycle of the output at the newest note's pitch (the synth's scope
/// meters, drawn as a smooth line), and its level up the right edge.
fn scope(ui: &mut Ui, panel: &Panel, size: egui::Vec2) {
    let (response, painter) = display(ui, size, Sense::hover());
    let visuals = ui.visuals().clone();
    let inner = response.rect.shrink(space::S);
    let meter = Rect::from_min_max(pos2(inner.right() - 3.0, inner.top()), inner.right_bottom());
    let rect = Rect::from_min_max(inner.min, pos2(meter.left() - space::S, inner.bottom()));
    painter.hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(
            stroke::HAIRLINE,
            visuals.weak_text_color().gamma_multiply(0.2),
        ),
    );
    let points: Vec<f32> = (0..wt::SCOPE_POINTS)
        .map(|i| panel.meters[wt::METER_SCOPE + i])
        .collect();
    let peak = points.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    if peak > 1e-4 {
        // Scaled to fill the height, so quiet sounds still show their shape.
        let scale = 1.0 / peak.max(0.25);
        let n = points.len();
        let at = |i: isize| points[i.clamp(0, n as isize - 1) as usize];
        let steps = 64;
        let line: Vec<Pos2> = (0..=steps)
            .map(|s| {
                // Catmull-Rom through the points.
                let x = s as f32 / steps as f32 * (n - 1) as f32;
                let i = x.floor() as isize;
                let t = x - i as f32;
                let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
                let y = 0.5
                    * (2.0 * p1
                        + (p2 - p0) * t
                        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
                        + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t * t * t);
                pos2(
                    rect.left() + rect.width() * s as f32 / steps as f32,
                    rect.center().y - (y * scale).clamp(-1.0, 1.0) * rect.height() / 2.0,
                )
            })
            .collect();
        painter.add(Shape::line(line, Stroke::new(1.5, accent(ui))));
    }
    // The level, in dB from -48 up.
    let level = panel.meters[wt::METER_LEVEL];
    let db = 20.0 * level.max(1e-6).log10();
    let fill = ((db + 48.0) / 54.0).clamp(0.0, 1.0);
    painter.rect_filled(meter, radius::SMALL, visuals.widgets.inactive.bg_fill);
    if fill > 0.0 {
        let color = if db > 0.0 {
            visuals.error_fg_color
        } else {
            accent(ui)
        };
        painter.rect_filled(
            Rect::from_min_max(
                pos2(meter.left(), meter.bottom() - fill * meter.height()),
                meter.max,
            ),
            radius::SMALL,
            color,
        );
    }
    response.on_hover_text(format!(
        "Output: one cycle of the newest note; peak {db:.1} dB"
    ));
}

/// The Voicing page: cards for the voices, the glide (with its curve), how
/// notes play it (bend, velocity, volume), and each oscillator's unison,
/// key and cross-modulation settings.
pub(super) fn page(ui: &mut Ui, panel: &mut Panel, live: &Live) {
    ui.spacing_mut().item_spacing.x = space::M;
    let compact = std::mem::replace(&mut panel.compact, true);
    let neutral = theme::section::NEUTRAL;
    chrome::section(ui, theme::section::NOTE, |ui| {
        chrome::section_header(ui, "Voices", theme::section::NOTE, |_| {});
        let mode = panel.params[wt::VOICE_MODE].id;
        panel.choice(ui, mode);
        ui.horizontal(|ui| {
            ui.weak("Polyphony");
            choice_menu(ui, panel, wt::POLYPHONY, &[], 8);
        });
        ui.horizontal(|ui| {
            ui.weak("Quality");
            let quality = panel.params[wt::QUALITY].id;
            if let Some(response) = panel.choice(ui, quality) {
                response.on_hover_text(
                    "High renders the oscillators at twice the rate: less aliasing from hard \
                     warps and FM, for about twice their cost",
                );
            }
        });
    });
    chrome::section(ui, theme::section::CONTROL, |ui| {
        chrome::section_header(ui, "Expression", theme::section::CONTROL, |_| {});
        knob_row(
            ui,
            panel,
            live,
            &[(wt::BEND_RANGE, "Bend"), (wt::VELOCITY_SENS, "Velocity")],
        );
    });
    chrome::section(ui, neutral, |ui| {
        chrome::section_header(ui, "Glide", neutral, |ui| {
            knobs::switch(ui, panel, wt::GLIDE_ALWAYS, "Always")
                .on_hover_text("Glide on every note; off, only from a key still held");
            knobs::switch(ui, panel, wt::GLIDE_SCALED, "Scaled")
                .on_hover_text("The time is per octave: short steps glide faster");
        });
        glide_curve(ui, panel);
        knob_row(
            ui,
            panel,
            live,
            &[(wt::GLIDE, "Time"), (wt::GLIDE_CURVE, "Curve")],
        );
    });
    chrome::section(ui, theme::section::OSC_A, |ui| {
        chrome::section_header(ui, "Oscillators", theme::section::OSC_A, |_| {});
        egui::Grid::new(panel.id("osc settings"))
            .spacing(ui.spacing().item_spacing)
            .show(ui, |ui| {
                ui.weak("");
                ui.weak("Unison detune");
                ui.weak("Keys");
                ui.weak("FM/AM/RM from");
                ui.end_row();
                for (osc, name) in OSC_NAMES.iter().enumerate() {
                    let p = |offset: usize| wt::osc_param(osc, offset);
                    ui.label(
                        egui::RichText::new(format!("Osc {name}"))
                            .color(super::oscillators::osc_color(osc)),
                    );
                    choice_menu(ui, panel, p(wt::OSC_DETUNE_MODE), &[], 5);
                    knobs::switch(ui, panel, p(wt::OSC_KEYTRACK), "Track")
                        .on_hover_text("Off, it plays C4 whatever the key");
                    choice_menu(ui, panel, p(wt::OSC_MOD_SOURCE), &[], 6);
                    ui.end_row();
                }
            });
        ui.weak("Unison detune: how the voices spread over the detune");
    });
    panel.compact = compact;
}

/// The glide's shape from the last note (bottom) to the next (top), as
/// the curve knob bends it.
fn glide_curve(ui: &mut Ui, panel: &Panel) {
    let (response, painter) = display(
        ui,
        vec2(
            2.0 * knob::compact_width(knob::COMPACT),
            knob::compact_height(knob::COMPACT),
        ),
        Sense::hover(),
    );
    let rect = response.rect.shrink(space::S);
    let curve = panel.values()[wt::GLIDE_CURVE];
    let line: Vec<Pos2> = (0..=32)
        .map(|i| {
            let t = i as f32 / 32.0;
            pos2(
                rect.left() + t * rect.width(),
                rect.bottom() - dsp::stage_shape(t, curve) * rect.height(),
            )
        })
        .collect();
    painter.add(Shape::line(line, Stroke::new(1.5, accent(ui))));
    painter.text(
        response.rect.right_bottom() - vec2(space::S, space::XS),
        Align2::RIGHT_BOTTOM,
        "glide",
        FontId::proportional(text::TINY),
        ui.visuals().weak_text_color(),
    );
}
