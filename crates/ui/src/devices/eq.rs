//! The EQ's panel: its curve over the sound's spectrum, a chip per band
//! (and one to add a band), and the selected band's controls.

use effects::eq::{self, BandType};
use eframe::egui::{self, RichText, Sense, Ui, vec2};
use engine::TapFrame;

use super::Panel;
use super::panel::KNOB_TEXT;
use crate::widgets::EqCurve;
use crate::widgets::eq_curve::{self, BandParams, Spectra, band_color, type_index, type_name};
use crate::widgets::scopes;
use crate::widgets::spectrum::{Spectrum, SpectrumSettings};

/// The spectra behind the curve, kept between frames.
#[derive(Clone, Default)]
struct Analysis {
    before: Spectrum,
    after: Spectrum,
    pass: u64,
}

fn spectrum_settings() -> SpectrumSettings {
    SpectrumSettings {
        floor_db: -90.0,
        ceiling_db: 6.0,
        fall: 30.0,
        ..SpectrumSettings::default()
    }
}

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let bands = eq_curve::bands(panel.params);
    let value = |panel: &Panel, index: Option<usize>| index.map(|i| panel.values()[i]);
    let on = |panel: &Panel, band: &BandParams| value(panel, band.on).unwrap_or(0.0) >= 0.5;
    let selected_id = panel.id("selected");
    let mut selected: usize = ui.data(|d| d.get_temp(selected_id)).unwrap_or(usize::MAX);
    if !bands.get(selected).is_some_and(|b| on(panel, b)) {
        selected = bands.iter().position(|b| on(panel, b)).unwrap_or(0);
    }

    // The sound before and after, analyzed once per frame.
    let settings = spectrum_settings();
    let state_id = panel.id("spectra");
    let mut spectra: Analysis = ui.data_mut(|d| d.remove_temp(state_id)).unwrap_or_default();
    let pass = ui.ctx().cumulative_pass_nr();
    if spectra.pass != pass {
        spectra.pass = pass;
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        if let Some(tap) = panel.tap() {
            let frames: Vec<TapFrame> = tap.latest(settings.size).collect();
            let before: Vec<f32> = frames.iter().map(|f| 0.5 * (f[0] + f[1])).collect();
            let after: Vec<f32> = frames.iter().map(|f| 0.5 * (f[2] + f[3])).collect();
            spectra.before.update(&before, &settings, dt);
            spectra.after.update(&after, &settings, dt);
        }
    }

    // The live dynamic offsets and output level, if this EQ has them.
    let meter_names = eq::DESCRIPTOR.meters.len();
    let offsets: Vec<f32> = if meter_names >= eq::BANDS {
        panel.meters[..eq::BANDS.min(panel.meters.len())].to_vec()
    } else {
        Vec::new()
    };
    let output = (meter_names >= eq::BANDS + 2)
        .then(|| [panel.meters[eq::BANDS], panel.meters[eq::BANDS + 1]]);

    let height =
        (ui.available_height() - (panel.knob_size + KNOB_TEXT) - super::panel::CONTROL_ROW)
            .clamp(100.0, 260.0);
    let values = panel.values().to_vec();
    ui.horizontal_top(|ui| {
        let response = EqCurve::new(panel.id("curve"), panel.params, &values, panel.sample_rate)
            .size(vec2(height * 2.8, height))
            .offsets(&offsets)
            .spectra(Spectra {
                before: &spectra.before,
                after: &spectra.after,
                settings: &settings,
            })
            .selected(selected)
            .show(ui);
        for (index, value) in &response.changes {
            panel.set(*index, *value);
        }
        if response.finished {
            panel.finish();
        }
        selected = response.selected;
        if let Some([left, right]) = output {
            let (rect, _) = ui.allocate_exact_size(
                vec2(crate::widgets::scopes::level_bars_width(2), height),
                Sense::hover(),
            );
            scopes::level_bars(ui.painter(), rect, &[left, right], -60.0, 6.0, ui.visuals());
        }
    });

    // A chip per band, and one to add a band.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
        for (index, band) in bands.iter().enumerate() {
            if !on(panel, band) {
                continue;
            }
            let text = RichText::new(format!("{}", index + 1))
                .color(band_color(index))
                .strong();
            if ui
                .selectable_label(index == selected, text)
                .on_hover_text(eq_curve::band_name(index))
                .clicked()
            {
                selected = index;
            }
        }
        let free = bands.iter().position(|b| !on(panel, b));
        if ui
            .add_enabled(free.is_some(), egui::Button::new("+ Band"))
            .on_hover_text("Add a band where there's room (or double-click the curve)")
            .clicked()
            && let Some(free) = free
        {
            add_band(panel, &bands, free);
            selected = free;
        }
    });

    // The selected band's controls, then the EQ's own.
    if let Some(band) = bands.get(selected).copied().filter(|b| on(panel, b)) {
        let n = selected + 1;
        let kind = BandType::from_index(value(panel, band.kind).unwrap_or(2.0));
        let dynamic = value(panel, band.dynamic).unwrap_or(0.0) >= 0.5;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(eq_curve::band_name(selected))
                        .color(band_color(selected))
                        .strong(),
                );
                type_menu(ui, panel, &band, kind);
                if band.dynamic.is_some() {
                    panel.toggle(ui, &format!("b{n}_dyn"), "Dynamic");
                }
            });
            panel.knob_as(ui, &format!("b{n}_freq"), Some("Frequency"));
            if kind.has_gain() {
                panel.knob_as(ui, &format!("b{n}_gain"), Some("Gain"));
            }
            panel.knob_as(ui, &format!("b{n}_q"), Some("Q"));
            if kind.has_slope() {
                ui.vertical(|ui| {
                    ui.weak("Slope");
                    panel.choice_menu(ui, &format!("b{n}_slope"));
                });
            }
            if dynamic {
                panel.knob_as(ui, &format!("b{n}_threshold"), Some("Threshold"));
                panel.knob_as(ui, &format!("b{n}_range"), Some("Range"));
            }
            ui.separator();
            let any_dynamic = bands
                .iter()
                .any(|b| on(panel, b) && value(panel, b.dynamic).unwrap_or(0.0) >= 0.5);
            if any_dynamic {
                panel.knob(ui, "attack");
                panel.knob(ui, "release");
            }
            panel.knob(ui, "output");
        });
    }
    ui.data_mut(|d| {
        d.insert_temp(selected_id, selected);
        d.insert_temp(state_id, spectra);
    });
}

/// The band's type as a menu.
fn type_menu(ui: &mut Ui, panel: &mut Panel, band: &BandParams, kind: BandType) {
    let Some(index) = band.kind else {
        return;
    };
    let mut chosen = None;
    ui.menu_button(type_name(kind), |ui| {
        for choice in BandType::ALL {
            if ui
                .selectable_label(choice == kind, type_name(choice))
                .clicked()
            {
                chosen = Some(choice);
                ui.close();
            }
        }
    });
    if let Some(choice) = chosen {
        panel.set(index, type_index(choice));
        panel.finish();
    }
}

/// Turns band `free` on as a flat bell in the widest gap between the bands
/// there are, so it's easy to grab.
fn add_band(panel: &mut Panel, bands: &[BandParams], free: usize) {
    let mut taken: Vec<f32> = bands
        .iter()
        .filter(|b| b.on.is_some_and(|i| panel.values()[i] >= 0.5))
        .filter_map(|b| b.freq.map(|i| panel.values()[i].ln()))
        .collect();
    taken.push(40f32.ln());
    taken.push(12_000f32.ln());
    taken.sort_by(f32::total_cmp);
    let (low, high) = taken
        .windows(2)
        .map(|w| (w[0], w[1]))
        .max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)))
        .unwrap_or((40f32.ln(), 12_000f32.ln()));
    let band = bands[free];
    let set = |panel: &mut Panel, index: Option<usize>, value: f32| {
        if let Some(index) = index {
            panel.set(index, value);
        }
    };
    set(panel, band.kind, type_index(BandType::Bell));
    set(panel, band.freq, ((low + high) / 2.0).exp());
    set(panel, band.gain, 0.0);
    set(panel, band.q, 1.0);
    set(panel, band.on, 1.0);
    panel.finish();
}
