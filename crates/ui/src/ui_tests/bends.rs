//! Per-note bends drawn in the piano roll's Bend mode.

use eframe::egui::{Key, Modifiers};
use project::{ClipId, ClipRef};

use super::Rig;

/// A clip with one note, C4 at the clip's start, open in the piano roll.
fn one_note(rig: &mut Rig) -> ClipId {
    let track = rig.app().session.project.tracks[0].id;
    rig.double_click(rig.lane_point(track, 1.1));
    let clip = rig.app().timeline.selection.open_clip.unwrap();
    rig.frames(2);
    rig.double_click(rig.app().piano_roll.point(3840.0 + 30.0, 60));
    assert_eq!(rig.notes(clip), [(0, 60)]);
    clip
}

/// The note's length and its bend as `(tick, semitones)`.
fn bend(rig: &Rig, clip: ClipId) -> (i64, Vec<(i64, f32)>) {
    let ClipRef::Midi(midi) = rig.app().session.project.clip(clip).unwrap().1 else {
        unreachable!()
    };
    let note = &midi.notes[0];
    let points = note.bend.iter().map(|p| (p.at.get(), p.semitones)).collect();
    (note.len.get(), points)
}

#[test]
fn bend_mode_toggles_with_b() {
    let mut rig = Rig::new("bend_mode");
    one_note(&mut rig);
    assert!(!rig.app().piano_roll.bend_mode);
    rig.key(Modifiers::NONE, Key::B);
    assert!(rig.app().piano_roll.bend_mode);
    rig.key(Modifiers::NONE, Key::B);
    assert!(!rig.app().piano_roll.bend_mode);
}

#[test]
fn draw_a_bend_on_a_note() {
    let mut rig = Rig::new("draw_bend");
    let clip = one_note(&mut rig);
    rig.key(Modifiers::NONE, Key::B);
    let (len, _) = bend(&rig, clip);
    // From a quarter of the way in, up two semitones by its end.
    let from = rig.app().piano_roll.point(3840.0 + len as f64 * 0.25, 60);
    let to = rig.app().piano_roll.point(3840.0 + len as f64 * 0.95, 62);
    rig.drag(from, to);
    rig.screenshot("drawn");
    let (_, points) = bend(&rig, clip);
    assert!(points.len() >= 2, "{points:?}");
    assert_eq!(points[0].1, 0.0, "it starts from the note's own pitch");
    assert!(points[0].0 > 0, "and stays there until the drag began");
    assert_eq!(points.last().unwrap().1, 2.0, "snapped to a whole semitone");
    assert!(points.windows(2).all(|w| w[0].0 <= w[1].0));
    assert_eq!(rig.app().session.undo_label(), Some("Bend Note"));
    // Still one note: the drag bent it rather than moving it.
    assert_eq!(rig.notes(clip), [(0, 60)]);
}

#[test]
fn clear_a_notes_bend() {
    let mut rig = Rig::new("clear_bend");
    let clip = one_note(&mut rig);
    rig.key(Modifiers::NONE, Key::B);
    let (len, _) = bend(&rig, clip);
    let from = rig.app().piano_roll.point(3840.0 + len as f64 * 0.25, 60);
    let to = rig.app().piano_roll.point(3840.0 + len as f64 * 0.9, 58);
    rig.drag(from, to);
    assert!(!bend(&rig, clip).1.is_empty());
    // In Bend mode a right-click clears the note's bend, and keeps the note.
    rig.right_click(rig.app().piano_roll.point(3840.0 + len as f64 * 0.1, 60));
    assert!(bend(&rig, clip).1.is_empty());
    assert_eq!(rig.notes(clip), [(0, 60)]);
    assert_eq!(rig.app().session.undo_label(), Some("Clear Bend"));
}
