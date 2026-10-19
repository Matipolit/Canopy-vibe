//! Reversing, stretching, slipping, looping, chopping and consolidating audio
//! clips, through the timeline and the Clip tab.

use eframe::egui::{Modifiers, Pos2, pos2};
use egui_kittest::kittest::Queryable;
use midi::Tick;
use project::{AudioClip, ClipId, ClipRef, StretchMode, TrackId};

use super::{Rig, peak, temp_dir, write_sine_wav};

const RATE: usize = 48_000;

/// A 4 s, 220 Hz tone on the first audio track, at the start: two bars.
fn tone(rig: &mut Rig, name: &str) -> (TrackId, ClipId, std::path::PathBuf) {
    let dir = temp_dir(name);
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
    (track, clip, dir)
}

fn audio(rig: &Rig, clip: ClipId) -> AudioClip {
    match rig.app().session.project.clip(clip).unwrap().1 {
        ClipRef::Audio(a) => a.clone(),
        _ => unreachable!(),
    }
}

fn clip_span(rig: &Rig, clip: ClipId) -> (i64, i64) {
    let project = &rig.app().session.project;
    let c = project.clip(clip).unwrap().1;
    (c.start().get(), project.clip_end(c).get())
}

/// Just inside a clip's right edge, halfway down.
fn right_edge(rig: &Rig, clip: ClipId) -> Pos2 {
    let rect = rig.app().timeline.clip_rect(clip).unwrap();
    pos2(rect.right() - 2.0, rect.center().y)
}

fn left_edge(rig: &Rig, clip: ClipId) -> Pos2 {
    let rect = rig.app().timeline.clip_rect(clip).unwrap();
    pos2(rect.left() + 2.0, rect.center().y)
}

/// Plays `seconds` from the start, once `clip` is rendered.
fn play(rig: &mut Rig, clip: ClipId, seconds: f32) -> Vec<f32> {
    // A frame first, for the last edit to ask for its render.
    rig.frames(1);
    rig.wait_for("the render", |app| !app.session.is_rendering(clip));
    rig.key(Modifiers::NONE, eframe::egui::Key::Home);
    rig.key(Modifiers::NONE, eframe::egui::Key::Space);
    let out = rig.render_audio((seconds * RATE as f32) as usize);
    rig.key(Modifiers::NONE, eframe::egui::Key::Space);
    out
}

/// Estimates the pitch from zero crossings over `from..to` seconds.
fn pitch(rig: &mut Rig, clip: ClipId, from: f32, to: f32) -> f32 {
    let out = play(rig, clip, to);
    let window = &out[(from * RATE as f32) as usize..];
    let crossings = window
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    crossings as f32 / 2.0 / (to - from)
}

#[test]
fn shift_dragging_an_edge_resamples_like_fl_studio() {
    let mut rig = Rig::new("resample");
    let (_, clip, dir) = tone(&mut rig, "resample");
    assert!((pitch(&mut rig, clip, 0.5, 1.5) - 220.0).abs() < 3.0);

    // Shift-drag the right edge to the middle: half as long, an octave up.
    let (start, end) = clip_span(&rig, clip);
    let edge = right_edge(&rig, clip);
    let middle = rig.app().timeline.viewport.x((start + end) as f64 / 2.0);
    rig.drag_with(edge, pos2(middle + 2.0, edge.y), Modifiers::SHIFT);
    rig.screenshot("stretched");
    assert_eq!(rig.app().session.undo_label(), Some("Stretch Clip"));
    let speed = audio(&rig, clip).stretch.speed;
    assert!((speed - 2.0).abs() < 0.05, "speed {speed}");
    let (_, new_end) = clip_span(&rig, clip);
    assert!(
        ((new_end - start) - (end - start) / 2).abs() < 60,
        "half as long"
    );
    let heard = pitch(&mut rig, clip, 0.25, 1.75);
    assert!((heard - 440.0).abs() < 8.0, "an octave up: {heard} Hz");
    let out = play(&mut rig, clip, 2.5);
    assert_eq!(
        peak(&out[RATE * 2 + 100..]),
        0.0,
        "and over after two seconds"
    );

    // In Stretch mode the same speed keeps the pitch.
    rig.app_mut().session.edit("Mode", |p| {
        p.edit_audio_clip(clip, |c| c.stretch.mode = StretchMode::Stretch)
    });
    let heard = pitch(&mut rig, clip, 0.25, 1.75);
    assert!((heard - 220.0).abs() < 8.0, "same pitch: {heard} Hz");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn reverse_loop_and_chop_from_the_menu() {
    let mut rig = Rig::new("audio_menu");
    let (track, clip, dir) = tone(&mut rig, "audio_menu");

    rig.right_click(rig.lane_point(track, 0.5));
    rig.harness.get_by_label("Reverse").click();
    rig.frames(2);
    assert!(audio(&rig, clip).reversed);
    assert_eq!(rig.app().session.undo_label(), Some("Reverse"));
    let heard = pitch(&mut rig, clip, 0.5, 1.5);
    assert!((heard - 220.0).abs() < 3.0, "still a 220 Hz tone: {heard}");

    // Dragging past the end of the file turns looping on.
    rig.frames(45);
    let edge = right_edge(&rig, clip);
    let four_bars = rig.app().timeline.viewport.x(4.0 * 3840.0);
    rig.drag(edge, pos2(four_bars + 2.0, edge.y));
    rig.screenshot("looped");
    let looped = audio(&rig, clip);
    assert!(looped.looped);
    assert_eq!(clip_span(&rig, clip).1, 4 * 3840);
    rig.wait_for("the render", |app| !app.session.is_rendering(clip));
    let out = play(&mut rig, clip, 7.0);
    assert!(peak(&out[RATE * 6..]) > 0.3, "the second pass plays");

    // Chop it to the grid: one piece per grid line.
    rig.frames(45);
    rig.right_click(rig.lane_point(track, 0.5));
    rig.screenshot("menu");
    // Submenu buttons end in an arrow.
    rig.harness.get_by_label_contains("Chop").click();
    rig.frames(2);
    rig.harness.get_by_label("To Grid").click();
    rig.frames(2);
    let step = rig.app().timeline.grid_step(&rig.app().session);
    let pieces = rig
        .app()
        .session
        .project
        .track(track)
        .unwrap()
        .clips()
        .len();
    assert_eq!(pieces as i64, 4 * 3840 / step.get(), "grid step {step:?}");
    rig.screenshot("chopped");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn alt_drag_slips_the_audio_inside_the_clip() {
    let mut rig = Rig::new("slip");
    let (_, clip, dir) = tone(&mut rig, "slip");
    // Trim the start by a bar, so there's audio before the clip to slip to.
    let edge = left_edge(&rig, clip);
    let bar = rig.app().timeline.viewport.x(3840.0);
    rig.drag(edge, pos2(bar - 2.0, edge.y));
    let trimmed = audio(&rig, clip);
    assert!(
        (trimmed.source_offset as i64 - 88_200).abs() < 200,
        "{}",
        trimmed.source_offset
    );

    // Alt-drag right by half a bar: the audio moves later, the clip stays.
    rig.frames(45);
    let rect = rig.app().timeline.clip_rect(clip).unwrap();
    let from = rect.center();
    let half_bar = rig.app().timeline.viewport.width_of(1920.0);
    rig.drag_with(
        from,
        from + eframe::egui::vec2(half_bar, 0.0),
        Modifiers::ALT,
    );
    let slipped = audio(&rig, clip);
    assert_eq!(slipped.start, trimmed.start, "the clip didn't move");
    assert!(
        (slipped.source_offset as i64 - 44_100).abs() < 400,
        "a second earlier in the file: {}",
        slipped.source_offset
    );
    assert_eq!(rig.app().session.undo_label(), Some("Slip Audio"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_clip_tab_and_consolidate() {
    let mut rig = Rig::new("clip_tab");
    let (track, clip, dir) = tone(&mut rig, "clip_tab");
    // Saved, so consolidated audio goes next to the project.
    assert!(rig.app_mut().session.save_as(&dir.join("song")));

    // Double-clicking an audio clip opens it in the Clip tab.
    rig.double_click(rig.lane_point(track, 0.5));
    rig.screenshot("clip_tab");
    assert!(!rig.app().piano_roll_shown());
    rig.harness.get_by_label("Normalize").click();
    rig.frames(2);
    let gain = audio(&rig, clip).gain_db;
    assert!(
        (gain - 4.3).abs() < 0.2,
        "20000/32768 normalized: {gain} dB"
    );

    rig.harness.get_by_label("Ø").click();
    rig.frames(2);
    assert!(audio(&rig, clip).polarity_inverted);

    // Split it and glue it back together, gap and all.
    rig.app_mut()
        .session
        .edit("Split", |p| p.split_clips(&[clip], Tick::new(3840)));
    let pieces: Vec<ClipId> = rig
        .app()
        .session
        .project
        .track(track)
        .unwrap()
        .clips()
        .iter()
        .map(|c| c.id())
        .collect();
    rig.app_mut().session.edit("Move", |p| {
        p.move_clips(&[project::ClipMove {
            clip: pieces[1],
            track,
            start: Tick::new(3 * 3840),
        }])
    });
    rig.app_mut().timeline.selection.clips = pieces.clone();
    rig.right_click(rig.lane_point(track, 0.5));
    // The Clip tab has the same button; either glues the selection.
    rig.harness
        .get_all_by_label("Consolidate")
        .next()
        .unwrap()
        .click();
    rig.frames(3);
    let clips = rig.app().session.project.track(track).unwrap().clips();
    assert_eq!(clips.len(), 1);
    let glued = clips[0].id();
    assert_eq!(clip_span(&rig, glued), (0, 4 * 3840));
    let source = rig.app().session.project.sources.last().unwrap().clone();
    assert!(source.path.starts_with(&dir) && source.path.exists());
    rig.key(Modifiers::NONE, eframe::egui::Key::Home);
    rig.key(Modifiers::NONE, eframe::egui::Key::Space);
    let out = rig.render_audio(RATE * 8);
    rig.key(Modifiers::NONE, eframe::egui::Key::Space);
    assert!(peak(&out[..RATE * 2]) > 0.9, "normalized");
    assert_eq!(peak(&out[RATE * 2 + 2000..RATE * 6 - 2000]), 0.0, "the gap");
    assert!(peak(&out[RATE * 6..]) > 0.9);
    let _ = std::fs::remove_dir_all(dir);
}
