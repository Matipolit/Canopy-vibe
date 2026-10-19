//! The "FX" page of an instrument's panel (the Wavetable's, as in Serum):
//! the track's own effects along its main signal path, compact, so a
//! synth's LFOs can be dragged onto them from the next page.
//!
//! These are the track's real devices, the same the rack and the Patcher
//! show. For now the page lists them by name.

use eframe::egui::{Color32, RichText, Ui};
use project::TrackId;

use crate::session::Session;
use crate::timeline::TimelineView;

/// The FX page of `track`'s instrument panel.
#[allow(dead_code)] // until the Wavetable's panel has the page
pub fn show(
    ui: &mut Ui,
    session: &mut Session,
    _timeline: &mut TimelineView,
    track: TrackId,
    color: Color32,
) {
    let Some(track) = session.project.track(track) else {
        return;
    };
    if track.devices.is_empty() {
        ui.weak("No effects on this track.");
    }
    for device in &track.devices {
        let name = RichText::new(device.name());
        ui.label(if device.bypassed {
            name.weak()
        } else {
            name.color(color)
        });
    }
}
