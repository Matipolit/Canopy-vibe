//! Tracks left alone are rendered in the background and played from the
//! render (`autocache.rs`); an edit hands them back to their own chain.

use std::time::Duration;

use egui_kittest::kittest::Queryable;
use midi::{Pitch, Tick, Velocity};
use project::{EffectRef, Note, TrackKind};

use super::Rig;

#[test]
fn an_idle_track_is_cached_and_an_edit_takes_it_back() {
    let mut rig = Rig::new("autocache");
    let session = &mut rig.app_mut().session;
    let track = session
        .project
        .track_ids()
        .into_iter()
        .find(|&id| {
            matches!(
                session.project.track(id).map(|t| &t.kind),
                Some(TrackKind::Midi { .. })
            )
        })
        .unwrap();
    session
        .edit("Notes", |p| {
            let bar = p.tempo.bar_len();
            let clip = p.add_midi_clip(track, Tick::ZERO, bar)?;
            let notes: Vec<Note> = (0..4)
                .map(|n| Note {
                    id: p.new_note_id(),
                    start: Tick::new(n * 960),
                    len: Tick::new(480),
                    pitch: Pitch::new(60 + n as u8).unwrap(),
                    velocity: Velocity::MAX,
                    release_velocity: Velocity::CENTER,
                    muted: false,
                    bend: Vec::new(),
                })
                .collect();
            p.set_notes(clip, notes)
        })
        .unwrap();
    session.autocache.enabled = true;
    session.autocache.idle = Duration::ZERO;
    session.autocache.min_cost = 0.0;
    rig.wait_for("the render", |app| app.session.autocache.is_cached(track));
    rig.frames(2);
    assert!(
        rig.harness.query_by_label_contains("· cached").is_some(),
        "the header says so"
    );
    rig.screenshot("cached");

    // Any change to what it plays takes the render away at once.
    rig.app_mut()
        .session
        .edit("Add EQ", |p| {
            p.add_device(Some(track), EffectRef::builtin("canopy.eq"), None)
        })
        .unwrap();
    rig.frames(1);
    assert!(!rig.app().session.autocache.is_cached(track));
    assert!(rig.harness.query_by_label_contains("· cached").is_none());

    // Undo brings back the very track the render was made from.
    rig.app_mut().session.undo();
    rig.frames(1);
    assert!(rig.app().session.autocache.is_cached(track));

    // And changing only its fader leaves the render playing.
    rig.app_mut()
        .session
        .edit("Volume", |p| {
            let mixer = project::Mixer {
                volume_db: -6.0,
                ..p.track(track).unwrap().mixer
            };
            p.set_mixer(track, mixer)
        })
        .unwrap();
    rig.frames(1);
    assert!(rig.app().session.autocache.is_cached(track));
}

#[test]
fn switching_caching_off_drops_every_render() {
    let mut rig = Rig::new("autocache_off");
    let track = rig.app().session.project.track_ids()[0];
    let session = &mut rig.app_mut().session;
    session
        .edit("Notes", |p| {
            let bar = p.tempo.bar_len();
            let clip = p.add_midi_clip(track, Tick::ZERO, bar)?;
            let note = Note {
                id: p.new_note_id(),
                start: Tick::ZERO,
                len: Tick::new(480),
                pitch: Pitch::A4,
                velocity: Velocity::MAX,
                release_velocity: Velocity::CENTER,
                muted: false,
                bend: Vec::new(),
            };
            p.set_notes(clip, vec![note])
        })
        .unwrap();
    session.autocache.enabled = true;
    session.autocache.idle = Duration::ZERO;
    session.autocache.min_cost = 0.0;
    rig.wait_for("the render", |app| app.session.autocache.is_cached(track));
    rig.app_mut().session.autocache.enabled = false;
    rig.frames(1);
    assert!(!rig.app().session.autocache.is_cached(track));
    assert_eq!(rig.app().session.autocache.bytes(), 0);
}
