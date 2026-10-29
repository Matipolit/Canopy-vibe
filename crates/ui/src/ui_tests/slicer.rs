//! The Slicer: loading a break by dropping it, slicing it by transients,
//! grid and hand, writing its slices to the piano roll, Alt-dropping a
//! file on the timeline, and a file it can't read.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Event, Key, Modifiers};
use egui_kittest::kittest::Queryable;
use midi::{NoteEventKind, Pitch, Velocity};
use project::{ClipRef, TrackId, TrackKind};

use super::{Rig, TestFile, peak, temp_dir};
use crate::slicers::FileState;

/// A one-bar break at 120 BPM (two seconds at 44.1 kHz): eight hits an
/// eighth apart, each a decaying tone of its own pitch.
pub(super) fn write_break(path: &Path) {
    let rate = 44_100u32;
    let hit = rate as usize / 4;
    let samples: Vec<i16> = (0..8 * hit)
        .map(|i| {
            let (k, at) = (i / hit, (i % hit) as f32);
            let hz = 150.0 * (1.0 + k as f32 * 0.25);
            let decay = (-at / 2500.0).exp();
            let tone = (std::f32::consts::TAU * hz * at / rate as f32).sin();
            (tone * decay * 24_000.0) as i16
        })
        .collect();
    let data = samples.len() as u32 * 2;
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((36 + data).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes()); // PCM
    bytes.extend(1u16.to_le_bytes()); // mono
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * 2).to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data.to_le_bytes());
    for s in samples {
        bytes.extend(s.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

/// The first MIDI track, selected, playing the Slicer, with the device
/// view showing it.
fn slicer_track(rig: &mut Rig) -> TrackId {
    let midi = rig.app().session.project.track_ids()[0];
    rig.click(rig.header_point(midi));
    rig.frames(2);
    let slicer = instruments::InstrumentRef::builtin("canopy.slicer");
    rig.app_mut()
        .session
        .edit("Change Instrument", |p| p.set_instrument(midi, slicer))
        .unwrap();
    rig.frames(3);
    midi
}

/// The slicer's waveform, as last drawn.
fn waveform(rig: &Rig) -> egui::Rect {
    rig.harness.get_by_label_contains("Slices: ").rect()
}

fn slices(rig: &Rig, track: TrackId) -> Vec<u64> {
    rig.app()
        .session
        .project
        .track(track)
        .unwrap()
        .slices()
        .to_vec()
}

/// Drops `file` at `pos` (with `modifiers` held).
fn drop_file(rig: &mut Rig, file: &Path, pos: egui::Pos2, modifiers: Modifiers) {
    rig.event(Event::ModifiersChanged(modifiers));
    rig.event(Event::PointerMoved(pos));
    rig.harness
        .input_mut()
        .dropped_files
        .push(std::sync::Arc::new(TestFile(file.to_owned())));
    rig.frames(2);
    rig.event(Event::ModifiersChanged(Modifiers::NONE));
}

/// A slicer track with the break dropped on its waveform and loaded.
fn loaded(rig: &mut Rig, name: &str) -> (TrackId, PathBuf) {
    let track = slicer_track(rig);
    let dir = temp_dir(name);
    let wav = dir.join("break.wav");
    write_break(&wav);
    let at = waveform(rig).center();
    drop_file(rig, &wav, at, Modifiers::NONE);
    rig.wait_for("the break", |app| {
        let t = app.session.project.track(track).unwrap();
        t.instrument_file().is_some() && !t.slices().is_empty()
    });
    rig.frames(3);
    (track, wav)
}

/// Plays `key` on the track for a tenth of a second.
fn play(rig: &mut Rig, track: TrackId, key: u8) -> Vec<f32> {
    let pitch = Pitch::new(key).unwrap();
    let session = &mut rig.app_mut().session;
    session.preview_note(
        track,
        NoteEventKind::On {
            pitch,
            velocity: Velocity::from_midi1(100),
        },
    );
    let out = rig.render_audio(4800);
    rig.app_mut().session.preview_note(
        track,
        NoteEventKind::Off {
            pitch,
            velocity: Velocity::CENTER,
        },
    );
    rig.render_audio(4800);
    out
}

#[test]
fn slice_a_break_by_grid_and_play_it() {
    let mut rig = Rig::new("slicer_grid");
    let (track, wav) = loaded(&mut rig, "slicer_grid");
    rig.screenshot("loaded");
    let t = rig.app().session.project.track(track).unwrap();
    assert_eq!(t.instrument_file(), Some(wav.as_path()));
    // Its tempo guessed from its length: a bar at 120.
    assert_eq!(t.instrument_param("original_tempo"), Some(120.0));
    // Sliced at its eight hits.
    let at_hits = slices(&rig, track);
    assert_eq!(at_hits.len(), 8, "{at_hits:?}");
    assert_eq!(rig.app().session.undo_label(), Some("Load Sample"));

    rig.harness.get_by_label("1/16").click();
    rig.frames(2);
    let grid = slices(&rig, track);
    assert_eq!(grid.len(), 16);
    assert!((grid[1] as i64 - 5512).abs() <= 1, "{grid:?}");
    assert_eq!(rig.app().session.undo_label(), Some("Slice by Grid"));
    rig.screenshot("sixteenths");

    // Every slice sounds on its key; past the last, nothing does.
    rig.wait_for("the loop in the engine", |app| {
        app.session.slicer_view(track).is_some()
    });
    rig.frames(2);
    assert!(peak(&play(&mut rig, track, 60)) > 0.1);
    assert!(peak(&play(&mut rig, track, 60 + 14)) > 0.1);
    assert_eq!(peak(&play(&mut rig, track, 60 + 16)), 0.0);

    rig.harness.get_by_label("Transients").click();
    rig.frames(2);
    assert_eq!(slices(&rig, track), at_hits);
    let _ = std::fs::remove_dir_all(wav.parent().unwrap());
}

#[test]
fn add_move_and_remove_a_marker() {
    let mut rig = Rig::new("slicer_markers");
    let (track, wav) = loaded(&mut rig, "slicer_markers");
    rig.harness.get_by_label("1/4").click();
    rig.frames(2);
    assert_eq!(slices(&rig, track), [0, 22_050, 44_100, 66_150]);
    let rect = waveform(&rig);
    let frames = 88_200.0;
    // Where frame `f` is drawn (the display's inset left and right).
    let area = rect.shrink2(egui::vec2(crate::theme::space::XS, 0.0));
    let x = |f: f32| area.left() + f / frames * area.width();
    let y = rect.center().y;

    // A double-click between markers adds one there.
    rig.double_click(egui::pos2(x(11_025.0), y));
    let added = slices(&rig, track);
    assert_eq!(added.len(), 5, "{added:?}");
    assert!((added[1] as f32 - 11_025.0).abs() < 300.0, "{added:?}");
    assert_eq!(rig.app().session.undo_label(), Some("Add Slice Marker"));

    // Dragged, it moves, as one undo step.
    rig.drag(
        egui::pos2(x(added[1] as f32), y),
        egui::pos2(x(16_000.0), y),
    );
    let moved = slices(&rig, track);
    assert_eq!(moved.len(), 5);
    assert!((moved[1] as f32 - 16_000.0).abs() < 300.0, "{moved:?}");
    assert_eq!(rig.app().session.undo_label(), Some("Move Slice Marker"));
    rig.screenshot("moved");

    // A double-click on it removes it.
    rig.double_click(egui::pos2(x(moved[1] as f32), y));
    assert_eq!(slices(&rig, track), [0, 22_050, 44_100, 66_150]);
    assert_eq!(rig.app().session.undo_label(), Some("Remove Slice Marker"));

    // So does Delete, once it's picked.
    rig.frames(45);
    rig.click(egui::pos2(x(44_100.0), y));
    rig.key(Modifiers::NONE, Key::Delete);
    assert_eq!(slices(&rig, track), [0, 22_050, 66_150]);
    // With none picked, Delete leaves the markers alone.
    rig.click(egui::pos2(x(10_000.0), y));
    rig.key(Modifiers::NONE, Key::Delete);
    assert_eq!(slices(&rig, track), [0, 22_050, 66_150]);
    let _ = std::fs::remove_dir_all(wav.parent().unwrap());
}

#[test]
fn slices_to_piano_roll_writes_the_loop() {
    let mut rig = Rig::new("slicer_piano_roll");
    let (track, wav) = loaded(&mut rig, "slicer_piano_roll");
    rig.harness.get_by_label("1/8").click();
    rig.frames(2);
    // The project at 140: the loop follows it, so its slices stay eighths.
    rig.app_mut()
        .session
        .edit("Tempo", |p| {
            p.set_tempo_bpm(140.0);
            Ok(())
        })
        .unwrap();
    rig.frames(2);
    rig.harness.get_by_label("Slices to Piano Roll").click();
    rig.frames(2);
    assert_eq!(rig.app().session.undo_label(), Some("Slices to Piano Roll"));
    let project = &rig.app().session.project;
    let t = project.track(track).unwrap();
    let TrackKind::Midi { clips, .. } = &t.kind else {
        panic!("a MIDI track");
    };
    assert_eq!(clips.len(), 1);
    let clip = &clips[0];
    assert_eq!(clip.length.get(), 4 * 960, "a bar");
    let notes: Vec<(i64, i64, u8)> = clip
        .notes
        .iter()
        .map(|n| (n.start.get(), n.len.get(), n.pitch.get()))
        .collect();
    let expected: Vec<(i64, i64, u8)> = (0..8).map(|k| (k * 480, 480, 60 + k as u8)).collect();
    assert_eq!(notes, expected);
    let id = clip.id;
    assert!(matches!(project.clip(id), Some((_, ClipRef::Midi(_)))));
    let _ = std::fs::remove_dir_all(wav.parent().unwrap());
}

#[test]
fn alt_drop_makes_a_slicer_track() {
    let mut rig = Rig::new("slicer_alt_drop");
    let dir = temp_dir("slicer_alt_drop");
    let wav = dir.join("Amen.wav");
    write_break(&wav);
    let before = rig.app().session.project.track_ids();
    let audio = before[2];
    // Onto an audio track's lane, Alt held: a new track all the same.
    let at = rig.lane_point(audio, 1.0);
    drop_file(&mut rig, &wav, at, Modifiers::ALT);
    rig.wait_for("the slicer track", |app| {
        app.session.project.track_ids().len() == before.len() + 1
    });
    rig.frames(3);
    let project = &rig.app().session.project;
    let track = project
        .track_ids()
        .into_iter()
        .find(|t| !before.contains(t))
        .unwrap();
    let t = project.track(track).unwrap();
    assert_eq!(t.name, "Amen");
    let TrackKind::Midi { instrument, .. } = &t.kind else {
        panic!("a MIDI track");
    };
    assert_eq!(instrument.id, "canopy.slicer");
    assert_eq!(t.instrument_file(), Some(wav.as_path()));
    assert_eq!(t.slices().len(), 8);
    let TrackKind::Audio { clips } = &project.track(audio).unwrap().kind else {
        panic!("still an audio track");
    };
    assert!(clips.is_empty(), "nothing imported as audio");
    // Selected and shown.
    assert_eq!(rig.app().timeline.selection.tracks, [track]);
    rig.harness.get_by_label_contains("Slices: 8 slices");
    rig.screenshot("alt drop");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_file_it_cant_read_leaves_the_slicer_silent_and_says_so() {
    let mut rig = Rig::new("slicer_missing");
    let track = slicer_track(&mut rig);
    rig.harness.get_by_label("Slices: No sample loaded");
    let dir = temp_dir("slicer_missing");
    for path in [dir.join("gone.wav"), dir.join("broken.wav")] {
        if path.ends_with("broken.wav") {
            std::fs::write(&path, b"RIFF....WAVE not really").unwrap();
        }
        rig.app_mut()
            .session
            .edit("Load", |p| p.set_instrument_file(track, Some(path.clone())))
            .unwrap();
        rig.wait_for("the failure", |app| {
            matches!(app.session.slicer_file(track), Some(FileState::Failed(_)))
        });
        rig.frames(2);
        rig.harness.get_by_label("Slices: Can't play this file");
        assert_eq!(peak(&play(&mut rig, track, 60)), 0.0);
    }
    rig.screenshot("missing");

    // Loading one through the slicer says why, and changes nothing.
    let file = rig
        .app()
        .session
        .project
        .track(track)
        .unwrap()
        .instrument_file()
        .map(Path::to_owned);
    rig.app_mut().session.load_slicer(
        dir.join("broken.wav"),
        Some(track),
        project::TrackPosition::END,
    );
    rig.wait_for("the error", |app| {
        app.session.message.as_ref().is_some_and(|m| m.error)
    });
    let after = rig
        .app()
        .session
        .project
        .track(track)
        .unwrap()
        .instrument_file()
        .map(Path::to_owned);
    assert_eq!(after, file);
    let _ = std::fs::remove_dir_all(dir);
}
