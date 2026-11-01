//! The History panel: every undo step listed, a click going back or
//! forward to any of them.

use eframe::egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use project::Origin;
use serde_json::json;

use super::Rig;
use super::mcp::{Client, listen};

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

#[test]
fn llm_steps_are_tagged_in_the_history() {
    let mut rig = Rig::new("history_llm");
    let advert = listen(&mut rig, "history");
    let mut client = Client::connect(&advert);
    let track = rig.app().session.project.track_ids()[0];

    // Two edits over the socket: two steps, each the client's.
    let (_, error) = client.call(
        &mut rig,
        "edit_track",
        json!({ "track": track.0, "name": "Bass" }),
    );
    assert!(!error);
    let (_, error) = client.call(
        &mut rig,
        "edit_track",
        json!({ "track": track.0, "volume_db": -3 }),
    );
    assert!(!error);
    let label = rig.app().session.undo_label().unwrap().to_owned();
    assert_eq!(label, "LLM: Edit Bass (volume)");
    let steps: Vec<Option<String>> = rig
        .app()
        .session
        .history()
        .steps()
        .map(|s| s.origin.client().map(str::to_owned))
        .collect();
    assert_eq!(
        steps[steps.len() - 2..],
        [Some("Test Client".into()), Some("Test Client".into())]
    );

    // The model sees who made them too.
    let (history, _) = client.call(&mut rig, "get_history", json!({ "last": 1 }));
    assert_eq!(history["steps"][0]["origin"], json!("llm"));
    assert_eq!(history["steps"][0]["client"], json!("Test Client"));

    // The panel tags them and gathers them under one header.
    rig.key(Modifiers::COMMAND, Key::H);
    rig.frames(10);
    rig.harness.get_by_label(&format!("{label} · Test Client"));
    rig.harness.get_by_label("Test Client · 2 steps");
    rig.screenshot("llm");

    // They undo like any other step.
    rig.key(Modifiers::COMMAND, Key::Z);
    rig.frames(2);
    let bass = rig.app().session.project.track(track).unwrap().clone();
    assert_eq!((bass.name.as_str(), bass.mixer.volume_db), ("Bass", 0.0));

    // Steps the session records for an LLM by other routes are its too.
    let session = &mut rig.app_mut().session;
    session.set_origin(Origin::Llm {
        client: "Other".into(),
    });
    session.edit("Rename to Sub", |p| p.rename_track(track, "Sub"));
    session.set_origin(Origin::User);
    session.edit("Rename to Low", |p| p.rename_track(track, "Low"));
    let steps: Vec<_> = session
        .history()
        .steps()
        .map(|s| (s.label.to_owned(), s.origin.clone()))
        .collect();
    let other = Origin::Llm {
        client: "Other".into(),
    };
    assert_eq!(
        steps[steps.len() - 2..],
        [
            ("LLM: Rename to Sub".to_owned(), other),
            ("Rename to Low".to_owned(), Origin::User),
        ]
    );
}
