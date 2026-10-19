//! Displays of a tapped sound and of meter values: an oscilloscope, a
//! stereo goniometer, a scrolling waveform colored by frequency (as in
//! MiniMeters), level bars, and graphs of a value over time.

use std::collections::VecDeque;

use eframe::egui::{self, Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};
use engine::TapFrame;

/// Which channels a display shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channels {
    /// Left and right, overlaid.
    Stereo,
    /// What went in (faint) and what came out, each as the average of
    /// both channels: what an effect does to the wave.
    InOut,
}

/// The oscilloscope: the newest `window` frames of `frames` (the tap's
/// output side), started on a rising zero crossing so a steady tone stands
/// still.
pub fn oscilloscope(
    painter: &Painter,
    rect: Rect,
    frames: &[TapFrame],
    window: usize,
    channels: Channels,
    colors: [Color32; 2],
) {
    let window = window.clamp(16, frames.len().max(16));
    if frames.len() < 2 {
        return;
    }
    // Search the older half for the latest rising crossing that still
    // leaves a whole window after it.
    let mid = |f: &TapFrame| 0.5 * (f[2] + f[3]);
    let latest_start = frames.len().saturating_sub(window);
    let earliest = latest_start.saturating_sub(window);
    let start = (earliest + 1..=latest_start)
        .rev()
        .find(|&i| mid(&frames[i - 1]) <= 0.0 && mid(&frames[i]) > 0.0)
        .unwrap_or(latest_start);
    let shown = &frames[start..(start + window).min(frames.len())];
    painter.hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, colors[0].gamma_multiply(0.15)),
    );
    let trace = |pick: &dyn Fn(&TapFrame) -> f32, color: Color32| {
        let n = shown.len().max(2) - 1;
        let points: Vec<Pos2> = shown
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let x = rect.left() + rect.width() * i as f32 / n as f32;
                let y = rect.center().y - pick(f).clamp(-1.0, 1.0) * rect.height() / 2.0;
                pos2(x, y)
            })
            .collect();
        painter.add(Shape::line(points, Stroke::new(1.2, color)));
    };
    match channels {
        Channels::Stereo => {
            trace(&|f| f[2], colors[0]);
            trace(&|f| f[3], colors[1]);
        }
        Channels::InOut => {
            trace(&|f| 0.5 * (f[0] + f[1]), colors[1]);
            trace(&mid, colors[0]);
        }
    }
}

/// The goniometer (vectorscope): each frame as a dot, mono straight up,
/// wide stereo spreading sideways, out-of-phase lying flat.
pub fn goniometer(painter: &Painter, rect: Rect, frames: &[TapFrame], color: Color32) {
    let center = rect.center();
    let radius = rect.width().min(rect.height()) / 2.0;
    let guide = color.gamma_multiply(0.15);
    // The L and R axes, and the mono line.
    for angle in [45f32, 135.0] {
        let d = vec2(angle.to_radians().cos(), -angle.to_radians().sin()) * radius;
        painter.line_segment([center - d, center + d], Stroke::new(1.0, guide));
    }
    painter.vline(center.x, rect.y_range(), Stroke::new(1.0, guide));
    painter.text(
        center + vec2(-radius * 0.72, -radius * 0.72),
        Align2::CENTER_CENTER,
        "L",
        FontId::proportional(crate::theme::text::TINY),
        guide.gamma_multiply(3.0),
    );
    painter.text(
        center + vec2(radius * 0.72, -radius * 0.72),
        Align2::CENTER_CENTER,
        "R",
        FontId::proportional(crate::theme::text::TINY),
        guide.gamma_multiply(3.0),
    );
    let mut mesh = egui::Mesh::default();
    let dot = color.gamma_multiply(0.55);
    // Zoomed so the loudest frame nearly reaches the edge (up to 24 dB),
    // so quiet material still shows its width.
    let loudest = frames
        .iter()
        .fold(0.0f32, |m, f| m.max(f[2].abs()).max(f[3].abs()));
    let zoom = (0.9 / loudest.max(0.063)).clamp(1.0, 16.0);
    let scale = radius * std::f32::consts::FRAC_1_SQRT_2 * zoom / 2.0;
    for f in frames {
        let (l, r) = (f[2], f[3]);
        let x = (r - l) * scale;
        let y = (l + r) * scale;
        let p = center + vec2(x, -y);
        if rect.contains(p) {
            mesh.add_colored_rect(Rect::from_center_size(p, vec2(1.6, 1.6)), dot);
        }
    }
    painter.add(mesh);
}

/// How alike the two channels are, from -1 (opposite) through 0 (unrelated)
/// to 1 (mono).
pub fn correlation(frames: &[TapFrame]) -> f32 {
    let (mut lr, mut ll, mut rr) = (0.0f64, 0.0f64, 0.0f64);
    for f in frames {
        let (l, r) = (f64::from(f[2]), f64::from(f[3]));
        lr += l * r;
        ll += l * l;
        rr += r * r;
    }
    if ll * rr < 1e-12 {
        return 0.0;
    }
    (lr / (ll * rr).sqrt()) as f32
}

/// A horizontal bar from -1 to 1 with a marker at `value`: red side out of
/// phase, green side in phase.
pub fn correlation_bar(painter: &Painter, rect: Rect, value: f32, visuals: &egui::Visuals) {
    painter.rect_filled(rect, 2.0, visuals.extreme_bg_color);
    let x = rect.center().x + value.clamp(-1.0, 1.0) * rect.width() / 2.0;
    let color = if value < 0.0 {
        Color32::from_rgb(230, 90, 70)
    } else {
        Color32::from_rgb(90, 200, 120)
    };
    let bar = Rect::from_x_y_ranges(
        rect.center().x.min(x)..=rect.center().x.max(x),
        rect.y_range(),
    );
    painter.rect_filled(bar, 1.0, color.gamma_multiply(0.7));
    painter.vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(1.0, visuals.weak_text_color()),
    );
    let font = FontId::proportional(crate::theme::text::TINY);
    let weak = visuals.weak_text_color();
    painter.text(
        rect.left_center() + vec2(2.0, 0.0),
        Align2::LEFT_CENTER,
        "-1",
        font.clone(),
        weak,
    );
    painter.text(
        rect.right_center() - vec2(2.0, 0.0),
        Align2::RIGHT_CENTER,
        "+1",
        font,
        weak,
    );
}

/// One column of the scrolling waveform: the range the sound covered and
/// how its energy split between lows, mids and highs.
#[derive(Debug, Clone, Copy, Default)]
struct Column {
    min: f32,
    max: f32,
    bands: [f32; 3],
}

/// A waveform that scrolls right to left, each column colored by where its
/// energy lies: red lows, green mids, blue highs (as MiniMeters does).
#[derive(Clone, Default)]
pub struct Waveform {
    columns: VecDeque<Column>,
    current: Column,
    count: usize,
    seen: u64,
    /// Band-split filter states: two one-pole low-passes.
    low: f32,
    below_high: f32,
}

/// Where the waveform splits lows from mids, and mids from highs.
const LOW_SPLIT_HZ: f32 = 250.0;
const HIGH_SPLIT_HZ: f32 = 3000.0;

impl Waveform {
    /// Takes in the frames that arrived since last time; `per_column` frames
    /// make one column, and `keep` columns are kept.
    pub fn update(
        &mut self,
        new: impl Iterator<Item = TapFrame>,
        total: u64,
        per_column: usize,
        keep: usize,
        sample_rate: f32,
    ) {
        let per_column = per_column.max(1);
        let coefficient = |hz: f32| 1.0 - (-std::f32::consts::TAU * hz / sample_rate).exp();
        let (a_low, a_high) = (coefficient(LOW_SPLIT_HZ), coefficient(HIGH_SPLIT_HZ));
        for f in new {
            let mono = 0.5 * (f[2] + f[3]);
            self.low += a_low * (mono - self.low);
            self.below_high += a_high * (mono - self.below_high);
            let (low, mid, high) = (self.low, self.below_high - self.low, mono - self.below_high);
            let c = &mut self.current;
            if self.count == 0 {
                c.min = mono;
                c.max = mono;
                c.bands = [0.0; 3];
            }
            c.min = c.min.min(mono);
            c.max = c.max.max(mono);
            c.bands[0] += low * low;
            c.bands[1] += mid * mid;
            c.bands[2] += high * high;
            self.count += 1;
            if self.count >= per_column {
                self.columns.push_back(*c);
                self.count = 0;
            }
        }
        while self.columns.len() > keep {
            self.columns.pop_front();
        }
        if !self.low.is_finite() || !self.below_high.is_finite() {
            self.low = 0.0;
            self.below_high = 0.0;
        }
        self.seen = total;
    }

    /// Frames already taken in.
    pub fn seen(&self) -> u64 {
        self.seen
    }

    pub fn paint(&self, painter: &Painter, rect: Rect, color_by_band: bool, plain: Color32) {
        painter.hline(
            rect.x_range(),
            rect.center().y,
            Stroke::new(1.0, plain.gamma_multiply(0.15)),
        );
        let width = rect.width().max(1.0) as usize;
        let half = rect.height() / 2.0;
        let mut mesh = egui::Mesh::default();
        for (i, column) in self.columns.iter().rev().take(width).enumerate() {
            let x = rect.right() - 1.0 - i as f32;
            let top = rect.center().y - column.max.clamp(-1.0, 1.0) * half;
            let bottom = rect.center().y - column.min.clamp(-1.0, 1.0) * half;
            let color = if color_by_band {
                band_color(column.bands)
            } else {
                plain
            };
            mesh.add_colored_rect(
                Rect::from_min_max(pos2(x, top), pos2(x + 1.0, bottom.max(top + 1.0))),
                color,
            );
        }
        painter.add(mesh);
    }
}

/// Red for lows, green for mids, blue for highs, as bright as the loudest.
fn band_color(bands: [f32; 3]) -> Color32 {
    let amplitudes = bands.map(|e| e.sqrt());
    let loudest = amplitudes.iter().copied().fold(1e-12, f32::max);
    let [r, g, b] = amplitudes.map(|a| (a / loudest).powf(0.7));
    let lift = |c: f32| (40.0 + c * 215.0) as u8;
    Color32::from_rgb(lift(r), lift(g), lift(b))
}

/// How wide `count` level bars side by side are ([`level_bars`]), at the
/// theme's width each.
pub fn level_bars_width(count: usize) -> f32 {
    use crate::theme::{size, space};
    let count = count.max(1) as f32;
    count * size::LEVEL_BAR + (count - 1.0) * space::XS
}

/// Level bars side by side, bottom to top from `floor` to `ceiling` dB,
/// green to yellow to red, each with its own color hint if given.
pub fn level_bars(
    painter: &Painter,
    rect: Rect,
    levels_db: &[f32],
    floor: f32,
    ceiling: f32,
    visuals: &egui::Visuals,
) {
    let n = levels_db.len().max(1) as f32;
    let gap = crate::theme::space::XS;
    let width = (rect.width() - gap * (n - 1.0)) / n;
    for (i, db) in levels_db.iter().enumerate() {
        let left = rect.left() + i as f32 * (width + gap);
        let bar = Rect::from_min_size(pos2(left, rect.top()), vec2(width, rect.height()));
        painter.rect_filled(bar, 1.0, visuals.extreme_bg_color);
        let t = ((db - floor) / (ceiling - floor)).clamp(0.0, 1.0);
        if t <= 0.0 {
            continue;
        }
        let fill = Rect::from_min_max(pos2(bar.left(), bar.bottom() - t * bar.height()), bar.max);
        painter.rect_filled(fill, 1.0, level_color(*db));
    }
}

/// Green up to -12 dB, yellow to -3, red above: the master meter's colors.
pub fn level_color(db: f32) -> Color32 {
    use crate::theme::color;
    if db > -3.0 {
        color::METER_HIGH
    } else if db > -12.0 {
        color::METER_MID
    } else {
        color::METER_LOW
    }
}

/// Gain reduction as a bar hanging from the top, `range` dB tall.
pub fn reduction_bar(painter: &Painter, rect: Rect, db: f32, range: f32, visuals: &egui::Visuals) {
    painter.rect_filled(rect, 1.0, visuals.extreme_bg_color);
    let t = (db.abs() / range).clamp(0.0, 1.0);
    let fill = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + t * rect.height()));
    painter.rect_filled(fill, 1.0, REDUCTION);
}

/// The color gain reduction is drawn in everywhere.
pub const REDUCTION: Color32 = Color32::from_rgb(235, 150, 60);

/// A dB scale beside a bar: tick labels at each of `marks`.
pub fn db_scale(
    painter: &Painter,
    rect: Rect,
    marks: &[f32],
    floor: f32,
    ceiling: f32,
    color: Color32,
) {
    for db in marks {
        let t = (db - floor) / (ceiling - floor);
        let y = rect.bottom() - t * rect.height();
        painter.text(
            pos2(rect.right(), y),
            Align2::RIGHT_CENTER,
            format!("{db}"),
            FontId::proportional(crate::theme::text::TINY),
            color,
        );
    }
}

/// A value recorded over time at a steady rate, for a scrolling graph (a
/// compressor's gain reduction, loudness over the last minute).
#[derive(Clone)]
pub struct History {
    values: VecDeque<f32>,
    /// Samples per second.
    rate: f32,
    capacity: usize,
    owed: f32,
}

impl History {
    pub fn new(seconds: f32, rate: f32) -> History {
        let capacity = (seconds * rate).ceil() as usize;
        History {
            values: VecDeque::with_capacity(capacity),
            rate,
            capacity,
            owed: 0.0,
        }
    }

    /// Records `value` for the `dt` seconds since the last call.
    pub fn push(&mut self, value: f32, dt: f32) {
        self.owed += dt.clamp(0.0, 1.0) * self.rate;
        while self.owed >= 1.0 {
            self.owed -= 1.0;
            if self.values.len() == self.capacity {
                self.values.pop_front();
            }
            self.values.push_back(value);
        }
    }

    /// The points of the graph, newest at the right edge, `to_y` placing
    /// each value.
    pub fn points(&self, rect: Rect, to_y: impl Fn(f32) -> f32) -> Vec<Pos2> {
        let step = rect.width() / self.capacity.max(2) as f32;
        let n = self.values.len();
        self.values
            .iter()
            .enumerate()
            .map(|(i, v)| pos2(rect.right() - (n - 1 - i) as f32 * step, to_y(*v)))
            .collect()
    }

    /// Draws the graph as a line, filled down (or up) to `base_y` if
    /// `fill` is visible.
    pub fn paint(
        &self,
        painter: &Painter,
        rect: Rect,
        to_y: impl Fn(f32) -> f32,
        base_y: f32,
        line: Stroke,
        fill: Color32,
    ) {
        let points = self.points(rect, to_y);
        if points.len() < 2 {
            return;
        }
        if fill.a() > 0 {
            let mut mesh = egui::Mesh::default();
            for pair in points.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let top = a.y.min(base_y);
                let bottom = a.y.max(base_y);
                mesh.add_colored_rect(
                    Rect::from_min_max(pos2(a.x, top), pos2(b.x.max(a.x + 0.5), bottom)),
                    fill,
                );
            }
            painter.add(mesh);
        }
        painter.add(Shape::line(points, line));
    }
}

impl Default for History {
    fn default() -> Self {
        History::new(4.0, 60.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_reads_mono_wide_and_inverted() {
        let tone: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.05).sin()).collect();
        let frames = |r: &dyn Fn(f32, usize) -> f32| -> Vec<TapFrame> {
            tone.iter()
                .enumerate()
                .map(|(i, l)| [0.0, 0.0, *l, r(*l, i)])
                .collect()
        };
        assert!((correlation(&frames(&|l, _| l)) - 1.0).abs() < 1e-4);
        assert!((correlation(&frames(&|l, _| -l)) + 1.0).abs() < 1e-4);
        let unrelated = correlation(&frames(&|_, i| (i as f32 * 0.31).cos()));
        assert!(unrelated.abs() < 0.2, "{unrelated}");
    }

    #[test]
    fn waveform_columns_take_the_range_and_color_by_band() {
        let rate = 48_000.0;
        let low: Vec<TapFrame> = (0..4800)
            .map(|i| {
                let s = 0.5 * (std::f32::consts::TAU * 60.0 * i as f32 / rate).sin();
                [0.0, 0.0, s, s]
            })
            .collect();
        let mut waveform = Waveform::default();
        // Columns longer than the tone's period catch both its peaks.
        waveform.update(low.iter().copied(), 4800, 960, 100, rate);
        assert_eq!(waveform.columns.len(), 5);
        let last = waveform.columns.back().unwrap();
        assert!(last.max > 0.4 && last.min < -0.4);
        let color = band_color(last.bands);
        assert!(color.r() > color.g() && color.r() > color.b(), "{color:?}");
    }

    #[test]
    fn history_records_at_a_steady_rate() {
        let mut history = History::new(1.0, 10.0);
        history.push(1.0, 0.25);
        history.push(2.0, 0.25);
        assert_eq!(history.values.len(), 5);
        history.push(3.0, 5.0);
        assert_eq!(history.values.len(), 10, "capped");
        assert_eq!(history.values.back(), Some(&3.0));
    }
}
