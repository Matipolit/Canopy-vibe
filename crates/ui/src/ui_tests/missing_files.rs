//! Audio files that moved away from a project, and locating them again.

use std::path::{Path, PathBuf};

use egui_kittest::kittest::Queryable;
use media::MediaError;
use midi::Tick;
use project::{SourceId, TrackId};

use super::{Rig, temp_dir, write_sine_wav};
use crate::session::SourceState;

/// Two 2 s tones, `a.wav` and `b.wav`, imported from `dir/old` onto the
/// first audio track and saved in `dir/project`; then `old` is renamed to
/// `new` and the project opened again, so neither file is where it says.
fn moved_folder(rig: &mut Rig, name: &str) -> (PathBuf, TrackId, [SourceId; 2]) {
    let dir = temp_dir(name);
    let (old, new, saved) = (dir.join("old"), dir.join("new"), dir.join("project"));
    for folder in [&old, &saved] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let _ = std::fs::remove_dir_all(&new);
    write_sine_wav(&old.join("a.wav"), 2, 220.0);
    write_sine_wav(&old.join("b.wav"), 2, 330.0);
    let track = rig.app().session.project.track_ids()[2];
    rig.app_mut().session.import_files(
        vec![old.join("a.wav"), old.join("b.wav")],
        Some(track),
        Tick::ZERO,
        project::TrackPosition::END,
    );
    rig.wait_for("the imports", |app| app.session.project.sources.len() == 2);
    let file = saved.join("song.canopy");
    assert!(rig.app_mut().session.save_as(&file));
    std::fs::rename(&old, &new).unwrap();

    rig.app_mut().session.open(&file);
    // Imports land in whichever order they decode.
    let id = |rig: &Rig, name: &str| {
        let sources = &rig.app().session.project.sources;
        sources.iter().find(|s| s.name() == name).unwrap().id
    };
    let ids = [id(rig, "a"), id(rig, "b")];
    rig.wait_for("both to fail", |app| ids.iter().all(|id| missing(app, *id)));
    (new, track, ids)
}

fn missing(app: &crate::app::CanopyApp, id: SourceId) -> bool {
    matches!(
        app.session.source_state(id),
        Some(SourceState::Failed(MediaError::NotFound))
    )
}

fn ready(app: &crate::app::CanopyApp, id: SourceId) -> bool {
    matches!(app.session.source_state(id), Some(SourceState::Ready(_)))
}

fn path(rig: &Rig, id: SourceId) -> PathBuf {
    rig.app().session.project.source(id).unwrap().path.clone()
}

#[test]
fn a_missing_clip_offers_to_locate_its_file_and_a_loaded_one_doesnt() {
    let mut rig = Rig::new("locate_menu");
    let (_, track, _) = moved_folder(&mut rig, "locate-menu");
    rig.frames(2);
    rig.right_click(rig.lane_point(track, 0.5));
    rig.screenshot("missing");
    assert!(rig.harness.query_by_label("Locate File…").is_some());
    assert!(rig.harness.query_by_label_contains("Duplicate").is_some());
    rig.key(eframe::egui::Modifiers::NONE, eframe::egui::Key::Escape);
    rig.frames(2);

    let dir = temp_dir("locate-menu-loaded");
    let wav = dir.join("here.wav");
    write_sine_wav(&wav, 2, 220.0);
    rig.app_mut().session.new_project();
    let track = rig.app().session.project.track_ids()[2];
    rig.app_mut().session.import_files(
        vec![wav],
        Some(track),
        Tick::ZERO,
        project::TrackPosition::END,
    );
    rig.wait_for("the import", |app| !app.session.project.sources.is_empty());
    rig.frames(2);
    rig.right_click(rig.lane_point(track, 0.5));
    assert!(rig.harness.query_by_label_contains("Duplicate").is_some());
    assert!(rig.harness.query_by_label("Locate File…").is_none());
}

#[test]
fn locating_one_file_finds_the_others_beside_it_in_one_undo_step() {
    let mut rig = Rig::new("locate_folder");
    let (new, _, [a, b]) = moved_folder(&mut rig, "locate-folder");

    rig.app_mut().session.locate_source(a, new.join("a.wav"));
    rig.wait_for("both to load", |app| ready(app, a) && ready(app, b));
    assert_eq!(path(&rig, a), new.join("a.wav"));
    assert_eq!(path(&rig, b), new.join("b.wav"));
    let session = &rig.app().session;
    assert_eq!(session.undo_label(), Some("Locate File"));
    let message = session.message.as_ref().unwrap();
    assert!(!message.error);
    assert_eq!(message.text, "Located a.wav and 1 other file beside it");

    // Undo puts the old paths back, and the clips are missing again: not
    // still playing what was located.
    rig.app_mut().session.undo();
    rig.wait_for("both to fail again", |app| {
        missing(app, a) && missing(app, b)
    });
    assert_eq!(path(&rig, a).parent().unwrap().file_name().unwrap(), "old");

    rig.app_mut().session.redo();
    rig.wait_for("both to load again", |app| ready(app, a) && ready(app, b));
}

#[test]
fn a_located_file_of_another_length_is_used_with_a_warning() {
    let mut rig = Rig::new("locate_other_length");
    let (new, _, [a, b]) = moved_folder(&mut rig, "locate-other-length");
    let longer = new.join("longer.wav");
    write_sine_wav(&longer, 3, 220.0);

    rig.app_mut().session.locate_source(a, longer.clone());
    rig.wait_for("it to load", |app| ready(app, a));
    let source = rig.app().session.project.source(a).unwrap().clone();
    assert_eq!((source.path, source.frames), (longer, 3 * 44_100));
    let message = rig.app().session.message.as_ref().unwrap();
    assert!(message.error, "{}", message.text);
    assert!(message.text.contains("may not line up"), "{}", message.text);
    // The others are still looked for under their own names.
    rig.wait_for("the other to load", |app| ready(app, b));
}

#[test]
fn a_file_that_wont_load_leaves_the_clip_missing() {
    let mut rig = Rig::new("locate_broken");
    let (new, _, [a, _]) = moved_folder(&mut rig, "locate-broken");
    let broken = new.join("broken.wav");
    std::fs::write(&broken, b"not audio").unwrap();

    rig.app_mut().session.locate_source(a, broken);
    rig.wait_for("the error", |app| {
        app.session
            .message
            .as_ref()
            .is_some_and(|m| m.error && m.text.starts_with("Couldn't load"))
    });
    assert!(missing(rig.app(), a));
    assert_eq!(
        path(&rig, a),
        Path::new(&new).with_file_name("old").join("a.wav")
    );
    assert_ne!(rig.app().session.undo_label(), Some("Locate File"));
}
