//! Snap settings and the bar/beat grid lines behind the lanes.

use eframe::egui::{Color32, Painter, Rect, Stroke};
use midi::{PPQ, Tick};
use project::TempoMap;

use super::Viewport;

/// Adaptive grid lines are at least this far apart.
const MIN_STEP_PX: f32 = 14.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridSize {
    /// Follows the zoom, like Ableton's default.
    Adaptive,
    Bar,
    /// A 1/n note.
    Note(i64),
    /// A 1/n note triplet.
    Triplet(i64),
}

impl GridSize {
    pub const ALL: [GridSize; 9] = [
        GridSize::Adaptive,
        GridSize::Bar,
        GridSize::Note(2),
        GridSize::Note(4),
        GridSize::Note(8),
        GridSize::Note(16),
        GridSize::Note(32),
        GridSize::Triplet(8),
        GridSize::Triplet(16),
    ];

    pub fn label(&self) -> String {
        match self {
            GridSize::Adaptive => "Adaptive".to_owned(),
            GridSize::Bar => "1 Bar".to_owned(),
            GridSize::Note(n) => format!("1/{n}"),
            GridSize::Triplet(n) => format!("1/{n}T"),
        }
    }

    fn fixed(&self, tempo: &TempoMap) -> Option<Tick> {
        match self {
            GridSize::Adaptive => None,
            GridSize::Bar => Some(tempo.bar_len()),
            GridSize::Note(n) => Some(Tick::new(PPQ * 4 / n)),
            GridSize::Triplet(n) => Some(Tick::new(PPQ * 4 * 2 / (n * 3))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub size: GridSize,
    pub snap: bool,
}

impl Default for Grid {
    fn default() -> Self {
        Grid {
            size: GridSize::Adaptive,
            snap: true,
        }
    }
}

impl Grid {
    /// The grid step at the current zoom.
    pub fn step(&self, viewport: &Viewport, tempo: &TempoMap) -> Tick {
        if let Some(fixed) = self.size.fixed(tempo) {
            return fixed;
        }
        let fits = |t: Tick| viewport.width_of(t.get() as f64) >= MIN_STEP_PX;
        // Halve from a bar down to a 1/32, then double whole bars up.
        let mut step = tempo.bar_len();
        if fits(step) {
            while step.get() > PPQ / 8 && fits(step / 2) {
                step = step / 2;
            }
        } else {
            while !fits(step) {
                step = step * 2;
            }
        }
        step
    }

    /// The grid line nearest to `tick`, or `tick` itself if snapping is off
    /// or bypassed (Alt held while dragging).
    pub fn snap(&self, tick: f64, step: Tick, bypass: bool) -> Tick {
        let raw = Tick::new(tick.round() as i64);
        if self.snap && !bypass {
            raw.round_to(step)
        } else {
            raw
        }
    }

    /// The grid line at or before `tick`, unless snapping is off.
    pub fn snap_floor(&self, tick: f64, step: Tick, bypass: bool) -> Tick {
        let raw = Tick::new(tick.floor() as i64);
        if self.snap && !bypass {
            raw.floor_to(step)
        } else {
            raw
        }
    }
}

/// Vertical lines for bars, beats and grid steps, finer ones fainter.
pub fn paint_lines(
    painter: &Painter,
    rect: Rect,
    viewport: &Viewport,
    tempo: &TempoMap,
    step: Tick,
    line: Color32,
) {
    let bar = tempo.bar_len().get();
    let beat = tempo.beat_len().get();
    let step = step.get().max(1);
    let first = (viewport.tick_at(rect.left()).max(0.0) as i64).div_euclid(step) * step;
    let last = viewport.tick_at(rect.right()) as i64;
    let mut tick = first;
    while tick <= last {
        let alpha = if tick % bar == 0 {
            1.0
        } else if tick % beat == 0 {
            0.55
        } else {
            0.3
        };
        let x = viewport.x(tick as f64).round() + 0.5;
        painter.vline(
            x,
            rect.y_range(),
            Stroke::new(1.0, line.gamma_multiply(alpha)),
        );
        tick += step;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_step_follows_zoom() {
        let tempo = TempoMap::default();
        let grid = Grid::default();
        let mut viewport = Viewport::default(); // a bar is 120 px
        assert_eq!(grid.step(&viewport, &tempo), Tick::new(480), "1/8 is 15 px");
        viewport.px_per_tick /= 16.0; // a bar is 7.5 px
        assert_eq!(grid.step(&viewport, &tempo), tempo.bar_len() * 2);
    }

    #[test]
    fn fixed_sizes_and_snapping() {
        let tempo = TempoMap::default();
        let viewport = Viewport::default();
        let mut grid = Grid {
            size: GridSize::Triplet(8),
            snap: true,
        };
        let step = grid.step(&viewport, &tempo);
        assert_eq!(step, Tick::new(320));
        assert_eq!(grid.snap(500.0, step, false), Tick::new(640));
        assert_eq!(grid.snap(500.0, step, true), Tick::new(500), "bypassed");
        assert_eq!(grid.snap_floor(500.0, step, false), Tick::new(320));
        grid.snap = false;
        assert_eq!(grid.snap(500.4, step, false), Tick::new(500));
    }
}
