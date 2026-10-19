//! The Wavetable synth's panel, in pages as Serum's: the oscillators with
//! the sub, noise and filters; the envelopes and LFOs; the modulation
//! matrix; voicing and the rest; and a browser of its presets. A strip of
//! its modulation sources sits by the page tabs on every page, the macros
//! and the output at the right.
//!
//! Modulation is drag and drop, as in Serum and Vital: drag a source's tile
//! (or a macro's name) onto any knob modulation can move, and a matrix
//! slot routes it there. A knob shows each slot aimed at it as a ring in
//! its source's color (drag the ring to set the depth; right-click the knob
//! for the slot's options) and a dot where modulation has its value now.
//! Beside each source the synth exports ([`wt::MOD_OUTPUTS`]) sits the
//! handle `crate::modulation` gives it, to drag onto the track's other
//! devices; the FX page shows those devices (`crate::fx_view`).
//!
//! The displays move with the sound: each oscillator draws its table in 3D
//! with the frame playing lit (morphed and warped as it plays), or its
//! spectrum; the filter curves follow their modulated cutoffs; LFOs and
//! envelopes show where the newest voice is; the output has a scope. All
//! of it comes from `wt::live_modulation` and the synth's meters, read once
//! a frame; the panel repaints only while they move (`Panel::animate`).
//!
//! Each oscillator can play its own table from a WAV file: Load… beside
//! its table menu, or a file dropped on its display (see [`take_drop`]),
//! loads it and picks "User"; Edit… opens the table in the editor, which
//! draws, morphs and saves tables as WAV files.

mod browser;
mod editor;
mod filters;
mod global;
mod knobs;
mod matrix;
mod modulation;
mod oscillators;

use eframe::egui::{self, RichText, Ui};
use instruments::builtin::wavetable as wt;

use super::Panel;
use crate::theme::space;
use knobs::Live;

pub(crate) use oscillators::take_drop;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Oscillators,
    Modulation,
    Matrix,
    Effects,
    Global,
    Browser,
}

/// The pages' tabs, in order.
const PAGES: [(Page, &str); 6] = [
    (Page::Oscillators, "Oscillators"),
    (Page::Modulation, "Envelopes & LFOs"),
    (Page::Matrix, "Matrix"),
    (Page::Effects, "FX"),
    (Page::Global, "Voicing"),
    (Page::Browser, "Browser"),
];

fn page_id(panel: &Panel) -> egui::Id {
    panel.id("page")
}

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    panel.knob_size = 28.0;
    // The oscillator page is wider than most views and scrolls sideways;
    // past the view's edge, wrapping text would stack it a letter a line.
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    let live = Live::new(panel);
    let id = page_id(panel);
    let mut page: Page = ui.data(|d| d.get_temp(id)).unwrap_or(Page::Oscillators);
    ui.horizontal(|ui| {
        for (choice, name) in PAGES {
            if ui.selectable_label(page == choice, name).clicked() {
                page = choice;
            }
        }
        ui.separator();
        let voices = panel.meters[wt::METER_VOICES] as usize;
        ui.weak(format!(
            "{voices} voice{}",
            if voices == 1 { "" } else { "s" }
        ));
        ui.separator();
        source_strip(ui, panel, &live, &mut page);
    });
    ui.data_mut(|d| d.insert_temp(id, page));
    ui.horizontal_top(|ui| {
        match page {
            Page::Oscillators => oscillators::page(ui, panel, &live),
            Page::Modulation => modulation::page(ui, panel, &live),
            Page::Matrix => matrix::page(ui, panel, &live),
            Page::Effects => {
                // The track's own effects, as the rack has them.
                if let Some(track) = panel.track {
                    let color = panel.color;
                    ui.vertical(|ui| {
                        crate::fx_view::show(ui, panel.session, panel.timeline, track, color)
                    });
                }
            }
            Page::Global => global::page(ui, panel, &live),
            Page::Browser => browser::page(ui, panel),
        }
        ui.separator();
        global::side(ui, panel, &live);
    });
    editor::window(ui.ctx(), panel);
}

/// The envelopes', LFOs' and note and controller sources' tiles, to drag
/// onto knobs, close together within a group and apart between groups.
/// Clicking an envelope's or LFO's opens it.
fn source_strip(ui: &mut Ui, panel: &mut Panel, live: &Live, page: &mut Page) {
    ui.spacing_mut().item_spacing.x = space::XS;
    for (group, sources) in wt::SOURCE_GROUPS {
        if group == "Macros" {
            // Dragged from their knobs' names at the right.
            continue;
        }
        for &source in sources {
            let tile = knobs::source_tile(ui, panel, live, source);
            knobs::export_handle(ui, panel, source);
            if tile.clicked() {
                if let Some(env) = wt::source_env(source) {
                    modulation::select_env(ui, panel, env);
                    *page = Page::Modulation;
                } else if let Some(lfo) = wt::source_lfo(source) {
                    modulation::select_lfo(ui, panel, lfo);
                    *page = Page::Modulation;
                }
            }
        }
        ui.add_space(space::M - space::XS);
    }
    if knobs::dragging_source(ui, panel) {
        ui.label(
            RichText::new("Drop on a knob to modulate it")
                .weak()
                .italics(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_destination_menus_offer_every_destination_once() {
        let mut listed: Vec<usize> = matrix::DESTINATION_GROUPS
            .iter()
            .flat_map(|(_, group)| group.iter().copied())
            .collect();
        listed.sort_unstable();
        let all: Vec<usize> = (0..wt::DESTINATIONS.len()).collect();
        assert_eq!(listed, all);
    }

    #[test]
    fn every_source_has_its_own_color() {
        let colors: Vec<_> = (1..wt::SOURCE_COUNT).map(knobs::source_color).collect();
        for (i, a) in colors.iter().enumerate() {
            assert_ne!(*a, egui::Color32::GRAY, "source {}", i + 1);
            for b in &colors[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
