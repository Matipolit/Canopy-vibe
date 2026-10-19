//! Where plugins are picked: the "Plugins" part of the "+ Effect" menu,
//! and the instrument picker.

use ::plugins::{PluginInfo, PluginKind};
use eframe::egui::{self, RichText, Ui};
use project::{DeviceId, Track, TrackId, TrackKind};

use crate::devices;
use crate::session::{PresetTarget, Session};

/// The plugins in the "+ Effect" menu of `owner`'s chain (`None`: the
/// master's), before the built-ins, by vendor. Returns the effect it added,
/// if any.
pub fn effect_menu(ui: &mut Ui, session: &mut Session, owner: Option<TrackId>) -> Option<DeviceId> {
    let picked = plugin_menu(ui, session, PluginKind::Effect);
    ui.separator();
    let info = picked?;
    let id = super::add_effect(session, owner, &info);
    ui.close();
    id
}

/// A "Plugins" submenu of the plugins of `kind`, by vendor; the one
/// clicked, if any.
fn plugin_menu(ui: &mut Ui, session: &mut Session, kind: PluginKind) -> Option<PluginInfo> {
    let catalog = session.plugins.manager.catalog();
    let plugins: Vec<PluginInfo> = catalog
        .of_kind(kind)
        .filter(|p| hosting::usable(p))
        .cloned()
        .collect();
    let scanning = catalog.is_scanning();
    let mut picked = None;
    let mut open_manager = false;
    ui.menu_button("Plugins", |ui| {
        if plugins.is_empty() {
            ui.label(
                RichText::new(if scanning {
                    "Looking for plugins…"
                } else {
                    "No plugins found"
                })
                .color(ui.visuals().weak_text_color()),
            );
        }
        let mut vendors: Vec<&str> = plugins.iter().map(|p| p.vendor.as_str()).collect();
        vendors.dedup();
        for vendor in vendors {
            let name = if vendor.is_empty() { "Unknown" } else { vendor };
            ui.menu_button(name, |ui| {
                for info in plugins.iter().filter(|p| p.vendor == vendor) {
                    let label = format!("{}  ({})", info.name, info.format.name());
                    let hover = format!(
                        "{} {}{}\n{}",
                        info.name,
                        info.version,
                        if info.bridged {
                            ", through yabridge"
                        } else {
                            ""
                        },
                        info.path.display()
                    );
                    if ui.button(label).on_hover_text(hover).clicked() {
                        picked = Some(info.clone());
                        ui.close();
                    }
                }
            });
        }
        ui.separator();
        if ui.button("Plugin Manager…").clicked() {
            open_manager = true;
            ui.close();
        }
    });
    if open_manager {
        session.plugins.window = true;
    }
    picked
}

/// The instrument box at the top of a MIDI track's device view: which
/// instrument it plays (a built-in, or a plugin from the "Plugins" menu),
/// and its presets.
pub fn instrument_picker(ui: &mut Ui, session: &mut Session, track: &Track) {
    let TrackKind::Midi {
        instrument, plugin, ..
    } = &track.kind
    else {
        return;
    };
    let current = instrument.descriptor();
    let shown = current.map_or_else(
        || {
            plugin
                .as_ref()
                .map_or_else(|| "Unavailable instrument".to_owned(), |p| p.name.clone())
        },
        |d| d.name.to_owned(),
    );
    egui::ComboBox::from_id_salt(("instrument", track.id.0))
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for descriptor in instruments::builtins() {
                let selected = instrument.format.is_builtin() && instrument.id == descriptor.id;
                if ui
                    .selectable_label(selected, descriptor.name)
                    .on_hover_text(descriptor.description)
                    .clicked()
                    && !selected
                {
                    let reference = instruments::InstrumentRef::builtin(descriptor.id);
                    session.edit("Change Instrument", |p| {
                        p.set_instrument(track.id, reference)
                    });
                }
            }
        })
        .response
        .on_hover_text(current.map_or("", |d| d.description));
    if let Some(info) = plugin_menu(ui, session, PluginKind::Instrument) {
        super::set_instrument(session, track.id, &info);
    }
    if current.is_none() {
        let note = if instrument.format.is_builtin() {
            format!(
                "{} isn't available, so this track is silent.",
                instrument.id
            )
        } else {
            super::panel::absence(
                session,
                hosting::Owner::Instrument(track.id),
                plugin.as_ref(),
                instrument.format,
            )
        };
        ui.colored_label(ui.visuals().warn_fg_color, note);
    } else {
        ui.menu_button("Presets", |ui| {
            let target = PresetTarget::Instrument(track.id);
            devices::preset_menu::menu(ui, session, target);
        });
        super::instrument_bar_extras(ui, session, track);
    }
}
