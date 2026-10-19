//! A plugin device's panel and title-bar controls: the button that opens
//! its own editor, the parameter its editor last moved, its parameters as
//! knobs and choices with the plugin's own value text (searchable, for
//! plugins with hundreds), and what shows when it isn't there.

use eframe::egui::{self, RichText, Ui};
use hosting::{Owner, Status};
use instruments::{Format, ParameterKind};
use project::{Device, PluginData, Track, TrackId, TrackKind};

use crate::devices::Panel;
use crate::session::Session;
use crate::theme::{self, space};
use crate::widgets::Knob;
use crate::widgets::chrome;

/// How many parameters a panel shows at once; more are found by searching.
const SHOWN: usize = 24;
/// Knobs in a row.
const PER_ROW: usize = 8;
/// More parameters than this get a search box.
const SEARCHABLE: usize = 12;

/// The body of a plugin's panel (an effect's or an instrument's): a note
/// if it isn't running, then its parameters.
pub(crate) fn panel(ui: &mut Ui, panel: &mut Panel) {
    let owner = super::owner_of(panel.owner);
    let status = panel.session.plugins.manager.status(owner);
    if status != Status::Loaded {
        let data = super::plugin_data(&panel.session.project, owner).cloned();
        let note = absence(panel.session, owner, data.as_ref(), panel.format);
        ui.colored_label(ui.visuals().warn_fg_color, note);
    }
    let search_id = panel.id("search");
    let mut search: String = ui.data(|d| d.get_temp(search_id)).unwrap_or_default();
    if panel.params.len() > SEARCHABLE {
        ui.horizontal(|ui| {
            let field = egui::TextEdit::singleline(&mut search)
                .hint_text(format!("Search {} parameters", panel.params.len()))
                .desired_width(180.0);
            if ui.add(field).changed() {
                ui.data_mut(|d| d.insert_temp(search_id, search.clone()));
            }
        });
    }
    let needle = search.trim().to_lowercase();
    let matching: Vec<usize> = (0..panel.params.len())
        .filter(|&i| needle.is_empty() || panel.params[i].name.to_lowercase().contains(&needle))
        .collect();
    let (choices, knobs): (Vec<usize>, Vec<usize>) = matching
        .iter()
        .take(SHOWN)
        .partition(|&&i| matches!(panel.params[i].kind, ParameterKind::Enum { .. }));
    if !choices.is_empty() {
        egui::Grid::new(panel.id("choices"))
            .num_columns(2)
            .spacing([space::M, space::S])
            .show(ui, |ui| {
                for index in choices {
                    ui.label(panel.params[index].name);
                    panel.choice_at(ui, index);
                    ui.end_row();
                }
            });
    }
    for row in knobs.chunks(PER_ROW) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            for &index in row {
                knob(ui, panel, owner, index);
            }
        });
    }
    let hidden = matching.len().saturating_sub(SHOWN);
    if hidden > 0 {
        ui.weak(format!(
            "{hidden} more: search to find them, or open the plugin's editor"
        ));
    } else if matching.is_empty() && !needle.is_empty() {
        ui.weak("No parameter by that name");
    }
}

/// A knob for parameter `index`, showing the value as the plugin does.
fn knob(ui: &mut Ui, panel: &mut Panel, owner: Owner, index: usize) {
    let param = panel.params[index];
    let value = panel.values()[index];
    let text = panel
        .session
        .plugins
        .manager
        .param_text(owner, index, value);
    let out = Knob::new(panel.id(("knob", index)), &param, value)
        .diameter(panel.knob_size)
        .label_color(panel.name_color(index))
        .text(text.as_deref())
        .show(ui);
    if let Some(value) = out.changed {
        panel.set(index, value);
    }
    panel.attach(&out.response, index);
}

/// Why `owner`'s plugin isn't playing, in a sentence.
pub(crate) fn absence(
    session: &Session,
    owner: Owner,
    data: Option<&PluginData>,
    format: Format,
) -> String {
    let name = match data {
        Some(data) if !data.vendor.is_empty() => {
            format!("{} ({}, {})", data.name, data.vendor, format.label())
        }
        Some(data) => format!("{} ({})", data.name, format.label()),
        None => format!("This {} plugin", format.label()),
    };
    let silent = match owner {
        Owner::Effect(_) => "the sound passes through",
        Owner::Instrument(_) => "this track is silent",
    };
    match session.plugins.manager.status(owner) {
        Status::Loaded => String::new(),
        Status::Scanning => format!("Looking for {name}…"),
        Status::NotInstalled => format!("{name} isn't installed, so {silent}."),
        Status::Failed(err) => format!("{name} wouldn't load ({err}), so {silent}."),
    }
}

/// What shows in place of an effect this build can't load, if it's a
/// plugin: who makes it, why it's missing, and what to do about it. False
/// if it isn't one, for the caller's own note.
pub fn missing(
    ui: &mut Ui,
    session: &mut Session,
    _owner: Option<TrackId>,
    device: &Device,
) -> bool {
    if device.effect.format.is_builtin() {
        return false;
    }
    let owner = Owner::Effect(device.id);
    let note = absence(session, owner, device.plugin.as_ref(), device.effect.format);
    ui.colored_label(ui.visuals().warn_fg_color, note);
    ui.horizontal(|ui| {
        if ui.button("Plugin Manager…").clicked() {
            session.plugins.window = true;
        }
        if ui.button("Remove").clicked() {
            let id = device.id;
            session.edit("Remove Effect", |p| p.remove_device(id));
        }
    });
    true
}

/// Controls on an effect's title bar for plugins: opening its editor, the
/// parameter its editor last touched. Draws nothing for built-ins.
pub fn title_bar_extras(
    ui: &mut Ui,
    session: &mut Session,
    owner: Option<TrackId>,
    device: &Device,
) {
    if device.effect.format.is_builtin() {
        return;
    }
    let track = owner
        .and_then(|t| session.project.track(t))
        .map_or("Master", |t| t.name.as_str());
    let title = format!("{track} — {}", device.name());
    extras(ui, session, Owner::Effect(device.id), &title);
}

/// The same for a track's plugin instrument, beside its picker.
pub(crate) fn instrument_bar_extras(ui: &mut Ui, session: &mut Session, track: &Track) {
    let TrackKind::Midi {
        instrument, plugin, ..
    } = &track.kind
    else {
        return;
    };
    if instrument.format.is_builtin() {
        return;
    }
    let name = plugin
        .as_ref()
        .map_or(instrument.id.as_str(), |p| p.name.as_str());
    let title = format!("{} — {name}", track.name);
    extras(ui, session, Owner::Instrument(track.id), &title);
}

fn extras(ui: &mut Ui, session: &mut Session, owner: Owner, title: &str) {
    let manager = &mut session.plugins.manager;
    if manager.has_editor(owner) {
        let open = manager.editor_is_open(owner);
        let lit = theme::accent(ui.visuals()).gamma_multiply(0.8);
        if chrome::toggle(
            ui,
            "Edit",
            open,
            lit,
            "Open the plugin's own editor in a window",
        )
        .clicked()
            && let Err(err) = manager.toggle_editor(owner, title)
        {
            session.error(format!("Couldn't open the editor: {err}"));
        }
    }
    let manager = &session.plugins.manager;
    if let Some(index) = manager.touched(owner)
        && let Some(name) = touched_name(session, owner, index)
    {
        ui.label(RichText::new(format!("Touched: {name}")).small().weak())
            .on_hover_text(
                "The parameter the plugin's editor moved last: automate it from the track's lanes",
            );
    }
}

/// The name of parameter `index` of `owner`'s plugin.
fn touched_name(session: &Session, owner: Owner, index: usize) -> Option<&'static str> {
    let params = match owner {
        Owner::Effect(device) => session.project.device(device)?.effect.descriptor()?.params,
        Owner::Instrument(track) => match &session.project.track(track)?.kind {
            TrackKind::Midi { instrument, .. } => instrument.descriptor()?.params,
            _ => return None,
        },
    };
    params.get(index).map(|p| p.name)
}
