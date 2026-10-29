//! Automation lanes: curves, drawing, several lanes, and Automate menus.

use eframe::egui::{Event, Key, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::kittest::Queryable;
use project::{AutomationPoint, AutomationTarget};

use super::Rig;

fn points(rig: &Rig, track: project::TrackId, target: &AutomationTarget) -> Vec<AutomationPoint> {
    rig.app()
        .session
        .project
        .track(track)
        .unwrap()
        .automation_lane(target)
        .map(|l| l.points.to_vec())
        .unwrap_or_default()
}

fn right_click(rig: &mut Rig, pos: Pos2) {
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

/// Shows the first track's volume lane and puts a low point at bar 1 and a
/// high one at bar 3.
fn ramp(rig: &mut Rig) -> (project::TrackId, eframe::egui::Rect) {
    let track = rig.app().session.project.tracks[0].id;
    rig.harness.get_all_by_label("A").next().unwrap().click();
    rig.frames(3);
    let lane = rig.app().timeline.automation_rect(track).unwrap();
    let viewport = rig.app().timeline.viewport;
    for (bar, height) in [(0.0, 0.1), (2.0, 0.9)] {
        let x = viewport.x(bar * 3840.0) + 1.0;
        rig.double_click(pos2(x, lane.bottom() - lane.height() * height));
    }
    assert_eq!(points(rig, track, &AutomationTarget::Volume).len(), 2);
    (track, lane)
}

#[test]
fn bend_a_segment_and_straighten_it() {
    let mut rig = Rig::new("automation_curve");
    let (track, lane) = ramp(&mut rig);
    let viewport = rig.app().timeline.viewport;
    let volume = AutomationTarget::Volume;
    let [a, b] = points(&rig, track, &volume)[..] else {
        unreachable!()
    };
    let dot = |rig: &Rig| {
        let p = points(rig, track, &volume);
        let mid = project::segment_value(p[0].value, p[1].value, p[0].curve, 0.5);
        let x = (viewport.x(a.tick.get() as f64) + viewport.x(b.tick.get() as f64)) / 2.0;
        pos2(x, lane.bottom() - 6.0 - mid * (lane.height() - 12.0))
    };

    // Down on a rising segment: slow first.
    rig.frames(45);
    let from = dot(&rig);
    rig.drag(from, from + vec2(0.0, 25.0));
    let curve = points(&rig, track, &volume)[0].curve;
    assert!(curve > 0.3, "bent to start slow: {curve}");
    assert_eq!(rig.app().session.undo_label(), Some("Edit Automation"));
    rig.screenshot("curved");

    rig.double_click(dot(&rig));
    assert_eq!(points(&rig, track, &volume)[0].curve, 0.0);
}

#[test]
fn draw_points_with_b_or_ctrl() {
    let mut rig = Rig::new("automation_draw");
    let track = rig.app().session.project.tracks[0].id;
    rig.harness.get_all_by_label("A").next().unwrap().click();
    rig.frames(3);
    let lane = rig.app().timeline.automation_rect(track).unwrap();
    let viewport = rig.app().timeline.viewport;

    rig.click(pos2(700.0, 420.0));
    rig.key(Modifiers::NONE, Key::B);
    assert!(rig.app().timeline.automation_draw);
    // A diagonal stroke over two bars.
    let from = pos2(viewport.x(0.0) + 2.0, lane.bottom() - 8.0);
    let to = pos2(viewport.x(2.0 * 3840.0) - 2.0, lane.top() + 8.0);
    rig.drag(from, to);
    let drawn = points(&rig, track, &AutomationTarget::Volume);
    assert!(drawn.len() >= 8, "a point per grid step: {}", drawn.len());
    assert!(
        drawn.windows(2).all(|w| w[1].value >= w[0].value - 0.05),
        "rising"
    );
    assert_eq!(rig.app().session.undo_label(), Some("Edit Automation"));
    rig.screenshot("drawn");

    // Undo removes the whole stroke at once.
    rig.click(pos2(700.0, 420.0));
    rig.key(Modifiers::COMMAND, Key::Z);
    assert!(points(&rig, track, &AutomationTarget::Volume).is_empty());

    // Ctrl+drag draws without draw mode.
    rig.key(Modifiers::NONE, Key::B);
    assert!(!rig.app().timeline.automation_draw);
    rig.event(Event::ModifiersChanged(Modifiers::COMMAND));
    rig.drag(from, to);
    rig.event(Event::ModifiersChanged(Modifiers::NONE));
    assert!(points(&rig, track, &AutomationTarget::Volume).len() >= 8);
}

#[test]
fn automate_from_controls_gives_more_lanes() {
    let mut rig = Rig::new("automate_menu");
    let track = rig.app().session.project.tracks[0].id;
    // The device view shows the first track's synth. Right-click Cutoff.
    let cutoff = rig.harness.get_by_label("Cutoff").rect().center();
    right_click(&mut rig, cutoff);
    rig.harness.get_by_label("Automate").click();
    let cutoff_value = rig.app().session.project.tracks[0].instrument_param("cutoff");
    assert_eq!(
        cutoff_value,
        Some(3000.0),
        "right-clicking doesn't move the slider"
    );
    assert_eq!(rig.app().session.undo_label(), None);
    rig.frames(3);
    let cutoff_lane = AutomationTarget::InstrumentParam("cutoff".into());
    assert_eq!(
        rig.app().timeline.automation.get(&track).map(Vec::as_slice),
        Some(&[cutoff_lane.clone()][..])
    );

    // And from the envelope editor's menu: a second lane.
    let id = eframe::egui::Id::new(("envelope", track.0, 0usize)).with("rect");
    let editor: eframe::egui::Rect = rig.harness.ctx.data(|d| d.get_temp(id)).unwrap();
    right_click(&mut rig, editor.right_center() - vec2(20.0, 0.0));
    rig.harness.get_by_label("Automate Attack").click();
    rig.frames(3);
    let lanes = rig.app().timeline.automation.get(&track).cloned().unwrap();
    assert_eq!(
        lanes,
        [
            cutoff_lane,
            AutomationTarget::InstrumentParam("attack".into())
        ]
    );
    rig.screenshot("two_lanes");

    // Each lane's × hides just that one.
    rig.harness
        .get_all_by_label("Hide lane")
        .next()
        .unwrap()
        .click();
    rig.frames(2);
    assert_eq!(
        rig.app().timeline.automation.get(&track).map(Vec::len),
        Some(1)
    );
}

#[test]
fn select_move_delete_and_generate_points() {
    let mut rig = Rig::new("automation_tools");
    let track = rig.app().session.project.tracks[0].id;
    rig.harness.get_all_by_label("A").next().unwrap().click();
    rig.frames(3);
    let volume = AutomationTarget::Volume;
    let five: Vec<AutomationPoint> = (0..5)
        .map(|i| AutomationPoint {
            tick: midi::Tick::new(i * 1920),
            value: 0.3,
            curve: 0.0,
        })
        .collect();
    let (t, v) = (track, volume.clone());
    rig.app_mut()
        .session
        .edit("Points", |p| p.set_automation(t, v, five));
    rig.frames(2);
    let lane = rig.app().timeline.automation_rect(track).unwrap();
    let viewport = rig.app().timeline.viewport;
    let at = |tick: f64, value: f32| {
        pos2(
            viewport.x(tick),
            lane.bottom() - 6.0 - value * (lane.height() - 12.0),
        )
    };

    // A box around the middle three.
    rig.frames(45);
    rig.drag(at(1000.0, 0.9) - vec2(0.0, 2.0), at(6700.0, 0.05));
    let selection = rig.app().timeline.automation_selection.clone().unwrap();
    assert_eq!(selection.indices, [1, 2, 3]);
    rig.screenshot("selected");

    // Dragging one moves all three up.
    rig.frames(45);
    rig.drag(at(3840.0, 0.3), at(3840.0, 0.6));
    let moved = points(&rig, track, &volume);
    let values: Vec<f32> = moved
        .iter()
        .map(|p| (p.value * 10.0).round() / 10.0)
        .collect();
    assert_eq!(values, [0.3, 0.6, 0.6, 0.6, 0.3]);
    assert_eq!(
        rig.app()
            .timeline
            .automation_selection
            .as_ref()
            .unwrap()
            .indices,
        [1, 2, 3]
    );

    // Delete removes them.
    rig.key(Modifiers::NONE, Key::Delete);
    assert_eq!(points(&rig, track, &volume).len(), 2);
    assert!(rig.app().timeline.automation_selection.is_none());
    assert_eq!(rig.app().session.project.tracks.len(), 4, "the track stays");

    // Generate a sine over the bars in view, previewed live.
    right_click(&mut rig, at(5000.0, 0.8));
    rig.harness.get_by_label("Generate…").click();
    rig.frames(3);
    rig.screenshot("generate");
    let preview = points(&rig, track, &volume);
    assert!(preview.len() > 10, "previewing: {}", preview.len());
    rig.harness.get_by_label("Apply").click();
    rig.frames(2);
    assert_eq!(rig.app().session.undo_label(), Some("Generate Automation"));
    let lane_now = rig
        .app()
        .session
        .project
        .track(track)
        .unwrap()
        .automation_lane(&volume)
        .unwrap()
        .clone();
    let quarter = lane_now.value_at(midi::Tick::new(960)).unwrap();
    assert!(quarter > 0.95, "a sine's peak a quarter bar in: {quarter}");

    // Random steps, rerolled: a different lane each time, within range.
    right_click(&mut rig, at(5000.0, 0.8));
    rig.harness.get_by_label("Generate…").click();
    rig.frames(2);
    // The Shape box (the synth's waveform buttons say "Sine" too).
    rig.harness
        .get_all_by_role(eframe::egui::accesskit::Role::ComboBox)
        .find(|n| n.value().as_deref() == Some("Sine"))
        .expect("the shape box")
        .click();
    rig.frames(2);
    rig.harness.get_by_label("Random steps").click();
    rig.frames(3);
    let first = points(&rig, track, &volume);
    rig.harness.get_by_label("Reroll").click();
    rig.frames(3);
    let second = points(&rig, track, &volume);
    assert_ne!(first, second);
    // Cancel puts back the sine.
    rig.harness.get_by_label("Cancel").click();
    rig.frames(2);
    let restored = rig
        .app()
        .session
        .project
        .track(track)
        .unwrap()
        .automation_lane(&volume)
        .unwrap()
        .clone();
    assert_eq!(restored, lane_now);
}
