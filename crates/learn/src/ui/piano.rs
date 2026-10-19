//! The on-screen piano: keys held, notes to play next with their fingers,
//! right and wrong flashes, the app's notes in hand colors. Pressing keys
//! plays them, and sliding across plays each in turn.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Id, Pos2, Rangef, Rect, Sense, Stroke, StrokeKind,
    Ui, pos2, vec2,
};
use serde::{Deserialize, Serialize};

use crate::theory::{is_black, pitch_label};

/// How many keys to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Size {
    /// As many as fit comfortably.
    #[default]
    Fit,
    Keys88,
    Keys76,
    Keys61,
    Keys49,
}

impl Size {
    pub const ALL: [Size; 5] = [
        Size::Fit,
        Size::Keys88,
        Size::Keys76,
        Size::Keys61,
        Size::Keys49,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Size::Fit => "Fit to window",
            Size::Keys88 => "88 keys",
            Size::Keys76 => "76 keys",
            Size::Keys61 => "61 keys",
            Size::Keys49 => "49 keys",
        }
    }

    /// The lowest and highest key for a keyboard `width` pixels wide.
    pub fn range(self, width: f32) -> (u8, u8) {
        const RANGES: [(u8, u8); 5] = [(21, 108), (28, 103), (36, 96), (36, 84), (48, 84)];
        match self {
            Size::Keys88 => RANGES[0],
            Size::Keys76 => RANGES[1],
            Size::Keys61 => RANGES[2],
            Size::Keys49 => RANGES[3],
            Size::Fit => RANGES
                .iter()
                .copied()
                .find(|&(low, high)| width / white_keys(low, high) as f32 >= 14.0)
                .unwrap_or(RANGES[4]),
        }
    }
}

fn white_keys(low: u8, high: u8) -> usize {
    (low..=high).filter(|k| !is_black(*k)).count()
}

/// Where each key is, for drawing and for the falling notes above.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub rect: Rect,
    pub low: u8,
    pub high: u8,
    white: f32,
}

/// Black keys sit a little off the gap between white keys, as on a piano.
fn black_offset(pitch: u8) -> f32 {
    match pitch % 12 {
        1 => -0.08,
        3 => 0.08,
        6 => -0.1,
        8 => 0.0,
        10 => 0.1,
        _ => 0.0,
    }
}

impl Layout {
    pub fn new(rect: Rect, low: u8, high: u8) -> Layout {
        // Start and end on white keys.
        let low = if is_black(low) { low - 1 } else { low };
        let high = if is_black(high) { high + 1 } else { high };
        let white = rect.width() / white_keys(low, high).max(1) as f32;
        Layout {
            rect,
            low,
            high,
            white,
        }
    }

    pub fn contains(&self, pitch: u8) -> bool {
        (self.low..=self.high).contains(&pitch)
    }

    fn whites_before(&self, pitch: u8) -> usize {
        (self.low..pitch).filter(|k| !is_black(*k)).count()
    }

    pub fn black_height(&self) -> f32 {
        self.rect.height() * 0.63
    }

    /// A key's left and right edges: what a falling note lines up with.
    pub fn lane(&self, pitch: u8) -> Option<Rangef> {
        if !self.contains(pitch) {
            return None;
        }
        let x = self.rect.left() + self.whites_before(pitch) as f32 * self.white;
        Some(if is_black(pitch) {
            let center = x + black_offset(pitch) * self.white;
            let half = self.white * 0.29;
            Rangef::new(center - half, center + half)
        } else {
            Rangef::new(x, x + self.white)
        })
    }

    pub fn key_rect(&self, pitch: u8) -> Option<Rect> {
        let lane = self.lane(pitch)?;
        let bottom = if is_black(pitch) {
            self.rect.top() + self.black_height()
        } else {
            self.rect.bottom()
        };
        Some(Rect::from_x_y_ranges(lane, self.rect.top()..=bottom))
    }

    /// The key under `pos`: black keys first, they're on top.
    pub fn key_at(&self, pos: Pos2) -> Option<u8> {
        if !self.rect.contains(pos) {
            return None;
        }
        let hit = |black: bool| {
            (self.low..=self.high)
                .filter(|k| is_black(*k) == black)
                .find(|k| self.key_rect(*k).is_some_and(|r| r.contains(pos)))
        };
        hit(true).or_else(|| hit(false))
    }
}

/// How a key looks this frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Look {
    /// Pressed, or flashing.
    pub fill: Option<Color32>,
    /// To be played next: a band of color at its front.
    pub hint: Option<Color32>,
    pub finger: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Down { pitch: u8, velocity: u8 },
    Up(u8),
}

/// Draws the keyboard in `rect` and returns what the mouse played.
pub fn show(ui: &mut Ui, layout: &Layout, looks: &[Look; 128], names: bool) -> Vec<KeyEvent> {
    let rect = layout.rect;
    let painter = ui.painter_at(rect.expand(1.0));
    painter.rect_filled(rect, 0.0, Color32::from_gray(18));

    let white_fill = Color32::from_gray(238);
    let black_fill = Color32::from_gray(26);
    let edge = Stroke::new(1.0, Color32::from_gray(120));
    let small = FontId::proportional((layout.white * 0.42).clamp(8.0, 11.0));

    for key in (layout.low..=layout.high).filter(|k| !is_black(*k)) {
        let Some(r) = layout.key_rect(key) else {
            continue;
        };
        let r = r.shrink2(vec2(0.5, 0.0));
        let look = looks[usize::from(key)];
        let radius = CornerRadius {
            nw: 0,
            ne: 0,
            sw: 3,
            se: 3,
        };
        painter.rect_filled(r, radius, look.fill.unwrap_or(white_fill));
        painter.rect_stroke(r, radius, edge, StrokeKind::Inside);
        if let Some(hint) = look.hint {
            let band = Rect::from_min_max(
                pos2(r.left() + 2.0, r.bottom() - r.height() * 0.3),
                r.max - vec2(2.0, 3.0),
            );
            painter.rect_filled(band, 3.0, hint.gamma_multiply(0.85));
        }
        let label_y = r.bottom() - 3.0;
        let mut label = None;
        if key % 12 == 0 && layout.white >= 11.0 {
            label = Some(pitch_label(key));
        } else if names && layout.white >= 14.0 {
            label = Some(
                pitch_label(key)
                    .trim_end_matches(char::is_numeric)
                    .to_owned(),
            );
        }
        if let Some(text) = label {
            let color = if key == 60 {
                Color32::from_gray(20)
            } else {
                Color32::from_gray(95)
            };
            painter.text(
                pos2(r.center().x, label_y),
                Align2::CENTER_BOTTOM,
                text,
                small.clone(),
                color,
            );
        }
        if let Some(finger) = look.finger {
            finger_badge(
                &painter,
                pos2(r.center().x, r.bottom() - r.height() * 0.3 - 2.0),
                layout.white,
                finger,
            );
        }
    }
    for key in (layout.low..=layout.high).filter(|k| is_black(*k)) {
        let Some(r) = layout.key_rect(key) else {
            continue;
        };
        let look = looks[usize::from(key)];
        let fill = look.fill.unwrap_or(black_fill);
        painter.rect_filled(
            r,
            CornerRadius {
                nw: 0,
                ne: 0,
                sw: 2,
                se: 2,
            },
            fill,
        );
        // A lighter front edge, as on a real key.
        if look.fill.is_none() {
            let front = Rect::from_min_max(
                pos2(r.left() + 1.5, r.bottom() - 5.0),
                r.max - vec2(1.5, 1.5),
            );
            painter.rect_filled(front, 1.0, Color32::from_gray(62));
        }
        if let Some(hint) = look.hint {
            let band = Rect::from_min_max(
                pos2(r.left() + 2.0, r.bottom() - r.height() * 0.35),
                r.max - vec2(2.0, 2.0),
            );
            painter.rect_filled(band, 2.0, hint);
        }
        if let Some(finger) = look.finger {
            finger_badge(
                &painter,
                pos2(r.center().x, r.bottom() - r.height() * 0.35 - 2.0),
                layout.white,
                finger,
            );
        }
    }
    // The felt strip along the back.
    painter.rect_filled(
        Rect::from_min_size(rect.min, vec2(rect.width(), 3.0)),
        0.0,
        Color32::from_rgb(120, 26, 36),
    );

    mouse(ui, layout)
}

fn finger_badge(painter: &egui::Painter, center: Pos2, white: f32, finger: u8) {
    let r = (white * 0.36).clamp(6.0, 10.0);
    let center = center - vec2(0.0, r);
    painter.circle(
        center,
        r,
        Color32::from_rgb(255, 255, 255),
        Stroke::new(1.5, Color32::from_gray(30)),
    );
    painter.text(
        center,
        Align2::CENTER_CENTER,
        finger.to_string(),
        FontId::proportional(r * 1.45),
        Color32::from_gray(20),
    );
}

/// Pressing plays the key under the pointer, harder towards the front.
fn mouse(ui: &mut Ui, layout: &Layout) -> Vec<KeyEvent> {
    let id = Id::new("piano_keys");
    let response = ui.interact(layout.rect, id, Sense::click_and_drag());
    let held_id = id.with("held");
    let held: Option<u8> = ui.data(|d| d.get_temp(held_id)).flatten();
    let now = if response.is_pointer_button_down_on() {
        response
            .interact_pointer_pos()
            .and_then(|p| layout.key_at(p).map(|k| (k, p)))
    } else {
        None
    };
    let mut events = Vec::new();
    if held.is_some()
        && held != now.map(|n| n.0)
        && let Some(old) = held
    {
        events.push(KeyEvent::Up(old));
    }
    if let Some((key, pos)) = now
        && held != Some(key)
    {
        let depth = layout
            .key_rect(key)
            .map_or(0.7, |r| ((pos.y - r.top()) / r.height()).clamp(0.0, 1.0));
        let velocity = (40.0 + 87.0 * depth).round() as u8;
        events.push(KeyEvent::Down {
            pitch: key,
            velocity,
        });
    }
    ui.data_mut(|d| d.insert_temp(held_id, now.map(|n| n.0)));
    if !events.is_empty() {
        ui.ctx().request_repaint();
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_fit_the_width() {
        assert_eq!(Size::Fit.range(1280.0), (21, 108));
        assert_eq!(Size::Fit.range(708.0), (28, 103));
        assert_eq!(Size::Fit.range(520.0), (36, 96));
        assert_eq!(Size::Fit.range(100.0), (48, 84));
        assert_eq!(Size::Keys61.range(4000.0), (36, 96));
    }

    #[test]
    fn keys_tile_the_keyboard() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(52.0 * 20.0, 100.0));
        let layout = Layout::new(rect, 21, 108);
        assert_eq!(layout.lane(21).unwrap(), Rangef::new(0.0, 20.0));
        assert_eq!(layout.lane(108).unwrap().max, 1040.0);
        // C4 is the 24th white key from A0.
        assert_eq!(layout.lane(60).unwrap().min, 23.0 * 20.0);
        let c_sharp = layout.lane(61).unwrap();
        assert!(c_sharp.min < 24.0 * 20.0 && c_sharp.max > 24.0 * 20.0);
        assert_eq!(
            layout.key_at(pos2(24.0 * 20.0, 10.0)),
            Some(61),
            "black on top"
        );
        assert_eq!(layout.key_at(pos2(24.0 * 20.0 - 2.0, 90.0)), Some(60));
        assert_eq!(layout.key_at(pos2(-5.0, 50.0)), None);
        assert!(layout.lane(20).is_none());
        // A range starting on a black key widens to the white below.
        assert_eq!(Layout::new(rect, 61, 72).low, 60);
    }
}
