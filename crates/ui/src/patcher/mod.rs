//! The Patcher: a chain's effect graph (`project::Routing`) as nodes and
//! cables on a canvas, hand-painted, in the detail panel or a window of its
//! own.
//!
//! These are the entry points the rest of the UI calls; what's behind them
//! grows here. Until the canvas exists the view isn't offered
//! ([`READY`]), and the rack notes when a chain it shows has a graph.

use eframe::egui::{Context, Ui};
use project::TrackId;

use crate::session::Session;
use crate::timeline::TimelineView;

/// Whether the detail panel offers the Patcher yet.
pub const READY: bool = false;

/// The Patcher view of the selected chain (a track's, or the master's), in
/// the detail panel.
pub fn show(ui: &mut Ui, _session: &mut Session, _timeline: &mut TimelineView) {
    ui.weak("The Patcher isn't available yet.");
}

/// Every frame: Patchers popped out into windows of their own.
pub fn windows(_ctx: &Context, _session: &mut Session, _timeline: &mut TimelineView) {}

/// Before `owner`'s effects in the rack: if its sound isn't a straight line
/// through them, says so (the rack lists them in the order sound reaches
/// them).
pub fn rack_banner(
    ui: &mut Ui,
    session: &mut Session,
    _timeline: &mut TimelineView,
    owner: Option<TrackId>,
) {
    if session.project.routing(owner).is_some() {
        ui.weak("Patched")
            .on_hover_text("These effects are patched as a graph; they're shown in the order the sound reaches them");
    }
}
