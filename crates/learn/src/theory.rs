//! The music theory the exercises need: spelled notes, key signatures,
//! scales, chords and scale fingerings. Pure data and functions.

use serde::{Deserialize, Serialize};

pub const LETTERS: [&str; 7] = ["C", "D", "E", "F", "G", "A", "B"];
const LETTER_SEMITONES: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
const BLACK: [bool; 12] = [
    false, true, false, true, false, false, true, false, true, false, true, false,
];

/// True for the black keys of a piano.
pub fn is_black(pitch: u8) -> bool {
    BLACK[usize::from(pitch % 12)]
}

/// Which hand plays something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Hand {
    Left,
    Right,
}

impl Hand {
    pub fn label(self) -> &'static str {
        match self {
            Hand::Left => "Left",
            Hand::Right => "Right",
        }
    }

    pub fn other(self) -> Hand {
        match self {
            Hand::Left => Hand::Right,
            Hand::Right => Hand::Left,
        }
    }
}

fn accidental_text(accidental: i8) -> &'static str {
    match accidental {
        -2 => "bb",
        -1 => "b",
        1 => "#",
        2 => "##",
        _ => "",
    }
}

/// A note name without an octave: a letter (0 = C … 6 = B) and an
/// accidental in semitones (-2 double flat … 2 double sharp).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NoteName {
    pub letter: u8,
    pub accidental: i8,
}

impl NoteName {
    pub const C: NoteName = NoteName::new(0, 0);

    pub const fn new(letter: u8, accidental: i8) -> NoteName {
        NoteName {
            letter: letter % 7,
            accidental,
        }
    }

    /// The roots the scale and chord pickers offer, in keyboard order.
    pub const ROOTS: [NoteName; 17] = [
        NoteName::new(0, 0),
        NoteName::new(0, 1),
        NoteName::new(1, -1),
        NoteName::new(1, 0),
        NoteName::new(1, 1),
        NoteName::new(2, -1),
        NoteName::new(2, 0),
        NoteName::new(3, 0),
        NoteName::new(3, 1),
        NoteName::new(4, -1),
        NoteName::new(4, 0),
        NoteName::new(4, 1),
        NoteName::new(5, -1),
        NoteName::new(5, 0),
        NoteName::new(5, 1),
        NoteName::new(6, -1),
        NoteName::new(6, 0),
    ];

    pub fn pitch_class(self) -> u8 {
        (LETTER_SEMITONES[usize::from(self.letter)] + i32::from(self.accidental)).rem_euclid(12)
            as u8
    }

    /// Place on the circle of fifths: C 0, G 1, F -1, F♯ 6, B♭ -2.
    pub fn fifths(self) -> i32 {
        const LETTER_FIFTHS: [i32; 7] = [0, 2, 4, -1, 1, 3, 5];
        LETTER_FIFTHS[usize::from(self.letter)] + 7 * i32::from(self.accidental)
    }

    /// The name with `#` and `b` for accidentals: "F#", "Bb", "F##".
    pub fn label(self) -> String {
        format!(
            "{}{}",
            LETTERS[usize::from(self.letter)],
            accidental_text(self.accidental)
        )
    }

    pub fn at(self, octave: i8) -> Spelled {
        Spelled {
            letter: self.letter,
            accidental: self.accidental,
            octave,
        }
    }
}

/// A note as written: letter, accidental and octave (scientific pitch
/// notation, so middle C is C4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Spelled {
    pub letter: u8,
    pub accidental: i8,
    pub octave: i8,
}

impl Spelled {
    pub const MIDDLE_C: Spelled = Spelled::new(0, 0, 4);

    pub const fn new(letter: u8, accidental: i8, octave: i8) -> Spelled {
        Spelled {
            letter: letter % 7,
            accidental,
            octave,
        }
    }

    pub fn name(self) -> NoteName {
        NoteName::new(self.letter, self.accidental)
    }

    /// The MIDI note number; may fall outside 0..=127 for silly spellings.
    pub fn midi(self) -> i32 {
        12 * (i32::from(self.octave) + 1)
            + LETTER_SEMITONES[usize::from(self.letter)]
            + i32::from(self.accidental)
    }

    pub fn pitch(self) -> Option<u8> {
        u8::try_from(self.midi()).ok().filter(|p| *p <= 127)
    }

    /// Diatonic steps from C-1: one per letter, whatever the accidental.
    /// What decides the line or space a note sits on.
    pub fn step(self) -> i32 {
        i32::from(self.octave) * 7 + i32::from(self.letter)
    }

    pub fn from_step(step: i32, accidental: i8) -> Spelled {
        Spelled {
            letter: step.rem_euclid(7) as u8,
            accidental,
            octave: step.div_euclid(7) as i8,
        }
    }

    /// "C#4", "Bb3".
    pub fn label(self) -> String {
        format!("{}{}", self.name().label(), self.octave)
    }

    /// The note `semitones` and `letters` above this one, spelled on the
    /// letter that lands on. How scales and chords are spelled.
    pub fn above(self, semitones: i32, letters: i32) -> Spelled {
        let natural = Spelled::from_step(self.step() + letters, 0);
        let accidental = self.midi() + semitones - natural.midi();
        if (-2..=2).contains(&accidental) {
            Spelled {
                accidental: accidental as i8,
                ..natural
            }
        } else {
            spell((self.midi() + semitones).clamp(0, 127) as u8, KeySig::C)
        }
    }
}

/// How a pitch is written in `key`: as the key's own note if it is one,
/// otherwise with sharps in sharp keys (and C) and flats in flat keys.
pub fn spell(pitch: u8, key: KeySig) -> Spelled {
    let octave = i32::from(pitch) / 12 - 1;
    let class = i32::from(pitch % 12);
    // The key's own notes first: E♯ in F♯ major, C♭ in G♭ major.
    for letter in 0..7u8 {
        let accidental = key.accidental(letter);
        let semis = LETTER_SEMITONES[usize::from(letter)] + i32::from(accidental);
        if semis.rem_euclid(12) == class {
            // B♯ belongs to the octave below, C♭ to the one above.
            let octave = octave + (class - semis).div_euclid(12);
            return Spelled::new(letter, accidental, octave as i8);
        }
    }
    let (letter, accidental) = if key.0 < 0 {
        [
            (0, 0),
            (1, -1),
            (1, 0),
            (2, -1),
            (2, 0),
            (3, 0),
            (4, -1),
            (4, 0),
            (5, -1),
            (5, 0),
            (6, -1),
            (6, 0),
        ][class as usize]
    } else {
        [
            (0, 0),
            (0, 1),
            (1, 0),
            (1, 1),
            (2, 0),
            (3, 0),
            (3, 1),
            (4, 0),
            (4, 1),
            (5, 0),
            (5, 1),
            (6, 0),
        ][class as usize]
    };
    Spelled::new(letter, accidental, octave as i8)
}

/// A pitch's usual name, with sharps: "C#4".
pub fn pitch_label(pitch: u8) -> String {
    spell(pitch, KeySig::C).label()
}

/// Pitch class names as chord symbols use them.
const CLASS_NAMES: [&str; 12] = [
    "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

/// A key signature as a count of sharps (positive) or flats (negative).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct KeySig(pub i8);

/// The order sharps are added to a key signature: F C G D A E B.
pub const SHARP_ORDER: [u8; 7] = [3, 0, 4, 1, 5, 2, 6];
/// The order flats are added: B E A D G C F.
pub const FLAT_ORDER: [u8; 7] = [6, 2, 5, 1, 4, 0, 3];

impl KeySig {
    pub const C: KeySig = KeySig(0);

    /// What this signature does to every `letter`: +1, -1 or 0.
    pub fn accidental(self, letter: u8) -> i8 {
        let n = usize::from(self.0.unsigned_abs()).min(7);
        if self.0 > 0 && SHARP_ORDER[..n].contains(&letter) {
            1
        } else if self.0 < 0 && FLAT_ORDER[..n].contains(&letter) {
            -1
        } else {
            0
        }
    }

    /// The letters it alters, in the order they're written.
    pub fn letters(self) -> &'static [u8] {
        let n = usize::from(self.0.unsigned_abs()).min(7);
        if self.0 >= 0 {
            &SHARP_ORDER[..n]
        } else {
            &FLAT_ORDER[..n]
        }
    }

    /// For a key with this many fifths, if it has a signature.
    pub fn from_fifths(fifths: i32) -> Option<KeySig> {
        (-7..=7).contains(&fifths).then_some(KeySig(fifths as i8))
    }

    pub fn major_tonic(self) -> NoteName {
        tonic_from_fifths(i32::from(self.0))
    }

    pub fn minor_tonic(self) -> NoteName {
        tonic_from_fifths(i32::from(self.0) + 3)
    }

    /// "D major", "Bb major".
    pub fn label(self) -> String {
        format!("{} major", self.major_tonic().label())
    }

    /// "2#", "3b", or "no sharps or flats".
    pub fn count_label(self) -> String {
        match self.0 {
            0 => "no sharps or flats".to_owned(),
            n if n > 0 => format!("{n}#"),
            n => format!("{}b", -n),
        }
    }
}

fn tonic_from_fifths(fifths: i32) -> NoteName {
    // F C G D A E B, as letters.
    const LETTERS_BY_FIFTH: [u8; 7] = [3, 0, 4, 1, 5, 2, 6];
    let index = fifths + 1;
    NoteName::new(
        LETTERS_BY_FIFTH[index.rem_euclid(7) as usize],
        index.div_euclid(7) as i8,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScaleKind {
    Major,
    NaturalMinor,
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

impl ScaleKind {
    pub const ALL: [ScaleKind; 12] = [
        ScaleKind::Major,
        ScaleKind::NaturalMinor,
        ScaleKind::HarmonicMinor,
        ScaleKind::MelodicMinor,
        ScaleKind::Dorian,
        ScaleKind::Phrygian,
        ScaleKind::Lydian,
        ScaleKind::Mixolydian,
        ScaleKind::Locrian,
        ScaleKind::MajorPentatonic,
        ScaleKind::MinorPentatonic,
        ScaleKind::Blues,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ScaleKind::Major => "major",
            ScaleKind::NaturalMinor => "natural minor",
            ScaleKind::HarmonicMinor => "harmonic minor",
            ScaleKind::MelodicMinor => "melodic minor",
            ScaleKind::Dorian => "dorian",
            ScaleKind::Phrygian => "phrygian",
            ScaleKind::Lydian => "lydian",
            ScaleKind::Mixolydian => "mixolydian",
            ScaleKind::Locrian => "locrian",
            ScaleKind::MajorPentatonic => "major pentatonic",
            ScaleKind::MinorPentatonic => "minor pentatonic",
            ScaleKind::Blues => "blues",
        }
    }

    /// Each degree going up, as (semitones, letters) above the root.
    pub fn degrees(self) -> &'static [(i32, i32)] {
        match self {
            ScaleKind::Major => &[(0, 0), (2, 1), (4, 2), (5, 3), (7, 4), (9, 5), (11, 6)],
            ScaleKind::NaturalMinor => &[(0, 0), (2, 1), (3, 2), (5, 3), (7, 4), (8, 5), (10, 6)],
            ScaleKind::HarmonicMinor => &[(0, 0), (2, 1), (3, 2), (5, 3), (7, 4), (8, 5), (11, 6)],
            ScaleKind::MelodicMinor => &[(0, 0), (2, 1), (3, 2), (5, 3), (7, 4), (9, 5), (11, 6)],
            ScaleKind::Dorian => &[(0, 0), (2, 1), (3, 2), (5, 3), (7, 4), (9, 5), (10, 6)],
            ScaleKind::Phrygian => &[(0, 0), (1, 1), (3, 2), (5, 3), (7, 4), (8, 5), (10, 6)],
            ScaleKind::Lydian => &[(0, 0), (2, 1), (4, 2), (6, 3), (7, 4), (9, 5), (11, 6)],
            ScaleKind::Mixolydian => &[(0, 0), (2, 1), (4, 2), (5, 3), (7, 4), (9, 5), (10, 6)],
            ScaleKind::Locrian => &[(0, 0), (1, 1), (3, 2), (5, 3), (6, 4), (8, 5), (10, 6)],
            ScaleKind::MajorPentatonic => &[(0, 0), (2, 1), (4, 2), (7, 4), (9, 5)],
            ScaleKind::MinorPentatonic => &[(0, 0), (3, 2), (5, 3), (7, 4), (10, 6)],
            ScaleKind::Blues => &[(0, 0), (3, 2), (5, 3), (6, 4), (7, 4), (10, 6)],
        }
    }

    /// Going down, where it differs: the melodic minor comes down natural.
    pub fn descending_degrees(self) -> &'static [(i32, i32)] {
        match self {
            ScaleKind::MelodicMinor => ScaleKind::NaturalMinor.degrees(),
            other => other.degrees(),
        }
    }

    /// Where the key signature sits on the circle of fifths relative to the
    /// root: a dorian scale is written in the major key a tone below.
    fn key_offset(self) -> i32 {
        match self {
            ScaleKind::Major | ScaleKind::MajorPentatonic => 0,
            ScaleKind::NaturalMinor
            | ScaleKind::HarmonicMinor
            | ScaleKind::MelodicMinor
            | ScaleKind::MinorPentatonic
            | ScaleKind::Blues => -3,
            ScaleKind::Dorian => -2,
            ScaleKind::Phrygian => -4,
            ScaleKind::Lydian => 1,
            ScaleKind::Mixolydian => -1,
            ScaleKind::Locrian => -5,
        }
    }

    /// Seven notes to the octave, so the usual fingerings apply.
    pub fn is_heptatonic(self) -> bool {
        self.degrees().len() == 7
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Scale {
    pub root: NoteName,
    pub kind: ScaleKind,
}

impl Scale {
    pub fn new(root: NoteName, kind: ScaleKind) -> Scale {
        Scale { root, kind }
    }

    /// "C# harmonic minor".
    pub fn name(self) -> String {
        format!("{} {}", self.root.label(), self.kind.name())
    }

    /// The key signature it's written with, unless that would need more
    /// than seven sharps or flats (G♯ major): then accidentals are written
    /// on the notes.
    pub fn key(self) -> Option<KeySig> {
        KeySig::from_fifths(self.root.fifths() + self.kind.key_offset())
    }

    /// The scale from the root in `octave`, up `octaves` octaves to the
    /// root again, and back down if `down`.
    pub fn notes(self, octave: i8, octaves: u8, down: bool) -> Vec<Spelled> {
        let root = self.root.at(octave);
        let octaves = i32::from(octaves.max(1));
        let mut notes = Vec::new();
        for o in 0..octaves {
            for &(semis, letters) in self.kind.degrees() {
                notes.push(root.above(semis + 12 * o, letters + 7 * o));
            }
        }
        notes.push(root.above(12 * octaves, 7 * octaves));
        if down {
            for o in (0..octaves).rev() {
                for &(semis, letters) in self.kind.descending_degrees().iter().rev() {
                    notes.push(root.above(semis + 12 * o, letters + 7 * o));
                }
            }
        }
        notes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChordKind {
    Major,
    Minor,
    Diminished,
    Augmented,
    Sus2,
    Sus4,
    Dominant7,
    Major7,
    Minor7,
    HalfDiminished7,
    Diminished7,
}

impl ChordKind {
    pub const ALL: [ChordKind; 11] = [
        ChordKind::Major,
        ChordKind::Minor,
        ChordKind::Diminished,
        ChordKind::Augmented,
        ChordKind::Sus2,
        ChordKind::Sus4,
        ChordKind::Dominant7,
        ChordKind::Major7,
        ChordKind::Minor7,
        ChordKind::HalfDiminished7,
        ChordKind::Diminished7,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ChordKind::Major => "major",
            ChordKind::Minor => "minor",
            ChordKind::Diminished => "diminished",
            ChordKind::Augmented => "augmented",
            ChordKind::Sus2 => "sus2",
            ChordKind::Sus4 => "sus4",
            ChordKind::Dominant7 => "dominant 7th",
            ChordKind::Major7 => "major 7th",
            ChordKind::Minor7 => "minor 7th",
            ChordKind::HalfDiminished7 => "half-diminished 7th",
            ChordKind::Diminished7 => "diminished 7th",
        }
    }

    /// As written after the root in a chord symbol: "m7" in "Am7".
    pub fn symbol(self) -> &'static str {
        match self {
            ChordKind::Major => "",
            ChordKind::Minor => "m",
            ChordKind::Diminished => "dim",
            ChordKind::Augmented => "aug",
            ChordKind::Sus2 => "sus2",
            ChordKind::Sus4 => "sus4",
            ChordKind::Dominant7 => "7",
            ChordKind::Major7 => "maj7",
            ChordKind::Minor7 => "m7",
            ChordKind::HalfDiminished7 => "m7b5",
            ChordKind::Diminished7 => "dim7",
        }
    }

    /// Each tone from the root up, as (semitones, letters) above it.
    pub fn tones(self) -> &'static [(i32, i32)] {
        match self {
            ChordKind::Major => &[(0, 0), (4, 2), (7, 4)],
            ChordKind::Minor => &[(0, 0), (3, 2), (7, 4)],
            ChordKind::Diminished => &[(0, 0), (3, 2), (6, 4)],
            ChordKind::Augmented => &[(0, 0), (4, 2), (8, 4)],
            ChordKind::Sus2 => &[(0, 0), (2, 1), (7, 4)],
            ChordKind::Sus4 => &[(0, 0), (5, 3), (7, 4)],
            ChordKind::Dominant7 => &[(0, 0), (4, 2), (7, 4), (10, 6)],
            ChordKind::Major7 => &[(0, 0), (4, 2), (7, 4), (11, 6)],
            ChordKind::Minor7 => &[(0, 0), (3, 2), (7, 4), (10, 6)],
            ChordKind::HalfDiminished7 => &[(0, 0), (3, 2), (6, 4), (10, 6)],
            ChordKind::Diminished7 => &[(0, 0), (3, 2), (6, 4), (9, 6)],
        }
    }

    /// Inversions it has besides root position.
    pub fn inversions(self) -> u8 {
        self.tones().len() as u8 - 1
    }
}

pub fn inversion_name(inversion: u8) -> &'static str {
    match inversion {
        0 => "root position",
        1 => "1st inversion",
        2 => "2nd inversion",
        _ => "3rd inversion",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Chord {
    pub root: NoteName,
    pub kind: ChordKind,
    pub inversion: u8,
}

impl Chord {
    pub fn new(root: NoteName, kind: ChordKind, inversion: u8) -> Chord {
        Chord {
            root,
            kind,
            inversion: inversion.min(kind.inversions()),
        }
    }

    /// The chord built on the root in `octave`, lowest note first: in an
    /// inversion the lowest tones go up an octave.
    pub fn notes(self, octave: i8) -> Vec<Spelled> {
        let root = self.root.at(octave);
        let tones = self.kind.tones();
        let k = usize::from(self.inversion);
        tones[k..]
            .iter()
            .map(|&(s, l)| root.above(s, l))
            .chain(tones[..k].iter().map(|&(s, l)| root.above(s + 12, l + 7)))
            .collect()
    }

    /// "Am7", or "Am7/C" in an inversion.
    pub fn symbol(self) -> String {
        let mut symbol = format!("{}{}", self.root.label(), self.kind.symbol());
        if self.inversion > 0 {
            let bass = self.notes(4)[0];
            symbol.push('/');
            symbol.push_str(&bass.name().label());
        }
        symbol
    }

    /// "C major, 1st inversion".
    pub fn name(self) -> String {
        format!(
            "{} {}, {}",
            self.root.label(),
            self.kind.name(),
            inversion_name(self.inversion)
        )
    }
}

/// A chord recognized from keys held down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recognized {
    pub root: u8,
    pub kind: ChordKind,
    pub inversion: u8,
}

impl Recognized {
    /// "Cmaj7", or "C/E" when the root isn't in the bass.
    pub fn symbol(self) -> String {
        let root = CLASS_NAMES[usize::from(self.root % 12)];
        let mut symbol = format!("{root}{}", self.kind.symbol());
        if self.inversion > 0 {
            let bass = (i32::from(self.root) + self.kind.tones()[usize::from(self.inversion)].0)
                .rem_euclid(12);
            symbol.push('/');
            symbol.push_str(CLASS_NAMES[bass as usize]);
        }
        symbol
    }
}

/// Names the chord `pitches` make, if they make one this knows: every
/// pitch class must belong to it and none of its tones be missing.
/// Prefers the reading with its root in the bass.
pub fn recognize(pitches: &[u8]) -> Option<Recognized> {
    let bass = *pitches.iter().min()?;
    let mut classes: Vec<i32> = pitches.iter().map(|p| i32::from(p % 12)).collect();
    classes.sort_unstable();
    classes.dedup();
    if classes.len() < 3 {
        return None;
    }
    let mut found = None;
    // The bass first, so root position wins over a symmetric reading.
    let bass_class = i32::from(bass % 12);
    let order =
        std::iter::once(bass_class).chain(classes.iter().copied().filter(|c| *c != bass_class));
    for root in order {
        let mut intervals: Vec<i32> = classes.iter().map(|c| (c - root).rem_euclid(12)).collect();
        intervals.sort_unstable();
        for kind in ChordKind::ALL {
            let mut tones: Vec<i32> = kind.tones().iter().map(|t| t.0 % 12).collect();
            tones.sort_unstable();
            if tones == intervals {
                let inversion = kind
                    .tones()
                    .iter()
                    .position(|t| (root + t.0).rem_euclid(12) == bass_class)
                    .unwrap_or(0) as u8;
                found.get_or_insert(Recognized {
                    root: root as u8,
                    kind,
                    inversion,
                });
            }
        }
        if found.is_some() {
            break;
        }
    }
    found
}

/// The usual fingering for a seven-note scale played up over whole octaves
/// (`notes` as [`Scale::notes`] gives them, without the way down): the thumb
/// never on a black key, crossing after black keys, the thumb on the tonic
/// when it's white. `None` for scales without a standard fingering.
pub fn scale_fingering(notes: &[Spelled], hand: Hand) -> Option<Vec<u8>> {
    if notes.len() < 8 || !(notes.len() - 1).is_multiple_of(7) {
        return None;
    }
    let black = |degree: usize| notes[degree].pitch().is_some_and(is_black);
    let white = |degree: usize| !black(degree);
    let mut best: Option<((usize, usize), i32)> = None;
    for a in 0..7 {
        for gap in [3, 4] {
            let b = (a + gap) % 7;
            if !white(a) || !white(b) {
                continue;
            }
            let score = if white(0) {
                let (wanted, fallback) = match hand {
                    Hand::Right => (3, 4),
                    Hand::Left => (4, 3),
                };
                let other = if a == 0 { b } else { a };
                let has_tonic = a == 0 || b == 0;
                i32::from(has_tonic) * 10
                    + i32::from(has_tonic && other == wanted) * 2
                    + i32::from(has_tonic && other == fallback)
            } else {
                // Right hand: the thumb goes under after a black key. Left
                // hand: a finger crosses over the thumb onto a black key.
                [a, b]
                    .iter()
                    .map(|&t| match hand {
                        Hand::Right => i32::from(black((t + 6) % 7)) * 2,
                        Hand::Left => i32::from(black((t + 1) % 7)) * 2,
                    })
                    .sum()
            };
            if best.is_none_or(|(_, s)| score > s) {
                best = Some(((a, b), score));
            }
        }
    }
    let ((a, b), _) = best?;
    let thumb = |degree: usize| degree == a || degree == b;
    let last = notes.len() - 1;
    let fingers = (0..notes.len())
        .map(|i| {
            let degree = i % 7;
            let finger = match hand {
                // Counting up from the last thumb.
                Hand::Right => (0..7).find(|k| thumb((degree + 7 - k) % 7)).map(|k| k + 1),
                // Counting down to the next thumb.
                Hand::Left => (0..7).find(|k| thumb((degree + k) % 7)).map(|k| k + 1),
            }
            .unwrap_or(1) as u8;
            match hand {
                // The top note ends the run on the next finger, not the thumb.
                Hand::Right if i == last && finger == 1 && i > 0 => {
                    let below = (0..7)
                        .find(|k| thumb((degree + 6 + 7 - k) % 7))
                        .unwrap_or(0)
                        + 1;
                    below as u8 + 1
                }
                // The bottom note starts on whichever finger reaches the next thumb.
                Hand::Left if i == 0 && finger == 1 => {
                    (1..8).find(|k| thumb((degree + k) % 7)).unwrap_or(4) as u8 + 1
                }
                _ => finger,
            }
        })
        .collect();
    Some(fingers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(notes: &[Spelled]) -> Vec<String> {
        notes.iter().map(|n| n.name().label()).collect()
    }

    fn root(label: &str) -> NoteName {
        *NoteName::ROOTS
            .iter()
            .find(|r| r.label() == label)
            .unwrap_or_else(|| panic!("no root {label}"))
    }

    #[test]
    fn spelled_notes_and_midi() {
        assert_eq!(Spelled::MIDDLE_C.midi(), 60);
        assert_eq!(Spelled::new(5, 0, 4).midi(), 69);
        assert_eq!(Spelled::new(6, 1, 3).midi(), 60, "B#3 is middle C");
        assert_eq!(Spelled::new(0, -1, 4).midi(), 59, "Cb4 is B3");
        assert_eq!(Spelled::new(0, 0, -1).midi(), 0);
        assert_eq!(Spelled::new(6, 1, 3).step(), Spelled::new(6, 0, 3).step());
        assert_eq!(
            Spelled::from_step(Spelled::MIDDLE_C.step() - 1, 0).label(),
            "B3"
        );
        assert_eq!(Spelled::new(3, 1, 4).label(), "F#4");
        assert_eq!(Spelled::new(6, -2, 2).label(), "Bbb2");
    }

    #[test]
    fn spelling_follows_the_key() {
        assert_eq!(spell(61, KeySig::C).label(), "C#4");
        assert_eq!(spell(61, KeySig(-2)).label(), "Db4");
        assert_eq!(spell(65, KeySig(6)).label(), "E#4", "F# major's own E#");
        assert_eq!(
            spell(59, KeySig(-6)).label(),
            "Cb4",
            "Gb major's Cb is above B3"
        );
        assert_eq!(
            spell(60, KeySig(7)).label(),
            "B#3",
            "C# major's B# belongs below"
        );
        assert_eq!(pitch_label(21), "A0");
        assert_eq!(pitch_label(108), "C8");
    }

    #[test]
    fn key_signatures() {
        let d = KeySig(2);
        assert_eq!(d.accidental(3), 1, "F#");
        assert_eq!(d.accidental(0), 1, "C#");
        assert_eq!(d.accidental(4), 0);
        let e_flat = KeySig(-3);
        assert_eq!(e_flat.letters(), &[6, 2, 5]);
        assert_eq!(e_flat.major_tonic().label(), "Eb");
        assert_eq!(e_flat.minor_tonic().label(), "C");
        assert_eq!(KeySig(6).major_tonic().label(), "F#");
        assert_eq!(KeySig(-7).major_tonic().label(), "Cb");
        assert_eq!(KeySig(7).minor_tonic().label(), "A#");
        assert_eq!(KeySig(0).label(), "C major");
        assert_eq!(KeySig(-1).count_label(), "1b");
        for f in -7..=7 {
            assert_eq!(KeySig(f).major_tonic().fifths(), i32::from(f));
        }
    }

    #[test]
    fn scales_are_spelled_one_letter_per_degree() {
        let c = Scale::new(NoteName::C, ScaleKind::Major);
        assert_eq!(
            names(&c.notes(4, 1, false)),
            ["C", "D", "E", "F", "G", "A", "B", "C"]
        );
        let g_sharp = Scale::new(root("G#"), ScaleKind::HarmonicMinor);
        assert_eq!(
            names(&g_sharp.notes(4, 1, false)),
            ["G#", "A#", "B", "C#", "D#", "E", "F##", "G#"]
        );
        let melodic = Scale::new(root("A"), ScaleKind::MelodicMinor).notes(4, 1, true);
        assert_eq!(
            names(&melodic),
            [
                "A", "B", "C", "D", "E", "F#", "G#", "A", "G", "F", "E", "D", "C", "B", "A"
            ],
            "up melodic, down natural"
        );
        let blues = Scale::new(NoteName::C, ScaleKind::Blues).notes(4, 1, false);
        assert_eq!(names(&blues), ["C", "Eb", "F", "Gb", "G", "Bb", "C"]);
        let pent = Scale::new(root("A"), ScaleKind::MinorPentatonic).notes(3, 2, false);
        assert_eq!(pent.len(), 11);
        assert_eq!(pent[10].midi(), 57 + 24);
        let dorian = Scale::new(root("D"), ScaleKind::Dorian);
        assert_eq!(dorian.key(), Some(KeySig::C));
        assert_eq!(
            names(&dorian.notes(4, 1, false)),
            ["D", "E", "F", "G", "A", "B", "C", "D"]
        );
        assert_eq!(Scale::new(root("G#"), ScaleKind::Major).key(), None);
        assert_eq!(
            Scale::new(root("C"), ScaleKind::NaturalMinor).key(),
            Some(KeySig(-3))
        );
        assert_eq!(Scale::new(root("Db"), ScaleKind::Locrian).key(), None);
        // Every scale from every root rises one octave in total.
        for r in NoteName::ROOTS {
            for kind in ScaleKind::ALL {
                let notes = Scale::new(r, kind).notes(4, 1, true);
                assert_eq!(notes.last().unwrap().midi(), notes[0].midi());
                let top = &notes[kind.degrees().len()];
                assert_eq!(
                    top.midi() - notes[0].midi(),
                    12,
                    "{}",
                    Scale::new(r, kind).name()
                );
                assert!(
                    notes
                        .windows(2)
                        .take(kind.degrees().len())
                        .all(|w| w[1].midi() > w[0].midi())
                );
            }
        }
    }

    #[test]
    fn chords_and_inversions() {
        let c = Chord::new(NoteName::C, ChordKind::Major, 0);
        assert_eq!(names(&c.notes(4)), ["C", "E", "G"]);
        let first = Chord::new(NoteName::C, ChordKind::Major, 1);
        assert_eq!(names(&first.notes(4)), ["E", "G", "C"]);
        assert_eq!(first.notes(4)[2].octave, 5);
        assert_eq!(first.symbol(), "C/E");
        let b_dim7 = Chord::new(root("B"), ChordKind::Diminished7, 0);
        assert_eq!(names(&b_dim7.notes(3)), ["B", "D", "F", "Ab"]);
        let c_dim7 = Chord::new(NoteName::C, ChordKind::Diminished7, 0);
        assert_eq!(names(&c_dim7.notes(3)), ["C", "Eb", "Gb", "Bbb"]);
        let third = Chord::new(root("G"), ChordKind::Dominant7, 3);
        assert_eq!(names(&third.notes(3)), ["F", "G", "B", "D"]);
        assert_eq!(third.name(), "G dominant 7th, 3rd inversion");
        assert_eq!(Chord::new(NoteName::C, ChordKind::Major, 5).inversion, 2);
    }

    #[test]
    fn recognizes_chords_from_keys() {
        let sym = |p: &[u8]| recognize(p).map(Recognized::symbol);
        assert_eq!(sym(&[60, 64, 67]).as_deref(), Some("C"));
        assert_eq!(sym(&[64, 67, 72]).as_deref(), Some("C/E"));
        assert_eq!(sym(&[57, 60, 64, 67]).as_deref(), Some("Am7"));
        assert_eq!(sym(&[43, 59, 62, 65]).as_deref(), Some("G7"));
        assert_eq!(sym(&[60, 63, 66, 69]).as_deref(), Some("Cdim7"));
        assert_eq!(
            sym(&[48, 60, 64, 67, 72]).as_deref(),
            Some("C"),
            "doubled notes"
        );
        assert_eq!(sym(&[60, 62]), None);
        assert_eq!(sym(&[60, 61, 62]), None);
    }

    #[test]
    fn standard_scale_fingerings() {
        let fingering = |r: &str, kind, hand| {
            let notes = Scale::new(root(r), kind).notes(4, 1, false);
            scale_fingering(&notes, hand).unwrap()
        };
        use Hand::{Left as L, Right as R};
        use ScaleKind::{HarmonicMinor as Hm, Major as M};
        assert_eq!(fingering("C", M, R), [1, 2, 3, 1, 2, 3, 4, 5]);
        assert_eq!(fingering("C", M, L), [5, 4, 3, 2, 1, 3, 2, 1]);
        assert_eq!(fingering("G", M, R), [1, 2, 3, 1, 2, 3, 4, 5]);
        assert_eq!(fingering("E", M, R), [1, 2, 3, 1, 2, 3, 4, 5]);
        assert_eq!(fingering("E", M, L), [5, 4, 3, 2, 1, 3, 2, 1]);
        assert_eq!(fingering("D", M, L), [5, 4, 3, 2, 1, 3, 2, 1]);
        assert_eq!(fingering("A", M, L), [5, 4, 3, 2, 1, 3, 2, 1]);
        assert_eq!(fingering("F", M, R), [1, 2, 3, 4, 1, 2, 3, 4]);
        assert_eq!(fingering("F", M, L), [5, 4, 3, 2, 1, 3, 2, 1]);
        assert_eq!(fingering("B", M, R), [1, 2, 3, 1, 2, 3, 4, 5]);
        assert_eq!(fingering("B", M, L), [4, 3, 2, 1, 4, 3, 2, 1]);
        assert_eq!(fingering("Bb", M, R), [4, 1, 2, 3, 1, 2, 3, 4]);
        assert_eq!(fingering("Bb", M, L), [3, 2, 1, 4, 3, 2, 1, 3]);
        assert_eq!(fingering("Eb", M, R), [3, 1, 2, 3, 4, 1, 2, 3]);
        assert_eq!(fingering("Eb", M, L), [3, 2, 1, 4, 3, 2, 1, 3]);
        assert_eq!(fingering("Ab", M, R), [3, 4, 1, 2, 3, 1, 2, 3]);
        assert_eq!(fingering("Ab", M, L), [3, 2, 1, 4, 3, 2, 1, 3]);
        assert_eq!(fingering("Db", M, R), [2, 3, 1, 2, 3, 4, 1, 2]);
        assert_eq!(fingering("Db", M, L), [3, 2, 1, 4, 3, 2, 1, 3]);
        assert_eq!(fingering("Gb", M, R), [2, 3, 4, 1, 2, 3, 1, 2]);
        assert_eq!(fingering("Gb", M, L), [4, 3, 2, 1, 3, 2, 1, 4]);
        assert_eq!(fingering("A", Hm, R), [1, 2, 3, 1, 2, 3, 4, 5]);
        assert_eq!(fingering("F#", Hm, R), [3, 4, 1, 2, 3, 1, 2, 3]);
        assert_eq!(fingering("C#", Hm, R), [3, 4, 1, 2, 3, 1, 2, 3]);
        // Two octaves: the thumb stays on the middle tonic.
        let two = Scale::new(NoteName::C, M).notes(4, 2, false);
        assert_eq!(
            scale_fingering(&two, R).unwrap(),
            [1, 2, 3, 1, 2, 3, 4, 1, 2, 3, 1, 2, 3, 4, 5]
        );
        assert_eq!(
            scale_fingering(&two, L).unwrap(),
            [5, 4, 3, 2, 1, 3, 2, 1, 4, 3, 2, 1, 3, 2, 1]
        );
        let blues = Scale::new(NoteName::C, ScaleKind::Blues).notes(4, 1, false);
        assert_eq!(scale_fingering(&blues, R), None);
    }
}
