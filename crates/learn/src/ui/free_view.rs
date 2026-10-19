//! Free play: whatever is held, on the grand staff and by name, with the
//! chord it makes.

use eframe::egui::{self, Align2, FontId, RichText, Ui, pos2};

use super::app::Shared;
use super::staff::{self, Head, Style, System};
use super::{theme, widgets};
use crate::notation::{Accidentals, Clef};
use crate::theory::{self, KeySig, inversion_name, recognize};

pub struct FreeView {
    /// The key notes are spelled in.
    pub key: KeySig,
}

const CLASS_NAMES: [&str; 12] = [
    "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

impl FreeView {
    pub fn ui(&mut self, ui: &mut Ui, shared: &Shared) {
        ui.horizontal(|ui| {
            ui.label("Key");
            egui::ComboBox::from_id_salt("free_key")
                .selected_text(widgets::key_label(self.key))
                .width(170.0)
                .show_ui(ui, |ui| {
                    for key in widgets::all_keys() {
                        ui.selectable_value(&mut self.key, key, widgets::key_label(key));
                    }
                })
                .response
                .on_hover_text("Spell notes in this key");
            if shared.pedal {
                ui.separator();
                ui.label(RichText::new("Ped.").strong().italics());
            }
        });
        ui.add_space(6.0);

        let held = shared.held_keys();
        let height = (ui.available_height() - 4.0).clamp(160.0, 420.0);
        let (rect, painter) = widgets::paper(ui, height);
        let staff_rect = egui::Rect::from_min_max(
            rect.min,
            pos2(rect.left() + rect.width() * 0.55, rect.bottom()),
        );
        let system = System::new(
            staff_rect.shrink(8.0),
            &[Clef::Treble, Clef::Bass],
            self.key,
            16.0,
            4.0,
        );
        system.draw(&painter, theme::INK);
        let mut accidentals = Accidentals::new(self.key);
        let heads: Vec<Head> = held
            .iter()
            .map(|&p| {
                let note = theory::spell(p, self.key);
                Head {
                    note,
                    clef: Clef::for_pitch(p),
                    color: theme::INK,
                    accidental: accidentals.write(note),
                    finger: None,
                }
            })
            .collect();
        let x = (system.notes_left + system.right) / 2.0;
        let style = Style {
            stem: false,
            paper: theme::PAPER,
            ink: theme::INK,
            finger_color: None,
        };
        staff::chord(&painter, &system, x, &heads, style);

        // The chord's name, and the notes.
        let text_x = staff_rect.right() + 24.0;
        let mut y = rect.top() + rect.height() * 0.3;
        if let Some(chord) = recognize(&held) {
            let big = FontId::proportional(44.0);
            painter.text(
                pos2(text_x, y),
                Align2::LEFT_CENTER,
                chord.symbol(),
                big,
                theme::INK,
            );
            y += 40.0;
            let name = format!(
                "{} {}, {}",
                CLASS_NAMES[usize::from(chord.root % 12)],
                chord.kind.name(),
                inversion_name(chord.inversion)
            );
            painter.text(
                pos2(text_x, y),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(18.0),
                theme::FAINT_INK,
            );
        }
        y += 44.0;
        let names: Vec<String> = held
            .iter()
            .map(|&p| theory::spell(p, self.key).label())
            .collect();
        painter.text(
            pos2(text_x, y),
            Align2::LEFT_CENTER,
            names.join("  "),
            FontId::monospace(18.0),
            theme::INK,
        );
    }
}
