//! Which string plays which note, chosen the way a guitarist would: stay in
//! the hand's position, use open strings in the first few frets, give each
//! note of a chord its own string with the low notes on the low strings, and
//! re-pick the same string for a repeated note.
//!
//! A note's string decides its timbre (a wound low string fretted high
//! sounds nothing like the open string above it), which notes can ring
//! together, and which ones cut each other off, so it matters as much as the
//! string model itself.

/// Strings on the neck, lowest first.
pub const STRINGS: usize = 6;

/// Open-string notes in standard tuning, low E (E2) to high E (E4).
pub const OPEN_NOTES: [u8; STRINGS] = [40, 45, 50, 55, 59, 64];

/// Highest fret on the neck.
pub const MAX_FRET: i32 = 24;

/// Frets the hand reaches above its index finger without moving.
const HAND_SPAN: i32 = 3;

/// Up to this fret the hand counts as being in an open position, where open
/// strings are the easiest notes to play.
const OPEN_POSITION: i32 = 4;

/// Costs, in "frets the hand would have to move". Only their order matters:
/// a chord never puts two notes on one string while another is free, a held
/// note is cut only if nothing else can play the new one, and a note fretted
/// a long way from the hand loses to one close by.
const OPEN_STRING_NEAR: f32 = 0.1;
const OPEN_STRING_FAR: f32 = 2.5;
const PER_FRET: f32 = 0.04;
const OFF_THE_NECK: f32 = 3.0;
const TAKEN_BY_CHORD: f32 = 100.0;
const CUTS_HELD_NOTE: f32 = 20.0;
const CUTS_RINGING_NOTE: f32 = 0.3;
const OUT_OF_ORDER: f32 = 8.0;
const SAME_NOTE_BONUS: f32 = 1.5;

/// What the assignment needs to know about one string.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StringView {
    /// The note it is playing, or about to play.
    pub note: Option<u8>,
    /// Held by a finger, or with a pluck on its way: a new note would cut it.
    pub busy: bool,
    /// Released but still sounding: a new note would cut its tail.
    pub ringing: bool,
}

/// The fret a note is played at on `string`, if it can be played there.
/// Notes below the low E go on the low E string, slackened like a baritone's
/// (negative frets); notes above the 24th fret of the high E go on the high E
/// string, past the end of the neck.
pub fn fret_on(note: u8, string: usize) -> Option<i32> {
    let fret = i32::from(note) - i32::from(OPEN_NOTES[string]);
    let on_neck = (0..=MAX_FRET).contains(&fret);
    let below = string == 0 && fret < 0;
    let above = string == STRINGS - 1 && fret > MAX_FRET;
    (on_neck || below || above).then_some(fret)
}

/// Where a harmonic is played: on `string`, stopped at `fret` (0 for a
/// natural harmonic, on the open string), touched lightly over `touch` so
/// that only `partial` and its multiples ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Harmonic {
    pub string: usize,
    pub fret: i32,
    pub partial: u8,
    pub touch: i32,
}

/// Natural harmonics: (partial, semitones above the open string, fret
/// touched). The 3rd and 5th partials are a couple of cents and 14 cents
/// off the tempered notes they stand for; they sound as nature tunes them.
const NATURAL_HARMONICS: [(u8, i32, i32); 4] = [(2, 12, 12), (3, 19, 7), (4, 24, 5), (5, 28, 4)];

/// Where to play `note` as a harmonic: a natural one on a free open string
/// if one sounds it, otherwise an artificial one (the note an octave down,
/// fretted, touched twelve frets higher) near the hand. Strings marked in
/// `used` are taken by other notes of the same chord; `busy` ones (holding
/// a note) are avoided.
pub fn harmonic(
    note: u8,
    hand: i32,
    used: &[bool; STRINGS],
    busy: &[bool; STRINGS],
) -> Option<Harmonic> {
    let mut best: Option<(f32, Harmonic)> = None;
    let mut consider = |cost: f32, harmonic: Harmonic| {
        let cost = cost + if busy[harmonic.string] { 20.0 } else { 0.0 };
        if !used[harmonic.string] && best.is_none_or(|(c, _)| cost < c) {
            best = Some((cost, harmonic));
        }
    };
    for (rank, &(partial, interval, touch)) in NATURAL_HARMONICS.iter().enumerate() {
        for (string, &open) in OPEN_NOTES.iter().enumerate() {
            if i32::from(open) + interval == i32::from(note) {
                consider(
                    rank as f32,
                    Harmonic {
                        string,
                        fret: 0,
                        partial,
                        touch,
                    },
                );
            }
        }
    }
    for (string, &open) in OPEN_NOTES.iter().enumerate() {
        let fret = i32::from(note) - 12 - i32::from(open);
        if (1..=MAX_FRET - 12).contains(&fret) {
            let reach = (hand - fret).max(fret - (hand + HAND_SPAN)).max(0);
            consider(
                10.0 + reach as f32,
                Harmonic {
                    string,
                    fret,
                    partial: 2,
                    touch: fret + 12,
                },
            );
        }
    }
    best.map(|(_, harmonic)| harmonic)
}

/// The fretting hand and the chord being played.
#[derive(Debug, Clone)]
pub struct Fretboard {
    /// The fret under the index finger. The hand covers it and the
    /// [`HAND_SPAN`] frets above.
    hand: i32,
    /// Strings already given a note of the current chord.
    chord_used: [bool; STRINGS],
    /// The highest of those strings.
    chord_last: Option<usize>,
}

impl Default for Fretboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Fretboard {
    pub fn new() -> Self {
        Self {
            hand: 1,
            chord_used: [false; STRINGS],
            chord_last: None,
        }
    }

    /// Back to the open position with no chord.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The fret under the index finger.
    pub fn hand(&self) -> i32 {
        self.hand
    }

    /// Picks a string for each of `notes` (ascending), which start together,
    /// and writes it to the same index of `out`. `continues_chord` says the
    /// notes belong with the previous call's (they came a moment later, as
    /// from a keyboard), so they avoid its strings too. Moves the hand to
    /// where the notes were played.
    pub fn assign(
        &mut self,
        notes: &[u8],
        strings: &[StringView; STRINGS],
        continues_chord: bool,
        out: &mut [usize],
    ) {
        if !continues_chord {
            self.chord_used = [false; STRINGS];
            self.chord_last = None;
        }
        let mut fretted: Option<(i32, i32)> = None;
        for (index, (&note, slot)) in notes.iter().zip(out.iter_mut()).enumerate() {
            // Notes above this one in the chord still need strings above it.
            let above = notes.len() - 1 - index;
            let mut best = (f32::INFINITY, 0, 0);
            for (string, view) in strings.iter().enumerate() {
                let Some(fret) = fret_on(note, string) else {
                    continue;
                };
                let cost = self.cost(note, string, fret, view, above);
                if cost < best.0 {
                    best = (cost, string, fret);
                }
            }
            let (_, string, fret) = best;
            *slot = string;
            self.chord_used[string] = true;
            self.chord_last = Some(self.chord_last.map_or(string, |last| last.max(string)));
            if (1..=MAX_FRET).contains(&fret) {
                fretted =
                    Some(fretted.map_or((fret, fret), |(lo, hi)| (lo.min(fret), hi.max(fret))));
            }
        }
        if let Some((lowest, highest)) = fretted {
            self.move_hand(lowest, highest);
        }
    }

    /// A legato note fretted at `fret` moves the hand like any other.
    pub fn follow(&mut self, fret: i32) {
        if (1..=MAX_FRET).contains(&fret) {
            self.move_hand(fret, fret);
        }
    }

    fn cost(&self, note: u8, string: usize, fret: i32, view: &StringView, above: usize) -> f32 {
        let mut cost = if fret == 0 {
            if self.hand <= OPEN_POSITION {
                OPEN_STRING_NEAR
            } else {
                OPEN_STRING_FAR
            }
        } else if (1..=MAX_FRET).contains(&fret) {
            let reach = (self.hand - fret)
                .max(fret - (self.hand + HAND_SPAN))
                .max(0);
            reach as f32 + PER_FRET * fret as f32
        } else {
            OFF_THE_NECK
        };
        let same_note = view.note == Some(note);
        if self.chord_used[string] {
            cost += TAKEN_BY_CHORD;
        } else if same_note {
            // Repeating a note: pick the same string again.
            cost -= SAME_NOTE_BONUS;
        } else if view.busy {
            cost += CUTS_HELD_NOTE;
        } else if view.ringing {
            cost += CUTS_RINGING_NOTE;
        }
        if self.chord_last.is_some_and(|last| string <= last) {
            cost += OUT_OF_ORDER;
        }
        if string + above > STRINGS - 1 {
            cost += OUT_OF_ORDER;
        }
        cost
    }

    /// Moves the hand as little as possible to cover frets `lowest..=highest`,
    /// or to the lowest if they span more than a hand.
    fn move_hand(&mut self, lowest: i32, highest: i32) {
        if lowest >= self.hand && highest <= self.hand + HAND_SPAN {
            return;
        }
        self.hand = if highest - lowest > HAND_SPAN || lowest < self.hand {
            lowest
        } else {
            highest - HAND_SPAN
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frets(notes: &[u8], strings: &[usize]) -> [Option<i32>; STRINGS] {
        let mut shape = [None; STRINGS];
        for (&note, &string) in notes.iter().zip(strings) {
            shape[string] = fret_on(note, string);
        }
        shape
    }

    fn chord(board: &mut Fretboard, notes: &[u8]) -> [Option<i32>; STRINGS] {
        let mut out = [0; 16];
        board.assign(notes, &[StringView::default(); STRINGS], false, &mut out);
        frets(notes, &out[..notes.len()])
    }

    #[test]
    fn open_chords_get_their_usual_shapes() {
        let mut board = Fretboard::new();
        assert_eq!(
            chord(&mut board, &[48, 52, 55, 60, 64]),
            [None, Some(3), Some(2), Some(0), Some(1), Some(0)],
            "C major, x32010"
        );
        assert_eq!(
            chord(&mut board, &[43, 47, 50, 55, 59, 67]),
            [Some(3), Some(2), Some(0), Some(0), Some(0), Some(3)],
            "G major, 320003"
        );
        assert_eq!(
            chord(&mut board, &[40, 47, 52, 55, 59, 64]),
            [Some(0), Some(2), Some(2), Some(0), Some(0), Some(0)],
            "E minor, 022000"
        );
    }

    #[test]
    fn six_note_chords_use_every_string_as_a_barre() {
        let mut board = Fretboard::new();
        assert_eq!(
            chord(&mut board, &[45, 52, 57, 61, 64, 69]),
            [Some(5), Some(7), Some(7), Some(6), Some(5), Some(5)],
            "A major barre at the 5th fret"
        );
        assert_eq!(board.hand(), 4, "the hand moved up to cover frets 5 to 7");
    }

    #[test]
    fn melodies_stay_in_position() {
        let mut board = Fretboard::new();
        let mut views = [StringView::default(); STRINGS];
        let mut played = Vec::new();
        // C major scale from C3, one note at a time, each released before the next.
        for note in [48, 50, 52, 53, 55, 57, 59, 60] {
            let mut out = [0];
            board.assign(&[note], &views, false, &mut out);
            let string = out[0];
            played.push((string, fret_on(note, string).unwrap()));
            for view in &mut views {
                view.busy = false;
            }
            views[string] = StringView {
                note: Some(note),
                busy: false,
                ringing: true,
            };
        }
        assert!(
            played.iter().all(|&(_, fret)| fret <= 3),
            "open position: {played:?}"
        );

        // An octave up, the hand moves and stays there.
        let mut out = [0];
        board.assign(&[76], &views, false, &mut out);
        assert_eq!((out[0], board.hand()), (5, 9), "E5 at the 12th fret");
        board.assign(&[74], &views, false, &mut out);
        assert_eq!(
            fret_on(74, out[0]),
            Some(10),
            "D5 nearby, not on a lower string"
        );
    }

    #[test]
    fn a_repeated_note_is_picked_on_the_same_string() {
        let mut board = Fretboard::new();
        let mut views = [StringView::default(); STRINGS];
        views[3] = StringView {
            note: Some(57),
            busy: true,
            ringing: false,
        };
        let mut out = [0];
        board.assign(&[57], &views, false, &mut out);
        assert_eq!(out[0], 3);
    }

    #[test]
    fn a_held_note_is_cut_only_when_nothing_else_can_play() {
        let mut board = Fretboard::new();
        let mut views = [StringView::default(); STRINGS];
        views[0] = StringView {
            note: Some(41),
            busy: true,
            ringing: false,
        };
        let mut out = [0];
        board.assign(&[45], &views, false, &mut out);
        assert_eq!(out[0], 1, "A2 on the open A rather than cutting F2");
        board.assign(&[40], &views, false, &mut out);
        assert_eq!(out[0], 0, "only the low string plays E2, so F2 is cut");
    }

    #[test]
    fn harmonics_are_natural_where_they_can_be() {
        let free = [false; STRINGS];
        // E3 is the low E's 12th-fret harmonic.
        assert_eq!(
            harmonic(52, 1, &free, &free),
            Some(Harmonic {
                string: 0,
                fret: 0,
                partial: 2,
                touch: 12
            })
        );
        assert_eq!(
            harmonic(64, 1, &free, &free).map(|h| (h.string, h.partial, h.touch)),
            Some((1, 3, 7)),
            "E4: the A string's 7th-fret harmonic, the lower partial"
        );
        let mut used = free;
        used[1] = true;
        assert_eq!(
            harmonic(64, 1, &used, &free).map(|h| (h.string, h.partial, h.touch)),
            Some((0, 4, 5)),
            "the A string taken, the low E's 5th-fret harmonic"
        );
        // C5 is no open string's harmonic: an artificial one, C4 fretted
        // and touched an octave up.
        let c5 = harmonic(72, 5, &free, &free).unwrap();
        assert_eq!(c5.partial, 2);
        assert_eq!(i32::from(OPEN_NOTES[c5.string]) + c5.fret + 12, 72);
        assert_eq!(c5.touch, c5.fret + 12);
    }

    #[test]
    fn notes_off_the_neck_go_to_the_outer_strings() {
        assert_eq!(fret_on(30, 0), Some(-10));
        assert_eq!(fret_on(30, 1), None);
        assert_eq!(fret_on(100, 5), Some(36));
        assert_eq!(fret_on(100, 4), None);
        let mut board = Fretboard::new();
        let shape = chord(&mut board, &[28, 100]);
        assert_eq!(shape[0], Some(-12));
        assert_eq!(shape[5], Some(36));
    }

    #[test]
    fn notes_arriving_just_after_a_chord_avoid_its_strings() {
        let mut board = Fretboard::new();
        let views = [StringView::default(); STRINGS];
        let mut out = [0; 2];
        board.assign(&[52, 59], &views, false, &mut out);
        assert_eq!(out, [2, 4], "E3 on the D string, B3 on the open B");
        // Notes a moment later (as from a keyboard) join the chord, even
        // before the strings it took show as busy.
        let mut late = [0];
        board.assign(&[57], &views, true, &mut late);
        assert_eq!(late[0], 3, "A3 on the free G string");
        board.assign(&[50], &views, true, &mut late);
        assert_ne!(late[0], 2, "the open D is taken by the chord's E3");
    }
}
