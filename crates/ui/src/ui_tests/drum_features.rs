//! The Drum Synth's panel, driven through the UI and checked in the project
//! and the sound: pads play and select, a layer's tab turns it on, its
//! source and mode change from their controls, and pads and layers copy,
//! paste and reset from their menus. Screenshotted with `CANOPY_SCREENSHOTS`.

use eframe::egui;
use egui_kittest::kittest::Queryable;
use instruments::builtin::drums::{self, Layer, Mode, Source, Voice, voices};
use project::TrackId;

use super::{Rig, peak};

/// The first MIDI track on the Drum Synth, selected.
fn drums(rig: &mut Rig) -> TrackId {
    let midi = rig.app().session.project.track_ids()[0];
    rig.click(rig.header_point(midi));
    rig.frames(2);
    let reference = instruments::InstrumentRef::builtin(drums::DESCRIPTOR.id);
    rig.app_mut()
        .session
        .edit("Change Instrument", |p| p.set_instrument(midi, reference));
    rig.frames(3);
    midi
}

/// Drum parameter `index` as the project has it.
fn param(rig: &Rig, track: TrackId, index: usize) -> f32 {
    let descriptor = &drums::DESCRIPTOR.params[index];
    rig.app()
        .session
        .project
        .track(track)
        .unwrap()
        .instrument_param(descriptor.id)
        .unwrap_or(descriptor.default_value())
}

fn voice(name: &str) -> &'static Voice {
    voices().iter().find(|v| v.name == name).unwrap()
}

/// The pad named `name` (the first widget of that name: pads come before
/// the selected pad's heading).
fn pad(rig: &Rig, name: &str) -> egui::Pos2 {
    rig.harness
        .get_all_by_label(name)
        .next()
        .unwrap_or_else(|| panic!("no pad {name}"))
        .rect()
        .center()
}

fn click_label(rig: &mut Rig, label: &str) {
    rig.harness.get_by_label(label).click();
    rig.frames(2);
}

#[test]
fn build_a_sound_from_layers() {
    let mut rig = Rig::new("drum_layers");
    let track = drums(&mut rig);
    rig.screenshot("kick");

    // Pressing the snare's pad plays it, and opens it.
    let at = pad(&rig, "Snare");
    rig.click(at);
    let mut loudest = 0.0f32;
    for _ in 0..10 {
        loudest = loudest.max(peak(&rig.render_audio(800)));
    }
    assert!(loudest > 0.01, "the snare played: {loudest}");
    rig.frames(2);
    rig.screenshot("snare");
    let labels: Vec<_> = rig
        .harness
        .get_all_by_label("Snare")
        .map(|n| n.rect())
        .collect();
    assert!(labels.len() >= 2, "the snare is open: {labels:?}");

    // Layer 5 is a spare: switch it on from its tab, and make it pink noise.
    let snare = voice("Snare");
    let mode = snare.layer_param(4, Layer::MODE);
    let source = snare.layer_param(4, Layer::SOURCE);
    click_label(&mut rig, "Layer 5");
    click_label(&mut rig, "Layer 5 on");
    assert_eq!(param(&rig, track, mode), Mode::On as usize as f32);
    assert_eq!(rig.app().session.undo_label(), Some("Layer On"));
    // Its source menu reads Sine.
    click_label(&mut rig, "Sine");
    click_label(&mut rig, "Pink Noise");
    assert_eq!(param(&rig, track, source), Source::Pink as usize as f32);
    rig.screenshot("layer 5 pink");

    // Heard only through the FM it puts on layer 6.
    click_label(&mut rig, "FM Only");
    assert_eq!(param(&rig, track, mode), Mode::FmOnly as usize as f32);

    // Copy the snare onto the clap's pad.
    rig.right_click(pad(&rig, "Snare"));
    click_label(&mut rig, "Copy pad");
    rig.right_click(pad(&rig, "Clap"));
    click_label(&mut rig, "Paste pad");
    assert_eq!(rig.app().session.undo_label(), Some("Paste Pad"));
    let clap = voice("Clap");
    for offset in 0..snare.params.len() {
        assert_eq!(
            param(&rig, track, clap.params.start + offset),
            param(&rig, track, snare.params.start + offset),
            "{}",
            drums::DESCRIPTOR.params[clap.params.start + offset].id
        );
    }

    // Back on the snare, reset layer 5 from its tab's menu.
    rig.click(pad(&rig, "Snare"));
    rig.frames(2);
    rig.right_click(rig.harness.get_by_label("Layer 5").rect().center());
    click_label(&mut rig, "Reset layer");
    assert_eq!(param(&rig, track, mode), Mode::Off as usize as f32);
    assert_eq!(param(&rig, track, source), Source::Sine as usize as f32);

    // Moving layer 1 right swaps it with layer 2.
    let head = |rig: &Rig, layer| param(rig, track, snare.layer_param(layer, Layer::PITCH));
    let (first, second) = (head(&rig, 0), head(&rig, 1));
    click_label(&mut rig, "Layer 1");
    rig.right_click(rig.harness.get_by_label("Layer 1").rect().center());
    click_label(&mut rig, "Move right");
    assert_eq!((head(&rig, 0), head(&rig, 1)), (second, first));
    rig.screenshot("layers moved");
}

#[test]
fn the_panel_fits_a_wide_window() {
    let mut rig = Rig::wide("drum_panel");
    drums(&mut rig);
    rig.click(pad(&rig, "Crash"));
    for _ in 0..5 {
        rig.render_audio(800);
    }
    rig.frames(3);
    rig.screenshot("crash");
}
