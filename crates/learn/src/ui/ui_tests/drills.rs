//! Scales and chords, driven by a MIDI keyboard.

use eframe::egui::{Key, Modifiers};

use super::Rig;
use crate::drill::{Kind, Settings};
use crate::song::Hands;
use crate::theory::{ChordKind, NoteName, ScaleKind};
use crate::ui::app::View;

fn set(rig: &mut Rig, settings: Settings) {
    let app = rig.app_mut();
    app.drill.set_settings(settings, &mut app.shared);
    rig.frames(2);
}

/// The pitches the drill wants next.
fn next(rig: &Rig) -> Vec<u8> {
    rig.app()
        .drill
        .drill
        .next_notes()
        .map(|n| n.pitch())
        .collect()
}

#[test]
fn a_chord_played_together_and_apart() {
    let mut rig = Rig::new("chord");
    rig.view(View::Drill);
    set(
        &mut rig,
        Settings {
            kind: Kind::Chord,
            chord: ChordKind::Major,
            inversion: Some(0),
            ..Settings::default()
        },
    );
    assert_eq!(next(&rig), [60, 64, 67]);
    rig.screenshot("c_major");

    // Together: done, and clean.
    rig.chord(&[60, 64, 67]);
    let drill = &rig.app().drill.drill;
    assert!(drill.is_finished());
    assert_eq!(drill.sequence.accuracy(), 1.0);
    rig.screenshot("together");

    // Rolled too slowly: not together, so it's played again.
    rig.key(Modifiers::NONE, Key::Enter);
    assert!(!rig.app().drill.drill.is_finished());
    rig.note_on(60);
    rig.wait(0.3);
    rig.note_on(64);
    rig.note_on(67);
    rig.frames(2);
    assert!(!rig.app().drill.drill.is_finished());
    assert_eq!(rig.app().drill.drill.sequence.mistakes, 1);
    rig.screenshot("apart");
    for p in [60, 64, 67] {
        rig.note_off(p);
    }
    rig.frames(2);
    rig.chord(&[60, 64, 67]);
    assert!(rig.app().drill.drill.is_finished());
    assert_eq!(
        rig.app().drill.drill.sequence.accuracy(),
        0.0,
        "not first time"
    );

    // A wrong note in the chord.
    rig.key(Modifiers::NONE, Key::Enter);
    rig.chord(&[60, 63, 67]);
    assert!(!rig.app().drill.drill.is_finished());
    assert_eq!(rig.app().drill.drill.sequence.mistakes, 1);
    rig.screenshot("wrong_note");
}

#[test]
fn every_inversion_hands_together() {
    let mut rig = Rig::new("inversions");
    rig.view(View::Drill);
    set(
        &mut rig,
        Settings {
            kind: Kind::Chord,
            root: NoteName::new(3, 0),
            chord: ChordKind::Dominant7,
            inversion: None,
            hands: Hands::Both,
            ..Settings::default()
        },
    );
    rig.screenshot("f7_both_hands");
    let steps = rig.app().drill.drill.steps();
    assert_eq!(steps, 5);
    for step in 0..steps {
        let notes = next(&rig);
        assert_eq!(notes.len(), 8, "four notes in each hand");
        rig.chord(&notes);
        if step == 2 {
            rig.screenshot("halfway");
        }
    }
    assert!(rig.app().drill.drill.is_finished());
    rig.screenshot("done");
}

#[test]
fn a_scale_in_order_with_fingering() {
    let mut rig = Rig::new("scale");
    rig.view(View::Drill);
    set(
        &mut rig,
        Settings {
            root: NoteName::new(1, 0),
            ..Settings::default()
        },
    );
    rig.screenshot("d_major");
    let fingers: Vec<u8> = rig
        .app()
        .drill
        .drill
        .notes
        .iter()
        .map(|n| n.finger.unwrap())
        .collect();
    assert_eq!(&fingers[..8], [1, 2, 3, 1, 2, 3, 4, 5]);
    let steps = rig.app().drill.drill.steps();
    for step in 0..steps {
        if step == 5 {
            // Skipping ahead is a mistake; the scale waits.
            let ahead = next(&rig)[0] + 2;
            rig.tap(ahead);
            assert_eq!(rig.app().drill.drill.sequence.index(), 5);
            rig.screenshot("mistake");
        }
        let notes = next(&rig);
        rig.tap(notes[0]);
    }
    let drill = &rig.app().drill.drill;
    assert!(drill.is_finished());
    assert_eq!(drill.sequence.mistakes, 1);
    rig.screenshot("done");
    let name = rig.app().drill.name();
    assert!(rig.app().shared.progress.exercises.contains_key(&name));
}

#[test]
fn hands_together_over_two_octaves() {
    let mut rig = Rig::new("scale_two_hands");
    rig.view(View::Drill);
    set(
        &mut rig,
        Settings {
            root: NoteName::new(6, -1),
            scale: ScaleKind::HarmonicMinor,
            hands: Hands::Both,
            octaves: 2,
            ..Settings::default()
        },
    );
    rig.screenshot("b_flat_harmonic_minor");
    for _ in 0..10 {
        let notes = next(&rig);
        rig.chord(&notes);
    }
    assert_eq!(rig.app().drill.drill.sequence.index(), 10);
    rig.screenshot("scrolled");
}

#[test]
fn in_time_with_the_metronome() {
    let mut rig = Rig::new("metronome");
    rig.view(View::Drill);
    set(
        &mut rig,
        Settings {
            metronome: true,
            bpm: 120.0,
            down: false,
            ..Settings::default()
        },
    );
    // Before Start, notes aren't graded.
    rig.tap(60);
    assert_eq!(rig.app().drill.drill.sequence.index(), 0);
    rig.key(Modifiers::NONE, Key::Space);
    assert!(rig.app().drill.running());
    assert_eq!(rig.app().shared.audio.metronome(), Some(120.0));
    let start = rig.app().drill.drill.started.unwrap();
    // Count-in: four beats of half a second.
    rig.wait(1.0);
    rig.screenshot("count_in");
    let steps = rig.app().drill.drill.steps();
    for step in 0..steps {
        let due = rig.app().drill.drill.sequence.due(step).unwrap();
        assert!((due - (start + 2.0 + 0.5 * step as f64)).abs() < 0.03);
        // A little late on odd notes.
        let lateness = if step % 2 == 1 { 0.05 } else { 0.0 };
        let wait = due + lateness - rig.app().shared.now();
        rig.wait(wait.max(0.0));
        let notes = next(&rig);
        rig.note_on(notes[0]);
        rig.frames(1);
        rig.note_off(notes[0]);
    }
    rig.frames(2);
    let drill = &rig.app().drill.drill;
    assert!(drill.is_finished());
    let timing = drill.sequence.timing;
    assert_eq!(timing.count, 8);
    let mean = timing.mean_abs().unwrap();
    assert!(
        (10.0..45.0).contains(&mean),
        "about 25 ms off on average: {mean}"
    );
    rig.screenshot("timed");
    rig.wait(1.0);
    assert!(
        !rig.app().drill.running(),
        "the metronome stops after the last note"
    );
    assert_eq!(rig.app().shared.audio.metronome(), None);
}
