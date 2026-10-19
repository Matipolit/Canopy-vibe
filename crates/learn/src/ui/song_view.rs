//! Songs: notes fall onto the keyboard, colored by hand. Wait, play along
//! or listen; pick the hands, the speed, a section to loop.

use std::path::{Path, PathBuf};

use eframe::egui::{
    self, Align2, Color32, FontId, Modifiers, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2,
    vec2,
};
use serde::{Deserialize, Serialize};

use super::app::Shared;
use super::piano::{Layout, Look};
use super::{theme, widgets};
use crate::grading::Rating;
use crate::progress::{self, Kind, Outcome};
use crate::song::{self, HandSplit, Hands, Judgment, Mode, NoteState, Output, Player, Song};
use crate::theory::{Hand, is_black, pitch_label};

/// Real seconds of music visible above the keyboard.
const LOOKAHEAD: f64 = 3.0;
const MAX_RECENT: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SongSettings {
    pub mode: Mode,
    pub hands: Hands,
    /// 1.0 as written.
    pub speed: f64,
    /// Note names on the falling notes.
    pub names: bool,
}

impl Default for SongSettings {
    fn default() -> Self {
        SongSettings {
            mode: Mode::Wait,
            hands: Hands::Right,
            speed: 1.0,
            names: true,
        }
    }
}

/// Where the song came from.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Builtin(&'static str),
    File(PathBuf),
}

/// A rating floating up from a key.
struct Popup {
    pitch: u8,
    rating: Rating,
    at: f64,
}

pub struct SongView {
    pub settings: SongSettings,
    pub player: Option<Player>,
    pub source: Option<Source>,
    pub recent: Vec<PathBuf>,
    out: Vec<Output>,
    popups: Vec<Popup>,
    result: Option<String>,
    recorded: bool,
    /// Loop markers, song seconds.
    pub loop_a: Option<f64>,
    pub loop_b: Option<f64>,
    pub looping: bool,
    /// Shift-drag on the progress bar: where it started.
    loop_drag: Option<f64>,
    /// Where the progress bar was drawn.
    pub bar_rect: Option<Rect>,
}

impl SongView {
    pub fn new(settings: SongSettings) -> SongView {
        SongView {
            settings,
            player: None,
            source: None,
            recent: Vec::new(),
            out: Vec::new(),
            popups: Vec::new(),
            result: None,
            recorded: false,
            loop_a: None,
            loop_b: None,
            looping: false,
            loop_drag: None,
            bar_rect: None,
        }
    }

    pub fn title(&self) -> Option<&str> {
        self.player.as_ref().map(|p| p.song().title.as_str())
    }

    fn flush(&mut self, shared: &mut Shared) {
        for output in self.out.drain(..) {
            shared.audio.play(output);
        }
    }

    pub fn open_builtin(&mut self, name: &'static str, shared: &mut Shared) {
        match song::builtin(name) {
            Some(song) => {
                self.set_song(song, shared);
                self.source = Some(Source::Builtin(name));
            }
            None => shared.error(format!("Couldn't build {name}")),
        }
    }

    pub fn open_file(&mut self, path: &Path, shared: &mut Shared) {
        match Song::load(path) {
            Ok(song) => {
                shared.info(format!(
                    "Opened {} · {} notes · {} part{}",
                    song.title,
                    song.notes.len(),
                    song.parts.len(),
                    if song.parts.len() == 1 { "" } else { "s" }
                ));
                self.set_song(song, shared);
                self.source = Some(Source::File(path.to_owned()));
                self.recent.retain(|p| p != path);
                self.recent.insert(0, path.to_owned());
                self.recent.truncate(MAX_RECENT);
            }
            Err(err) => shared.error(format!("Couldn't open {}: {err}", path.display())),
        }
    }

    pub fn open_dialog(&mut self, shared: &mut Shared) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Open a MIDI file")
            .add_filter("MIDI", &["mid", "midi", "kar", "rmi"])
            .pick_file()
        {
            self.open_file(&path, shared);
        }
    }

    fn set_song(&mut self, song: Song, shared: &mut Shared) {
        self.pause(shared);
        let split = song.default_split();
        let mut player = Player::new(song, split, self.settings.mode, self.settings.hands);
        player.speed = self.settings.speed;
        player.restart(&mut self.out);
        self.player = Some(player);
        self.loop_a = None;
        self.loop_b = None;
        self.looping = false;
        self.result = None;
        self.recorded = false;
        self.popups.clear();
        self.flush(shared);
    }

    pub fn is_playing(&self) -> bool {
        self.player.as_ref().is_some_and(Player::is_playing)
    }

    pub fn pause(&mut self, shared: &mut Shared) {
        let now = shared.now();
        if let Some(player) = self.player.as_mut().filter(|p| p.is_playing()) {
            player.set_playing(false, now, &mut self.out);
        }
        self.flush(shared);
        shared.audio.silence_app();
    }

    pub fn toggle(&mut self, shared: &mut Shared) {
        let now = shared.now();
        let Some(player) = self.player.as_mut() else {
            return;
        };
        let playing = !player.is_playing();
        if playing && player.is_finished() {
            self.result = None;
            self.recorded = false;
        }
        player.set_playing(playing, now, &mut self.out);
        self.flush(shared);
        if !playing {
            shared.audio.silence_app();
        }
    }

    pub fn restart(&mut self, shared: &mut Shared) {
        if let Some(player) = self.player.as_mut() {
            player.restart(&mut self.out);
            self.result = None;
            self.recorded = false;
            self.popups.clear();
        }
        self.flush(shared);
    }

    pub fn seek(&mut self, position: f64, shared: &mut Shared) {
        if let Some(player) = self.player.as_mut() {
            player.seek(position, &mut self.out);
            self.result = None;
        }
        self.flush(shared);
    }

    fn apply_settings(&mut self, shared: &mut Shared) {
        if let Some(player) = self.player.as_mut() {
            if player.mode != self.settings.mode || player.hands != self.settings.hands {
                player.set_mode(self.settings.mode, self.settings.hands, &mut self.out);
                self.result = None;
            }
            player.speed = self.settings.speed;
        }
        self.flush(shared);
    }

    fn apply_loop(&mut self, shared: &mut Shared) {
        let range = match (self.looping, self.loop_a, self.loop_b) {
            (true, Some(a), Some(b)) if b > a + 0.1 => Some((a, b)),
            _ => None,
        };
        if let Some(player) = self.player.as_mut()
            && player.looped != range
        {
            player.looped = range;
            if range.is_some() {
                player.restart(&mut self.out);
            }
        }
        self.flush(shared);
    }

    /// What's being practised, for the practice clock.
    pub fn practising(&self) -> Option<String> {
        let player = self.player.as_ref()?;
        player.is_playing().then(|| player.song().title.clone())
    }

    pub fn on_note(&mut self, shared: &mut Shared, pitch: u8, time: f64) {
        let Some(player) = self.player.as_mut() else {
            return;
        };
        let judgment = player.press(pitch, time, &mut self.out);
        match judgment {
            Judgment::Right { rating, .. } => {
                shared.flash(pitch, theme::RIGHT);
                if let Some(rating) = rating {
                    self.popups.push(Popup {
                        pitch,
                        rating,
                        at: time,
                    });
                }
            }
            Judgment::Wrong => shared.flash(pitch, theme::WRONG),
            Judgment::None => {}
        }
        self.flush(shared);
    }

    pub fn update(&mut self, shared: &mut Shared, now: f64) {
        if let Some(player) = self.player.as_mut() {
            player.advance(now, &mut self.out);
        }
        self.flush(shared);
        self.popups.retain(|p| now - p.at < 0.9);
        if self.player.as_ref().is_some_and(Player::is_finished) && !self.recorded {
            self.recorded = true;
            self.finish(shared);
        }
    }

    fn finish(&mut self, shared: &mut Shared) {
        let Some(player) = self.player.as_ref() else {
            return;
        };
        let mode = player.mode;
        let Some(score) = player.stats.accuracy(mode) else {
            self.result = Some("Finished".to_owned());
            return;
        };
        let timing = player.stats.timing.mean_abs();
        let mut text = format!("Accuracy {:.0}%", score * 100.0);
        if let Some(summary) = player.stats.timing.summary() {
            text.push_str(&format!(" · timing {summary}"));
        }
        if player.is_complete_run() {
            let mut variant = format!(
                "{} · {} hand{}",
                mode.label(),
                player.hands.label(),
                if player.hands == Hands::Both { "s" } else { "" }
            );
            if (player.speed - 1.0).abs() > 1e-6 {
                variant.push_str(&format!(" · {:.0}%", player.speed * 100.0));
            }
            let outcome = Outcome {
                score,
                streak: 0,
                timing_ms: timing,
            };
            let date = progress::date_label(progress::today());
            let title = player.song().title.clone();
            if shared
                .progress
                .finish(Kind::Song, &title, &variant, outcome, &date)
            {
                text.push_str(" · New best!");
            }
        }
        self.result = Some(text);
    }

    /// Space: play or pause; Home: from the top; Left/Right: 5 s back or
    /// on; Ctrl+[ / Ctrl+]: loop start / end here; Ctrl+L: loop on/off.
    pub fn shortcuts(&mut self, ctx: &egui::Context, shared: &mut Shared) {
        let key = |m: Modifiers, k: egui::Key| ctx.input_mut(|i| i.consume_key(m, k));
        if key(Modifiers::NONE, egui::Key::Space) {
            self.toggle(shared);
        }
        if key(Modifiers::NONE, egui::Key::Home) {
            self.restart(shared);
        }
        let position = self.player.as_ref().map(Player::position);
        if let Some(position) = position {
            if key(Modifiers::NONE, egui::Key::ArrowLeft) {
                self.seek(position - 5.0, shared);
            }
            if key(Modifiers::NONE, egui::Key::ArrowRight) {
                self.seek(position + 5.0, shared);
            }
            if key(Modifiers::COMMAND, egui::Key::OpenBracket) {
                self.loop_a = Some(position.max(0.0));
                self.apply_loop(shared);
            }
            if key(Modifiers::COMMAND, egui::Key::CloseBracket) {
                self.loop_b = Some(position.max(0.0));
                self.looping = true;
                self.apply_loop(shared);
            }
            if key(Modifiers::COMMAND, egui::Key::L) {
                self.looping = !self.looping;
                self.apply_loop(shared);
            }
        }
    }

    /// The app's notes and the ones to play next, on the keyboard.
    pub fn looks(&self, looks: &mut [Look; 128]) {
        let Some(player) = &self.player else { return };
        for (pitch, hand) in player.sounding() {
            looks[usize::from(pitch)].fill = Some(theme::hand(hand).gamma_multiply(0.8));
        }
        let ahead = 0.12 * player.speed;
        let mut next: Vec<(u8, Hand)> = player
            .waiting_for()
            .into_iter()
            .map(|p| (p, Hand::Right))
            .collect();
        if next.is_empty() {
            next = player.upcoming(ahead);
        } else {
            next = player
                .upcoming(0.0)
                .into_iter()
                .filter(|(p, _)| next.iter().any(|n| n.0 == *p))
                .collect();
        }
        for (pitch, hand) in next {
            looks[usize::from(pitch)].hint = Some(theme::hand(hand));
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, shared: &mut Shared, layout: Option<Layout>) {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                self.toolbar(ui, shared);
                ui.add_space(2.0);
                self.transport(ui, shared);
            });
        let Some(layout) = layout else { return };
        self.lane(ui, shared, &layout);
    }

    fn toolbar(&mut self, ui: &mut Ui, shared: &mut Shared) {
        let before = self.settings.clone();
        ui.horizontal_wrapped(|ui| {
            let title = self.title().unwrap_or("Pick a song").to_owned();
            let mut open_builtin = None;
            let mut open_recent = None;
            let mut open_dialog = false;
            egui::ComboBox::from_id_salt("song")
                .selected_text(RichText::new(title).strong())
                .width(200.0)
                .show_ui(ui, |ui| {
                    for (name, _) in song::BUILTIN {
                        let on = self.source == Some(Source::Builtin(name));
                        if ui.selectable_label(on, *name).clicked() {
                            open_builtin = Some(*name);
                        }
                    }
                    if !self.recent.is_empty() {
                        ui.separator();
                    }
                    for path in &self.recent {
                        let name = path.file_stem().map_or_else(
                            || path.display().to_string(),
                            |s| s.to_string_lossy().into_owned(),
                        );
                        let on = self.source.as_ref() == Some(&Source::File(path.clone()));
                        if ui
                            .selectable_label(on, name)
                            .on_hover_text(path.display().to_string())
                            .clicked()
                        {
                            open_recent = Some(path.clone());
                        }
                    }
                    ui.separator();
                    if ui.button("Open…").clicked() {
                        open_dialog = true;
                    }
                });
            if ui
                .button("Open…")
                .on_hover_text("A MIDI file (Ctrl+O)")
                .clicked()
            {
                open_dialog = true;
            }
            if let Some(name) = open_builtin {
                self.open_builtin(name, shared);
            }
            if let Some(path) = open_recent {
                self.open_file(&path, shared);
            }
            if open_dialog {
                self.open_dialog(shared);
            }
            ui.separator();
            let modes: Vec<(Mode, &str)> = Mode::ALL.iter().map(|m| (*m, m.label())).collect();
            widgets::choice(ui, &mut self.settings.mode, &modes);
            ui.separator();
            ui.add_enabled_ui(self.settings.mode != Mode::Listen, |ui| {
                ui.label(RichText::new("You play").weak());
                let hands: Vec<(Hands, &str)> =
                    Hands::ALL.iter().map(|h| (*h, h.label())).collect();
                widgets::choice(ui, &mut self.settings.hands, &hands);
            });
            self.parts_menu(ui, shared);
            ui.separator();
            let mut percent = self.settings.speed * 100.0;
            if ui
                .add(
                    egui::DragValue::new(&mut percent)
                        .range(25.0..=150.0)
                        .speed(1.0)
                        .update_while_editing(false)
                        .fixed_decimals(0)
                        .suffix("%"),
                )
                .on_hover_text("Speed")
                .changed()
            {
                self.settings.speed = percent / 100.0;
            }
            ui.toggle_value(&mut self.settings.names, "Names")
                .on_hover_text("Note names on the falling notes");
        });
        if self.settings != before {
            self.apply_settings(shared);
        }
    }

    /// Which part each hand plays.
    fn parts_menu(&mut self, ui: &mut Ui, shared: &mut Shared) {
        let Some(player) = self.player.as_ref() else {
            return;
        };
        let song = player.song();
        let mut split = player.split().clone();
        ui.menu_button("Parts", |ui| {
            let mut by_parts = matches!(split, HandSplit::Parts(_));
            if song.parts.len() > 1 {
                ui.horizontal(|ui| {
                    if ui.selectable_label(by_parts, "By part").clicked() {
                        by_parts = true;
                    }
                    if ui.selectable_label(!by_parts, "Split at a key").clicked() {
                        by_parts = false;
                    }
                });
                ui.separator();
            } else {
                by_parts = false;
            }
            if by_parts {
                let mut hands = match &split {
                    HandSplit::Parts(h) => h.clone(),
                    HandSplit::At(_) => match song.default_split() {
                        HandSplit::Parts(h) => h,
                        HandSplit::At(_) => vec![Hand::Right; song.parts.len()],
                    },
                };
                egui::Grid::new("parts").num_columns(3).show(ui, |ui| {
                    for (i, part) in song.parts.iter().enumerate() {
                        ui.label(&part.name);
                        ui.weak(format!("{} notes", part.notes));
                        ui.horizontal(|ui| {
                            for hand in [Hand::Left, Hand::Right] {
                                let on = hands[i] == hand;
                                let text = RichText::new(hand.label()).color(theme::hand(hand));
                                if ui.selectable_label(on, text).clicked() {
                                    hands[i] = hand;
                                }
                            }
                        });
                        ui.end_row();
                    }
                });
                split = HandSplit::Parts(hands);
            } else {
                let mut at = match split {
                    HandSplit::At(p) => p,
                    HandSplit::Parts(_) => 60,
                };
                ui.horizontal(|ui| {
                    ui.label("Right hand from");
                    ui.add(
                        egui::DragValue::new(&mut at)
                            .range(21..=108)
                            .custom_formatter(|v, _| pitch_label(v as u8)),
                    );
                });
                split = HandSplit::At(at);
            }
        });
        if &split != player.split()
            && let Some(player) = self.player.as_mut()
        {
            player.set_split(split, &mut self.out);
            self.flush(shared);
        }
    }

    fn transport(&mut self, ui: &mut Ui, shared: &mut Shared) {
        ui.horizontal(|ui| {
            let has_song = self.player.is_some();
            let playing = self.is_playing();
            let label = if playing { "⏸ Pause" } else { "▶ Play" };
            if ui
                .add_enabled(has_song, egui::Button::new(label))
                .on_hover_text("Space")
                .clicked()
            {
                self.toggle(shared);
            }
            if ui
                .add_enabled(has_song, egui::Button::new("⏮"))
                .on_hover_text("From the top (Home)")
                .clicked()
            {
                self.restart(shared);
            }
            let (position, duration) = self
                .player
                .as_ref()
                .map_or((0.0, 0.0), |p| (p.position(), p.song().duration));
            let width = (ui.available_width() - 420.0).max(160.0);
            self.seek_bar(ui, shared, width, position, duration);
            ui.monospace(format!(
                "{} / {}",
                clock_time(position),
                clock_time(duration)
            ));
            ui.separator();
            if ui
                .add_enabled(
                    has_song,
                    egui::Button::selectable(self.loop_a.is_some(), "A"),
                )
                .on_hover_text("Loop from here (Ctrl+[), or Shift+drag the bar")
                .clicked()
            {
                self.loop_a = Some(position.max(0.0));
                self.apply_loop(shared);
            }
            if ui
                .add_enabled(
                    has_song,
                    egui::Button::selectable(self.loop_b.is_some(), "B"),
                )
                .on_hover_text("Loop to here (Ctrl+])")
                .clicked()
            {
                self.loop_b = Some(position.max(0.0));
                self.looping = true;
                self.apply_loop(shared);
            }
            let can_loop = matches!((self.loop_a, self.loop_b), (Some(a), Some(b)) if b > a);
            if ui
                .add_enabled(can_loop, egui::Button::selectable(self.looping, "🔁 Loop"))
                .on_hover_text("Ctrl+L")
                .clicked()
            {
                self.looping = !self.looping;
                self.apply_loop(shared);
            }
        });
        if let Some(player) = &self.player {
            ui.horizontal(|ui| {
                let stats = &player.stats;
                match player.mode {
                    Mode::Listen => {
                        ui.label(RichText::new("Listening").weak());
                    }
                    mode => {
                        let (done, total) = player.progress();
                        widgets::stat(ui, "Notes", format!("{done}/{total}"), None);
                        widgets::stat(ui, "✔", stats.hits.to_string(), Some(theme::RIGHT));
                        if mode == Mode::PlayAlong {
                            widgets::stat(
                                ui,
                                "Missed",
                                stats.misses.to_string(),
                                (stats.misses > 0).then_some(theme::LATE),
                            );
                        }
                        widgets::stat(
                            ui,
                            "✖",
                            stats.wrong.to_string(),
                            (stats.wrong > 0).then_some(theme::WRONG),
                        );
                        widgets::stat(ui, "Accuracy", widgets::percent(stats.accuracy(mode)), None);
                        if let Some(summary) = stats.timing.summary() {
                            widgets::stat(ui, "Timing", summary, None);
                        }
                    }
                }
                if let Some(result) = &self.result {
                    ui.separator();
                    ui.label(
                        RichText::new(format!("Finished · {result}"))
                            .strong()
                            .color(theme::RIGHT),
                    );
                }
            });
        }
    }

    /// Click or drag to move; Shift+drag to mark a loop.
    fn seek_bar(
        &mut self,
        ui: &mut Ui,
        shared: &mut Shared,
        width: f32,
        position: f64,
        duration: f64,
    ) {
        let (rect, response) = ui.allocate_exact_size(vec2(width, 16.0), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
        if duration <= 0.0 {
            return;
        }
        let x_of = |t: f64| rect.left() + (t / duration).clamp(0.0, 1.0) as f32 * rect.width();
        let t_of = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * duration;
        if let (Some(a), Some(b)) = (self.loop_a, self.loop_b) {
            let color = if self.looping {
                theme::LATE.gamma_multiply(0.45)
            } else {
                theme::LATE.gamma_multiply(0.18)
            };
            painter.rect_filled(
                Rect::from_x_y_ranges(x_of(a)..=x_of(b), rect.y_range()),
                2.0,
                color,
            );
        }
        let played = Rect::from_min_max(rect.min, pos2(x_of(position.max(0.0)), rect.bottom()));
        painter.rect_filled(played, 4.0, visuals.selection.bg_fill.gamma_multiply(0.7));
        if let Some(player) = &self.player {
            for &bar in player.song().bars.iter().step_by(4) {
                painter.vline(
                    x_of(bar),
                    rect.bottom() - 4.0..=rect.bottom(),
                    Stroke::new(1.0, visuals.weak_text_color()),
                );
            }
        }
        painter.vline(
            x_of(position.max(0.0)),
            rect.y_range(),
            Stroke::new(2.0, visuals.strong_text_color()),
        );
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
            StrokeKind::Inside,
        );

        self.bar_rect = Some(rect);
        let shift = ui.input(|i| i.modifiers.shift);
        let down = response.is_pointer_button_down_on();
        if down && let Some(pointer) = response.interact_pointer_pos() {
            let t = t_of(pointer.x);
            if self.loop_drag.is_none() && shift {
                self.loop_drag = Some(t);
            }
            match self.loop_drag {
                Some(from) => {
                    self.loop_a = Some(from.min(t));
                    self.loop_b = Some(from.max(t));
                }
                // Scrub: follow the pointer while it's down.
                None if (t - position).abs() > 0.05 => self.seek(t, shared),
                None => {}
            }
        }
        if !down && self.loop_drag.take().is_some() {
            match (self.loop_a, self.loop_b) {
                (Some(a), Some(b)) if b - a > 0.2 => self.looping = true,
                _ => {
                    self.loop_a = None;
                    self.loop_b = None;
                }
            }
            self.apply_loop(shared);
        }
        if response.hovered()
            && let Some(hover) = response.hover_pos()
        {
            response.on_hover_text_at_pointer(clock_time(t_of(hover.x)));
        }
    }

    /// Notes falling onto the keyboard, lined up with its keys.
    fn lane(&self, ui: &mut Ui, shared: &Shared, layout: &Layout) {
        let available = ui.available_rect_before_wrap();
        ui.allocate_rect(available, Sense::hover());
        let rect = Rect::from_x_y_ranges(layout.rect.x_range(), available.y_range());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::LANE);
        for pitch in layout.low..=layout.high {
            if pitch % 12 == 0 || pitch % 12 == 5 {
                let color = if pitch % 12 == 0 {
                    theme::LANE_BAR
                } else {
                    theme::LANE_LINE
                };
                if let Some(lane) = layout.lane(pitch) {
                    painter.vline(lane.min, rect.y_range(), Stroke::new(1.0, color));
                }
            }
        }
        let Some(player) = &self.player else {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Pick a song or open a MIDI file",
                FontId::proportional(18.0),
                Color32::from_gray(140),
            );
            return;
        };
        let song = player.song();
        let now = shared.now();
        let position = player.position();
        let span = LOOKAHEAD * player.speed;
        let y = |t: f64| rect.bottom() - ((t - position) / span) as f32 * rect.height();

        if let Some((a, b)) = player.looped {
            let band = Rect::from_x_y_ranges(
                rect.x_range(),
                y(b).max(rect.top())..=y(a).min(rect.bottom()),
            );
            painter.rect_filled(band, 0.0, theme::LATE.gamma_multiply(0.06));
        }
        for &beat in &song.beats {
            if beat >= position && beat <= position + span {
                painter.hline(rect.x_range(), y(beat), Stroke::new(1.0, theme::LANE_LINE));
            }
        }
        for (i, &bar) in song.bars.iter().enumerate() {
            if bar >= position && bar <= position + span {
                painter.hline(rect.x_range(), y(bar), Stroke::new(1.0, theme::LANE_BAR));
                painter.text(
                    pos2(rect.left() + 4.0, y(bar) - 2.0),
                    Align2::LEFT_BOTTOM,
                    (i + 1).to_string(),
                    FontId::proportional(11.0),
                    Color32::from_gray(120),
                );
            }
        }

        let waiting = player.waiting_for();
        let pulse = (0.5 + 0.5 * (now * 6.0).sin()) as f32;
        let font = FontId::proportional(11.0);
        let first = song.notes.partition_point(|n| n.start < position - 20.0);
        for (k, note) in song.notes[first..].iter().enumerate() {
            if note.start > position + span {
                break;
            }
            if note.end < position - 0.05 {
                continue;
            }
            let i = first + k;
            let Some(lane) = layout.lane(note.pitch) else {
                continue;
            };
            let top = y(note.end).max(rect.top() - 4.0);
            let bottom = y(note.start).min(rect.bottom() + 6.0);
            if bottom - top < 1.0 {
                continue;
            }
            let r = Rect::from_x_y_ranges(lane.shrink(1.5), top..=bottom);
            let state = player.state(i);
            let base = theme::hand(player.hand(i));
            let base = if is_black(note.pitch) {
                base.gamma_multiply(0.82)
            } else {
                base
            };
            let sounding = note.start <= position && position < note.end;
            let fill = match state {
                NoteState::Auto => base.gamma_multiply(0.5),
                NoteState::Pending => base,
                NoteState::Hit(_) => lighten(base, 0.35),
                NoteState::Missed => Color32::from_gray(80),
            };
            painter.rect_filled(r, 3.0, fill);
            let outline = match state {
                NoteState::Pending
                    if waiting.contains(&note.pitch) && note.start <= position + 0.05 =>
                {
                    Some(Stroke::new(
                        2.0,
                        Color32::WHITE.gamma_multiply(0.4 + 0.6 * pulse),
                    ))
                }
                NoteState::Missed => Some(Stroke::new(1.0, theme::WRONG)),
                _ if sounding => Some(Stroke::new(1.0, Color32::from_white_alpha(140))),
                _ => None,
            };
            if let Some(stroke) = outline {
                painter.rect_stroke(r, 3.0, stroke, StrokeKind::Outside);
            }
            if self.settings.names && r.height() >= 15.0 && r.width() >= 11.0 {
                let name = pitch_label(note.pitch);
                let name = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '-');
                painter.text(
                    pos2(r.center().x, r.bottom() - 2.0),
                    Align2::CENTER_BOTTOM,
                    name,
                    font.clone(),
                    Color32::from_black_alpha(200),
                );
            }
        }
        // The line notes land on.
        painter.hline(
            rect.x_range(),
            rect.bottom() - 1.0,
            Stroke::new(2.0, Color32::from_white_alpha(60)),
        );

        for popup in &self.popups {
            let Some(lane) = layout.lane(popup.pitch) else {
                continue;
            };
            let age = (now - popup.at) as f32;
            let color = match popup.rating {
                Rating::Perfect => theme::RIGHT,
                Rating::Good => theme::RIGHT.gamma_multiply(0.8),
                Rating::Fair => theme::LATE,
                Rating::Off => theme::WRONG,
            };
            painter.text(
                pos2(lane.center(), rect.bottom() - 18.0 - age * 40.0),
                Align2::CENTER_CENTER,
                popup.rating.label(),
                FontId::proportional(14.0),
                color.gamma_multiply((1.0 - age / 0.9).clamp(0.0, 1.0)),
            );
        }

        let big = FontId::proportional(42.0);
        if player.is_playing() && position < 0.0 {
            let count = (-position / player.speed).ceil();
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                format!("{count:.0}"),
                big,
                Color32::from_gray(200),
            );
        } else if !player.is_playing() && !player.is_finished() {
            painter.text(
                rect.center_top() + vec2(0.0, 30.0),
                Align2::CENTER_CENTER,
                "Space to play",
                FontId::proportional(16.0),
                Color32::from_gray(130),
            );
        } else if !waiting.is_empty() && player.is_playing() {
            let names: Vec<String> = waiting.iter().map(|p| pitch_label(*p)).collect();
            painter.text(
                rect.left_top() + vec2(28.0, 14.0),
                Align2::LEFT_TOP,
                format!("Waiting for {}", names.join(" ")),
                FontId::proportional(14.0),
                Color32::from_gray(200),
            );
        }
        if let (true, Some(result)) = (player.is_finished(), &self.result) {
            let band = Rect::from_center_size(rect.center(), vec2(rect.width().min(560.0), 70.0));
            painter.rect_filled(band, 8.0, Color32::from_black_alpha(200));
            painter.text(
                band.center() - vec2(0.0, 12.0),
                Align2::CENTER_CENTER,
                "Finished",
                FontId::proportional(20.0),
                Color32::WHITE,
            );
            painter.text(
                band.center() + vec2(0.0, 14.0),
                Align2::CENTER_CENTER,
                result,
                FontId::proportional(15.0),
                Color32::from_gray(220),
            );
        }
    }
}

fn lighten(color: Color32, amount: f32) -> Color32 {
    let mix = |c: u8| (f32::from(c) + (255.0 - f32::from(c)) * amount) as u8;
    Color32::from_rgb(mix(color.r()), mix(color.g()), mix(color.b()))
}

/// "1:05".
fn clock_time(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
