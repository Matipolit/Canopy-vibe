//! The filter's panel: its response over the sound's spectrum, drawn at
//! the cutoffs where they are right now (so a modulated sweep moves on
//! screen as it sounds), each side's curve (left bright, right faint) with
//! a handle to drag the cutoff and resonance; the input and output levels
//! beside it; then the type menu and the knobs, Var named for what it does
//! in the type chosen.

use effects::multifilter::{
    self, Family, Kind, METER_CUTOFF_LEFT, METER_CUTOFF_RIGHT, METER_INPUT, METER_OUTPUT,
    METER_RESONANCE, VOWEL_NAMES,
};
use eframe::egui::{self, Align2, FontId, Pos2, Sense, Shape, Stroke, Ui, pos2, vec2};
use engine::TapFrame;

use super::Panel;
use super::panel::display;
use crate::widgets::scopes;
use crate::widgets::spectrum::{Axes, Spectrum, SpectrumSettings};

/// The display's level range: room above 0 for resonance, and enough
/// below for the stop band to read.
const TOP_DB: f32 = 24.0;
const FLOOR_DB: f32 = -48.0;
/// The level bars' range.
const BAR_FLOOR_DB: f32 = -60.0;
const BAR_TOP_DB: f32 = 6.0;

/// The output's spectrum, kept between frames.
#[derive(Clone, Default)]
struct Analysis {
    after: Spectrum,
    pass: u64,
}

fn spectrum_settings() -> SpectrumSettings {
    SpectrumSettings {
        size: 4096,
        floor_db: FLOOR_DB,
        ceiling_db: TOP_DB,
        fall: 40.0,
        ..SpectrumSettings::default()
    }
}

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    // Room for the type menu above the knobs.
    let switches = crate::theme::size::CONTROL + crate::theme::space::M;
    let height = 2.0 * panel.fit_knobs_beside(ui, 2, switches) + switches;
    let kind = Kind::from_index(panel.value("type"));
    ui.horizontal_top(|ui| {
        response_display(ui, panel, kind, vec2(height * 2.0, height));
        let (rect, _) = ui.allocate_exact_size(
            vec2(crate::widgets::scopes::level_bars_width(2), height),
            Sense::hover(),
        );
        let db = |v: f32| dsp::gain_to_db(v.max(1e-6));
        scopes::level_bars(
            ui.painter(),
            rect,
            &[
                db(panel.meters[METER_INPUT]),
                db(panel.meters[METER_OUTPUT]),
            ],
            BAR_FLOOR_DB,
            BAR_TOP_DB,
            ui.visuals(),
        );
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.weak("Type");
                panel.choice_menu(ui, "type");
            });
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                let cutoff = match kind.family() {
                    Family::Comb => "Pitch",
                    Family::Formant => "Shift",
                    _ => "Cutoff",
                };
                let live = live_cutoffs(panel);
                panel.knob_with(ui, "cutoff", Some(cutoff), Some((live[0] * live[1]).sqrt()));
                let resonance = match kind.family() {
                    Family::Comb | Family::Phaser => "Feedback",
                    _ => "Resonance",
                };
                let live = panel.meters[METER_RESONANCE] * 100.0;
                panel.knob_with(ui, "resonance", Some(resonance), Some(live));
                panel.knob(ui, "drive");
                if let Some(name) = kind.var_name() {
                    panel.knob_as(ui, "var", Some(name));
                }
                if kind.has_gain() {
                    panel.knob(ui, "gain");
                }
            });
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                panel.knob_as(ui, "spread", Some("Spread"));
                panel.knobs(ui, &["mix", "output"]);
            });
        });
    });
}

/// Where the left and right cutoffs are now: the meters, or the knobs
/// before any sound has gone through.
fn live_cutoffs(panel: &Panel) -> [f32; 2] {
    let metered = [
        panel.meters[METER_CUTOFF_LEFT],
        panel.meters[METER_CUTOFF_RIGHT],
    ];
    if metered.iter().all(|hz| hz.is_finite() && *hz > 0.0) {
        return metered;
    }
    let cutoff = panel.value("cutoff");
    let half = panel.value("spread") / 24.0;
    [cutoff * (-half).exp2(), cutoff * half.exp2()]
}

/// `hz` as the display labels it.
fn hz_label(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.2} kHz", hz / 1000.0)
    } else {
        format!("{hz:.0} Hz")
    }
}

/// The response of each side over the output's spectrum, with a handle at
/// the cutoff: drag it sideways for the cutoff, up or down for the
/// resonance.
fn response_display(ui: &mut Ui, panel: &mut Panel, kind: Kind, size: egui::Vec2) {
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    let rect = response.rect.shrink2(vec2(6.0, 14.0));
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let weak = visuals.weak_text_color();
    let axes = Axes {
        rect,
        low_hz: 20.0,
        high_hz: 20_000.0,
        floor_db: FLOOR_DB,
        ceiling_db: TOP_DB,
    };
    axes.paint_grid(&painter, &visuals, 12.0);

    // The output's spectrum, analyzed once per frame.
    let settings = spectrum_settings();
    let state_id = panel.id("spectrum");
    let mut analysis: Analysis = ui.data_mut(|d| d.remove_temp(state_id)).unwrap_or_default();
    let pass = ui.ctx().cumulative_pass_nr();
    if analysis.pass != pass {
        analysis.pass = pass;
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        if let Some(tap) = panel.tap() {
            let frames: Vec<TapFrame> = tap.latest(settings.size).collect();
            let after: Vec<f32> = frames.iter().map(|f| 0.5 * (f[2] + f[3])).collect();
            analysis.after.update(&after, &settings, dt);
        }
    }
    analysis.after.paint(
        &painter,
        &axes,
        &settings,
        panel.sample_rate,
        Stroke::new(1.0, weak.gamma_multiply(0.35)),
        weak.gamma_multiply(0.08),
    );
    ui.data_mut(|d| d.insert_temp(state_id, analysis));

    // Each side's curve at its cutoff now: one point a pixel.
    let values = panel.values().to_vec();
    let sample_rate = panel.sample_rate;
    let live = live_cutoffs(panel);
    let columns = rect.width().max(2.0) as usize;
    let curve = |cutoff: f32| -> Vec<Pos2> {
        (0..=columns)
            .map(|i| {
                let x = rect.left() + rect.width() * i as f32 / columns as f32;
                let gain = multifilter::response(&values, cutoff, axes.hz(x), sample_rate);
                pos2(x, axes.y(dsp::gain_to_db(gain)))
            })
            .collect()
    };
    let (left, right) = (curve(live[0]), curve(live[1]));
    if (live[0] - live[1]).abs() > 0.01 {
        painter.add(Shape::line(
            right,
            Stroke::new(1.2, weak.gamma_multiply(0.9)),
        ));
    }
    let mut fill = egui::Mesh::default();
    for pair in left.windows(2) {
        fill.add_colored_rect(
            egui::Rect::from_min_max(pair[0], pos2(pair[1].x, rect.bottom())),
            accent.gamma_multiply(0.12),
        );
    }
    painter.add(fill);
    painter.add(Shape::line(left, Stroke::new(1.8, accent)));

    // The handle, on the curve at the knob's cutoff.
    let cutoff = panel.value("cutoff");
    let at_cutoff = multifilter::response(&values, cutoff, cutoff, sample_rate);
    let handle = pos2(axes.x(cutoff), axes.y(dsp::gain_to_db(at_cutoff)));
    painter.circle_filled(handle, 5.0, accent);
    painter.circle_stroke(handle, 5.0, Stroke::new(1.0, visuals.strong_text_color()));

    // What it is and where, in words.
    let resonance = panel.value("resonance");
    let name = multifilter::TYPES[kind.index()];
    let detail = match kind.family() {
        Family::Formant => {
            let vowel = panel.value("var") / 100.0 * (VOWEL_NAMES.len() - 1) as f32;
            let first = VOWEL_NAMES[vowel.floor() as usize];
            let next = VOWEL_NAMES[(vowel.ceil() as usize).min(VOWEL_NAMES.len() - 1)];
            if first == next {
                format!("{name} · {first}")
            } else {
                format!("{name} · {first}–{next}")
            }
        }
        _ => format!("{name} · {} · {resonance:.0}%", hz_label(cutoff)),
    };
    painter.text(
        response.rect.left_top() + super::panel::DISPLAY_INSET,
        Align2::LEFT_TOP,
        detail,
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );

    if response.dragged() {
        let delta = response.drag_delta();
        if delta.x != 0.0 {
            // A pixel is a pixel's worth of the axis.
            let hz = axes.hz(axes.x(panel.value("cutoff")) + delta.x);
            panel.set_id("cutoff", hz);
        }
        if delta.y != 0.0 {
            panel.set_id("resonance", resonance - delta.y * 0.5);
        }
    }
    if response.drag_stopped() {
        panel.finish();
    }
    response.on_hover_text("Drag sideways for the cutoff, up or down for the resonance");
}
