//! The frequency shifter's panel: what it does to a harmonic sound, the
//! harmonics of a low note (faint) and where the shift moves them now
//! (bright, the left side's above the line and the right's below), so a
//! modulated shift slides them as it sounds and shows how a few Hz turn a
//! chord into a clang; with Ring, the other sideband too; with feedback,
//! the repeats spiralling further out, fainter each time. Then the knobs.

use effects::freqshift::{self, METER_SHIFT_LEFT, METER_SHIFT_RIGHT};
use eframe::egui::{self, Align2, Color32, FontId, Sense, Stroke, Ui, pos2, vec2};

use super::Panel;
use super::panel::display;

/// The note whose harmonics the display shifts, and how many it draws.
const NOTE_HZ: f32 = 110.0;
const HARMONICS: usize = 16;
/// Repeats drawn for the feedback.
const REPEATS: usize = 4;

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let switches = crate::theme::size::CONTROL + crate::theme::space::M;
    let height = 2.0 * panel.fit_knobs_beside(ui, 2, switches) + switches;
    ui.horizontal_top(|ui| {
        partials_display(ui, panel, vec2(height * 2.0, height));
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.weak("Range");
                panel.choice(ui, "range");
            });
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                let live = panel.meters[METER_SHIFT_LEFT];
                let range = freqshift::RANGES
                    [(panel.value("range") as usize).min(freqshift::RANGES.len() - 1)];
                panel.knob_with(ui, "shift", None, Some(100.0 * live / range));
                panel.knobs(ui, &["ring", "stereo", "mix"]);
            });
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                panel.knobs(ui, &["delay", "feedback", "blur", "output"]);
            });
        });
    });
}

fn hz_label(hz: f32) -> String {
    let sign = if hz < 0.0 { "-" } else { "+" };
    let hz = hz.abs();
    if hz >= 1000.0 {
        format!("{sign}{:.2} kHz", hz / 1000.0)
    } else if hz >= 10.0 {
        format!("{sign}{hz:.0} Hz")
    } else {
        format!("{sign}{hz:.1} Hz")
    }
}

fn partials_display(ui: &mut Ui, panel: &mut Panel, size: egui::Vec2) {
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    let rect = response.rect.shrink2(vec2(8.0, 16.0));
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let weak = visuals.weak_text_color();
    let other = visuals.strong_text_color().gamma_multiply(0.7);

    let values = panel.values().to_vec();
    let knob = freqshift::shifts(&values);
    let metered = [
        panel.meters[METER_SHIFT_LEFT],
        panel.meters[METER_SHIFT_RIGHT],
    ];
    // The meters read 0 until sound has gone through; then they follow
    // whatever moves the shift.
    let shifts = if metered.iter().any(|&s| s != 0.0) {
        metered
    } else {
        knob
    };
    let ring = panel.value("ring") / 100.0;
    let feedback = panel.value("feedback") / 100.0;

    let span = 1000f32.ln();
    let x_of = |hz: f32| rect.left() + (hz / 20.0).ln() / span * rect.width();
    let middle = rect.center().y;
    let half = rect.height() / 2.0;
    let grid = Stroke::new(1.0, weak.gamma_multiply(0.2));
    painter.hline(rect.x_range(), middle, grid);
    for (hz, text) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
        painter.vline(x_of(hz), rect.y_range(), grid);
        painter.text(
            pos2(x_of(hz) + 2.0, rect.bottom() + 2.0),
            Align2::LEFT_TOP,
            text,
            FontId::proportional(crate::theme::text::TINY),
            weak.gamma_multiply(0.8),
        );
    }

    // A line for each partial at `hz`, up from the middle for the left
    // side (or down for the right), `height` of the way, if it's audible.
    let line = |hz: f32, side: usize, height: f32, color: Color32| {
        if !(20.0..=20_000.0).contains(&hz) || height <= 0.01 {
            return;
        }
        let x = x_of(hz);
        let end = if side == 0 {
            middle - height * half
        } else {
            middle + height * half
        };
        painter.line_segment([pos2(x, middle), pos2(x, end)], Stroke::new(1.5, color));
    };
    for n in 1..=HARMONICS {
        let hz = NOTE_HZ * n as f32;
        // The note itself, faint, both sides.
        let level = 0.9 / (n as f32).sqrt();
        line(hz, 0, level, weak.gamma_multiply(0.35));
        line(hz, 1, level, weak.gamma_multiply(0.35));
        for (side, shift) in shifts.iter().enumerate() {
            let color = if side == 0 { accent } else { other };
            let kept = 1.0 - 0.3 * ring;
            line(hz + shift, side, level * kept, color);
            if ring > 0.0 {
                line(
                    hz - shift,
                    side,
                    level * 0.7 * ring,
                    color.gamma_multiply(0.6),
                );
            }
            // The repeats, each shifted again and fainter.
            let mut fade = 1.0;
            for repeat in 2..=REPEATS + 1 {
                fade *= feedback;
                line(
                    hz + repeat as f32 * shift,
                    side,
                    level * kept * fade,
                    color.gamma_multiply(0.5 * fade),
                );
            }
        }
    }

    let label = if (shifts[0] - shifts[1]).abs() > 0.05 {
        format!("L {}  ·  R {}", hz_label(shifts[0]), hz_label(shifts[1]))
    } else {
        hz_label(shifts[0])
    };
    painter.text(
        response.rect.left_top() + super::panel::DISPLAY_INSET,
        Align2::LEFT_TOP,
        label,
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );
    if ring > 0.0 {
        painter.text(
            response.rect.right_top()
                + vec2(
                    -super::panel::DISPLAY_INSET.x,
                    super::panel::DISPLAY_INSET.y,
                ),
            Align2::RIGHT_TOP,
            format!("ring {:.0}%", ring * 100.0),
            FontId::proportional(crate::theme::text::SMALL),
            weak,
        );
    }

    // Drag sideways for the shift.
    if response.dragged() {
        let delta = response.drag_delta();
        if delta.x != 0.0 {
            panel.set_id("shift", panel.value("shift") + delta.x * 0.25);
        }
    }
    if response.drag_stopped() {
        panel.finish();
    }
    response.on_hover_text("Drag sideways for the shift");
}
