//! A spectrum analyzer, in the style of MiniMeters and FabFilter Pro-Q: a
//! log frequency axis, a slope that makes a balanced mix look level, instant
//! rises and smooth falls, and an optional peak hold. The EQ draws one
//! behind its curve; the analyzer draws one on its own.

use std::cell::RefCell;
use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, pos2};
use realfft::{RealFftPlanner, RealToComplex};

/// How a spectrum is analyzed and drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectrumSettings {
    /// FFT length in samples: longer resolves lows better, reacts slower.
    pub size: usize,
    /// dB per octave added above 1 kHz (and taken away below), so pink noise
    /// (and most mixes) look level. 4.5 is what most analyzers use.
    pub slope: f32,
    pub floor_db: f32,
    pub ceiling_db: f32,
    pub low_hz: f32,
    pub high_hz: f32,
    /// How fast the line falls back, in dB per second.
    pub fall: f32,
    pub peak_hold: bool,
    /// Each point averages this many octaves around it, so the line reads
    /// as a shape rather than a comb of harmonics.
    pub smoothing: f32,
}

impl Default for SpectrumSettings {
    fn default() -> Self {
        SpectrumSettings {
            size: 8192,
            slope: 4.5,
            floor_db: -84.0,
            ceiling_db: 6.0,
            low_hz: 20.0,
            high_hz: 20_000.0,
            fall: 40.0,
            peak_hold: false,
            smoothing: 1.0 / 12.0,
        }
    }
}

/// A log-frequency, linear-dB mapping onto a rectangle.
#[derive(Debug, Clone, Copy)]
pub struct Axes {
    pub rect: Rect,
    pub low_hz: f32,
    pub high_hz: f32,
    pub floor_db: f32,
    pub ceiling_db: f32,
}

impl Axes {
    pub fn x(&self, hz: f32) -> f32 {
        let t = (hz.max(1.0) / self.low_hz).ln() / (self.high_hz / self.low_hz).ln();
        self.rect.left() + t * self.rect.width()
    }

    pub fn hz(&self, x: f32) -> f32 {
        let t = ((x - self.rect.left()) / self.rect.width()).clamp(0.0, 1.0);
        self.low_hz * (self.high_hz / self.low_hz).powf(t)
    }

    pub fn y(&self, db: f32) -> f32 {
        let t = (db - self.floor_db) / (self.ceiling_db - self.floor_db);
        self.rect.bottom() - t.clamp(0.0, 1.0) * self.rect.height()
    }

    pub fn db(&self, y: f32) -> f32 {
        let t = (self.rect.bottom() - y) / self.rect.height();
        self.floor_db + t * (self.ceiling_db - self.floor_db)
    }

    /// Frequency lines with a few labels, and level lines every `db_step`.
    pub fn paint_grid(&self, painter: &Painter, visuals: &egui::Visuals, db_step: f32) {
        let grid = visuals.weak_text_color().gamma_multiply(0.16);
        let label = visuals.weak_text_color().gamma_multiply(0.8);
        let font = FontId::proportional(crate::theme::text::TINY);
        let mut decade = 10.0;
        while decade <= self.high_hz {
            for step in 1..10 {
                let hz = decade * step as f32;
                if hz < self.low_hz || hz > self.high_hz {
                    continue;
                }
                let x = self.x(hz);
                let strong = step == 1;
                let stroke = Stroke::new(
                    1.0,
                    if strong {
                        grid.gamma_multiply(1.8)
                    } else {
                        grid
                    },
                );
                painter.vline(x, self.rect.y_range(), stroke);
                if matches!(step, 1 | 2 | 5) && x < self.rect.right() - 16.0 {
                    let text = if hz >= 1000.0 {
                        format!("{}k", hz / 1000.0)
                    } else {
                        format!("{hz}")
                    };
                    painter.text(
                        pos2(x + 2.0, self.rect.bottom() - 1.0),
                        Align2::LEFT_BOTTOM,
                        text,
                        font.clone(),
                        label,
                    );
                }
            }
            decade *= 10.0;
        }
        if db_step > 0.0 {
            let mut db = (self.ceiling_db / db_step).floor() * db_step;
            while db > self.floor_db {
                let y = self.y(db);
                painter.hline(self.rect.x_range(), y, Stroke::new(1.0, grid));
                painter.text(
                    pos2(self.rect.right() - 2.0, y),
                    Align2::RIGHT_CENTER,
                    format!("{db}"),
                    font.clone(),
                    label,
                );
                db -= db_step;
            }
        }
    }
}

/// What a spectrum display remembers between frames.
#[derive(Clone, Default)]
pub struct Spectrum {
    /// Smoothed level of each FFT bin, in dB.
    levels: Vec<f32>,
    peaks: Vec<f32>,
    size: usize,
    window: Arc<[f32]>,
}

thread_local! {
    static PLANNER: RefCell<RealFftPlanner<f32>> = RefCell::new(RealFftPlanner::new());
}

fn plan(size: usize) -> Arc<dyn RealToComplex<f32>> {
    PLANNER.with(|p| p.borrow_mut().plan_fft_forward(size))
}

impl Spectrum {
    /// Analyzes the newest `samples` (oldest first; fewer than the FFT size
    /// is padded with silence) and blends them in: rises are instant, falls
    /// go at `settings.fall` over `dt` seconds.
    pub fn update(&mut self, samples: &[f32], settings: &SpectrumSettings, dt: f32) {
        let size = settings.size.clamp(256, 65_536).next_power_of_two();
        if self.size != size {
            self.size = size;
            self.window = (0..size)
                .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size as f32).cos())
                .collect();
            self.levels = vec![settings.floor_db; size / 2 + 1];
            self.peaks = self.levels.clone();
        }
        let fft = plan(size);
        let mut input = fft.make_input_vec();
        let skip = samples.len().saturating_sub(size);
        let offset = size - (samples.len() - skip);
        for (i, sample) in samples[skip..].iter().enumerate() {
            input[offset + i] = sample * self.window[offset + i];
        }
        let mut output = fft.make_output_vec();
        if fft.process(&mut input, &mut output).is_err() {
            return;
        }
        // A full-scale sine reads 0 dB: the Hann window halves the peak bin.
        let scale = 4.0 / size as f32;
        let fall = settings.fall * dt;
        for ((level, peak), bin) in self.levels.iter_mut().zip(&mut self.peaks).zip(&output) {
            let db = 20.0 * (bin.norm() * scale).max(1e-9).log10();
            *level = db.max(*level - fall).max(settings.floor_db - 20.0);
            *peak = peak.max(*level);
        }
    }

    pub fn clear_peaks(&mut self) {
        self.peaks.clone_from(&self.levels);
    }

    /// The level between `hz_low` and `hz_high`, as drawn: the loudest bin
    /// there (so a tone reads its true level), or interpolated between bins
    /// where they're wider, plus the slope.
    fn level_at(levels: &[f32], hz_low: f32, hz_high: f32, bin_hz: f32, slope: f32) -> f32 {
        let last = levels.len().saturating_sub(1);
        let (from, to) = (hz_low / bin_hz, hz_high / bin_hz);
        let db = if to.floor() > from.ceil() {
            let (a, b) = (from.ceil() as usize, (to.floor() as usize).min(last));
            levels[a.min(last)..=b.max(a.min(last))]
                .iter()
                .copied()
                .fold(f32::MIN, f32::max)
        } else {
            let center = (from + to) / 2.0;
            let i = (center.floor() as usize).min(last);
            let t = center - i as f32;
            let next = levels[(i + 1).min(last)];
            levels[i] + (next - levels[i]) * t
        };
        let hz = (hz_low * hz_high).sqrt().max(1.0);
        db + slope * (hz / 1000.0).log2()
    }

    /// Draws the spectrum into `axes`, filled underneath with `fill` and
    /// outlined with `line`; the peak hold, if on, as a thin line.
    pub fn paint(
        &self,
        painter: &Painter,
        axes: &Axes,
        settings: &SpectrumSettings,
        sample_rate: f32,
        line: Stroke,
        fill: Color32,
    ) {
        if self.levels.is_empty() || self.size == 0 {
            return;
        }
        let bin_hz = sample_rate / self.size as f32;
        let rect = axes.rect;
        let step = 2.0;
        let widen = 2f32.powf(settings.smoothing / 2.0);
        let band = |x: f32| (axes.hz(x) / widen, axes.hz(x + step) * widen);
        let mut points: Vec<Pos2> = Vec::with_capacity((rect.width() / step) as usize + 2);
        let mut mesh = egui::Mesh::default();
        let mut x = rect.left();
        while x < rect.right() {
            let (low, high) = band(x);
            let db = Self::level_at(&self.levels, low, high, bin_hz, settings.slope);
            let y = axes.y(db);
            points.push(pos2(x, y));
            if fill.a() > 0 {
                mesh.add_colored_rect(
                    Rect::from_min_max(pos2(x, y), pos2(x + step, rect.bottom())),
                    fill,
                );
            }
            x += step;
        }
        painter.add(mesh);
        painter.add(Shape::line(points, line));
        if settings.peak_hold {
            let mut peaks = Vec::with_capacity((rect.width() / step) as usize + 2);
            let mut x = rect.left();
            while x < rect.right() {
                let (low, high) = band(x);
                let db = Self::level_at(&self.peaks, low, high, bin_hz, settings.slope);
                peaks.push(pos2(x, axes.y(db)));
                x += step;
            }
            painter.add(Shape::line(
                peaks,
                Stroke::new(1.0, line.color.gamma_multiply(0.6)),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_scale_sine_reads_zero_db_at_its_frequency() {
        let rate = 48_000.0;
        let settings = SpectrumSettings::default();
        let samples: Vec<f32> = (0..8192)
            .map(|i| (std::f32::consts::TAU * 1500.0 * i as f32 / rate).sin())
            .collect();
        let mut spectrum = Spectrum::default();
        spectrum.update(&samples, &settings, 1.0 / 60.0);
        let bin_hz = rate / 8192.0;
        let at = |s: &Spectrum, hz: f32| Spectrum::level_at(&s.levels, hz, hz, bin_hz, 0.0);
        assert!(
            at(&spectrum, 1500.0).abs() < 1.5,
            "{}",
            at(&spectrum, 1500.0)
        );
        assert!(at(&spectrum, 6000.0) < -60.0, "{}", at(&spectrum, 6000.0));

        // Silence falls at the set rate rather than vanishing.
        spectrum.update(&[0.0; 8192], &settings, 0.5);
        let fallen = at(&spectrum, 1500.0);
        assert!((fallen + 20.0).abs() < 2.0, "{fallen}");
    }
}
