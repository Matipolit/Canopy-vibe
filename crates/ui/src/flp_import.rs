//! File → Import FL Studio Project…: reads an `.flp`, shows what's in it
//! and what each FL instrument and effect becomes (the user can change
//! that), and on Import replaces the project with a new, unsaved one.
//!
//! Third-party plugins installed here come in as themselves, with the
//! settings FL saved, unless the user picks a built-in instead; the ones
//! that aren't get the built-in their name suggests.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use eframe::egui;
use flp::plugins;
use flp::{AudioInfo, Choice, Choices, Files, Import, Options, Plan, PlanItem, Role};

use crate::session::Session;
use crate::widgets::chrome;

/// The Import FL Studio Project window.
pub struct Dialog {
    path: PathBuf,
    name: String,
    fl: flp::Project,
    plan: Plan,
    choices: Choices,
    files: Files,
    options: Options,
    /// The import with the current choices: what the summary shows and
    /// what Import puts in place.
    preview: Import,
}

pub enum Outcome {
    Open,
    Cancel,
    Import,
}

impl Dialog {
    /// Asks which file to import; `None` if the user cancelled.
    pub fn pick() -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Import FL Studio project")
            .add_filter("FL Studio project", &["flp"])
            .pick_file()
    }

    /// Where the files the import makes go (SFZs that let FL Sampler
    /// channels play, Vital's wavetables): canopy's data folder, one folder
    /// per imported project.
    pub fn default_sfz_dir(path: &Path) -> Option<PathBuf> {
        let stem = path.file_stem()?.to_string_lossy().into_owned();
        eframe::storage_dir("canopy").map(|dir| dir.join("flp-import").join(stem))
    }

    /// Reads `path` and finds the samples it uses. Fails for files that
    /// aren't FL Studio projects or can't be read. `installed` are the
    /// plugins found here ([`installed`]).
    pub fn open(
        path: &Path,
        sfz_dir: Option<PathBuf>,
        installed: HashSet<flp::PluginId>,
    ) -> Result<Dialog, String> {
        let bytes = std::fs::read(path)
            .map_err(|err| format!("Couldn't open {}: {err}", path.display()))?;
        let fl = flp::parse(&bytes)
            .map_err(|err| format!("Couldn't import {}: {err}", path.display()))?;
        let files = Files::find(&fl, Some(path), &probe);
        let plan = Plan::new(&fl);
        let mut choices = plan.choices();
        for item in &plan.items {
            if item.plugins.iter().any(|p| installed.contains(p)) {
                choices.set(item.role, &item.name, Choice::Plugin);
            }
        }
        let options = Options { sfz_dir, installed };
        let preview = flp::import(&fl, &choices, &files, &options);
        let name = path.file_stem().map_or_else(
            || path.display().to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        Ok(Dialog {
            path: path.to_owned(),
            name,
            fl,
            plan,
            choices,
            files,
            options,
            preview,
        })
    }

    #[cfg(test)]
    pub(crate) fn preview(&self) -> &Import {
        &self.preview
    }

    #[cfg(test)]
    pub(crate) fn plan(&self) -> &Plan {
        &self.plan
    }

    /// What `item` becomes, as chosen.
    #[cfg(test)]
    pub(crate) fn choice(&self, item: &PlanItem) -> Choice {
        self.choices
            .get(item.role, &item.name)
            .unwrap_or(item.suggestion)
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Outcome {
        let mut outcome = Outcome::Open;
        let mut changed = false;
        let Dialog {
            path,
            name,
            plan,
            choices,
            preview,
            options,
            ..
        } = self;
        egui::Window::new("Import FL Studio Project")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.strong(name.as_str())
                    .on_hover_text(path.display().to_string());
                summary(ui, preview);
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("flp_plan")
                    .max_height(300.0)
                    .show(ui, |ui| {
                        egui::Grid::new("flp_replacements")
                            .num_columns(2)
                            .spacing([16.0, 4.0])
                            .show(ui, |ui| {
                                for role in [Role::Instrument, Role::Effect] {
                                    changed |=
                                        replacements(ui, plan, choices, &options.installed, role);
                                }
                            });
                    });
                if !preview.report.is_empty() {
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt("flp_report")
                        .max_height(120.0)
                        .show(ui, |ui| {
                            for line in &preview.report {
                                ui.weak(line);
                            }
                        });
                }
                ui.separator();
                let buttons = chrome::dialog_buttons(ui, "Import", true, None, "Cancel");
                if buttons
                    .primary
                    .on_hover_text("As a new, unsaved project")
                    .clicked()
                {
                    outcome = Outcome::Import;
                }
                if buttons.cancel.clicked() {
                    outcome = Outcome::Cancel;
                }
            });
        if changed {
            self.preview = flp::import(&self.fl, &self.choices, &self.files, &self.options);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            outcome = Outcome::Cancel;
        }
        outcome
    }

    /// Replaces the session's project with the import, the way File → Open
    /// does (fresh undo history and engine state), but with no file behind
    /// it: Save asks where, and closing asks first.
    pub fn import_into(self, session: &mut Session) {
        let Dialog { name, preview, .. } = self;
        let mut unwritten = 0;
        for file in &preview.files {
            let written = file
                .path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&file.path, &file.contents));
            if written.is_err() {
                unwritten += 1;
            }
        }
        let text = match preview.project.to_file_string() {
            Ok(text) => text,
            Err(err) => return session.error(format!("Couldn't import {name}: {err}")),
        };
        // Session replaces projects by opening them, so open a copy.
        let temp = temp_project_path();
        if let Err(err) = std::fs::write(&temp, text) {
            return session.error(format!("Couldn't import {name}: {err}"));
        }
        session.open(&temp);
        let _ = std::fs::remove_file(&temp);
        if session.path.as_deref() != Some(temp.as_path()) {
            return; // open reported why
        }
        session.path = None;
        // Count it as a change, so it isn't lost without asking.
        let region = session.project.loop_region;
        session.set_loop(region);
        if unwritten > 0 {
            session.error(format!(
                "Imported {name}, but couldn't write {unwritten} sampler or wavetable files"
            ));
        } else {
            session.info(format!("Imported {name}"));
        }
    }
}

/// What the import holds, in numbers.
fn summary(ui: &mut egui::Ui, preview: &Import) {
    let s = preview.summary;
    egui::Grid::new("flp_summary")
        .num_columns(2)
        .spacing([24.0, 2.0])
        .show(ui, |ui| {
            let mut row = |label: &str, value: String| {
                ui.label(label);
                ui.monospace(value);
                ui.end_row();
            };
            row("Tempo", format!("{:.2} BPM", s.tempo));
            row("Tracks", s.tracks.to_string());
            row(
                "MIDI clips",
                format!("{} ({} notes)", s.midi_clips, s.notes),
            );
            row("Audio clips", s.audio_clips.to_string());
            row("Effects", s.effects.to_string());
            row("Automation", format!("{} lanes", s.automation_lanes));
        });
}

/// The plugins scanning found here, as the importer names them.
pub fn installed(catalog: &hosting::Catalog) -> HashSet<flp::PluginId> {
    catalog
        .plugins()
        .iter()
        .map(|info| flp::PluginId {
            format: match info.format {
                ::plugins::PluginFormat::Clap => flp::PluginFormat::Clap,
                ::plugins::PluginFormat::Vst3 => flp::PluginFormat::Vst3,
                ::plugins::PluginFormat::Vst2 => flp::PluginFormat::Vst2,
            },
            id: info.id.clone(),
        })
        .collect()
}

/// What the menu calls [`Choice::Plugin`] for `item`: the formats of it
/// installed here, and a note when some of FL's aren't.
fn hosted_label(
    item: &PlanItem,
    installed: &HashSet<flp::PluginId>,
) -> (String, Option<&'static str>) {
    let mut formats: Vec<&str> = item
        .plugins
        .iter()
        .filter(|p| installed.contains(p))
        .map(|p| match p.format {
            flp::PluginFormat::Clap => "CLAP",
            flp::PluginFormat::Vst3 => "VST3",
            flp::PluginFormat::Vst2 => "VST2",
        })
        .collect();
    formats.dedup();
    let some_missing = item.plugins.iter().any(|p| !installed.contains(p));
    (
        format!("{} ({})", item.name, formats.join(", ")),
        some_missing.then_some(
            "FL used other builds of it too; where those aren't installed, a built-in stands in",
        ),
    )
}

/// One section of the plan, as grid rows: each FL plugin with a menu of
/// what it becomes: the plugin itself where it's installed here, or a
/// built-in. True if the user changed something.
fn replacements(
    ui: &mut egui::Ui,
    plan: &Plan,
    choices: &mut Choices,
    installed: &HashSet<flp::PluginId>,
    role: Role,
) -> bool {
    let items: Vec<_> = plan.items.iter().filter(|i| i.role == role).collect();
    if items.is_empty() {
        return false;
    }
    let (title, options) = match role {
        Role::Instrument => ("Instruments", plugins::instrument_choices()),
        Role::Effect => ("Effects", plugins::effect_choices()),
    };
    let mut changed = false;
    ui.strong(title);
    ui.end_row();
    for item in items {
        let text = match item.used_by.len() {
            1 => item.name.clone(),
            n => format!("{} ×{n}", item.name),
        };
        let mut used = item.used_by.join(", ");
        let label = if item.recognized {
            ui.label(text)
        } else {
            used.push_str("\nNo match: a guess");
            ui.colored_label(ui.visuals().warn_fg_color, text)
        };
        label.on_hover_text(used);
        let current = choices
            .get(item.role, &item.name)
            .unwrap_or(item.suggestion);
        let hostable = item.plugins.iter().any(|p| installed.contains(p));
        let (hosted, note) = hosted_label(item, installed);
        let label_of = |choice: Choice| match choice {
            Choice::Plugin => hosted.clone(),
            other => other.label().to_owned(),
        };
        let offered: Vec<Choice> = hostable
            .then_some(Choice::Plugin)
            .into_iter()
            .chain(options.iter().copied())
            .collect();
        let button = ui.menu_button(label_of(current), |ui| {
            for option in &offered {
                if ui
                    .selectable_label(*option == current, label_of(*option))
                    .clicked()
                {
                    if *option != current {
                        choices.set(item.role, &item.name, *option);
                        changed = true;
                    }
                    ui.close();
                }
            }
        });
        if current == Choice::Plugin {
            let hover = match note {
                Some(note) => format!("The installed plugin, with the settings FL saved. {note}"),
                None => "The installed plugin, with the settings FL saved".to_owned(),
            };
            button.response.on_hover_text(hover);
        }
        ui.end_row();
    }
    changed
}

/// Reads an audio file's format, for samples the project uses.
fn probe(path: &Path) -> Option<AudioInfo> {
    let (buffer, channels) = media::decode(path).ok()?;
    Some(AudioInfo {
        sample_rate: buffer.sample_rate(),
        frames: buffer.frames() as u64,
        channels,
    })
}

/// A fresh file name to pass the imported project through.
fn temp_project_path() -> PathBuf {
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "canopy-flp-import-{}-{n}.{}",
        std::process::id(),
        project::FILE_EXTENSION
    ))
}
