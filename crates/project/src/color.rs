use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b }
    }

    /// Colors handed out to new tracks in turn, and offered in the picker.
    pub const PALETTE: [Color; 12] = [
        Color::rgb(0xE8, 0x7A, 0x5D),
        Color::rgb(0xF2, 0xB1, 0x34),
        Color::rgb(0xC9, 0xD1, 0x4F),
        Color::rgb(0x7B, 0xC9, 0x6F),
        Color::rgb(0x4F, 0xBF, 0xA8),
        Color::rgb(0x4B, 0xB3, 0xD9),
        Color::rgb(0x5D, 0x8C, 0xE8),
        Color::rgb(0x8E, 0x7B, 0xE0),
        Color::rgb(0xC0, 0x7B, 0xD9),
        Color::rgb(0xE0, 0x6F, 0xA6),
        Color::rgb(0xA8, 0x8B, 0x6E),
        Color::rgb(0x9A, 0xA5, 0xB1),
    ];
}
