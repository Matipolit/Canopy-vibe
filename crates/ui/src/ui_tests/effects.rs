//! The effect rack: adding, bypassing, moving and removing effects, the EQ
//! curve, automating an effect parameter, and the master's effects.

use eframe::egui::{self, Event, Key, Modifiers, MouseWheelUnit, Pos2, vec2};
use egui_kittest::kittest::Queryable;
use project::AutomationTarget;

use super::Rig;
use crate::devices::Owner;
use crate::widgets::eq_curve;

fn names(rig: &Rig, track: Option<project::TrackId>) -> Vec<String> {
    rig.app()
        .session
        .project
        .devices(track)
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

#[test]
fn build_a_chain() {
    let mut rig = Rig::wide("effects");
    // An audio track: no instrument, so the whole chain fits on screen.
    let track = rig.app().session.project.track_ids()[2];
    rig.click(rig.header_point(track));
    rig.frames(2);

    // Add three effects from the device view.
    add(&mut rig, "EQ");
    add(&mut rig, "Compressor");
    add(&mut rig, "Reverb");
    assert_eq!(names(&rig, Some(track)), ["EQ", "Compressor", "Reverb"]);
    assert_eq!(rig.app().session.undo_label(), Some("Add Effect"));
    rig.screenshot("chain");

    // Right-click a name to move it; the switch bypasses it.
    rig.right_click(rig.harness.get_by_label("Reverb").rect().center());
    rig.harness.get_by_label("Move Left Ctrl+Left").click();
    rig.frames(2);
    assert_eq!(names(&rig, Some(track)), ["EQ", "Reverb", "Compressor"]);
    let reverb = rig.app().session.project.devices(Some(track)).unwrap()[1].id;
    // The switch just left of the Reverb's name (every effect has one).
    let name = rig.harness.get_by_label("Reverb").rect().center();
    let switch: Pos2 = rig
        .harness
        .get_all_by_label("On")
        .map(|n| n.rect().center())
        .filter(|p| (p.y - name.y).abs() < 4.0 && p.x < name.x)
        .max_by(|a, b| a.x.total_cmp(&b.x))
        .expect("the reverb's switch");
    rig.click(switch);
    assert!(rig.app().session.project.device(reverb).unwrap().bypassed);

    // Effect parameters automate like any other.
    let target = AutomationTarget::DeviceParam {
        device: reverb,
        param: "mix".into(),
    };
    // The reverb's Mix knob: it's left of the compressor's now.
    let mix = rig
        .harness
        .get_all_by_label("Mix")
        .map(|n| n.rect().center())
        .min_by(|a, b| a.x.total_cmp(&b.x))
        .unwrap();
    rig.right_click(mix);
    rig.harness.get_by_label("Automate").click();
    rig.frames(3);
    assert_eq!(
        rig.app().timeline.automation.get(&track).map(Vec::as_slice),
        Some(&[target][..])
    );

    // Removing one goes in one step, and undo brings it back.
    rig.right_click(rig.harness.get_by_label("Compressor").rect().center());
    rig.harness.get_by_label("Remove Delete").click();
    rig.frames(2);
    assert_eq!(names(&rig, Some(track)), ["EQ", "Reverb"]);
    rig.app_mut().session.undo();
    rig.frames(2);
    assert_eq!(names(&rig, Some(track)), ["EQ", "Reverb", "Compressor"]);
}

#[test]
fn save_and_load_presets() {
    let mut rig = Rig::new("presets");
    let dir = super::temp_dir("presets");
    rig.app_mut().session.presets_dir = Some(dir.clone());
    let track = rig.app().session.project.track_ids()[2];
    rig.click(rig.header_point(track));
    rig.frames(2);
    add(&mut rig, "Reverb");
    let reverb = rig.app().session.project.devices(Some(track)).unwrap()[0].id;
    rig.app_mut()
        .session
        .edit("Set", |p| p.set_device_param(reverb, "decay", 7.5));
    rig.frames(2);
    let decay = |rig: &Rig| {
        rig.app()
            .session
            .project
            .device(reverb)
            .unwrap()
            .param("decay")
            .unwrap()
    };

    // Right-click the name → Save Preset…, name it, Enter.
    rig.right_click(rig.harness.get_by_label("Reverb").rect().center());
    rig.harness.get_by_label("Save Preset…").click();
    rig.frames(2);
    // The name field has the focus as the window opens.
    rig.event(Event::Text("Long tail".into()));
    rig.frames(1);
    rig.screenshot("naming");
    rig.key(Modifiers::NONE, egui::Key::Enter);
    rig.frames(2);
    assert!(dir.join("canopy.reverb").join("Long tail.ron").exists());

    // Default Settings, then load it back: one undo step each.
    rig.right_click(rig.harness.get_by_label("Reverb").rect().center());
    rig.harness.get_by_label("Default Settings").click();
    rig.frames(2);
    assert_eq!(decay(&rig), 2.0);
    rig.right_click(rig.harness.get_by_label("Reverb").rect().center());
    rig.harness.get_by_label_contains("Load Preset").click();
    rig.frames(2);
    rig.harness.get_by_label("Long tail").click();
    rig.frames(2);
    assert_eq!(decay(&rig), 7.5);
    assert_eq!(rig.app().session.undo_label(), Some("Load Preset"));

    // Instruments have them too, by the instrument picker.
    let midi = rig.app().session.project.track_ids()[0];
    rig.app_mut()
        .session
        .edit("Set", |p| p.set_instrument_param(midi, "cutoff", 500.0));
    rig.app_mut()
        .session
        .save_preset(crate::session::PresetTarget::Instrument(midi), "Dark");
    rig.click(rig.header_point(midi));
    rig.frames(2);
    rig.harness.get_by_label("Presets").click();
    rig.frames(2);
    rig.harness.get_by_label("Default Settings").click();
    rig.frames(2);
    let cutoff = |rig: &Rig| {
        rig.app()
            .session
            .project
            .track(midi)
            .unwrap()
            .instrument_param("cutoff")
    };
    assert_eq!(cutoff(&rig), Some(3000.0));
    let presets = rig
        .app()
        .session
        .presets(crate::session::PresetTarget::Instrument(midi));
    rig.app_mut()
        .session
        .load_preset(crate::session::PresetTarget::Instrument(midi), &presets[0]);
    assert_eq!(cutoff(&rig), Some(500.0));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn drag_to_reorder_and_fold() {
    let mut rig = Rig::wide("reorder");
    let track = rig.app().session.project.track_ids()[2];
    rig.click(rig.header_point(track));
    rig.frames(2);
    add(&mut rig, "Delay");
    add(&mut rig, "Reverb");
    add(&mut rig, "Compressor");
    assert_eq!(names(&rig, Some(track)), ["Delay", "Reverb", "Compressor"]);

    // Drag the compressor's name onto the left half of the delay.
    let from = rig.harness.get_by_label("Compressor").rect().center();
    let delay = rig.harness.get_by_label("Delay").rect();
    rig.drag(from, delay.left_center() + vec2(4.0, 30.0));
    assert_eq!(names(&rig, Some(track)), ["Compressor", "Delay", "Reverb"]);
    assert_eq!(rig.app().session.undo_label(), Some("Move Effect"));

    // Double-click a name to fold it to its header: its knobs go.
    let reverb = rig.app().session.project.devices(Some(track)).unwrap()[2].id;
    let decay_knobs = |rig: &Rig| rig.harness.query_all_by_label("Decay").count();
    assert_eq!(decay_knobs(&rig), 1);
    let name = rig.harness.get_by_label("Reverb").rect().center();
    rig.double_click(name);
    assert!(rig.app().session.project.device(reverb).unwrap().folded);
    // The panel slides shut (briefly), then its knobs are gone.
    rig.settle();
    assert_eq!(decay_knobs(&rig), 0);
    rig.screenshot("folded");
    // Folding isn't an undo step, and undoing something else keeps it.
    assert_eq!(rig.app().session.undo_label(), Some("Move Effect"));
    rig.app_mut().session.undo();
    rig.frames(2);
    assert!(rig.app().session.project.device(reverb).unwrap().folded);
}

/// The middle of an effect's name in its title bar: the leftmost one so
/// called.
fn bar(rig: &Rig, name: &str) -> Pos2 {
    bars(rig, name)[0]
}

/// Every effect called `name`, left to right.
fn bars(rig: &Rig, name: &str) -> Vec<Pos2> {
    let mut found: Vec<Pos2> = rig
        .harness
        .get_all_by_label(name)
        .map(|n| n.rect().center())
        // Below the timeline: in the device view.
        .filter(|p| p.y > 400.0)
        .collect();
    found.sort_by(|a, b| a.x.total_cmp(&b.x));
    found
}

/// The title bar button called `label` on the bar of the effect at `name`.
fn bar_button(rig: &Rig, label: &str, name: Pos2) -> Pos2 {
    rig.harness
        .get_all_by_label(label)
        .map(|n| n.rect().center())
        .filter(|p| (p.y - name.y).abs() < 6.0 && p.x > name.x)
        .min_by(|a, b| a.x.total_cmp(&b.x))
        .unwrap_or_else(|| panic!("{label} beside {name:?}"))
}

#[test]
fn select_duplicate_copy_paste_and_remove_effects() {
    let mut rig = Rig::wide("rack");
    let ids = rig.app().session.project.track_ids();
    let (track, other) = (ids[2], ids[3]);
    rig.click(rig.header_point(track));
    rig.frames(2);
    add(&mut rig, "EQ");
    add(&mut rig, "Compressor");
    add(&mut rig, "Reverb");
    let chain = |rig: &Rig| -> Vec<project::DeviceId> {
        let devices = rig.app().session.project.devices(Some(track)).unwrap();
        devices.iter().map(|d| d.id).collect()
    };
    let [_, compressor, reverb] = chain(&rig)[..] else {
        panic!("three effects");
    };
    let selection = |rig: &Rig| rig.app().timeline.selection.devices.clone();

    // Click a title bar to select its effect; Ctrl+D puts a copy after it.
    rig.click(bar(&rig, "Compressor"));
    assert_eq!(selection(&rig), [compressor]);
    rig.screenshot("selected");
    rig.key(Modifiers::COMMAND, egui::Key::D);
    assert_eq!(
        names(&rig, Some(track)),
        ["EQ", "Compressor", "Compressor", "Reverb"]
    );
    assert_eq!(rig.app().session.undo_label(), Some("Duplicate Effect"));
    let copy = chain(&rig)[2];
    assert_eq!(selection(&rig), [copy], "the copy is selected");

    // Delete removes it, and the next one along is selected. The track
    // stays: the keyboard went to the effects.
    rig.key(Modifiers::NONE, egui::Key::Delete);
    assert_eq!(names(&rig, Some(track)), ["EQ", "Compressor", "Reverb"]);
    assert_eq!(selection(&rig), [reverb]);
    assert!(rig.app().session.project.track(track).is_some());

    // Ctrl-click adds one; Delete removes both, as one step.
    rig.click_with(bar(&rig, "EQ"), Modifiers::COMMAND);
    rig.key(Modifiers::NONE, egui::Key::Delete);
    assert_eq!(names(&rig, Some(track)), ["Compressor"]);
    assert_eq!(rig.app().session.undo_label(), Some("Remove Effects"));
    rig.app_mut().session.undo();
    rig.frames(2);
    assert_eq!(names(&rig, Some(track)), ["EQ", "Compressor", "Reverb"]);

    // The bar's own buttons act on their effect.
    let button = bar_button(&rig, "Duplicate effect", bar(&rig, "Reverb"));
    rig.click(button);
    assert_eq!(
        names(&rig, Some(track)),
        ["EQ", "Compressor", "Reverb", "Reverb"]
    );
    let copy = *bars(&rig, "Reverb").last().unwrap();
    rig.click(bar_button(&rig, "Remove effect", copy));
    assert_eq!(names(&rig, Some(track)), ["EQ", "Compressor", "Reverb"]);

    // Copy, then paste into another track's chain. The keyboard follows
    // the last click, so click in the device view before pasting.
    rig.click(bar(&rig, "Compressor"));
    rig.event(Event::Copy);
    rig.click(rig.header_point(other));
    rig.frames(2);
    let other_name = rig.app().session.project.track(other).unwrap().name.clone();
    rig.click(bar(&rig, &other_name));
    rig.event(Event::Paste("canopy effects".into()));
    rig.frames(2);
    assert_eq!(names(&rig, Some(other)), ["Compressor"]);
    assert_eq!(rig.app().session.undo_label(), Some("Paste Effect"));
    assert_eq!(names(&rig, Some(track)), ["EQ", "Compressor", "Reverb"]);
    rig.screenshot("pasted");
}

#[test]
fn drag_effects_to_other_tracks_and_the_master() {
    let mut rig = Rig::wide("rack_drag");
    let ids = rig.app().session.project.track_ids();
    let (track, other) = (ids[2], ids[3]);
    let other_name = rig.app().session.project.track(other).unwrap().name.clone();
    rig.click(rig.header_point(track));
    rig.frames(2);
    add(&mut rig, "Delay");
    add(&mut rig, "Reverb");

    // Dragged by its bar onto another track's header, it moves there.
    rig.drag(bar(&rig, "Reverb"), rig.header_point(other));
    assert_eq!(names(&rig, Some(track)), ["Delay"]);
    assert_eq!(names(&rig, Some(other)), ["Reverb"]);
    assert_eq!(rig.app().session.undo_label(), Some("Move Effect"));
    let message = rig.app().session.message.as_ref().unwrap().text.clone();
    assert_eq!(message, format!("Moved Reverb to {other_name}"));

    // With Ctrl held, a copy goes and the original stays.
    rig.drag_with(
        bar(&rig, "Delay"),
        rig.header_point(other),
        Modifiers::COMMAND,
    );
    assert_eq!(names(&rig, Some(track)), ["Delay"]);
    assert_eq!(names(&rig, Some(other)), ["Reverb", "Delay"]);
    assert_eq!(rig.app().session.undo_label(), Some("Copy Effect"));

    // The bar's menu sends it to the master.
    rig.click(bar_button(&rig, "Effect menu", bar(&rig, "Delay")));
    rig.frames(2);
    rig.screenshot("menu");
    rig.harness.get_by_label_contains("Move to").click();
    rig.frames(2);
    let master = rig
        .harness
        .get_all_by_label("Master")
        .map(|n| n.rect().center())
        .find(|p| p.x < 1000.0)
        .expect("the menu's Master");
    rig.click(master);
    rig.frames(2);
    assert_eq!(names(&rig, Some(track)), Vec::<String>::new());
    assert_eq!(names(&rig, None), ["Delay"]);
}

#[test]
fn shape_the_eq() {
    let mut rig = Rig::new("eq");
    let track = rig.app().session.project.track_ids()[2];
    rig.click(rig.header_point(track));
    rig.frames(2);
    add(&mut rig, "EQ");
    let eq = rig.app().session.project.devices(Some(track)).unwrap()[0].clone();
    let params = eq.effect.descriptor().unwrap().params;
    let device = eq.id;
    let id = eframe::egui::Id::new(("panel", Owner::Device(device), "curve")).with("rect");
    let rect: eframe::egui::Rect = rig.harness.ctx.data(|d| d.get_temp(id)).unwrap();
    let param = |rig: &Rig, id: &str| {
        rig.app()
            .session
            .project
            .device(device)
            .unwrap()
            .param(id)
            .unwrap()
    };

    // Drag the third band up 6 dB.
    let from = eq_curve::handle_pos(rect, params, &eq.values(), 2);
    let six_db = eq_curve::y_of(rect, 0.0) - eq_curve::y_of(rect, 6.0);
    rig.drag(from, from - vec2(0.0, six_db));
    let gain = param(&rig, "b3_gain");
    assert!((gain - 6.0).abs() < 0.5, "gain {gain}");
    assert_eq!(rig.app().session.undo_label(), Some("Change Effect"));

    // Double-click an empty spot for a new band there.
    let on = |rig: &Rig| {
        (1..=effects::eq::BANDS)
            .filter(|b| param(rig, &format!("b{b}_on")) >= 0.5)
            .count()
    };
    let before = on(&rig);
    let spot = eframe::egui::pos2(rect.left() + rect.width() * 0.8, eq_curve::y_of(rect, -9.0));
    rig.double_click(spot);
    assert_eq!(on(&rig), before + 1);
    let added = (1..=effects::eq::BANDS)
        .find(|b| {
            param(&rig, &format!("b{b}_on")) >= 0.5 && param(&rig, &format!("b{b}_gain")) < -6.0
        })
        .expect("a band at -9 dB");
    assert!(param(&rig, &format!("b{added}_freq")) > 3000.0);
    // It's selected: its controls are the ones shown.
    rig.harness.get_by_label(&format!("Band {added}"));
    rig.screenshot("added");

    // Right-click its handle to remove it again.
    let values = rig.app().session.project.device(device).unwrap().values();
    let handle = eq_curve::handle_pos(rect, params, &values, added - 1);
    rig.right_click(handle);
    rig.harness.get_by_label("Remove").click();
    rig.frames(2);
    assert_eq!(on(&rig), before);
}

#[test]
fn knobs_drag_scroll_type_and_reset() {
    let mut rig = Rig::new("knobs");
    let track = rig.app().session.project.track_ids()[2];
    rig.click(rig.header_point(track));
    rig.frames(2);
    add(&mut rig, "Reverb");
    let reverb = rig.app().session.project.devices(Some(track)).unwrap()[0].id;
    let decay = |rig: &Rig| {
        rig.app()
            .session
            .project
            .device(reverb)
            .unwrap()
            .param("decay")
            .unwrap()
    };
    let knob = rig.harness.get_by_label("Decay").rect().center();

    // Up turns it up, as one undo step.
    rig.drag(knob, knob - vec2(0.0, 60.0));
    let dragged = decay(&rig);
    assert!(dragged > 4.0, "{dragged}");
    assert_eq!(rig.app().session.undo_label(), Some("Change Effect"));

    // Shift makes the same drag a fine one.
    rig.drag_with(knob, knob + vec2(0.0, 60.0), Modifiers::SHIFT);
    let fine = decay(&rig);
    assert!(
        fine < dragged && fine > dragged * 0.7,
        "{fine} from {dragged}"
    );

    // The wheel steps it.
    rig.event(Event::PointerMoved(knob));
    rig.event(Event::MouseWheel {
        unit: MouseWheelUnit::Line,
        delta: vec2(0.0, -1.0),
        modifiers: Modifiers::NONE,
        phase: eframe::egui::TouchPhase::Move,
    });
    assert!(decay(&rig) < fine);

    // Ctrl+click goes back to the default.
    rig.click_with(knob, Modifiers::COMMAND);
    assert_eq!(decay(&rig), 2.0);
    assert_eq!(rig.app().session.undo_label(), Some("Reset Parameter"));

    // Double-click to type a value: what's typed replaces what's shown,
    // in any unit the knob's reads.
    rig.double_click(knob);
    rig.screenshot("typing");
    rig.event(Event::Text("3.5 s".into()));
    rig.key(Modifiers::NONE, Key::Enter);
    assert_eq!(decay(&rig), 3.5);
    assert_eq!(rig.app().session.undo_label(), Some("Change Effect"));
    rig.double_click(knob);
    rig.event(Event::Text("800 ms".into()));
    rig.key(Modifiers::NONE, Key::Enter);
    assert!((decay(&rig) - 0.8).abs() < 1e-6);
    // Escape, or something that isn't a value, leaves it.
    rig.double_click(knob);
    rig.event(Event::Text("5".into()));
    rig.key(Modifiers::NONE, Key::Escape);
    assert!((decay(&rig) - 0.8).abs() < 1e-6);
    rig.double_click(knob);
    rig.event(Event::Text("long".into()));
    rig.key(Modifiers::NONE, Key::Enter);
    assert!((decay(&rig) - 0.8).abs() < 1e-6);

    // The right-click menu types and resets too.
    rig.right_click(knob);
    rig.harness.get_by_label("Reset to Default").click();
    rig.frames(2);
    assert_eq!(decay(&rig), 2.0);
    rig.right_click(knob);
    rig.harness.get_by_label("Type Value…").click();
    rig.frames(2);
    rig.event(Event::Text("12".into()));
    rig.key(Modifiers::NONE, Key::Enter);
    assert_eq!(decay(&rig), 12.0);
}

#[test]
fn choices_switch_and_automate() {
    let mut rig = Rig::new("choices");
    let track = rig.app().session.project.track_ids()[2];
    rig.click(rig.header_point(track));
    rig.frames(2);
    add(&mut rig, "Compressor");
    let compressor = rig.app().session.project.devices(Some(track)).unwrap()[0].id;
    let model = |rig: &Rig| {
        rig.app()
            .session
            .project
            .device(compressor)
            .unwrap()
            .param("model")
            .unwrap()
    };
    let fet = rig.harness.get_by_label("FET").rect().center();
    rig.click(fet);
    assert_eq!(model(&rig), 1.0);
    assert_eq!(rig.app().session.undo_label(), Some("Change Effect"));

    // Right-click any of its buttons to automate the choice.
    rig.right_click(fet);
    rig.harness.get_by_label("Automate").click();
    rig.frames(3);
    let target = AutomationTarget::DeviceParam {
        device: compressor,
        param: "model".into(),
    };
    assert_eq!(
        rig.app().timeline.automation.get(&track).map(Vec::as_slice),
        Some(&[target][..])
    );
    assert_eq!(model(&rig), 1.0, "right-clicking didn't change it");
}

#[test]
fn route_a_sidechain_from_the_header() {
    let mut rig = Rig::new("sidechain");
    let ids = rig.app().session.project.track_ids();
    let (kick, bass) = (ids[0], ids[2]);
    rig.click(rig.header_point(bass));
    rig.frames(2);
    add(&mut rig, "Compressor");
    let compressor = rig.app().session.project.devices(Some(bass)).unwrap()[0].id;
    rig.harness.get_by_label("Sidechain").click();
    rig.frames(2);
    let kick_name = rig.app().session.project.track(kick).unwrap().name.clone();
    // The menu's entry, below the track's own header.
    let entry = rig
        .harness
        .get_all_by_label(&kick_name)
        .map(|n| n.rect().center())
        .max_by(|a, b| a.y.total_cmp(&b.y))
        .unwrap();
    rig.click(entry);
    rig.frames(2);
    let routed = rig
        .app()
        .session
        .project
        .device(compressor)
        .unwrap()
        .sidechain;
    assert_eq!(routed, Some(kick));
    assert_eq!(rig.app().session.undo_label(), Some("Route Sidechain"));
    rig.harness.get_by_label(&format!("SC: {kick_name}"));
    rig.screenshot("routed");
}

#[test]
fn the_master_has_effects_too() {
    let mut rig = Rig::new("master_effects");
    rig.harness.get_by_label("Master").click();
    rig.frames(2);
    assert!(rig.app().timeline.selection.master);
    add(&mut rig, "Compressor");
    assert_eq!(names(&rig, None), ["Compressor"]);
    rig.screenshot("master");
    // Selecting a track goes back to it.
    let track = rig.app().session.project.tracks[0].id;
    rig.click(rig.header_point(track));
    assert!(!rig.app().timeline.selection.master);
}
