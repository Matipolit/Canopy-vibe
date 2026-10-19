//! Small pieces the views share.

use eframe::egui::{self, Color32, Painter, Rect, RichText, Sense, Stroke, StrokeKind, Ui, vec2};

use super::theme;
use crate::theory::KeySig;

/// A sheet of paper `height` tall across the available width, for notation.
pub fn paper(ui: &mut Ui, height: f32) -> (Rect, Painter) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, theme::PAPER);
    painter.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0, Color32::from_black_alpha(90)),
        StrokeKind::Inside,
    );
    (rect, painter)
}

/// Buttons for one of several choices; true if it changed.
pub fn choice<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    for &(option, label) in options {
        if ui.selectable_label(*value == option, label).clicked() && *value != option {
            *value = option;
            changed = true;
        }
    }
    changed
}

/// A label and a number, the number stronger: "Streak 5". A one-symbol
/// label ("✔") takes the number's color.
pub fn stat(ui: &mut Ui, label: &str, value: impl Into<String>, color: Option<Color32>) {
    match color {
        Some(color) if label.chars().count() == 1 => ui.label(RichText::new(label).color(color)),
        _ => ui.label(RichText::new(label).weak()),
    };
    let mut text = RichText::new(value.into()).strong().monospace();
    if let Some(color) = color {
        text = text.color(color);
    }
    ui.label(text);
}

pub fn percent(value: Option<f64>) -> String {
    value.map_or_else(|| "–".to_owned(), |v| format!("{:.0}%", v * 100.0))
}

/// Every key signature, for pickers: "C major", "G major (1#)"…
pub fn key_label(key: KeySig) -> String {
    if key.0 == 0 {
        key.label()
    } else {
        format!("{} ({})", key.label(), key.count_label())
    }
}

/// The keys from seven flats to seven sharps, in circle-of-fifths order.
pub fn all_keys() -> impl Iterator<Item = KeySig> {
    (-7..=7).map(KeySig)
}

/// A tempo field like the DAW's.
pub fn bpm_drag(value: &mut f64) -> egui::DragValue<'_> {
    egui::DragValue::new(value)
        .range(30.0..=220.0)
        .speed(0.5)
        .update_while_editing(false)
        .fixed_decimals(0)
        .suffix(" BPM")
}
