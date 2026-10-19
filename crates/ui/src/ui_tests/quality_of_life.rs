//! Zooming, scrolling, clipboard and nudging, driven through real input.

use eframe::egui::{self, Event, Key, Modifiers, MouseWheelUnit, PointerButton, pos2, vec2};

use egui_kittest::kittest::Queryable;

use super::Rig;

fn alt_wheel(rig: &mut Rig, at: egui::Pos2, notches: f32) {
    rig.event(Event::PointerMoved(at));
    rig.event(Event::ModifiersChanged(Modifiers::ALT));
    rig.event(Event::MouseWheel {
        unit: MouseWheelUnit::Line,
        delta: vec2(0.0, notches),
        modifiers: Modifiers::ALT,
        phase: egui::TouchPhase::Move,
    });
    rig.frames(8);
    rig.event(Event::ModifiersChanged(Modifiers::NONE));
    rig.frames(2);
}

#[test]
fn alt_wheel_zooms_track_and_key_heights() {
    let mut rig = Rig::new("vertical_zoom");
    let track = rig.app().session.project.tracks[0].id;
    let before = rig.app().timeline.track_height;
    let at = rig.lane_point(track, 2.0);
    alt_wheel(&mut rig, at, 3.0);
    let taller = rig.app().timeline.track_height;
    assert!(taller > before, "{before} -> {taller}");
    assert!(rig.app().timeline.row_rect(track).unwrap().height() > before);
    let at = rig.lane_point(track, 2.0);
    alt_wheel(&mut rig, at, -12.0);
    assert!(rig.app().timeline.track_height < before, "and back down");
    rig.screenshot("short_rows");

    // The same gesture over the piano roll changes its key height.
    rig.double_click(rig.lane_point(track, 1.1));
    rig.frames(2);
    let key = |rig: &Rig| {
        let a = rig.app().piano_roll.point(3840.0, 60);
        let b = rig.app().piano_roll.point(3840.0, 61);
        a.y - b.y
    };
    let keys_before = key(&rig);
    let at = rig.app().piano_roll.point(4000.0, 60);
    alt_wheel(&mut rig, at, 3.0);
    assert!(key(&rig) > keys_before);
}

#[test]
fn copy_paste_and_nudge_clips() {
    let mut rig = Rig::new("clipboard");
    let track = rig.app().session.project.tracks[0].id;
    rig.double_click(rig.lane_point(track, 0.1));
    let starts = |rig: &Rig| {
        rig.app()
            .session
            .project
            .track(track)
            .unwrap()
            .clips()
            .iter()
            .map(|c| c.start().get())
            .collect::<Vec<_>>()
    };
    assert_eq!(starts(&rig), [0]);

    // Select it, then copy; click at bar 3 for the marker, then paste.
    rig.frames(45);
    rig.click(rig.lane_point(track, 0.5));
    rig.event(Event::Copy);
    rig.frames(45);
    rig.click(rig.lane_point(track, 2.0) + vec2(1.0, 0.0));
    assert_eq!(rig.app().timeline.marker, Some(midi::Tick::new(7680)));
    rig.event(Event::Paste("canopy clips".into()));
    rig.frames(2);
    assert_eq!(starts(&rig), [0, 7680]);
    assert_eq!(
        rig.app().timeline.selection.clips.len(),
        1,
        "the copy is selected"
    );

    // Right arrow nudges the selection a grid step (an eighth at this zoom).
    rig.key(Modifiers::NONE, Key::ArrowRight);
    assert_eq!(starts(&rig), [0, 7680 + 480]);
    rig.key(Modifiers::NONE, Key::ArrowLeft);
    assert_eq!(starts(&rig), [0, 7680]);
}

#[test]
fn clicking_a_lane_while_stopped_moves_the_playhead() {
    let mut rig = Rig::new("click_seek");
    let track = rig.app().session.project.tracks[2].id;
    rig.click(rig.lane_point(track, 3.0) + vec2(1.0, 0.0));
    rig.render_audio(512);
    let playhead = rig.app().session.playhead();
    assert!(
        (playhead - 3.0 * 3840.0).abs() < 1.0,
        "playhead at {playhead}"
    );
}

#[test]
fn scrollbar_and_edge_scrolling() {
    let mut rig = Rig::new("scrolling");
    let track = rig.app().session.project.tracks[0].id;
    // Drag the scrollbar thumb right.
    let lanes_left = rig.app().timeline.viewport.left;
    let scrollbar = pos2(lanes_left + 20.0, scrollbar_y(&rig));
    rig.drag(scrollbar, scrollbar + vec2(100.0, 0.0));
    let scrolled = rig.app().timeline.viewport.start;
    assert!(scrolled > 0.0, "scrollbar moved the view to {scrolled}");

    // Dragging a clip against the right edge scrolls the timeline.
    let mut rig = Rig::new("edge_scroll");
    rig.double_click(rig.lane_point(track, 0.1));
    rig.frames(45);
    let from = rig.lane_point(track, 0.5);
    let edge = pos2(
        rig.app().timeline.viewport.left + rig.app().timeline.viewport.width - 5.0,
        from.y,
    );
    rig.event(Event::PointerMoved(from));
    rig.press(from, true, Modifiers::NONE);
    for i in 1..=8 {
        rig.event(Event::PointerMoved(from + (edge - from) * (i as f32 / 8.0)));
    }
    rig.frames(30);
    assert!(
        rig.app().timeline.viewport.start > 0.0,
        "scrolled while held at the edge"
    );
    rig.press(edge, false, Modifiers::NONE);
    rig.frames(2);
    let clip = rig.app().session.project.track(track).unwrap().clips()[0];
    assert!(
        clip.start().get() > 7 * 3840,
        "dropped beyond the first screen: {}",
        clip.start().get()
    );
}

fn scrollbar_y(rig: &Rig) -> f32 {
    rig.app().timeline.scrollbar_rect().center().y
}

#[test]
fn notes_copy_paste_and_right_click_delete() {
    let mut rig = Rig::new("note_clipboard");
    let track = rig.app().session.project.tracks[0].id;
    rig.double_click(rig.lane_point(track, 1.1));
    let clip = rig.app().timeline.selection.open_clip.unwrap();
    rig.frames(2);
    let point = rig.app().piano_roll.point(3840.0 + 30.0, 60);
    rig.double_click(point);
    assert_eq!(rig.notes(clip), [(0, 60)]);

    // The new note is selected: copy it, click an empty spot a beat later, paste.
    rig.event(Event::Copy);
    rig.frames(45);
    rig.click(rig.app().piano_roll.point(3840.0 + 960.0 + 30.0, 64));
    rig.event(Event::Paste("canopy notes".into()));
    rig.frames(2);
    assert_eq!(rig.notes(clip), [(0, 60), (960, 60)]);

    // Right-click deletes one.
    let pasted = rig.app().piano_roll.point(3840.0 + 960.0 + 30.0, 60);
    rig.event(Event::PointerMoved(pasted));
    for pressed in [true, false] {
        rig.event(Event::PointerButton {
            pos: pasted,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    rig.frames(2);
    assert_eq!(rig.notes(clip), [(0, 60)]);
}

#[test]
fn pick_a_scale_and_see_it_in_the_piano_roll() {
    use project::{Scale, ScaleMode};
    let mut rig = Rig::new("scale");
    let track = rig.app().session.project.tracks[0].id;
    rig.double_click(rig.lane_point(track, 0.5));
    // The transport's scale button is labelled with the scale.
    let open_picker = |rig: &mut Rig, current: &str| {
        rig.harness.get_by_label(current).click();
        rig.frames(2);
    };
    open_picker(&mut rig, "No scale");
    rig.harness.get_by_label("Minor").click();
    rig.frames(2);
    assert_eq!(
        rig.app().session.project.scale,
        Some(Scale::new(0, ScaleMode::Minor))
    );
    open_picker(&mut rig, "C minor");
    rig.harness.get_by_label("A#").click();
    rig.frames(2);
    assert_eq!(
        rig.app().session.project.scale,
        Some(Scale::new(10, ScaleMode::Minor))
    );
    assert_eq!(rig.app().session.undo_label(), Some("Set Scale"));
    rig.screenshot("piano_roll");
    open_picker(&mut rig, "A# minor");
    rig.screenshot("picker");
    rig.harness.get_by_label("No scale").click();
    rig.frames(2);
    assert_eq!(rig.app().session.project.scale, None);
}

#[test]
fn shift_drag_the_ruler_to_loop() {
    let mut rig = Rig::new("loop");
    let track = rig.app().session.project.tracks[0].id;
    // The ruler runs just above the first track.
    let ruler_y = rig.app().timeline.row_rect(track).unwrap().top() - 8.0;
    let at_bar = |rig: &Rig, bar: f32| pos2(rig.lane_point(track, bar).x, ruler_y);
    let bar = 3840;

    // Shift+drag from bar 2 to bar 4: a loop there, on.
    rig.drag_with(at_bar(&rig, 1.0), at_bar(&rig, 3.0), Modifiers::SHIFT);
    let region = rig.app().session.project.loop_region.expect("a loop");
    assert_eq!((region.start.get(), region.end.get()), (bar, 3 * bar));
    assert!(region.on);
    assert_eq!(
        rig.app().session.undo_label(),
        None,
        "looping isn't an undo step"
    );
    rig.screenshot("set");

    // Drag its end out to bar 5.
    rig.drag(at_bar(&rig, 3.0), at_bar(&rig, 4.0));
    let region = rig.app().session.project.loop_region.unwrap();
    assert_eq!(region.end.get(), 4 * bar);

    // It plays round: from bar 2, after four bars of playing it's still in it.
    rig.click(rig.lane_point(track, 1.0));
    rig.key(Modifiers::NONE, Key::Space);
    rig.render_audio(4 * super::SAMPLES_PER_BAR);
    let playhead = rig.app().session.playhead();
    assert!(
        (bar as f64..(4 * bar) as f64).contains(&playhead),
        "{playhead}"
    );

    // L turns it off; the region stays, marked.
    rig.key(Modifiers::NONE, Key::L);
    let region = rig.app().session.project.loop_region.unwrap();
    assert!(!region.on);
    rig.key(Modifiers::NONE, Key::Space);
}
