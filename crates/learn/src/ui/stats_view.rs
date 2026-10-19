//! Progress: practice time by day, and the best results of every exercise
//! and song.

use std::collections::BTreeMap;

use eframe::egui::{self, Align2, FontId, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use super::app::Shared;
use super::theme;
use crate::progress::{self, Progress, Record, duration_label};

/// Days in the chart.
const DAYS: i64 = 14;

#[derive(Default)]
pub struct StatsView {
    confirm_reset: bool,
}

impl StatsView {
    pub fn ui(&mut self, ui: &mut Ui, shared: &mut Shared) {
        let progress = &shared.progress;
        let today = progress::today();
        let days = progress.recent_days(today, DAYS);
        let week: f64 = days.iter().rev().take(7).map(|d| d.1).sum();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Practice").size(18.0).strong());
            ui.separator();
            for (label, seconds) in [
                ("Today", days.last().map_or(0.0, |d| d.1)),
                ("7 days", week),
                ("All time", progress.total_seconds()),
            ] {
                ui.label(RichText::new(label).weak());
                ui.label(RichText::new(duration_label(seconds)).strong().monospace());
                ui.add_space(8.0);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Reset…")
                    .on_hover_text("Forget all progress")
                    .clicked()
                {
                    self.confirm_reset = true;
                }
            });
        });
        ui.add_space(4.0);
        chart(ui, &days);
        ui.add_space(8.0);

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.columns(2, |columns| {
                    table(
                        &mut columns[0],
                        "Exercises",
                        "exercises",
                        &progress.exercises,
                    );
                    table(&mut columns[1], "Songs", "songs", &progress.songs);
                });
            });

        if self.confirm_reset {
            let mut choice = None;
            egui::Modal::new(egui::Id::new("reset_progress")).show(ui.ctx(), |ui| {
                ui.set_width(300.0);
                ui.heading("Reset progress?");
                ui.label("Best scores and practice time will be gone.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Reset").clicked() {
                        choice = Some(true);
                    }
                    if ui.button("Cancel").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        choice = Some(false);
                    }
                });
            });
            if let Some(reset) = choice {
                if reset {
                    shared.progress = Progress::default();
                }
                self.confirm_reset = false;
            }
        }
    }
}

/// Minutes practised each day, as bars.
fn chart(ui: &mut Ui, days: &[(String, f64)]) {
    let height = 110.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
    let plot = Rect::from_min_max(rect.min + vec2(40.0, 10.0), rect.max - vec2(10.0, 18.0));
    let most = days.iter().map(|d| d.1).fold(0.0, f64::max).max(600.0);
    // A gridline every 10 or 30 minutes.
    let minutes = most / 60.0;
    let every = if minutes > 90.0 { 30.0 } else { 10.0 };
    let mut m = 0.0;
    while m <= minutes {
        let y = plot.bottom() - (m / minutes) as f32 * plot.height();
        painter.hline(
            plot.x_range(),
            y,
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        );
        painter.text(
            pos2(plot.left() - 6.0, y),
            Align2::RIGHT_CENTER,
            format!("{m:.0}m"),
            FontId::proportional(10.0),
            visuals.weak_text_color(),
        );
        m += every;
    }
    let width = plot.width() / days.len().max(1) as f32;
    for (i, (date, seconds)) in days.iter().enumerate() {
        let x = plot.left() + (i as f32 + 0.5) * width;
        let h = (*seconds / most) as f32 * plot.height();
        let bar = Rect::from_min_max(
            pos2(x - width * 0.32, plot.bottom() - h),
            pos2(x + width * 0.32, plot.bottom()),
        );
        let today = i + 1 == days.len();
        let color = if today { theme::RIGHT } else { theme::TARGET };
        painter.rect_filled(bar, 2.0, color);
        let day = date.rsplit('-').next().unwrap_or("");
        painter.text(
            pos2(x, plot.bottom() + 3.0),
            Align2::CENTER_TOP,
            day,
            FontId::proportional(10.0),
            if today {
                visuals.strong_text_color()
            } else {
                visuals.weak_text_color()
            },
        );
    }
}

fn table(ui: &mut Ui, title: &str, id: &str, records: &BTreeMap<String, Record>) {
    ui.label(RichText::new(title).size(16.0).strong());
    if records.is_empty() {
        ui.weak("Nothing yet");
        return;
    }
    let mut rows: Vec<(&String, &Record)> = records.iter().collect();
    rows.sort_by(|a, b| b.1.last.cmp(&a.1.last).then(a.0.cmp(b.0)));
    egui::Grid::new(id)
        .num_columns(4)
        .striped(true)
        .spacing([14.0, 4.0])
        .show(ui, |ui| {
            for header in ["", "Best", "Time", "Last"] {
                ui.label(RichText::new(header).weak());
            }
            ui.end_row();
            for (name, record) in rows {
                ui.label(name.as_str());
                ui.vertical(|ui| {
                    if record.bests.is_empty() {
                        ui.weak("–");
                    }
                    for (variant, best) in &record.bests {
                        let mut text = format!("{:.0}%", best.score * 100.0);
                        if best.streak > 0 {
                            text.push_str(&format!(" · streak {}", best.streak));
                        }
                        if let Some(ms) = best.timing_ms {
                            if ms >= 1000.0 {
                                text.push_str(&format!(" · {:.1} s", ms / 1000.0));
                            } else {
                                text.push_str(&format!(" · ±{ms:.0} ms"));
                            }
                        }
                        if !variant.is_empty() {
                            text = format!("{variant}: {text}");
                        }
                        ui.label(RichText::new(text).monospace());
                    }
                });
                ui.label(duration_label(record.seconds));
                ui.label(RichText::new(record.last.clone().unwrap_or_default()).weak());
                ui.end_row();
            }
        });
}
