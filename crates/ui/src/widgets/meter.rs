//! Stereo peak meter, drawn by hand. A template for the other custom widgets
//! a DAW needs (piano roll, timeline, mixer strips): allocate a rect, read
//! input from the response, paint.

use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, pos2, vec2};

const MIN_DB: f32 = -60.0;
const MAX_DB: f32 = 6.0;
const SCALE_MARKS_DB: [f32; 6] = [6.0, 0.0, -6.0, -12.0, -24.0, -48.0];
/// How fast the bar falls back after a peak.
const FALLOFF_DB_PER_SEC: f32 = 24.0;
const PEAK_HOLD_SECS: f32 = 1.5;

const SCALE_WIDTH: f32 = 22.0;
const BAR_WIDTH: f32 = 12.0;
const BAR_GAP: f32 = 4.0;
const CLIP_LED_HEIGHT: f32 = 6.0;

const GREEN: Color32 = crate::theme::color::METER_LOW;
const YELLOW: Color32 = crate::theme::color::METER_MID;
const RED: Color32 = crate::theme::color::METER_HIGH;
/// (ceiling in dB, color) bands, from the bottom of the meter up.
const ZONES: [(f32, Color32); 3] = [(-12.0, GREEN), (-3.0, YELLOW), (MAX_DB, RED)];

#[derive(Default)]
pub struct StereoMeter {
    channels: [Channel; 2],
}

#[derive(Clone, Copy)]
struct Channel {
    level_db: f32,
    hold_db: f32,
    hold_timer: f32,
    clipped: bool,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            level_db: MIN_DB,
            hold_db: MIN_DB,
            hold_timer: 0.0,
            clipped: false,
        }
    }
}

impl StereoMeter {
    /// Feed in the latest peaks (linear amplitude) and the frame time.
    pub fn update(&mut self, peaks: [f32; 2], dt: f32) {
        for (ch, peak) in self.channels.iter_mut().zip(peaks) {
            let db = dsp::gain_to_db(peak).clamp(MIN_DB, MAX_DB);
            let falloff = FALLOFF_DB_PER_SEC * dt;

            ch.level_db = (ch.level_db - falloff).max(db).max(MIN_DB);

            if db >= ch.hold_db {
                ch.hold_db = db;
                ch.hold_timer = PEAK_HOLD_SECS;
            } else if ch.hold_timer > 0.0 {
                ch.hold_timer -= dt;
            } else {
                ch.hold_db = (ch.hold_db - falloff).max(MIN_DB);
            }

            if peak >= 1.0 {
                ch.clipped = true;
            }
        }
    }

    /// True while there's still something moving on screen.
    pub fn is_active(&self) -> bool {
        self.channels
            .iter()
            .any(|c| c.level_db > MIN_DB || c.hold_db > MIN_DB)
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let width = SCALE_WIDTH + 2.0 * BAR_WIDTH + BAR_GAP;
        let (rect, response) =
            ui.allocate_exact_size(vec2(width, ui.available_height()), Sense::click());
        let response = response.on_hover_text("Click to reset clip indicators");
        if response.clicked() {
            for ch in &mut self.channels {
                ch.clipped = false;
            }
        }

        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        let trough = visuals.extreme_bg_color;
        let bars_left = rect.left() + SCALE_WIDTH;
        let bars_area = Rect::from_min_max(
            pos2(bars_left, rect.top() + CLIP_LED_HEIGHT + 4.0),
            rect.right_bottom(),
        );
        let y_for = |db: f32| {
            let t = (db - MIN_DB) / (MAX_DB - MIN_DB);
            bars_area.bottom() - t * bars_area.height()
        };

        for db in SCALE_MARKS_DB {
            let y = y_for(db);
            painter.text(
                pos2(bars_left - 4.0, y),
                Align2::RIGHT_CENTER,
                if db > 0.0 {
                    format!("+{db}")
                } else {
                    format!("{db}")
                },
                FontId::monospace(crate::theme::text::TINY),
                visuals.weak_text_color(),
            );
        }

        for (i, ch) in self.channels.iter().enumerate() {
            let x = bars_left + i as f32 * (BAR_WIDTH + BAR_GAP);
            let bar = Rect::from_x_y_ranges(x..=x + BAR_WIDTH, bars_area.y_range());
            painter.rect_filled(bar, 2.0, trough);

            // Fill each color band up to the current level.
            let mut floor_db = MIN_DB;
            for (ceiling_db, color) in ZONES {
                let top_db = ch.level_db.min(ceiling_db);
                if top_db > floor_db {
                    let segment =
                        Rect::from_x_y_ranges(bar.x_range(), y_for(top_db)..=y_for(floor_db));
                    painter.rect_filled(segment, 0.0, color);
                }
                floor_db = ceiling_db;
            }

            if ch.hold_db > MIN_DB {
                painter.hline(
                    bar.x_range(),
                    y_for(ch.hold_db),
                    Stroke::new(2.0, visuals.text_color()),
                );
            }

            let led =
                Rect::from_x_y_ranges(bar.x_range(), rect.top()..=rect.top() + CLIP_LED_HEIGHT);
            painter.rect_filled(led, 1.0, if ch.clipped { RED } else { trough });
        }

        // Tick marks across the bars, drawn last so they sit on top.
        for db in SCALE_MARKS_DB {
            let tick = Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.4));
            painter.hline(bars_left..=rect.right(), y_for(db), tick);
        }

        response
    }
}
