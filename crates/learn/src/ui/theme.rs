//! Colors shared by the views. Feedback colors match the DAW's: its MIDI
//! light's green and its record button's red.

use eframe::egui::Color32;

use crate::theory::Hand;

/// Sheet music: ink on paper, whatever the UI theme.
pub const PAPER: Color32 = Color32::from_rgb(250, 248, 241);
pub const INK: Color32 = Color32::from_rgb(22, 22, 28);
/// Notes still to come, on paper.
pub const FAINT_INK: Color32 = Color32::from_rgb(150, 148, 142);

pub const RIGHT: Color32 = Color32::from_rgb(70, 170, 90);
pub const WRONG: Color32 = Color32::from_rgb(220, 60, 60);
/// A note to play next.
pub const TARGET: Color32 = Color32::from_rgb(40, 120, 230);
/// Played, but not the first time.
pub const LATE: Color32 = Color32::from_rgb(225, 145, 40);

/// Notes each hand plays, in songs and on the keys.
pub const RIGHT_HAND: Color32 = Color32::from_rgb(90, 175, 245);
pub const LEFT_HAND: Color32 = Color32::from_rgb(245, 160, 70);

/// What the keyboard shows for keys held down.
pub const HELD: Color32 = Color32::from_rgb(120, 140, 255);

pub fn hand(hand: Hand) -> Color32 {
    match hand {
        Hand::Right => RIGHT_HAND,
        Hand::Left => LEFT_HAND,
    }
}

/// The falling-notes lane's background.
pub const LANE: Color32 = Color32::from_rgb(24, 26, 32);
pub const LANE_LINE: Color32 = Color32::from_rgb(44, 47, 56);
pub const LANE_BAR: Color32 = Color32::from_rgb(70, 74, 86);
