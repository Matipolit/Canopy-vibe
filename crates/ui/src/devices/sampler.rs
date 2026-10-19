//! The SFZ Sampler's panel: which instrument file it plays (load, reload),
//! what's in it, a keyboard map of its regions lit as they play (click a
//! key to hear it), and its knobs.

use eframe::egui::{self, Color32, RichText, Ui, vec2};
use instruments::builtin::sampler::{
    METER_KEYS, METER_KEYSWITCH, METER_REGIONS, METER_VOICES, RegionInfo, Trigger,
};

use super::{Owner, Panel};
use crate::banks::BankState;
use crate::widgets::keys::{KeyMap, Span};

/// Velocity layers get a row each above the keys, up to this many.
const MAX_ROWS: usize = 6;

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let Owner::Instrument(track) = panel.owner else {
        return;
    };
    let file = panel
        .session
        .project
        .track(track)
        .and_then(|t| t.instrument_file())
        .map(std::path::Path::to_owned);
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            if ui
                .button(if file.is_some() {
                    "Load…"
                } else {
                    "Load SFZ…"
                })
                .on_hover_text("Pick an .sfz instrument")
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("SFZ instrument", &["sfz", "SFZ"])
                    .pick_file()
            {
                panel.session.set_instrument_file(track, path);
            }
            if file.is_some()
                && ui
                    .button("Reload")
                    .on_hover_text("Read the file again, after editing it")
                    .clicked()
            {
                panel.session.reload_bank(track);
            }
            if let Some(path) = &file {
                ui.label(
                    RichText::new(path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    ))
                    .strong(),
                )
                .on_hover_text(path.display().to_string());
            }
        });
        let bank = match panel.session.bank(track) {
            Some(BankState::Ready(bank)) => Some(bank.clone()),
            Some(BankState::Loading) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading samples…");
                });
                None
            }
            Some(BankState::Failed(err)) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("Couldn't load: {err}"));
                None
            }
            None => {
                ui.weak("No instrument loaded: it plays nothing until you load one.");
                None
            }
        };
        if let Some(bank) = &bank {
            let count = |n: usize, one: &str, many: &str| {
                format!("{n} {}", if n == 1 { one } else { many })
            };
            ui.label(format!(
                "{} · {} · {:.1} MB · {} playing",
                count(bank.regions().len(), "region", "regions"),
                count(bank.sample_count(), "sample", "samples"),
                bank.sample_memory_bytes() as f64 / 1_048_576.0,
                count(panel.meters[METER_VOICES] as usize, "voice", "voices"),
            ));
            let warnings = bank.warnings();
            if !warnings.is_empty() {
                egui::CollapsingHeader::new(
                    RichText::new(format!("{} warnings", warnings.len()))
                        .color(ui.visuals().warn_fg_color),
                )
                .id_salt(panel.id("warnings"))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(80.0)
                        .show(ui, |ui| {
                            for warning in warnings {
                                ui.label(RichText::new(warning).small());
                            }
                        });
                });
            }
            key_map(ui, panel, bank.regions());
        }
        panel.knobs(
            ui,
            &["volume", "pan", "tune", "velocity", "attack", "release"],
        );
        ui.horizontal(|ui| {
            ui.label("Polyphony");
            panel.choice_menu(ui, "polyphony");
        });
    });
}

/// The regions over a keyboard: a row per velocity layer, keyswitches in
/// their own color, lit while they play.
fn key_map(ui: &mut Ui, panel: &mut Panel, regions: &[RegionInfo]) {
    let mut layers: Vec<(u8, u8)> = regions
        .iter()
        .filter(|r| r.trigger == Trigger::Attack)
        .map(|r| (r.lovel, r.hivel))
        .collect();
    layers.sort_unstable();
    layers.dedup();
    let sounding: Vec<usize> = panel.meters[METER_REGIONS..]
        .iter()
        .filter(|v| **v >= 0.0)
        .map(|v| *v as usize)
        .collect();
    let accent = crate::theme::accent(ui.visuals());
    let mut spans: Vec<Span> = regions
        .iter()
        .enumerate()
        .map(|(index, region)| {
            let row = layers
                .iter()
                .position(|l| *l == (region.lovel, region.hivel))
                .unwrap_or(0)
                .min(MAX_ROWS - 1);
            let color = if sounding.contains(&index) {
                Color32::WHITE
            } else if region.trigger != Trigger::Attack {
                Color32::from_rgb(150, 150, 150)
            } else {
                accent
            };
            Span {
                low: region.lokey,
                high: region.hikey,
                root: Some(region.root_key),
                row,
                color,
            }
        })
        .collect();
    let switches: Vec<u8> = regions.iter().filter_map(|r| r.keyswitch).collect();
    let active = panel.meters[METER_KEYSWITCH];
    for key in switches {
        spans.push(Span {
            low: key,
            high: key,
            root: None,
            row: MAX_ROWS,
            color: if active >= 0.0 && active as u8 == key {
                Color32::from_rgb(240, 200, 90)
            } else {
                Color32::from_rgb(170, 120, 60)
            },
        });
    }
    let low = regions.iter().map(|r| r.lokey).min().unwrap_or(36).min(36);
    let high = regions.iter().map(|r| r.hikey).max().unwrap_or(96).max(84);
    let lit: Vec<u8> = (0..128u8)
        .filter(|key| {
            let word = panel.meters[METER_KEYS + usize::from(*key) / 24] as u32;
            word >> (key % 24) & 1 == 1
        })
        .collect();
    let width = (f32::from(high - low) + 1.0) * 9.0;
    let event = KeyMap::new(panel.id("keys"), low, high, &spans)
        .lit(&lit)
        .size(vec2(width.clamp(300.0, 900.0), 80.0))
        .show(ui);
    if let Some(event) = event {
        panel.play(event);
    }
}
