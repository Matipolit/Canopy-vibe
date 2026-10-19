//! A piano keyboard with spans drawn above it: which keys a sampler's
//! regions cover, or which notes a drum kit answers to. Pressing a key
//! plays it.

use eframe::egui::{self, Align2, Color32, FontId, Id, Rect, Sense, Stroke, pos2, vec2};

/// A span of keys to mark above the keyboard, on its own row.
#[derive(Debug, Clone, Copy)]
pub struct Span {
    pub low: u8,
    pub high: u8,
    /// A key to mark inside it (a region's root).
    pub root: Option<u8>,
    pub row: usize,
    pub color: Color32,
}

/// A key pressed or let go this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Down(u8),
    Up(u8),
}

pub struct KeyMap<'a> {
    id: Id,
    low: u8,
    high: u8,
    spans: &'a [Span],
    /// Keys sounding now, lit.
    lit: &'a [u8],
    size: egui::Vec2,
}

const BLACK: [bool; 12] = [
    false, true, false, true, false, false, true, false, true, false, true, false,
];

impl<'a> KeyMap<'a> {
    pub fn new(id: Id, low: u8, high: u8, spans: &'a [Span]) -> Self {
        KeyMap {
            id,
            low: low.min(high),
            high: high.max(low),
            spans,
            lit: &[],
            size: vec2(400.0, 80.0),
        }
    }

    pub fn lit(mut self, keys: &'a [u8]) -> Self {
        self.lit = keys;
        self
    }

    pub fn size(mut self, size: egui::Vec2) -> Self {
        self.size = size;
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> Option<KeyEvent> {
        let (rect, response) = ui.allocate_exact_size(self.size, Sense::click_and_drag());
        let visuals = ui.visuals().clone();
        let painter = ui.painter_at(rect);
        let rows = self.spans.iter().map(|s| s.row + 1).max().unwrap_or(0);
        let row_height = 5.0;
        let spans_height = rows as f32 * (row_height + 2.0) + 2.0;
        let keys = Rect::from_min_max(pos2(rect.left(), rect.top() + spans_height), rect.max);
        // A piano's layout: white keys side by side, black ones narrower,
        // over the gaps between them.
        let is_black = |key: u8| BLACK[usize::from(key % 12)];
        let whites_before = |key: u8| (self.low..key).filter(|k| !is_black(*k)).count();
        let white_count = whites_before(self.high) + usize::from(!is_black(self.high));
        let width = keys.width() / white_count.max(1) as f32;
        let key_rect = |key: u8| {
            let i = whites_before(key) as f32;
            if is_black(key) {
                Rect::from_center_size(
                    pos2(keys.left() + i * width, keys.top() + keys.height() * 0.3),
                    vec2(width * 0.6, keys.height() * 0.6),
                )
            } else {
                Rect::from_min_size(
                    pos2(keys.left() + i * width, keys.top()),
                    vec2(width, keys.height()),
                )
            }
        };

        // White keys, then black ones over them.
        let lit_color = visuals.selection.bg_fill;
        for black in [false, true] {
            for key in (self.low..=self.high).filter(|k| is_black(*k) == black) {
                let lit = self.lit.contains(&key);
                let fill = match (black, lit) {
                    (_, true) => lit_color,
                    (true, false) => Color32::from_gray(30),
                    (false, false) => Color32::from_gray(205),
                };
                let r = key_rect(key);
                painter.rect_filled(r.shrink2(vec2(0.5, 0.0)), 1.0, fill);
            }
        }
        for key in self.low..=self.high {
            let r = key_rect(key);
            if key % 12 == 0 && width * 7.0 > 30.0 {
                painter.text(
                    pos2(r.center().x, keys.bottom() - 2.0),
                    Align2::CENTER_BOTTOM,
                    format!("C{}", i32::from(key) / 12 - 1),
                    FontId::proportional(crate::theme::text::TINY),
                    Color32::from_gray(60),
                );
            }
        }
        for span in self.spans {
            let (low, high) = (span.low.max(self.low), span.high.min(self.high));
            if low > high {
                continue;
            }
            let top = rect.top() + 2.0 + span.row as f32 * (row_height + 2.0);
            let bar = Rect::from_min_max(
                pos2(key_rect(low).left(), top),
                pos2(key_rect(high).right(), top + row_height),
            );
            painter.rect_filled(bar, 1.5, span.color.gamma_multiply(0.75));
            if let Some(root) = span.root.filter(|r| (low..=high).contains(r)) {
                painter.rect_stroke(
                    Rect::from_center_size(
                        pos2(key_rect(root).center().x, bar.center().y),
                        vec2(width.max(3.0), row_height + 2.0),
                    ),
                    1.0,
                    Stroke::new(1.0, visuals.strong_text_color()),
                    egui::StrokeKind::Outside,
                );
            }
        }

        // Pressing plays; sliding across keys plays each in turn.
        let held_id = self.id.with("held");
        let held: Option<u8> = ui.data(|d| d.get_temp(held_id)).flatten();
        // Black keys first: they sit on top.
        let under = response.interact_pointer_pos().and_then(|pos| {
            let hit = |black: bool| {
                (self.low..=self.high)
                    .filter(|k| is_black(*k) == black)
                    .find(|k| key_rect(*k).contains(pos))
            };
            hit(true).or_else(|| hit(false))
        });
        let pressed = response.is_pointer_button_down_on();
        let now = if pressed { under } else { None };
        let event = match (held, now) {
            (Some(old), Some(new)) if old != new => {
                // Release first; the new key goes down next frame.
                Some(KeyEvent::Up(old))
            }
            (Some(old), None) => Some(KeyEvent::Up(old)),
            (None, Some(new)) => Some(KeyEvent::Down(new)),
            _ => None,
        };
        let held = match event {
            Some(KeyEvent::Down(key)) => Some(key),
            Some(KeyEvent::Up(_)) => None,
            None => held,
        };
        ui.data_mut(|d| d.insert_temp(held_id, held));
        if event.is_some() {
            ui.ctx().request_repaint();
        }
        event
    }
}
