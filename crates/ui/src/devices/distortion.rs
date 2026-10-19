//! The distortion's panel: its curve (what each input level comes out as,
//! with how far the input reaches now shaded), an oscilloscope of what
//! goes in and comes out, the filter's response under the curve when it's
//! on, the levels in and out, then the mode, the filter's place and the
//! oversampling, and the knobs, drive named for what it does in the mode.

use effects::distortion::{self, FILTER_OFF, FILTER_PRE, MAX_HOLD, Mode};
use eframe::egui::{self, Align2, FontId, Rect, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2};
use engine::TapFrame;

use super::Panel;
use super::panel::display;
use crate::widgets::scopes::{self, Channels};

/// How far the curve reaches either way.
const CURVE_SPAN: f32 = 1.5;

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let switches = crate::theme::size::CONTROL + crate::theme::space::M;
    let height = 2.0 * panel.fit_knobs_beside(ui, 2, switches) + switches;
    let mode = Mode::from_index(panel.value("mode"));
    let frames: Vec<TapFrame> = panel
        .tap()
        .map(|tap| tap.latest((panel.sample_rate * 0.06) as usize).collect())
        .unwrap_or_default();
    ui.horizontal_top(|ui| {
        transfer(ui, panel, mode, height);
        let visuals = ui.visuals().clone();
        let (response, painter) = display(ui, vec2(height * 1.3, height), Sense::hover());
        let window = (panel.sample_rate * 0.012) as usize;
        scopes::oscilloscope(
            &painter,
            response.rect.shrink(6.0),
            &frames,
            window,
            Channels::InOut,
            [
                crate::theme::accent(&visuals),
                visuals.weak_text_color().gamma_multiply(0.6),
            ],
        );
        painter.text(
            response.rect.left_top() + super::panel::DISPLAY_INSET,
            Align2::LEFT_TOP,
            "In · Out",
            FontId::proportional(crate::theme::text::TINY),
            visuals.weak_text_color(),
        );
        let (rect, _) = ui.allocate_exact_size(
            vec2(crate::widgets::scopes::level_bars_width(2), height),
            Sense::hover(),
        );
        let db = |v: f32| dsp::gain_to_db(v.max(1e-6));
        scopes::level_bars(
            ui.painter(),
            rect,
            &[db(panel.meters[1]), db(panel.meters[0])],
            -60.0,
            6.0,
            &visuals,
        );
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                panel.choice_menu(ui, "mode");
                ui.separator();
                ui.weak("Filter");
                panel.choice(ui, "filter");
                ui.separator();
                panel.choice(ui, "oversampling");
            });
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                let drive = match mode {
                    Mode::Downsample => "Rate",
                    Mode::BitCrush => "Bits",
                    _ => "Drive",
                };
                panel.knob_as(ui, "drive", Some(drive));
                panel.knobs(ui, &["bias", "mix", "output"]);
            });
            let filtered = panel.value("filter") as usize != FILTER_OFF;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                ui.add_enabled_ui(filtered, |ui| {
                    panel.knob_as(ui, "filter_type", Some("Type"));
                    panel.knob_as(ui, "filter_freq", Some("Frequency"));
                    panel.knob_as(ui, "filter_res", Some("Resonance"));
                });
            });
        });
    });
}

/// The curve: what each input level comes out as, at these settings, with
/// how far the input reaches now; under it, while the filter's on, its
/// response from 20 Hz to 20 kHz, and what the drive means in this mode.
fn transfer(ui: &mut Ui, panel: &Panel, mode: Mode, size: f32) {
    let (response, painter) = display(ui, Vec2::splat(size), Sense::hover());
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let weak = visuals.weak_text_color();
    let filtered = panel.value("filter") as usize != FILTER_OFF;
    let full = response.rect.shrink(6.0);
    let strip = if filtered {
        (size * 0.18).max(14.0)
    } else {
        0.0
    };
    let rect = Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - strip));
    let to_x = |x: f32| rect.center().x + x / CURVE_SPAN * rect.width() / 2.0;
    let to_y = |y: f32| rect.center().y - (y / CURVE_SPAN).clamp(-1.0, 1.0) * rect.height() / 2.0;
    let grid = weak.gamma_multiply(0.18);
    painter.hline(rect.x_range(), rect.center().y, Stroke::new(1.0, grid));
    painter.vline(rect.center().x, rect.y_range(), Stroke::new(1.0, grid));
    for level in [-1.0, 1.0] {
        painter.vline(to_x(level), rect.y_range(), Stroke::new(1.0, grid));
        painter.hline(rect.x_range(), to_y(level), Stroke::new(1.0, grid));
    }
    let values = panel.values().to_vec();
    let points: Vec<egui::Pos2> = (0..=240)
        .map(|i| {
            let x = -CURVE_SPAN + 2.0 * CURVE_SPAN * i as f32 / 240.0;
            pos2(to_x(x), to_y(distortion::curve(&values, x)))
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(2.0, accent)));
    let reach = panel.meters[1].min(CURVE_SPAN);
    if reach > 1e-4 {
        painter.rect_filled(
            Rect::from_x_y_ranges(to_x(-reach)..=to_x(reach), rect.y_range()),
            0.0,
            accent.gamma_multiply(0.08),
        );
    }

    // What drive means here, in its own units.
    let drive = panel.value("drive") / 100.0;
    let meaning = match mode {
        Mode::Downsample => {
            let hold = MAX_HOLD.powf(drive);
            let rate = panel.sample_rate / hold;
            if rate >= 1000.0 {
                format!("{:.1} kHz", rate / 1000.0)
            } else {
                format!("{rate:.0} Hz")
            }
        }
        Mode::BitCrush => format!("{:.1} bits", 16.0 - 14.0 * drive),
        _ => format!("+{:.1} dB", distortion::MAX_DRIVE_DB * drive),
    };
    painter.text(
        rect.left_top(),
        Align2::LEFT_TOP,
        meaning,
        FontId::proportional(crate::theme::text::TINY),
        weak,
    );

    if filtered {
        let band = Rect::from_min_max(pos2(full.left(), full.bottom() - strip + 3.0), full.max);
        painter.rect_filled(band, crate::theme::radius::SMALL, weak.gamma_multiply(0.1));
        let span = 1000f32.ln();
        let columns = band.width().max(2.0) as usize;
        let line: Vec<egui::Pos2> = (0..=columns)
            .map(|i| {
                let x = band.left() + band.width() * i as f32 / columns as f32;
                let hz = 20.0 * ((x - band.left()) / band.width() * span).exp();
                let gain = distortion::filter_response(&values, hz, panel.sample_rate);
                let db = dsp::gain_to_db(gain).clamp(-30.0, 12.0);
                pos2(x, band.bottom() - (db + 30.0) / 42.0 * band.height())
            })
            .collect();
        painter.add(Shape::line(
            line,
            Stroke::new(1.2, accent.gamma_multiply(0.8)),
        ));
        let place = if panel.value("filter") as usize == FILTER_PRE {
            "pre"
        } else {
            "post"
        };
        painter.text(
            band.right_top(),
            Align2::RIGHT_TOP,
            place,
            FontId::proportional(crate::theme::text::TINY),
            weak,
        );
    }
}
