//! A system of staves on paper: lines, brace, clefs and key signature, and
//! notes placed on it with their ledger lines, accidentals, stems and
//! fingering.

use eframe::egui::{Align2, Color32, FontId, Painter, Rect, Stroke, pos2};

use super::glyphs::{self, HEAD_HALF};
use crate::notation::{Clef, accidental_columns, head_offsets, ledger_lines, stem_up};
use crate::theory::{KeySig, Spelled};

/// Spaces between the treble staff's bottom line and the bass staff's top.
const STAFF_GAP: f32 = 6.0;

pub struct System {
    pub space: f32,
    /// Each staff and the height of its bottom line, top staff first.
    pub staves: Vec<(Clef, f32)>,
    /// Where the staff lines start (the system line) and end.
    pub left: f32,
    pub right: f32,
    pub key: KeySig,
    /// Where notes can go, after the clef and key signature.
    pub notes_left: f32,
}

impl System {
    /// Lays out `clefs` centered in `rect`, as large as fits with `margin`
    /// spaces above and below for ledger lines, up to `max_space` pixels
    /// a space.
    pub fn new(rect: Rect, clefs: &[Clef], key: KeySig, max_space: f32, margin: f32) -> System {
        let count = clefs.len().max(1) as f32;
        let body = count * 4.0 + (count - 1.0) * STAFF_GAP;
        let space = (rect.height() / (body + 2.0 * margin))
            .min(max_space)
            .max(4.0);
        let top = rect.center().y - body * space / 2.0;
        let staves = clefs
            .iter()
            .enumerate()
            .map(|(i, &clef)| (clef, top + (i as f32 * (4.0 + STAFF_GAP) + 4.0) * space))
            .collect();
        let grand = clefs.len() > 1;
        let left = rect.left() + if grand { 2.2 } else { 1.0 } * space;
        let right = rect.right() - 1.0 * space;
        let key_width: f32 = clef_key_width(key);
        System {
            space,
            staves,
            left,
            right,
            key,
            notes_left: left + 3.8 * space + key_width * space + 1.0 * space,
        }
    }

    pub fn has(&self, clef: Clef) -> bool {
        self.staves.iter().any(|s| s.0 == clef)
    }

    fn bottom_of(&self, clef: Clef) -> Option<f32> {
        self.staves.iter().find(|s| s.0 == clef).map(|s| s.1)
    }

    /// The height of `position` on `clef`'s staff.
    pub fn y(&self, clef: Clef, position: i32) -> f32 {
        let bottom = self
            .bottom_of(clef)
            .or_else(|| self.staves.first().map(|s| s.1))
            .unwrap_or(0.0);
        bottom - position as f32 * self.space / 2.0
    }

    /// The top line of the top staff.
    pub fn top(&self) -> f32 {
        self.staves.first().map_or(0.0, |s| s.1 - 4.0 * self.space)
    }

    /// The bottom line of the bottom staff.
    pub fn bottom(&self) -> f32 {
        self.staves.last().map_or(0.0, |s| s.1)
    }

    fn line(&self, width: f32, color: Color32) -> Stroke {
        Stroke::new((width * self.space).max(1.0), color)
    }

    /// Staff lines, brace, clefs and key signature.
    pub fn draw(&self, painter: &Painter, ink: Color32) {
        let s = self.space;
        let line = self.line(0.09, ink);
        for &(clef, bottom) in &self.staves {
            for i in 0..5 {
                painter.hline(self.left..=self.right, bottom - i as f32 * s, line);
            }
            let x = self.left + 0.7 * s;
            match clef {
                Clef::Treble => glyphs::treble_clef(painter, pos2(x, bottom - s), s, ink),
                Clef::Bass => {
                    glyphs::bass_clef(painter, pos2(x + 0.1 * s, bottom - 3.0 * s), s, ink)
                }
            }
            let mut kx = self.left + 3.8 * s;
            for (position, accidental) in clef.key_positions(self.key) {
                let w = key_step(accidental);
                glyphs::accidental(
                    painter,
                    pos2(kx + w * s / 2.0, self.y(clef, position)),
                    s,
                    accidental,
                    ink,
                );
                kx += w * s;
            }
        }
        if self.staves.len() > 1 {
            painter.vline(self.left, self.top()..=self.bottom(), self.line(0.16, ink));
            glyphs::brace(
                painter,
                self.left - 0.35 * s,
                self.top(),
                self.bottom(),
                s,
                ink,
            );
        }
    }

    /// A bar line through every staff.
    pub fn bar_line(&self, painter: &Painter, x: f32, ink: Color32) {
        painter.vline(x, self.top()..=self.bottom(), self.line(0.12, ink));
    }

    /// The double bar that ends a piece: thin, then thick.
    pub fn end_line(&self, painter: &Painter, x: f32, ink: Color32) {
        let s = self.space;
        painter.vline(
            x - 0.75 * s,
            self.top()..=self.bottom(),
            self.line(0.12, ink),
        );
        painter.vline(
            x - 0.25 * s,
            self.top()..=self.bottom(),
            self.line(0.5, ink),
        );
    }
}

/// How far each accidental of a key signature moves along, in spaces.
fn key_step(accidental: i8) -> f32 {
    if accidental > 0 { 1.0 } else { 0.85 }
}

fn clef_key_width(key: KeySig) -> f32 {
    let step = key_step(key.0.signum());
    f32::from(key.0.unsigned_abs()) * step
}

/// A note head to draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Head {
    pub note: Spelled,
    pub clef: Clef,
    pub color: Color32,
    /// The accidental to write before it, if any.
    pub accidental: Option<i8>,
    pub finger: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// Quarter notes with stems; otherwise whole notes.
    pub stem: bool,
    pub paper: Color32,
    pub ink: Color32,
    /// Fingering numbers in this color (or each head's own if `None`).
    pub finger_color: Option<Color32>,
}

/// Draws the heads of one chord (or one note) at `x`, on whichever staves
/// they belong to: a stem per staff, ledger lines, accidentals out of each
/// other's way, and fingering above the treble or below the bass. Returns
/// the highest and lowest point drawn.
pub fn chord(
    painter: &Painter,
    system: &System,
    x: f32,
    heads: &[Head],
    style: Style,
) -> (f32, f32) {
    let s = system.space;
    let head_w = 2.0 * HEAD_HALF * s;
    let mut top = system.top();
    let mut bottom = system.bottom();
    let mut reach = |y0: f32, y1: f32| {
        top = top.min(y0.min(y1));
        bottom = bottom.max(y0.max(y1));
    };
    for clef in [Clef::Treble, Clef::Bass] {
        let on: Vec<&Head> = heads
            .iter()
            .filter(|h| h.clef == clef && system.has(clef))
            .collect();
        if on.is_empty() {
            continue;
        }
        let positions: Vec<i32> = on.iter().map(|h| clef.position(h.note)).collect();
        let up = if style.stem {
            stem_up(&positions)
        } else {
            true
        };
        let offsets = head_offsets(&positions, up);
        let shift = if style.stem { 0.97 } else { 1.12 };
        let any_right = offsets.contains(&1);
        let any_left = offsets.contains(&-1);
        let (low, high) = (
            *positions.iter().min().unwrap_or(&0),
            *positions.iter().max().unwrap_or(&0),
        );

        // Ledger lines, wide enough for heads pushed aside.
        let half = if style.stem { head_w / 2.0 } else { 0.74 * s };
        let x0 = x - half - 0.38 * s - if any_left { head_w * shift } else { 0.0 };
        let x1 = x + half + 0.38 * s + if any_right { head_w * shift } else { 0.0 };
        let ledger = Stroke::new((0.15 * s).max(1.0), style.ink);
        for p in ledger_lines(low).into_iter().chain(ledger_lines(high)) {
            painter.hline(x0..=x1, system.y(clef, p), ledger);
        }
        reach(
            system.y(clef, high) - 0.6 * s,
            system.y(clef, low) + 0.6 * s,
        );

        for (head, offset) in on.iter().zip(&offsets) {
            let center = pos2(
                x + *offset as f32 * head_w * shift,
                system.y(clef, clef.position(head.note)),
            );
            if style.stem {
                glyphs::note_head(painter, center, s, head.color);
            } else {
                glyphs::whole_note(painter, center, s, head.color, style.paper);
            }
        }

        // One stem for the chord, from the far head past the near one.
        let mut stem_end = None;
        if style.stem {
            let color = if on.iter().all(|h| h.color == on[0].color) {
                on[0].color
            } else {
                style.ink
            };
            let sx = if up {
                x + head_w / 2.0 - 0.06 * s
            } else {
                x - head_w / 2.0 + 0.06 * s
            };
            let (from, to) = if up {
                (
                    system.y(clef, low) - 0.2 * s,
                    system.y(clef, high) - 3.4 * s,
                )
            } else {
                (
                    system.y(clef, high) + 0.2 * s,
                    system.y(clef, low) + 3.4 * s,
                )
            };
            // Stems reach at least the middle line from far ledger notes.
            let middle = system.y(clef, 4);
            let to = if up { to.min(middle) } else { to.max(middle) };
            painter.line_segment(
                [pos2(sx, from), pos2(sx, to)],
                Stroke::new((0.12 * s).max(1.0), color),
            );
            reach(from, to);
            stem_end = Some(to);
        }

        // Accidentals, in columns leading left from the heads.
        let with: Vec<(usize, i32, i8)> = on
            .iter()
            .enumerate()
            .filter_map(|(i, h)| h.accidental.map(|a| (i, positions[i], a)))
            .collect();
        if !with.is_empty() {
            let columns = accidental_columns(&with.iter().map(|w| w.1).collect::<Vec<_>>());
            let count = columns.iter().max().map_or(0, |c| c + 1);
            let widths: Vec<f32> = (0..count)
                .map(|c| {
                    with.iter()
                        .zip(&columns)
                        .filter(|(_, col)| **col == c)
                        .map(|(w, _)| glyphs::accidental_width(w.2))
                        .fold(0.0, f32::max)
                })
                .collect();
            let start = x - half - 0.3 * s - if any_left { head_w * shift } else { 0.0 };
            for ((i, position, accidental), column) in with.iter().zip(&columns) {
                let before: f32 = widths[..*column].iter().map(|w| w + 0.2).sum();
                let w = widths[*column];
                let cx = start - (before + w / 2.0) * s;
                let y = system.y(clef, *position);
                glyphs::accidental(painter, pos2(cx, y), s, *accidental, on[*i].color);
                reach(y - 1.8 * s, y + 1.4 * s);
            }
        }

        // Fingering: above the treble staff, below the bass, the top
        // note's finger on top.
        let mut fingers: Vec<(i32, u8, Color32)> = on
            .iter()
            .zip(&positions)
            .filter_map(|(h, p)| {
                h.finger
                    .map(|f| (*p, f, style.finger_color.unwrap_or(h.color)))
            })
            .collect();
        if fingers.is_empty() {
            continue;
        }
        fingers.sort_by_key(|f| std::cmp::Reverse(f.0));
        let font = FontId::proportional(1.15 * s);
        let step = 1.1 * s;
        match clef {
            Clef::Treble => {
                let mut y = system
                    .y(clef, high.max(8) + 3)
                    .min(system.y(clef, high) - 1.6 * s);
                if let Some(end) = stem_end.filter(|_| up) {
                    y = y.min(end - 0.8 * s);
                }
                let y0 = y - (fingers.len() - 1) as f32 * step;
                for (k, (_, finger, color)) in fingers.iter().enumerate() {
                    let at = pos2(x, y0 + k as f32 * step);
                    painter.text(
                        at,
                        Align2::CENTER_CENTER,
                        finger.to_string(),
                        font.clone(),
                        *color,
                    );
                }
                reach(y0 - 0.6 * s, y);
            }
            Clef::Bass => {
                let mut y = system
                    .y(clef, low.min(0) - 3)
                    .max(system.y(clef, low) + 1.6 * s);
                if let Some(end) = stem_end.filter(|_| !up) {
                    y = y.max(end + 0.8 * s);
                }
                for (k, (_, finger, color)) in fingers.iter().enumerate() {
                    let at = pos2(x, y + k as f32 * step);
                    painter.text(
                        at,
                        Align2::CENTER_CENTER,
                        finger.to_string(),
                        font.clone(),
                        *color,
                    );
                }
                reach(y, y + (fingers.len() as f32 - 0.5) * step);
            }
        }
    }
    (top, bottom)
}
