//! Spectra and spectrograms drawn as PNG images: what the MCP server's
//! `plot` tool returns, and what the Analyzer's Save image is meant to
//! write. They are drawn in software (tiny-skia, text in the app's Inter
//! through ab_glyph) on the app's dark palette, with the live spectrum
//! display's scales (`ui/src/widgets/spectrum.rs`): frequency on a log
//! axis, levels in dB where a full-scale sine reads 0, and an optional
//! slope pivoting at 1 kHz that makes a balanced mix look level.
//!
//! Each image comes with JSON saying where its plot sits in the picture and
//! the ranges and scales of its axes, so whoever reads it can turn pixels
//! back into hertz, decibels and beats.

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont, point};
use serde_json::{Value, json};
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

use crate::Signal;
use crate::report::{self, power_db};
use crate::spectrum::{self, Spectrum, Stft};

/// The app's colors, copied from `canopy-widgets/src/theme.rs` (`surface` and
/// `section`): this crate draws without egui, so it can't name them there.
mod palette {
    pub type Rgb = [u8; 3];

    pub const CANVAS: Rgb = [16, 18, 21];
    pub const PANEL: Rgb = [25, 27, 31];
    pub const BORDER: Rgb = [40, 43, 49];
    pub const TEXT: Rgb = [222, 226, 232];
    pub const TEXT_WEAK: Rgb = [140, 147, 158];
    pub const ACCENT: Rgb = [92, 178, 238];
    pub const ACCENT_FILL: Rgb = [40, 104, 150];

    pub const OSC_A: Rgb = [92, 178, 238];
    pub const OSC_B: Rgb = [80, 205, 190];
    pub const OSC_C: Rgb = [175, 145, 245];
    pub const FILTER: Rgb = [240, 125, 95];
    pub const ENVELOPE: Rgb = [110, 210, 150];
    pub const MACRO: Rgb = [245, 165, 80];

    pub const WHITE: Rgb = [255, 255, 255];

    /// Overlaid curves take these in turn: the Analyzer's own two first.
    pub const CURVES: [Rgb; 6] = [OSC_A, MACRO, OSC_B, FILTER, OSC_C, ENVELOPE];
}

use palette::Rgb;

const INTER: &[u8] = include_bytes!("../../canopy-widgets/assets/fonts/Inter-Regular.ttf");

/// Tick labels and markers' text, in pixels per em.
const LABEL: f32 = 12.0;
/// The legend's names.
const LEGEND: f32 = 13.0;
/// Inter's capital height, in ems: text is aligned on its capitals.
const CAP_HEIGHT: f32 = 0.727;

pub const DEFAULT_WIDTH: u32 = 1024;
pub const DEFAULT_HEIGHT: u32 = 512;
/// The largest image drawn: a reader such as a language model pays for
/// every pixel.
pub const MAX_WIDTH: u32 = 1600;
pub const MAX_HEIGHT: u32 = 1000;
/// The smallest image whose labels still fit.
pub const MIN_WIDTH: u32 = 320;
pub const MIN_HEIGHT: u32 = 200;

/// How much a spectrum's line is smoothed: each point looks at the bins
/// within a twelfth of an octave either side (see [`smoothed_level`]), so
/// a tone keeps its level and a comb of harmonics reads as a shape.
pub const SMOOTHING_OCTAVES: f32 = 1.0 / 6.0;

/// The frequency a slope pivots on.
pub const SLOPE_PIVOT_HZ: f32 = 1000.0;

/// An image's size and the ranges it shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plot {
    pub width: u32,
    pub height: u32,
    /// The frequency axis, lowest and highest, in Hz.
    pub freq: (f32, f32),
    /// The level axis (a spectrogram's color scale), floor and ceiling, in
    /// dB.
    pub db: (f32, f32),
    /// dB per octave added above [`SLOPE_PIVOT_HZ`] and taken away below
    /// it, as the Analyzer's Slope does; 0 draws levels as measured. Only
    /// spectra use it.
    pub slope: f32,
}

impl Default for Plot {
    fn default() -> Plot {
        Plot {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            freq: (spectrum::LOWEST_HZ, spectrum::HIGHEST_HZ),
            db: (-90.0, 0.0),
            slope: 0.0,
        }
    }
}

impl Plot {
    /// This plot within what can be drawn: the size between the smallest
    /// and largest, at least an octave of frequencies above 1 Hz, at least
    /// 6 dB of levels, and the defaults for whatever isn't a number.
    pub fn capped(&self) -> Plot {
        let default = Plot::default();
        let finite = |pair: (f32, f32), or: (f32, f32)| {
            if pair.0.is_finite() && pair.1.is_finite() {
                pair
            } else {
                or
            }
        };
        let (low, high) = finite(self.freq, default.freq);
        let low = low.clamp(1.0, 96_000.0);
        let (floor, ceiling) = finite(self.db, default.db);
        Plot {
            width: self.width.clamp(MIN_WIDTH, MAX_WIDTH),
            height: self.height.clamp(MIN_HEIGHT, MAX_HEIGHT),
            freq: (low, high.max(2.0 * low)),
            db: (floor, ceiling.max(floor + 6.0)),
            slope: if self.slope.is_finite() {
                self.slope
            } else {
                0.0
            },
        }
    }
}

/// Where the plot sits in the image, in pixels.
#[derive(Debug, Clone, Copy)]
struct Area {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl Area {
    /// `plot`'s image less the margins.
    fn inside(plot: &Plot, left: f32, top: f32, right: f32, bottom: f32) -> Area {
        Area {
            left,
            top,
            right: plot.width as f32 - right,
            bottom: plot.height as f32 - bottom,
        }
    }

    fn width(&self) -> f32 {
        self.right - self.left
    }

    fn height(&self) -> f32 {
        self.bottom - self.top
    }

    fn json(&self) -> Value {
        json!({
            "left": self.left as i64,
            "top": self.top as i64,
            "right": self.right as i64,
            "bottom": self.bottom as i64,
        })
    }
}

/// A log frequency axis from `low` to `high` Hz over pixels `from` to `to`
/// (`to` above `from` on a vertical axis).
#[derive(Debug, Clone, Copy)]
struct FreqAxis {
    low: f32,
    high: f32,
    from: f32,
    to: f32,
}

impl FreqAxis {
    fn pos(&self, hz: f32) -> f32 {
        let t = (hz.max(1e-3) / self.low).ln() / (self.high / self.low).ln();
        self.from + t * (self.to - self.from)
    }

    fn hz(&self, pos: f32) -> f32 {
        let t = (pos - self.from) / (self.to - self.from);
        self.low * (self.high / self.low).powf(t)
    }

    /// Lines at 1–9 × each decade, the decades strong; labels at 1, 2 and 5.
    fn ticks(&self) -> Vec<(f32, bool, bool)> {
        let mut ticks = Vec::new();
        let mut decade = 10f32.powf(self.low.log10().floor());
        while decade <= self.high {
            for step in 1..10 {
                let hz = decade * step as f32;
                if (self.low..=self.high).contains(&hz) {
                    ticks.push((hz, step == 1, matches!(step, 1 | 2 | 5)));
                }
            }
            decade *= 10.0;
        }
        ticks
    }
}

/// "20", "500", "1k", "2.5k": a frequency as the spectrum display labels
/// its lines.
fn tick_label(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{}k", hz / 1000.0)
    } else {
        format!("{hz}")
    }
}

/// "440 Hz", "1.25 kHz": a peak's frequency.
fn hz_label(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.2} kHz", hz / 1000.0)
    } else {
        format!("{hz:.0} Hz")
    }
}

/// A linear axis from `low` to `high` over pixels `from` to `to`.
#[derive(Debug, Clone, Copy)]
struct LinearAxis {
    low: f32,
    high: f32,
    from: f32,
    to: f32,
}

impl LinearAxis {
    fn pos(&self, value: f32) -> f32 {
        self.from + (value - self.low) / (self.high - self.low) * (self.to - self.from)
    }

    /// The levels a line is drawn at: round numbers, at most ten of them.
    fn ticks(&self) -> Vec<f32> {
        let range = self.high - self.low;
        let step = [3.0, 6.0, 10.0, 20.0, 30.0, 50.0, 100.0]
            .into_iter()
            .find(|step| range / step <= 10.0)
            .unwrap_or(range / 5.0);
        let mut ticks = Vec::new();
        let mut value = (self.low / step).ceil() * step;
        while value <= self.high + 1e-3 {
            ticks.push(value);
            value += step;
        }
        ticks
    }
}

fn color(rgb: Rgb, alpha: f32) -> Color {
    Color::from_rgba8(
        rgb[0],
        rgb[1],
        rgb[2],
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

fn hex(rgb: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let channel =
        |i: usize| (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t).round() as u8;
    [channel(0), channel(1), channel(2)]
}

/// Where text is placed relative to the point given: its start, middle or
/// end across; its capitals' top, middle or baseline down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Start,
    Centre,
    End,
}

/// An image being drawn: always opaque, so its premultiplied pixels are
/// plain RGB.
struct Canvas {
    pixmap: Pixmap,
    font: FontRef<'static>,
    /// Text already placed, so labels don't print over each other.
    taken: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Canvas {
        let mut pixmap = Pixmap::new(width, height).expect("plot sizes are capped and non-zero");
        pixmap.fill(color(palette::PANEL, 1.0));
        Canvas {
            pixmap,
            font: FontRef::try_from_slice(INTER).expect("the bundled Inter parses"),
            taken: Vec::new(),
        }
    }

    fn paint(rgb: Rgb, alpha: f32) -> Paint<'static> {
        let mut paint = Paint::default();
        paint.set_color(color(rgb, alpha));
        paint.anti_alias = true;
        paint
    }

    /// Fills a rectangle, its edges rounded to whole pixels: rectangles
    /// here are grid lines and fills, which should be crisp.
    fn rect(&mut self, left: f32, top: f32, right: f32, bottom: f32, rgb: Rgb, alpha: f32) {
        let (left, top) = (left.round(), top.round());
        let (right, bottom) = (right.round().max(left + 1.0), bottom.round().max(top + 1.0));
        if let Some(rect) = Rect::from_ltrb(left, top, right, bottom) {
            let mut paint = Canvas::paint(rgb, alpha);
            paint.anti_alias = false;
            self.pixmap
                .fill_rect(rect, &paint, Transform::identity(), None);
        }
    }

    /// A hairline across the pixel column `x` falls in.
    fn vline(&mut self, x: f32, top: f32, bottom: f32, rgb: Rgb, alpha: f32) {
        let x = x.floor();
        self.rect(x, top, x + 1.0, bottom, rgb, alpha);
    }

    fn hline(&mut self, y: f32, left: f32, right: f32, rgb: Rgb, alpha: f32) {
        let y = y.floor();
        self.rect(left, y, right, y + 1.0, rgb, alpha);
    }

    /// A display, as the app draws one: the canvas color inside a border.
    fn display(&mut self, area: Area) {
        self.rect(
            area.left - 1.0,
            area.top - 1.0,
            area.right + 1.0,
            area.bottom + 1.0,
            palette::BORDER,
            1.0,
        );
        self.rect(
            area.left,
            area.top,
            area.right,
            area.bottom,
            palette::CANVAS,
            1.0,
        );
    }

    fn polyline(&mut self, points: &[(f32, f32)], width: f32, rgb: Rgb, alpha: f32) {
        let mut path = PathBuilder::new();
        for (i, &(x, y)) in points.iter().enumerate() {
            if i == 0 {
                path.move_to(x, y);
            } else {
                path.line_to(x, y);
            }
        }
        if let Some(path) = path.finish() {
            let stroke = Stroke {
                width,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            self.pixmap.stroke_path(
                &path,
                &Canvas::paint(rgb, alpha),
                &stroke,
                Transform::identity(),
                None,
            );
        }
    }

    fn polygon(&mut self, points: &[(f32, f32)], rgb: Rgb, alpha: f32) {
        let mut path = PathBuilder::new();
        for (i, &(x, y)) in points.iter().enumerate() {
            if i == 0 {
                path.move_to(x, y);
            } else {
                path.line_to(x, y);
            }
        }
        path.close();
        if let Some(path) = path.finish() {
            self.pixmap.fill_path(
                &path,
                &Canvas::paint(rgb, alpha),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    fn dot(&mut self, x: f32, y: f32, radius: f32, rgb: Rgb) {
        let mut path = PathBuilder::new();
        path.push_circle(x, y, radius);
        if let Some(path) = path.finish() {
            let paint = Canvas::paint(rgb, 1.0);
            self.pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
            let ring = Stroke {
                width: 1.5,
                ..Stroke::default()
            };
            self.pixmap.stroke_path(
                &path,
                &Canvas::paint(palette::CANVAS, 1.0),
                &ring,
                Transform::identity(),
                None,
            );
        }
    }

    /// Mixes `rgb` into the pixel at (`x`, `y`) by `alpha`.
    fn blend(&mut self, x: i32, y: i32, rgb: Rgb, alpha: f32) {
        let (width, height) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        if x < 0 || y < 0 || x >= width || y >= height || alpha <= 0.0 {
            return;
        }
        let i = (y * width + x) as usize * 4;
        let data = self.pixmap.data_mut();
        let alpha = alpha.min(1.0);
        for c in 0..3 {
            let old = f32::from(data[i + c]);
            data[i + c] = (old + (f32::from(rgb[c]) - old) * alpha).round() as u8;
        }
    }

    fn set(&mut self, x: usize, y: usize, rgb: Rgb) {
        let i = (y * self.pixmap.width() as usize + x) * 4;
        self.pixmap.data_mut()[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }

    fn scale(&self, size: f32) -> PxScale {
        let em = self.font.units_per_em().unwrap_or(1000.0);
        PxScale::from(size * self.font.height_unscaled() / em)
    }

    /// Each glyph of `text` and where it starts, and the width of it all.
    fn layout(&self, size: f32, text: &str) -> (Vec<(GlyphId, f32)>, f32) {
        let font = self.font.as_scaled(self.scale(size));
        let mut glyphs = Vec::with_capacity(text.len());
        let (mut caret, mut previous) = (0.0, None);
        for c in text.chars() {
            let id = font.glyph_id(c);
            if let Some(previous) = previous {
                caret += font.kern(previous, id);
            }
            glyphs.push((id, caret));
            caret += font.h_advance(id);
            previous = Some(id);
        }
        (glyphs, caret)
    }

    fn text_width(&self, size: f32, text: &str) -> f32 {
        self.layout(size, text).1
    }

    /// Where `text` would go, aligned to (`x`, `y`): left, top, right,
    /// bottom of its capitals.
    fn text_box(&self, x: f32, y: f32, align: (Align, Align), size: f32, text: &str) -> [f32; 4] {
        let width = self.text_width(size, text);
        let cap = CAP_HEIGHT * size;
        let left = match align.0 {
            Align::Start => x,
            Align::Centre => x - width / 2.0,
            Align::End => x - width,
        };
        let top = match align.1 {
            Align::Start => y,
            Align::Centre => y - cap / 2.0,
            Align::End => y - cap,
        };
        [left, top, left + width, top + cap]
    }

    fn text(&mut self, x: f32, y: f32, align: (Align, Align), size: f32, rgb: Rgb, text: &str) {
        let [left, top, ..] = self.text_box(x, y, align, size, text);
        let baseline = (top + CAP_HEIGHT * size).round();
        let left = left.round();
        let scale = self.scale(size);
        let (glyphs, _) = self.layout(size, text);
        let mut coverage: Vec<(i32, i32, f32)> = Vec::new();
        for (id, at) in glyphs {
            let glyph = id.with_scale_and_position(scale, point(left + at, baseline));
            if let Some(outline) = self.font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, c| {
                    coverage.push((
                        bounds.min.x as i32 + gx as i32,
                        bounds.min.y as i32 + gy as i32,
                        c,
                    ));
                });
            }
        }
        for (px, py, c) in coverage {
            self.blend(px, py, rgb, c);
        }
    }

    /// Draws `text` at the labels' size unless it would overlap text placed
    /// before or leave `within` (left, right); says whether it did.
    fn label(
        &mut self,
        x: f32,
        y: f32,
        align: (Align, Align),
        rgb: Rgb,
        text: &str,
        within: (f32, f32),
    ) -> bool {
        let b = self.text_box(x, y, align, LABEL, text);
        let gap = 4.0;
        if b[0] < within.0 || b[2] > within.1 {
            return false;
        }
        let clear = self.taken.iter().all(|t| {
            b[2] + gap <= t[0] || t[2] + gap <= b[0] || b[3] + gap <= t[1] || t[3] + gap <= b[1]
        });
        if clear {
            self.taken.push(b);
            self.text(x, y, align, LABEL, rgb, text);
        }
        clear
    }

    fn png(&self) -> Vec<u8> {
        let rgb: Vec<u8> = self
            .pixmap
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, self.pixmap.width(), self.pixmap.height());
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        // Writing to memory fails only on a wrong size, which can't happen.
        let mut writer = encoder
            .write_header()
            .expect("a PNG header writes to memory");
        writer
            .write_image_data(&rgb)
            .expect("an RGB image of its own size encodes");
        drop(writer);
        out
    }
}

/// The level from `low` to `high` Hz in a dB spectrum `bin_hz` apart, as
/// the spectrum display reads it: the loudest bin there, or between the
/// two nearest where bins are wider than the range.
fn level_at(bins: &[f32], bin_hz: f32, low: f32, high: f32) -> f32 {
    let last = bins.len().saturating_sub(1);
    let (from, to) = (low / bin_hz, high / bin_hz);
    if to.floor() > from.ceil() {
        let a = (from.ceil() as usize).min(last);
        let b = (to.floor() as usize).min(last);
        bins[a..=b.max(a)].iter().copied().fold(f32::MIN, f32::max)
    } else {
        let centre = (from + to) / 2.0;
        let i = (centre.floor().max(0.0) as usize).min(last);
        let t = (centre - i as f32).clamp(0.0, 1.0);
        bins[i] + (bins[(i + 1).min(last)] - bins[i]) * t
    }
}

/// The level drawn at `hz` from a dB spectrum `bin_hz` apart, smoothed
/// over `octaves`: each bin within half that either side counts for less
/// the further off-centre it is (a Hann taper in log frequency, taken in
/// dB) and the loudest wins. A tone so keeps its level at its frequency and
/// falls away smoothly to either side rather than as a flat-topped block;
/// where bins are further apart than that, the line runs straight between
/// them.
fn smoothed_level(bins: &[f32], bin_hz: f32, hz: f32, octaves: f32) -> f32 {
    let last = bins.len().saturating_sub(1);
    let half = octaves / 2.0;
    let (low, high) = (hz * 2f32.powf(-half), hz * 2f32.powf(half));
    let first = (low / bin_hz).ceil() as usize;
    let end = ((high / bin_hz).floor() as usize).min(last);
    let mut loudest = level_at(bins, bin_hz, hz, hz);
    for (k, &db) in bins.iter().enumerate().take(end + 1).skip(first) {
        let off = (k as f32 * bin_hz / hz).log2() / half;
        let weight = 0.5 + 0.5 * (std::f32::consts::PI * off).cos();
        if weight > 1e-4 {
            loudest = loudest.max(db + 20.0 * weight.log10());
        }
    }
    loudest
}

/// Frequency lines across `area`, labelled below it or beside it, from
/// `axis` (horizontal when `across`).
fn frequency_grid(canvas: &mut Canvas, area: Area, axis: &FreqAxis, across: bool, alpha: f32) {
    for (hz, strong, labelled) in axis.ticks() {
        let pos = axis.pos(hz);
        let line = if strong { alpha * 1.8 } else { alpha };
        if across {
            canvas.vline(pos, area.top, area.bottom, palette::TEXT_WEAK, line);
        } else if labelled {
            canvas.hline(pos, area.left, area.right, palette::TEXT_WEAK, line);
        }
        if labelled {
            let text = tick_label(hz);
            if across {
                // The last label would run into the unit at the corner.
                if pos < area.right - 28.0 {
                    canvas.label(
                        pos,
                        area.bottom + 7.0,
                        (Align::Centre, Align::Start),
                        palette::TEXT_WEAK,
                        &text,
                        (0.0, f32::MAX),
                    );
                }
            } else {
                canvas.hline(pos, area.left - 4.0, area.left, palette::TEXT_WEAK, 0.8);
                canvas.label(
                    area.left - 7.0,
                    pos,
                    (Align::End, Align::Centre),
                    palette::TEXT_WEAK,
                    &text,
                    (0.0, f32::MAX),
                );
            }
        }
    }
}

/// Draws `curves` (a name and an average spectrum each) over one another,
/// in the app's section colors, with a legend when there are names; each
/// curve's `peaks` strongest resonant peaks are marked with their
/// frequency. Returns the PNG and the axes as JSON:
///
/// ```json
/// { "kind": "spectrum", "width": 1024, "height": 512,
///   "plot": { "left": 48, "top": 34, "right": 1008, "bottom": 484 },
///   "x": { "scale": "log", "from_hz": 20, "to_hz": 20000 },
///   "y": { "scale": "linear", "from_db": -90.0, "to_db": 0.0,
///          "slope_db_per_octave": 0.0, "slope_pivot_hz": 1000 },
///   "smoothing_octaves": 0.167,
///   "curves": [{ "name": "Vocal", "color": "#5cb2ee",
///                "peaks": [{ "hz": 1000, "db": -6.0 }] }] }
/// ```
///
/// `plot` holds the plot's edges in pixels (`x` runs left to right, `y`
/// bottom to top); a peak's `db` is its level as measured, without the
/// slope.
pub fn spectrum_png(curves: &[(String, Spectrum)], plot: &Plot, peaks: usize) -> (Vec<u8>, Value) {
    let plot = plot.capped();
    let mut canvas = Canvas::new(plot.width, plot.height);
    let area = Area::inside(&plot, 48.0, 34.0, 16.0, 28.0);
    canvas.display(area);
    let x = FreqAxis {
        low: plot.freq.0,
        high: plot.freq.1,
        from: area.left,
        to: area.right,
    };
    let y = LinearAxis {
        low: plot.db.0,
        high: plot.db.1,
        from: area.bottom,
        to: area.top,
    };

    frequency_grid(&mut canvas, area, &x, true, 0.16);
    canvas.text(
        area.right,
        area.bottom + 7.0,
        (Align::End, Align::Start),
        LABEL,
        palette::TEXT_WEAK,
        "Hz",
    );
    for db in y.ticks() {
        let pos = y.pos(db);
        canvas.hline(pos, area.left, area.right, palette::TEXT_WEAK, 0.16);
        canvas.label(
            area.left - 7.0,
            pos,
            (Align::End, Align::Centre),
            palette::TEXT_WEAK,
            &format!("{db}"),
            (0.0, f32::MAX),
        );
    }
    canvas.text(
        area.left - 7.0,
        area.top - 12.0,
        (Align::End, Align::End),
        LABEL,
        palette::TEXT_WEAK,
        "dB",
    );

    // Each curve's line and peak hold, a point per pixel column.
    let trace = |levels: &[f32], spectrum: &Spectrum| -> Vec<(f32, f32)> {
        let mut points = Vec::with_capacity(area.width() as usize + 1);
        let mut px = area.left;
        while px <= area.right {
            let hz = x.hz(px);
            let db = smoothed_level(levels, spectrum.bin_hz(), hz, SMOOTHING_OCTAVES)
                + plot.slope * (hz / SLOPE_PIVOT_HZ).log2();
            points.push((px, y.pos(db).clamp(area.top, area.bottom)));
            px += 1.0;
        }
        points
    };
    let colors: Vec<Rgb> = (0..curves.len())
        .map(|i| palette::CURVES[i % palette::CURVES.len()])
        .collect();
    let lines: Vec<Vec<(f32, f32)>> = curves.iter().map(|(_, s)| trace(&s.bins, s)).collect();
    let fill = if curves.len() > 1 { 0.07 } else { 0.16 };
    for (points, &rgb) in lines.iter().zip(&colors) {
        let mut shape = points.clone();
        shape.push((area.right, area.bottom));
        shape.push((area.left, area.bottom));
        canvas.polygon(&shape, rgb, fill);
    }
    for ((_, spectrum), &rgb) in curves.iter().zip(&colors) {
        canvas.polyline(&trace(&spectrum.peak_hold, spectrum), 1.0, rgb, 0.4);
    }
    for (points, &rgb) in lines.iter().zip(&colors) {
        canvas.polyline(points, 1.5, rgb, 1.0);
    }

    // Peaks: a dot on the line, the frequency above it (or below, if
    // there's no room).
    let mut curve_json = Vec::new();
    for ((name, spectrum), &rgb) in curves.iter().zip(&colors) {
        let found: Vec<spectrum::Peak> = spectrum::peaks(spectrum, peaks)
            .into_iter()
            .filter(|p| (plot.freq.0..=plot.freq.1).contains(&p.hz))
            .collect();
        for peak in &found {
            let px = x.pos(peak.hz);
            let db = peak.db + plot.slope * (peak.hz / SLOPE_PIVOT_HZ).log2();
            let py = y.pos(db).clamp(area.top, area.bottom);
            canvas.dot(px, py, 3.0, rgb);
            let text = hz_label(peak.hz);
            let within = (area.left, area.right);
            let above = py - 8.0 - CAP_HEIGHT * LABEL >= area.top
                && canvas.label(
                    px,
                    py - 8.0,
                    (Align::Centre, Align::End),
                    palette::TEXT,
                    &text,
                    within,
                );
            if !above {
                canvas.label(
                    px,
                    py + 8.0,
                    (Align::Centre, Align::Start),
                    palette::TEXT,
                    &text,
                    within,
                );
            }
        }
        curve_json.push(json!({
            "name": name,
            "color": hex(rgb),
            "peaks": found.iter().map(|p| json!({ "hz": p.hz, "db": p.db })).collect::<Vec<_>>(),
        }));
    }

    // The legend, above the plot.
    if curves.iter().any(|(name, _)| !name.is_empty()) {
        let mut at = area.left;
        let middle = area.top - 16.0;
        for ((name, _), &rgb) in curves.iter().zip(&colors) {
            canvas.rect(at, middle - 5.0, at + 10.0, middle + 5.0, rgb, 1.0);
            canvas.text(
                at + 16.0,
                middle,
                (Align::Start, Align::Centre),
                LEGEND,
                palette::TEXT,
                name,
            );
            at += 16.0 + canvas.text_width(LEGEND, name) + 20.0;
        }
    }

    let axes = report::round(json!({
        "kind": "spectrum",
        "width": plot.width,
        "height": plot.height,
        "plot": area.json(),
        "x": { "scale": "log", "from_hz": plot.freq.0, "to_hz": plot.freq.1 },
        "y": {
            "scale": "linear",
            "from_db": plot.db.0,
            "to_db": plot.db.1,
            "slope_db_per_octave": plot.slope,
            "slope_pivot_hz": SLOPE_PIVOT_HZ,
        },
        "smoothing_octaves": SMOOTHING_OCTAVES,
        "curves": curve_json,
    }));
    (canvas.png(), axes)
}

/// How many columns a spectrogram drawn on `plot` has room for: one per
/// pixel across its plot. A spectrogram of that many draws sharpest.
pub fn spectrogram_columns(plot: &Plot) -> usize {
    let plot = plot.capped();
    spectrogram_area(&plot).width() as usize
}

fn spectrogram_area(plot: &Plot) -> Area {
    Area::inside(plot, 48.0, 34.0, 64.0, 28.0)
}

/// Levels over time and frequency.
#[derive(Debug, Clone, PartialEq)]
pub struct Spectrogram {
    /// dB per bin (0 Hz to Nyquist, `rows` of them) per column, column by
    /// column: `db[column * rows + row]`.
    pub db: Vec<f32>,
    pub columns: usize,
    pub rows: usize,
}

impl Spectrogram {
    /// One column's levels, 0 Hz first.
    pub fn column(&self, column: usize) -> &[f32] {
        &self.db[column * self.rows..(column + 1) * self.rows]
    }

    /// The transform's length, from the rows.
    pub fn fft(&self) -> usize {
        (self.rows.max(2) - 1) * 2
    }
}

/// `signal`'s spectrogram (both channels averaged) in `columns` equal
/// slices of time, from Hann windows `fft` frames long (a power of two,
/// 64 to 65536). Each column holds the loudest each bin got in the windows
/// centred within it, half overlapping, so a short event shows however
/// long a column lasts.
pub fn spectrogram(signal: &Signal, fft: usize, columns: usize) -> Spectrogram {
    let fft = fft.clamp(64, 65_536).next_power_of_two();
    let columns = columns.max(1);
    let mut stft = Stft::new(fft);
    let rows = stft.bins();
    let mono = signal.mono();
    let frames = mono.len();
    // Silence either side, so windows centred near the ends fit.
    let mut padded = vec![0.0f32; frames + fft];
    padded[fft / 2..fft / 2 + frames].copy_from_slice(&mono);

    let mut db = Vec::with_capacity(columns * rows);
    let mut power = vec![0.0f32; rows];
    let mut loudest = vec![0.0f32; rows];
    for c in 0..columns {
        let (start, end) = (c * frames / columns, (c + 1) * frames / columns);
        loudest.fill(0.0);
        if end > start {
            let hop = (fft / 2).min(end - start);
            let mut centre = start + (end - start) % hop / 2 + hop / 2;
            while centre < end {
                stft.power(&padded[centre..centre + fft], &mut power);
                for (l, &p) in loudest.iter_mut().zip(&power) {
                    *l = l.max(p);
                }
                centre += hop;
            }
        }
        db.extend(loudest.iter().map(|&p| power_db(f64::from(p)) as f32));
    }
    Spectrogram { db, columns, rows }
}

/// Something to mark on a spectrogram, in beats from the song's start.
#[derive(Debug, Clone, PartialEq)]
pub enum Marker {
    /// A stretch of time: a sibilant, a clash's worst bars, the loop.
    Span { from: f64, to: f64, label: String },
    /// A moment: a transient.
    At { beat: f64, label: String },
}

/// The spectrogram's colors, from silence to the ceiling: the canvas, then
/// the accent's blues, the Macro orange, white. Violet sits between the
/// blue and the orange, which would otherwise meet in a grey that reads as
/// quieter than the blue below it.
const COLOR_MAP: [(f32, Rgb); 6] = [
    (0.0, palette::CANVAS),
    (0.28, palette::ACCENT_FILL),
    (0.52, palette::ACCENT),
    (0.7, palette::OSC_C),
    (0.87, palette::MACRO),
    (1.0, palette::WHITE),
];

fn heat(t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    for pair in COLOR_MAP.windows(2) {
        let ((a, from), (b, to)) = (pair[0], pair[1]);
        if t <= b {
            return mix(from, to, (t - a) / (b - a));
        }
    }
    palette::WHITE
}

/// Draws `spectrogram` (of `signal`, which gives its place in the song):
/// time across, labelled in bars; log frequency up; level as color, with a
/// scale beside it; `markers` over it. Returns the PNG and the axes as
/// JSON:
///
/// ```json
/// { "kind": "spectrogram", "width": 1024, "height": 512,
///   "plot": { "left": 48, "top": 34, "right": 960, "bottom": 484 },
///   "x": { "scale": "linear", "from_beat": 4.0, "to_beat": 20.0,
///          "from": "2.1", "to": "6.1", "seconds": 8.0 },
///   "y": { "scale": "log", "from_hz": 20, "to_hz": 20000 },
///   "color": { "from_db": -90.0, "to_db": 0.0,
///              "map": "dark, blue, light blue, violet, orange, white" },
///   "fft": 2048, "columns": 912,
///   "markers": [{ "kind": "span", "from_beat": 5.0, "to_beat": 5.25, "label": "ess" },
///               { "kind": "at", "beat": 8.0, "label": "onset" }] }
/// ```
pub fn spectrogram_png(
    spectrogram: &Spectrogram,
    signal: &Signal,
    plot: &Plot,
    markers: &[Marker],
) -> (Vec<u8>, Value) {
    let plot = plot.capped();
    let mut canvas = Canvas::new(plot.width, plot.height);
    let area = spectrogram_area(&plot);
    canvas.display(area);
    let y = FreqAxis {
        low: plot.freq.0,
        high: plot.freq.1,
        from: area.bottom,
        to: area.top,
    };
    let frames = signal.frames().max(1) as f64;
    let x_of_beat =
        |beat: f64| -> f32 { area.left + (signal.frame_at(beat) / frames) as f32 * area.width() };
    let (floor, ceiling) = plot.db;
    let bin_hz = signal.rate as f32 / spectrogram.fft() as f32;

    // The levels, pixel by pixel.
    let (left, top) = (area.left as usize, area.top as usize);
    let (width, height) = (area.width() as usize, area.height() as usize);
    for row in 0..height {
        let py = (top + row) as f32;
        let (low, high) = (y.hz(py + 1.0), y.hz(py));
        for col in 0..width {
            let column = (col * spectrogram.columns / width.max(1)).min(spectrogram.columns - 1);
            let db = level_at(spectrogram.column(column), bin_hz, low, high);
            canvas.set(
                left + col,
                top + row,
                heat((db - floor) / (ceiling - floor)),
            );
        }
    }

    // Frequencies up the left.
    frequency_grid(&mut canvas, area, &y, false, 0.1);
    canvas.text(
        area.left - 7.0,
        area.top - 12.0,
        (Align::End, Align::End),
        LABEL,
        palette::TEXT_WEAK,
        "Hz",
    );

    // Bars across the bottom: every bar's line where they're far enough
    // apart, a number every so many bars so the numbers don't crowd.
    let start = signal.start_beat;
    let end = signal.beat_at(signal.frames() as f64);
    let bar_beats = signal.bar_beats();
    let bar_px = (x_of_beat(start + bar_beats) - x_of_beat(start)).max(1e-3);
    let mut stride = 1i64;
    while bar_px * (stride as f32) < 36.0 {
        stride *= 2;
    }
    let (first, last) = signal.bars();
    let mut lines = 0;
    for bar in first..=last + 1 {
        let beat = (bar - 1) as f64 * bar_beats;
        if beat < start - 1e-9 || beat > end + 1e-9 {
            continue;
        }
        let px = x_of_beat(beat);
        let numbered = (bar - 1) % stride == 0;
        if numbered || bar_px >= 6.0 {
            canvas.vline(
                px,
                area.top,
                area.bottom,
                palette::WHITE,
                if numbered { 0.22 } else { 0.1 },
            );
            lines += 1;
        }
        if numbered {
            canvas.vline(px, area.bottom, area.bottom + 4.0, palette::TEXT_WEAK, 0.8);
            canvas.label(
                px + 3.0,
                area.bottom + 7.0,
                (Align::Start, Align::Start),
                palette::TEXT_WEAK,
                &bar.to_string(),
                (area.left, area.right),
            );
        }
    }
    // Under two bar lines, beats say where things are.
    if lines < 2 {
        let mut beat = start.ceil();
        while beat <= end + 1e-9 {
            let px = x_of_beat(beat);
            canvas.vline(px, area.top, area.bottom, palette::WHITE, 0.12);
            canvas.label(
                px + 3.0,
                area.bottom + 7.0,
                (Align::Start, Align::Start),
                palette::TEXT_WEAK,
                &signal.bar_beat(beat),
                (area.left, area.right),
            );
            beat += 1.0;
        }
    }
    canvas.text(
        area.left - 7.0,
        area.bottom + 7.0,
        (Align::End, Align::Start),
        LABEL,
        palette::TEXT_WEAK,
        "Bar",
    );

    // The color scale on the right.
    let scale_left = area.right + 12.0;
    for row in 0..height {
        let t = 1.0 - (row as f32 + 0.5) / height as f32;
        let py = area.top + row as f32;
        canvas.rect(scale_left, py, scale_left + 10.0, py + 1.0, heat(t), 1.0);
    }
    let level = LinearAxis {
        low: floor,
        high: ceiling,
        from: area.bottom,
        to: area.top,
    };
    for db in level.ticks() {
        let py = level.pos(db);
        canvas.hline(
            py,
            scale_left + 10.0,
            scale_left + 13.0,
            palette::TEXT_WEAK,
            0.8,
        );
        canvas.label(
            scale_left + 16.0,
            py,
            (Align::Start, Align::Centre),
            palette::TEXT_WEAK,
            &format!("{db}"),
            (0.0, f32::MAX),
        );
    }
    canvas.text(
        scale_left,
        area.top - 12.0,
        (Align::Start, Align::End),
        LABEL,
        palette::TEXT_WEAK,
        "dB",
    );

    // Markers over it all, their labels above the plot: spans in green,
    // moments in the text color, both apart from the levels' colors.
    let mut marker_json = Vec::new();
    let clip = |px: f32| px.clamp(area.left, area.right);
    for marker in markers {
        match marker {
            Marker::Span { from, to, label } => {
                let (a, b) = (clip(x_of_beat(*from)), clip(x_of_beat(*to)));
                if b > a {
                    // Lightly tinted, so the levels under it keep their colors.
                    canvas.rect(a, area.top, b, area.bottom, palette::ENVELOPE, 0.08);
                    canvas.rect(a, area.top, b, area.top + 3.0, palette::ENVELOPE, 0.9);
                    canvas.vline(a, area.top, area.bottom, palette::ENVELOPE, 0.9);
                    canvas.vline(b, area.top, area.bottom, palette::ENVELOPE, 0.9);
                    canvas.label(
                        a,
                        area.top - 9.0,
                        (Align::Start, Align::End),
                        palette::ENVELOPE,
                        label,
                        (area.left, plot.width as f32),
                    );
                }
                marker_json.push(
                    json!({ "kind": "span", "from_beat": from, "to_beat": to, "label": label }),
                );
            }
            Marker::At { beat, label } => {
                let px = x_of_beat(*beat);
                if (area.left..=area.right).contains(&px) {
                    let line = [(px, area.top), (px, area.bottom)];
                    canvas.polyline(&line, 1.5, palette::TEXT, 0.9);
                    canvas.label(
                        px,
                        area.top - 9.0,
                        (Align::Centre, Align::End),
                        palette::TEXT,
                        label,
                        (area.left, plot.width as f32),
                    );
                }
                marker_json.push(json!({ "kind": "at", "beat": beat, "label": label }));
            }
        }
    }

    let axes = report::round(json!({
        "kind": "spectrogram",
        "width": plot.width,
        "height": plot.height,
        "plot": area.json(),
        "x": {
            "scale": "linear",
            "from_beat": start,
            "to_beat": end,
            "from": signal.bar_beat(start),
            "to": signal.bar_beat(end),
            "seconds": signal.seconds(),
        },
        "y": { "scale": "log", "from_hz": plot.freq.0, "to_hz": plot.freq.1 },
        "color": {
            "from_db": floor,
            "to_db": ceiling,
            "map": "dark, blue, light blue, violet, orange, white",
        },
        "fft": spectrogram.fft(),
        "columns": spectrogram.columns,
        "markers": marker_json,
    }));
    (canvas.png(), axes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectrum::{FFT, average};
    use crate::testing::{band_noise, noise, sine, vowel};
    use project::TempoMap;

    const RATE: u32 = 48_000;

    /// Width, height and RGB pixels.
    fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgb);
        pixels.truncate(info.buffer_size());
        (info.width, info.height, pixels)
    }

    fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> Rgb {
        let i = ((y * width + x) * 3) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2]]
    }

    fn edge(axes: &Value, side: &str) -> u32 {
        axes["plot"][side].as_u64().unwrap() as u32
    }

    /// With `CANOPY_SCREENSHOTS` set, keeps `png` there to look at.
    fn keep(png: &[u8], name: &str) {
        if let Some(dir) = std::env::var_os("CANOPY_SCREENSHOTS") {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(std::path::Path::new(&dir).join(name), png).unwrap();
        }
    }

    #[test]
    fn a_png_of_the_asked_size_decodes() {
        let tempo = TempoMap::new(120.0);
        let wave = sine(440.0, 0.5, 1.0, RATE);
        let signal = Signal::new(&wave, &wave, RATE, 0.0, &tempo);

        let curves = [("Sine".to_string(), average(&signal, FFT))];
        let (png, axes) = spectrum_png(&curves, &Plot::default(), 4);
        let (width, height, _) = decode(&png);
        assert_eq!((width, height), (1024, 512));
        assert_eq!(
            (axes["width"].as_u64(), axes["height"].as_u64()),
            (Some(1024), Some(512))
        );
        assert_eq!(axes["x"]["from_hz"], 20);
        assert_eq!(axes["x"]["to_hz"], 20_000);
        assert_eq!(axes["curves"][0]["peaks"][0]["hz"], 440);
        assert!(edge(&axes, "right") < width && edge(&axes, "bottom") < height);

        let plot = Plot {
            width: 800,
            height: 400,
            ..Plot::default()
        };
        let gram = spectrogram(&signal, 2048, 400);
        assert_eq!((gram.columns, gram.rows), (400, 1025));
        let (png, axes) = spectrogram_png(&gram, &signal, &plot, &[]);
        assert_eq!((decode(&png).0, decode(&png).1), (800, 400));
        assert_eq!(axes["x"]["to_beat"], 2.0);
        assert_eq!(axes["fft"], 2048);
    }

    #[test]
    fn a_sines_peak_is_in_its_column() {
        let tempo = TempoMap::new(120.0);
        let wave = sine(1000.0, 0.5, 2.0, RATE);
        let signal = Signal::new(&wave, &wave, RATE, 0.0, &tempo);
        let curves = [("Sine".to_string(), average(&signal, FFT))];
        let (png, axes) = spectrum_png(&curves, &Plot::default(), 1);
        let (width, _, pixels) = decode(&png);
        let (left, right) = (edge(&axes, "left"), edge(&axes, "right"));
        let (top, bottom) = (edge(&axes, "top"), edge(&axes, "bottom"));

        // The curve's color is the accent: the topmost such pixel of each
        // column, and the columns where it is highest.
        let is_curve = |c: Rgb| c[2] > 190 && c[2] as i32 - c[0] as i32 > 80;
        let heights: Vec<(u32, u32)> = (left..right)
            .filter_map(|x| {
                (top..bottom)
                    .find(|&y| is_curve(pixel(&pixels, width, x, y)))
                    .map(|y| (x, y))
            })
            .collect();
        let highest = heights.iter().map(|h| h.1).min().unwrap();
        let columns: Vec<u32> = heights
            .iter()
            .filter(|h| h.1 <= highest + 1)
            .map(|h| h.0)
            .collect();
        let found = (columns[0] + columns[columns.len() - 1]) as f32 / 2.0;
        let expected = left as f32 + (1000f32 / 20.0).ln() / 1000f32.ln() * (right - left) as f32;
        assert!(
            (found - expected).abs() <= 2.0,
            "{found} against {expected}: {columns:?}"
        );

        // -6 dB on a -90..0 axis.
        let level = top as f32 + 6.0 / 90.0 * (bottom - top) as f32;
        assert!(
            (highest as f32 - level).abs() <= 6.0,
            "{highest} against {level}"
        );
    }

    #[test]
    fn sizes_are_capped() {
        let tempo = TempoMap::new(120.0);
        let wave = noise(RATE as usize, 5);
        let signal = Signal::new(&wave, &wave, RATE, 0.0, &tempo);
        let curves = [(String::new(), average(&signal, FFT))];
        let gram = spectrogram(&signal, 1024, 100);
        for (asked, got) in [((5000, 4000), (1600, 1000)), ((10, 0), (320, 200))] {
            let plot = Plot {
                width: asked.0,
                height: asked.1,
                ..Plot::default()
            };
            let (png, axes) = spectrum_png(&curves, &plot, 0);
            let (width, height, _) = decode(&png);
            assert_eq!((width, height), got);
            assert_eq!(axes["width"], got.0);
            let (png, _) = spectrogram_png(&gram, &signal, &plot, &[]);
            let (width, height, _) = decode(&png);
            assert_eq!((width, height), got);
        }
        let odd = Plot {
            freq: (f32::NAN, 100.0),
            db: (0.0, -10.0),
            ..Plot::default()
        }
        .capped();
        assert_eq!(odd.freq, (20.0, 20_000.0));
        assert_eq!(odd.db, (0.0, 6.0));
    }

    #[test]
    fn a_burst_shows_in_its_time_columns() {
        // Four seconds of near silence with a 3 kHz tone from 1.5 to 2 s
        // (beats 3 to 4 at 120 BPM).
        let tempo = TempoMap::new(120.0);
        let mut wave: Vec<f32> = noise(4 * RATE as usize, 9)
            .iter()
            .map(|n| n * 1e-4)
            .collect();
        let tone = sine(3000.0, 0.5, 0.5, RATE);
        let at = (1.5 * f64::from(RATE)) as usize;
        for (w, t) in wave[at..].iter_mut().zip(&tone) {
            *w += t;
        }
        let signal = Signal::new(&wave, &wave, RATE, 0.0, &tempo);
        let gram = spectrogram(&signal, 2048, 400);
        let row = (3000.0 / (RATE as f32 / 2048.0)).round() as usize;
        let at = |seconds: f32| gram.column((seconds / 4.0 * 400.0) as usize)[row];
        assert!(at(1.75) > -10.0, "{}", at(1.75));
        assert!(
            at(0.5) < -60.0 && at(3.0) < -60.0,
            "{} {}",
            at(0.5),
            at(3.0)
        );

        let markers = [Marker::Span {
            from: 3.0,
            to: 4.0,
            label: "tone".to_string(),
        }];
        let (png, axes) = spectrogram_png(&gram, &signal, &Plot::default(), &[]);
        let (width, _, pixels) = decode(&png);
        let (left, right) = (edge(&axes, "left") as f32, edge(&axes, "right") as f32);
        let (top, bottom) = (edge(&axes, "top") as f32, edge(&axes, "bottom") as f32);
        let x = |beat: f32| (left + beat / 8.0 * (right - left)) as u32;
        let y = (bottom - (3000f32 / 20.0).ln() / 1000f32.ln() * (bottom - top)) as u32;
        let bright = |beat: f32| {
            pixel(&pixels, width, x(beat), y)
                .iter()
                .map(|&c| u32::from(c))
                .sum::<u32>()
        };
        assert!(bright(3.5) > 400, "{}", bright(3.5));
        assert!(
            bright(1.0) < 100 && bright(6.0) < 100,
            "{} {}",
            bright(1.0),
            bright(6.0)
        );

        let (_, axes) = spectrogram_png(&gram, &signal, &Plot::default(), &markers);
        assert_eq!(axes["markers"][0]["kind"], "span");
        assert_eq!(axes["markers"][0]["to_beat"], 4.0);
    }

    #[test]
    fn plots_are_kept_to_look_at_when_asked() {
        // A vocal's vowels with ess bursts, and a guitar under it; the
        // song starts the render at bar 2.
        let tempo = TempoMap::new(120.0);
        let seconds = 8.0;
        let mut vocal = vowel(220.0, 0.3, 8000.0, seconds, RATE);
        let ess_len = (0.12 * f64::from(RATE)) as usize;
        let ess = band_noise(6000.0, 8000.0, ess_len, RATE, 11);
        let bursts = [1.0, 3.25, 5.5];
        for second in bursts {
            let start = (second * f64::from(RATE)) as usize;
            for (i, s) in ess.iter().enumerate() {
                let fade = (i.min(ess_len - i) as f32 / (0.005 * RATE as f32)).min(1.0);
                vocal[start + i] += 0.25 * s * fade;
            }
        }
        let guitar = vowel(110.0, 0.2, 5000.0, seconds, RATE);
        let start_beat = 4.0;
        let vocal_signal = Signal::new(&vocal, &vocal, RATE, start_beat, &tempo);
        let guitar_signal = Signal::new(&guitar, &guitar, RATE, start_beat, &tempo);
        let curves = [
            ("Vocal".to_string(), average(&vocal_signal, FFT)),
            ("Guitar".to_string(), average(&guitar_signal, FFT)),
        ];
        let (png, _) = spectrum_png(&curves, &Plot::default(), 3);
        keep(&png, "analysis_spectrum.png");

        let plot = Plot::default();
        let gram = spectrogram(&vocal_signal, 2048, spectrogram_columns(&plot));
        assert_eq!(gram.columns, 912);
        let mut markers: Vec<Marker> = bursts
            .iter()
            .map(|&second| Marker::Span {
                from: start_beat + 2.0 * second,
                to: start_beat + 2.0 * (second + 0.12),
                label: "ess".to_string(),
            })
            .collect();
        markers.push(Marker::At {
            beat: start_beat + 8.0,
            label: "onset".to_string(),
        });
        let (png, axes) = spectrogram_png(&gram, &vocal_signal, &plot, &markers);
        keep(&png, "analysis_spectrogram.png");
        assert_eq!(axes["x"]["from"], "2.1");
        assert_eq!(axes["x"]["to"], "6.1");
        assert_eq!(axes["markers"].as_array().unwrap().len(), 4);
    }
}
