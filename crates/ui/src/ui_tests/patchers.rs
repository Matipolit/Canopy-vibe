//! Patchers in the device view: adding one, opening it to put effects
//! inside, wrapping effects into one, mapping knobs inside to its macros,
//! and what a macro then does.

use eframe::egui::{Event, Modifiers};
use egui_kittest::kittest::Queryable;
use project::{Chain, DeviceId, EffectRef, PATCHER};

use super::Rig;

fn names(rig: &Rig, chain: Chain) -> Vec<String> {
    rig.app()
        .session
        .project
        .devices(chain)
        .unwrap()
        .iter()
        .map(|d| d.name().to_owned())
        .collect()
}

fn add(rig: &mut Rig, effect: &str) {
    rig.harness.get_by_label("+ Effect").click();
    rig.frames(2);
    rig.harness.get_by_label(effect).click();
    rig.frames(2);
}

fn param(rig: &Rig, device: DeviceId, id: &str) -> f32 {
    rig.app()
        .session
        .project
        .device(device)
        .unwrap()
        .param(id)
        .unwrap()
}

#[test]
fn a_patcher_opens_to_take_effects_inside() {
    let mut rig = Rig::wide("patcher_open");
    let track = rig.app().session.project.track_ids()[2];
    rig.click(rig.header_point(track));
    rig.frames(2);
    add(&mut rig, "Patcher");
    assert_eq!(names(&rig, Chain::Track(track)), ["Patcher"]);
    let patcher = rig.app().session.project.devices(Some(track)).unwrap()[0].id;
    rig.screenshot("outside");

    rig.harness.get_by_label("Open").click();
    rig.frames(2);
    assert_eq!(rig.app().timeline.open_patcher, Some(patcher));
    // "+ Effect" now adds inside it.
    add(&mut rig, "Reverb");
    add(&mut rig, "Delay");
    assert_eq!(names(&rig, Chain::Patch(patcher)), ["Reverb", "Delay"]);
    assert_eq!(names(&rig, Chain::Track(track)), ["Patcher"]);
    rig.screenshot("inside");

    // Graph shows the same effects as nodes between Input and Output.
    rig.harness.get_by_label("Graph").click();
    rig.frames(3);
    assert!(rig.app().timeline.patcher_graph);
    assert!(rig.harness.query_by_label_contains("Input").is_some());
    assert!(rig.harness.query_by_label_contains("Output").is_some());
    rig.screenshot("graph");
    rig.harness.get_by_label("Effects").click();
    rig.frames(2);

    rig.harness.get_by_label("‹ Back").click();
    rig.frames(2);
    assert_eq!(rig.app().timeline.open_patcher, None);
    assert!(rig.harness.query_by_label("Open").is_some());
}

#[test]
fn selected_effects_go_into_a_patcher_and_out_again() {
    let mut rig = Rig::wide("patcher_wrap");
    let track = rig.app().session.project.track_ids()[2];
    rig.click(rig.header_point(track));
    rig.frames(2);
    add(&mut rig, "EQ");
    add(&mut rig, "Compressor");
    add(&mut rig, "Reverb");
    // Select the compressor and the reverb, then Into Patcher.
    rig.click(rig.harness.get_by_label("Compressor").rect().center());
    let reverb = rig.harness.get_by_label("Reverb").rect().center();
    rig.click_with(reverb, Modifiers::SHIFT);
    rig.right_click(reverb);
    rig.harness.get_by_label("Into Patcher").click();
    rig.frames(2);
    assert_eq!(names(&rig, Chain::Track(track)), ["EQ", "Patcher"]);
    let patcher = rig.app().session.project.devices(Some(track)).unwrap()[1].id;
    assert_eq!(names(&rig, Chain::Patch(patcher)), ["Compressor", "Reverb"]);
    assert_eq!(rig.app().session.undo_label(), Some("Into Patcher"));

    rig.right_click(rig.harness.get_by_label("Patcher").rect().center());
    rig.harness.get_by_label("Unpack").click();
    rig.frames(2);
    assert_eq!(names(&rig, Chain::Track(track)), ["EQ", "Compressor", "Reverb"]);
}

#[test]
fn a_knob_inside_maps_to_a_macro_that_then_drives_it() {
    let mut rig = Rig::wide("patcher_macros");
    let track = rig.app().session.project.track_ids()[2];
    let (patcher, reverb) = {
        let session = &mut rig.app_mut().session;
        let patcher = session
            .edit("Add", |p| {
                p.add_device(Some(track), EffectRef::builtin(PATCHER), None)
            })
            .unwrap();
        let reverb = session
            .edit("Add", |p| {
                p.add_device(Chain::Patch(patcher), EffectRef::builtin("canopy.reverb"), None)
            })
            .unwrap();
        session.edit("Set", |p| p.set_device_param(reverb, "dry", 40.0));
        (patcher, reverb)
    };
    rig.click(rig.header_point(track));
    rig.frames(2);
    rig.harness.get_by_label("Open").click();
    rig.frames(2);

    // Right-click the reverb's Dry, Map to Macro, Macro 1.
    rig.right_click(rig.harness.get_by_label("Dry").rect().center());
    rig.harness.get_by_label_contains("Map to Macro").click();
    rig.frames(2);
    rig.harness.get_by_label("Macro 1").click();
    rig.frames(2);
    let patch = rig.app().session.project.patch(patcher).unwrap().clone();
    assert_eq!(patch.links.len(), 1);
    assert_eq!(patch.links[0].device, reverb);
    // The macro took the knob's place: nothing jumped.
    assert!((param(&rig, patcher, "macro_1") - 40.0).abs() < 1e-3);
    assert!((param(&rig, reverb, "dry") - 40.0).abs() < 1e-3);
    assert_eq!(rig.app().session.undo_label(), Some("Map to Macro"));

    // Outside, Macro 1 now drives the reverb's dry level.
    rig.harness.get_by_label("‹ Back").click();
    rig.frames(2);
    rig.screenshot("macros");
    let session = &mut rig.app_mut().session;
    session.live_device_param(patcher, 0, "macro_1", 75.0);
    session.end_live("Change Effect");
    assert!((param(&rig, reverb, "dry") - 75.0).abs() < 1e-3);

    // Its menu names it and narrows the range.
    rig.right_click(rig.harness.get_by_label("Macro 1").rect().center());
    rig.frames(1);
    rig.event(Event::Text("Space".into()));
    rig.frames(1);
    assert!(rig.harness.query_by_label_contains("Reverb: Dry").is_some());
    rig.key(Modifiers::NONE, eframe::egui::Key::Enter);
    rig.frames(2);
    let patch = rig.app().session.project.patch(patcher).unwrap();
    assert_eq!(patch.macro_name(0), "Space");
    let session = &mut rig.app_mut().session;
    session.live_macro_link(patcher, 0, reverb, "dry", (0.5, 1.0));
    session.end_live("Macro Range");
    // 75% of the way from 50% to 100%.
    assert!((param(&rig, reverb, "dry") - 87.5).abs() < 1e-3);
}
