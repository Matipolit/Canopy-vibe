//! MIDI keyboards and controllers: playing the selected track, recording,
//! linking a knob to a control, and the metronome.

use eframe::egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use engine::LiveEvent;
use midi::{NoteEventKind, Pitch, Velocity};
use project::{AutomationTarget, ClipRef};

use super::{Rig, peak};
use crate::midi_input::Control;

const RATE: usize = 48_000;

fn key(pitch: u8, on: bool) -> LiveEvent {
    let pitch = Pitch::new(pitch).unwrap();
    LiveEvent::Note(if on {
        NoteEventKind::On {
            pitch,
            velocity: Velocity::from_midi1(100),
        }
    } else {
        NoteEventKind::Off {
            pitch,
            velocity: Velocity::CENTER,
        }
    })
}

#[test]
fn a_keyboard_plays_the_selected_track_and_records_into_it() {
    let mut rig = Rig::new("midi_keys");
    let track = rig.app().session.project.tracks[0].id;
    rig.frames(2);
    assert_eq!(
        rig.app().session.live_target(),
        Some(track),
        "the selected MIDI track"
    );
    let live = rig.app().session.live_sender().unwrap();

    live.send(key(69, true));
    assert!(peak(&rig.render_audio(RATE / 10)) > 0.05, "plays");
    assert!(rig.app().session.live_keys()[69]);
    live.send(key(69, false));
    let out = rig.render_audio(RATE);
    assert!(peak(&out[RATE - 4800..]) < 1e-3, "released");
    assert!(!rig.app().session.live_keys()[69]);

    // Selecting an audio track keeps the keyboard on the MIDI track.
    let audio = rig.app().session.project.track_ids()[2];
    rig.click(rig.header_point(audio));
    assert_eq!(rig.app().session.live_target(), Some(track));

    // Record: R, play, half a second in hold C4 for half a second, stop.
    rig.key(Modifiers::NONE, Key::R);
    assert!(rig.app().session.is_recording());
    rig.key(Modifiers::NONE, Key::Space);
    rig.render_audio(RATE / 2);
    live.send(key(60, true));
    rig.render_audio(RATE / 2);
    live.send(key(60, false));
    rig.render_audio(RATE / 4);
    rig.key(Modifiers::NONE, Key::Space);
    rig.screenshot("recorded");
    assert_eq!(rig.app().session.undo_label(), Some("Record"));
    let project = &rig.app().session.project;
    let clips = project.track(track).unwrap().clips();
    assert_eq!(clips.len(), 1);
    let ClipRef::Midi(clip) = clips[0] else {
        unreachable!()
    };
    assert_eq!(
        (clip.start.get(), clip.length.get()),
        (0, 3840),
        "a whole bar"
    );
    let [note] = &clip.notes[..] else {
        panic!("{:?}", clip.notes)
    };
    assert_eq!(note.pitch.get(), 60);
    // Half a second is 960 ticks at 120 BPM; a block or two either way.
    assert!(
        (note.start.get() - 960).abs() < 20,
        "start {}",
        note.start.get()
    );
    assert!(
        (note.len.get() - 960).abs() < 20,
        "length {}",
        note.len.get()
    );

    // Recording again over it adds to the same clip.
    rig.key(Modifiers::NONE, Key::Home);
    rig.key(Modifiers::NONE, Key::Space);
    rig.render_audio(RATE / 4);
    live.send(key(64, true));
    rig.render_audio(RATE / 4);
    live.send(key(64, false));
    rig.key(Modifiers::NONE, Key::Space);
    let project = &rig.app().session.project;
    let clips = project.track(track).unwrap().clips();
    assert_eq!(clips.len(), 1);
    let ClipRef::Midi(clip) = clips[0] else {
        unreachable!()
    };
    assert_eq!(clip.notes.len(), 2);
}

#[test]
fn link_a_knob_to_a_control() {
    let mut rig = Rig::new("midi_learn");
    let track = rig.app().session.project.tracks[0].id;
    // The device view shows the first track's synth. Right-click Cutoff.
    let knob_at = rig.harness.get_by_label("Cutoff").rect().center();
    rig.right_click(knob_at);
    rig.harness.get_by_label("Link to Controller").click();
    rig.frames(2);
    assert!(rig.app().session.learning().is_some());

    let knob = |value| Control {
        channel: 0,
        controller: 74,
        value,
    };
    rig.app_mut().session.control(knob(0));
    let target = AutomationTarget::InstrumentParam("cutoff".into());
    let link = rig
        .app()
        .session
        .project
        .controller_for(Some(track), &target)
        .unwrap();
    assert_eq!(link.controller, 74);
    assert_eq!(rig.app().session.undo_label(), Some("Link Controller"));

    // Turning it all the way up opens the filter all the way.
    rig.app_mut().session.control(knob(127));
    let cutoff = |rig: &Rig| {
        rig.app()
            .session
            .project
            .track(track)
            .unwrap()
            .instrument_param("cutoff")
    };
    let max = cutoff(&rig).unwrap();
    rig.app_mut().session.control(knob(0));
    let min = cutoff(&rig).unwrap();
    assert!(max > 15_000.0 && min < 30.0, "{min}..{max}");
    // Left alone a moment, the turns become one undo step.
    std::thread::sleep(std::time::Duration::from_millis(450));
    rig.frames(1);
    assert_eq!(rig.app().session.undo_label(), Some("Controller"));

    // The menu offers to unlink it.
    rig.right_click(knob_at);
    rig.harness.get_by_label("Unlink CC 74").click();
    rig.frames(2);
    assert!(rig.app().session.project.controller_links.is_empty());
}

#[test]
fn ctrl_m_toggles_the_metronome() {
    let mut rig = Rig::new("metronome");
    rig.key(Modifiers::COMMAND, Key::M);
    assert!(rig.app().session.metronome());
    rig.key(Modifiers::NONE, Key::Space);
    let out = rig.render_audio(RATE);
    rig.key(Modifiers::NONE, Key::Space);
    assert!(peak(&out[..1000]) > 0.1, "clicks on the first beat");
    assert!(
        peak(&out[RATE / 2..RATE / 2 + 1000]) > 0.1,
        "and the second"
    );
    assert!(peak(&out[5000..20_000]) < 1e-3);
}
