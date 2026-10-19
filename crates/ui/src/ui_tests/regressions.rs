//! Regression tests for bugs found in review: gestures released off their
//! widget, failed saves, stale loads, shortcut focus, and more.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Event, Key, Modifiers, pos2, vec2};
use egui_kittest::kittest::Queryable;
use project::AutomationTarget;

use super::{Rig, TestFile, temp_dir, write_sine_wav};

fn volume_points(rig: &Rig, track: project::TrackId) -> Vec<project::AutomationPoint> {
    rig.app()
        .session
        .project
        .track(track)
        .unwrap()
        .automation_lane(&AutomationTarget::Volume)
        .map(|l| l.points.clone())
        .unwrap_or_default()
}

/// A volume lane on the first track with one point at bar 3.
fn lane_with_a_point(rig: &mut Rig) -> (project::TrackId, egui::Rect, f32) {
    let track = rig.app().session.project.tracks[0].id;
    rig.harness
        .get_all_by_label("A")
        .next()
        .expect("an A button")
        .click();
    rig.frames(3);
    let lane = rig
        .app()
        .timeline
        .automation_rect(track)
        .expect("lane is shown");
    let x = rig.app().timeline.viewport.x(2.0 * 3840.0);
    rig.double_click(pos2(x + 1.0, lane.center().y));
    assert_eq!(volume_points(rig, track).len(), 1);
    (track, lane, x)
}

#[test]
fn shift_tab_keeps_shortcuts_working() {
    let mut rig = Rig::new("shift_tab");
    rig.click(pos2(700.0, 420.0));
    let before = rig.app().piano_roll_shown();
    rig.key(Modifiers::SHIFT, Key::Tab);
    assert_ne!(rig.app().piano_roll_shown(), before);
    rig.key(Modifiers::NONE, Key::Space);
    assert!(rig.app().session.is_playing(), "Space still plays");
    rig.key(Modifiers::SHIFT, Key::Tab);
    assert_eq!(
        rig.app().piano_roll_shown(),
        before,
        "and Shift+Tab switches back"
    );
}

#[test]
fn automation_point_released_off_the_lane_still_moves() {
    let mut rig = Rig::new("automation_off_lane");
    let (track, lane, x) = lane_with_a_point(&mut rig);
    let y = lane.center().y;
    rig.drag(pos2(x, y), pos2(lane.right() + 40.0, y));
    rig.frames(3);
    assert!(volume_points(&rig, track)[0].tick.get() > 7680);

    // Nothing stale is left behind to resurface in the next edit.
    let bar_one = rig.app().timeline.viewport.x(3840.0);
    rig.double_click(pos2(bar_one + 1.0, y));
    let ticks: Vec<i64> = volume_points(&rig, track)
        .iter()
        .map(|p| p.tick.get())
        .collect();
    assert_eq!(ticks.len(), 2);
    assert_eq!(ticks[0], 3840);
}

#[test]
fn escape_cancels_an_automation_drag() {
    let mut rig = Rig::new("automation_escape");
    let (track, lane, x) = lane_with_a_point(&mut rig);
    let before = volume_points(&rig, track);
    let y = lane.center().y;
    rig.event(Event::PointerMoved(pos2(x, y)));
    rig.press(pos2(x, y), true, Modifiers::NONE);
    for i in 1..=4 {
        rig.event(Event::PointerMoved(pos2(x + 30.0 * i as f32, y)));
    }
    rig.key(Modifiers::NONE, Key::Escape);
    rig.press(pos2(x + 120.0, y), false, Modifiers::NONE);
    rig.frames(2);
    assert_eq!(volume_points(&rig, track), before);
}

#[test]
fn piano_roll_note_released_outside_is_committed() {
    let mut rig = Rig::new("note_outside");
    let track = rig.app().session.project.tracks[0].id;
    rig.double_click(rig.lane_point(track, 1.1));
    let clip = rig.app().timeline.selection.open_clip.unwrap();
    rig.frames(2);
    let point = rig.app().piano_roll.point(3840.0 + 30.0, 60);
    rig.double_click(point);
    assert_eq!(rig.notes(clip), [(0, 60)]);
    rig.frames(45);
    let top = rig.app().piano_roll_rect().top();
    rig.drag(point + vec2(5.0, 0.0), pos2(point.x + 5.0, top - 30.0));
    rig.frames(3);
    let notes = rig.notes(clip);
    assert_eq!(notes.len(), 1);
    assert!(notes[0].1 > 60, "moved up: {notes:?}");
}

#[test]
fn clip_drag_survives_zooming_mid_drag() {
    let mut rig = Rig::new("zoom_mid_drag");
    let track = rig.app().session.project.tracks[0].id;
    rig.double_click(rig.lane_point(track, 0.1));
    let clip = rig.app().session.project.track(track).unwrap().clips()[0].id();
    rig.frames(45);
    let from = rig.lane_point(track, 0.5);
    rig.event(Event::PointerMoved(from));
    rig.press(from, true, Modifiers::NONE);
    let far = rig.lane_point(track, 7.0);
    for i in 1..=8 {
        let t = i as f32 / 8.0;
        rig.event(Event::PointerMoved(from + (far - from) * t));
    }
    rig.event(Event::Zoom(4.0));
    rig.frames(2);
    rig.event(Event::PointerMoved(far + vec2(10.0, 0.0)));
    rig.press(far + vec2(10.0, 0.0), false, Modifiers::NONE);
    rig.frames(3);
    assert_ne!(
        rig.app()
            .session
            .project
            .clip(clip)
            .unwrap()
            .1
            .start()
            .get(),
        0
    );
}

#[test]
fn failed_save_in_the_dialog_keeps_the_project() {
    let mut rig = Rig::new("failed_save");
    rig.app_mut().session.path = Some(PathBuf::from(
        "/nonexistent-dir-for-canopy-tests/song.canopy",
    ));
    rig.key(Modifiers::COMMAND, Key::T);
    assert_eq!(rig.app().session.project.track_ids().len(), 5);
    rig.key(Modifiers::COMMAND, Key::N);
    rig.frames(2);
    rig.harness.get_by_label("Save").click();
    rig.frames(3);
    assert_eq!(rig.app().session.project.track_ids().len(), 5);
    assert!(rig.app().session.is_dirty());
}

#[test]
fn moving_a_note_before_the_clip_origin_keeps_its_start() {
    let mut rig = Rig::new("negative_note");
    let track = rig.app().session.project.tracks[0].id;
    rig.double_click(rig.lane_point(track, 1.1));
    let clip = rig.app().timeline.selection.open_clip.unwrap();
    rig.frames(45);
    let edge = rig.lane_point(track, 1.0) + vec2(2.0, 0.0);
    rig.drag(edge, rig.lane_point(track, 0.0));
    rig.frames(2);
    let point = rig.app().piano_roll.point(960.0 + 30.0, 60);
    rig.double_click(point);
    let (start, _) = rig.notes(clip)[0];
    assert!(start < 0);
    rig.frames(45);
    let up = rig.app().piano_roll.point(960.0 + 30.0, 62);
    rig.drag(point + vec2(4.0, 0.0), up + vec2(4.0, 0.0));
    assert_eq!(rig.notes(clip), [(start, 62)]);
}

#[test]
fn imports_still_loading_dont_land_in_the_next_project() {
    let mut rig = Rig::new("import_next");
    let dir = temp_dir("import-next");
    let wav = dir.join("sine.wav");
    write_sine_wav(&wav, 1, 220.0);
    let target = rig.lane_point(rig.app().session.project.track_ids()[2], 1.0);
    rig.event(Event::PointerMoved(target));
    rig.harness
        .input_mut()
        .dropped_files
        .push(std::sync::Arc::new(TestFile(wav)));
    rig.frames(1);
    rig.app_mut().session.new_project();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(10));
        rig.frames(1);
    }
    assert!(rig.app().session.project.sources.is_empty());
    assert!(!rig.app().session.is_dirty());
}

#[test]
fn dropping_only_unsupported_files_says_why() {
    let mut rig = Rig::new("unsupported");
    let dir = temp_dir("unsupported");
    let txt = dir.join("notes.txt");
    std::fs::write(&txt, "hi").unwrap();
    let target = rig.lane_point(rig.app().session.project.track_ids()[2], 1.0);
    rig.event(Event::PointerMoved(target));
    rig.harness
        .input_mut()
        .dropped_files
        .push(std::sync::Arc::new(TestFile(txt)));
    rig.frames(2);
    assert!(rig.app().session.message.as_ref().unwrap().error);
}

#[test]
fn save_as_keeps_dots_in_the_name() {
    let mut rig = Rig::new("dots");
    let dir = temp_dir("dots");
    assert!(rig.app_mut().session.save_as(&dir.join("mix v1.2")));
    let saved = rig.app().session.path.clone().unwrap();
    assert_eq!(saved.file_name().unwrap(), "mix v1.2.canopy");
}

#[test]
fn a_closed_projects_audio_doesnt_replace_the_next_ones() {
    let mut rig = Rig::new("stale_source");
    let dir = temp_dir("stale");
    let (big, small) = (dir.join("big.wav"), dir.join("small.wav"));
    write_sine_wav(&big, 60, 110.0);
    write_sine_wav(&small, 1, 440.0);

    let track = rig.app().session.project.track_ids()[2];
    rig.app_mut().session.import_files(
        vec![big],
        Some(track),
        midi::Tick::ZERO,
        project::TrackPosition::END,
    );
    rig.wait_for("big import", |app| !app.session.project.sources.is_empty());
    let file = dir.join("a.canopy");
    assert!(rig.app_mut().session.save_as(&file));

    // Reopen (the big file starts decoding again), then immediately start a
    // new project and import a small file, which gets the same source id.
    rig.app_mut().session.open(&file);
    rig.app_mut().session.new_project();
    let track = rig.app().session.project.track_ids()[2];
    rig.app_mut().session.import_files(
        vec![small],
        Some(track),
        midi::Tick::ZERO,
        project::TrackPosition::END,
    );
    rig.wait_for("small import", |app| {
        !app.session.project.sources.is_empty()
    });
    let id = rig.app().session.project.sources[0].id;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
        rig.frames(1);
    }
    let loaded = match rig.app().session.source_state(id) {
        Some(crate::session::SourceState::Ready(s)) => s.native_frames,
        _ => 0,
    };
    assert_eq!(loaded, 44_100);
}

#[test]
fn typing_a_tempo_is_one_undo_step() {
    let mut rig = Rig::new("typed_tempo");
    let field = rig
        .harness
        .get_all_by_role(egui::accesskit::Role::SpinButton)
        .map(|n| n.rect())
        .find(|r| r.top() < 60.0)
        .expect("the tempo field")
        .center();
    rig.click(field);
    rig.frames(2);
    rig.event(Event::Text("9".into()));
    assert_eq!(
        rig.app().session.project.tempo.tempo.to_bpm(),
        120.0,
        "not applied per keystroke"
    );
    rig.event(Event::Text("0".into()));
    rig.key(Modifiers::NONE, Key::Enter);
    // Stored as microseconds per beat, so 90 comes back a hair under.
    assert!((rig.app().session.project.tempo.tempo.to_bpm() - 90.0).abs() < 1e-3);
    rig.click(pos2(700.0, 420.0));
    rig.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(rig.app().session.project.tempo.tempo.to_bpm(), 120.0);
}

#[test]
fn an_import_finishing_mid_drag_waits_for_the_drag() {
    let mut rig = Rig::new("import_mid_drag");
    let dir = temp_dir("mid-drag");
    let wav = dir.join("sine.wav");
    write_sine_wav(&wav, 2, 220.0);
    let tracks = rig.app().session.project.track_ids();
    let row = rig.app().timeline.row_rect(tracks[0]).unwrap();
    let volume = pos2(row.left() + 75.0, row.top() + 33.0);
    rig.app_mut().session.import_files(
        vec![wav],
        Some(tracks[2]),
        midi::Tick::ZERO,
        project::TrackPosition::END,
    );
    rig.event(Event::PointerMoved(volume));
    rig.press(volume, true, Modifiers::NONE);
    for i in 1..=4 {
        rig.event(Event::PointerMoved(volume - vec2(10.0 * i as f32, 0.0)));
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        std::thread::sleep(Duration::from_millis(10));
        rig.frames(1);
    }
    assert!(
        rig.app().session.project.sources.is_empty(),
        "held back during the drag"
    );
    rig.press(volume - vec2(40.0, 0.0), false, Modifiers::NONE);
    rig.wait_for("the import", |app| !app.session.project.sources.is_empty());
    let dragged = rig
        .app()
        .session
        .project
        .track(tracks[0])
        .unwrap()
        .mixer
        .volume_db;
    assert!(dragged < 0.0);

    rig.click(pos2(700.0, 420.0));
    assert_eq!(rig.app().session.undo_label(), Some("Import Audio"));
    rig.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(rig.app().session.undo_label(), Some("Volume"));
    rig.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(
        rig.app()
            .session
            .project
            .track(tracks[0])
            .unwrap()
            .mixer
            .volume_db,
        0.0
    );
}

#[test]
fn piano_roll_keys_stay_out_of_the_save_dialog() {
    let mut rig = Rig::new("modal_keys");
    let track = rig.app().session.project.tracks[0].id;
    rig.double_click(rig.lane_point(track, 1.1));
    let clip = rig.app().timeline.selection.open_clip.unwrap();
    rig.frames(2);
    let point = rig.app().piano_roll.point(3840.0 + 30.0, 60);
    rig.double_click(point);
    rig.key(Modifiers::COMMAND, Key::N);
    assert!(rig.harness.query_by_label("Save changes?").is_some());
    rig.key(Modifiers::NONE, Key::Delete);
    assert_eq!(
        rig.notes(clip).len(),
        1,
        "Delete didn't reach the piano roll"
    );
    rig.key(Modifiers::NONE, Key::Escape);
    assert!(rig.harness.query_by_label("Save changes?").is_none());
}

#[test]
fn undo_leaves_folding_alone() {
    let mut rig = Rig::new("fold_undo");
    let ids = rig.app().session.project.track_ids();
    let group = rig
        .app_mut()
        .session
        .edit("Group", |p| p.group_tracks(&[ids[2], ids[3]]))
        .unwrap();
    rig.app_mut()
        .session
        .edit("Rename", |p| p.rename_track(ids[0], "Lead"))
        .unwrap();
    rig.frames(2);
    let row = rig.app().timeline.row_rect(group).unwrap();
    rig.click(pos2(row.left() + 16.0, row.top() + 12.0));
    assert!(rig.app().session.project.track(group).unwrap().is_folded());
    rig.click(pos2(700.0, 420.0));
    rig.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(
        rig.app().session.project.track(ids[0]).unwrap().name,
        "1 MIDI"
    );
    assert!(rig.app().session.project.track(group).unwrap().is_folded());
}
