/// A MIDI note number, `0..=127`. 60 is middle C, 69 is A4 (440 Hz).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// Deserializing checks the range, so a hand-edited file can't smuggle in note 200.
#[cfg_attr(feature = "serde", serde(try_from = "u8", into = "u8"))]
#[repr(transparent)]
pub struct Pitch(u8);

impl Pitch {
    pub const MIN: Pitch = Pitch(0);
    pub const MAX: Pitch = Pitch(127);
    pub const MIDDLE_C: Pitch = Pitch(60);
    pub const A4: Pitch = Pitch(69);

    pub const fn new(note: u8) -> Option<Pitch> {
        if note <= Self::MAX.0 {
            Some(Pitch(note))
        } else {
            None
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub fn frequency(self) -> f32 {
        440.0 * 2f32.powf((self.0 as f32 - Self::A4.0 as f32) / 12.0)
    }
}

impl TryFrom<u8> for Pitch {
    type Error = u8;

    /// Fails with the original value if it's above 127.
    fn try_from(note: u8) -> Result<Self, Self::Error> {
        Pitch::new(note).ok_or(note)
    }
}

impl From<Pitch> for u8 {
    fn from(pitch: Pitch) -> u8 {
        pitch.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_notes_above_127() {
        assert_eq!(Pitch::new(127), Some(Pitch::MAX));
        assert_eq!(Pitch::new(128), None);
        assert_eq!(Pitch::try_from(200), Err(200));
    }

    #[test]
    fn frequencies() {
        assert_eq!(Pitch::A4.frequency(), 440.0);
        assert!((Pitch::MIDDLE_C.frequency() - 261.626).abs() < 1e-3);
        assert!((Pitch::new(81).unwrap().frequency() - 880.0).abs() < 1e-3);
    }
}
