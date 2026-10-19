//! Note reading: one note on the staff (or its name), find it on the
//! keyboard. The notes already answered stay on the staff to its left,
//! green if found first time.

use std::collections::VecDeque;

use eframe::egui::{self, Align2, Color32, FontId, Rect, RichText, Ui, pos2, vec2};

use super::app::Shared;
use super::glyphs;
use super::staff::{self, Head, Style, System};
use super::{theme, widgets};
use crate::grading::Octave;
use crate::notation::{Accidentals, Clef};
use crate::progress::{self, Kind, Outcome};
use crate::reading::{Answer, Clefs, KeyChoice, Question, Reading, Settings, Show};
use crate::rng::Rng;
use crate::theory::{self, Spelled};

/// Answered notes kept on the staff.
const HISTORY: usize = 7;

pub struct ReadingView {
    pub reading: Reading,
    /// Answered notes, oldest first, and whether each was right first time.
    history: VecDeque<(Question, bool)>,
    /// A wrong key, shown on the staff until this time.
    wrong: Option<(u8, f64)>,
    /// What the last round's result said.
    result: Option<String>,
}

impl ReadingView {
    pub fn new(settings: Settings, rng: Rng, now: f64) -> ReadingView {
        ReadingView {
            reading: Reading::new(settings, rng, now),
            history: VecDeque::new(),
            wrong: None,
            result: None,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.reading.settings
    }

    pub fn set_settings(&mut self, settings: Settings, now: f64) {
        self.reading.settings = settings;
        self.restart(now);
    }

    pub fn restart(&mut self, now: f64) {
        self.reading.restart(now);
        self.history.clear();
        self.wrong = None;
        self.result = None;
    }

    pub fn name(&self) -> String {
        self.reading.settings.name()
    }

    pub fn on_note(&mut self, shared: &mut Shared, pitch: u8, time: f64) {
        let question = self.reading.question;
        match self.reading.press(pitch, time) {
            Answer::Right { first_try, .. } => {
                shared.flash(pitch, theme::RIGHT);
                self.history.push_back((question, first_try));
                while self.history.len() > HISTORY {
                    self.history.pop_front();
                }
                self.wrong = None;
                if self.reading.finished {
                    self.finish(shared);
                }
            }
            Answer::Wrong { played } => {
                shared.flash(played, theme::WRONG);
                self.wrong = Some((played, time + 1.0));
            }
            Answer::Ignored => {}
        }
    }

    fn finish(&mut self, shared: &mut Shared) {
        let r = &self.reading;
        let Some(score) = r.accuracy() else { return };
        let outcome = Outcome {
            score,
            streak: r.best_streak,
            timing_ms: r.mean_time().map(|t| t * 1000.0),
        };
        let date = progress::date_label(progress::today());
        let best = shared
            .progress
            .finish(Kind::Exercise, &r.settings.name(), "", outcome, &date);
        let mut text = format!(
            "{}/{} first time ({:.0}%) · best streak {}",
            r.correct,
            r.answered,
            score * 100.0,
            r.best_streak
        );
        if let Some(t) = r.mean_time() {
            text.push_str(&format!(" · {t:.1} s a note"));
        }
        if best {
            text.push_str(" · New best!");
        }
        self.result = Some(text);
    }

    /// Esc: skips the note, Enter: a new round.
    pub fn shortcuts(&mut self, ctx: &egui::Context, now: f64) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.reading.skip(now);
            self.wrong = None;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            self.restart(now);
        }
    }

    pub fn animating(&self, now: f64) -> bool {
        self.wrong.is_some_and(|w| w.1 > now)
    }

    pub fn ui(&mut self, ui: &mut Ui, shared: &mut Shared) {
        let now = shared.now();
        let mut settings = self.reading.settings.clone();
        ui.horizontal_wrapped(|ui| {
            widgets::choice(
                ui,
                &mut settings.show,
                &[(Show::Staff, "Staff"), (Show::Name, "Key names")],
            );
            ui.separator();
            ui.add_enabled_ui(settings.show == Show::Staff, |ui| {
                widgets::choice(
                    ui,
                    &mut settings.clefs,
                    &[
                        (Clefs::Treble, "Treble"),
                        (Clefs::Bass, "Bass"),
                        (Clefs::Both, "Grand staff"),
                    ],
                );
                ui.separator();
                let range = |n: u8| match n {
                    0 => "On the staff".to_owned(),
                    1 => "1 ledger line".to_owned(),
                    n => format!("{n} ledger lines"),
                };
                egui::ComboBox::from_id_salt("reading_range")
                    .selected_text(range(settings.ledger))
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for n in 0..=3 {
                            ui.selectable_value(&mut settings.ledger, n, range(n));
                        }
                    })
                    .response
                    .on_hover_text("How far notes go above and below the staff");
                let key_text = match settings.key {
                    KeyChoice::Fixed(k) => widgets::key_label(k),
                    KeyChoice::Random => "Any key".to_owned(),
                };
                egui::ComboBox::from_id_salt("reading_key")
                    .selected_text(key_text)
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.key, KeyChoice::Random, "Any key")
                            .on_hover_text("A new key signature every round");
                        for key in widgets::all_keys() {
                            ui.selectable_value(
                                &mut settings.key,
                                KeyChoice::Fixed(key),
                                widgets::key_label(key),
                            );
                        }
                    });
            });
            ui.toggle_value(&mut settings.accidentals, "Accidentals")
                .on_hover_text("Sharps, flats and naturals outside the key");
            ui.separator();
            widgets::choice(
                ui,
                &mut settings.octave,
                &[(Octave::Exact, "Exact octave"), (Octave::Any, "Any octave")],
            );
            ui.separator();
            let round = |n: u32| {
                if n == 0 {
                    "Endless".to_owned()
                } else {
                    format!("{n} notes")
                }
            };
            egui::ComboBox::from_id_salt("reading_round")
                .selected_text(round(settings.round))
                .width(90.0)
                .show_ui(ui, |ui| {
                    for n in [10, 20, 50, 0] {
                        ui.selectable_value(&mut settings.round, n, round(n));
                    }
                });
            if ui.button("↻ New round").on_hover_text("Enter").clicked() {
                self.restart(now);
            }
        });
        if settings != self.reading.settings {
            self.set_settings(settings, now);
        }
        ui.add_space(4.0);
        self.score_line(ui);
        ui.add_space(6.0);

        let height = (ui.available_height() - 4.0).clamp(150.0, 440.0);
        let (rect, painter) = widgets::paper(ui, height);
        match self.reading.settings.show {
            Show::Staff => self.draw_staff(&painter, rect, now),
            Show::Name => self.draw_name(&painter, rect, now),
        }
        if let Some(result) = &self.result {
            let band = Rect::from_center_size(rect.center(), vec2(rect.width().min(640.0), 74.0));
            painter.rect_filled(
                band,
                8.0,
                Color32::from_rgba_unmultiplied(250, 248, 241, 235),
            );
            painter.rect_stroke(
                band,
                8.0,
                egui::Stroke::new(1.0, theme::FAINT_INK),
                egui::StrokeKind::Inside,
            );
            painter.text(
                band.center() - vec2(0.0, 14.0),
                Align2::CENTER_CENTER,
                "Round done",
                FontId::proportional(22.0),
                theme::INK,
            );
            painter.text(
                band.center() + vec2(0.0, 16.0),
                Align2::CENTER_CENTER,
                result,
                FontId::proportional(16.0),
                theme::INK,
            );
        }
    }

    fn score_line(&self, ui: &mut Ui) {
        let r = &self.reading;
        ui.horizontal(|ui| {
            if r.settings.round > 0 {
                let done = r.answered as f32 / r.settings.round as f32;
                ui.add(
                    egui::ProgressBar::new(done)
                        .desired_width(140.0)
                        .text(format!("{} / {}", r.answered, r.settings.round)),
                );
            } else {
                widgets::stat(ui, "Notes", r.answered.to_string(), None);
            }
            ui.separator();
            widgets::stat(ui, "✔", r.correct.to_string(), Some(theme::RIGHT));
            widgets::stat(ui, "✖", r.wrong.to_string(), Some(theme::WRONG));
            ui.separator();
            widgets::stat(ui, "Streak", r.streak.to_string(), None);
            widgets::stat(ui, "best", r.best_streak.to_string(), None);
            ui.separator();
            widgets::stat(ui, "Accuracy", widgets::percent(r.accuracy()), None);
            if let Some(t) = r.mean_time() {
                widgets::stat(ui, "Per note", format!("{t:.1} s"), None);
            }
            ui.separator();
            ui.label(RichText::new(r.key.label()).weak());
        });
    }

    fn draw_staff(&self, painter: &egui::Painter, rect: Rect, now: f64) {
        let r = &self.reading;
        let clefs: Vec<Clef> = [Clef::Treble, Clef::Bass]
            .into_iter()
            .filter(|c| r.settings.clefs.includes(*c))
            .collect();
        let margin = 2.0 + f32::from(r.settings.ledger);
        let system = System::new(rect.shrink(10.0), &clefs, r.key, 18.0, margin.max(4.0));
        system.draw(painter, theme::INK);
        let s = system.space;
        let gap = 3.6 * s;
        let style = Style {
            stem: false,
            paper: theme::PAPER,
            ink: theme::INK,
            finger_color: None,
        };
        let first = system.notes_left + 1.4 * s;
        let current_x = (first + HISTORY as f32 * gap).min(system.right - 6.0 * s);
        let head = |question: &Question, color: Color32| {
            let mut accidentals = Accidentals::new(r.key);
            Head {
                note: question.note,
                clef: question.clef,
                color,
                accidental: accidentals.write(question.note),
                finger: None,
            }
        };
        for (i, (question, first_try)) in self.history.iter().rev().enumerate() {
            let x = current_x - (i + 1) as f32 * gap;
            if x < first - 0.5 * s {
                break;
            }
            let color = if *first_try {
                theme::RIGHT
            } else {
                theme::LATE
            };
            let fade = 1.0 - i as f32 / (HISTORY as f32 + 1.0);
            staff::chord(
                painter,
                &system,
                x,
                &[head(question, color.gamma_multiply(fade))],
                style,
            );
        }
        if r.finished {
            return;
        }
        // The note to find, in a soft column.
        let column = Rect::from_x_y_ranges(
            current_x - 1.7 * s..=current_x + 1.7 * s,
            system.top() - 3.0 * s..=system.bottom() + 3.0 * s,
        );
        painter.rect_filled(column, 6.0, theme::TARGET.gamma_multiply(0.10));
        staff::chord(
            painter,
            &system,
            current_x,
            &[head(&r.question, theme::INK)],
            style,
        );
        // A wrong key, written where it would be.
        if let Some((played, until)) = self.wrong.filter(|w| w.1 > now) {
            let note = theory::spell(played, r.key);
            let clef = wrong_clef(&clefs, r.question.clef, played);
            let ghost = Question { note, clef };
            let alpha = ((until - now) / 0.4).clamp(0.0, 1.0) as f32;
            let x = current_x + 3.4 * s;
            staff::chord(
                painter,
                &system,
                x,
                &[head(&ghost, theme::WRONG.gamma_multiply(alpha))],
                style,
            );
            painter.text(
                pos2(x, system.bottom() + 2.6 * s),
                Align2::CENTER_CENTER,
                note.label(),
                FontId::proportional(1.1 * s),
                theme::WRONG.gamma_multiply(alpha),
            );
        }
    }

    fn draw_name(&self, painter: &egui::Painter, rect: Rect, now: f64) {
        let r = &self.reading;
        if r.finished {
            return;
        }
        let size = (rect.height() * 0.4).min(110.0);
        spelled_name(painter, rect.center(), r.question.note, size, theme::INK);
        if let Some((played, until)) = self.wrong.filter(|w| w.1 > now) {
            let alpha = ((until - now) / 0.4).clamp(0.0, 1.0) as f32;
            painter.text(
                rect.center() + vec2(0.0, size * 0.75),
                Align2::CENTER_CENTER,
                format!("✖ {}", theory::pitch_label(played)),
                FontId::proportional(size * 0.25),
                theme::WRONG.gamma_multiply(alpha),
            );
        }
    }
}

/// Where to write a wrong key on a grand staff: with the note it was
/// meant to be, unless it's far from it.
fn wrong_clef(clefs: &[Clef], target: Clef, played: u8) -> Clef {
    if clefs.len() == 1 {
        return clefs[0];
    }
    match target {
        Clef::Treble if played >= 53 => Clef::Treble,
        Clef::Bass if played <= 67 => Clef::Bass,
        _ => Clef::for_pitch(played),
    }
}

/// A note's name in big letters, its accidental drawn as the sign:
/// "F♯4".
pub fn spelled_name(
    painter: &egui::Painter,
    center: egui::Pos2,
    note: Spelled,
    size: f32,
    color: Color32,
) {
    let letter = theory::LETTERS[usize::from(note.letter)];
    let letter_galley =
        painter.layout_no_wrap(letter.to_owned(), FontId::proportional(size), color);
    let octave_galley = painter.layout_no_wrap(
        note.octave.to_string(),
        FontId::proportional(size * 0.5),
        color,
    );
    let space = size * 0.16;
    let accidental_w = if note.accidental == 0 {
        0.0
    } else {
        glyphs::accidental_width(note.accidental) * space + 0.3 * space
    };
    let width = letter_galley.size().x + accidental_w + octave_galley.size().x + 0.2 * space;
    let left = center.x - width / 2.0;
    let top = center.y - letter_galley.size().y / 2.0;
    painter.galley(pos2(left, top), letter_galley.clone(), color);
    let mut x = left + letter_galley.size().x + 0.15 * space;
    if note.accidental != 0 {
        let w = glyphs::accidental_width(note.accidental) * space;
        glyphs::accidental(
            painter,
            pos2(x + w / 2.0, center.y - size * 0.18),
            space,
            note.accidental,
            color,
        );
        x += accidental_w;
    }
    painter.galley(pos2(x, center.y + size * 0.05), octave_galley, color);
}
