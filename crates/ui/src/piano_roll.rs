//! The piano roll: the bottom detail view for the selected MIDI clip.
//!
//! It shares the arrangement's time axis (timeline ticks), so its bars line
//! up with the ruler above. Each gesture commits one `set_notes` edit when
//! the mouse button is released.

use std::collections::HashMap;

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke,
    StrokeKind, pos2, vec2,
};
use midi::{NoteEventKind, Pitch, Tick, Velocity};
use project::{ClipId, ClipRef, MidiClip, Note, NoteId, TrackId, TrackKind};

use crate::session::Session;
use crate::theme::{self, color, radius, size, space, stroke, text};
use crate::timeline::{
    Glide, Grid, GridSize, Ruler, RulerAction, Viewport, WheelZoom, color32, paint_lines,
};
use crate::widgets::chrome::{self, Icon};

mod pattern;
mod tools;

const KEYBOARD_WIDTH: f32 = 58.0;
/// Wider, for a drum kit's pad names.
const NAMED_KEYBOARD_WIDTH: f32 = 84.0;
const RULER_HEIGHT: f32 = size::RULER;
const VELOCITY_HEIGHT: f32 = 52.0;
const MIN_KEY_HEIGHT: f32 = 4.0;
const MAX_KEY_HEIGHT: f32 = 40.0;
/// Grab zone at a note's right end for resizing.
const RESIZE_EDGE: f32 = 5.0;

#[derive(Debug, Clone)]
enum Gesture {
    Move {
        grabbed: NoteId,
        anchor: (f64, i32),
        original: Vec<Note>,
        delta: (Tick, i32),
    },
    Resize {
        grabbed: NoteId,
        anchor: f64,
        original: Vec<Note>,
        delta: Tick,
    },
    Band {
        origin: Pos2,
        current: Pos2,
        base: Vec<NoteId>,
    },
    Velocity {
        original: Vec<Note>,
        grabbed: NoteId,
        anchor_y: f32,
        delta: f32,
    },
}

pub struct PianoRoll {
    viewport: Viewport,
    pub grid: Grid,
    key_height: f32,
    /// Pixels scrolled down from the top of the highest key.
    scroll_y: f32,
    selection: Vec<NoteId>,
    gesture: Option<Gesture>,
    shown: Option<ClipId>,
    /// Where the shown clip started last frame, to follow it when it moves.
    shown_start: Tick,
    /// Length new notes get: the last one drawn or resized.
    note_len: Tick,
    /// Copied notes, starting from zero.
    clipboard: Vec<Note>,
    /// Where pasted notes go: the last click on empty space, in content ticks.
    cursor: Option<Tick>,
    previewing: Option<(TrackId, Pitch)>,
    /// True when keyboard shortcuts should go here rather than the timeline.
    pub focused: bool,
    /// The Tools menu's settings and window, and the chord stamp.
    pub tools: tools::Tools,
    rect: Rect,
    /// Where the notes and the velocity stems were drawn this frame.
    notes_area: Rect,
    velocity_area: Rect,
    /// A zoom to fit in progress, easing the view there.
    zoom: Option<ZoomTo>,
    /// Ctrl+wheel zoom still easing in.
    wheel_zoom: WheelZoom,
    /// How wide the keyboard was drawn this frame.
    keyboard_width: f32,
    /// Where each clip was viewed last, to come back to it: the view, the
    /// keys' height and scroll, and where the clip started then.
    views: HashMap<ClipId, SavedView>,
}

/// A clip's view in the piano roll, kept while another clip is open.
#[derive(Debug, Clone, Copy)]
struct SavedView {
    viewport: Viewport,
    key_height: f32,
    scroll_y: f32,
    clip_start: Tick,
}

/// A zoom to fit on its way: time glides, and the keys' height and scroll
/// go along, `(from, to)`.
#[derive(Debug, Clone, Copy)]
struct ZoomTo {
    time: Glide,
    key_height: (f32, f32),
    scroll_y: (f32, f32),
}

impl Default for PianoRoll {
    fn default() -> Self {
        PianoRoll {
            viewport: Viewport::default(),
            grid: Grid::default(),
            key_height: 12.0,
            scroll_y: 0.0,
            selection: Vec::new(),
            gesture: None,
            shown: None,
            shown_start: Tick::ZERO,
            note_len: Tick::new(240),
            clipboard: Vec::new(),
            cursor: None,
            previewing: None,
            focused: false,
            tools: tools::Tools::default(),
            rect: Rect::NOTHING,
            notes_area: Rect::NOTHING,
            velocity_area: Rect::NOTHING,
            zoom: None,
            wheel_zoom: WheelZoom::default(),
            keyboard_width: KEYBOARD_WIDTH,
            views: HashMap::new(),
        }
    }
}

/// The clip being shown, copied out of the project for this frame.
struct Shown {
    id: ClipId,
    track: TrackId,
    clip: MidiClip,
    color: Color32,
}

impl Shown {
    /// Timeline tick of the clip's content position zero.
    fn origin(&self) -> Tick {
        self.clip.start - self.clip.offset
    }
}

impl PianoRoll {
    pub fn contains(&self, pos: Pos2) -> bool {
        self.rect.contains(pos)
    }

    #[cfg(test)]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Screen position of the middle of `pitch`'s row at timeline tick `tick`.
    #[cfg(test)]
    pub fn point(&self, tick: f64, pitch: u8) -> Pos2 {
        let area = Rect::from_min_max(
            pos2(
                self.rect.left() + self.keyboard_width,
                self.rect.top() + RULER_HEIGHT,
            ),
            self.rect.max,
        );
        pos2(
            self.viewport.x(tick),
            self.y_of(area, pitch) + self.key_height / 2.0,
        )
    }

    /// A point on the ruler at timeline tick `tick`: in the loop brace along
    /// its top, or below it.
    #[cfg(test)]
    pub fn ruler_point(&self, tick: f64, brace: bool) -> Pos2 {
        let y = if brace {
            size::LOOP_BRACE / 2.0
        } else {
            RULER_HEIGHT - 6.0
        };
        pos2(self.viewport.x(tick), self.rect.top() + y)
    }

    #[cfg(test)]
    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// `keys` is false while a dialog is up, so its keys aren't taken here.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        clip: Option<ClipId>,
        keys: bool,
    ) {
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        self.rect = rect;
        let shown = clip.and_then(|id| {
            let (track, ClipRef::Midi(midi)) = session.project.clip(id)? else {
                return None;
            };
            let color = color32(session.project.track(track)?.color);
            Some(Shown {
                id,
                track,
                clip: midi.clone(),
                color,
            })
        });
        let Some(shown) = shown else {
            chrome::empty_hint(
                ui.painter(),
                rect,
                "No MIDI clip open",
                "Double-click a MIDI clip to edit its notes here, or an empty MIDI lane to make one",
            );
            self.shown = None;
            return;
        };

        // A drum kit's keys are its pads, by name: room for them.
        let named = note_names(session, shown.track)
            .is_some_and(|names| names.iter().flatten().any(|name| !name.is_empty()));
        self.keyboard_width = if named {
            NAMED_KEYBOARD_WIDTH
        } else {
            KEYBOARD_WIDTH
        };
        let keys_rect = Rect::from_min_max(
            pos2(rect.left(), rect.top() + RULER_HEIGHT),
            pos2(
                rect.left() + self.keyboard_width,
                rect.bottom() - VELOCITY_HEIGHT,
            ),
        );
        let ruler = Rect::from_min_max(
            pos2(keys_rect.right(), rect.top()),
            pos2(rect.right(), keys_rect.top()),
        );
        let notes_rect = Rect::from_min_max(
            pos2(keys_rect.right(), keys_rect.top()),
            pos2(rect.right(), keys_rect.bottom()),
        );
        let velocity = Rect::from_min_max(pos2(keys_rect.right(), keys_rect.bottom()), rect.max);
        self.viewport
            .set_screen(notes_rect.left(), notes_rect.width());
        self.viewport.min_start = (shown.origin().get() as f64).min(0.0);

        if self.shown != Some(shown.id) {
            // Keep where the last clip was viewed, and go back to where
            // this one was, or else frame it.
            if let Some(old) = self.shown {
                self.views.insert(
                    old,
                    SavedView {
                        viewport: self.viewport,
                        key_height: self.key_height,
                        scroll_y: self.scroll_y,
                        clip_start: self.shown_start,
                    },
                );
            }
            self.shown = Some(shown.id);
            self.selection.clear();
            self.gesture = None;
            self.zoom = None;
            self.wheel_zoom.stop();
            match self.views.get(&shown.id) {
                Some(saved) => {
                    self.viewport.start = saved.viewport.start;
                    self.viewport.px_per_tick = saved.viewport.px_per_tick;
                    // If the clip has moved since, the view moves with it.
                    self.viewport
                        .shift((shown.clip.start - saved.clip_start).get() as f64);
                    self.key_height = saved.key_height;
                    self.scroll_y = saved.scroll_y;
                }
                None => {
                    // An empty clip opens where its instrument plays.
                    let fallback = fallback_center(session, shown.track);
                    self.frame_clip(&shown, notes_rect.height(), fallback);
                }
            }
        } else if shown.clip.start != self.shown_start {
            // The clip moved in the arrangement: scroll along with it.
            self.viewport
                .shift((shown.clip.start - self.shown_start).get() as f64);
        }
        self.shown_start = shown.clip.start;

        self.glide(ui);
        self.handle_wheel(ui, rect, notes_rect);
        if self
            .wheel_zoom
            .step(&mut self.viewport, ui.input(|i| i.stable_dt))
        {
            ui.ctx().request_repaint();
        }
        let step = self.grid.step(&self.viewport, &session.project.tempo);

        // Notes as they'd be if the current gesture ended now.
        let notes = self.preview_notes(&shown.clip.notes);

        self.notes_area = notes_rect;
        self.velocity_area = velocity.shrink2(vec2(0.0, 4.0));
        self.ruler(ui, ruler, session, step);
        self.paint_grid(ui, notes_rect, &shown, session, step);
        self.keyboard(ui, keys_rect, session, &shown);
        self.notes(ui, notes_rect, session, &shown, &notes, step);
        self.velocity_lane(ui, velocity, &shown, &notes);
        self.drive_gesture(ui, session, &shown, &notes, step);
        if keys {
            self.keyboard_shortcuts(ui, session, &shown);
        }
        self.tool_window(ui.ctx(), session);
        if ui.input(|i| i.pointer.any_released())
            && let Some((track, pitch)) = self.previewing.take()
        {
            session.preview_note(
                track,
                NoteEventKind::Off {
                    pitch,
                    velocity: Velocity::CENTER,
                },
            );
        }
    }

    /// Zooms to the clip and scrolls to its notes (or to `fallback`, a
    /// pitch, when it has none).
    fn frame_clip(&mut self, shown: &Shown, height: f32, fallback: f32) {
        self.viewport
            .fit(shown.clip.start.get() as f64, shown.clip.end().get() as f64);
        let pitches = shown.clip.notes.iter().map(|n| n.pitch.get() as f32);
        let (low, high) = pitches.fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p), hi.max(p)));
        let center = if low <= high {
            (low + high) / 2.0
        } else {
            fallback
        };
        self.scroll_y = (127.0 - center + 0.5) * self.key_height - height / 2.0;
    }

    fn y_of(&self, area: Rect, pitch: u8) -> f32 {
        area.top() + (127 - pitch) as f32 * self.key_height - self.scroll_y
    }

    fn pitch_at(&self, area: Rect, y: f32) -> i32 {
        127 - ((y - area.top() + self.scroll_y) / self.key_height).floor() as i32
    }

    /// Moves a zoom to fit along, asking for frames until it lands.
    fn glide(&mut self, ui: &egui::Ui) {
        let Some(zoom) = self.zoom else {
            return;
        };
        let t = zoom.time.progress(ui.input(|i| i.time));
        zoom.time.apply(&mut self.viewport, t);
        let lerp = |(from, to): (f32, f32)| from + (to - from) * t;
        self.key_height = lerp(zoom.key_height);
        self.scroll_y = lerp(zoom.scroll_y);
        if t < 1.0 {
            ui.ctx().request_repaint();
        } else {
            self.zoom = None;
        }
    }

    /// The open clip, if it's still a MIDI clip.
    fn open_clip<'a>(&self, session: &'a Session) -> Option<&'a MidiClip> {
        match session.project.clip(self.shown?)? {
            (_, ClipRef::Midi(midi)) => Some(midi),
            _ => None,
        }
    }

    /// Zooms to the selected notes, or to the whole clip if none are: their
    /// time fills the view and their keys sit in the middle, a little
    /// taller if there's room. Eases there.
    pub fn zoom_to_fit(&mut self, ui: &egui::Ui, session: &Session) {
        let Some(clip) = self.open_clip(session) else {
            return;
        };
        let origin = clip.start - clip.offset;
        let selected: Vec<&Note> = clip
            .notes
            .iter()
            .filter(|n| self.selection.contains(&n.id))
            .collect();
        let (start, end, notes) = if selected.is_empty() {
            let notes: Vec<&Note> = clip.notes.iter().collect();
            (clip.start, clip.end(), notes)
        } else {
            let start = selected.iter().map(|n| origin + n.start).min();
            let end = selected.iter().map(|n| origin + n.end()).max();
            (
                start.unwrap_or(clip.start),
                end.unwrap_or(clip.end()),
                selected,
            )
        };
        let target = self
            .viewport
            .fitted(start.get() as f64, end.get().max(start.get() + 1) as f64);
        let height = self.notes_area.height().max(1.0);
        let (low, high) = notes
            .iter()
            .map(|n| n.pitch.get() as f32)
            .fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p), hi.max(p)));
        let (key_height, center) = if low <= high {
            // Room for a few keys around them, never bigger than comfortable.
            let fit = height / (high - low + 7.0);
            (fit.clamp(MIN_KEY_HEIGHT, 20.0), (low + high) / 2.0)
        } else {
            let center = 127.0 - (self.scroll_y + height / 2.0) / self.key_height;
            (self.key_height, center)
        };
        let max = (128.0 * key_height - height).max(0.0);
        let scroll_y = ((127.0 - center + 0.5) * key_height - height / 2.0).clamp(0.0, max);
        self.wheel_zoom.stop();
        self.zoom = Some(ZoomTo {
            time: Glide::new(&self.viewport, &target, ui.input(|i| i.time)),
            key_height: (self.key_height, key_height),
            scroll_y: (self.scroll_y, scroll_y),
        });
        ui.ctx().request_repaint();
    }

    /// L in the piano roll: turns looping off, or on. Turning it on loops
    /// the loop if it's over the clip, or else the clip, and starts the
    /// playhead inside it.
    pub fn toggle_loop(&mut self, session: &mut Session) {
        let Some(clip) = self.open_clip(session) else {
            session.toggle_loop();
            return;
        };
        let (start, end) = (clip.start, clip.end());
        match session.project.loop_region {
            Some(region) if region.on => session.set_loop(Some(project::LoopRegion {
                on: false,
                ..region
            })),
            Some(region) if region.start < end && region.end > start => {
                session.set_loop(Some(project::LoopRegion { on: true, ..region }));
                session.enter_loop();
            }
            _ => session.loop_span(start, end),
        }
    }

    /// Shift+L in the piano roll: loops the selected notes (out to the
    /// grid), or the whole clip if none are selected.
    pub fn loop_selection(&mut self, session: &mut Session) {
        let Some(clip) = self.open_clip(session) else {
            return;
        };
        let origin = clip.start - clip.offset;
        let selected = clip.notes.iter().filter(|n| self.selection.contains(&n.id));
        let span = selected.fold(None, |span: Option<(Tick, Tick)>, n| {
            let (start, end) = (origin + n.start, origin + n.end());
            Some(span.map_or((start, end), |(a, b)| (a.min(start), b.max(end))))
        });
        let (start, end) = match span {
            Some((start, end)) => {
                let step = self
                    .grid
                    .step(&self.viewport, &session.project.tempo)
                    .get()
                    .max(1);
                let floor = start.get().div_euclid(step) * step;
                let ceil = (end.get() + step - 1).div_euclid(step) * step;
                (Tick::new(floor.max(0)), Tick::new(ceil))
            }
            None => (clip.start, clip.end()),
        };
        session.loop_span(start, end);
    }

    /// The piano roll's controls, in the detail view's bar: its grid and
    /// snap, looping, zoom to fit, the chord stamp and the tools.
    pub fn toolbar(&mut self, ui: &mut egui::Ui, session: &mut Session) {
        ui.label("Grid");
        egui::ComboBox::from_id_salt("piano_grid")
            .selected_text(self.grid.size.label())
            .width(80.0)
            .show_ui(ui, |ui| {
                for size in GridSize::ALL {
                    ui.selectable_value(&mut self.grid.size, size, size.label());
                }
            });
        if chrome::switch(ui, self.grid.snap, "Snap", "Snap to the grid", "Ctrl+4").clicked() {
            self.grid.snap = !self.grid.snap;
        }
        ui.separator();
        let looping = session.project.loop_region.is_some_and(|r| r.on);
        if chrome::switch(
            ui,
            looping,
            "Loop",
            "Loop the clip while you edit it (drag along the ruler's top to set where)",
            "L",
        )
        .clicked()
        {
            self.toggle_loop(session);
        }
        let fit = chrome::icon_button(ui, Icon::Fit, "Zoom to fit notes", "");
        let fit = chrome::shortcut_tip(fit, "Zoom to the selected notes, or the clip", "Z");
        if fit.clicked() {
            self.zoom_to_fit(ui, session);
        }
        ui.separator();
        self.tools_menu(ui, session);
    }

    fn handle_wheel(&mut self, ui: &egui::Ui, rect: Rect, notes: Rect) {
        let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        if !rect.contains(pointer) {
            return;
        }
        let (zoom, scroll, modifiers) =
            ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers));
        if zoom != 1.0 || scroll != egui::Vec2::ZERO {
            // The wheel takes over from a zoom to fit.
            self.zoom = None;
        }
        let pointer_x = pointer.x.max(notes.left());
        if modifiers.alt {
            // Alt+wheel (which egui reports as a vertical scroll) zooms the
            // keys, keeping the one under the pointer still.
            if scroll.y != 0.0 {
                let anchor = (pointer.y - notes.top() + self.scroll_y) / self.key_height;
                let factor = (scroll.y / 300.0).exp();
                self.key_height = (self.key_height * factor).clamp(MIN_KEY_HEIGHT, MAX_KEY_HEIGHT);
                self.scroll_y = anchor * self.key_height - (pointer.y - notes.top());
            }
        } else {
            if zoom != 1.0 {
                self.wheel_zoom.add(zoom as f64, pointer_x);
            }
            self.viewport.scroll_by_px(scroll.x);
            self.scroll_y -= scroll.y;
        }
        let max = 128.0 * self.key_height - notes.height();
        self.scroll_y = self.scroll_y.clamp(0.0, max.max(0.0));
    }

    fn preview_notes(&self, notes: &[Note]) -> Vec<Note> {
        match &self.gesture {
            Some(Gesture::Move {
                original, delta, ..
            }) => {
                let mut notes = notes.to_vec();
                for note in &mut notes {
                    if let Some(orig) = original.iter().find(|o| o.id == note.id) {
                        note.start = orig.start + delta.0;
                        note.pitch = shift_pitch(orig.pitch, delta.1);
                    }
                }
                notes
            }
            Some(Gesture::Resize {
                original, delta, ..
            }) => {
                let mut notes = notes.to_vec();
                for note in &mut notes {
                    if let Some(orig) = original.iter().find(|o| o.id == note.id) {
                        note.len = (orig.len + *delta).max(Tick::new(1));
                    }
                }
                notes
            }
            Some(Gesture::Velocity {
                original, delta, ..
            }) => {
                let mut notes = notes.to_vec();
                for note in &mut notes {
                    if let Some(orig) = original.iter().find(|o| o.id == note.id) {
                        note.velocity = shift_velocity(orig.velocity, *delta);
                    }
                }
                notes
            }
            _ => notes.to_vec(),
        }
    }

    /// The same ruler as the arrangement's, on the same ticks: it moves the
    /// playhead and sets the project's loop, which the brace shows here as
    /// over the arrangement.
    fn ruler(&mut self, ui: &mut egui::Ui, rect: Rect, session: &mut Session, step: Tick) {
        let ruler = Ruler {
            id: egui::Id::new("piano_ruler"),
            viewport: &self.viewport,
            grid: &self.grid,
            step,
            tempo: &session.project.tempo,
            region: session.project.loop_region,
            playhead: session.playhead(),
            marker: None,
        };
        let action = ruler.show(ui, rect);
        match action {
            Some(RulerAction::Seek(tick)) => session.seek(tick),
            Some(RulerAction::SetLoop(region)) => {
                let was_on = session.project.loop_region.is_some_and(|r| r.on);
                session.set_loop(Some(region));
                // Clicking the brace on asks to hear the loop, as L does.
                if region.on && !was_on && !ui.input(|i| i.pointer.primary_down()) {
                    session.enter_loop();
                }
            }
            // Drawing or moving the loop here is asking to hear it: if the
            // playhead is outside it, it starts at the loop, playing or not.
            Some(RulerAction::LoopSet) => session.enter_loop(),
            None => {}
        }
        if action.is_some() {
            self.focused = true;
        }
    }

    fn paint_grid(&self, ui: &egui::Ui, rect: Rect, shown: &Shown, session: &Session, step: Tick) {
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        let dark = visuals.extreme_bg_color;
        let light = dark.lerp_to_gamma(visuals.panel_fill, 0.6);
        // With a scale, its notes are the light rows and its root stands out;
        // without one, rows follow the keyboard.
        let scale = session.project.scale;
        let root = light.lerp_to_gamma(shown.color, 0.22);
        // A drum kit's notes are its pads: those rows are light.
        let names = note_names(session, shown.track);
        for pitch in 0..=127u8 {
            let y = self.y_of(rect, pitch);
            if y > rect.bottom() || y + self.key_height < rect.top() {
                continue;
            }
            let row = Rect::from_min_max(
                pos2(rect.left(), y),
                pos2(rect.right(), y + self.key_height),
            );
            let note = Pitch::new(pitch).expect("0..=127");
            let fill = match (names.as_ref(), scale) {
                (Some(names), _) if names[usize::from(pitch)].is_some() => light,
                (Some(_), _) => dark,
                (None, Some(scale)) if scale.is_root(note) => root,
                (None, Some(scale)) if scale.contains(note) => light,
                (None, Some(_)) => dark,
                (None, None) if is_black(pitch) => dark,
                (None, None) => light,
            };
            painter.rect_filled(row, 0.0, fill);
            if pitch % 12 == 0 {
                painter.hline(
                    rect.x_range(),
                    row.bottom(),
                    Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.5)),
                );
            }
        }
        paint_lines(
            &painter,
            rect,
            &self.viewport,
            &session.project.tempo,
            step,
            visuals.weak_text_color().gamma_multiply(0.3),
        );

        // Shade everything outside the clip: those notes don't play.
        let start = self.viewport.x(shown.clip.start.get() as f64);
        let end = self.viewport.x(shown.clip.end().get() as f64);
        if start > rect.left() {
            painter.rect_filled(
                Rect::from_min_max(rect.min, pos2(start, rect.bottom())),
                0.0,
                color::SHADE,
            );
        }
        if end < rect.right() {
            painter.rect_filled(
                Rect::from_min_max(pos2(end, rect.top()), rect.max),
                0.0,
                color::SHADE,
            );
        }
        for x in [start, end] {
            painter.vline(
                x,
                rect.y_range(),
                Stroke::new(stroke::EMPHASIS, shown.color),
            );
        }
        // The loop's bounds, while it's on, as over the arrangement.
        if let Some(region) = session.project.loop_region.filter(|r| r.on) {
            for tick in [region.start, region.end] {
                let x = self.viewport.x(tick.get() as f64);
                painter.vline(
                    x,
                    rect.y_range(),
                    Stroke::new(stroke::HAIRLINE, color::LOOP.gamma_multiply(0.6)),
                );
            }
        }
        let x = self.viewport.x(session.playhead());
        painter.vline(
            x,
            rect.y_range(),
            Stroke::new(stroke::HAIRLINE, color::PLAYHEAD),
        );
    }

    fn keyboard(&mut self, ui: &egui::Ui, rect: Rect, session: &mut Session, shown: &Shown) {
        let painter = ui.painter_at(rect);
        let names = note_names(session, shown.track);
        for pitch in 0..=127u8 {
            let y = self.y_of(rect, pitch);
            if y > rect.bottom() || y + self.key_height < rect.top() {
                continue;
            }
            let row = Rect::from_min_max(
                pos2(rect.left(), y),
                pos2(rect.right(), y + self.key_height),
            );
            let held = self.previewing == Some((shown.track, Pitch::new(pitch).expect("0..=127")))
                || (session.live_target() == Some(shown.track)
                    && session.live_keys()[usize::from(pitch)]);
            // Every row is a white key underneath; black keys sit on top,
            // shorter, as on a real keyboard.
            painter.rect_filled(row, 0.0, Color32::from_gray(228));
            if matches!(pitch % 12, 0 | 5) {
                painter.hline(
                    row.x_range(),
                    row.bottom(),
                    Stroke::new(1.0, Color32::from_gray(150)),
                );
            }
            let key = if is_black(pitch) {
                Rect::from_min_max(row.min, pos2(row.left() + row.width() * 0.62, row.bottom()))
            } else {
                row
            };
            let fill = match (is_black(pitch), held) {
                (_, true) => shown.color,
                (true, false) => Color32::from_gray(35),
                (false, false) => Color32::from_gray(228),
            };
            painter.rect_filled(key, 1.0, fill);
            match names.as_ref() {
                // A drum kit: each pad's name on its key, the rest dimmed.
                Some(names) => match names[usize::from(pitch)] {
                    Some(name) if !name.is_empty() && self.key_height >= 8.0 => {
                        painter.rect_filled(
                            Rect::from_min_max(row.min, pos2(row.right(), row.bottom())),
                            0.0,
                            if held {
                                shown.color
                            } else {
                                Color32::from_gray(228)
                            },
                        );
                        painter.text(
                            pos2(row.left() + 3.0, row.center().y),
                            Align2::LEFT_CENTER,
                            name,
                            FontId::proportional((self.key_height - 2.0).clamp(7.0, text::SMALL)),
                            Color32::from_gray(30),
                        );
                    }
                    Some(_) if pitch % 12 == 0 && self.key_height >= 9.0 => {
                        painter.text(
                            pos2(row.right() - 3.0, row.center().y),
                            Align2::RIGHT_CENTER,
                            note_name(pitch),
                            FontId::proportional((self.key_height - 2.0).min(text::SMALL)),
                            Color32::from_gray(40),
                        );
                    }
                    Some(_) => {}
                    None => {
                        painter.rect_filled(row, 0.0, Color32::from_black_alpha(90));
                    }
                },
                None if pitch % 12 == 0 && self.key_height >= 9.0 => {
                    painter.text(
                        pos2(row.right() - 3.0, row.center().y),
                        Align2::RIGHT_CENTER,
                        note_name(pitch),
                        FontId::proportional((self.key_height - 2.0).min(text::SMALL)),
                        Color32::from_gray(40),
                    );
                }
                None => {}
            }
        }
        let response = ui.interact(rect, egui::Id::new("piano_keys"), Sense::click_and_drag());
        if response.is_pointer_button_down_on()
            && let Some(pos) = response.interact_pointer_pos()
            && let Ok(pitch) = u8::try_from(self.pitch_at(rect, pos.y))
            && let Some(pitch) = Pitch::new(pitch)
        {
            self.focused = true;
            self.audition(session, shown.track, pitch);
        }
    }

    /// Plays `pitch` until the mouse button is released, releasing whatever
    /// was auditioned before.
    fn audition(&mut self, session: &mut Session, track: TrackId, pitch: Pitch) {
        if self.previewing == Some((track, pitch)) {
            return;
        }
        if let Some((old_track, old)) = self.previewing.take() {
            session.preview_note(
                old_track,
                NoteEventKind::Off {
                    pitch: old,
                    velocity: Velocity::CENTER,
                },
            );
        }
        session.preview_note(
            track,
            NoteEventKind::On {
                pitch,
                velocity: Velocity::from_midi1(100),
            },
        );
        self.previewing = Some((track, pitch));
    }

    fn note_rect(&self, area: Rect, origin: Tick, note: &Note) -> Rect {
        let y = self.y_of(area, note.pitch.get());
        let left = self.viewport.x((origin + note.start).get() as f64);
        let right = self
            .viewport
            .x((origin + note.end()).get() as f64)
            .max(left + 3.0);
        Rect::from_min_max(pos2(left, y + 0.5), pos2(right, y + self.key_height - 0.5))
    }

    fn notes(
        &mut self,
        ui: &egui::Ui,
        area: Rect,
        session: &mut Session,
        shown: &Shown,
        notes: &[Note],
        step: Tick,
    ) {
        let origin = shown.origin();
        let modifiers = ui.input(|i| i.modifiers);
        let painter = ui.painter_at(area);

        // Empty space first, so notes drawn on top get the clicks.
        let background = ui.interact(area, egui::Id::new("piano_grid"), Sense::click_and_drag());
        if background.clicked() || background.drag_started() {
            self.focused = true;
        }
        if background.clicked() && !modifiers.command && !modifiers.shift {
            self.selection.clear();
        }
        if (background.clicked() || background.secondary_clicked())
            && let Some(pos) = background.interact_pointer_pos()
        {
            self.cursor = Some(
                self.grid
                    .snap_floor(self.viewport.tick_at(pos.x), step, modifiers.alt)
                    - origin,
            );
        }
        if background.double_clicked()
            && let Some(pos) = background.interact_pointer_pos()
            && let Some(pitch) = u8::try_from(self.pitch_at(area, pos.y))
                .ok()
                .and_then(Pitch::new)
        {
            let tick = self
                .grid
                .snap_floor(self.viewport.tick_at(pos.x), step, modifiers.alt);
            let note = Note {
                id: NoteId(0),
                start: tick - origin,
                len: self.note_len,
                pitch,
                velocity: Velocity::from_midi1(100),
                release_velocity: Velocity::CENTER,
                muted: false,
            };
            // One note, or a chord on it.
            let stamped = self.stamp(&session.project, note);
            let label = if stamped.len() > 1 {
                "Add Chord"
            } else {
                "Add Note"
            };
            let mut all = shown.clip.notes.clone();
            let added = session.edit(label, |p| {
                let mut ids = Vec::new();
                for note in stamped {
                    let id = p.new_note_id();
                    ids.push(id);
                    all.push(Note { id, ..note });
                }
                p.set_notes(shown.id, all)?;
                Ok(ids)
            });
            if let Some(ids) = added {
                self.selection = ids;
            }
            self.audition(session, shown.track, pitch);
        }
        if background.secondary_clicked() {
            self.focused = true;
        }
        background.context_menu(|ui| self.grid_menu(ui, session, shown));
        if background.drag_started()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            let base = if modifiers.command || modifiers.shift {
                self.selection.clone()
            } else {
                Vec::new()
            };
            self.gesture = Some(Gesture::Band {
                origin,
                current: origin,
                base,
            });
        }

        for note in notes {
            let rect = self.note_rect(area, origin, note);
            if !rect.intersects(area) {
                continue;
            }
            let selected = self.selection.contains(&note.id);
            // Selection and hover fade rather than snap.
            let ctx = ui.ctx();
            let lit = theme::fade(ctx, egui::Id::new(("note selected", note.id.0)), selected);
            let hovered = ctx.animate_bool_with_time(
                egui::Id::new(("note hovered", note.id.0)),
                self.gesture.is_none() && ui.rect_contains_pointer(rect),
                theme::anim::FAST,
            );
            let inside =
                (shown.clip.offset..shown.clip.offset + shown.clip.length).contains(&note.start);
            let strength = 0.35 + 0.65 * note.velocity.as_f32() + 0.12 * hovered;
            let mut fill = Color32::from_gray(20).lerp_to_gamma(shown.color, strength.min(1.0));
            if !inside || note.muted {
                fill = fill.gamma_multiply(0.4);
            }
            let corner = egui::CornerRadius::same(radius::SMALL);
            painter.rect_filled(rect, corner, fill);
            let stroke = Stroke::new(
                stroke::HAIRLINE + (stroke::EMPHASIS - stroke::HAIRLINE) * lit,
                color::OUTLINE.lerp_to_gamma(color::SELECTED_OUTLINE, lit),
            );
            painter.rect_stroke(rect, corner, stroke, StrokeKind::Inside);

            let body = ui.interact(
                rect,
                egui::Id::new(("note", note.id.0)),
                Sense::click_and_drag(),
            );
            let edge_rect = Rect::from_min_max(
                pos2(
                    rect.right() - RESIZE_EDGE.min(rect.width() / 3.0),
                    rect.top(),
                ),
                rect.max,
            );
            let edge = ui.interact(
                edge_rect,
                egui::Id::new(("note_end", note.id.0)),
                Sense::drag(),
            );
            if edge.hovered() || edge.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            self.note_response(ui, session, shown, note, &body, &edge, area);
        }

        if let Some(cursor) = self.cursor {
            let x = self.viewport.x((origin + cursor).get() as f64);
            painter.vline(
                x,
                area.y_range(),
                Stroke::new(1.0, ui.visuals().text_color().gamma_multiply(0.5)),
            );
        }
        if let Some(Gesture::Band {
            origin, current, ..
        }) = &self.gesture
        {
            let band = Rect::from_two_pos(*origin, *current);
            let visuals = ui.visuals();
            painter.rect(
                band,
                0.0,
                visuals.selection.bg_fill.gamma_multiply(0.2),
                Stroke::new(1.0, visuals.selection.stroke.color),
                StrokeKind::Inside,
            );
        }
    }

    /// Clicks on a note, and the start of drags on it; `drive_gesture`
    /// takes over from there.
    #[allow(clippy::too_many_arguments)]
    fn note_response(
        &mut self,
        ui: &egui::Ui,
        session: &mut Session,
        shown: &Shown,
        note: &Note,
        body: &egui::Response,
        edge: &egui::Response,
        area: Rect,
    ) {
        let modifiers = ui.input(|i| i.modifiers);
        if body.is_pointer_button_down_on() && self.gesture.is_none() {
            self.focused = true;
            self.audition(session, shown.track, note.pitch);
        }
        if body.clicked() {
            self.select(note.id, modifiers);
        }
        // Double-click, or right-click as in FL Studio, deletes the note.
        if body.double_clicked() || body.secondary_clicked() {
            let remaining: Vec<Note> = shown
                .clip
                .notes
                .iter()
                .filter(|n| n.id != note.id)
                .cloned()
                .collect();
            session.edit("Delete Note", |p| p.set_notes(shown.id, remaining));
            self.selection.retain(|id| *id != note.id);
            return;
        }

        if body.drag_started() || edge.drag_started() {
            if !self.selection.contains(&note.id) {
                self.selection = vec![note.id];
            }
            let original: Vec<Note> = shown
                .clip
                .notes
                .iter()
                .filter(|n| self.selection.contains(&n.id))
                .cloned()
                .collect();
            let press = ui.input(|i| i.pointer.press_origin()).unwrap_or_default();
            let anchor_tick = self.viewport.tick_at(press.x);
            self.gesture = Some(if edge.drag_started() {
                Gesture::Resize {
                    grabbed: note.id,
                    anchor: anchor_tick,
                    original,
                    delta: Tick::ZERO,
                }
            } else {
                Gesture::Move {
                    grabbed: note.id,
                    anchor: (anchor_tick, self.pitch_at(area, press.y)),
                    original,
                    delta: (Tick::ZERO, 0),
                }
            });
        }
    }

    /// Follows the pointer for the drag in progress and commits it on
    /// release, wherever that happens. Escape cancels it.
    fn drive_gesture(
        &mut self,
        ui: &egui::Ui,
        session: &mut Session,
        shown: &Shown,
        notes: &[Note],
        step: Tick,
    ) {
        if self.gesture.is_none() {
            return;
        }
        let (down, pos, modifiers, escape) = ui.input(|i| {
            (
                i.pointer.primary_down(),
                i.pointer.latest_pos(),
                i.modifiers,
                i.key_pressed(Key::Escape),
            )
        });
        if escape {
            self.gesture = None;
            return;
        }
        if !down {
            self.finish_gesture(session, shown);
            return;
        }
        let Some(pos) = pos else {
            return;
        };
        let origin = shown.origin();
        let area = self.notes_area;
        let pointer_pitch = self.pitch_at(area, pos.y);
        let pointer_tick = self.viewport.tick_at(pos.x);
        let bypass = modifiers.alt;
        let snap = |tick: f64| self.grid.snap(tick, step, bypass);
        let min_len = if self.grid.snap && !bypass {
            step
        } else {
            Tick::new(1)
        };
        let mut audition = None;
        match &mut self.gesture {
            Some(Gesture::Move {
                grabbed,
                anchor,
                original,
                delta,
            }) => {
                let Some(grabbed_note) = original.iter().find(|n| n.id == *grabbed) else {
                    return;
                };
                // Snap the distance moved, not the position, so a note that's
                // off the grid stays as far off it, and a purely vertical drag
                // doesn't shift it in time.
                let raw = pointer_tick - anchor.0;
                let moved = if self.grid.snap && !bypass {
                    Tick::new(raw.round() as i64).round_to(step)
                } else {
                    Tick::new(raw.round() as i64)
                };
                // Notes may sit before the clip's content origin when its left
                // edge was dragged out, but not before where the clip starts.
                let floor = shown.clip.offset.min(Tick::ZERO);
                let earliest = original.iter().map(|n| n.start).min().unwrap_or(Tick::ZERO);
                let ticks = moved.max((floor - earliest).min(Tick::ZERO));
                let low = original
                    .iter()
                    .map(|n| n.pitch.get() as i32)
                    .min()
                    .unwrap_or(0);
                let high = original
                    .iter()
                    .map(|n| n.pitch.get() as i32)
                    .max()
                    .unwrap_or(127);
                let pitch = (pointer_pitch - anchor.1).clamp(-low, 127 - high);
                if pitch != delta.1 {
                    audition = Some(shift_pitch(grabbed_note.pitch, pitch));
                }
                *delta = (ticks, pitch);
            }
            Some(Gesture::Resize {
                grabbed,
                anchor,
                original,
                delta,
            }) => {
                let Some(grabbed_note) = original.iter().find(|n| n.id == *grabbed) else {
                    return;
                };
                let end = (origin + grabbed_note.end()).get() as f64 + pointer_tick - *anchor;
                let target_end = snap(end) - origin;
                let shortest = original.iter().map(|n| n.len).min().unwrap_or(Tick::new(1));
                let min_len = min_len.min(grabbed_note.len);
                *delta = (target_end - grabbed_note.end()).max(min_len - shortest);
            }
            Some(Gesture::Band {
                origin: band_origin,
                current,
                base,
            }) => {
                *current = pos;
                let band = Rect::from_two_pos(*band_origin, pos);
                let mut selection = base.clone();
                for note in notes {
                    let y = area.top() + (127 - note.pitch.get()) as f32 * self.key_height
                        - self.scroll_y;
                    let left = self.viewport.x((origin + note.start).get() as f64);
                    let right = self
                        .viewport
                        .x((origin + note.end()).get() as f64)
                        .max(left + 3.0);
                    let rect = Rect::from_min_max(pos2(left, y), pos2(right, y + self.key_height));
                    if rect.intersects(band) && !selection.contains(&note.id) {
                        selection.push(note.id);
                    }
                }
                self.selection = selection;
            }
            Some(Gesture::Velocity {
                anchor_y, delta, ..
            }) => {
                *delta = (*anchor_y - pos.y) / self.velocity_area.height().max(1.0);
            }
            None => {}
        }
        if let Some(pitch) = audition {
            self.audition(session, shown.track, pitch);
        }
    }

    fn finish_gesture(&mut self, session: &mut Session, shown: &Shown) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        let notes = self.preview_notes_with(&shown.clip.notes, &gesture);
        match gesture {
            Gesture::Move { delta, .. } if delta != (Tick::ZERO, 0) => {
                session.edit("Move Notes", |p| p.set_notes(shown.id, notes));
            }
            Gesture::Move {
                grabbed, original, ..
            } => {
                // A click without a drag sets the length new notes get.
                if let Some(note) = original.iter().find(|n| n.id == grabbed) {
                    self.note_len = note.len;
                }
            }
            Gesture::Resize {
                delta,
                grabbed,
                original,
                ..
            } if delta != Tick::ZERO => {
                if let Some(note) = original.iter().find(|n| n.id == grabbed) {
                    self.note_len = (note.len + delta).max(Tick::new(1));
                }
                session.edit("Resize Notes", |p| p.set_notes(shown.id, notes));
            }
            Gesture::Velocity { delta, .. } if delta != 0.0 => {
                session.edit("Change Velocity", |p| p.set_notes(shown.id, notes));
            }
            _ => {}
        }
    }

    fn preview_notes_with(&self, notes: &[Note], gesture: &Gesture) -> Vec<Note> {
        let saved = PianoRoll {
            gesture: Some(gesture.clone()),
            ..PianoRoll::default()
        };
        saved.preview_notes(notes)
    }

    fn select(&mut self, id: NoteId, modifiers: Modifiers) {
        if modifiers.command {
            if let Some(i) = self.selection.iter().position(|n| *n == id) {
                self.selection.remove(i);
            } else {
                self.selection.push(id);
            }
        } else if modifiers.shift {
            if !self.selection.contains(&id) {
                self.selection.push(id);
            }
        } else {
            self.selection = vec![id];
        }
    }

    /// Velocity stems under the notes. Dragging one changes every selected
    /// note by the same amount.
    fn velocity_lane(&mut self, ui: &egui::Ui, rect: Rect, shown: &Shown, notes: &[Note]) {
        let keyboard = self.keyboard_width;
        let painter = ui.painter_at(rect.expand2(vec2(keyboard, 0.0)));
        let visuals = ui.visuals();
        painter.rect_filled(rect, 0.0, visuals.faint_bg_color);
        painter.hline(
            rect.x_range(),
            rect.top() + 0.5,
            visuals.widgets.noninteractive.bg_stroke,
        );
        painter.text(
            pos2(rect.left() - keyboard + space::S, rect.top() + space::S),
            Align2::LEFT_TOP,
            "Velocity",
            FontId::proportional(text::TINY),
            visuals.weak_text_color(),
        );
        let usable = rect.shrink2(vec2(0.0, 4.0));
        let origin = shown.origin();
        for note in notes {
            let x = self.viewport.x((origin + note.start).get() as f64);
            if x < rect.left() - 4.0 || x > rect.right() + 4.0 {
                continue;
            }
            let top = usable.bottom() - note.velocity.as_f32() * usable.height();
            let lit = ui.ctx().animate_bool_with_time(
                egui::Id::new(("note selected", note.id.0)),
                self.selection.contains(&note.id),
                theme::anim::NORMAL,
            );
            let color = shown.color.lerp_to_gamma(color::SELECTED_OUTLINE, lit);
            painter.vline(
                x,
                top..=usable.bottom(),
                Stroke::new(stroke::EMPHASIS, color),
            );
            painter.circle_filled(pos2(x, top), 3.0, color);

            let hit = Rect::from_center_size(pos2(x, top), vec2(9.0, 9.0));
            let response = ui.interact(hit, egui::Id::new(("velocity", note.id.0)), Sense::drag());
            if response.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
            }
            if response.drag_started() {
                if !self.selection.contains(&note.id) {
                    self.selection = vec![note.id];
                }
                let original = shown
                    .clip
                    .notes
                    .iter()
                    .filter(|n| self.selection.contains(&n.id))
                    .cloned()
                    .collect();
                self.gesture = Some(Gesture::Velocity {
                    original,
                    grabbed: note.id,
                    anchor_y: ui.input(|i| i.pointer.press_origin()).map_or(top, |p| p.y),
                    delta: 0.0,
                });
            }
            let dragging = matches!(&self.gesture, Some(Gesture::Velocity { grabbed, .. }) if *grabbed == note.id);
            if dragging {
                painter.text(
                    pos2(x + 6.0, top),
                    Align2::LEFT_CENTER,
                    note.velocity.to_midi1().to_string(),
                    FontId::proportional(text::SMALL),
                    visuals.strong_text_color(),
                );
            }
        }
    }

    fn select_all(&mut self, shown: &Shown) {
        self.selection = shown.clip.notes.iter().map(|n| n.id).collect();
    }

    /// Pastes the copied notes at the cursor (the last click on empty
    /// space), or at the clip's start, and selects them.
    fn paste(&mut self, session: &mut Session, shown: &Shown) {
        if self.clipboard.is_empty() {
            return;
        }
        let at = self.cursor.unwrap_or(shown.clip.offset);
        let mut notes = shown.clip.notes.clone();
        let clipboard = self.clipboard.clone();
        let pasted = session.edit("Paste Notes", |p| {
            let mut ids = Vec::new();
            for note in &clipboard {
                let id = p.new_note_id();
                ids.push(id);
                notes.push(Note {
                    id,
                    start: at + note.start,
                    ..note.clone()
                });
            }
            p.set_notes(shown.id, notes)?;
            Ok(ids)
        });
        if let Some(ids) = pasted {
            // Paste again and the next copy follows on.
            let len = self
                .clipboard
                .iter()
                .map(|n| n.end())
                .max()
                .unwrap_or(Tick::ZERO);
            self.cursor = Some(at + len);
            self.selection = ids;
        }
    }

    fn delete_selected(&mut self, session: &mut Session, shown: &Shown) {
        if self.selection.is_empty() {
            return;
        }
        let remaining: Vec<Note> = shown
            .clip
            .notes
            .iter()
            .filter(|n| !self.selection.contains(&n.id))
            .cloned()
            .collect();
        session.edit("Delete Notes", |p| p.set_notes(shown.id, remaining));
        self.selection.clear();
    }

    /// The right-click menu on empty space in the grid: the same things
    /// the keys and the toolbar do, with their shortcuts.
    fn grid_menu(&mut self, ui: &mut egui::Ui, session: &mut Session, shown: &Shown) {
        let selected = !self.selection.is_empty();
        if chrome::menu_item_enabled(ui, !self.clipboard.is_empty(), "Paste", "Ctrl+V").clicked() {
            self.paste(session, shown);
            ui.close();
        }
        if chrome::menu_item(ui, "Select All", "Ctrl+A").clicked() {
            self.select_all(shown);
            ui.close();
        }
        ui.separator();
        let looping = session.project.loop_region.is_some_and(|r| r.on);
        let label = if looping { "Stop Looping" } else { "Loop" };
        if chrome::menu_item(ui, label, "L").clicked() {
            self.toggle_loop(session);
            ui.close();
        }
        if chrome::menu_item_enabled(ui, selected, "Loop Selection", "Shift+L").clicked() {
            self.loop_selection(session);
            ui.close();
        }
        if chrome::menu_item(ui, "Zoom to Fit", "Z").clicked() {
            self.zoom_to_fit(ui, session);
            ui.close();
        }
        ui.menu_button("Tools", |ui| self.tool_items(ui, session));
        ui.separator();
        if chrome::menu_item_enabled(ui, selected, "Delete Notes", "Del").clicked() {
            self.delete_selected(session, shown);
            ui.close();
        }
    }

    fn keyboard_shortcuts(&mut self, ui: &egui::Ui, session: &mut Session, shown: &Shown) {
        if !self.focused || ui.ctx().text_edit_focused() {
            return;
        }
        let pressed = |key, modifiers| ui.ctx().input_mut(|i| i.consume_key(modifiers, key));
        if pressed(Key::A, Modifiers::COMMAND) {
            self.select_all(shown);
        }
        let (copy, cut, paste) = crate::app::clipboard_events(ui.ctx());
        if (copy || cut) && !self.selection.is_empty() {
            let selected: Vec<Note> = shown
                .clip
                .notes
                .iter()
                .filter(|n| self.selection.contains(&n.id))
                .cloned()
                .collect();
            let earliest = selected.iter().map(|n| n.start).min().unwrap_or(Tick::ZERO);
            self.clipboard = selected
                .into_iter()
                .map(|n| Note {
                    start: n.start - earliest,
                    ..n
                })
                .collect();
            ui.ctx().copy_text("canopy notes".to_owned());
            if cut {
                let remaining: Vec<Note> = shown
                    .clip
                    .notes
                    .iter()
                    .filter(|n| !self.selection.contains(&n.id))
                    .cloned()
                    .collect();
                session.edit("Cut Notes", |p| p.set_notes(shown.id, remaining));
                self.selection.clear();
            }
        }
        if paste {
            self.paste(session, shown);
        }
        if pressed(Key::Escape, Modifiers::NONE) {
            self.selection.clear();
        }
        if pressed(Key::Z, Modifiers::NONE) {
            self.zoom_to_fit(ui, session);
        }
        // Tools work on everything when nothing is selected.
        self.tool_shortcuts(ui, session);
        if self.selection.is_empty() {
            return;
        }
        if pressed(Key::Delete, Modifiers::NONE) || pressed(Key::Backspace, Modifiers::NONE) {
            self.delete_selected(session, shown);
        }
        // An octave with Ctrl, as in FL Studio (or Shift).
        let octave = |key| pressed(key, Modifiers::COMMAND) || pressed(key, Modifiers::SHIFT);
        let transpose = if octave(Key::ArrowUp) {
            12
        } else if octave(Key::ArrowDown) {
            -12
        } else if pressed(Key::ArrowUp, Modifiers::NONE) {
            1
        } else if pressed(Key::ArrowDown, Modifiers::NONE) {
            -1
        } else {
            0
        };
        if transpose != 0 {
            let selected = |n: &&Note| self.selection.contains(&n.id);
            let fits = shown
                .clip
                .notes
                .iter()
                .filter(selected)
                .all(|n| (0..=127).contains(&(n.pitch.get() as i32 + transpose)));
            if fits {
                let notes: Vec<Note> = shown
                    .clip
                    .notes
                    .iter()
                    .map(|n| {
                        let mut n = n.clone();
                        if self.selection.contains(&n.id) {
                            n.pitch = shift_pitch(n.pitch, transpose);
                        }
                        n
                    })
                    .collect();
                session.edit("Transpose", |p| p.set_notes(shown.id, notes));
            }
        }
        let nudge = if pressed(Key::ArrowLeft, Modifiers::NONE) {
            -1
        } else if pressed(Key::ArrowRight, Modifiers::NONE) {
            1
        } else {
            0
        };
        if nudge != 0 {
            let step = self.grid.step(&self.viewport, &session.project.tempo) * nudge;
            let notes: Vec<Note> = shown
                .clip
                .notes
                .iter()
                .map(|n| {
                    let mut n = n.clone();
                    if self.selection.contains(&n.id) {
                        n.start += step;
                    }
                    n
                })
                .collect();
            session.edit("Nudge Notes", |p| p.set_notes(shown.id, notes));
        }
        if pressed(Key::D, Modifiers::COMMAND) {
            // Copies right after the selection, like Ctrl+D in the arrangement.
            let selected: Vec<Note> = shown
                .clip
                .notes
                .iter()
                .filter(|n| self.selection.contains(&n.id))
                .cloned()
                .collect();
            let start = selected.iter().map(|n| n.start).min().unwrap_or(Tick::ZERO);
            let end = selected.iter().map(|n| n.end()).max().unwrap_or(Tick::ZERO);
            let mut notes = shown.clip.notes.clone();
            let copies = session.edit("Duplicate Notes", |p| {
                let mut ids = Vec::new();
                for note in &selected {
                    let id = p.new_note_id();
                    ids.push(id);
                    notes.push(Note {
                        id,
                        start: note.start + (end - start),
                        ..note.clone()
                    });
                }
                p.set_notes(shown.id, notes)?;
                Ok(ids)
            });
            if let Some(copies) = copies {
                self.selection = copies;
            }
        }
    }
}

fn shift_pitch(pitch: Pitch, semitones: i32) -> Pitch {
    Pitch::new((pitch.get() as i32 + semitones).clamp(0, 127) as u8).expect("clamped")
}

fn shift_velocity(velocity: Velocity, delta: f32) -> Velocity {
    let value = (velocity.as_f32() + delta).clamp(1.0 / 127.0, 1.0);
    Velocity::new((value * u16::MAX as f32).round() as u16)
}

fn is_black(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

/// Which keys the track's instrument plays, for instruments that know: a
/// drum kit (each pad's name), or a sampler's loaded instrument (its keys,
/// and "Switch" on keyswitches). `None` for a key that plays nothing; an
/// empty name for one that plays but has no name.
fn note_names(session: &Session, track: TrackId) -> Option<[Option<&'static str>; 128]> {
    use instruments::builtin::{drums, sampler};
    let track_id = track;
    let track = session.project.track(track)?;
    let TrackKind::Midi { instrument, .. } = &track.kind else {
        return None;
    };
    if instrument.id == drums::DESCRIPTOR.id {
        let voices = drums::voices();
        return Some(std::array::from_fn(|note| {
            drums::voice_for_note(note as u8).map(|voice| voices[voice].name)
        }));
    }
    if instrument.id == sampler::DESCRIPTOR.id
        && let Some(crate::banks::BankState::Ready(bank)) = session.bank(track_id)
    {
        let mut keys = [None; 128];
        for region in bank.regions() {
            for key in region.lokey..=region.hikey.min(127) {
                keys[usize::from(key)].get_or_insert("");
            }
            if let Some(switch) = region.keyswitch {
                keys[usize::from(switch.min(127))] = Some("Switch");
            }
        }
        return Some(keys);
    }
    None
}

/// Where an empty clip's view centres: on a drum kit's pads, a sampler's
/// keys, or middle C.
fn fallback_center(session: &Session, track: TrackId) -> f32 {
    let Some(names) = note_names(session, track) else {
        return 60.0;
    };
    let is_drums = session.project.track(track).is_some_and(|t| {
        matches!(&t.kind, TrackKind::Midi { instrument, .. }
            if instrument.id == instruments::builtin::drums::DESCRIPTOR.id)
    });
    if is_drums {
        // Kick to crash.
        return 43.0;
    }
    let mapped: Vec<usize> = (0..128).filter(|k| names[*k].is_some()).collect();
    match (mapped.first(), mapped.last()) {
        (Some(low), Some(high)) => (low + high) as f32 / 2.0,
        _ => 60.0,
    }
}

/// Note names with middle C (60) as C3, as in Ableton.
fn note_name(pitch: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", NAMES[pitch as usize % 12], pitch as i32 / 12 - 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_keys() {
        assert_eq!(note_name(60), "C3");
        assert_eq!(note_name(0), "C-2");
        assert_eq!(note_name(69), "A3");
        assert!(is_black(61) && !is_black(64));
    }

    #[test]
    fn velocity_stays_audible() {
        assert_eq!(shift_velocity(Velocity::MAX, 0.5), Velocity::MAX);
        assert!(shift_velocity(Velocity::MIN, -1.0) > Velocity::MIN);
    }
}
