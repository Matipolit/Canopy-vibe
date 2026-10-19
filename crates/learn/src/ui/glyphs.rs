//! Music symbols drawn with the painter: clefs, accidentals, note heads and
//! the grand staff's brace. egui's fonts have none of them, so each is a
//! shape: strokes of varying width (like a pen's) and filled ovals.
//!
//! Shapes are designed in staff spaces with y pointing up, from a
//! reference point the caller places: the clef's line, the note's center.

use std::f32::consts::TAU;

use eframe::egui::{Color32, Mesh, Painter, Pos2, Shape, Stroke, pos2, vec2};

/// Glyph units to the screen: `origin` is (0, 0), one unit is a staff space.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub origin: Pos2,
    pub space: f32,
}

impl Frame {
    pub fn at(&self, x: f32, y: f32) -> Pos2 {
        pos2(
            self.origin.x + x * self.space,
            self.origin.y - y * self.space,
        )
    }
}

/// Catmull-Rom through (x, y, width) points, `steps` samples per segment.
fn spline(points: &[(f32, f32, f32)], steps: usize) -> Vec<(f32, f32, f32)> {
    let n = points.len();
    if n < 3 {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(n * steps);
    for i in 0..n - 1 {
        let p0 = points[i.saturating_sub(1)];
        let p1 = points[i];
        let p2 = points[i + 1];
        let p3 = points[(i + 2).min(n - 1)];
        for k in 0..steps {
            let t = k as f32 / steps as f32;
            let (t2, t3) = (t * t, t * t * t);
            let f = |a: f32, b: f32, c: f32, d: f32| {
                0.5 * (2.0 * b
                    + (c - a) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
                    + (3.0 * b - a - 3.0 * c + d) * t3)
            };
            out.push((
                f(p0.0, p1.0, p2.0, p3.0),
                f(p0.1, p1.1, p2.1, p3.1),
                p1.2 + (p2.2 - p1.2) * t,
            ));
        }
    }
    out.push(points[n - 1]);
    out
}

/// A smooth stroke through screen points, each with its own width in
/// pixels: a filled strip with anti-aliased edges and round ends.
pub fn ribbon_px(painter: &Painter, points: &[(Pos2, f32)], color: Color32) {
    let units: Vec<(f32, f32, f32)> = points.iter().map(|(p, w)| (p.x, p.y, *w)).collect();
    let samples = spline(&units, 8);
    let n = samples.len();
    if n < 2 {
        return;
    }
    let pts: Vec<Pos2> = samples.iter().map(|s| pos2(s.0, s.1)).collect();
    let mut left = Vec::with_capacity(n);
    let mut right = Vec::with_capacity(n);
    let mut normal = vec2(0.0, 1.0);
    for i in 0..n {
        let d = pts[(i + 1).min(n - 1)] - pts[i.saturating_sub(1)];
        if d.length() > 1e-4 {
            let d = d.normalized();
            normal = vec2(-d.y, d.x);
        }
        let half = samples[i].2 * 0.5;
        left.push(pts[i] + normal * half);
        right.push(pts[i] - normal * half);
    }
    let mut mesh = Mesh::default();
    mesh.reserve_vertices(2 * n);
    mesh.reserve_triangles(2 * n);
    for i in 0..n {
        mesh.colored_vertex(left[i], color);
        mesh.colored_vertex(right[i], color);
    }
    for i in 0..n as u32 - 1 {
        let a = 2 * i;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 3, a + 2);
    }
    painter.add(Shape::mesh(mesh));
    // The strip itself has hard edges; thin lines along them smooth it.
    let edge = Stroke::new(0.8, color);
    painter.add(Shape::line(left, edge));
    painter.add(Shape::line(right, edge));
    painter.circle_filled(pts[0], samples[0].2 * 0.5, color);
    painter.circle_filled(pts[n - 1], samples[n - 1].2 * 0.5, color);
}

/// A stroke through (x, y, width) points in glyph units.
pub fn ribbon(painter: &Painter, frame: Frame, points: &[(f32, f32, f32)], color: Color32) {
    let px: Vec<(Pos2, f32)> = points
        .iter()
        .map(|&(x, y, w)| (frame.at(x, y), w * frame.space))
        .collect();
    ribbon_px(painter, &px, color);
}

/// A filled quadrilateral of glyph-unit corners (must be convex).
fn quad(painter: &Painter, frame: Frame, corners: [(f32, f32); 4], color: Color32) {
    let points = corners.iter().map(|&(x, y)| frame.at(x, y)).collect();
    painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
}

/// A straight bar of constant thickness `t` (measured vertically).
fn bar(painter: &Painter, frame: Frame, from: (f32, f32), to: (f32, f32), t: f32, color: Color32) {
    let h = t / 2.0;
    quad(
        painter,
        frame,
        [
            (from.0, from.1 - h),
            (to.0, to.1 - h),
            (to.0, to.1 + h),
            (from.0, from.1 + h),
        ],
        color,
    );
}

/// A thin vertical line, at least a pixel wide.
fn vertical(painter: &Painter, frame: Frame, x: f32, y0: f32, y1: f32, width: f32, color: Color32) {
    let w = (width * frame.space).max(1.0);
    painter.line_segment([frame.at(x, y0), frame.at(x, y1)], Stroke::new(w, color));
}

/// The G clef. `origin` is its left edge on the G line (the treble
/// staff's second line), which its curl winds around.
pub fn treble_clef(painter: &Painter, origin: Pos2, space: f32, color: Color32) {
    let frame = Frame { origin, space };
    // From the end of the curl, round the body, up through the loop at the
    // top, and down the stem to the hook.
    const PATH: [(f32, f32, f32); 33] = [
        (1.30, -0.49, 0.04),
        (1.01, -0.30, 0.10),
        (0.85, 0.10, 0.20),
        (1.07, 0.57, 0.30),
        (1.60, 0.77, 0.38),
        (2.07, 0.57, 0.34),
        (2.31, -0.03, 0.28),
        (2.17, -0.57, 0.20),
        (1.80, -0.85, 0.13),
        (1.33, -0.95, 0.10),
        (0.67, -0.77, 0.15),
        (0.20, -0.23, 0.26),
        (0.14, 0.37, 0.30),
        (0.30, 1.03, 0.32),
        (0.67, 1.70, 0.33),
        (1.13, 2.17, 0.30),
        (1.57, 2.70, 0.24),
        (1.80, 3.23, 0.17),
        (1.80, 3.83, 0.12),
        (1.57, 4.27, 0.08),
        (1.33, 4.17, 0.12),
        (1.07, 3.57, 0.18),
        (0.97, 2.90, 0.19),
        (1.03, 2.37, 0.17),
        (1.20, 1.90, 0.12),
        (1.40, 1.03, 0.10),
        (1.60, 0.10, 0.10),
        (1.77, -0.83, 0.11),
        (1.90, -1.50, 0.15),
        (1.92, -2.03, 0.15),
        (1.73, -2.40, 0.12),
        (1.27, -2.51, 0.10),
        (0.97, -2.30, 0.08),
    ];
    ribbon(painter, frame, &PATH, color);
    painter.circle_filled(frame.at(0.93, -1.93), 0.40 * space, color);
}

/// Width of the G clef, in spaces.
pub const TREBLE_WIDTH: f32 = 2.5;

/// The F clef. `origin` is its left edge on the F line (the bass staff's
/// fourth line), which its two dots straddle.
pub fn bass_clef(painter: &Painter, origin: Pos2, space: f32, color: Color32) {
    let frame = Frame { origin, space };
    const PATH: [(f32, f32, f32); 11] = [
        (0.30, 0.48, 0.05),
        (0.62, 0.95, 0.10),
        (1.15, 1.13, 0.13),
        (1.70, 1.00, 0.24),
        (2.08, 0.55, 0.40),
        (2.15, 0.00, 0.54),
        (2.00, -0.70, 0.50),
        (1.60, -1.40, 0.38),
        (1.00, -1.98, 0.24),
        (0.40, -2.33, 0.11),
        (-0.05, -2.50, 0.04),
    ];
    ribbon(painter, frame, &PATH, color);
    painter.add(Shape::ellipse_filled(
        frame.at(0.47, 0.10),
        vec2(0.46, 0.40) * space,
        color,
    ));
    for y in [0.5, -0.5] {
        painter.circle_filled(frame.at(2.72, y), 0.15 * space, color);
    }
}

pub const BASS_WIDTH: f32 = 2.9;

/// How wide an accidental is, in spaces.
pub fn accidental_width(accidental: i8) -> f32 {
    match accidental {
        1 => 0.95,
        -1 => 0.8,
        2 => 0.85,
        -2 => 1.35,
        _ => 0.65,
    }
}

/// An accidental centered at `center` (the note's height): -2 double
/// flat, -1 flat, 0 natural, 1 sharp, 2 double sharp.
pub fn accidental(painter: &Painter, center: Pos2, space: f32, accidental: i8, color: Color32) {
    match accidental {
        1 => sharp(painter, center, space, color),
        -1 => flat(painter, center + vec2(-0.37 * space, 0.0), space, color),
        2 => double_sharp(painter, center, space, color),
        -2 => {
            flat(painter, center + vec2(-0.65 * space, 0.0), space, color);
            flat(painter, center + vec2(-0.05 * space, 0.0), space, color);
        }
        _ => natural(painter, center, space, color),
    }
}

fn sharp(painter: &Painter, center: Pos2, space: f32, color: Color32) {
    let f = Frame {
        origin: center,
        space,
    };
    vertical(painter, f, -0.2, -1.3, 1.15, 0.09, color);
    vertical(painter, f, 0.2, -1.15, 1.3, 0.09, color);
    bar(painter, f, (-0.45, -0.52), (0.45, -0.24), 0.2, color);
    bar(painter, f, (-0.45, 0.24), (0.45, 0.52), 0.2, color);
}

/// `origin` is the stem, at the note's height.
fn flat(painter: &Painter, origin: Pos2, space: f32, color: Color32) {
    let f = Frame { origin, space };
    vertical(painter, f, 0.0, -0.52, 1.8, 0.09, color);
    ribbon(
        painter,
        f,
        &[
            (0.0, 0.12, 0.06),
            (0.22, 0.42, 0.09),
            (0.48, 0.47, 0.13),
            (0.68, 0.30, 0.18),
            (0.66, 0.02, 0.18),
            (0.45, -0.26, 0.13),
            (0.20, -0.43, 0.08),
            (0.02, -0.50, 0.06),
        ],
        color,
    );
}

fn natural(painter: &Painter, center: Pos2, space: f32, color: Color32) {
    let f = Frame {
        origin: center,
        space,
    };
    vertical(painter, f, -0.24, -0.55, 1.35, 0.09, color);
    vertical(painter, f, 0.24, -1.35, 0.55, 0.09, color);
    bar(painter, f, (-0.24, 0.28), (0.24, 0.46), 0.19, color);
    bar(painter, f, (-0.24, -0.46), (0.24, -0.28), 0.19, color);
}

fn double_sharp(painter: &Painter, center: Pos2, space: f32, color: Color32) {
    let f = Frame {
        origin: center,
        space,
    };
    let stroke = Stroke::new((0.11 * space).max(1.0), color);
    painter.line_segment([f.at(-0.3, -0.3), f.at(0.3, 0.3)], stroke);
    painter.line_segment([f.at(-0.3, 0.3), f.at(0.3, -0.3)], stroke);
    for (x, y) in [(-0.3, -0.3), (0.3, 0.3), (-0.3, 0.3), (0.3, -0.3)] {
        let r = 0.15;
        quad(
            painter,
            f,
            [(x - r, y), (x, y + r), (x + r, y), (x, y - r)],
            color,
        );
    }
}

/// Half the width of a note head, in spaces.
pub const HEAD_HALF: f32 = 0.59;

/// A filled (quarter-note) head, tilted as engraved.
pub fn note_head(painter: &Painter, center: Pos2, space: f32, color: Color32) {
    painter.add(Shape::convex_polygon(
        ellipse(center, space, 0.56, 0.39, 0.35),
        color,
        Stroke::NONE,
    ));
}

/// An open (whole-note) head: an oval with a slanted hole.
pub fn whole_note(painter: &Painter, center: Pos2, space: f32, color: Color32, paper: Color32) {
    painter.add(Shape::convex_polygon(
        ellipse(center, space, 0.74, 0.48, 0.0),
        color,
        Stroke::NONE,
    ));
    painter.add(Shape::convex_polygon(
        ellipse(center, space, 0.36, 0.25, 0.95),
        paper,
        Stroke::NONE,
    ));
}

/// Points round an ellipse of radii `rx`, `ry` (spaces), turned `angle`
/// radians anticlockwise.
fn ellipse(center: Pos2, space: f32, rx: f32, ry: f32, angle: f32) -> Vec<Pos2> {
    let (sin, cos) = angle.sin_cos();
    (0..32)
        .map(|k| {
            let t = k as f32 / 32.0 * TAU;
            let (x, y) = (rx * t.cos(), ry * t.sin());
            pos2(
                center.x + (x * cos - y * sin) * space,
                center.y - (x * sin + y * cos) * space,
            )
        })
        .collect()
}

/// The brace joining a grand staff, its tips at `right` from `top` to
/// `bottom`.
pub fn brace(painter: &Painter, right: f32, top: f32, bottom: f32, space: f32, color: Color32) {
    let height = bottom - top;
    // Upper half, tip to point: (spaces left of `right`, fraction down, width).
    const HALF: [(f32, f32, f32); 7] = [
        (0.05, 0.0, 0.04),
        (0.45, 0.035, 0.10),
        (0.70, 0.12, 0.20),
        (0.76, 0.27, 0.27),
        (0.82, 0.41, 0.21),
        (1.05, 0.485, 0.09),
        (1.25, 0.5, 0.04),
    ];
    let lower = HALF.iter().rev().skip(1).map(|p| (p.0, 1.0 - p.1, p.2));
    let points: Vec<(Pos2, f32)> = HALF
        .iter()
        .copied()
        .chain(lower)
        .map(|(x, y, w)| (pos2(right - x * space, top + y * height), w * space))
        .collect();
    ribbon_px(painter, &points, color);
}
