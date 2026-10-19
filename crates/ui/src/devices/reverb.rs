//! The reverb's panel: the type above the reverb's display and knobs (see
//! `space.rs`, which draws what any type does to a hit), and the tail's
//! modulation beside them.

use eframe::egui::Ui;

use super::Panel;

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    ui.horizontal(|ui| {
        ui.weak("Type");
        panel.choice(ui, "type");
    });
    ui.horizontal_top(|ui| {
        super::space::reverb(ui, panel);
        ui.vertical(|ui| {
            panel.knob_as(ui, "mod_depth", Some("Mod"));
            panel.knob_as(ui, "mod_rate", Some("Mod rate"));
        });
    });
}
