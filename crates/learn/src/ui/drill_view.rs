//! Scales and chords: the notes on the staff with fingering, the next ones
//! lit on the keyboard, checked as they're played, optionally against the
//! metronome.

use eframe::egui::{self, Align2, FontId, Rect, RichText, Ui, pos2, vec2};

use super::app::Shared;
use super::piano::Look;
use super::staff::{self, Head, Style, System};
use super::{theme, widgets};
use crate::drill::{COUNT_IN, Drill, Kind, Settings};
use crate::grading::{Progress, Rating};
use crate::notation::{Accidentals, Clef};
use crate::progress::{self, Outcome};
use crate::rng::Rng;
use crate::song::Hands;
use crate::theory::{self, ChordKind, Hand, KeySig, NoteName, ScaleKind};

pub struct DrillView {
    pub drill: Drill,
    /// A wrong key, shown next to the note until this time.
    wrong: Option<(u8, f64)>,
    /// A chord's notes weren't together: say so until this time.
    apart: Option<f64>,
    result: Option<String>,
    /// The metronome is clicking for this drill.
    running: bool,
    /// The first beat, as placed from the audio transport.
    origin: Option<f64>,
    rng: Rng,
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

impl DrillView {
    pub fn new(settings: Settings, rng: Rng) -> DrillView {
        DrillView {
            drill: Drill::new(settings),
            wrong: None,
            apart: None,
            result: None,
            running: false,
            origin: None,
            rng,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.drill.settings
    }

    pub fn name(&self) -> String {
        self.drill.settings.name()
    }

    pub fn set_settings(&mut self, settings: Settings, shared: &mut Shared) {
        self.stop(shared);
        self.drill = Drill::new(settings);
        self.wrong = None;
        self.apart = None;
        self.result = None;
    }

    pub fn restart(&mut self, shared: &mut Shared) {
        let settings = self.drill.settings.clone();
        self.set_settings(settings, shared);
    }

    /// With the metronome: starts it, from the top, after a bar of count-in.
    pub fn start(&mut self, shared: &mut Shared) {
        self.restart(shared);
        if !self.drill.settings.metronome {
            return;
        }
        shared.audio.set_metronome(Some(self.drill.settings.bpm));
        let beat_zero = shared.now() + shared.audio.latency();
        self.drill.start_metronome(beat_zero);
        self.running = true;
        self.origin = None;
    }

    pub fn stop(&mut self, shared: &mut Shared) {
        if self.running {
            shared.audio.set_metronome(None);
            self.running = false;
        }
    }

    /// Practising, for the practice clock: the metronome is going.
    pub fn running(&self) -> bool {
        self.running
    }

    pub fn animating(&self, now: f64) -> bool {
        self.running || self.wrong.is_some_and(|w| w.1 > now) || self.apart.is_some_and(|a| a > now)
    }

    pub fn on_note(&mut self, shared: &mut Shared, pitch: u8, time: f64) {
        // With the metronome on, notes before Start are just playing.
        if self.drill.settings.metronome && !self.running {
            return;
        }
        match self.drill.press(pitch, time) {
            Progress::Done { .. } => shared.flash(pitch, theme::RIGHT),
            Progress::Finished => {
                shared.flash(pitch, theme::RIGHT);
                self.finish(shared);
            }
            Progress::Wrong(played) => {
                shared.flash(played, theme::WRONG);
                self.wrong = Some((played, time + 0.8));
            }
            Progress::Apart => self.apart = Some(time + 1.5),
            Progress::Partial | Progress::Nothing => {}
        }
    }

    pub fn update(&mut self, shared: &mut Shared, now: f64) {
        if self.drill.tick(now) == Progress::Apart {
            self.apart = Some(now + 1.5);
        }
        if !self.running {
            return;
        }
        // The transport says when its first beat really was: take the
        // earliest reading, the one least delayed by the frame.
        if let Some(seconds) = shared.audio.transport_seconds().filter(|s| *s > 0.0) {
            let candidate = now - seconds + shared.audio.latency();
            let origin = self.origin.map_or(candidate, |o| o.min(candidate));
            self.origin = Some(origin);
            self.drill.set_beat_zero(origin);
        }
        // Stop clicking a beat after the last note.
        if let Some(done) = self.drill.finished_at
            && now > done + self.drill.settings.beat()
        {
            self.stop(shared);
        }
    }

    fn finish(&mut self, shared: &mut Shared) {
        let sequence = &self.drill.sequence;
        let score = sequence.accuracy();
        let timing = sequence.timing.mean_abs();
        let outcome = Outcome {
            score,
            streak: 0,
            timing_ms: timing,
        };
        let date = progress::date_label(progress::today());
        let settings = &self.drill.settings;
        let best = shared.progress.finish(
            progress::Kind::Exercise,
            &settings.name(),
            &settings.variant(),
            outcome,
            &date,
        );
        let clean = sequence.results().iter().filter(|r| r.clean).count();
        let mut text = format!(
            "{clean}/{} clean ({:.0}%) · {} mistake{}",
            sequence.len(),
            score * 100.0,
            sequence.mistakes,
            if sequence.mistakes == 1 { "" } else { "s" }
        );
        if let Some(seconds) = self.drill.elapsed(shared.now()) {
            text.push_str(&format!(" · {seconds:.1} s"));
        }
        if let Some(summary) = sequence.timing.summary() {
            text.push_str(&format!(" · timing {summary}"));
        }
        if best {
            text.push_str(" · New best!");
        }
        self.result = Some(text);
    }

    /// Space: start (with the metronome) or start over; Enter: start over.
    pub fn shortcuts(&mut self, ctx: &egui::Context, shared: &mut Shared) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space)) {
            self.start(shared);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            self.restart(shared);
        }
    }

    /// The next notes, lit on the keyboard with their fingers.
    pub fn looks(&self, looks: &mut [Look; 128]) {
        if self.drill.settings.metronome && !self.running {
            return;
        }
        for note in self.drill.next_notes() {
            let look = &mut looks[usize::from(note.pitch())];
            look.hint = Some(theme::TARGET);
            look.finger = note.finger;
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, shared: &mut Shared) {
        let now = shared.now();
        let mut settings = self.drill.settings.clone();
        let mut start = false;
        ui.horizontal_wrapped(|ui| {
            widgets::choice(
                ui,
                &mut settings.kind,
                &[(Kind::Scale, "Scale"), (Kind::Chord, "Chord")],
            );
            ui.separator();
            ui.menu_button(RichText::new(settings.root.label()).strong(), |ui| {
                ui.set_max_width(6.0 * 34.0);
                ui.horizontal_wrapped(|ui| {
                    for root in NoteName::ROOTS {
                        let on = settings.root == root;
                        if ui
                            .add_sized([28.0, 20.0], egui::Button::selectable(on, root.label()))
                            .clicked()
                        {
                            settings.root = root;
                            ui.close();
                        }
                    }
                });
            })
            .response
            .on_hover_text("Root");
            match settings.kind {
                Kind::Scale => {
                    egui::ComboBox::from_id_salt("drill_scale")
                        .selected_text(capitalized(settings.scale.name()))
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for kind in ScaleKind::ALL {
                                ui.selectable_value(
                                    &mut settings.scale,
                                    kind,
                                    capitalized(kind.name()),
                                );
                            }
                        });
                }
                Kind::Chord => {
                    egui::ComboBox::from_id_salt("drill_chord")
                        .selected_text(capitalized(settings.chord.name()))
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for kind in ChordKind::ALL {
                                ui.selectable_value(
                                    &mut settings.chord,
                                    kind,
                                    capitalized(kind.name()),
                                );
                            }
                        });
                    let inversion = |i: Option<u8>| match i {
                        None => "All inversions".to_owned(),
                        Some(i) => capitalized(theory::inversion_name(i)),
                    };
                    egui::ComboBox::from_id_salt("drill_inversion")
                        .selected_text(inversion(settings.inversion))
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut settings.inversion, None, inversion(None));
                            for i in 0..=settings.chord.inversions() {
                                ui.selectable_value(
                                    &mut settings.inversion,
                                    Some(i),
                                    inversion(Some(i)),
                                );
                            }
                        });
                }
            }
            ui.separator();
            widgets::choice(
                ui,
                &mut settings.hands,
                &[
                    (Hands::Left, "Left"),
                    (Hands::Right, "Right"),
                    (Hands::Both, "Both"),
                ],
            );
            if settings.kind == Kind::Scale {
                ui.separator();
                widgets::choice(
                    ui,
                    &mut settings.octaves,
                    &[(1, "1 octave"), (2, "2 octaves")],
                );
                ui.toggle_value(&mut settings.down, "Up & down");
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.toggle_value(&mut settings.metronome, "♩ Metronome")
                .on_hover_text("Play in time; Space starts the count-in");
            ui.add_enabled_ui(settings.metronome, |ui| {
                ui.add(widgets::bpm_drag(&mut settings.bpm));
                if settings.kind == Kind::Scale {
                    widgets::choice(
                        ui,
                        &mut settings.per_beat,
                        &[(1, "1 a beat"), (2, "2 a beat"), (4, "4 a beat")],
                    );
                }
                let label = if self.running {
                    "↻ Restart"
                } else {
                    "▶ Start"
                };
                if ui.button(label).on_hover_text("Space").clicked() {
                    start = true;
                }
            });
            ui.separator();
            if ui.button("↻ Again").on_hover_text("Enter").clicked() {
                self.restart(shared);
            }
            if ui
                .button("Random")
                .on_hover_text("Another root and type")
                .clicked()
            {
                settings.root = *self.rng.pick(&NoteName::ROOTS).unwrap_or(&NoteName::C);
                match settings.kind {
                    Kind::Scale => {
                        settings.scale =
                            *self.rng.pick(&ScaleKind::ALL).unwrap_or(&ScaleKind::Major)
                    }
                    Kind::Chord => {
                        settings.chord =
                            *self.rng.pick(&ChordKind::ALL).unwrap_or(&ChordKind::Major)
                    }
                }
            }
            if self.running {
                ui.separator();
                self.beat_lights(ui, now);
            }
        });
        if settings != self.drill.settings {
            self.set_settings(settings, shared);
        }
        if start {
            self.start(shared);
        }
        ui.add_space(4.0);
        self.status_line(ui, now);
        ui.add_space(6.0);
        let height = (ui.available_height() - 4.0).clamp(160.0, 460.0);
        let (rect, painter) = widgets::paper(ui, height);
        self.draw(&painter, rect, now);
    }

    fn beat_lights(&self, ui: &mut Ui, now: f64) {
        let Some(zero) = self.drill.started else {
            return;
        };
        let beat = self.drill.settings.beat();
        let count = ((now - zero) / beat).floor();
        let (rect, _) = ui.allocate_exact_size(vec2(64.0, 14.0), egui::Sense::hover());
        for i in 0..4 {
            let lit = count >= 0.0 && count as i64 % 4 == i;
            let center = pos2(rect.left() + 8.0 + i as f32 * 16.0, rect.center().y);
            let color = match (lit, i) {
                (true, 0) => theme::LATE,
                (true, _) => theme::RIGHT,
                _ => ui.visuals().weak_text_color().gamma_multiply(0.5),
            };
            ui.painter().circle_filled(center, 5.0, color);
        }
        let counting = now - zero < COUNT_IN * beat;
        if counting {
            ui.label(RichText::new("count-in").weak());
        }
        ui.ctx().request_repaint();
    }

    fn status_line(&self, ui: &mut Ui, now: f64) {
        let drill = &self.drill;
        let sequence = &drill.sequence;
        ui.horizontal(|ui| {
            let hands = match drill.settings.hands {
                Hands::Left => "left hand",
                Hands::Right => "right hand",
                Hands::Both => "hands together",
            };
            ui.label(RichText::new(drill.title()).size(20.0).strong());
            ui.label(RichText::new(hands).weak());
            if let Some(key) = drill.key.filter(|k| k.0 != 0) {
                ui.label(RichText::new(format!("· {}", key.count_label())).weak());
            }
            ui.separator();
            if let Some(result) = &self.result {
                ui.label(
                    RichText::new(format!("Done · {result}"))
                        .color(theme::RIGHT)
                        .strong(),
                );
                return;
            }
            ui.add(
                egui::ProgressBar::new(sequence.index() as f32 / sequence.len().max(1) as f32)
                    .desired_width(120.0)
                    .text(format!("{} / {}", sequence.index(), sequence.len())),
            );
            widgets::stat(
                ui,
                "Mistakes",
                sequence.mistakes.to_string(),
                (sequence.mistakes > 0).then_some(theme::WRONG),
            );
            if let Some(summary) = sequence.timing.summary() {
                widgets::stat(ui, "Timing", summary, None);
            }
            if let Some(apart) = self.apart.filter(|a| *a > now) {
                let alpha = ((apart - now) / 0.5).clamp(0.0, 1.0) as f32;
                ui.label(
                    RichText::new("Together!")
                        .strong()
                        .color(theme::LATE.gamma_multiply(alpha)),
                );
            }
            if drill.settings.metronome && !self.running {
                ui.label(RichText::new("Space to start").weak());
            }
        });
    }

    fn draw(&self, painter: &egui::Painter, rect: Rect, now: f64) {
        let drill = &self.drill;
        let settings = &drill.settings;
        let clefs: Vec<Clef> = match settings.hands {
            Hands::Left => vec![Clef::Bass],
            Hands::Right => vec![Clef::Treble],
            Hands::Both => vec![Clef::Treble, Clef::Bass],
        };
        let key = drill.key.unwrap_or(KeySig::C);
        let area = rect.shrink(10.0);
        // Chords are few to a page, so they can be bigger.
        let max_space = if settings.kind == Kind::Chord {
            19.0
        } else {
            15.0
        };
        let full = System::new(area, &clefs, key, max_space, 5.0);
        let s = full.space;
        let steps = drill.steps().max(1);
        let per_bar = match settings.kind {
            Kind::Scale => 4 * usize::from(settings.per_beat.max(1)),
            Kind::Chord => 1,
        };
        let bars = steps.div_ceil(per_bar);
        let bar_room = 1.2 * s;
        let (min_step, max_step) = match settings.kind {
            Kind::Scale => (2.7 * s, 4.2 * s),
            Kind::Chord => (6.0 * s, 8.0 * s),
        };
        let lead = 0.6 * s;
        let room = full.right - full.notes_left - lead;
        let step_w = ((room - bars as f32 * bar_room) / steps as f32).clamp(min_step, max_step);
        // From the first note to the end bar line.
        let content = steps as f32 * step_w + bars as f32 * bar_room;
        let fits = content <= room;
        // A short drill gets a short staff, in the middle.
        let system = if fits {
            let width = area.width() - (room - content);
            System::new(
                Rect::from_center_size(area.center(), vec2(width, area.height())),
                &clefs,
                key,
                max_space,
                5.0,
            )
        } else {
            full
        };
        system.draw(painter, theme::INK);
        let start = system.notes_left + lead;
        let available = system.right - start;
        let x_of = |i: usize| start + (i as f32 + 0.5) * step_w + (i / per_bar) as f32 * bar_room;
        // Scroll to keep the next notes in view.
        let current = drill.sequence.index().min(steps - 1);
        let scroll = if fits {
            0.0
        } else {
            (x_of(current) - (start + available * 0.45)).clamp(0.0, content - available)
        };
        let clip = Rect::from_x_y_ranges(start - 0.8 * s..=system.right + 1.0, rect.y_range());
        let painter = painter.with_clip_rect(clip);
        let style = Style {
            stem: settings.kind == Kind::Scale,
            paper: theme::PAPER,
            ink: theme::INK,
            finger_color: None,
        };

        let results = drill.sequence.results();
        let partial = drill.sequence.partial();
        let mut accidentals = [Accidentals::new(key), Accidentals::new(key)];
        let mut labels = Vec::new();
        let mut highest = system.top();
        for step in 0..drill.steps() {
            if step % per_bar == 0 {
                for a in &mut accidentals {
                    a.bar();
                }
            }
            let x = x_of(step) - scroll;
            let color = |pitch: u8| {
                if step < results.len() {
                    if results[step].clean {
                        theme::RIGHT
                    } else {
                        theme::LATE
                    }
                } else if step == drill.sequence.index() {
                    if partial.contains(&pitch) {
                        theme::RIGHT
                    } else {
                        theme::TARGET
                    }
                } else {
                    theme::INK
                }
            };
            let heads: Vec<Head> = drill
                .notes
                .iter()
                .filter(|n| n.step == step)
                .map(|n| {
                    let state = &mut accidentals[usize::from(n.hand == Hand::Right)];
                    Head {
                        note: n.note,
                        clef: n.clef,
                        color: color(n.pitch()),
                        accidental: state.write(n.note),
                        finger: n.finger,
                    }
                })
                .collect();
            if step == drill.sequence.index() && !drill.is_finished() {
                let half = (step_w * 0.45).min(if style.stem { 1.7 } else { 2.6 } * s);
                let column = Rect::from_x_y_ranges(
                    x - half..=x + half,
                    system.top() - 3.5 * s..=system.bottom() + 3.5 * s,
                );
                painter.rect_filled(column, 6.0, theme::TARGET.gamma_multiply(0.09));
            }
            let (top, _) = staff::chord(&painter, &system, x, &heads, style);
            highest = highest.min(top);
            if let Some(label) = drill.labels.get(step) {
                let color = heads.first().map_or(theme::INK, |h| h.color);
                labels.push((x, label, color));
            }
            // How far from the beat it was.
            if let Some(error) = results.get(step).and_then(|r| r.error_ms) {
                let color = match Rating::of(error) {
                    Rating::Perfect | Rating::Good => theme::RIGHT,
                    Rating::Fair => theme::LATE,
                    Rating::Off => theme::WRONG,
                };
                painter.text(
                    pos2(x, system.bottom() + 4.2 * s),
                    Align2::CENTER_CENTER,
                    format!("{error:+.0}"),
                    FontId::proportional(0.95 * s),
                    color,
                );
            }
            let end = step + 1 == steps;
            if (step + 1) % per_bar == 0 || end {
                let bx = x + step_w / 2.0 + bar_room * 0.5;
                if end {
                    system.end_line(&painter, bx + 0.6 * s, theme::INK);
                } else {
                    system.bar_line(&painter, bx, theme::INK);
                }
            }
        }
        // Chord symbols in a row above everything.
        for (x, label, color) in labels {
            painter.text(
                pos2(x, highest - 0.5 * s),
                Align2::CENTER_BOTTOM,
                label,
                FontId::proportional(1.4 * s),
                color,
            );
        }

        // A wrong key, next to the note it should have been.
        if let Some((played, until)) = self.wrong.filter(|w| w.1 > now) {
            let x = x_of(current) - scroll + step_w * 0.5;
            let near = drill.next_notes().next();
            let clef = near.map_or(Clef::for_pitch(played), |n| {
                if clefs.len() == 1 { clefs[0] } else { n.clef }
            });
            let alpha = ((until - now) / 0.3).clamp(0.0, 1.0) as f32;
            let note = theory::spell(played, key);
            let head = Head {
                note,
                clef,
                color: theme::WRONG.gamma_multiply(alpha),
                accidental: Accidentals::new(key).write(note),
                finger: None,
            };
            staff::chord(
                &painter,
                &system,
                x,
                &[head],
                Style {
                    stem: false,
                    ..style
                },
            );
        }
    }
}
