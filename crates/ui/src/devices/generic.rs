//! The panel of a device without one of its own: its choices, then a knob
//! per number in as many rows as the height allows, then its envelopes.

use eframe::egui::{self, Grid, Ui};
use instruments::ParameterKind;

use super::Panel;

/// How tall a knob row is, with its name and value.
pub(super) fn knob_height(panel: &Panel) -> f32 {
    panel.knob_size + super::panel::KNOB_TEXT
}

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let mut knobs = Vec::new();
    let mut choices = Vec::new();
    for (index, param) in panel.params.iter().enumerate() {
        if panel.in_envelope(index) {
            continue;
        }
        match param.kind {
            ParameterKind::Float { .. } => knobs.push(param.id),
            ParameterKind::Enum { .. } => choices.push((param.id, param.name)),
        }
    }
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            if !choices.is_empty() {
                Grid::new(panel.id("choices"))
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        for (id, name) in &choices {
                            ui.label(*name);
                            panel.choice(ui, id);
                            ui.end_row();
                        }
                    });
            }
            knob_rows(ui, panel, &knobs);
        });
        let envelopes = panel.envelopes.len();
        for n in 0..envelopes {
            ui.separator();
            // Capped, so a tall view leaves room beside it for the effects.
            let height = (ui.available_height() - 8.0).clamp(100.0, 150.0);
            panel.envelope(ui, n, egui::vec2(height * 2.6, height));
        }
    });
}

/// Knobs in rows of up to six (more rows only if they fit the height),
/// filled evenly.
pub(super) fn knob_rows(ui: &mut Ui, panel: &mut Panel, ids: &[&str]) {
    if ids.is_empty() {
        return;
    }
    let fit = ((ui.available_height() / knob_height(panel)).floor() as usize).max(1);
    let rows = ids.len().div_ceil(6).clamp(1, fit);
    let per_row = ids.len().div_ceil(rows);
    for row in ids.chunks(per_row) {
        panel.knobs(ui, row);
    }
}
