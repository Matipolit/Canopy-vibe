//! The keyboard and devices: what's held shows on the staff and the keys,
//! the on-screen and typing keyboards play, the instrument and the menus.

use eframe::egui::{Event, Key, Modifiers};
use egui_kittest::kittest::Queryable;

use super::{Rig, peak};
use crate::ui::app::View;

#[test]
fn held_notes_show_on_the_staff_and_sound() {
    let mut rig = Rig::new("keyboard");
    rig.screenshot("start");
    // A MIDI keyboard holds a C major 7th over a low C.
    for p in [48, 64, 67, 71] {
        rig.note_on(p);
    }
    rig.frames(4);
    assert_eq!(rig.app().shared.held_keys(), [48, 64, 67, 71]);
    assert!(peak(&rig.take_audio()) > 0.01, "the keys sound");
    rig.screenshot("held_chord");
    // The pedal shows too.
    rig.app().shared.devices.simulate(&[0xB0, 64, 127]);
    rig.frames(2);
    assert!(rig.app().shared.pedal);
    for p in [48, 64, 67, 71] {
        rig.note_off(p);
    }
    rig.app().shared.devices.simulate(&[0xB0, 64, 0]);
    rig.frames(2);
    assert!(rig.app().shared.held_keys().is_empty());
    assert!(!rig.app().shared.pedal);

    // Spelled in another key: a flat key writes B-flat, not A-sharp.
    rig.app_mut().free.key = crate::theory::KeySig(-2);
    for p in [58, 62, 65] {
        rig.note_on(p);
    }
    rig.frames(3);
    rig.screenshot("b_flat_major");
}

#[test]
fn clicking_and_typing_play_notes() {
    let mut rig = Rig::new("playing");
    let middle_c = rig.piano_key(60);
    rig.event(Event::PointerMoved(middle_c));
    rig.press(middle_c, true);
    rig.frames(2);
    assert!(rig.app().shared.held[60], "clicking plays");
    rig.take_audio();
    rig.frames(6);
    assert!(peak(&rig.take_audio()) > 0.01);
    rig.screenshot("clicked");
    // Sliding to the next key plays that one instead.
    let d = rig.piano_key(62);
    rig.event(Event::PointerMoved(d));
    rig.frames(2);
    assert!(!rig.app().shared.held[60] && rig.app().shared.held[62]);
    rig.press(d, false);
    rig.frames(2);
    assert!(!rig.app().shared.held[62]);

    // Z is C3 on the typing keyboard; = moves it up an octave.
    rig.harness.key_down(Key::Z);
    rig.frames(2);
    assert!(rig.app().shared.held[48]);
    rig.harness.key_up(Key::Z);
    rig.frames(2);
    assert!(!rig.app().shared.held[48]);
    rig.key(Modifiers::NONE, Key::Equals);
    rig.harness.key_down(Key::Q);
    rig.frames(2);
    assert!(rig.app().shared.held[72], "Q is C5 now");
    rig.harness.key_up(Key::Q);
    rig.frames(2);
    // Typing off: letters are just letters.
    rig.app_mut().typing_on = false;
    rig.harness.key_down(Key::X);
    rig.frames(2);
    assert!(rig.app().shared.held_keys().is_empty());
    rig.harness.key_up(Key::X);
}

#[test]
fn instruments_menus_and_the_shortcuts_window() {
    let mut rig = Rig::new("menus");
    assert_eq!(
        rig.app().shared.audio.instrument(),
        instruments::DEFAULT_INSTRUMENT
    );
    rig.harness.get_by_value("Poly Synth").click();
    rig.frames(2);
    rig.screenshot("instruments");
    rig.harness.get_by_label("Wavetable").click();
    rig.frames(2);
    assert_eq!(rig.app().shared.audio.instrument(), "canopy.wavetable");
    rig.note_on(69);
    rig.frames(6);
    assert!(peak(&rig.take_audio()) > 0.01, "the new instrument plays");
    rig.note_off(69);

    rig.harness.get_by_label("MIDI").click();
    rig.frames(2);
    assert!(rig.harness.query_by_label("No MIDI devices").is_some());
    rig.screenshot("midi_menu");
    rig.key(Modifiers::NONE, Key::Escape);

    rig.key(Modifiers::NONE, Key::F1);
    assert!(rig.app().help_open);
    rig.screenshot("shortcuts");
    rig.key(Modifiers::NONE, Key::F1);
    assert!(!rig.app().help_open);

    // Ctrl+number switches views.
    rig.key(Modifiers::COMMAND, Key::Num4);
    assert_eq!(rig.app().view, View::Songs);
    rig.key(Modifiers::COMMAND, Key::Num2);
    assert_eq!(rig.app().view, View::Reading);
}

#[test]
fn a_narrow_window_shows_fewer_keys() {
    let mut rig = Rig::with_size("narrow", eframe::egui::vec2(720.0, 560.0));
    let layout = rig.app().layout.unwrap();
    assert_eq!((layout.low, layout.high), (28, 103), "76 keys fit");
    rig.view(View::Drill);
    rig.screenshot("drill");
    rig.view(View::Songs);
    rig.screenshot("songs");
}
