//! Playing on the computer keyboard, laid out like FL Studio's typing
//! keyboard: the bottom letter row is one octave (Z = C, S = C♯, X = D…),
//! the top row the octave above (Q = C, 2 = C♯, W = D…).

use std::collections::HashMap;

use eframe::egui::Key;

/// Semitones above the lower row's C.
pub fn semitone(key: Key) -> Option<u8> {
    use Key::*;
    Some(match key {
        Z => 0,
        S => 1,
        X => 2,
        D => 3,
        C => 4,
        V => 5,
        G => 6,
        B => 7,
        H => 8,
        N => 9,
        J => 10,
        M => 11,
        Comma => 12,
        L => 13,
        Period => 14,
        Semicolon => 15,
        Slash => 16,
        Q => 12,
        Num2 => 13,
        W => 14,
        Num3 => 15,
        E => 16,
        R => 17,
        Num5 => 18,
        T => 19,
        Num6 => 20,
        Y => 21,
        Num7 => 22,
        U => 23,
        I => 24,
        Num9 => 25,
        O => 26,
        Num0 => 27,
        P => 28,
        _ => return None,
    })
}

pub struct Typing {
    /// The octave of the lower row's C: 3 puts Z on C3 and Q on middle C.
    pub octave: i8,
    /// Keys down and the pitch each started, so shifting the octave
    /// mid-note still ends the right one.
    held: HashMap<Key, u8>,
}

impl Default for Typing {
    fn default() -> Self {
        Typing {
            octave: 3,
            held: HashMap::new(),
        }
    }
}

impl Typing {
    /// A key went down or up: the pitch to start or stop, if it's one of
    /// the piano keys. Auto-repeat is ignored.
    pub fn key(&mut self, key: Key, pressed: bool, repeat: bool) -> Option<(u8, bool)> {
        if pressed {
            if repeat || self.held.contains_key(&key) {
                return None;
            }
            let pitch = 12 * (i32::from(self.octave) + 1) + i32::from(semitone(key)?);
            let pitch = u8::try_from(pitch).ok().filter(|p| *p <= 127)?;
            self.held.insert(key, pitch);
            Some((pitch, true))
        } else {
            self.held.remove(&key).map(|p| (p, false))
        }
    }

    pub fn shift(&mut self, octaves: i8) {
        self.octave = (self.octave + octaves).clamp(0, 7);
    }

    /// Lets go of everything, e.g. when the window loses focus.
    pub fn release_all(&mut self) -> Vec<u8> {
        self.held.drain().map(|(_, p)| p).collect()
    }

    /// The lowest and highest pitch the rows reach now.
    pub fn span(&self) -> (u8, u8) {
        let low = 12 * (self.octave as u8 + 1);
        (low, low + 28)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_octaves() {
        let mut t = Typing::default();
        assert_eq!(t.key(Key::Z, true, false), Some((48, true)));
        assert_eq!(t.key(Key::Q, true, false), Some((60, true)));
        assert_eq!(t.key(Key::Num2, true, false), Some((61, true)));
        assert_eq!(t.key(Key::Comma, true, false), Some((60, true)));
        assert_eq!(t.key(Key::P, true, false), Some((76, true)));
        assert_eq!(t.key(Key::A, true, false), None, "not a piano key");
        assert_eq!(t.key(Key::Z, true, true), None, "auto-repeat");
        assert_eq!(t.key(Key::Z, true, false), None, "already down");
        assert_eq!(t.key(Key::Z, false, false), Some((48, false)));
        assert_eq!(t.key(Key::Z, false, false), None);
    }

    #[test]
    fn octave_shifts_keep_held_notes() {
        let mut t = Typing::default();
        t.key(Key::X, true, false);
        t.shift(1);
        assert_eq!(
            t.key(Key::X, false, false),
            Some((50, false)),
            "the D that started"
        );
        assert_eq!(t.key(Key::X, true, false), Some((62, true)));
        t.shift(10);
        assert_eq!(t.octave, 7);
        assert_eq!(t.span(), (96, 124));
        let mut released = t.release_all();
        released.sort();
        assert_eq!(released, [62]);
    }
}
