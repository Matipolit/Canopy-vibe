//! The chorus's panel: what the sweep does to the sound right now (each
//! side's response at the LFO's position, over the stretch the sweep moves
//! through), the LFO's cycle with where each side is in it, and the mode
//! switches and knobs beside them.

use effects::chorus::{self, CHORUS, FLANGER, NOTES, PHASER};
use eframe::egui::{self, Align2, FontId, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};

use super::Panel;
use super::panel::display;

/// The response's range in dB: room above 0 for the feedback's
/// resonances, and enough below for the notches to read as notches.
const TOP_DB: f32 = 12.0;
const FLOOR_DB: f32 = -30.0;

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    // Room for the switches above the knobs.
    let switches = crate::theme::size::CONTROL + crate::theme::space::M;
    let height = 2.0 * panel.fit_knobs_beside(ui, 2, switches) + switches;
    let mode = panel.value("mode") as usize;
    ui.horizontal_top(|ui| {
        sweep_display(ui, panel, vec2(height * 1.7, height));
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                panel.choice(ui, "mode");
                ui.separator();
                panel.choice(ui, "sync");
                ui.separator();
                panel.choice(ui, "shape");
            });
            let synced = panel.value("sync") < 0.5;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                if synced {
                    panel.knob(ui, "note");
                } else {
                    panel.knob_as(ui, "rate", Some("Rate"));
                }
                panel.knobs(ui, &["depth", "spread"]);
                if mode == PHASER {
                    panel.knob(ui, "stages");
                }
            });
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                let tone: &[&str] = match mode {
                    CHORUS => &["delay", "voices"],
                    FLANGER => &["delay", "feedback"],
                    _ => &["center", "feedback"],
                };
                panel.knobs(ui, tone);
                panel.knobs(ui, &["mix", "output"]);
            });
        });
    });
}

/// `hz` as the display labels it.
fn hz_label(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.1} kHz", hz / 1000.0)
    } else {
        format!("{hz:.0} Hz")
    }
}

/// `ms` as the display labels it.
fn ms_label(ms: f32) -> String {
    if ms >= 10.0 {
        format!("{ms:.1}")
    } else {
        format!("{ms:.2}")
    }
}

/// Above: the response of each side (left bright, right faint) at the
/// LFO's position, on 20 Hz to 20 kHz, with the stretch the sweep moves
/// through shaded (the phaser's corner, the chorus's and flanger's lowest
/// notch). Below: one cycle of the LFO and where each side is in it. Drag
/// sideways to move the sweep, up or down for the depth.
fn sweep_display(ui: &mut Ui, panel: &mut Panel, size: egui::Vec2) {
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    let full = response.rect.shrink2(vec2(8.0, 8.0));
    let strip = (full.height() * 0.2).clamp(12.0, 28.0);
    let curves = Rect::from_min_max(
        full.min + vec2(0.0, 12.0),
        pos2(full.right(), full.bottom() - strip - 12.0),
    );
    let cycle = Rect::from_min_max(pos2(full.left(), full.bottom() - strip), full.max);
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let weak = visuals.weak_text_color();

    let values = panel.values().to_vec();
    let sample_rate = panel.sample_rate;
    let mode = panel.value("mode") as usize;
    let bpm = panel.session.project.tempo.tempo.to_bpm();
    let rate = chorus::lfo_rate(&values, bpm) as f32;
    let (left, right) = (panel.meters[0], panel.meters[1]);

    let span = 1000f32.ln();
    let x_of = |hz: f32| curves.left() + (hz.max(1.0) / 20.0).ln() / span * curves.width();
    let hz_at = |x: f32| 20.0 * ((x - curves.left()) / curves.width() * span).exp();
    let y_of = |db: f32| {
        curves.top() + (TOP_DB - db.clamp(FLOOR_DB, TOP_DB)) / (TOP_DB - FLOOR_DB) * curves.height()
    };

    // Grid: 0 and -12 dB, and the decades.
    let grid = Stroke::new(1.0, weak.gamma_multiply(0.2));
    for db in [0.0, -12.0] {
        painter.hline(curves.x_range(), y_of(db), grid);
    }
    for (hz, text) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
        painter.vline(x_of(hz), curves.y_range(), grid);
        painter.text(
            pos2(x_of(hz) + 2.0, curves.bottom() + 1.0),
            Align2::LEFT_TOP,
            text,
            FontId::proportional(crate::theme::text::TINY),
            weak.gamma_multiply(0.8),
        );
    }

    // Where the sweep reaches, shaded: the phaser's corner, or the lowest
    // notch of the comb (half a cycle at the delay).
    let (low, high) = chorus::sweep(&values, rate, sample_rate);
    let (from, to) = if mode == PHASER {
        (low, high)
    } else {
        (500.0 / high, 500.0 / low)
    };
    painter.rect_filled(
        Rect::from_x_y_ranges(
            x_of(from)..=x_of(to).max(x_of(from) + 1.5),
            curves.y_range(),
        ),
        0.0,
        accent.gamma_multiply(0.08),
    );

    // The responses: one point a pixel, right side under the left, which is
    // filled below so a dense comb reads as a band.
    let columns = curves.width().max(2.0) as usize;
    let response_at = |phase: f32| -> Vec<egui::Pos2> {
        (0..=columns)
            .map(|i| {
                let x = curves.left() + curves.width() * i as f32 / columns as f32;
                let gain = chorus::response(&values, phase, rate, hz_at(x), sample_rate);
                pos2(x, y_of(dsp::gain_to_db(gain)))
            })
            .collect()
    };
    let (right_curve, left_curve) = (response_at(right), response_at(left));
    let mut fill = egui::Mesh::default();
    for pair in left_curve.windows(2) {
        fill.add_colored_rect(
            Rect::from_min_max(pos2(pair[0].x, pair[0].y), pos2(pair[1].x, curves.bottom())),
            accent.gamma_multiply(0.14),
        );
    }
    painter.add(fill);
    painter.add(Shape::line(
        right_curve,
        Stroke::new(1.2, weak.gamma_multiply(0.9)),
    ));
    painter.add(Shape::line(left_curve, Stroke::new(1.6, accent)));

    // One cycle of the LFO, and where each side is.
    let triangle = panel.value("shape");
    let wave = |phase: f32| {
        let sine = (std::f32::consts::TAU * phase).sin();
        let straight = 4.0 * ((phase + 0.75).fract() - 0.5).abs() - 1.0;
        sine + triangle * (straight - sine)
    };
    let x_in = |phase: f32| cycle.left() + phase * cycle.width();
    let y_in = |lfo: f32| cycle.center().y - lfo * cycle.height() / 2.0;
    painter.hline(cycle.x_range(), cycle.center().y, grid);
    let points: Vec<egui::Pos2> = (0..=96)
        .map(|i| {
            let phase = i as f32 / 96.0;
            pos2(x_in(phase), y_in(wave(phase)))
        })
        .collect();
    painter.add(Shape::line(
        points,
        Stroke::new(1.2, visuals.text_color().gamma_multiply(0.5)),
    ));
    for (phase, color) in [(right, weak), (left, accent)] {
        let phase = phase.rem_euclid(1.0);
        painter.circle_filled(pos2(x_in(phase), y_in(wave(phase))), 3.5, color);
    }

    // What the LFO and the sweep are doing, in words.
    let speed = if panel.value("sync") < 0.5 {
        let note = (panel.value("note") as usize).min(NOTES.len() - 1);
        format!("{} · {rate:.2} Hz", NOTES[note].0)
    } else {
        format!("{rate:.2} Hz")
    };
    let reach = if mode == PHASER {
        format!("{} – {}", hz_label(low), hz_label(high))
    } else {
        format!("{} – {} ms", ms_label(low), ms_label(high))
    };
    painter.text(
        pos2(full.left(), full.top() - 2.0),
        Align2::LEFT_TOP,
        speed,
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );
    painter.text(
        pos2(full.right(), full.top() - 2.0),
        Align2::RIGHT_TOP,
        reach,
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );

    if response.dragged() {
        let delta = response.drag_delta();
        // Right moves the sweep up: a higher center, a shorter delay.
        let factor = (delta.x / 150.0).exp();
        if delta.x != 0.0 && mode == PHASER {
            panel.set_id("center", panel.value("center") * factor);
        } else if delta.x != 0.0 {
            panel.set_id("delay", panel.value("delay") / factor);
        }
        if delta.y != 0.0 {
            panel.set_id("depth", panel.value("depth") - delta.y * 0.5);
        }
    }
    if response.drag_stopped() {
        panel.finish();
    }
    response.on_hover_text("Drag sideways to move the sweep, up or down for the depth");
}
