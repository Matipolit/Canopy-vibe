//! Freezing MIDI clips, muting clips, and audio clip fade and volume handles.

use eframe::egui::{Event, Key, Modifiers, PointerButton, pos2, vec2};
use egui_kittest::kittest::Queryable;
use midi::{Pitch, Tick, Velocity};
use project::{ClipRef, Note, TrackKind};

use super::{Rig, peak, temp_dir, write_sine_wav};
use crate::timeline::clip::AudioHandles;

const SAMPLES_PER_BAR: usize = 96_000;

fn right_click(rig: &mut Rig, pos: eframe::egui::Pos2) {
    rig.event(Event::PointerMoved(pos));
    for pressed in [true, false] {
        rig.event(Event::PointerButton {
            pos,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    rig.frames(2);
}

#[test]
fn freeze_a_midi_clip_to_audio() {
    let mut rig = Rig::new("freeze");
    let dir = temp_dir("freeze");
    let track = rig.app().session.project.tracks[0].id;
    let clip = rig
        .app_mut()
        .session
        .edit("Add", |p| {
            let bar = p.tempo.bar_len();
            let clip = p.add_midi_clip(track, bar, bar)?;
            let note = Note {
                id: p.new_note_id(),
                start: Tick::ZERO,
                len: Tick::new(960),
                pitch: Pitch::A4,
                velocity: Velocity::MAX,
                release_velocity: Velocity::CENTER,
                muted: false,
                bend: Vec::new(),
            };
            p.set_notes(clip, vec![note])?;
            Ok(clip)
        })
        .unwrap();
    // Saved, so the frozen audio goes next to the project, not into the
    // user's data folder.
    assert!(rig.app_mut().session.save_as(&dir.join("song")));
    rig.frames(2);

    let at = rig.lane_point(track, 1.3);
    right_click(&mut rig, at);
    rig.harness.get_by_label("Freeze to Audio").click();
    rig.frames(3);
    rig.screenshot("frozen");

    let project = &rig.app().session.project;
    let ids = project.track_ids();
    let frozen = ids[1];
    let frozen_track = project.track(frozen).unwrap();
    assert_eq!(frozen_track.name, "1 MIDI (frozen)");
    assert!(matches!(frozen_track.kind, TrackKind::Audio { .. }));
    let audio_clip = frozen_track.clips()[0];
    assert_eq!(audio_clip.start(), project.tempo.bar_len());
    assert!(
        project.clip(clip).unwrap().1.muted(),
        "the MIDI clip is muted"
    );
    let source = &project.sources[0];
    assert!(
        source.path.exists(),
        "{} was written",
        source.path.display()
    );
    assert!(source.path.starts_with(&dir), "next to the project");

    // It plays from the audio track now.
    rig.key(Modifiers::NONE, Key::Home);
    rig.key(Modifiers::NONE, Key::Space);
    let before = rig.render_audio(SAMPLES_PER_BAR);
    assert_eq!(peak(&before), 0.0);
    let frozen_audio = rig.render_audio(SAMPLES_PER_BAR / 4);
    assert!(peak(&frozen_audio) > 0.05, "the frozen audio plays");
    rig.key(Modifiers::NONE, Key::Space);

    // Freezing again reuses the frozen track.
    rig.app_mut()
        .session
        .edit("Unmute", |p| p.set_clips_muted(&[clip], false))
        .unwrap();
    rig.app_mut().session.freeze_clip(clip).unwrap();
    assert_eq!(rig.app().session.project.track_ids().len(), ids.len());

    // One undo step each.
    rig.app_mut().session.undo();
    rig.app_mut().session.undo();
    rig.app_mut().session.undo();
    let project = &rig.app().session.project;
    assert_eq!(project.track_ids().len(), 4, "the frozen track is gone");
    assert!(!project.clip(clip).unwrap().1.muted());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn zero_toggles_clip_mute() {
    let mut rig = Rig::new("mute");
    let track = rig.app().session.project.tracks[0].id;
    rig.double_click(rig.lane_point(track, 0.1));
    let clip = rig.app().session.project.track(track).unwrap().clips()[0].id();
    rig.click(pos2(700.0, 420.0));
    rig.frames(45);
    rig.click(rig.lane_point(track, 0.5));
    rig.key(Modifiers::NONE, Key::Num0);
    assert!(rig.app().session.project.clip(clip).unwrap().1.muted());
    rig.screenshot("muted");
    rig.key(Modifiers::NONE, Key::Num0);
    assert!(!rig.app().session.project.clip(clip).unwrap().1.muted());
}

#[test]
fn fade_and_volume_handles() {
    let mut rig = Rig::new("fades");
    let dir = temp_dir("fades");
    let wav = dir.join("tone.wav");
    write_sine_wav(&wav, 4, 220.0);
    let track = rig.app().session.project.track_ids()[2];
    rig.app_mut().session.import_files(
        vec![wav],
        Some(track),
        Tick::ZERO,
        project::TrackPosition::END,
    );
    rig.wait_for("the import", |app| !app.session.project.sources.is_empty());
    rig.frames(2);
    let clip = rig.app().session.project.track(track).unwrap().clips()[0].id();
    let audio = |rig: &Rig| match rig.app().session.project.clip(clip).unwrap().1 {
        ClipRef::Audio(a) => a.clone(),
        _ => unreachable!(),
    };
    let handles = |rig: &Rig| {
        let rect = rig.app().timeline.clip_rect(clip).unwrap();
        AudioHandles::new(
            rect,
            &audio(rig),
            &rig.app().session.project,
            &rig.app().timeline.viewport,
        )
        .unwrap()
    };

    // Drag the fade-in handle half a bar in (a second at 120 BPM).
    let from = handles(&rig).fade_in_handle();
    rig.drag(from, from + vec2(60.0, 0.0));
    let fade_in = audio(&rig).fade_in;
    assert!(
        (fade_in as i64 - 44_100).abs() < 500,
        "a second of fade-in: {fade_in}"
    );
    assert_eq!(rig.app().session.undo_label(), Some("Clip Fade"));

    // Bend it: drag the middle dot down.
    rig.frames(45);
    let dot = handles(&rig).fade_in_curve.expect("long enough to grab");
    rig.drag(dot, dot + vec2(0.0, 20.0));
    assert!(
        audio(&rig).fade_in_curve > 0.2,
        "bent: {}",
        audio(&rig).fade_in_curve
    );

    // Drag the volume handle down 30 px: 10 dB quieter.
    rig.frames(45);
    let volume = handles(&rig).volume;
    rig.drag(volume, volume + vec2(0.0, 30.0));
    assert!(
        (audio(&rig).gain_db + 10.0).abs() < 0.5,
        "gain {}",
        audio(&rig).gain_db
    );
    rig.screenshot("handles");

    // It sounds that way: half a bar in, still deep in the fade; later, -10 dB.
    rig.key(Modifiers::NONE, Key::Home);
    rig.key(Modifiers::NONE, Key::Space);
    let out = rig.render_audio(SAMPLES_PER_BAR * 2);
    let early = peak(&out[4800..9600]);
    let steady = peak(&out[SAMPLES_PER_BAR + 4800..SAMPLES_PER_BAR + 9600]);
    let tone = 20_000.0 / 32_768.0;
    assert!((steady - tone * 0.316).abs() < 0.02, "-10 dB: {steady}");
    assert!(early < steady * 0.2, "fading in: {early}");
    rig.key(Modifiers::NONE, Key::Space);

    // Escape mid-drag puts it back; double-click resets.
    let before = audio(&rig);
    let volume = handles(&rig).volume;
    rig.event(Event::PointerMoved(volume));
    rig.press(volume, true, Modifiers::NONE);
    for i in 1..=4 {
        rig.event(Event::PointerMoved(volume + vec2(0.0, 10.0 * i as f32)));
    }
    rig.key(Modifiers::NONE, Key::Escape);
    rig.press(volume + vec2(0.0, 40.0), false, Modifiers::NONE);
    rig.frames(2);
    assert_eq!(audio(&rig), before);
    rig.double_click(handles(&rig).fade_in_handle());
    assert_eq!(audio(&rig).fade_in, 0);
    let _ = std::fs::remove_dir_all(dir);
}
