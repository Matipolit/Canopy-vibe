//! The Amp's panel: the voicing and cabinet, the response of the tone
//! stack, presence and cabinet together (what the knobs do to the tone,
//! without the distortion), input and output levels and the power amp's
//! sag, then the amp's knobs as an amp lays them out.

use effects::amp::{AmpSim, METER_INPUT, METER_OUTPUT, METER_SAG, settings};
use eframe::egui::{Align2, FontId, Pos2, Sense, Shape, Stroke, Ui, vec2};

use super::Panel;
use super::panel::{CONTROL_ROW, DISPLAY_INSET, KNOB_TEXT, display};
use crate::theme::{self, size, space, stroke, text};
use crate::widgets::scopes;

/// The response is drawn from this frequency to [`HIGHEST_HZ`].
const LOWEST_HZ: f32 = 40.0;
const HIGHEST_HZ: f32 = 12_000.0;
/// Decibels shown below the response's highest point.
const RANGE_DB: f32 = 36.0;

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let height =
        (ui.available_height() - (panel.knob_size + KNOB_TEXT) - CONTROL_ROW).clamp(80.0, 180.0);
    ui.horizontal(|ui| {
        panel.choice(ui, "voicing");
        ui.separator();
        panel.choice_menu(ui, "cabinet");
    });
    ui.horizontal_top(|ui| {
        response(ui, panel, height);
        let (rect, _) =
            ui.allocate_exact_size(vec2(scopes::level_bars_width(2), height), Sense::hover());
        let db = |v: f32| dsp::gain_to_db(v.max(1e-6));
        let visuals = ui.visuals().clone();
        scopes::level_bars(
            ui.painter(),
            rect,
            &[
                db(panel.meters[METER_INPUT]),
                db(panel.meters[METER_OUTPUT]),
            ],
            -60.0,
            6.0,
            &visuals,
        );
        ui.vertical(|ui| {
            ui.add_space(space::S);
            ui.label(format!("Sag {:.1} dB", panel.meters[METER_SAG]))
                .on_hover_text("How much the power amp is holding back under load");
        });
    });
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        panel.knob(ui, "gain");
        ui.separator();
        panel.knobs(ui, &["bass", "mid", "treble"]);
        ui.separator();
        panel.knobs(ui, &["presence", "sag"]);
        ui.separator();
        panel.knobs(ui, &["mic", "output"]);
    });
    panel.animate(ui);
}

/// The tone stack, presence and cabinet's response, its highest point at
/// the top.
fn response(ui: &mut Ui, panel: &Panel, height: f32) {
    let (response, painter) = display(ui, vec2(height * 2.8, height), Sense::hover());
    // A row at the top for the display's name.
    let rect = response
        .rect
        .shrink2(vec2(space::M, space::M + space::XS))
        .with_min_y(response.rect.top() + size::CONTROL);
    let visuals = ui.visuals().clone();
    let accent = theme::accent(&visuals);
    let span = (HIGHEST_HZ / LOWEST_HZ).log2();
    let to_x = |hz: f32| rect.left() + (hz / LOWEST_HZ).log2() / span * rect.width();
    let amp = settings(panel.values());
    let points = 160;
    let db: Vec<f32> = (0..=points)
        .map(|i| {
            let hz = LOWEST_HZ * (span * i as f32 / points as f32).exp2();
            AmpSim::response_db(&amp, panel.sample_rate, hz)
        })
        .collect();
    let top = db.iter().fold(f32::MIN, |m, &d| m.max(d));
    let to_y = |d: f32| rect.top() + ((top - d) / RANGE_DB).clamp(0.0, 1.0) * rect.height();
    let grid = visuals.weak_text_color().gamma_multiply(0.2);
    for hz in [100.0, 1000.0, 10_000.0] {
        painter.vline(
            to_x(hz),
            rect.y_range(),
            Stroke::new(stroke::HAIRLINE, grid),
        );
        painter.text(
            Pos2::new(to_x(hz) + space::XS, rect.bottom() + 1.0),
            Align2::LEFT_TOP,
            if hz < 1000.0 {
                format!("{hz:.0}")
            } else {
                format!("{:.0}k", hz / 1000.0)
            },
            FontId::proportional(text::TINY),
            visuals.weak_text_color(),
        );
    }
    for d in [12.0, 24.0] {
        painter.hline(
            rect.x_range(),
            to_y(top - d),
            Stroke::new(stroke::HAIRLINE, grid),
        );
    }
    let line: Vec<Pos2> = db
        .iter()
        .enumerate()
        .map(|(i, &d)| {
            let hz = LOWEST_HZ * (span * i as f32 / points as f32).exp2();
            Pos2::new(to_x(hz), to_y(d))
        })
        .collect();
    painter.add(Shape::line(line, Stroke::new(2.0, accent)));
    painter.text(
        response.rect.left_top() + DISPLAY_INSET,
        Align2::LEFT_TOP,
        "Tone stack and cabinet",
        FontId::proportional(text::TINY),
        visuals.weak_text_color(),
    );
}
