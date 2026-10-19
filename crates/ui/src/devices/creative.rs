//! The waveshaper's, shaper's and vocoder's panels.

use effects::shaper::{self, FILTER, MIX, PAN, POINT, POINTS, RATES, VOLUME};
use eframe::egui::{
    self, Align2, FontId, Rect, RichText, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2,
};
use engine::TapFrame;

use super::Panel;
use super::panel::{KNOB_TEXT, display};
use crate::widgets::scopes::{self, Channels};
use crate::widgets::shape::ShapeEditor;

/// The height left for displays above a row of knobs and a row of switches.
fn display_height(ui: &Ui, panel: &Panel) -> f32 {
    (ui.available_height() - (panel.knob_size + KNOB_TEXT) - super::panel::CONTROL_ROW)
        .clamp(90.0, 200.0)
}

/// How far the waveshaper's curve reaches either way.
const CURVE_SPAN: f32 = 1.5;

pub(super) fn waveshaper(ui: &mut Ui, panel: &mut Panel) {
    let height = display_height(ui, panel);
    ui.horizontal(|ui| {
        panel.choice_menu(ui, "shape");
        panel.choice(ui, "oversampling");
    });
    let frames: Vec<TapFrame> = panel
        .tap()
        .map(|tap| tap.latest((panel.sample_rate * 0.06) as usize).collect())
        .unwrap_or_default();
    ui.horizontal_top(|ui| {
        transfer(ui, panel, height);
        let (response, painter) = display(ui, vec2(height * 1.8, height), Sense::hover());
        let visuals = ui.visuals().clone();
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
        // Input and output peaks, linear in the meters.
        let (rect, _) = ui.allocate_exact_size(
            vec2(crate::widgets::scopes::level_bars_width(2), height),
            Sense::hover(),
        );
        let db = |v: f32| dsp::gain_to_db(v.max(1e-6));
        scopes::level_bars(
            ui.painter(),
            rect,
            &[db(panel.meters[0]), db(panel.meters[1])],
            -60.0,
            6.0,
            &visuals,
        );
    });
    let bitcrush = panel.value("shape") as usize == 6;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
        panel.knob_as(ui, "drive", Some(if bitcrush { "Bits" } else { "Drive" }));
        panel.knobs(ui, &["bias", "tone", "output", "mix"]);
    });
}

/// The transfer curve: what each input level comes out as, at these
/// settings, with how far the input reaches now.
fn transfer(ui: &mut Ui, panel: &Panel, size: f32) {
    let (response, painter) = display(ui, Vec2::splat(size), Sense::hover());
    let rect = response.rect.shrink(6.0);
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let to_x = |x: f32| rect.center().x + x / CURVE_SPAN * rect.width() / 2.0;
    let to_y = |y: f32| rect.center().y - (y / CURVE_SPAN).clamp(-1.0, 1.0) * rect.height() / 2.0;
    let grid = visuals.weak_text_color().gamma_multiply(0.18);
    painter.hline(rect.x_range(), rect.center().y, Stroke::new(1.0, grid));
    painter.vline(rect.center().x, rect.y_range(), Stroke::new(1.0, grid));
    painter.line_segment(
        [
            pos2(to_x(-CURVE_SPAN), to_y(-CURVE_SPAN)),
            pos2(to_x(CURVE_SPAN), to_y(CURVE_SPAN)),
        ],
        Stroke::new(1.0, grid.gamma_multiply(1.5)),
    );
    for level in [-1.0, 1.0] {
        painter.vline(to_x(level), rect.y_range(), Stroke::new(1.0, grid));
        painter.hline(rect.x_range(), to_y(level), Stroke::new(1.0, grid));
    }
    let values = panel.values().to_vec();
    let points: Vec<egui::Pos2> = (0..=200)
        .map(|i| {
            let x = -CURVE_SPAN + 2.0 * CURVE_SPAN * i as f32 / 200.0;
            pos2(to_x(x), to_y(effects::waveshaper::curve(&values, x)))
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(2.0, accent)));
    // The input's reach, shaded.
    let reach = panel.meters[0].min(CURVE_SPAN);
    if reach > 1e-4 {
        painter.rect_filled(
            Rect::from_x_y_ranges(to_x(-reach)..=to_x(reach), rect.y_range()),
            0.0,
            accent.gamma_multiply(0.08),
        );
    }
}

pub(super) fn shaper(ui: &mut Ui, panel: &mut Panel) {
    let module_id = panel.id("module");
    let mut module: usize = ui.data(|d| d.get_temp(module_id)).unwrap_or(VOLUME);
    let names = [(VOLUME, "Volume"), (FILTER, "Filter"), (PAN, "Pan")];
    ui.horizontal(|ui| {
        for (m, name) in names {
            let on = panel.values()[shaper::param(m, shaper::ON)] >= 0.5;
            let text = if on {
                RichText::new(name).strong()
            } else {
                RichText::new(name).weak()
            };
            if ui
                .selectable_label(module == m, text)
                .on_hover_text(format!("Show the {} shape", name.to_lowercase()))
                .clicked()
            {
                module = m;
            }
        }
        ui.separator();
        let on_id = panel.params[shaper::param(module, shaper::ON)].id;
        panel.toggle(ui, on_id, "On");
        let rate_id = panel.params[shaper::param(module, shaper::RATE)].id;
        panel.choice_menu(ui, rate_id);
    });
    let height = display_height(ui, panel);
    let values = panel.values().to_vec();
    let points: Vec<f32> = (0..POINTS)
        .map(|i| values[shaper::param(module, POINT + i)])
        .collect();
    let curve = |phase: f32| effects::shaper::shape_at(&values, module, phase);
    let rate = RATES[(values[shaper::param(module, shaper::RATE)] as usize).min(RATES.len() - 1)].1;
    // Beat lines: sixteenths across a beat or less, beats across more.
    let divisions = if rate <= 1.0 {
        (rate * 4.0).round().max(2.0) as usize
    } else {
        (rate.round() as usize).clamp(2, 16)
    };
    let playing = panel.values()[shaper::param(module, shaper::ON)] >= 0.5;
    let response = ShapeEditor::new(panel.id(("shape", module)), &points, &curve)
        .size(vec2(height * 3.2, height))
        .color(crate::theme::accent(ui.visuals()))
        .playhead(playing.then(|| panel.meters[2 * module]))
        .divisions(divisions)
        .show(ui);
    for (i, value) in response.changes {
        panel.set(shaper::param(module, POINT + i), value);
    }
    if response.finished {
        panel.finish();
    }
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
        for which in [shaper::DEPTH, shaper::PHASE, shaper::SMOOTHING] {
            let id = panel.params[shaper::param(module, which)].id;
            let label = ["Depth", "Phase", "Smooth"][which - shaper::DEPTH];
            panel.knob_as(ui, id, Some(label));
        }
        if module == FILTER {
            ui.separator();
            let id = panel.params[shaper::FILTER_TYPE].id;
            super::panel::knob_slot(ui, panel.knob_size, "", |ui| panel.choice(ui, id));
            for (index, label) in [
                (shaper::FILTER_LOW, "From"),
                (shaper::FILTER_HIGH, "To"),
                (shaper::RESONANCE, "Resonance"),
            ] {
                let id = panel.params[index].id;
                panel.knob_as(ui, id, Some(label));
            }
        }
        ui.separator();
        let id = panel.params[MIX].id;
        panel.knob(ui, id);
    });
    ui.data_mut(|d| d.insert_temp(module_id, module));
}

pub(super) fn vocoder(ui: &mut Ui, panel: &mut Panel) {
    let height = display_height(ui, panel);
    ui.horizontal(|ui| {
        ui.label("Bands");
        panel.choice_menu(ui, "bands");
    });
    let (response, painter) = display(ui, vec2(height * 3.0, height), Sense::hover());
    let rect = response.rect.shrink(crate::theme::space::M);
    let visuals = ui.visuals().clone();
    let count = effects::vocoder::band_count(panel.value("bands")).max(2);
    let (low, high) = (panel.value("low"), panel.value("high"));
    let to_x =
        |hz: f32| rect.left() + (hz / 40.0).max(1e-6).ln() / (20_000f32 / 40.0).ln() * rect.width();
    let bar = (rect.width() / count as f32 * 0.6).max(2.0);
    for i in 0..count {
        let hz = low * (high / low).powf(i as f32 / (count - 1) as f32);
        let level = panel.meters.get(i).copied().unwrap_or(0.0);
        let db = dsp::gain_to_db(level.max(1e-6));
        let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let x = to_x(hz);
        painter.rect_filled(
            Rect::from_x_y_ranges(x - bar / 2.0..=x + bar / 2.0, rect.y_range()),
            1.0,
            visuals.weak_text_color().gamma_multiply(0.12),
        );
        if t > 0.0 {
            painter.rect_filled(
                Rect::from_x_y_ranges(
                    x - bar / 2.0..=x + bar / 2.0,
                    rect.bottom() - t * rect.height()..=rect.bottom(),
                ),
                1.0,
                crate::theme::accent(&visuals).gamma_multiply(0.4 + 0.6 * t),
            );
        }
    }
    for (hz, text) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
        painter.text(
            pos2(to_x(hz), rect.bottom()),
            Align2::CENTER_TOP,
            text,
            FontId::proportional(crate::theme::text::TINY),
            visuals.weak_text_color(),
        );
    }
    if !panel.session_has_sidechain() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "No sidechain",
            FontId::proportional(crate::theme::text::SMALL),
            visuals.weak_text_color(),
        );
    }
    panel.knobs(
        ui,
        &[
            "low",
            "high",
            "width",
            "attack",
            "release",
            "formant",
            "sibilance",
            "output",
            "mix",
        ],
    );
}
