//! Painting a clip body: a waveform from the media peaks, or a miniature of
//! its notes.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Mesh, Painter, Pos2, Rect, Stroke, StrokeKind,
    pos2,
};
use project::{AudioClip, ClipRef, MidiClip, Project};

use super::Viewport;
use crate::session::{Session, SourceState};
use crate::theme::{self, color, radius, stroke, text};

/// Height of the strip with the clip's name.
pub const TITLE_HEIGHT: f32 = 13.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipLook {
    /// In its place. `selected` and `hovered` are how far those highlights
    /// have faded in, 0 to 1.
    Normal { selected: f32, hovered: f32 },
    /// Left behind while a drag shows where it's going.
    Dimmed,
    /// The preview of where a drag will put it.
    Ghost,
}

#[allow(clippy::too_many_arguments)]
pub fn paint(
    painter: &Painter,
    rect: Rect,
    clip: ClipRef<'_>,
    color: Color32,
    look: ClipLook,
    show_handles: bool,
    session: &Session,
    viewport: &Viewport,
) {
    let alpha = match look {
        ClipLook::Dimmed => 0.35,
        ClipLook::Ghost => 0.6,
        _ => 1.0,
    };
    // Muted clips keep their place but lose their color.
    let color = if clip.muted() {
        let gray = color.r().max(color.g()).max(color.b()) / 2 + 30;
        Color32::from_gray(gray)
    } else {
        color
    };
    let corner = CornerRadius::same(radius::CONTROL);
    // A hovered clip lifts a little out of the lane.
    let hovered = match look {
        ClipLook::Normal { hovered, .. } => hovered,
        _ => 0.0,
    };
    let lift = 0.07 * hovered;
    let body = theme::surface::CANVAS
        .lerp_to_gamma(color, 0.30 + lift)
        .gamma_multiply(alpha);
    painter.rect_filled(rect, corner, body);
    let title = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + TITLE_HEIGHT));
    painter.rect_filled(
        title,
        CornerRadius {
            nw: radius::CONTROL,
            ne: radius::CONTROL,
            sw: 0,
            se: 0,
        },
        color
            .lerp_to_gamma(Color32::WHITE, 0.08 * hovered)
            .gamma_multiply(alpha),
    );
    if rect.width() > 24.0 {
        let text_painter = painter.with_clip_rect(
            painter
                .clip_rect()
                .intersect(title.shrink2([3.0, 0.0].into())),
        );
        text_painter.text(
            pos2(title.left() + 4.0, title.center().y),
            Align2::LEFT_CENTER,
            clip.name(),
            text::medium(text::SMALL - 0.5),
            color::ON_BRIGHT,
        );
    }

    // A pulsing dot while the clip's audio is being rendered.
    if session.is_rendering(clip.id()) && rect.width() > 16.0 {
        let time = painter.ctx().input(|i| i.time);
        let pulse = 0.5 + 0.5 * (time * 6.0).sin() as f32;
        let dot = pos2(rect.right() - 7.0, title.center().y);
        painter.circle_filled(
            dot,
            3.0,
            Color32::from_black_alpha(120 + (100.0 * pulse) as u8),
        );
        painter.ctx().request_repaint();
    }

    let content =
        Rect::from_min_max(pos2(rect.left(), title.bottom()), rect.max).shrink2([0.0, 2.0].into());
    let ink = Color32::WHITE
        .lerp_to_gamma(color, 0.45)
        .gamma_multiply(0.95 * alpha);
    let content_painter = painter.with_clip_rect(painter.clip_rect().intersect(content));
    match clip {
        ClipRef::Midi(midi) => paint_notes(
            &content_painter,
            content,
            midi,
            ink,
            &session.project,
            viewport,
        ),
        ClipRef::Audio(audio) => {
            paint_waveform(&content_painter, content, audio, ink, session, viewport)
        }
    }

    if let ClipRef::Audio(audio) = clip
        && let Some(handles) = AudioHandles::new(rect, audio, &session.project, viewport)
    {
        paint_fades(&content_painter, &handles, audio);
        if show_handles {
            handles.paint(painter, color);
        }
    }

    painter.rect_stroke(rect, corner, outline(look), StrokeKind::Inside);
}

/// A clip's outline: dark at rest, white when selected (fading between),
/// faint white on a drag's preview.
fn outline(look: ClipLook) -> Stroke {
    match look {
        ClipLook::Normal { selected, .. } => Stroke::new(
            stroke::HAIRLINE + (stroke::EMPHASIS - stroke::HAIRLINE) * selected,
            color::OUTLINE.lerp_to_gamma(color::SELECTED_OUTLINE, selected),
        ),
        ClipLook::Ghost => Stroke::new(
            stroke::HAIRLINE,
            color::SELECTED_OUTLINE.gamma_multiply(0.8),
        ),
        ClipLook::Dimmed => Stroke::new(stroke::HAIRLINE, color::OUTLINE),
    }
}

fn paint_notes(
    painter: &Painter,
    rect: Rect,
    clip: &MidiClip,
    ink: Color32,
    project: &Project,
    viewport: &Viewport,
) {
    let Some((low, high)) =
        clip.audible_notes()
            .map(|n| n.pitch.get())
            .fold(None, |range: Option<(u8, u8)>, pitch| match range {
                None => Some((pitch, pitch)),
                Some((low, high)) => Some((low.min(pitch), high.max(pitch))),
            })
    else {
        return;
    };
    // Show at least an octave so a single note doesn't fill the clip.
    let span = (high - low).max(11) as f32 + 1.0;
    let low = low.saturating_sub(((span as u8).saturating_sub(high - low + 1)) / 2);
    let row = rect.height() / span;
    let origin = clip.start - clip.offset;
    // One mesh for the whole clip: thousands of notes as shapes of their
    // own are most of what a long song costs to paint.
    let mut mesh = Mesh::default();
    let visible = painter.clip_rect();
    for note in clip.audible_notes() {
        let start = viewport.x((origin + note.start).get() as f64);
        if start > visible.right() {
            continue;
        }
        let end = viewport
            .x((origin + note.end()).min(clip.end()).get() as f64)
            .max(start + 1.0);
        if end < visible.left() {
            continue;
        }
        let y = rect.bottom() - (note.pitch.get().saturating_sub(low) as f32 + 1.0) * row;
        let note_rect = Rect::from_min_max(pos2(start, y), pos2(end, y + row.max(1.5)));
        mesh.add_colored_rect(note_rect, ink);
    }
    if !mesh.is_empty() {
        painter.add(mesh);
    }
    let _ = project;
}

fn paint_waveform(
    painter: &Painter,
    rect: Rect,
    clip: &AudioClip,
    ink: Color32,
    session: &Session,
    viewport: &Viewport,
) {
    let project = &session.project;
    if project.source(clip.source).is_none() {
        return;
    }
    let source_state = session.source_state(clip.source);
    let peaks = match source_state {
        Some(SourceState::Ready(loaded)) => &loaded.peaks,
        Some(SourceState::Failed(err)) => {
            painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(200, 40, 40, 60));
            painter.text(
                rect.left_center() + [4.0, 0.0].into(),
                Align2::LEFT_CENTER,
                format!("can't load: {err}"),
                FontId::proportional(text::SMALL),
                ink,
            );
            return;
        }
        _ => {
            painter.text(
                rect.left_center() + [4.0, 0.0].into(),
                Align2::LEFT_CENTER,
                "loading…",
                FontId::proportional(text::SMALL),
                ink,
            );
            return;
        }
    };

    let polarity = if clip.polarity_inverted { -1.0 } else { 1.0 };
    let gain = dsp::db_to_gain(clip.gain_db) * polarity;
    let mid = rect.center().y;
    let half = rect.height() / 2.0;
    let visible = rect.intersect(painter.clip_rect());
    if !visible.is_positive() {
        return;
    }
    // Columns map to stretches of material; reversing and looping decide
    // where in the file those are.
    let start = clip.start.get() as f64;
    let material_at = |x: f32| project.audio_material(clip, viewport.tick_at(x) - start);
    let played = clip.played() as f64;
    let mut mesh = Mesh::default();
    let mut x = visible.left().floor();
    while x < visible.right() {
        let (from, to) = (material_at(x).max(0.0), material_at(x + 1.0).min(played));
        if to > from {
            let (low, high) = material_peaks(peaks, clip, from, to);
            let gain = gain * fade_gain(clip, from);
            let (a, b) = (high * gain, low * gain);
            let top = mid - a.max(b).clamp(-1.0, 1.0) * half;
            let bottom = mid - a.min(b).clamp(-1.0, 1.0) * half;
            let column = Rect::from_min_max(
                pos2(x, top.min(mid - 0.5)),
                pos2(x + 1.0, bottom.max(mid + 0.5)),
            );
            mesh.add_colored_rect(column, ink);
        }
        x += 1.0;
    }
    painter.add(mesh);

    // Where each repeat of a loop begins.
    if clip.looped {
        let len = clip.source_len.max(1) as f64;
        let mut seam = len - clip.loop_phase as f64 % len;
        let line = Stroke::new(1.0, ink.gamma_multiply(0.45));
        while seam < played {
            let x = viewport.x(start + project.audio_ticks(clip, seam));
            if x > visible.right() {
                break;
            }
            if x >= visible.left() {
                painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], line);
            }
            seam += len;
        }
    }
}

/// The lowest and highest sample a clip plays between material positions
/// `from..to`, wherever in its file reversing and looping put them.
fn material_peaks(peaks: &media::Peaks, clip: &AudioClip, from: f64, to: f64) -> (f32, f32) {
    let len = clip.source_len.max(1) as f64;
    // Spans of the region, measured from its start in playing order.
    let spans = if !clip.looped {
        [(from, to), (0.0, 0.0)]
    } else if to - from >= len {
        [(0.0, len), (0.0, 0.0)]
    } else {
        let a = (clip.loop_phase as f64 + from).rem_euclid(len);
        let b = a + (to - from);
        if b <= len {
            [(a, b), (0.0, 0.0)]
        } else {
            [(a, len), (0.0, b - len)]
        }
    };
    let offset = clip.source_offset as f64;
    let (mut low, mut high) = (f32::MAX, f32::MIN);
    for (a, b) in spans.into_iter().filter(|(a, b)| b > a) {
        let (a, b) = if clip.reversed {
            (len - b, len - a)
        } else {
            (a, b)
        };
        let (lo, hi) = peaks.min_max(offset + a, offset + b);
        low = low.min(lo);
        high = high.max(hi);
    }
    if low > high { (0.0, 0.0) } else { (low, high) }
}

/// How loud the clip's fades leave the material `material` frames in,
/// `0..=1`.
pub fn fade_gain(clip: &AudioClip, material: f64) -> f32 {
    let into = material;
    let left = clip.played() as f64 - material;
    let mut gain = 1.0;
    if clip.fade_in > 0 && into < clip.fade_in as f64 {
        gain *= dsp::stage_shape((into / clip.fade_in as f64) as f32, clip.fade_in_curve);
    }
    if clip.fade_out > 0 && left < clip.fade_out as f64 {
        gain *= dsp::stage_shape((left / clip.fade_out as f64) as f32, clip.fade_out_curve);
    }
    gain
}

/// Where an audio clip's fade and volume handles are on screen.
#[derive(Debug, Clone, Copy)]
pub struct AudioHandles {
    /// Below the title strip.
    pub content: Rect,
    pub fade_in_x: f32,
    pub fade_out_x: f32,
    /// The middle of each fade's curve, dragged to bend it. `None` when the
    /// fade is too short on screen to grab.
    pub fade_in_curve: Option<Pos2>,
    pub fade_out_curve: Option<Pos2>,
    pub volume: Pos2,
}

/// Handles only show on clips at least this wide.
const MIN_HANDLE_WIDTH: f32 = 36.0;

impl AudioHandles {
    pub fn new(
        rect: Rect,
        clip: &AudioClip,
        project: &Project,
        viewport: &Viewport,
    ) -> Option<AudioHandles> {
        if rect.width() < MIN_HANDLE_WIDTH {
            return None;
        }
        project.source(clip.source)?;
        let px = |frames: u64| viewport.width_of(project.audio_ticks(clip, frames as f64));
        let content = Rect::from_min_max(pos2(rect.left(), rect.top() + TITLE_HEIGHT), rect.max);
        let fade_in_x = (rect.left() + px(clip.fade_in)).min(rect.right());
        let fade_out_x = (rect.right() - px(clip.fade_out)).max(rect.left());
        let level_y =
            |curve: f32| content.bottom() - dsp::stage_shape(0.5, curve) * content.height();
        let curve_dot = |a: f32, b: f32, curve: f32| {
            ((b - a).abs() > 14.0).then(|| pos2((a + b) / 2.0, level_y(curve)))
        };
        Some(AudioHandles {
            content,
            fade_in_x,
            fade_out_x,
            fade_in_curve: curve_dot(rect.left(), fade_in_x, clip.fade_in_curve),
            fade_out_curve: curve_dot(fade_out_x, rect.right(), clip.fade_out_curve),
            volume: pos2(rect.center().x, content.top() + 5.0),
        })
    }

    pub fn fade_in_handle(&self) -> Pos2 {
        pos2(self.fade_in_x, self.content.top() + 4.0)
    }

    pub fn fade_out_handle(&self) -> Pos2 {
        pos2(self.fade_out_x, self.content.top() + 4.0)
    }

    fn paint(&self, painter: &Painter, color: Color32) {
        let ink = Color32::WHITE;
        for center in [self.fade_in_handle(), self.fade_out_handle()] {
            let square = Rect::from_center_size(center, egui::vec2(7.0, 7.0));
            painter.rect_filled(square, 1.0, ink);
            painter.rect_stroke(
                square,
                1.0,
                Stroke::new(1.0, Color32::from_black_alpha(180)),
                StrokeKind::Outside,
            );
        }
        for dot in [self.fade_in_curve, self.fade_out_curve]
            .into_iter()
            .flatten()
        {
            painter.circle(dot, 3.5, color, Stroke::new(1.5, ink));
        }
        // The volume handle: a small bar to drag up and down.
        let bar = Rect::from_center_size(self.volume, egui::vec2(14.0, 5.0));
        painter.rect_filled(bar, 2.0, ink);
        painter.rect_stroke(
            bar,
            2.0,
            Stroke::new(1.0, Color32::from_black_alpha(180)),
            StrokeKind::Outside,
        );
    }
}

/// Darkens what each fade takes away and draws its curve.
fn paint_fades(painter: &Painter, handles: &AudioHandles, clip: &AudioClip) {
    let content = handles.content;
    let shade = Color32::from_black_alpha(110);
    let line = Color32::from_white_alpha(200);
    let fades = [
        (
            content.left(),
            handles.fade_in_x,
            clip.fade_in,
            clip.fade_in_curve,
            true,
        ),
        (
            handles.fade_out_x,
            content.right(),
            clip.fade_out,
            clip.fade_out_curve,
            false,
        ),
    ];
    for (from, to, frames, curve, rising) in fades {
        if frames == 0 || to - from < 1.0 {
            continue;
        }
        let level = |x: f32| {
            let t = (x - from) / (to - from);
            dsp::stage_shape(if rising { t } else { 1.0 - t }, curve)
        };
        let y_of = |x: f32| content.bottom() - level(x) * content.height();
        let mut mesh = Mesh::default();
        let mut x = from.floor();
        while x < to {
            mesh.add_colored_rect(
                Rect::from_min_max(pos2(x, content.top()), pos2(x + 1.0, y_of(x + 0.5))),
                shade,
            );
            x += 1.0;
        }
        painter.add(mesh);
        let steps = 24;
        let points = (0..=steps)
            .map(|i| {
                let x = from + (to - from) * i as f32 / steps as f32;
                pos2(x, y_of(x))
            })
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(1.2, line)));
    }
}
