//! Each effect's and instrument's own panel in the device view, built from
//! one toolkit ([`Panel`]): knobs, choices and switches that know about
//! automation, controller links, undo and resetting, plus the device's
//! meters and tapped sound for its displays. Devices without a panel of
//! their own get a plain grid of knobs.

mod amp;
mod analyzer;
mod chorus;
mod convolve;
mod creative;
mod distortion;
mod drums;
mod dynamics;
mod eq;
mod filter;
mod freqshift;
mod generic;
mod guitar;
mod hyper;
mod mastering;
mod panel;
pub(crate) mod preset_menu;
mod reverb;
mod sampler;
mod space;
mod transient;
mod utility;
mod wavetable;

pub(crate) use analyzer::windows as analyzer_windows;
pub(crate) use panel::{Owner, Panel};
pub(crate) use wavetable::take_drop as wavetable_drop;

/// The body of an effect's panel, under its header.
pub(crate) fn effect(ui: &mut eframe::egui::Ui, panel: &mut Panel) {
    if !panel.format.is_builtin() {
        return crate::plugins::panel(ui, panel);
    }
    match panel.kind {
        "canopy.amp" => amp::show(ui, panel),
        "canopy.analyzer" => analyzer::show(ui, panel),
        "canopy.chorus" => chorus::show(ui, panel),
        "canopy.clipper" => mastering::clipper(ui, panel),
        "canopy.compressor" => dynamics::compressor(ui, panel),
        "canopy.convolve" => convolve::show(ui, panel),
        "canopy.ducker" => dynamics::ducker(ui, panel),
        "canopy.eq" => eq::show(ui, panel),
        "canopy.hyper" => hyper::show(ui, panel),
        "canopy.filter" => filter::show(ui, panel),
        "canopy.frequency_shifter" => freqshift::show(ui, panel),
        "canopy.limiter" => dynamics::limiter(ui, panel),
        "canopy.multiband" => dynamics::multiband(ui, panel),
        "canopy.shaper" => creative::shaper(ui, panel),
        "canopy.vocoder" => creative::vocoder(ui, panel),
        "canopy.waveshaper" => creative::waveshaper(ui, panel),
        "canopy.reverb" => reverb::show(ui, panel),
        "canopy.delay" => space::delay(ui, panel),
        "canopy.distortion" => distortion::show(ui, panel),
        "canopy.transient" => transient::show(ui, panel),
        "canopy.utility" => utility::show(ui, panel),
        _ => generic::show(ui, panel),
    }
}

/// The body of an instrument's panel.
pub(crate) fn instrument(ui: &mut eframe::egui::Ui, panel: &mut Panel) {
    if !panel.format.is_builtin() {
        return crate::plugins::panel(ui, panel);
    }
    match panel.kind {
        "canopy.drums" => drums::show(ui, panel),
        "canopy.guitar" => guitar::show(ui, panel),
        "canopy.sampler" => sampler::show(ui, panel),
        "canopy.wavetable" => wavetable::show(ui, panel),
        _ => generic::show(ui, panel),
    }
}
