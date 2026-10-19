//! The utility's panel: its switches, then the output's stereo image as the
//! ellipse its two sides trace on a goniometer (upright for mono, leaning
//! towards the side it's balanced to, round when wide, lying flat when the
//! sides are opposite), worked out from the meters, with the correlation
//! under it, each side's peak beside it, and the knobs, the side delay's
//! next to the width with which side it holds back.

use std::f32::consts::{FRAC_1_SQRT_2, TAU};

use effects::utility::{
    CHANNEL_MODES, DELAY_OFF, DELAY_SIDES, METER_CORRELATION, METER_PEAK_LEFT, METER_PEAK_RIGHT,
    METER_RMS_LEFT, METER_RMS_RIGHT, STEREO,
};
use eframe::egui::{Align2, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2};

use super::Panel;
use super::panel::display;
use crate::widgets::scopes;

/// The level range of the peak bars.
const FLOOR_DB: f32 = -60.0;
const CEILING_DB: f32 = 6.0;
/// Below this RMS level on both sides there is no image to draw.
const QUIET_DB: f32 = -80.0;

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    ui.horizontal(|ui| {
        panel.choice_menu(ui, "channels");
        ui.separator();
        panel.toggle(ui, "swap", "Swap");
        panel.toggle(ui, "invert_left", "Ø L");
        panel.toggle(ui, "invert_right", "Ø R");
        ui.separator();
        panel.toggle(ui, "dc", "DC");
        panel.toggle(ui, "mute", "Mute");
    });
    let height = 2.0 * panel.fit_knobs(ui, 2);
    ui.horizontal_top(|ui| {
        image(ui, panel, height);
        peaks(ui, panel, height);
        ui.vertical(|ui| {
            panel.knobs(ui, &["gain", "balance", "width"]);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                let on = panel.value("bass_mono") >= 0.5;
                super::panel::knob_slot(ui, panel.knob_size, "", |ui| {
                    panel.toggle(ui, "bass_mono", "Bass mono")
                });
                ui.add_enabled_ui(on, |ui| panel.knob(ui, "mono_below"));
            });
        });
        // The side delay, and under it which side it holds back.
        ui.vertical(|ui| {
            let delayed = panel.value("delay_side") as usize != DELAY_OFF;
            ui.add_enabled_ui(delayed, |ui| panel.knob(ui, "delay"));
            // Level with the Bass mono switch beside it.
            super::panel::knob_slot(ui, panel.knob_size, "", |ui| panel.choice(ui, "delay_side"));
        });
    });
}

/// The output's stereo image: the ellipse that holds its spread on a
/// goniometer (left up and to the left, right up and to the right), from
/// each side's RMS and their correlation, sized to the display whatever
/// the level. The correlation is the bar under it.
fn image(ui: &mut Ui, panel: &Panel, size: f32) {
    let (response, painter) = display(ui, Vec2::splat(size), Sense::hover());
    let outer = response.rect.shrink(6.0);
    let bar = Rect::from_min_max(pos2(outer.left(), outer.bottom() - 10.0), outer.max);
    let scope = Rect::from_min_max(outer.min, pos2(outer.right(), bar.top() - 4.0));
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let weak = visuals.weak_text_color();
    let center = scope.center();
    let radius = scope.width().min(scope.height()) / 2.0;

    let guide = Stroke::new(1.0, weak.gamma_multiply(0.25));
    let diagonal = vec2(FRAC_1_SQRT_2, -FRAC_1_SQRT_2) * radius;
    painter.line_segment([center - diagonal, center + diagonal], guide);
    let other = vec2(diagonal.x, -diagonal.y);
    painter.line_segment([center - other, center + other], guide);
    painter.vline(center.x, scope.y_range(), guide);
    for (side, x) in [("L", -1.0), ("R", 1.0)] {
        painter.text(
            center + vec2(x * radius * 0.78, -radius * 0.78),
            Align2::CENTER_CENTER,
            side,
            FontId::proportional(crate::theme::text::TINY),
            weak.gamma_multiply(0.8),
        );
    }

    let m = panel.meters;
    if let Some([a, b, c]) = shape(&m) {
        let scale = 0.85 * radius;
        let points: Vec<Pos2> = (0..64)
            .map(|i| {
                let (v, u) = (i as f32 / 64.0 * TAU).sin_cos();
                center + vec2(a * u + b * v, -(b * u + c * v)) * scale
            })
            .collect();
        painter.add(Shape::convex_polygon(
            points,
            accent.gamma_multiply(0.25),
            Stroke::new(1.5, accent),
        ));
    }
    scopes::correlation_bar(&painter, bar, m[METER_CORRELATION], &visuals);

    // What the switches do to the image, in words, and the correlation's
    // number, in the corners the guides leave free.
    let mut state = Vec::new();
    let channels = panel.value("channels") as usize;
    if channels != STEREO {
        state.push(
            CHANNEL_MODES
                .get(channels)
                .copied()
                .unwrap_or("?")
                .to_owned(),
        );
    }
    if panel.value("bass_mono") >= 0.5 {
        state.push(format!("mono < {:.0} Hz", panel.value("mono_below")));
    }
    let delayed = panel.value("delay_side") as usize;
    if delayed != DELAY_OFF {
        let side = DELAY_SIDES.get(delayed).copied().unwrap_or("?");
        state.push(format!("{side} +{:.1} ms", panel.value("delay")));
    }
    painter.text(
        scope.left_bottom(),
        Align2::LEFT_BOTTOM,
        state.join(" · "),
        FontId::proportional(crate::theme::text::TINY),
        visuals.text_color(),
    );
    painter.text(
        scope.right_bottom(),
        Align2::RIGHT_BOTTOM,
        format!("{:+.2}", m[METER_CORRELATION]),
        FontId::monospace(crate::theme::text::TINY),
        weak,
    );
    if panel.value("mute") >= 0.5 {
        painter.text(
            center,
            Align2::CENTER_CENTER,
            "Muted",
            FontId::proportional(crate::theme::text::BODY),
            visuals.strong_text_color(),
        );
    }
}

/// The image's shape from the meters: the square root `[[a, b], [b, c]]`
/// of the sides' spread on a goniometer, across as `(R - L) / √2` and up
/// as `(L + R) / √2`, so that it takes the unit circle to the ellipse. It
/// is scaled so the ellipse reaches 1 at its longest; `None` when there's
/// no sound to draw.
fn shape(meters: &[f32]) -> Option<[f32; 3]> {
    let (rms_l, rms_r) = (meters[METER_RMS_LEFT], meters[METER_RMS_RIGHT]);
    if rms_l.max(rms_r) <= QUIET_DB {
        return None;
    }
    let (l, r) = (dsp::db_to_gain(rms_l), dsp::db_to_gain(rms_r));
    let (ll, rr, lr) = (l * l, r * r, meters[METER_CORRELATION] * l * r);
    let (xx, yy, xy) = (
        (ll + rr - 2.0 * lr) / 2.0,
        (ll + rr + 2.0 * lr) / 2.0,
        (rr - ll) / 2.0,
    );
    // A 2×2 covariance's square root is `(C + √det · I) / √(trace +
    // 2√det)`; its longest reach is the root of the larger eigenvalue.
    let det_root = (xx * yy - xy * xy).max(0.0).sqrt();
    let trace_root = (xx + yy + 2.0 * det_root).sqrt();
    let largest = 0.5 * (xx + yy) + (0.25 * (xx - yy).powi(2) + xy * xy).sqrt();
    let scale = trace_root * largest.sqrt();
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    Some([(xx + det_root) / scale, xy / scale, (yy + det_root) / scale])
}

/// Each side's peak, with 0 dB marked and its number under it.
fn peaks(ui: &mut Ui, panel: &Panel, height: f32) {
    let (response, painter) = display(ui, vec2(48.0, height), Sense::hover());
    let rect = response.rect.shrink(5.0);
    let bars = Rect::from_min_max(rect.min, rect.max - vec2(0.0, 24.0));
    let visuals = ui.visuals().clone();
    let weak = visuals.weak_text_color();
    let m = panel.meters;
    let levels = [m[METER_PEAK_LEFT], m[METER_PEAK_RIGHT]];
    scopes::level_bars(&painter, bars, &levels, FLOOR_DB, CEILING_DB, &visuals);
    let zero = bars.bottom() - -FLOOR_DB / (CEILING_DB - FLOOR_DB) * bars.height();
    painter.hline(
        bars.x_range(),
        zero,
        Stroke::new(1.0, weak.gamma_multiply(0.6)),
    );
    for (i, (side, db)) in ["L", "R"].iter().zip(levels).enumerate() {
        let x = bars.left() + bars.width() * (0.25 + 0.5 * i as f32);
        painter.text(
            pos2(x, rect.bottom() - 22.0),
            Align2::CENTER_TOP,
            *side,
            FontId::proportional(crate::theme::text::TINY),
            weak,
        );
        let number = if db <= -99.0 {
            "-inf".to_owned()
        } else {
            format!("{db:.0}")
        };
        painter.text(
            pos2(x, rect.bottom() - 10.0),
            Align2::CENTER_TOP,
            number,
            FontId::monospace(crate::theme::text::TINY),
            visuals.text_color(),
        );
    }
    response.on_hover_text("Output peak, left and right (dB)");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The image's shape for each side's RMS (dB) and their correlation.
    fn image_of(left_db: f32, right_db: f32, correlation: f32) -> Option<[f32; 3]> {
        let mut meters = [0.0; effects::MAX_METERS];
        meters[METER_RMS_LEFT] = left_db;
        meters[METER_RMS_RIGHT] = right_db;
        meters[METER_CORRELATION] = correlation;
        shape(&meters)
    }

    /// Where the shape takes a point of the unit circle.
    fn reach([a, b, c]: [f32; 3], (u, v): (f32, f32)) -> (f32, f32) {
        (a * u + b * v, b * u + c * v)
    }

    #[test]
    fn the_image_stands_leans_and_lies_as_a_goniometer_does() {
        let close = |(x, y): (f32, f32), (to_x, to_y): (f32, f32)| {
            (x - to_x).abs() < 1e-3 && (y - to_y).abs() < 1e-3
        };
        let h = FRAC_1_SQRT_2;
        // Mono: an upright line, reaching 1.
        let mono = image_of(-6.0, -6.0, 1.0).unwrap();
        assert!(close(reach(mono, (0.0, 1.0)), (0.0, 1.0)), "{mono:?}");
        assert!(close(reach(mono, (1.0, 0.0)), (0.0, 0.0)), "{mono:?}");
        // Opposite sides: lying flat.
        let opposite = image_of(-6.0, -6.0, -1.0).unwrap();
        assert!(close(reach(opposite, (1.0, 0.0)), (1.0, 0.0)));
        assert!(close(reach(opposite, (0.0, 1.0)), (0.0, 0.0)));
        // Left only: along the up-left diagonal.
        let left = image_of(-6.0, -100.0, 0.0).unwrap();
        assert!(close(reach(left, (-h, h)), (-h, h)), "{left:?}");
        assert!(close(reach(left, (h, h)), (0.0, 0.0)), "{left:?}");
        // Unrelated sides, as loud: round.
        let round = image_of(-20.0, -20.0, 0.0).unwrap();
        assert!(close(reach(round, (h, h)), (h, h)), "{round:?}");
        // Nothing to draw for silence.
        assert_eq!(image_of(-100.0, -100.0, 0.0), None);
    }
}
