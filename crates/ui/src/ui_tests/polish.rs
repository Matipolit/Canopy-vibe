//! The UI polish pass: looping from the piano roll, zoom to fit, eased
//! zooming, views that are remembered, a window that goes idle, and knobs
//! turned from the keyboard.

use eframe::egui::{Event, Key, Modifiers, vec2};
use egui_kittest::kittest::Queryable;
use midi::Tick;
use project::{AutomationTarget, LoopRegion};

use super::{Rig, SAMPLES_PER_BAR, peak};

const BAR: i64 = 3840;

/// A one-bar MIDI clip at bar 2, open in the piano roll, with a note at its
/// start. Returns the clip.
fn open_clip_with_a_note(rig: &mut Rig) -> project::ClipId {
    let track = rig.app().session.project.tracks[0].id;
    rig.double_click(rig.lane_point(track, 1.1));
    let clip = rig.app().timeline.selection.open_clip.expect("opened");
    rig.frames(2);
    rig.double_click(rig.app().piano_roll.point(BAR as f64 + 30.0, 60));
    assert_eq!(rig.notes(clip), [(0, 60)]);
    clip
}

/// Lets the engine take what it's been sent (a seek), as the audio thread
/// would, and the app hear where the playhead is.
fn sync(rig: &mut Rig) {
    rig.render_audio(256);
}

fn region(rig: &Rig) -> Option<LoopRegion> {
    rig.app().session.project.loop_region
}

#[test]
fn the_piano_rolls_ruler_sets_and_toggles_the_loop_and_playback_goes_round_it() {
    let mut rig = Rig::new("piano_loop");
    let clip = open_clip_with_a_note(&mut rig);
    rig.key(Modifiers::NONE, Key::Home);
    sync(&mut rig);
    assert_eq!(rig.app().session.playhead(), 0.0);

    // Drag along the top of the piano roll's ruler: the first half of the
    // clip loops, and the playhead goes to it to be heard.
    let from = rig.app().piano_roll.ruler_point(BAR as f64, true);
    let to = rig.app().piano_roll.ruler_point(1.5 * BAR as f64, true);
    rig.drag(from, to);
    sync(&mut rig);
    let looped = LoopRegion {
        start: Tick::new(BAR),
        end: Tick::new(BAR + BAR / 2),
        on: true,
    };
    assert_eq!(region(&rig), Some(looped));
    assert_eq!(rig.app().session.playhead(), BAR as f64, "into the loop");
    rig.screenshot("set");

    // The arrangement's ruler shows the same loop, at the clip's place in
    // the song: it's the project's one loop.
    let over_clip = rig.app().timeline.ruler_point(1.25 * BAR as f64, true);
    assert!(
        rig.app()
            .timeline
            .clip_rect(clip)
            .unwrap()
            .x_range()
            .contains(over_clip.x)
    );

    // L turns it off and on again while the piano roll has the keys; so
    // does a click on the brace.
    rig.key(Modifiers::NONE, Key::L);
    assert_eq!(
        region(&rig),
        Some(LoopRegion {
            on: false,
            ..looped
        })
    );
    rig.key(Modifiers::NONE, Key::L);
    assert_eq!(region(&rig), Some(looped));
    let brace = rig.app().piano_roll.ruler_point(1.25 * BAR as f64, true);
    rig.click(brace);
    assert_eq!(
        region(&rig),
        Some(LoopRegion {
            on: false,
            ..looped
        })
    );
    rig.screenshot("off");
    rig.frames(45);
    rig.click(brace);
    assert_eq!(region(&rig), Some(looped));
    // A click below the brace moves the playhead and leaves the loop be.
    let below = rig.app().piano_roll.ruler_point(1.75 * BAR as f64, false);
    rig.frames(45);
    rig.click(below);
    sync(&mut rig);
    assert_eq!(region(&rig), Some(looped));
    assert!(rig.app().session.playhead() > 1.5 * BAR as f64);
    rig.key(Modifiers::NONE, Key::L);
    rig.key(Modifiers::NONE, Key::L);
    sync(&mut rig);
    assert_eq!(
        rig.app().session.playhead(),
        BAR as f64,
        "turning it on with L starts the playhead in it"
    );

    // Playing: two bars of audio go round the half-bar loop four times, and
    // the note at its start is heard each time round.
    rig.key(Modifiers::NONE, Key::Space);
    assert!(rig.app().session.is_playing());
    let inside = |rig: &Rig| {
        let at = rig.app().session.playhead();
        (BAR as f64..1.5 * BAR as f64).contains(&at)
    };
    let half = SAMPLES_PER_BAR / 2;
    for _ in 0..4 {
        let pass = rig.render_audio(half);
        assert!(peak(&pass) > 0.01, "the note plays every time round");
        assert!(inside(&rig), "{}", rig.app().session.playhead());
    }

    // Editing the clip while it plays: a second note, and playback stays
    // in the loop.
    let point = rig.app().piano_roll.point(1.25 * BAR as f64 + 30.0, 64);
    rig.double_click(point);
    assert_eq!(rig.notes(clip).len(), 2);
    assert!(rig.app().session.is_playing());
    for _ in 0..3 {
        rig.render_audio(half);
        assert!(inside(&rig), "{}", rig.app().session.playhead());
    }
    rig.key(Modifiers::NONE, Key::Space);
}

/// The piano roll's own Loop button, beside its grid (below the
/// transport's).
fn piano_loop_button(rig: &Rig) -> eframe::egui::Pos2 {
    rig.harness
        .get_all_by_label("Loop")
        .map(|n| n.rect().center())
        .max_by(|a, b| a.y.total_cmp(&b.y))
        .expect("a Loop button")
}

/// The transport's Loop button.
fn transport_loop_button(rig: &Rig) -> eframe::egui::Pos2 {
    rig.harness
        .get_all_by_label("Loop")
        .map(|n| n.rect().center())
        .min_by(|a, b| a.y.total_cmp(&b.y))
        .expect("a Loop button")
}

#[test]
fn the_piano_rolls_loop_button_loops_the_clip_and_is_the_transports_loop() {
    let mut rig = Rig::new("piano_loop_button");
    open_clip_with_a_note(&mut rig);
    assert_eq!(region(&rig), None);
    // The playhead is elsewhere in the song.
    rig.app_mut().session.seek(Tick::new(5 * BAR));
    sync(&mut rig);

    // With no loop yet, the button loops the clip, and the playhead goes
    // there to hear it.
    rig.click(piano_loop_button(&rig));
    sync(&mut rig);
    let clip = LoopRegion {
        start: Tick::new(BAR),
        end: Tick::new(2 * BAR),
        on: true,
    };
    assert_eq!(region(&rig), Some(clip));
    assert_eq!(rig.app().session.playhead(), BAR as f64);

    // It's the song's one loop: the transport's Loop turns it off, and the
    // piano roll's turns the same loop back on.
    rig.frames(45);
    rig.click(transport_loop_button(&rig));
    assert_eq!(region(&rig), Some(LoopRegion { on: false, ..clip }));
    rig.frames(45);
    rig.click(piano_loop_button(&rig));
    assert_eq!(region(&rig), Some(clip));
}

#[test]
fn l_in_the_piano_roll_keeps_a_loop_over_the_clip_and_otherwise_loops_the_clip() {
    let mut rig = Rig::new("piano_loop_l");
    open_clip_with_a_note(&mut rig);
    assert!(rig.app().piano_roll.focused, "the piano roll has the keys");
    let clip = LoopRegion {
        start: Tick::new(BAR),
        end: Tick::new(2 * BAR),
        on: true,
    };

    // A loop elsewhere in the song, off: L loops the clip instead.
    rig.app_mut().session.set_loop(Some(LoopRegion {
        start: Tick::new(5 * BAR),
        end: Tick::new(6 * BAR),
        on: false,
    }));
    rig.key(Modifiers::NONE, Key::L);
    assert_eq!(region(&rig), Some(clip));
    rig.key(Modifiers::NONE, Key::L);
    assert_eq!(region(&rig), Some(LoopRegion { on: false, ..clip }));

    // A loop over part of the clip is kept as it is.
    let part = LoopRegion {
        start: Tick::new(BAR + BAR / 4),
        end: Tick::new(3 * BAR),
        on: false,
    };
    rig.app_mut().session.set_loop(Some(part));
    rig.key(Modifiers::NONE, Key::L);
    assert_eq!(region(&rig), Some(LoopRegion { on: true, ..part }));
}

#[test]
fn the_brace_is_the_same_loop_in_both_rulers() {
    let mut rig = Rig::new("piano_loop_both");
    open_clip_with_a_note(&mut rig);
    let looped = LoopRegion {
        start: Tick::new(BAR),
        end: Tick::new(BAR + BAR / 2),
        on: true,
    };
    rig.app_mut().session.set_loop(Some(looped));
    rig.frames(2);

    // Drag the loop's end along the piano roll's ruler.
    let end = rig.app().piano_roll.ruler_point(1.5 * BAR as f64, true);
    let later = rig.app().piano_roll.ruler_point(1.75 * BAR as f64, true);
    rig.drag(end, later);
    let longer = LoopRegion {
        end: Tick::new(BAR + 3 * BAR / 4),
        ..looped
    };
    assert_eq!(region(&rig), Some(longer));

    // Then its start along the arrangement's.
    rig.frames(45);
    let start = rig.app().timeline.ruler_point(BAR as f64, true);
    let to = rig.app().timeline.ruler_point(1.25 * BAR as f64, true);
    rig.drag(start, to);
    let shorter = LoopRegion {
        start: Tick::new(BAR + BAR / 4),
        ..longer
    };
    assert_eq!(region(&rig), Some(shorter));

    // The piano roll's brace moved with it: a click inside it turns the
    // loop off, and one where it used to start moves the playhead instead.
    rig.frames(45);
    rig.click(rig.app().piano_roll.ruler_point(1.5 * BAR as f64, true));
    assert_eq!(
        region(&rig),
        Some(LoopRegion {
            on: false,
            ..shorter
        })
    );
    rig.frames(45);
    let before = rig.app().piano_roll.ruler_point(1.1 * BAR as f64, true);
    rig.click(before);
    sync(&mut rig);
    assert_eq!(
        region(&rig),
        Some(LoopRegion {
            on: false,
            ..shorter
        })
    );
    let at = rig.app().session.playhead();
    assert!((at - 1.1 * BAR as f64).abs() < (BAR / 8) as f64, "{at}");
}

#[test]
fn shift_l_in_the_piano_roll_loops_the_selected_notes() {
    let mut rig = Rig::new("piano_loop_notes");
    open_clip_with_a_note(&mut rig);
    // Select the note (it's selected after being added) and loop it: out
    // to the grid around it.
    rig.key(Modifiers::SHIFT, Key::L);
    let region = region(&rig).expect("a loop");
    assert!(region.on);
    assert_eq!(region.start, Tick::new(BAR));
    assert!(region.end > region.start && region.end <= Tick::new(BAR + BAR / 4));
}

#[test]
fn clicking_the_arrangements_loop_brace_turns_it_on_and_off() {
    let mut rig = Rig::new("brace_click");
    let looped = LoopRegion {
        start: Tick::new(2 * BAR),
        end: Tick::new(4 * BAR),
        on: true,
    };
    rig.app_mut().session.set_loop(Some(looped));
    rig.frames(2);
    let brace = rig.app().timeline.ruler_point(3.0 * BAR as f64, true);
    rig.click(brace);
    assert_eq!(
        region(&rig),
        Some(LoopRegion {
            on: false,
            ..looped
        })
    );
    rig.frames(45);
    rig.click(brace);
    assert_eq!(region(&rig), Some(looped));
    // Outside the loop, the brace's strip seeks like the rest of the ruler.
    rig.frames(45);
    let outside = rig.app().timeline.ruler_point(5.0 * BAR as f64, true);
    rig.click(outside);
    sync(&mut rig);
    assert_eq!(region(&rig), Some(looped));
    assert_eq!(rig.app().session.playhead(), 5.0 * BAR as f64);
}

#[test]
fn zoom_to_fit_glides_to_the_selection_and_the_song() {
    let mut rig = Rig::new("zoom_fit");
    let track = rig.app().session.project.tracks[0].id;
    // A clip at bar 3, two bars long.
    let clip = rig
        .app_mut()
        .session
        .edit("Clip", |p| {
            p.add_midi_clip(track, Tick::new(2 * BAR), Tick::new(2 * BAR))
        })
        .unwrap();
    rig.frames(2);
    let rect = rig.app().timeline.clip_rect(clip).unwrap();
    rig.click(rect.center() + vec2(0.0, 6.0));
    assert_eq!(rig.app().timeline.selection.clips, [clip]);

    let before = rig.app().timeline.viewport;
    rig.key(Modifiers::NONE, Key::Z);
    let moving = rig.app().timeline.viewport;
    assert_ne!(moving, before, "on its way");
    let steps = rig.settle();
    assert!(steps > 1, "it eases there rather than jumping");
    let view = rig.app().timeline.viewport;
    let (start, end) = ((2 * BAR) as f64, (4 * BAR) as f64);
    assert!(view.start <= start && view.end() >= end, "{view:?}");
    let filled = (end - start) / (view.end() - view.start);
    assert!(filled > 0.85, "the clip fills the view: {filled}");

    // Shift+Z: the whole song, at least eight bars.
    rig.key(Modifiers::SHIFT, Key::Z);
    rig.settle();
    let view = rig.app().timeline.viewport;
    assert_eq!(view.start, 0.0);
    assert!(view.end() >= (8 * BAR) as f64 && view.end() < (10 * BAR) as f64);
}

#[test]
fn zoom_to_fit_in_the_piano_roll_frames_the_notes_or_the_clip() {
    let mut rig = Rig::new("piano_fit");
    let clip = open_clip_with_a_note(&mut rig);
    // Zoom far out, then Z: back to the clip.
    let over = rig.app().piano_roll.point(1.5 * BAR as f64, 60);
    rig.event(Event::PointerMoved(over));
    rig.event(Event::Zoom(0.05));
    rig.settle();
    let out = rig.app().piano_roll.viewport();
    assert!(out.end() - out.start > (8 * BAR) as f64, "zoomed out");
    rig.key(Modifiers::NONE, Key::Escape);
    rig.key(Modifiers::NONE, Key::Z);
    rig.settle();
    let view = rig.app().piano_roll.viewport();
    assert!(view.start <= BAR as f64 && view.end() >= (2 * BAR) as f64);
    assert!(view.end() - view.start < 1.5 * BAR as f64, "{view:?}");

    // With a note selected, Z frames that note.
    let note = rig.app().piano_roll.point(BAR as f64 + 60.0, 60);
    rig.frames(45);
    rig.click(note);
    rig.key(Modifiers::NONE, Key::Z);
    rig.settle();
    let view = rig.app().piano_roll.viewport();
    assert!(view.start <= BAR as f64 && view.end() >= (BAR + 240) as f64);
    assert!(view.end() - view.start < (BAR / 2) as f64, "{view:?}");
    let _ = clip;
}

#[test]
fn ctrl_wheel_zoom_eases_in() {
    let mut rig = Rig::new("eased_zoom");
    let track = rig.app().session.project.tracks[0].id;
    let at = rig.lane_point(track, 4.0);
    let before = rig.app().timeline.viewport.px_per_tick;
    rig.event(Event::PointerMoved(at));
    rig.event(Event::Zoom(2.0));
    let first = rig.app().timeline.viewport.px_per_tick / before;
    assert!(
        first > 1.0 && first < 1.9,
        "part of the way at first: {first}"
    );
    rig.settle();
    let landed = rig.app().timeline.viewport.px_per_tick / before;
    assert!((landed - 2.0).abs() < 1e-6, "{landed}");
}

#[test]
fn the_piano_roll_remembers_each_clips_view() {
    let mut rig = Rig::new("piano_views");
    let track = rig.app().session.project.tracks[0].id;
    let (a, b) = rig
        .app_mut()
        .session
        .edit("Clips", |p| {
            Ok((
                p.add_midi_clip(track, Tick::ZERO, Tick::new(BAR))?,
                p.add_midi_clip(track, Tick::new(4 * BAR), Tick::new(BAR))?,
            ))
        })
        .unwrap();
    rig.frames(2);
    let open = |rig: &mut Rig, clip| {
        let rect = rig.app().timeline.clip_rect(clip).unwrap();
        rig.double_click(rect.center() + vec2(0.0, 6.0));
        rig.settle();
    };
    open(&mut rig, a);
    // Zoom in on A.
    let over = rig.app().piano_roll.point(0.5 * BAR as f64, 60);
    rig.event(Event::PointerMoved(over));
    rig.event(Event::Zoom(3.0));
    rig.settle();
    let zoomed = rig.app().piano_roll.viewport();
    open(&mut rig, b);
    assert_ne!(
        rig.app().piano_roll.viewport().start,
        zoomed.start,
        "B framed"
    );
    open(&mut rig, a);
    let back = rig.app().piano_roll.viewport();
    assert!((back.start - zoomed.start).abs() < 1e-6);
    assert!((back.px_per_tick - zoomed.px_per_tick).abs() < 1e-12);
}

/// Whether the window, left alone, goes to sleep: it settles (animations
/// done), waits out the slow checks a device panel keeps up for a couple
/// of seconds after the last thing that happened, and then asks for no
/// frame at all, not even a delayed one.
fn sleeps(rig: &mut Rig) -> bool {
    rig.settle();
    rig.frames(180);
    rig.settle();
    rig.repaint_delay() == std::time::Duration::MAX
}

#[test]
fn every_view_sleeps_when_left_alone() {
    let mut rig = Rig::new("sleep");
    assert!(
        sleeps(&mut rig),
        "the arrangement: {:?}",
        rig.repaint_delay()
    );

    // Automation lanes shown, then hidden: they grow open and shrink
    // shut, then the window sleeps again.
    use crate::timeline::Action;
    let first = rig.app().session.project.tracks[0].id;
    rig.app_mut().timeline.push(Action::ToggleAutomation(first));
    assert!(sleeps(&mut rig), "lanes open: {:?}", rig.repaint_delay());
    rig.app_mut().timeline.push(Action::ToggleAutomation(first));
    assert!(sleeps(&mut rig), "lanes shut: {:?}", rig.repaint_delay());

    // The piano roll, with a note selected.
    open_clip_with_a_note(&mut rig);
    assert!(
        sleeps(&mut rig),
        "the piano roll: {:?}",
        rig.repaint_delay()
    );

    // The device view: an instrument with meters, and effects with
    // displays.
    let track = rig.app().session.project.tracks[0].id;
    rig.app_mut().session.edit("Effects", |p| {
        p.add_device(Some(track), project::EffectRef::builtin("canopy.eq"), None)?;
        p.add_device(
            Some(track),
            project::EffectRef::builtin("canopy.compressor"),
            None,
        )
    });
    rig.key(Modifiers::SHIFT, Key::Tab);
    assert!(
        rig.harness.query_by_label("Compressor").is_some(),
        "showing"
    );
    assert!(
        sleeps(&mut rig),
        "the device view: {:?}",
        rig.repaint_delay()
    );

    // Playing keeps frames coming for the playhead and meters; once
    // stopped, it sleeps again.
    rig.key(Modifiers::NONE, Key::Space);
    rig.render_audio(512);
    assert_eq!(rig.repaint_delay(), std::time::Duration::ZERO, "playing");
    rig.key(Modifiers::NONE, Key::Space);
    rig.render_audio(512);
    assert!(sleeps(&mut rig), "stopped: {:?}", rig.repaint_delay());

    // A group folded shut, and opened again.
    let ids = rig.app().session.project.track_ids();
    rig.app_mut()
        .timeline
        .push(Action::GroupTracks(vec![ids[2], ids[3]]));
    rig.frames(1);
    rig.settle();
    let group = rig.app().timeline.selection.tracks[0];
    rig.app_mut().timeline.push(Action::SetFolded(group, true));
    assert!(sleeps(&mut rig), "folded: {:?}", rig.repaint_delay());
    rig.app_mut().timeline.push(Action::SetFolded(group, false));
    assert!(sleeps(&mut rig), "unfolded: {:?}", rig.repaint_delay());
}

#[test]
fn an_idle_window_stops_repainting() {
    let mut rig = Rig::new("idle");
    rig.settle();
    // After a fade (a selection, a hover) and a zoom to fit, it goes quiet
    // again: animations ask for frames only while they move.
    let track = rig.app().session.project.tracks[1].id;
    let point = rig.header_point(track);
    rig.click(point);
    rig.settle();
    rig.key(Modifiers::NONE, Key::Z);
    assert!(rig.settle() > 1);
    let clip = open_clip_with_a_note(&mut rig);
    rig.settle();
    let _ = clip;
    // Playing keeps frames coming, for the playhead; stopping ends them.
    rig.key(Modifiers::NONE, Key::Space);
    assert!(rig.harness.try_run().is_err(), "playing repaints");
    rig.key(Modifiers::NONE, Key::Space);
    rig.render_audio(512);
    rig.settle();
}

#[test]
fn tab_shows_where_the_keyboard_is() {
    use crate::widgets::chrome::keyboard_focus;
    let mut rig = Rig::new("focus");
    let ctx = rig.harness.ctx.clone();
    assert!(keyboard_focus(&ctx).is_none(), "nothing until Tab");

    // Tab rings a control, and the next Tab the next one.
    rig.key(Modifiers::NONE, Key::Tab);
    let (first, ..) = keyboard_focus(&ctx).expect("Tab focuses a control");
    rig.key(Modifiers::NONE, Key::Tab);
    let (second, ..) = keyboard_focus(&ctx).expect("still focused");
    assert_ne!(first, second);
    assert!(rig.settle() >= 1, "the ring glides there, then rests");

    // A click elsewhere takes the focus, and the ring, away.
    let track = rig.app().session.project.tracks[0].id;
    rig.click(rig.lane_point(track, 6.0));
    assert!(keyboard_focus(&ctx).is_none());
}

/// One notch of the wheel at `at` with `modifiers` held, given time to
/// land (scrolling and zooming are eased).
fn wheel(rig: &mut Rig, at: eframe::egui::Pos2, modifiers: Modifiers, notches: f32) {
    use eframe::egui::{MouseWheelUnit, TouchPhase};
    rig.event(Event::PointerMoved(at));
    rig.event(Event::ModifiersChanged(modifiers));
    rig.event(Event::MouseWheel {
        unit: MouseWheelUnit::Line,
        delta: vec2(0.0, notches),
        modifiers,
        phase: TouchPhase::Move,
    });
    rig.settle();
    rig.event(Event::ModifiersChanged(Modifiers::NONE));
    rig.frames(2);
}

#[test]
fn the_wheel_means_the_same_over_the_arrangement_and_the_piano_roll() {
    let mut rig = Rig::new("wheel");
    let track = rig.app().session.project.tracks[0].id;
    let ctrl = Modifiers::COMMAND;

    // The arrangement: Ctrl+wheel zooms time, Shift+wheel scrolls it.
    let at = rig.lane_point(track, 3.0);
    let before = rig.app().timeline.viewport;
    wheel(&mut rig, at, ctrl, 2.0);
    let zoomed = rig.app().timeline.viewport;
    assert!(zoomed.px_per_tick > before.px_per_tick, "zoomed in");
    let at = rig.lane_point(track, 3.0);
    wheel(&mut rig, at, Modifiers::SHIFT, -3.0);
    let scrolled = rig.app().timeline.viewport;
    assert!(scrolled.start > zoomed.start, "later in time");
    assert_eq!(scrolled.px_per_tick, zoomed.px_per_tick, "without zooming");

    // The piano roll: the same.
    rig.app_mut().timeline.viewport = before;
    rig.frames(2);
    open_clip_with_a_note(&mut rig);
    let at = rig.app().piano_roll.point(1.5 * BAR as f64, 60);
    let before = rig.app().piano_roll.viewport();
    wheel(&mut rig, at, ctrl, 2.0);
    let zoomed = rig.app().piano_roll.viewport();
    assert!(zoomed.px_per_tick > before.px_per_tick, "zoomed in");
    wheel(&mut rig, at, Modifiers::SHIFT, -3.0);
    let scrolled = rig.app().piano_roll.viewport();
    assert!(scrolled.start > zoomed.start, "later in time");
    assert_eq!(scrolled.px_per_tick, zoomed.px_per_tick, "without zooming");
}

#[test]
fn the_arrangement_opens_scrolled_and_zoomed_as_it_was_saved() {
    let mut rig = Rig::new("saved_view");
    let dir = super::temp_dir("saved_view");
    let file = dir.join("song.canopy");
    // Rows enough to scroll through.
    rig.app_mut().session.edit("Tracks", |p| {
        for _ in 0..12 {
            p.add_track(project::NewTrack::Midi, project::TrackPosition::END)?;
        }
        Ok(())
    });
    assert!(rig.app_mut().session.save_as(&file));
    rig.frames(2);

    // Zoomed in, scrolled on in time and down the rows, the rows taller:
    // kept with the project, but neither an undo step nor a change to
    // save.
    let track = rig.app().session.project.tracks[0].id;
    let at = rig.lane_point(track, 3.0);
    wheel(&mut rig, at, Modifiers::COMMAND, 2.0);
    wheel(&mut rig, at, Modifiers::SHIFT, -3.0);
    wheel(&mut rig, at, Modifiers::ALT, 2.0);
    wheel(&mut rig, at, Modifiers::NONE, -4.0);
    rig.settle();
    let view = rig.app().timeline.arrangement_view();
    let fresh = crate::timeline::TimelineView::default().arrangement_view();
    assert!(view.px_per_tick > fresh.px_per_tick, "zoomed in");
    assert!(view.start > 0.0, "scrolled on");
    assert!(view.track_height > fresh.track_height, "taller rows");
    assert!(view.scroll > 0.0, "scrolled down");
    let session = &rig.app().session;
    assert_eq!(session.project.arrangement_view, Some(view));
    assert_eq!(session.undo_label(), Some("Tracks"), "not an undo step");
    assert!(!session.is_dirty(), "nothing to save for");

    // Undo and redo keep the view as it is rather than going back to an
    // old one.
    rig.app_mut()
        .session
        .edit("Rename", |p| p.rename_track(track, "Lead"));
    rig.app_mut().session.undo();
    rig.frames(2);
    assert_eq!(rig.app().session.project.arrangement_view, Some(view));
    assert_eq!(rig.app().timeline.arrangement_view(), view);
    rig.app_mut().session.redo();
    rig.frames(2);
    assert_eq!(rig.app().session.project.arrangement_view, Some(view));

    // Saved, looked at some other way, then opened again: as it was.
    assert!(rig.app_mut().session.save_as(&file));
    rig.key(Modifiers::NONE, Key::Z);
    rig.settle();
    assert_ne!(rig.app().timeline.arrangement_view(), view);
    rig.app_mut().open_project(&file);
    rig.frames(3);
    let opened = rig.app().timeline.arrangement_view();
    assert_eq!(opened.start, view.start);
    assert_eq!(opened.px_per_tick, view.px_per_tick);
    assert_eq!(opened.track_height, view.track_height);
    assert!(
        (opened.scroll - view.scroll).abs() < 0.5,
        "{opened:?} {view:?}"
    );
    let session = &rig.app().session;
    assert_eq!(session.undo_label(), None);
    assert!(!session.is_dirty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn right_click_on_the_piano_rolls_grid_offers_what_the_keys_do() {
    let mut rig = Rig::new("grid_menu");
    let clip = open_clip_with_a_note(&mut rig);
    rig.double_click(rig.app().piano_roll.point(1.5 * BAR as f64 + 30.0, 64));
    assert_eq!(rig.notes(clip).len(), 2);
    rig.key(Modifiers::NONE, Key::Escape);

    // Empty space, away from the notes.
    let empty = rig.app().piano_roll.point(1.25 * BAR as f64, 62);
    rig.right_click(empty);
    rig.screenshot("menu");
    // Menu items are labelled with their shortcuts.
    rig.harness.get_by_label("Select All Ctrl+A").click();
    rig.frames(2);
    rig.right_click(empty);
    rig.harness.get_by_label("Delete Notes Del").click();
    rig.frames(2);
    assert!(rig.notes(clip).is_empty(), "both deleted");
    assert_eq!(rig.app().session.undo_label(), Some("Delete Notes"));

    // Loop, from the same menu: the clip, as L would.
    rig.right_click(empty);
    rig.harness.get_by_label("Loop L").click();
    rig.frames(2);
    assert_eq!(
        region(&rig),
        Some(LoopRegion {
            start: Tick::new(BAR),
            end: Tick::new(2 * BAR),
            on: true
        })
    );
}

#[test]
fn shown_lanes_and_unfolded_groups_grow_open_then_rest() {
    use crate::timeline::Action;
    let mut rig = Rig::new("grow");
    let ids = rig.app().session.project.track_ids();
    let top = |rig: &Rig, track| rig.app().timeline.row_rect(track).unwrap().top();

    // Showing a track's lanes: the rows under it make room bit by bit.
    let before = top(&rig, ids[1]);
    rig.app_mut()
        .timeline
        .push(Action::ToggleAutomation(ids[0]));
    rig.frames(3);
    let opening = top(&rig, ids[1]) - before;
    rig.settle();
    let open = top(&rig, ids[1]) - before;
    assert!(opening > 0.0 && opening < open, "{opening} of {open}");
    assert!(open >= 48.0, "a whole lane: {open}");
    rig.app_mut()
        .timeline
        .push(Action::ToggleAutomation(ids[0]));
    rig.settle();

    // Unfolding a group: its tracks grow open the same way.
    rig.app_mut()
        .timeline
        .push(Action::GroupTracks(vec![ids[0], ids[1]]));
    rig.frames(2);
    let group = rig.app().session.project.track_ids()[0];
    rig.app_mut().timeline.push(Action::SetFolded(group, true));
    rig.settle();
    let folded = top(&rig, ids[2]);
    rig.app_mut().timeline.push(Action::SetFolded(group, false));
    rig.frames(3);
    let opening = top(&rig, ids[2]) - folded;
    rig.settle();
    let open = top(&rig, ids[2]) - folded;
    assert!(opening > 0.0 && opening < open, "{opening} of {open}");
    assert!((open - 2.0 * rig.app().timeline.track_height).abs() < 1.0);
}

#[test]
fn hidden_lanes_and_folded_groups_shrink_shut_then_rest() {
    use crate::timeline::Action;
    let mut rig = Rig::new("shrink");
    let ids = rig.app().session.project.track_ids();
    let top = |rig: &Rig, track| rig.app().timeline.row_rect(track).unwrap().top();
    let closed = top(&rig, ids[1]);
    rig.app_mut()
        .timeline
        .push(Action::ToggleAutomation(ids[0]));
    rig.frames(1);
    rig.settle();
    let open = top(&rig, ids[1]);
    assert!(open >= closed + 48.0, "a whole lane: {open} from {closed}");

    // Hiding the lanes: the rows under them close up bit by bit, as
    // they opened, then the window rests.
    rig.app_mut()
        .timeline
        .push(Action::ToggleAutomation(ids[0]));
    rig.frames(3);
    let closing = top(&rig, ids[1]);
    assert!(
        closing > closed && closing < open,
        "{closing} between {closed} and {open}"
    );
    rig.screenshot("lane closing");
    let steps = rig.settle();
    assert!(steps > 1, "moved for a few frames: {steps}");
    assert_eq!(top(&rig, ids[1]), closed);

    // One lane hidden from its header: the same.
    rig.app_mut()
        .timeline
        .push(Action::ShowAutomation(ids[0], AutomationTarget::Volume));
    rig.frames(1);
    rig.settle();
    rig.app_mut()
        .timeline
        .push(Action::HideAutomation(ids[0], AutomationTarget::Volume));
    rig.frames(3);
    let closing = top(&rig, ids[1]);
    assert!(
        closing > closed && closing < open,
        "{closing} between {closed} and {open}"
    );
    rig.settle();
    assert_eq!(top(&rig, ids[1]), closed);

    // Folding a group: its tracks shrink shut, and while they do, drags
    // no longer find them.
    rig.app_mut()
        .timeline
        .push(Action::GroupTracks(vec![ids[0], ids[1]]));
    rig.frames(1);
    rig.settle();
    let group = rig.app().session.project.track_ids()[0];
    let unfolded = top(&rig, ids[2]);
    rig.app_mut().timeline.push(Action::SetFolded(group, true));
    rig.frames(3);
    let folding = top(&rig, ids[2]);
    assert!(rig.app().timeline.row_rect(ids[0]).is_none(), "going");
    rig.screenshot("group folding");
    let steps = rig.settle();
    assert!(steps > 1, "moved for a few frames: {steps}");
    let folded = top(&rig, ids[2]);
    let tracks = 2.0 * rig.app().timeline.track_height;
    assert!(
        (unfolded - folded - tracks).abs() < 1.0,
        "{unfolded} to {folded}"
    );
    assert!(folding < unfolded && folding > folded, "{folding} between");
    assert!(sleeps(&mut rig), "after folding: {:?}", rig.repaint_delay());
}

#[test]
fn shift_tab_switches_the_detail_view_without_moving_the_focus() {
    use crate::widgets::chrome::keyboard_focus;
    let mut rig = Rig::new("shift_tab");
    let ctx = rig.harness.ctx.clone();
    rig.key(Modifiers::SHIFT, Key::Tab);
    rig.frames(2);
    assert!(
        keyboard_focus(&ctx).is_none(),
        "nothing focused, nothing ringed"
    );
}

/// A Reverb on the third track, shown in the device view, with its Decay
/// knob given the keyboard's focus (as Tab would). Returns the effect.
fn focused_decay(rig: &mut Rig) -> project::DeviceId {
    let track = rig.app().session.project.track_ids()[2];
    let reverb = rig
        .app_mut()
        .session
        .edit("Reverb", |p| {
            p.add_device(
                Some(track),
                project::EffectRef::builtin("canopy.reverb"),
                None,
            )
        })
        .unwrap();
    rig.click(rig.header_point(track));
    rig.frames(2);
    rig.harness.get_by_label("Decay").focus();
    rig.frames(2);
    assert!(rig.harness.get_by_label("Decay").is_focused());
    reverb
}

fn decay(rig: &Rig, reverb: project::DeviceId) -> f32 {
    let device = rig.app().session.project.device(reverb).unwrap();
    device.param("decay").unwrap()
}

/// The Reverb's Decay range.
fn decay_range(rig: &Rig, reverb: project::DeviceId) -> (f32, f32) {
    let device = rig.app().session.project.device(reverb).unwrap();
    let param = device
        .effect
        .descriptor()
        .unwrap()
        .params
        .iter()
        .find(|p| p.id == "decay")
        .unwrap();
    match param.kind {
        instruments::ParameterKind::Float { min, max, .. } => (min, max),
        _ => unreachable!("a time"),
    }
}

#[test]
fn a_focused_knob_turns_with_the_arrow_keys() {
    let mut rig = Rig::new("knob_keys");
    let reverb = focused_decay(&mut rig);
    let ctx = rig.harness.ctx.clone();
    // It rings itself, round, rather than wearing the app's rectangle; the
    // keyboard focus is still where the app says it is.
    assert!(crate::widgets::chrome::focus_rings_itself(&ctx));
    assert!(crate::widgets::chrome::keyboard_focus(&ctx).is_some());
    rig.screenshot("focused");

    // Up and Right turn it up a wheel notch a press, each press an undo
    // step; the focus stays put rather than moving on.
    let start = decay(&rig, reverb);
    rig.key(Modifiers::NONE, Key::ArrowUp);
    let up = decay(&rig, reverb);
    assert!(up > start, "{up} from {start}");
    assert_eq!(rig.app().session.undo_label(), Some("Change Effect"));
    rig.key(Modifiers::NONE, Key::ArrowRight);
    let right = decay(&rig, reverb);
    assert!(right > up, "{right} from {up}");
    assert!(rig.harness.get_by_label("Decay").is_focused());

    // Down and Left turn it back.
    rig.key(Modifiers::NONE, Key::ArrowDown);
    rig.key(Modifiers::NONE, Key::ArrowLeft);
    assert!((decay(&rig, reverb) - start).abs() < 1e-3);

    // Shift makes a step finer.
    rig.key(Modifiers::SHIFT, Key::ArrowUp);
    let fine = decay(&rig, reverb) - start;
    assert!(fine > 0.0 && fine < (up - start) / 4.0, "{fine}");

    // End and Home go to either end; Home doesn't also move the playhead.
    rig.app_mut().session.seek(Tick::new(BAR));
    sync(&mut rig);
    let (min, max) = decay_range(&rig, reverb);
    rig.key(Modifiers::NONE, Key::End);
    assert_eq!(decay(&rig, reverb), max);
    rig.key(Modifiers::NONE, Key::Home);
    assert_eq!(decay(&rig, reverb), min);
    sync(&mut rig);
    assert_eq!(rig.app().session.playhead(), BAR as f64, "the knob's Home");
    rig.app_mut().session.undo();
    assert_eq!(decay(&rig, reverb), max, "Home was a step of its own");

    // Escape lets go of the knob; then the keys are the shortcuts' again.
    rig.key(Modifiers::NONE, Key::Escape);
    assert!(!rig.harness.get_by_label("Decay").is_focused());
    rig.key(Modifiers::NONE, Key::Home);
    sync(&mut rig);
    assert_eq!(rig.app().session.playhead(), 0.0);
    assert_eq!(decay(&rig, reverb), max);
}

#[test]
fn holding_an_arrow_key_turns_a_knob_as_one_undo_step() {
    let mut rig = Rig::new("knob_burst");
    let reverb = focused_decay(&mut rig);
    let start = decay(&rig, reverb);
    let key = |pressed, repeat| Event::Key {
        key: Key::ArrowUp,
        pressed,
        repeat,
        modifiers: Modifiers::NONE,
        physical_key: None,
    };

    // Held down, the key repeats: each repeat turns it further, heard at
    // once, but nothing is an undo step until the key is let go.
    rig.event(key(true, false));
    let mut last = decay(&rig, reverb);
    assert!(last > start);
    for _ in 0..4 {
        rig.event(key(true, true));
        rig.frames(1);
        let now = decay(&rig, reverb);
        assert!(now > last, "{now} after {last}");
        last = now;
    }
    assert_eq!(rig.app().session.undo_label(), Some("Reverb"), "still held");
    rig.event(key(false, false));
    rig.frames(1);
    assert_eq!(decay(&rig, reverb), last);
    assert_eq!(rig.app().session.undo_label(), Some("Change Effect"));

    // One undo takes the whole burst back.
    rig.app_mut().session.undo();
    assert_eq!(decay(&rig, reverb), start);
    assert_eq!(rig.app().session.undo_label(), Some("Reverb"));
}
