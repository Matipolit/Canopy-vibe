//! Note reading, driven by a MIDI keyboard.

use eframe::egui::{Key, Modifiers};

use super::Rig;
use crate::grading::Octave;
use crate::reading::{Clefs, KeyChoice, Settings, Show};
use crate::theory::KeySig;
use crate::ui::app::View;

fn set(rig: &mut Rig, settings: Settings) {
    let app = rig.app_mut();
    let now = app.shared.now();
    app.reading.set_settings(settings, now);
    rig.frames(2);
}

fn target(rig: &Rig) -> u8 {
    rig.app().reading.reading.question.pitch()
}

#[test]
fn a_round_of_note_reading() {
    let mut rig = Rig::new("reading");
    rig.view(View::Reading);
    set(
        &mut rig,
        Settings {
            round: 5,
            ..Settings::default()
        },
    );
    rig.screenshot("first_note");

    // A wrong key: counted, shown, and the note stays.
    let first = target(&rig);
    let wrong = if first > 60 { first - 1 } else { first + 1 };
    rig.tap(wrong);
    {
        let r = &rig.app().reading.reading;
        assert_eq!((r.wrong, r.correct, r.streak), (1, 0, 0));
        assert_eq!(r.question.pitch(), first);
    }
    rig.screenshot("wrong_key");
    rig.tap(first);
    assert_eq!(rig.app().reading.reading.answered, 1);
    assert_eq!(
        rig.app().reading.reading.correct,
        0,
        "not right the first time"
    );

    // Four right in a row.
    for i in 1..=4 {
        let note = target(&rig);
        rig.wait(0.5);
        rig.tap(note);
        if i < 4 {
            assert_eq!(rig.app().reading.reading.streak, i);
        }
        if i == 3 {
            rig.screenshot("streak");
        }
    }
    let r = &rig.app().reading.reading;
    assert!(r.finished);
    assert_eq!((r.correct, r.answered, r.best_streak), (4, 5, 4));
    rig.screenshot("round_done");
    let name = rig.app().reading.name();
    let best = &rig.app().shared.progress.exercises[&name].bests[""];
    assert!((best.score - 0.8).abs() < 1e-9);
    assert_eq!(best.streak, 4);

    // Notes between rounds don't count; Enter starts the next.
    rig.tap(60);
    assert_eq!(rig.app().reading.reading.wrong, 1);
    rig.key(Modifiers::NONE, Key::Enter);
    assert!(!rig.app().reading.reading.finished);
    assert_eq!(rig.app().reading.reading.answered, 0);
}

#[test]
fn key_names_any_octave() {
    let mut rig = Rig::new("key_names");
    rig.view(View::Reading);
    set(
        &mut rig,
        Settings {
            show: Show::Name,
            octave: Octave::Any,
            accidentals: true,
            ..Settings::default()
        },
    );
    // Find a note with an accidental for the picture.
    for _ in 0..30 {
        if rig.app().reading.reading.question.note.accidental != 0 {
            break;
        }
        let now = rig.app().shared.now();
        rig.app_mut().reading.reading.skip(now);
    }
    rig.frames(2);
    rig.screenshot("name");
    let note = target(&rig);
    // Any octave will do.
    let other = if note >= 72 { note - 12 } else { note + 12 };
    rig.tap(other);
    assert_eq!(rig.app().reading.reading.correct, 1);
}

#[test]
fn clefs_ledger_lines_and_key_signatures() {
    let mut rig = Rig::new("clefs");
    rig.view(View::Reading);
    set(
        &mut rig,
        Settings {
            clefs: Clefs::Bass,
            ledger: 3,
            accidentals: true,
            key: KeyChoice::Fixed(KeySig(-4)),
            ..Settings::default()
        },
    );
    for _ in 0..5 {
        let note = target(&rig);
        rig.tap(note);
    }
    rig.screenshot("bass_a_flat_major");
    // Ab major: every B, E, A and D is flat unless an accidental says otherwise.
    let q = rig.app().reading.reading.question;
    assert_eq!(q.clef, crate::notation::Clef::Bass);

    set(
        &mut rig,
        Settings {
            clefs: Clefs::Treble,
            ledger: 2,
            key: KeyChoice::Fixed(KeySig(5)),
            ..Settings::default()
        },
    );
    for _ in 0..6 {
        let note = target(&rig);
        rig.tap(note);
    }
    rig.screenshot("treble_b_major");
    let wrong = target(&rig) + 2;
    rig.tap(wrong);
    rig.screenshot("treble_wrong");
}
