//! Every built-in instrument's panel fits the device view of a 1280×800
//! window: no scrolling sideways to reach a control.

use eframe::egui;
use egui_kittest::kittest::Queryable;
use project::TrackId;

use super::Rig;

/// The master meter's column at the window's right, which the device view
/// stops at.
const METER: f32 = 84.0;

/// The first MIDI track, selected, playing instrument `id`.
fn with_instrument(rig: &mut Rig, id: &str) -> TrackId {
    let midi = rig.app().session.project.track_ids()[0];
    rig.click(rig.header_point(midi));
    rig.frames(2);
    let reference = instruments::InstrumentRef::builtin(id);
    rig.app_mut()
        .session
        .edit("Change Instrument", |p| p.set_instrument(midi, reference))
        .unwrap();
    rig.frames(3);
    midi
}

/// The instrument's card, as the device view last drew it.
fn card(rig: &Rig) -> egui::Rect {
    rig.harness
        .ctx
        .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("instrument card")))
        .expect("an instrument card")
}

fn assert_fits(rig: &Rig, what: &str) {
    let right = rig.harness.ctx.content_rect().right() - METER;
    let card = card(rig);
    assert!(
        card.right() <= right,
        "{what}: the card reaches {} of {right} ({} wide)",
        card.right(),
        card.width()
    );
}

#[test]
fn every_wavetable_page_fits_a_1280_window() {
    let mut rig = Rig::new("wavetable_widths");
    with_instrument(&mut rig, "canopy.wavetable");
    for page in [
        "Oscillators",
        "Envelopes & LFOs",
        "Matrix",
        "FX",
        "Voicing",
        "Browser",
    ] {
        rig.harness.get_by_label(page).click();
        rig.frames(3);
        rig.screenshot(page);
        assert_fits(&rig, page);
    }
}

#[test]
fn the_drum_synth_fits_a_1280_window() {
    let mut rig = Rig::new("drums_width");
    with_instrument(&mut rig, "canopy.drums");
    rig.screenshot("drums");
    assert_fits(&rig, "drums");
}

#[test]
fn the_guitar_fits_a_1280_window() {
    let mut rig = Rig::new("guitar_width");
    with_instrument(&mut rig, "canopy.guitar");
    rig.screenshot("guitar");
    assert_fits(&rig, "guitar");
}

#[test]
fn the_poly_synth_fits_a_1280_window() {
    let mut rig = Rig::new("poly_synth_width");
    with_instrument(&mut rig, "canopy.poly_synth");
    rig.screenshot("poly synth");
    assert_fits(&rig, "poly synth");
}

#[test]
fn the_sampler_fits_a_1280_window() {
    let mut rig = Rig::new("sampler_width");
    with_instrument(&mut rig, "canopy.sampler");
    rig.screenshot("sampler");
    assert_fits(&rig, "sampler");
}

/// How far above the window's bottom the device view's edge is dragged: its
/// smallest height and the status bar under it.
const SHORT: f32 = 170.0;

/// Drags the device view's top edge down until it's about as short as it
/// goes (past that, it folds away).
fn shorten_device_view(rig: &mut Rig) {
    let tab = rig.harness.get_by_label("Device").rect();
    let edge = egui::pos2(tab.right() + 200.0, tab.top() - 2.0);
    let bottom = rig.harness.ctx.content_rect().bottom();
    rig.drag(edge, egui::pos2(edge.x, bottom - SHORT));
    rig.frames(3);
}

/// Scrolls the device view with the wheel until `label` shows above the
/// status bar, and says whether it did.
fn scroll_to(rig: &mut Rig, label: &str) -> bool {
    let status = rig.harness.ctx.content_rect().bottom() - STATUS;
    let card = card(rig);
    rig.event(egui::Event::PointerMoved(egui::pos2(
        card.center().x,
        card.bottom() - 8.0,
    )));
    for _ in 0..20 {
        // The lowest of them: the track's own Volume is higher up, say.
        let lowest = rig
            .harness
            .get_all_by_label(label)
            .map(|n| n.rect().bottom())
            .fold(f32::MIN, f32::max);
        if lowest <= status {
            return true;
        }
        rig.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -40.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        });
        rig.frames(1);
    }
    false
}

/// The status bar's height, at the window's bottom.
const STATUS: f32 = 24.0;

#[test]
fn a_short_device_view_scrolls_to_every_knob() {
    for (id, last) in [
        ("canopy.poly_synth", "Gain"),
        ("canopy.wavetable", "Random"),
        ("canopy.drums", "Velocity"),
        ("canopy.guitar", "Level"),
        ("canopy.sampler", "Release"),
    ] {
        let mut rig = Rig::new("short_device_view");
        with_instrument(&mut rig, id);
        shorten_device_view(&mut rig);
        rig.screenshot(id);
        assert!(scroll_to(&mut rig, last), "{id}: {last} comes into view");
    }
}
