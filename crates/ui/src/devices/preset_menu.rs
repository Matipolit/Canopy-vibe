//! The presets menu of an effect (in its name's right-click menu) or an
//! instrument (its Presets button): saved presets to load, Save Preset…,
//! and Default Settings; and the Rack menu of a track (or the master),
//! for its instrument and effects together. Saving asks for a name in a
//! small window.

use eframe::egui::{self, Ui};
use project::TrackId;

use crate::session::{PresetTarget, Session};

#[derive(Debug, Clone)]
struct SaveDialog {
    saving: Saving,
    name: String,
}

/// What a name is being asked for.
#[derive(Debug, Clone, Copy)]
enum Saving {
    Preset(PresetTarget),
    /// A track's rack, or the master's for `None`.
    Rack(Option<TrackId>),
}

fn ask_name(ui: &Ui, saving: Saving) {
    let dialog = SaveDialog {
        saving,
        name: String::new(),
    };
    ui.ctx().data_mut(|d| d.insert_temp(dialog_id(), dialog));
}

fn dialog_id() -> egui::Id {
    egui::Id::new("save_preset_dialog")
}

/// The menu's contents.
pub(crate) fn menu(ui: &mut Ui, session: &mut Session, target: PresetTarget) {
    let presets = session.presets(target);
    let factory = session.factory_presets(target);
    ui.add_enabled_ui(!presets.is_empty() || !factory.is_empty(), |ui| {
        ui.menu_button("Load Preset", |ui| {
            for (name, values) in factory {
                if ui.button(name).clicked() {
                    session.load_factory_preset(target, values);
                    ui.close();
                }
            }
            if !presets.is_empty() {
                ui.separator();
            }
            for preset in &presets {
                if ui.button(&preset.name).clicked() {
                    session.load_preset(target, preset);
                    ui.close();
                }
            }
        });
    });
    if ui.button("Save Preset…").clicked() {
        ask_name(ui, Saving::Preset(target));
        ui.close();
    }
    if ui.button("Default Settings").clicked() {
        session.reset_device(target);
        ui.close();
    }
}

/// The Rack button of a track (`None`: the master): its instrument and
/// effects, saved together or replaced by a saved rack.
pub(crate) fn rack_button(ui: &mut Ui, session: &mut Session, owner: Option<TrackId>) {
    ui.menu_button("Rack", |ui| {
        let racks = session.racks();
        ui.add_enabled_ui(!racks.is_empty(), |ui| {
            ui.menu_button("Load Rack", |ui| {
                for rack in &racks {
                    if ui.button(&rack.name).clicked() {
                        session.load_rack(owner, rack);
                        ui.close();
                    }
                }
            });
        })
        .response
        .on_disabled_hover_text("No racks saved yet");
        if ui
            .button("Save Rack…")
            .on_hover_text("The instrument and every effect, as one preset")
            .clicked()
        {
            ask_name(ui, Saving::Rack(owner));
            ui.close();
        }
    })
    .response
    .on_hover_text("Save or load the instrument and effects together");
}

/// The window that names a preset being saved. Call once per frame.
pub(crate) fn save_window(ctx: &egui::Context, session: &mut Session) {
    let Some(mut dialog) = ctx.data(|d| d.get_temp::<SaveDialog>(dialog_id())) else {
        return;
    };
    let mut open = true;
    let mut done = false;
    let title = match dialog.saving {
        Saving::Preset(_) => "Save Preset",
        Saving::Rack(_) => "Save Rack",
    };
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            // Read before the field, which takes the key for itself.
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let field = ui.add(
                egui::TextEdit::singleline(&mut dialog.name)
                    .hint_text("Name")
                    .desired_width(220.0),
            );
            field.request_focus();
            let named = !dialog.name.trim().is_empty();
            let buttons = crate::widgets::chrome::dialog_buttons(ui, "Save", named, None, "Cancel");
            if (buttons.primary.clicked() || enter) && named {
                match dialog.saving {
                    Saving::Preset(target) => session.save_preset(target, &dialog.name),
                    Saving::Rack(owner) => session.save_rack(owner, &dialog.name),
                }
                done = true;
            }
            if buttons.cancel.clicked() {
                done = true;
            }
        });
    if done || !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        ctx.data_mut(|d| d.remove::<SaveDialog>(dialog_id()));
    } else {
        ctx.data_mut(|d| d.insert_temp(dialog_id(), dialog));
    }
}
