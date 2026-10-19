//! The Plugin Manager window: what scanning found and couldn't read,
//! where it looks, rescanning, and how editor windows behave.

use ::plugins::PluginFormat;
use eframe::egui::{self, Context, RichText};

use crate::session::Session;
use crate::theme::space;

pub(super) fn show(ctx: &Context, session: &mut Session) {
    if !session.plugins.window {
        return;
    }
    let mut open = true;
    egui::Window::new("Plugin Manager")
        .open(&mut open)
        .default_size([560.0, 420.0])
        .show(ctx, |ui| contents(ui, session));
    session.plugins.window = open;
}

fn contents(ui: &mut egui::Ui, session: &mut Session) {
    let wake = session.waker();
    let catalog = session.plugins.manager.catalog();
    let (found, failed, scanning) = (
        catalog.plugins().len(),
        catalog.failures().len(),
        catalog.is_scanning(),
    );
    ui.horizontal(|ui| {
        if scanning {
            ui.spinner();
            ui.label("Looking for plugins…");
        } else {
            ui.label(format!(
                "{found} plugin{} found{}",
                if found == 1 { "" } else { "s" },
                if failed > 0 {
                    format!(
                        ", {failed} file{} couldn't be read",
                        if failed == 1 { "" } else { "s" }
                    )
                } else {
                    String::new()
                }
            ));
        }
    });
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!scanning, egui::Button::new("Rescan"))
            .on_hover_text("Look for new and changed plugins")
            .clicked()
        {
            session.plugins.rescan(wake.clone(), false);
        }
        if ui
            .add_enabled(!scanning && failed > 0, egui::Button::new("Retry Failed"))
            .on_hover_text("Read the files that failed last time again (a plugin that's been licensed since, say)")
            .clicked()
        {
            session.plugins.rescan(wake.clone(), true);
        }
        let mut on_top = session.plugins.manager.keep_on_top();
        if ui
            .checkbox(&mut on_top, "Keep editors on top")
            .on_hover_text("Plugin editors open in windows of their own; keep them above canopy's")
            .changed()
        {
            session.plugins.manager.set_keep_on_top(on_top);
        }
    });
    ui.add_space(space::S);
    let filter_id = egui::Id::new("plugin manager filter");
    let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
    if ui
        .add(egui::TextEdit::singleline(&mut filter).hint_text("Filter by name, vendor or format"))
        .changed()
    {
        ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
    }
    let needle = filter.trim().to_lowercase();
    let catalog = session.plugins.manager.catalog();
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::Grid::new("plugin list")
                .num_columns(4)
                .striped(true)
                .spacing([space::L, space::XS])
                .show(ui, |ui| {
                    for info in catalog.plugins() {
                        let text = format!("{} {} {}", info.name, info.vendor, info.format.name())
                            .to_lowercase();
                        if !needle.is_empty() && !text.contains(&needle) {
                            continue;
                        }
                        ui.label(&info.name)
                            .on_hover_text(info.path.display().to_string());
                        ui.label(&info.vendor);
                        ui.label(if info.bridged {
                            format!("{} (yabridge)", info.format.name())
                        } else {
                            info.format.name().to_owned()
                        });
                        let usable = hosting::usable(info);
                        ui.label(if usable {
                            RichText::new(info.kind.name())
                        } else {
                            RichText::new(format!("{} (not usable here)", info.kind.name())).weak()
                        });
                        ui.end_row();
                    }
                });
        });
    if failed > 0 {
        egui::CollapsingHeader::new(format!("Couldn't read ({failed})")).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(120.0)
                .id_salt("plugin failures")
                .show(ui, |ui| {
                    for failure in catalog.failures() {
                        ui.label(RichText::new(failure.path.display().to_string()).small());
                        ui.label(RichText::new(&failure.error).small().weak());
                    }
                });
        });
    }
    egui::CollapsingHeader::new("Where canopy looks").show(ui, |ui| {
        for format in PluginFormat::ALL {
            for path in ::plugins::scan::default_paths(format) {
                ui.label(RichText::new(format!("{}: {}", format.name(), path.display())).small());
            }
        }
        ui.label(
            RichText::new(
                "Set CLAP_PATH or VST_PATH to add folders. On Linux, Windows plugins come in through yabridge.",
            )
            .small()
            .weak(),
        );
    });
}
