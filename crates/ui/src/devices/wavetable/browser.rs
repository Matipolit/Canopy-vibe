//! The preset browser: the synth's factory presets by category (their
//! names say it, "Bass: Reese") and the user's own, with a search; a
//! click loads one (as one undo step). Below, the current sound saves
//! under a name, as the Presets menu by the instrument picker does.

use std::collections::BTreeMap;

use eframe::egui::{self, RichText, Ui};

use super::super::Panel;
use super::oscillators::track_of;
use crate::session::PresetTarget;
use crate::theme::{size, space};

/// The browser's state between frames: the category picked, the search,
/// the name being typed to save, and the preset last loaded.
#[derive(Debug, Clone, Default)]
struct State {
    category: Option<String>,
    search: String,
    name: String,
    loaded: Option<String>,
}

/// What can be loaded.
enum Entry {
    Factory(&'static str, BTreeMap<String, f32>),
    User(crate::presets::Preset),
}

impl Entry {
    fn name(&self) -> &str {
        match self {
            Entry::Factory(name, _) => name,
            Entry::User(preset) => &preset.name,
        }
    }

    /// The category a preset's name gives ("Bass" of "Bass: Reese"), or
    /// "User" for the user's own.
    fn category(&self) -> &str {
        match self {
            Entry::Factory(name, _) => name.split_once(": ").map_or("Other", |(c, _)| c),
            Entry::User(_) => "User",
        }
    }

    /// The name without its category.
    fn short_name(&self) -> &str {
        match self {
            Entry::Factory(name, _) => name.split_once(": ").map_or(name, |(_, n)| n),
            Entry::User(preset) => &preset.name,
        }
    }
}

/// The browser's width, beside the macros.
const WIDTH: f32 = 700.0;

pub(super) fn page(ui: &mut Ui, panel: &mut Panel) {
    let Some(track) = track_of(panel) else {
        return;
    };
    let target = PresetTarget::Instrument(track);
    let id = panel.id("browser");
    let mut state: State = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let mut entries: Vec<Entry> = panel
        .session
        .factory_presets(target)
        .into_iter()
        .map(|(name, values)| Entry::Factory(name, values))
        .collect();
    entries.extend(panel.session.presets(target).into_iter().map(Entry::User));
    let mut categories: Vec<String> = Vec::new();
    for entry in &entries {
        let category = entry.category().to_owned();
        if !categories.contains(&category) {
            categories.push(category);
        }
    }
    ui.vertical(|ui| {
        // As wide as the other pages' cards: the list wraps within it
        // rather than across the whole scrolling view.
        ui.set_width(WIDTH);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.search)
                    .hint_text("Search")
                    .desired_width(150.0),
            );
            if ui
                .selectable_label(state.category.is_none(), "All")
                .clicked()
            {
                state.category = None;
            }
            for category in &categories {
                let selected = state.category.as_deref() == Some(category.as_str());
                if ui.selectable_label(selected, category).clicked() {
                    state.category = Some(category.clone());
                }
            }
        });
        let search = state.search.to_lowercase();
        let shown: Vec<&Entry> = entries
            .iter()
            .filter(|e| state.category.as_deref().is_none_or(|c| e.category() == c))
            .filter(|e| search.is_empty() || e.name().to_lowercase().contains(&search))
            .collect();
        // Room left under the list for the row that saves.
        let save_row = size::CONTROL + space::M + space::XS;
        let height = (ui.available_height() - save_row).max(60.0);
        let mut load = None;
        egui::ScrollArea::vertical()
            .id_salt(panel.id("browser scroll"))
            .max_height(height)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(space::S, space::S);
                    for (i, entry) in shown.iter().enumerate() {
                        let current = state.loaded.as_deref() == Some(entry.name());
                        let text = if state.category.is_none() && entry.category() != "Other" {
                            RichText::new(format!("{} · {}", entry.short_name(), entry.category()))
                        } else {
                            RichText::new(entry.short_name())
                        };
                        let button = egui::Button::new(text)
                            .selected(current)
                            .min_size(egui::vec2(130.0, size::CONTROL));
                        if ui.add(button).clicked() {
                            load = Some(i);
                        }
                    }
                    if shown.is_empty() {
                        ui.weak("No presets match");
                    }
                });
            });
        if let Some(i) = load {
            let entry = shown[i];
            state.loaded = Some(entry.name().to_owned());
            match entry {
                Entry::Factory(_, values) => {
                    panel.session.load_factory_preset(target, values.clone())
                }
                Entry::User(preset) => panel.session.load_preset(target, preset),
            }
        }
        ui.horizontal(|ui| {
            ui.weak("Save as");
            let field = ui.add(
                egui::TextEdit::singleline(&mut state.name)
                    .hint_text("Name")
                    .desired_width(150.0),
            );
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let named = !state.name.trim().is_empty();
            if (ui.add_enabled(named, egui::Button::new("Save")).clicked() || enter) && named {
                panel.session.save_preset(target, state.name.trim());
                state.loaded = Some(state.name.trim().to_owned());
                state.name.clear();
            }
            ui.separator();
            if ui
                .button("Init")
                .on_hover_text("Back to the default patch")
                .clicked()
            {
                panel.session.reset_device(target);
                state.loaded = None;
            }
        });
    });
    ui.data_mut(|d| d.insert_temp(id, state));
}
