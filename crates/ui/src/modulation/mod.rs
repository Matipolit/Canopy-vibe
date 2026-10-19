//! Modulation in the UI: the modulator column in the device view (macros,
//! LFOs, envelopes, followers), the handles modulation sources are dragged
//! from onto the knobs they move, and the table of a track's routes.
//!
//! These are the entry points the rest of the UI calls; what's behind them
//! grows here. For now there's nothing to show: the column is empty and
//! source handles take no room.

use eframe::egui::{self, Color32, Context, Ui};
use project::{ModSource, Track, TrackId};

use crate::session::Session;
use crate::timeline::TimelineView;

/// The modulator column of a track's device view, between its mixer and
/// its instrument.
pub fn column(
    _ui: &mut Ui,
    _session: &mut Session,
    _timeline: &mut TimelineView,
    _track: &Track,
    _color: Color32,
) {
}

/// A handle to drag modulation source `source` of track `owner` from, onto
/// what it should move (an instrument's LFO, beside the LFO). Returns its
/// response.
#[allow(dead_code)] // until instrument panels place them
pub fn source_handle(
    ui: &mut Ui,
    _session: &mut Session,
    owner: TrackId,
    source: ModSource,
) -> egui::Response {
    let id = ui.id().with(("modulation source", owner, source));
    ui.interact(egui::Rect::NOTHING, id, egui::Sense::hover())
}

/// Whether a modulation source is being dragged, for things that open up
/// to show drop targets (a page tab the pointer rests on).
#[allow(dead_code)] // until instrument panels ask
pub fn is_dragging(_ctx: &Context) -> bool {
    false
}
