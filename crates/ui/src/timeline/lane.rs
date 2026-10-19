//! One track's row of the timeline: grid, clips, and clicks on empty space.

use eframe::egui::{self, Rect, Sense, pos2};
use midi::Tick;
use project::{ClipId, Track, TrackKind};

use super::clip::{self, ClipLook};
use super::{Action, Gesture, SelectMode, TimelineView, color32, id, interaction, paint_lines};
use crate::session::Session;
use crate::theme;

pub(super) fn show(
    view: &mut TimelineView,
    ui: &mut egui::Ui,
    session: &Session,
    track: &Track,
    rect: Rect,
    step: Tick,
    clip_rects: &mut Vec<(ClipId, Rect)>,
) {
    let visuals = ui.visuals();
    let painter = ui.painter_at(rect);
    let selected = view.selection.has_track(track.id);
    let background = if selected {
        visuals
            .extreme_bg_color
            .lerp_to_gamma(visuals.selection.bg_fill, 0.08)
    } else {
        visuals.extreme_bg_color
    };
    painter.rect_filled(rect, 0.0, background);
    paint_lines(
        &painter,
        rect,
        &view.viewport,
        &session.project.tempo,
        step,
        visuals.weak_text_color().gamma_multiply(0.25),
    );
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        visuals.widgets.noninteractive.bg_stroke,
    );

    let response = ui.interact(rect, id("lane", track.id.0), Sense::click_and_drag());
    background_response(view, ui, session, track, &response, step);

    let clip_area = rect.shrink2([0.0, 2.0].into());
    if track.is_group() {
        paint_summary(view, &painter, clip_area, track, session);
        return;
    }
    let moving = moving_clips(view);
    // A clip's widgets (its body, edges and handles) only matter where the
    // pointer can reach them this frame: over the clip, or anywhere while a
    // button is down, a gesture is in progress or a menu is open (a drag
    // or a menu outlives the pointer's stay over the clip). Registering
    // widgets for every clip in view is most of a frame on a long song.
    let everywhere = view.gesture.is_some()
        || ui.input(|i| i.pointer.any_down())
        || egui::Popup::is_any_open(ui.ctx());
    for clip in track.clips() {
        let start = view.viewport.x(clip.start().get() as f64);
        let end = view.viewport.x(session.project.clip_end(clip).get() as f64);
        if end < rect.left() || start > rect.right() {
            continue;
        }
        let clip_rect = Rect::from_min_max(
            pos2(start, clip_area.top()),
            pos2(end.max(start + 2.0), clip_area.bottom()),
        );
        clip_rects.push((clip.id(), clip_rect));
        let selected = view.selection.has_clip(clip.id());
        let pointed = ui.rect_contains_pointer(clip_rect);
        let look = if moving.contains(&clip.id()) {
            ClipLook::Dimmed
        } else {
            // Selection and hover fade rather than snap.
            let ctx = ui.ctx();
            ClipLook::Normal {
                selected: theme::fade(ctx, id("clip selected", clip.id().0), selected),
                hovered: ctx.animate_bool_with_time(
                    id("clip hovered", clip.id().0),
                    pointed && view.gesture.is_none(),
                    theme::anim::FAST,
                ),
            }
        };
        let handles = selected
            || pointed
            || matches!(&view.gesture, Some(Gesture::ClipHandle(drag)) if drag.clip == clip.id());
        clip::paint(
            &painter,
            clip_rect,
            clip,
            color32(track.color),
            look,
            handles,
            session,
            &view.viewport,
        );
        // The handles' grab squares reach a little past the clip.
        if everywhere || ui.rect_contains_pointer(clip_rect.expand(8.0)) {
            interaction::clip(view, ui, session, track, clip, clip_rect, step);
        }
    }
}

/// Clips that a drag in progress is moving or resizing.
fn moving_clips(view: &TimelineView) -> Vec<ClipId> {
    match &view.gesture {
        Some(Gesture::MoveClips(drag)) if drag.has_moved() => {
            drag.clips.iter().map(|c| c.id).collect()
        }
        Some(Gesture::Resize(resize)) => vec![resize.clip],
        _ => Vec::new(),
    }
}

fn background_response(
    view: &mut TimelineView,
    ui: &egui::Ui,
    session: &Session,
    track: &Track,
    response: &egui::Response,
    step: Tick,
) {
    let modifiers = ui.input(|i| i.modifiers);
    let viewport = view.viewport;
    let pointer_tick = |pos: egui::Pos2| viewport.tick_at(pos.x).max(0.0);
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        if !modifiers.command && !modifiers.shift {
            view.push(Action::ClearSelection);
        }
        view.push(Action::SelectTrack {
            track: track.id,
            mode: SelectMode::Replace,
        });
        let tick = view.grid.snap(pointer_tick(pos), step, modifiers.alt);
        view.push(Action::SetMarker(tick));
        // While stopped, playback starts where you clicked, as in Ableton.
        if !session.is_playing() {
            view.push(Action::Seek(tick));
        }
    }
    if response.double_clicked()
        && let TrackKind::Midi { .. } = track.kind
        && let Some(pos) = response.interact_pointer_pos()
    {
        let start = view.grid.snap_floor(pointer_tick(pos), step, modifiers.alt);
        view.push(Action::AddMidiClip {
            track: track.id,
            start,
            length: session.project.tempo.bar_len(),
        });
    }

    // Dragging across empty space selects the clips it touches.
    if response.drag_started()
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
    {
        let base = if modifiers.command || modifiers.shift {
            view.selection.clips.clone()
        } else {
            Vec::new()
        };
        view.gesture = Some(Gesture::RubberBand {
            origin,
            current: origin,
            base,
        });
    }
}

/// A group's lane shows its children's clips in miniature, one thin strip
/// per child, so a folded group still shows where things happen.
fn paint_summary(
    view: &TimelineView,
    painter: &egui::Painter,
    rect: Rect,
    group: &Track,
    session: &Session,
) {
    let mut lanes = Vec::new();
    group.walk(&mut |t| {
        if !t.is_group() {
            lanes.push(t);
        }
    });
    if lanes.is_empty() {
        return;
    }
    let row = (rect.height() / lanes.len() as f32).clamp(2.0, 8.0);
    let top = rect.center().y - row * lanes.len() as f32 / 2.0;
    for (i, track) in lanes.iter().enumerate() {
        let y = top + i as f32 * row;
        for clip in track.clips() {
            let start = view.viewport.x(clip.start().get() as f64);
            let end = view.viewport.x(session.project.clip_end(clip).get() as f64);
            let strip =
                Rect::from_min_max(pos2(start, y), pos2(end.max(start + 1.0), y + row - 1.0));
            painter.rect_filled(strip, 1.0, color32(track.color).gamma_multiply(0.8));
        }
    }
}
