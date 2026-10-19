//! Where notes go on a staff: lines and spaces, ledger lines, the key
//! signature, which accidentals to write, and how a chord's heads and
//! accidentals make room for each other. Drawing is in `ui::staff`.
//!
//! Positions count half-spaces up from the bottom line: 0 is the bottom
//! line, 1 the space above it, 8 the top line.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::theory::{KeySig, Spelled};

pub const TOP_LINE: i32 = 8;
pub const MIDDLE_LINE: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Clef {
    Treble,
    Bass,
}

impl Clef {
    pub fn label(self) -> &'static str {
        match self {
            Clef::Treble => "Treble",
            Clef::Bass => "Bass",
        }
    }

    /// The note on the bottom line.
    pub fn bottom(self) -> Spelled {
        match self {
            Clef::Treble => Spelled::new(2, 0, 4),
            Clef::Bass => Spelled::new(4, 0, 2),
        }
    }

    pub fn position(self, note: Spelled) -> i32 {
        note.step() - self.bottom().step()
    }

    /// The natural note at `position`.
    pub fn note_at(self, position: i32) -> Spelled {
        Spelled::from_step(self.bottom().step() + position, 0)
    }

    /// Middle C and up on the treble staff, below it on the bass.
    pub fn for_pitch(pitch: u8) -> Clef {
        if pitch >= 60 {
            Clef::Treble
        } else {
            Clef::Bass
        }
    }

    /// Where each accidental of `key` is written, in order.
    pub fn key_positions(self, key: KeySig) -> Vec<(i32, i8)> {
        const TREBLE_SHARPS: [i32; 7] = [8, 5, 9, 6, 3, 7, 4];
        const TREBLE_FLATS: [i32; 7] = [4, 7, 3, 6, 2, 5, 1];
        let shift = match self {
            Clef::Treble => 0,
            Clef::Bass => -2,
        };
        let n = usize::from(key.0.unsigned_abs()).min(7);
        let (places, sign) = if key.0 >= 0 {
            (&TREBLE_SHARPS, 1)
        } else {
            (&TREBLE_FLATS, -1)
        };
        places[..n].iter().map(|p| (p + shift, sign)).collect()
    }
}

/// The ledger lines a note at `position` needs, nearest the staff first.
pub fn ledger_lines(position: i32) -> Vec<i32> {
    if position <= -2 {
        (position..=-2).rev().filter(|p| p % 2 == 0).collect()
    } else if position >= TOP_LINE + 2 {
        (TOP_LINE + 2..=position).filter(|p| p % 2 == 0).collect()
    } else {
        Vec::new()
    }
}

/// Which accidentals to write in a bar: an accidental lasts until the bar
/// line, so a note only needs one when it differs from what's in force.
#[derive(Debug, Clone)]
pub struct Accidentals {
    key: KeySig,
    /// By diatonic step: what an earlier note in the bar changed it to.
    altered: HashMap<i32, i8>,
}

impl Accidentals {
    pub fn new(key: KeySig) -> Accidentals {
        Accidentals {
            key,
            altered: HashMap::new(),
        }
    }

    /// The accidental to write before `note` (0 is a natural sign), if any.
    pub fn write(&mut self, note: Spelled) -> Option<i8> {
        let current = self
            .altered
            .get(&note.step())
            .copied()
            .unwrap_or_else(|| self.key.accidental(note.letter));
        if note.accidental == current {
            return None;
        }
        self.altered.insert(note.step(), note.accidental);
        Some(note.accidental)
    }

    /// A bar line: accidentals stop.
    pub fn bar(&mut self) {
        self.altered.clear();
    }
}

/// Stem up for notes mostly below the middle line, down otherwise.
pub fn stem_up(positions: &[i32]) -> bool {
    let (Some(low), Some(high)) = (positions.iter().min(), positions.iter().max()) else {
        return true;
    };
    MIDDLE_LINE - low > high - MIDDLE_LINE
}

/// For each head of a chord (positions in any order), which side of the
/// stem it goes: 0 on its usual side, +1 one head-width right, -1 left.
/// Heads a second apart can't share a column, so every other one moves.
pub fn head_offsets(positions: &[i32], up: bool) -> Vec<i32> {
    let mut order: Vec<usize> = (0..positions.len()).collect();
    order.sort_by_key(|&i| positions[i]);
    if !up {
        order.reverse();
    }
    let mut offsets = vec![0; positions.len()];
    let mut previous: Option<(i32, bool)> = None;
    for i in order {
        let moved = matches!(previous, Some((p, false)) if (positions[i] - p).abs() == 1);
        if moved {
            offsets[i] = if up { 1 } else { -1 };
        }
        previous = Some((positions[i], moved));
    }
    offsets
}

/// For each accidental of a chord, given by position, which column it goes
/// in: 0 next to the heads, 1 left of that, and so on, so none is closer
/// than a sixth to another in its column. As engravers do it: the top one
/// nearest, then the bottom one, then the next from the top…
pub fn accidental_columns(accidentals: &[i32]) -> Vec<usize> {
    let mut sorted: Vec<usize> = (0..accidentals.len()).collect();
    sorted.sort_by_key(|&i| std::cmp::Reverse(accidentals[i]));
    let mut order = Vec::with_capacity(sorted.len());
    let (mut top, mut bottom) = (0, sorted.len());
    while top < bottom {
        order.push(sorted[top]);
        top += 1;
        if top < bottom {
            bottom -= 1;
            order.push(sorted[bottom]);
        }
    }
    let mut columns: Vec<Vec<i32>> = Vec::new();
    let mut placed = vec![0; accidentals.len()];
    for i in order {
        let p = accidentals[i];
        let column = columns
            .iter()
            .position(|c| c.iter().all(|q| (q - p).abs() >= 6))
            .unwrap_or_else(|| {
                columns.push(Vec::new());
                columns.len() - 1
            });
        columns[column].push(p);
        placed[i] = column;
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_on_each_clef() {
        assert_eq!(
            Clef::Treble.position(Spelled::new(2, 0, 4)),
            0,
            "E4 bottom line"
        );
        assert_eq!(
            Clef::Treble.position(Spelled::new(3, 0, 5)),
            8,
            "F5 top line"
        );
        assert_eq!(Clef::Treble.position(Spelled::MIDDLE_C), -2);
        assert_eq!(Clef::Bass.position(Spelled::MIDDLE_C), 10);
        assert_eq!(Clef::Bass.position(Spelled::new(5, 0, 3)), 8, "A3 top line");
        assert_eq!(
            Clef::Bass.position(Spelled::new(3, 1, 3)),
            6,
            "F#3 on the F line"
        );
        assert_eq!(Clef::Bass.note_at(0).label(), "G2");
        assert_eq!(Clef::for_pitch(60), Clef::Treble);
        assert_eq!(Clef::for_pitch(59), Clef::Bass);
    }

    #[test]
    fn ledger_lines_reach_the_note() {
        assert!(ledger_lines(-1).is_empty());
        assert_eq!(ledger_lines(-2), [-2], "middle C");
        assert_eq!(ledger_lines(-3), [-2]);
        assert_eq!(ledger_lines(-6), [-2, -4, -6]);
        assert!(ledger_lines(9).is_empty());
        assert_eq!(ledger_lines(10), [10]);
        assert_eq!(ledger_lines(13), [10, 12]);
        assert!(ledger_lines(4).is_empty());
    }

    #[test]
    fn key_signatures_sit_where_engravers_put_them() {
        assert_eq!(
            Clef::Treble.key_positions(KeySig(2)),
            [(8, 1), (5, 1)],
            "F# on the top line, C# in the third space"
        );
        assert_eq!(
            Clef::Treble.key_positions(KeySig(-3)),
            [(4, -1), (7, -1), (3, -1)]
        );
        assert_eq!(
            Clef::Bass.key_positions(KeySig(1)),
            [(6, 1)],
            "F#3 on the F line"
        );
        assert_eq!(Clef::Bass.key_positions(KeySig(-7)).last(), Some(&(-1, -1)));
        assert!(Clef::Bass.key_positions(KeySig::C).is_empty());
        // Every one of them names the letter it alters.
        for clef in [Clef::Treble, Clef::Bass] {
            for n in -7i8..=7 {
                let key = KeySig(n);
                let letters: Vec<u8> = clef
                    .key_positions(key)
                    .iter()
                    .map(|(p, _)| clef.note_at(*p).letter)
                    .collect();
                assert_eq!(letters, key.letters());
            }
        }
    }

    #[test]
    fn accidentals_last_until_the_bar_line() {
        let mut acc = Accidentals::new(KeySig(-3));
        assert_eq!(acc.write(Spelled::new(2, -1, 4)), None, "Eb is in the key");
        assert_eq!(acc.write(Spelled::new(5, 0, 4)), Some(0), "A natural");
        assert_eq!(acc.write(Spelled::new(5, 0, 4)), None, "still natural");
        assert_eq!(
            acc.write(Spelled::new(5, 0, 5)),
            Some(0),
            "other octaves need their own"
        );
        assert_eq!(
            acc.write(Spelled::new(5, -1, 4)),
            Some(-1),
            "back to flat: written"
        );
        acc.bar();
        assert_eq!(acc.write(Spelled::new(5, -1, 4)), None);
        assert_eq!(acc.write(Spelled::new(3, 1, 4)), Some(1));
        assert_eq!(acc.write(Spelled::new(3, 2, 4)), Some(2));
    }

    #[test]
    fn stems_and_seconds() {
        assert!(stem_up(&[0]));
        assert!(!stem_up(&[4]), "the middle line goes down");
        assert!(!stem_up(&[7]));
        assert!(stem_up(&[-2, 2, 6]), "further below than above");
        // C D E with stem up: D moves right.
        assert_eq!(head_offsets(&[-2, -1, 0], true), [0, 1, 0]);
        // Bb C E G (C7, 3rd inversion) on the treble staff, stem up.
        assert_eq!(head_offsets(&[-3, -2, 0, 2], true), [0, 1, 0, 0]);
        // Stem down: the lower of a second moves left.
        assert_eq!(head_offsets(&[6, 7], false), [-1, 0]);
        assert_eq!(head_offsets(&[2, 4, 6], true), [0, 0, 0]);
    }

    #[test]
    fn crowded_accidentals_step_left() {
        assert_eq!(accidental_columns(&[0, 8]), [0, 0], "an octave apart share");
        assert_eq!(accidental_columns(&[4, 2]), [0, 1]);
        assert_eq!(accidental_columns(&[0, 2, 4]), [1, 2, 0]);
        assert_eq!(accidental_columns(&[1, 7, 4]), [0, 0, 1]);
    }
}
