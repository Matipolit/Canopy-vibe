//! Duplicating, copying, cutting and pasting whole tracks.

use eframe::egui::{Event, Key, Modifiers};
use egui_kittest::kittest::Queryable;
use instruments::InstrumentRef;
use midi::{Pitch, Tick, Velocity};
use project::{Mixer, Note, TrackId, TrackKind};

use super::Rig;

/// The first track playing `instrument` with one setting moved, and a
/// clip holding one long note.
fn instrument_track(rig: &mut Rig, instrument: &str, param: &str, value: f32) -> TrackId {
    let track = rig.app().session.project.track_ids()[0];
    rig.app_mut().session.edit("Setup", |p| {
        p.set_instrument(track, InstrumentRef::builtin(instrument))?;
        p.set_instrument_param(track, param, value)?;
        let clip = p.add_midi_clip(track, Tick::ZERO, Tick::new(3840))?;
        let note = Note {
            id: p.new_note_id(),
            start: Tick::ZERO,
            len: Tick::new(3840),
            pitch: Pitch::new(57).unwrap(),
            velocity: Velocity::MAX,
            release_velocity: Velocity::CENTER,
            muted: false,
            bend: Vec::new(),
        };
        p.set_notes(clip, vec![note])
    });
    rig.frames(2);
    track
}

fn wavetable_track(rig: &mut Rig) -> TrackId {
    instrument_track(rig, "canopy.wavetable", "filter_cutoff", 0.3)
}

fn instrument(rig: &Rig, track: TrackId) -> (String, Vec<(String, f32)>) {
    match &rig.app().session.project.track(track).unwrap().kind {
        TrackKind::Midi {
            instrument, params, ..
        } => (
            instrument.id.clone(),
            params.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        ),
        _ => panic!("not a MIDI track"),
    }
}

/// The level (RMS) of half a second from the start, left and right, with
/// `left` panned hard left, `right` hard right and the rest muted. Both
/// play at once, so neither hears the other's tail.
fn levels_apart(rig: &mut Rig, left: TrackId, right: TrackId) -> (f32, f32) {
    let ids = rig.app().session.project.track_ids();
    rig.app_mut().session.edit("Pan", |p| {
        for id in &ids {
            let mixer = Mixer {
                mute: *id != left && *id != right,
                pan: if *id == left { -1.0 } else { 1.0 },
                ..p.track(*id).unwrap().mixer
            };
            p.set_mixer(*id, mixer)?;
        }
        Ok(())
    });
    rig.frames(2);
    rig.key(Modifiers::NONE, Key::Home);
    rig.key(Modifiers::NONE, Key::Space);
    let mut out = vec![0.0; 24_000 * 2];
    for chunk in out.chunks_mut(512 * 2) {
        rig.engine.process_interleaved(chunk, 2);
    }
    rig.key(Modifiers::NONE, Key::Space);
    let rms = |channel: usize| {
        let sum: f32 = out.chunks(2).map(|f| f[channel] * f[channel]).sum();
        (sum / 24_000.0).sqrt()
    };
    (rms(0), rms(1))
}

#[test]
fn the_header_menu_duplicates_an_instrument_track_that_sounds_the_same() {
    let mut rig = Rig::new("duplicate_track");
    // The Poly Synth: the Wavetable starts its voices at random phases, so
    // even two tracks set up alike by hand differ in level.
    let track = instrument_track(&mut rig, "canopy.poly_synth", "cutoff", 800.0);
    rig.right_click(rig.header_point(track));
    rig.screenshot("menu");
    rig.harness.get_by_label_contains("Duplicate").click();
    rig.frames(2);

    let session = &rig.app().session;
    assert_eq!(session.undo_label(), Some("Duplicate Track"));
    let copy = session.project.track_ids()[1];
    let (name, copy_name) = (
        &session.project.track(track).unwrap().name,
        &session.project.track(copy).unwrap().name,
    );
    assert_eq!(*copy_name, format!("{name} 2"));
    assert_eq!(rig.app().timeline.selection.tracks, [copy]);
    assert_eq!(instrument(&rig, copy), instrument(&rig, track));
    assert_eq!(
        rig.app().session.project.track(copy).unwrap().clips().len(),
        1
    );

    let (original, duplicate) = levels_apart(&mut rig, track, copy);
    assert!(original > 0.01, "the original plays: {original}");
    assert!(
        (duplicate - original).abs() < 0.02 * original,
        "{duplicate} against {original}"
    );
}

#[test]
fn ctrl_c_and_ctrl_v_take_tracks_or_clips_by_whats_selected() {
    let mut rig = Rig::new("copy_tracks");
    let track = wavetable_track(&mut rig);
    let count = |rig: &Rig| rig.app().session.project.track_ids().len();
    let before = count(&rig);

    // A header click selects the track and no clips: tracks are copied.
    rig.click(rig.header_point(track));
    rig.event(Event::Copy);
    rig.event(Event::Paste("canopy".into()));
    rig.frames(2);
    assert_eq!(count(&rig), before + 1);
    assert_eq!(rig.app().session.undo_label(), Some("Paste Track"));
    let pasted = rig.app().session.project.track_ids()[1];
    assert_eq!(instrument(&rig, pasted), instrument(&rig, track));
    assert_eq!(rig.app().timeline.selection.tracks, [pasted]);

    // With a clip selected, Ctrl+C takes the clip and Ctrl+V pastes a clip.
    rig.frames(45);
    rig.click(rig.lane_point(track, 0.5));
    assert_eq!(rig.app().timeline.selection.clips.len(), 1);
    rig.event(Event::Copy);
    rig.event(Event::Paste("canopy".into()));
    rig.frames(2);
    assert_eq!(count(&rig), before + 1, "no new track");
    assert_eq!(rig.app().session.undo_label(), Some("Paste"));
}

#[test]
fn cutting_a_track_and_undoing_brings_it_back() {
    let mut rig = Rig::new("cut_track");
    let track = wavetable_track(&mut rig);
    rig.click(rig.header_point(track));
    rig.event(Event::Cut);
    rig.frames(2);
    assert!(rig.app().session.project.track(track).is_none());
    assert_eq!(rig.app().session.undo_label(), Some("Cut Track"));

    // Pasted elsewhere, it's whole again, under new ids.
    let last = *rig.app().session.project.track_ids().last().unwrap();
    rig.click(rig.header_point(last));
    rig.event(Event::Paste("canopy".into()));
    rig.frames(2);
    let pasted = *rig.app().session.project.track_ids().last().unwrap();
    assert_ne!(pasted, last);
    assert_eq!(instrument(&rig, pasted).0, "canopy.wavetable");

    rig.key(Modifiers::COMMAND, Key::Z);
    rig.key(Modifiers::COMMAND, Key::Z);
    assert!(rig.app().session.project.track(track).is_some());
}

#[test]
fn ctrl_d_on_selected_tracks_duplicates_them() {
    let mut rig = Rig::new("ctrl_d_tracks");
    let tracks = rig.app().session.project.track_ids();
    rig.click(rig.header_point(tracks[2]));
    rig.click_with(rig.header_point(tracks[3]), Modifiers::COMMAND);
    rig.key(Modifiers::COMMAND, Key::D);
    let now = rig.app().session.project.track_ids();
    assert_eq!(now.len(), tracks.len() + 2);
    assert_eq!(rig.app().session.undo_label(), Some("Duplicate Tracks"));
    assert_eq!(rig.app().timeline.selection.tracks, [now[3], now[5]]);
}
