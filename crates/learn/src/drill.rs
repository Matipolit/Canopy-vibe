//! Scales and chords: what to play, in which octave, with which fingers,
//! checked in order (a scale) or together (a chord), and optionally timed
//! against a metronome.

use serde::{Deserialize, Serialize};

use crate::grading::{CHORD_WINDOW, Octave, Progress, Sequence};
use crate::notation::{Clef, ledger_lines};
use crate::song::Hands;
use crate::theory::{
    Chord, ChordKind, Hand, KeySig, NoteName, Scale, ScaleKind, Spelled, scale_fingering,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Scale,
    Chord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub kind: Kind,
    pub root: NoteName,
    pub scale: ScaleKind,
    pub chord: ChordKind,
    /// `None`: every inversion in turn, then root position an octave up.
    pub inversion: Option<u8>,
    pub hands: Hands,
    pub octaves: u8,
    pub down: bool,
    pub metronome: bool,
    pub bpm: f64,
    /// Notes (or chords) per beat against the metronome.
    pub per_beat: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            kind: Kind::Scale,
            root: NoteName::C,
            scale: ScaleKind::Major,
            chord: ChordKind::Major,
            inversion: None,
            hands: Hands::Right,
            octaves: 1,
            down: true,
            metronome: false,
            bpm: 72.0,
            per_beat: 1,
        }
    }
}

impl Settings {
    /// What's being practised, for the title and the stats.
    pub fn name(&self) -> String {
        let hands = match self.hands {
            Hands::Left => "left hand",
            Hands::Right => "right hand",
            Hands::Both => "hands together",
        };
        match self.kind {
            Kind::Scale => {
                let octaves = if self.octaves > 1 {
                    format!(" · {} octaves", self.octaves)
                } else {
                    String::new()
                };
                let way = if self.down { "" } else { " · up" };
                format!(
                    "{} scale · {hands}{octaves}{way}",
                    Scale::new(self.root, self.scale).name()
                )
            }
            Kind::Chord => {
                let inversion = match self.inversion {
                    None => "inversions".to_owned(),
                    Some(i) => crate::theory::inversion_name(i).to_owned(),
                };
                format!(
                    "{} {} chord · {inversion} · {hands}",
                    self.root.label(),
                    self.chord.name()
                )
            }
        }
    }

    /// Best scores are kept per tempo.
    pub fn variant(&self) -> String {
        if self.metronome {
            format!("{:.0} BPM", self.bpm)
        } else {
            "free time".to_owned()
        }
    }

    /// Seconds per beat.
    pub fn beat(&self) -> f64 {
        60.0 / self.bpm.max(20.0)
    }
}

/// A note of the drill as written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrillNote {
    pub note: Spelled,
    pub clef: Clef,
    pub hand: Hand,
    pub finger: Option<u8>,
    /// Which step it belongs to: notes of a chord share one.
    pub step: usize,
}

impl DrillNote {
    pub fn pitch(&self) -> u8 {
        self.note.pitch().unwrap_or(60)
    }
}

/// Beats of count-in before the first note, with the metronome.
pub const COUNT_IN: f64 = 4.0;

pub struct Drill {
    pub settings: Settings,
    pub key: Option<KeySig>,
    pub notes: Vec<DrillNote>,
    /// Above each step, for chords: "C/E".
    pub labels: Vec<String>,
    pub sequence: Sequence,
    /// When the first note was played (free time) or the metronome started.
    pub started: Option<f64>,
    pub finished_at: Option<f64>,
}

impl Drill {
    pub fn new(settings: Settings) -> Drill {
        let (notes, labels, key) = match settings.kind {
            Kind::Scale => scale_notes(&settings),
            Kind::Chord => chord_notes(&settings),
        };
        let steps = notes.iter().map(|n| n.step + 1).max().unwrap_or(0);
        let pitches: Vec<Vec<u8>> = (0..steps)
            .map(|s| {
                notes
                    .iter()
                    .filter(|n| n.step == s)
                    .map(DrillNote::pitch)
                    .collect()
            })
            .collect();
        let mut sequence = Sequence::new(pitches, CHORD_WINDOW, Octave::Exact);
        if settings.metronome {
            let per = settings.beat() / f64::from(settings.per_beat.max(1));
            let times = (0..steps)
                .map(|i| COUNT_IN * settings.beat() + i as f64 * per)
                .collect();
            sequence = sequence.with_schedule(times);
        }
        Drill {
            settings,
            key,
            notes,
            labels,
            sequence,
            started: None,
            finished_at: None,
        }
    }

    pub fn title(&self) -> String {
        match self.settings.kind {
            Kind::Scale => Scale::new(self.settings.root, self.settings.scale).name(),
            Kind::Chord => format!(
                "{} {}",
                self.settings.root.label(),
                self.settings.chord.name()
            ),
        }
    }

    pub fn steps(&self) -> usize {
        self.sequence.len()
    }

    /// With the metronome: starts the clock, the count-in's first beat at `beat_zero`.
    pub fn start_metronome(&mut self, beat_zero: f64) {
        self.started = Some(beat_zero);
        self.sequence.set_origin(beat_zero);
    }

    /// The metronome's first beat has been placed more exactly.
    pub fn set_beat_zero(&mut self, beat_zero: f64) {
        if self.settings.metronome && self.started.is_some() {
            self.started = Some(beat_zero);
            self.sequence.set_origin(beat_zero);
        }
    }

    pub fn press(&mut self, pitch: u8, now: f64) -> Progress {
        if self.sequence.is_finished() {
            return Progress::Nothing;
        }
        self.started.get_or_insert(now);
        let progress = self.sequence.press(pitch, now);
        if progress == Progress::Finished {
            self.finished_at = Some(now);
        }
        progress
    }

    pub fn tick(&mut self, now: f64) -> Progress {
        self.sequence.tick(now)
    }

    pub fn is_finished(&self) -> bool {
        self.sequence.is_finished()
    }

    /// The notes to play next.
    pub fn next_notes(&self) -> impl Iterator<Item = &DrillNote> {
        let step = self.sequence.index();
        self.notes.iter().filter(move |n| n.step == step)
    }

    pub fn elapsed(&self, now: f64) -> Option<f64> {
        Some(self.finished_at.unwrap_or(now) - self.started?)
    }
}

/// Total ledger lines `notes` need on `clef`: fewer is easier to read.
fn ledgers(notes: &[Spelled], clef: Clef) -> usize {
    notes
        .iter()
        .map(|n| ledger_lines(clef.position(*n)).len())
        .sum()
}

/// Picks the octave the right hand starts in, the left hand an octave
/// lower: the usual one (around middle C), unless another needs at least
/// two ledger lines fewer.
fn place(hands: Hands, notes_at: impl Fn(i8) -> Vec<Spelled>) -> (i8, i8) {
    let candidates: &[i8] = match hands {
        Hands::Right => &[4, 3, 5],
        Hands::Left => &[3, 2, 4],
        Hands::Both => &[4, 3, 5],
    };
    let cost = |o: i8| match hands {
        Hands::Right => ledgers(&notes_at(o), Clef::Treble),
        Hands::Left => ledgers(&notes_at(o), Clef::Bass),
        Hands::Both => ledgers(&notes_at(o), Clef::Treble) + ledgers(&notes_at(o - 1), Clef::Bass),
    };
    let mut best = candidates[0];
    let mut best_cost = cost(best);
    for &o in &candidates[1..] {
        let c = cost(o);
        if c + 2 <= best_cost {
            (best, best_cost) = (o, c);
        }
    }
    match hands {
        Hands::Left => (best + 1, best),
        _ => (best, best - 1),
    }
}

fn hands_of(hands: Hands) -> Vec<(Hand, Clef)> {
    match hands {
        Hands::Left => vec![(Hand::Left, Clef::Bass)],
        Hands::Right => vec![(Hand::Right, Clef::Treble)],
        Hands::Both => vec![(Hand::Left, Clef::Bass), (Hand::Right, Clef::Treble)],
    }
}

fn scale_notes(settings: &Settings) -> (Vec<DrillNote>, Vec<String>, Option<KeySig>) {
    let scale = Scale::new(settings.root, settings.scale);
    let octaves = settings.octaves.clamp(1, 2);
    let (right, left) = place(settings.hands, |o| scale.notes(o, octaves, settings.down));
    let mut notes = Vec::new();
    for (hand, clef) in hands_of(settings.hands) {
        let octave = if hand == Hand::Right { right } else { left };
        let written = scale.notes(octave, octaves, settings.down);
        let up = scale.notes(octave, octaves, false);
        let fingers = scale_fingering(&up, hand);
        for (step, note) in written.iter().enumerate() {
            // Coming down, each note keeps the finger it had going up.
            let finger = fingers.as_ref().and_then(|f| {
                let i = if step < up.len() {
                    step
                } else {
                    2 * (up.len() - 1) - step
                };
                f.get(i).copied()
            });
            notes.push(DrillNote {
                note: *note,
                clef,
                hand,
                finger,
                step,
            });
        }
    }
    (notes, Vec::new(), scale.key())
}

fn chord_notes(settings: &Settings) -> (Vec<DrillNote>, Vec<String>, Option<KeySig>) {
    let kind = settings.chord;
    // Each chord of the drill: an inversion, and octaves above the base.
    let chords: Vec<(u8, i8)> = match settings.inversion {
        Some(i) => vec![(i.min(kind.inversions()), 0)],
        None => (0..=kind.inversions())
            .map(|i| (i, 0))
            .chain(std::iter::once((0, 1)))
            .collect(),
    };
    let voicings = |base: i8| -> Vec<Vec<Spelled>> {
        chords
            .iter()
            .map(|&(inv, up)| Chord::new(settings.root, kind, inv).notes(base + up))
            .collect()
    };
    let (right, left) = place(settings.hands, |o| voicings(o).concat());
    let mut notes = Vec::new();
    let mut labels = Vec::new();
    for (step, &(inv, _)) in chords.iter().enumerate() {
        labels.push(Chord::new(settings.root, kind, inv).symbol());
        for (hand, clef) in hands_of(settings.hands) {
            let octave = if hand == Hand::Right { right } else { left };
            let voicing = &voicings(octave)[step];
            let fingers = chord_fingering(voicing, hand);
            for (note, finger) in voicing.iter().zip(fingers) {
                notes.push(DrillNote {
                    note: *note,
                    clef,
                    hand,
                    finger: Some(finger),
                    step,
                });
            }
        }
    }
    // A lone chord reads best with its accidentals on the notes.
    (notes, labels, None)
}

/// Block-chord fingering, lowest note first: the outer fingers on the
/// outer notes, and an inner finger that leaves room for the gap beside it.
pub fn chord_fingering(notes: &[Spelled], hand: Hand) -> Vec<u8> {
    let pitches: Vec<i32> = notes.iter().map(|n| n.midi()).collect();
    let middle = |gap: i32| match gap {
        g if g >= 5 => 2,
        g if g >= 3 => 3,
        _ => 4,
    };
    match (pitches.as_slice(), hand) {
        ([_, b, c], Hand::Right) => vec![1, middle(c - b), 5],
        ([a, b, _], Hand::Left) => vec![5, middle(b - a), 1],
        ([_, _, c, d], Hand::Right) => {
            if d - c <= 2 {
                vec![1, 2, 3, 4]
            } else {
                vec![1, 2, 3, 5]
            }
        }
        ([a, b, _, _], Hand::Left) => {
            if b - a <= 2 {
                vec![5, 4, 2, 1]
            } else {
                vec![5, 3, 2, 1]
            }
        }
        _ => match hand {
            Hand::Right => (1..=notes.len() as u8).collect(),
            Hand::Left => (1..=notes.len() as u8).rev().collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(drill: &Drill, hand: Hand) -> Vec<String> {
        drill
            .notes
            .iter()
            .filter(|n| n.hand == hand)
            .map(|n| n.note.label())
            .collect()
    }

    #[test]
    fn a_scale_with_fingers_up_and_down() {
        let drill = Drill::new(Settings::default());
        assert_eq!(drill.title(), "C major");
        assert_eq!(drill.steps(), 15);
        assert_eq!(drill.key, Some(KeySig::C));
        let right = labels(&drill, Hand::Right);
        assert_eq!(right[0], "C4");
        assert_eq!(right[7], "C5");
        assert_eq!(right[14], "C4");
        let fingers: Vec<u8> = drill.notes.iter().map(|n| n.finger.unwrap()).collect();
        assert_eq!(fingers, [1, 2, 3, 1, 2, 3, 4, 5, 4, 3, 2, 1, 3, 2, 1]);
        assert!(drill.notes.iter().all(|n| n.clef == Clef::Treble));
    }

    #[test]
    fn hands_together_an_octave_apart_and_placed_to_read() {
        let drill = Drill::new(Settings {
            root: NoteName::new(5, 0),
            hands: Hands::Both,
            down: false,
            ..Settings::default()
        });
        let (right, left) = (labels(&drill, Hand::Right), labels(&drill, Hand::Left));
        assert_eq!(right.len(), 8);
        assert_eq!(right[0], "A3", "A4 would climb above the treble staff");
        assert_eq!(left[0], "A2");
        // Each step is both hands' notes, played together.
        assert_eq!(drill.sequence.current(), Some(&[45, 57][..]));
        let left_fingers: Vec<u8> = drill
            .notes
            .iter()
            .filter(|n| n.hand == Hand::Left)
            .map(|n| n.finger.unwrap())
            .collect();
        assert_eq!(left_fingers, [5, 4, 3, 2, 1, 3, 2, 1]);
    }

    #[test]
    fn scales_are_played_in_order() {
        let mut drill = Drill::new(Settings {
            down: false,
            ..Settings::default()
        });
        assert_eq!(
            drill.press(60, 1.0),
            Progress::Done {
                index: 0,
                error_ms: None
            }
        );
        assert_eq!(drill.started, Some(1.0));
        assert_eq!(drill.press(64, 1.5), Progress::Wrong(64));
        for (i, p) in [62, 64, 65, 67, 69, 71].into_iter().enumerate() {
            assert!(matches!(
                drill.press(p, 2.0 + i as f64),
                Progress::Done { .. }
            ));
        }
        assert_eq!(drill.press(72, 9.0), Progress::Finished);
        assert_eq!(drill.elapsed(20.0), Some(8.0));
        assert_eq!(drill.sequence.mistakes, 1);
    }

    #[test]
    fn chords_with_every_inversion() {
        let drill = Drill::new(Settings {
            kind: Kind::Chord,
            ..Settings::default()
        });
        assert_eq!(drill.labels, ["C", "C/E", "C/G", "C"]);
        assert_eq!(drill.steps(), 4);
        let steps: Vec<Vec<String>> = (0..4)
            .map(|s| {
                drill
                    .notes
                    .iter()
                    .filter(|n| n.step == s)
                    .map(|n| n.note.label())
                    .collect()
            })
            .collect();
        assert_eq!(steps[0], ["C4", "E4", "G4"]);
        assert_eq!(steps[1], ["E4", "G4", "C5"]);
        assert_eq!(steps[3], ["C5", "E5", "G5"]);
        let fingers: Vec<u8> = drill.notes.iter().map(|n| n.finger.unwrap()).collect();
        assert_eq!(&fingers[..9], [1, 3, 5, 1, 2, 5, 1, 3, 5]);
    }

    #[test]
    fn chord_fingerings() {
        let f = |root: NoteName, kind, inv, hand| {
            chord_fingering(&Chord::new(root, kind, inv).notes(3), hand)
        };
        let c = NoteName::C;
        assert_eq!(f(c, ChordKind::Major, 0, Hand::Left), [5, 3, 1]);
        assert_eq!(f(c, ChordKind::Major, 1, Hand::Left), [5, 3, 1]);
        assert_eq!(f(c, ChordKind::Major, 2, Hand::Left), [5, 2, 1]);
        assert_eq!(f(c, ChordKind::Sus4, 0, Hand::Right), [1, 4, 5]);
        assert_eq!(f(c, ChordKind::Sus2, 0, Hand::Left), [5, 4, 1]);
        assert_eq!(f(c, ChordKind::Dominant7, 0, Hand::Right), [1, 2, 3, 5]);
        assert_eq!(f(c, ChordKind::Dominant7, 1, Hand::Right), [1, 2, 3, 4]);
        assert_eq!(f(c, ChordKind::Dominant7, 3, Hand::Left), [5, 4, 2, 1]);
    }

    #[test]
    fn metronome_schedule_after_a_bar_of_count_in() {
        let mut drill = Drill::new(Settings {
            metronome: true,
            bpm: 120.0,
            per_beat: 2,
            down: false,
            ..Settings::default()
        });
        drill.start_metronome(10.0);
        // Four beats of half a second, then a note every quarter second.
        assert_eq!(drill.sequence.due(0), Some(12.0));
        assert_eq!(drill.sequence.due(2), Some(12.5));
        let Progress::Done {
            error_ms: Some(e), ..
        } = drill.press(60, 12.03)
        else {
            panic!()
        };
        assert!((e - 30.0).abs() < 1e-6);
        drill.set_beat_zero(10.01);
        assert!((drill.sequence.due(1).unwrap() - 12.26).abs() < 1e-9);
        assert_eq!(drill.settings.variant(), "120 BPM");
    }

    #[test]
    fn names() {
        assert_eq!(Settings::default().name(), "C major scale · right hand");
        let chord = Settings {
            kind: Kind::Chord,
            chord: ChordKind::Minor7,
            inversion: Some(1),
            hands: Hands::Both,
            ..Settings::default()
        };
        assert_eq!(
            chord.name(),
            "C minor 7th chord · 1st inversion · hands together"
        );
    }
}
