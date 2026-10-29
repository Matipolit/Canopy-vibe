//! The History panel: every undo step listed, a click going back or
//! forward to any of them.

use eframe::egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;

use super::Rig;

#[test]
fn click_a_step_in_the_history_to_go_back_and_forward_to_it() {
    let mut rig = Rig::new("history");
    let track = rig.app().session.project.track_ids()[0];
    let original = rig.app().session.project.track(track).unwrap().name.clone();
    for name in ["Bass", "Sub", "Low end"] {
        rig.app_mut()
            .session
            .edit(&format!("Rename to {name}"), |p| {
                p.rename_track(track, name)
            })
            .unwrap();
    }
    let name = |rig: &Rig| rig.app().session.project.track(track).unwrap().name.clone();

    rig.key(Modifiers::COMMAND, Key::H);
    rig.frames(10);
    assert!(rig.app().history_open);
    rig.screenshot("open");

    // Back two steps, to the first rename.
    rig.harness.get_by_label("Rename to Bass").click();
    rig.frames(2);
    assert_eq!(name(&rig), "Bass");
    assert_eq!(rig.app().session.history().position(), 1);
    assert_eq!(rig.app().session.redo_label(), Some("Rename to Sub"));
    rig.screenshot("back");

    // Back to before any of them, then forward to the last.
    rig.harness.get_by_label("Start").click();
    rig.frames(2);
    assert_eq!(name(&rig), original);
    rig.harness.get_by_label("Rename to Low end").click();
    rig.frames(2);
    assert_eq!(name(&rig), "Low end");

    // Undo moves through the same list.
    rig.key(Modifiers::COMMAND, Key::Z);
    rig.frames(2);
    assert_eq!(rig.app().session.history().position(), 2);
    assert_eq!(name(&rig), "Sub");

    rig.key(Modifiers::COMMAND, Key::H);
    rig.frames(10);
    assert!(!rig.app().history_open);
}
