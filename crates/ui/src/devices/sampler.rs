//! The SFZ Sampler's panel: which instrument file it plays (load, reload),
//! what's in it, and a map of its regions (each a block over the keys it
//! covers and the velocities it answers, lit as it plays) over a keyboard
//! strip (click a key to hear it); then its knobs in cards: the amp, the
//! pitch, and the envelope's offsets, drawn.

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, pos2, vec2,
};
use instruments::builtin::sampler::{
    METER_KEYS, METER_KEYSWITCH, METER_REGIONS, METER_VOICES, RegionInfo, Trigger,
};

use super::panel::{DISPLAY_INSET, display};
use super::{Owner, Panel};
use crate::banks::BankState;
use crate::theme::{self, size, space, stroke, text};
use crate::widgets::keys::{KeyMap, Span, key_rect};
use crate::widgets::{chrome, knob};

/// The keyboard strip under the region map.
const KEYS_HEIGHT: f32 = 40.0;
/// How wide each key of the map is, and the map's narrowest and widest.
const KEY_WIDTH: f32 = 9.0;
const MIN_MAP: f32 = 360.0;
const MAX_MAP: f32 = 640.0;
/// The keys shown when nothing (or little) is loaded: C2 to C7.
const LOWEST: u8 = 36;
const HIGHEST: u8 = 96;
/// The least the region map shrinks to in a short panel.
const MIN_REGIONS: f32 = 24.0;

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let Owner::Instrument(track) = panel.owner else {
        return;
    };
    let height = ui.available_height();
    let compact = std::mem::replace(&mut panel.compact, true);
    egui::ScrollArea::vertical()
        .id_salt(panel.id("sampler scroll"))
        .auto_shrink([true, true])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::M;
                instrument(ui, panel, track, height);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = space::S;
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = space::M;
                        card(ui, "Amp", theme::section::NOTE, |ui| {
                            panel.knob(ui, "volume");
                            panel.knob(ui, "pan");
                            panel.knob(ui, "velocity");
                        });
                        card(ui, "Pitch", theme::section::OSC_A, |ui| {
                            panel.knob(ui, "tune");
                        });
                    });
                    let width = ui.min_rect().width();
                    envelope(ui, panel, width, height);
                });
            });
        });
    panel.compact = compact;
}

/// A titled card with a row of compact knobs.
fn card(ui: &mut Ui, title: &str, color: Color32, knobs: impl FnOnce(&mut Ui)) {
    chrome::section(ui, color, |ui| {
        chrome::section_header(ui, title, color, |_| {});
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            knobs(ui);
        });
    });
}

/// The instrument's card: load and reload its file, its name, and the
/// polyphony in the header; what it holds (or why it holds nothing); and
/// its regions over the keys.
fn instrument(ui: &mut Ui, panel: &mut Panel, track: project::TrackId, height: f32) {
    let file = panel
        .session
        .project
        .track(track)
        .and_then(|t| t.instrument_file())
        .map(std::path::Path::to_owned);
    let bank = match panel.session.bank(track) {
        Some(BankState::Ready(bank)) => Ok(Some(bank.clone())),
        Some(BankState::Loading) => Ok(None),
        Some(BankState::Failed(err)) => Err(Some(err.clone())),
        None => Err(None),
    };
    let regions = match &bank {
        Ok(Some(bank)) => bank.regions(),
        _ => &[],
    };
    let (low, high) = key_range(regions);
    let width = ((f32::from(high - low) + 1.0) * KEY_WIDTH).clamp(MIN_MAP, MAX_MAP);
    let color = theme::section::NOTE;
    chrome::section(ui, color, |ui| {
        ui.set_width(width);
        chrome::section_header(ui, "", color, |ui| {
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
            // Polyphony at the right first, so the file's name takes only
            // the room left between (shortened to fit) instead of running
            // under it.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                panel.choice_menu(ui, "polyphony");
                chrome::caption(ui, "Polyphony");
                if let Some(path) = &file {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(path.file_name().map_or_else(
                                    || path.display().to_string(),
                                    |n| n.to_string_lossy().into_owned(),
                                ))
                                .strong(),
                            )
                            .truncate(),
                        )
                        .on_hover_text(path.display().to_string());
                    });
                }
            });
        });
        // What it holds, or why it holds nothing; with nothing loaded the
        // map says so.
        ui.horizontal(|ui| match &bank {
            Ok(Some(bank)) => {
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
            }
            Ok(None) => {
                ui.spinner();
                ui.label("Loading samples…");
            }
            Err(Some(err)) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("Couldn't load: {err}"));
            }
            Err(None) => {}
        });
        if let Ok(Some(bank)) = &bank {
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
        }
        // The card is as tall as the panel leaves (less the gap under it)
        // and the region map takes what the lines above and the keys leave.
        let inside = height - space::S - 2.0 * space::S;
        let map =
            (inside - ui.min_rect().height() - KEYS_HEIGHT - 2.0 * space::XS).max(MIN_REGIONS);
        let empty = match &bank {
            Err(None) => Some((
                "No instrument loaded",
                "It plays nothing until you load an .sfz instrument",
            )),
            Ok(Some(_)) => Some(("No regions", "The file maps no samples to keys")),
            _ => None,
        };
        key_map(ui, panel, regions, (low, high), vec2(width, map), empty);
    });
}

/// The keys the map shows: C2 to C7, and further where the regions reach.
fn key_range(regions: &[RegionInfo]) -> (u8, u8) {
    let low = regions
        .iter()
        .map(|r| r.lokey)
        .min()
        .unwrap_or(LOWEST)
        .min(LOWEST);
    let high = regions
        .iter()
        .map(|r| r.hikey)
        .max()
        .unwrap_or(HIGHEST)
        .max(HIGHEST);
    (low, high)
}

/// The regions as blocks over the keys they cover, as high as the
/// velocities they answer (soft at the bottom, hard at the top), lit while
/// they play, each root key marked; under them a keyboard strip with the
/// keyswitches over it in their own color. `size` is the region map's;
/// `empty` is what it says (a title and a hint) when there are no regions.
fn key_map(
    ui: &mut Ui,
    panel: &mut Panel,
    regions: &[RegionInfo],
    (low, high): (u8, u8),
    size: egui::Vec2,
    empty: Option<(&str, &str)>,
) {
    let sounding: Vec<usize> = panel.meters[METER_REGIONS..]
        .iter()
        .filter(|v| **v >= 0.0)
        .map(|v| *v as usize)
        .collect();
    let (response, painter) = display(ui, size, Sense::hover());
    let area = response.rect.shrink2(vec2(0.0, space::XS));
    let visuals = ui.visuals().clone();
    let color = theme::section::NOTE;
    // Octave lines, so the blocks line up with the keys under them.
    for key in (low..=high).filter(|k| k % 12 == 0) {
        let x = key_rect(low, high, area, key).left();
        painter.vline(
            x,
            area.y_range(),
            Stroke::new(
                stroke::HAIRLINE,
                visuals.weak_text_color().gamma_multiply(0.2),
            ),
        );
    }
    let velocity_y = |velocity: u8| area.bottom() - f32::from(velocity) / 127.0 * area.height();
    for (index, region) in regions.iter().enumerate() {
        let (from, to) = (region.lokey.max(low), region.hikey.min(high));
        if from > to {
            continue;
        }
        let block = Rect::from_min_max(
            pos2(
                key_rect(low, high, area, from).left(),
                velocity_y(region.hivel),
            ),
            pos2(
                key_rect(low, high, area, to).right(),
                velocity_y(region.lovel.saturating_sub(1)),
            ),
        )
        .shrink(0.5);
        let playing = sounding.contains(&index);
        let ink = if region.trigger != Trigger::Attack {
            theme::section::NEUTRAL
        } else {
            color
        };
        let fill = if playing { 0.8 } else { 0.22 };
        painter.rect_filled(block, 1.0, ink.gamma_multiply(fill));
        painter.rect_stroke(
            block,
            1.0,
            Stroke::new(stroke::HAIRLINE, ink.gamma_multiply(0.7)),
            egui::StrokeKind::Inside,
        );
        if (from..=to).contains(&region.root_key) {
            let x = key_rect(low, high, area, region.root_key).center().x;
            painter.circle_filled(
                Pos2::new(x, block.bottom() - 3.0),
                2.0,
                visuals.strong_text_color(),
            );
        }
    }
    if regions.is_empty() {
        if let Some((title, hint)) = empty {
            chrome::empty_hint(&painter, response.rect, title, hint);
        }
    } else {
        painter.text(
            response.rect.left_top() + DISPLAY_INSET,
            Align2::LEFT_TOP,
            "Velocity",
            FontId::proportional(text::TINY),
            visuals.weak_text_color(),
        );
    }
    response.on_hover_text("The regions: their keys across, their velocities up");

    let active = panel.meters[METER_KEYSWITCH];
    let spans: Vec<Span> = regions
        .iter()
        .filter_map(|r| r.keyswitch)
        .map(|key| Span {
            low: key,
            high: key,
            root: None,
            row: 0,
            color: if active >= 0.0 && active as u8 == key {
                Color32::from_rgb(240, 200, 90)
            } else {
                Color32::from_rgb(170, 120, 60)
            },
        })
        .collect();
    let lit: Vec<u8> = (0..128u8)
        .filter(|key| {
            let word = panel.meters[METER_KEYS + usize::from(*key) / 24] as u32;
            word >> (key % 24) & 1 == 1
        })
        .collect();
    let event = KeyMap::new(panel.id("keys"), low, high, &spans)
        .lit(&lit)
        .size(vec2(size.x, KEYS_HEIGHT))
        .show(ui);
    if let Some(event) = event {
        panel.play(event);
    }
}

/// The envelope's card: the attack and release added to every region's
/// own, drawn as a note's level rising and falling (faintly, as the file
/// has them), and their knobs.
fn envelope(ui: &mut Ui, panel: &mut Panel, width: f32, height: f32) {
    let color = theme::section::ENVELOPE;
    chrome::section(ui, color, |ui| {
        ui.set_width(width - 2.0 * space::S);
        chrome::section_header(ui, "Envelope", color, |_| {});
        // What the cards above and this card's own header and knobs leave
        // of the panel's height (less the gap under it): two cards' margins
        // and headers, two rows of knobs, and the gaps between.
        let knobs = knob::compact_height(knob::COMPACT);
        let fixed = 6.0 * space::S + 2.0 * (size::CONTROL + knobs) + 3.0 * space::XS;
        let tall = (height - fixed).max(MIN_REGIONS);
        ar_display(ui, panel, vec2(width - 2.0 * space::S, tall));
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            panel.knob(ui, "attack");
            panel.knob(ui, "release");
        });
    });
}

/// A note's level through its attack, held, and its release, each as long
/// as its knob is turned from the file's own (the faint shape, at the
/// middle of each knob's range).
fn ar_display(ui: &mut Ui, panel: &Panel, size: egui::Vec2) {
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink(space::S);
    let at = |id: &str| {
        panel
            .index(id)
            .map_or(0.5, |i| panel.params[i].to_normalized(panel.value(id)))
    };
    // Each ramp takes up to two fifths of the width, at the end of its knob.
    let shape = |attack: f32, release: f32| -> Vec<Pos2> {
        let ramp = 0.4 * rect.width();
        let top = rect.top() + space::XS;
        vec![
            rect.left_bottom(),
            pos2(rect.left() + attack * ramp, top),
            pos2(rect.right() - release * ramp, top),
            rect.right_bottom(),
        ]
    };
    let color = theme::section::ENVELOPE;
    let (attack, release) = (at("attack"), at("release"));
    if (attack - 0.5).abs() > 1e-3 || (release - 0.5).abs() > 1e-3 {
        painter.add(Shape::line(
            shape(0.5, 0.5),
            Stroke::new(stroke::HAIRLINE, color.gamma_multiply(0.35)),
        ));
    }
    let line = shape(attack, release);
    painter.add(Shape::convex_polygon(
        line.clone(),
        color.gamma_multiply(0.15),
        Stroke::NONE,
    ));
    painter.add(Shape::line(line, Stroke::new(stroke::EMPHASIS, color)));
    response.on_hover_text(
        "Attack and release, added to each region's own: the faint shape is the file's",
    );
}
