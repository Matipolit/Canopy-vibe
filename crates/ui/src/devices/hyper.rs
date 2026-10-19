//! Hyper/Dimension's panel: the voices as dots across the stereo field,
//! each as high or low as it's detuned right now (so they swim about as
//! the LFOs turn), inside the band the detune allows; Dimension as a glow
//! out to the sides, as wide as its amount; then the knobs, Hyper's on the
//! first row, Dimension's and the output on the second.

use effects::hyper::{self, MAX_CENTS, MAX_VOICES, METER_PITCH, METER_VOICES};
use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};

use super::Panel;
use super::panel::display;

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let height = 2.0 * panel.fit_knobs(ui, 2);
    ui.horizontal_top(|ui| {
        voices_display(ui, panel, vec2(height * 1.8, height));
        ui.vertical(|ui| {
            panel.knobs(ui, &["voices", "detune", "rate", "hyper_mix"]);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                panel.knob_as(ui, "size", Some("Size"));
                panel.knob_as(ui, "dimension", Some("Dimension"));
                panel.knob(ui, "output");
            });
        });
    });
}

fn voices_display(ui: &mut Ui, panel: &mut Panel, size: egui::Vec2) {
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    let rect = response.rect.shrink2(vec2(14.0, 16.0));
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let weak = visuals.weak_text_color();

    let detune = panel.value("detune") / 100.0;
    let cents = hyper::detune_cents(detune);
    let mix = panel.value("hyper_mix") / 100.0;
    let dimension = panel.value("dimension") / 100.0;
    let knob_voices = panel.value("voices") + 1.0;
    let voices = if panel.meters[METER_VOICES] >= 1.0 {
        panel.meters[METER_VOICES]
    } else {
        knob_voices
    };
    // Pitch, up to the most detune there is, against stereo position.
    let x_of = |pan: f32| rect.center().x + pan * rect.width() / 2.0;
    let y_of = |c: f32| rect.center().y - (c / MAX_CENTS).clamp(-1.0, 1.0) * rect.height() / 2.0;

    // Dimension: a glow out to both sides, as strong as its amount.
    if dimension > 0.0 {
        let glow = accent.gamma_multiply(0.12 * dimension);
        for side in [-1.0f32, 1.0] {
            let reach = 0.25 + 0.75 * dimension;
            let edge = x_of(side);
            let inner = x_of(side * (1.0 - reach));
            let (left, right) = if side < 0.0 {
                (edge, inner)
            } else {
                (inner, edge)
            };
            let band = Rect::from_x_y_ranges(left..=right, rect.y_range());
            painter.rect_filled(band, crate::theme::radius::PANEL, glow);
        }
    }

    // The band the detune allows, and the centre line.
    let grid = Stroke::new(1.0, weak.gamma_multiply(0.25));
    painter.hline(rect.x_range(), rect.center().y, grid);
    painter.vline(rect.center().x, rect.y_range(), grid);
    if cents > 0.01 {
        painter.rect_filled(
            Rect::from_x_y_ranges(rect.x_range(), y_of(cents)..=y_of(-cents)),
            2.0,
            accent.gamma_multiply(0.07),
        );
    }
    for (text, align, x) in [
        ("L", Align2::LEFT_BOTTOM, rect.left()),
        ("R", Align2::RIGHT_BOTTOM, rect.right()),
    ] {
        painter.text(
            pos2(x, rect.bottom() + 12.0),
            align,
            text,
            FontId::proportional(crate::theme::text::TINY),
            weak,
        );
    }

    // Each voice where it is and how detuned it is now; the ones fading
    // in or out fainter.
    let dot = accent.gamma_multiply(0.35 + 0.65 * mix.sqrt());
    for voice in 0..MAX_VOICES {
        let level = (voices - voice as f32).clamp(0.0, 1.0);
        if level <= 0.0 {
            continue;
        }
        let pitch = panel.meters[METER_PITCH + voice];
        let pitch = if pitch.is_finite() { pitch } else { 0.0 };
        let at = pos2(x_of(hyper::voice_pan(voice, voices)), y_of(pitch));
        painter.line_segment(
            [pos2(at.x, rect.center().y), at],
            Stroke::new(1.5, dot.gamma_multiply(0.5 * level)),
        );
        painter.circle_filled(at, 5.0, dot.gamma_multiply(level));
    }

    let rate = panel.value("rate");
    let voice_count = knob_voices as usize;
    let label = format!(
        "{voice_count} voice{} · ±{cents:.1} cents · {rate:.2} Hz",
        if voice_count == 1 { "" } else { "s" }
    );
    painter.text(
        response.rect.left_top() + super::panel::DISPLAY_INSET,
        Align2::LEFT_TOP,
        label,
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );
    if dimension > 0.0 {
        painter.text(
            response.rect.right_top()
                + vec2(
                    -super::panel::DISPLAY_INSET.x,
                    super::panel::DISPLAY_INSET.y,
                ),
            Align2::RIGHT_TOP,
            format!("dimension {:.0}%", dimension * 100.0),
            FontId::proportional(crate::theme::text::SMALL),
            Color32::from_rgba_unmultiplied(
                accent.r(),
                accent.g(),
                accent.b(),
                (120.0 + 135.0 * dimension) as u8,
            ),
        );
    }

    // Drag up or down for the detune, sideways for the dimension.
    if response.dragged() {
        let delta = response.drag_delta();
        if delta.y != 0.0 {
            panel.set_id("detune", detune * 100.0 - delta.y * 0.5);
        }
        if delta.x != 0.0 {
            panel.set_id("dimension", dimension * 100.0 + delta.x * 0.5);
        }
    }
    if response.drag_stopped() {
        panel.finish();
    }
    response.on_hover_text("Drag up or down for the detune, sideways for the dimension");
}
