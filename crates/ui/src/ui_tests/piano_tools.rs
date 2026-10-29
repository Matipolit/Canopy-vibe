//! The piano roll's chord stamp and Tools menu.

use eframe::egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use project::{ClipRef, Scale, ScaleMode};

use super::Rig;

/// `(start, pitch)` of every note, sorted.
fn notes(rig: &Rig, clip: project::ClipId) -> Vec<(i64, u8)> {
    let mut notes = rig.notes(clip);
    notes.sort();
    notes
}

fn starts_on_grid(rig: &Rig, clip: project::ClipId, grid: i64) -> bool {
    let ClipRef::Midi(midi) = rig.app().session.project.clip(clip).unwrap().1 else {
        unreachable!()
    };
    midi.notes.iter().all(|n| n.start.get() % grid == 0)
}

#[test]
fn ctrl_up_and_down_move_an_octave() {
    let mut rig = Rig::new("octave");
    let track = rig.app().session.project.tracks[0].id;
    rig.double_click(rig.lane_point(track, 1.1));
    let clip = rig.app().timeline.selection.open_clip.unwrap();
    rig.frames(2);
    rig.double_click(rig.app().piano_roll.point(3840.0 + 30.0, 60));
    assert_eq!(notes(&rig, clip), [(0, 60)]);
    rig.key(Modifiers::COMMAND, Key::ArrowUp);
    assert_eq!(notes(&rig, clip), [(0, 72)]);
    rig.key(Modifiers::NONE, Key::ArrowDown);
    assert_eq!(notes(&rig, clip), [(0, 71)]);
    rig.key(Modifiers::COMMAND, Key::ArrowDown);
    assert_eq!(notes(&rig, clip), [(0, 59)]);
}

#[test]
fn a_drum_kits_pads_name_the_rows() {
    let mut rig = Rig::new("drum_rows");
    let track = rig.app().session.project.tracks[0].id;
    let drums = instruments::InstrumentRef::builtin("canopy.drums");
    rig.app_mut()
        .session
        .edit("Drums", |p| p.set_instrument(track, drums));
    rig.double_click(rig.lane_point(track, 1.1));
    let clip = rig.app().timeline.selection.open_clip.unwrap();
    rig.frames(2);
    // A kick on the kick's row.
    rig.double_click(rig.app().piano_roll.point(3840.0 + 30.0, 36));
    assert_eq!(notes(&rig, clip), [(0, 36)]);
    rig.screenshot("named");
}

#[test]
fn chords_and_tools() {
    let mut rig = Rig::new("piano_tools");
    let track = rig.app().session.project.tracks[0].id;
    rig.app_mut().session.edit("Scale", |p| {
        p.set_scale(Some(Scale::new(0, ScaleMode::Major)))
    });
    rig.double_click(rig.lane_point(track, 1.1));
    let clip = rig.app().timeline.selection.open_clip.unwrap();
    rig.frames(2);

    // The chord box: a double-click writes a triad of the scale.
    rig.harness
        .get_all_by_role(eframe::egui::accesskit::Role::ComboBox)
        .find(|n| n.value().as_deref() == Some("Note"))
        .expect("the chord box")
        .click();
    rig.frames(2);
    rig.harness.get_by_label("Triad").click();
    rig.frames(2);
    rig.double_click(rig.app().piano_roll.point(3840.0 + 30.0, 62));
    assert_eq!(
        notes(&rig, clip),
        [(0, 62), (0, 65), (0, 69)],
        "D minor, in C major"
    );
    assert_eq!(rig.app().session.undo_label(), Some("Add Chord"));

    // A beat long, the chord has room for an arpeggio of 1/16 steps.
    let ClipRef::Midi(midi) = rig.app().session.project.clip(clip).unwrap().1 else {
        unreachable!()
    };
    let mut long = midi.notes.to_vec();
    for note in &mut long {
        note.len = midi::Tick::new(960);
    }
    rig.app_mut()
        .session
        .edit("Lengthen", |p| p.set_notes(clip, long));

    // Alt+A arpeggiates the chord, previewing it before Apply.
    rig.key(Modifiers::ALT, Key::A);
    rig.frames(2);
    rig.screenshot("arpeggiate");
    let previewed = notes(&rig, clip);
    assert!(previewed.len() > 3, "previewed: {previewed:?}");
    rig.harness.get_by_label("Apply").click();
    rig.frames(2);
    assert_eq!(rig.app().session.undo_label(), Some("Arpeggiate"));
    assert_eq!(notes(&rig, clip), previewed);

    // Naturalize knocks them off the grid; Ctrl+Q puts them back.
    rig.key(Modifiers::ALT, Key::R);
    rig.frames(2);
    rig.harness.get_by_label("Apply").click();
    rig.frames(2);
    assert_eq!(rig.app().session.undo_label(), Some("Naturalize"));
    assert!(!starts_on_grid(&rig, clip, 120), "naturalized");
    rig.key(Modifiers::COMMAND, Key::Q);
    assert_eq!(rig.app().session.undo_label(), Some("Quantize"));
    assert!(starts_on_grid(&rig, clip, 120), "quantized");

    // Cancel puts back what a window previewed.
    let before = notes(&rig, clip);
    rig.key(Modifiers::ALT, Key::U);
    rig.frames(2);
    assert_ne!(notes(&rig, clip), before, "chop previews");
    rig.harness.get_by_label("Cancel").click();
    rig.frames(2);
    assert_eq!(notes(&rig, clip), before);

    // From the menu: Flip turns the line upside down.
    rig.harness.get_by_label("Tools").click();
    rig.frames(2);
    rig.harness.get_by_label("Flip Alt+Y").click();
    rig.frames(2);
    assert_eq!(rig.app().session.undo_label(), Some("Flip Notes"));

    // Fit to Scale with no scale says why it can't.
    rig.app_mut().session.edit("Scale", |p| p.set_scale(None));
    rig.harness.get_by_label("Tools").click();
    rig.frames(2);
    rig.harness.get_by_label("Fit to Scale").click();
    rig.frames(2);
    let message = rig.app().session.message.as_ref().unwrap();
    assert!(message.error && message.text.contains("scale"));
}

/// Notes sounding below the guitar's range: its key-switches.
fn switches(rig: &Rig, clip: project::ClipId) -> Vec<(i64, u8)> {
    notes(rig, clip).into_iter().filter(|n| n.1 < 24).collect()
}

/// Where each stroke starts (its notes start together).
fn stroke_starts(rig: &Rig, clip: project::ClipId) -> Vec<i64> {
    let mut starts: Vec<i64> = notes(rig, clip)
        .into_iter()
        .filter(|n| n.1 >= 24)
        .map(|n| n.0)
        .collect();
    starts.dedup();
    starts
}

#[test]
fn a_strum_pattern_strums_held_chords_in_one_undo_step() {
    use egui_kittest::kittest::NodeT;

    let mut rig = Rig::new("strum_pattern");
    let track = rig.app().session.project.tracks[0].id;
    let guitar = instruments::InstrumentRef::builtin("canopy.guitar");
    rig.app_mut()
        .session
        .edit("Guitar", |p| p.set_instrument(track, guitar));
    rig.double_click(rig.lane_point(track, 1.1));
    let clip = rig.app().timeline.selection.open_clip.unwrap();
    // Two bars: G held for one, C for the other.
    let bar = 3840;
    let held: Vec<(i64, u8)> = [43u8, 47, 50, 55, 59, 67]
        .iter()
        .map(|&p| (0, p))
        .chain([48u8, 52, 55, 60, 64].iter().map(|&p| (bar, p)))
        .collect();
    rig.app_mut().session.edit("Chords", |p| {
        p.set_clip_bounds(clip, midi::Tick::new(bar), midi::Tick::new(3 * bar))?;
        let notes: Vec<_> = held
            .iter()
            .map(|&(start, pitch)| project::Note {
                id: p.new_note_id(),
                start: midi::Tick::new(start),
                len: midi::Tick::new(bar),
                pitch: midi::Pitch::new(pitch).unwrap(),
                velocity: midi::Velocity::from_midi1(100),
                release_velocity: midi::Velocity::CENTER,
                muted: false,
            })
            .collect();
        p.set_notes(clip, notes)
    });
    rig.frames(2);
    let mut before = notes(&rig, clip);
    before.sort();

    // With nothing selected the window says what to do, and can't apply.
    // An empty row between the chords' notes.
    rig.click(rig.app().piano_roll.point(bar as f64 + 30.0, 53));
    rig.key(Modifiers::ALT, Key::P);
    rig.frames(2);
    rig.harness.get_by_label("Select held chords to strum");
    assert!(
        rig.harness
            .get_by_label("Apply")
            .accesskit_node()
            .is_disabled()
    );
    rig.screenshot("hint");

    // Picking the chords out starts the preview: Old Faithful by default.
    rig.key(Modifiers::COMMAND, Key::A);
    rig.frames(2);
    assert_eq!(
        stroke_starts(&rig, clip),
        [
            0, 960, 1440, 2400, 2880, 3360, 3840, 4800, 5280, 6240, 6720, 7200
        ]
    );
    assert!(
        rig.harness
            .query_by_label("Select held chords to strum")
            .is_none()
    );

    // A pattern that can't be played says why, and can't be applied.
    let field = || eframe::egui::accesskit::Role::TextInput;
    rig.harness
        .get_all_by_role(field())
        .find(|n| n.value().as_deref() == Some("D_du_uDu"))
        .expect("the pattern field")
        .focus();
    rig.frames(1);
    for _ in 0..10 {
        rig.key(Modifiers::NONE, Key::Backspace);
    }
    rig.harness.get_by_role(field()).type_text("D-q");
    rig.frames(2);
    rig.harness.get_by_label_contains("isn't a step");
    assert!(
        rig.harness
            .get_by_label("Apply")
            .accesskit_node()
            .is_disabled()
    );
    rig.screenshot("error");

    // A preset: the island strum, chucks on 2 and 4.
    rig.harness
        .get_all_by_role(eframe::egui::accesskit::Role::ComboBox)
        .find(|n| n.value().as_deref() == Some("Custom"))
        .expect("the preset menu")
        .click();
    rig.frames(2);
    rig.screenshot("presets");
    rig.harness.get_by_label("Funk sixteenths").click();
    rig.frames(3);
    rig.screenshot("funk");
    rig.harness
        .get_all_by_role(eframe::egui::accesskit::Role::ComboBox)
        .find(|n| n.value().as_deref() == Some("Funk sixteenths"))
        .expect("the preset menu")
        .click();
    rig.frames(2);
    rig.harness.get_by_label("Island strum").click();
    rig.frames(3);
    rig.screenshot("island");

    // Listen loops the chords' bars (the clip's two) and plays them.
    rig.harness.get_by_label("Listen").click();
    rig.frames(2);
    assert!(rig.app().session.is_playing());
    let region = rig.app().session.project.loop_region.unwrap();
    assert_eq!(
        (region.start.get(), region.end.get(), region.on),
        (bar, 3 * bar, true)
    );
    rig.harness.get_by_label("Listen").click();
    rig.frames(2);
    assert!(!rig.app().session.is_playing());

    rig.harness.get_by_label("Apply").click();
    rig.frames(2);
    assert_eq!(rig.app().session.undo_label(), Some("Strum Pattern"));
    assert_eq!(
        stroke_starts(&rig, clip),
        [
            0, 960, 1440, 2400, 2880, 3360, 3840, 4800, 5280, 6240, 6720, 7200
        ]
    );
    // The guitar's dead-note key-switch before each chuck, open after.
    assert_eq!(
        switches(&rig, clip),
        [
            (950, 15),
            (1430, 12),
            (2870, 15),
            (3350, 12),
            (4790, 15),
            (5270, 12),
            (6710, 15),
            (7190, 12)
        ]
    );

    // One undo brings the held chords back.
    rig.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(notes(&rig, clip), before);
}
