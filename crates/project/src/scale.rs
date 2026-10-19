//! A project's key: a root note and a scale, which the piano roll shows and
//! chord tools fit to.

use midi::Pitch;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScaleMode {
    Major,
    Minor,
    HarmonicMinor,
    MelodicMinor,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    Locrian,
    MajorPentatonic,
    MinorPentatonic,
    Blues,
}

impl ScaleMode {
    pub const ALL: [ScaleMode; 12] = [
        ScaleMode::Major,
        ScaleMode::Minor,
        ScaleMode::HarmonicMinor,
        ScaleMode::MelodicMinor,
        ScaleMode::Dorian,
        ScaleMode::Phrygian,
        ScaleMode::Lydian,
        ScaleMode::Mixolydian,
        ScaleMode::Locrian,
        ScaleMode::MajorPentatonic,
        ScaleMode::MinorPentatonic,
        ScaleMode::Blues,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ScaleMode::Major => "Major",
            ScaleMode::Minor => "Minor",
            ScaleMode::HarmonicMinor => "Harmonic minor",
            ScaleMode::MelodicMinor => "Melodic minor",
            ScaleMode::Dorian => "Dorian",
            ScaleMode::Phrygian => "Phrygian",
            ScaleMode::Lydian => "Lydian",
            ScaleMode::Mixolydian => "Mixolydian",
            ScaleMode::Locrian => "Locrian",
            ScaleMode::MajorPentatonic => "Major pentatonic",
            ScaleMode::MinorPentatonic => "Minor pentatonic",
            ScaleMode::Blues => "Blues",
        }
    }

    /// Semitones above the root, ascending.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            ScaleMode::Major => &[0, 2, 4, 5, 7, 9, 11],
            ScaleMode::Minor => &[0, 2, 3, 5, 7, 8, 10],
            ScaleMode::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            ScaleMode::MelodicMinor => &[0, 2, 3, 5, 7, 9, 11],
            ScaleMode::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            ScaleMode::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            ScaleMode::Lydian => &[0, 2, 4, 6, 7, 9, 11],
            ScaleMode::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            ScaleMode::Locrian => &[0, 1, 3, 5, 6, 8, 10],
            ScaleMode::MajorPentatonic => &[0, 2, 4, 7, 9],
            ScaleMode::MinorPentatonic => &[0, 3, 5, 7, 10],
            ScaleMode::Blues => &[0, 3, 5, 6, 7, 10],
        }
    }
}

/// Note names by pitch class, with sharps.
pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Scale {
    /// Pitch class of the root, `0..12` (0 is C).
    pub root: u8,
    pub mode: ScaleMode,
}

impl Scale {
    pub fn new(root: u8, mode: ScaleMode) -> Scale {
        Scale {
            root: root % 12,
            mode,
        }
    }

    /// "A minor", "F# Dorian".
    pub fn name(self) -> String {
        let mode = self.mode.name();
        let mode = if matches!(self.mode, ScaleMode::Major | ScaleMode::Minor) {
            mode.to_lowercase()
        } else {
            mode.to_owned()
        };
        format!("{} {mode}", NOTE_NAMES[usize::from(self.root % 12)])
    }

    /// Where `pitch` sits in the scale, counting from the root (0), or
    /// `None` if it's not in it.
    pub fn degree(self, pitch: Pitch) -> Option<usize> {
        let class = (pitch.get() + 12 - self.root % 12) % 12;
        self.mode.intervals().iter().position(|&i| i == class)
    }

    pub fn contains(self, pitch: Pitch) -> bool {
        self.degree(pitch).is_some()
    }

    pub fn is_root(self, pitch: Pitch) -> bool {
        pitch.get() % 12 == self.root % 12
    }

    /// The nearest pitch in the scale, preferring the one below on a tie.
    pub fn snap(self, pitch: Pitch) -> Pitch {
        let p = i16::from(pitch.get());
        (0..12)
            .flat_map(|d| [p - d, p + d])
            .filter_map(|q| u8::try_from(q).ok().and_then(Pitch::new))
            .find(|q| self.contains(*q))
            .unwrap_or(pitch)
    }

    /// `pitch` moved `steps` scale degrees (negative: down), from the
    /// scale note at or below it. `None` past the MIDI range.
    pub fn step(self, pitch: Pitch, steps: i32) -> Option<Pitch> {
        let intervals = self.mode.intervals();
        let n = intervals.len() as i32;
        // The scale note at or below, as (octave, degree) from the root at C-1.
        let below = (0..12u8)
            .filter_map(|d| pitch.get().checked_sub(d).and_then(Pitch::new))
            .find(|q| self.contains(*q))?;
        let from_root = i32::from(below.get()) - i32::from(self.root % 12);
        let octave = from_root.div_euclid(12);
        let degree = self.degree(below)? as i32 + steps;
        let (octave, degree) = (octave + degree.div_euclid(n), degree.rem_euclid(n));
        let value = i32::from(self.root % 12) + octave * 12 + i32::from(intervals[degree as usize]);
        u8::try_from(value).ok().and_then(Pitch::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u8) -> Pitch {
        Pitch::new(n).unwrap()
    }

    #[test]
    fn membership_and_names() {
        let a_minor = Scale::new(9, ScaleMode::Minor);
        assert_eq!(a_minor.name(), "A minor");
        assert_eq!(Scale::new(6, ScaleMode::Dorian).name(), "F# Dorian");
        // A B C D E F G.
        let ins: Vec<u8> = (60..72).filter(|n| a_minor.contains(p(*n))).collect();
        assert_eq!(ins, [60, 62, 64, 65, 67, 69, 71]);
        assert!(a_minor.is_root(p(69)) && a_minor.is_root(p(57)));
        assert_eq!(a_minor.degree(p(60)), Some(2));
    }

    #[test]
    fn snapping_and_stepping() {
        let c_major = Scale::new(0, ScaleMode::Major);
        assert_eq!(c_major.snap(p(61)), p(60), "ties go down");
        assert_eq!(c_major.snap(p(63)), p(62));
        assert_eq!(c_major.snap(p(64)), p(64));
        // Thirds: C → E → G → B → D.
        assert_eq!(c_major.step(p(60), 2), Some(p(64)));
        assert_eq!(c_major.step(p(64), 2), Some(p(67)));
        assert_eq!(c_major.step(p(71), 2), Some(p(74)));
        assert_eq!(c_major.step(p(60), -1), Some(p(59)));
        // From off the scale: counts from the note below.
        assert_eq!(c_major.step(p(61), 2), Some(p(64)));
        assert_eq!(c_major.step(p(127), 7), None);
        let blues = Scale::new(9, ScaleMode::Blues);
        assert_eq!(blues.step(p(69), 6), Some(p(81)), "an octave is six steps");
    }
}
