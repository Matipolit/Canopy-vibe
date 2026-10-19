//! Note reading: a note appears, the player finds it on the keyboard. The
//! questions, the score and the rounds; the staff is drawn by the UI.

use serde::{Deserialize, Serialize};

use crate::grading::{Octave, note_matches};
use crate::notation::{Clef, TOP_LINE};
use crate::rng::Rng;
use crate::theory::{KeySig, Spelled};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Clefs {
    Treble,
    Bass,
    Both,
}

impl Clefs {
    pub const ALL: [Clefs; 3] = [Clefs::Treble, Clefs::Bass, Clefs::Both];

    pub fn label(self) -> &'static str {
        match self {
            Clefs::Treble => "Treble",
            Clefs::Bass => "Bass",
            Clefs::Both => "Grand staff",
        }
    }

    pub fn includes(self, clef: Clef) -> bool {
        match self {
            Clefs::Both => true,
            Clefs::Treble => clef == Clef::Treble,
            Clefs::Bass => clef == Clef::Bass,
        }
    }
}

/// The key the notes are written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyChoice {
    Fixed(KeySig),
    /// A new one each round.
    Random,
}

/// What the player reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Show {
    Staff,
    /// The note's name instead: finding keys on the keyboard.
    Name,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub clefs: Clefs,
    /// How many ledger lines notes may go beyond the staff.
    pub ledger: u8,
    pub accidentals: bool,
    pub key: KeyChoice,
    pub show: Show,
    pub octave: Octave,
    /// Notes per round; 0 for no end.
    pub round: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            clefs: Clefs::Both,
            ledger: 1,
            accidentals: false,
            key: KeyChoice::Fixed(KeySig::C),
            show: Show::Staff,
            octave: Octave::Exact,
            round: 20,
        }
    }
}

impl Settings {
    /// For the stats: the settings that make it harder or easier.
    pub fn name(&self) -> String {
        let what = match self.show {
            Show::Staff => format!("Reading · {}", self.clefs.label()),
            Show::Name => "Key names".to_owned(),
        };
        let range = match self.ledger {
            0 => "staff".to_owned(),
            1 => "1 ledger line".to_owned(),
            n => format!("{n} ledger lines"),
        };
        let key = match self.key {
            KeyChoice::Fixed(k) if k == KeySig::C => String::new(),
            KeyChoice::Fixed(k) => format!(" · {}", k.label()),
            KeyChoice::Random => " · any key".to_owned(),
        };
        let accidentals = if self.accidentals {
            " · accidentals"
        } else {
            ""
        };
        let octave = match self.octave {
            Octave::Any => " · any octave",
            Octave::Exact => "",
        };
        if self.show == Show::Name {
            format!("{what}{accidentals}{octave}")
        } else {
            format!("{what} · {range}{key}{accidentals}{octave}")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Question {
    pub note: Spelled,
    pub clef: Clef,
}

impl Question {
    pub fn pitch(&self) -> u8 {
        self.note.pitch().unwrap_or(60)
    }
}

/// Picks a note: a clef the settings allow, a line or space within the
/// range, the key's accidental, sometimes another one.
pub fn question(
    settings: &Settings,
    key: KeySig,
    rng: &mut Rng,
    previous: Option<Question>,
) -> Question {
    let clefs: Vec<Clef> = [Clef::Treble, Clef::Bass]
        .into_iter()
        .filter(|c| settings.clefs.includes(*c))
        .collect();
    let reach = 1 + 2 * i32::from(settings.ledger.min(4));
    let mut candidate = None;
    for _ in 0..20 {
        let clef = *rng.pick(&clefs).unwrap_or(&Clef::Treble);
        let position = rng.range(-reach, TOP_LINE + reach);
        let natural = clef.note_at(position);
        let in_key = key.accidental(natural.letter);
        let mut accidental = in_key;
        if settings.accidentals && rng.chance(0.4) {
            let options: Vec<i8> = [-1, 0, 1]
                .into_iter()
                .filter(|a| *a != in_key)
                // E#, B#, Fb and Cb only when the key has them.
                .filter(|a| !matches!((natural.letter, a), (2 | 6, 1) | (3 | 0, -1)))
                .collect();
            accidental = *rng.pick(&options).unwrap_or(&in_key);
        }
        let note = Spelled {
            accidental,
            ..natural
        };
        let q = Question { note, clef };
        candidate = Some(q);
        let repeat = previous.is_some_and(|p| p.pitch() == q.pitch());
        if !repeat && note.pitch().is_some_and(|p| (21..=108).contains(&p)) {
            break;
        }
    }
    candidate.unwrap_or(Question {
        note: Spelled::MIDDLE_C,
        clef: Clef::Treble,
    })
}

/// What a key press did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Answer {
    /// The right key; `seconds` since the note appeared.
    Right {
        first_try: bool,
        seconds: f64,
    },
    Wrong {
        played: u8,
    },
    /// Between rounds.
    Ignored,
}

pub struct Reading {
    pub settings: Settings,
    pub key: KeySig,
    pub question: Question,
    asked_at: f64,
    missed: bool,
    rng: Rng,
    /// Right the first time.
    pub correct: u32,
    /// Wrong presses.
    pub wrong: u32,
    pub streak: u32,
    pub best_streak: u32,
    /// Notes done this round.
    pub answered: u32,
    /// Seconds to the right answer, summed over first-try answers.
    pub think_time: f64,
    /// The round is over until restarted.
    pub finished: bool,
}

impl Reading {
    pub fn new(settings: Settings, mut rng: Rng, now: f64) -> Reading {
        let key = pick_key(settings.key, &mut rng);
        let question = question(&settings, key, &mut rng, None);
        Reading {
            settings,
            key,
            question,
            asked_at: now,
            missed: false,
            rng,
            correct: 0,
            wrong: 0,
            streak: 0,
            best_streak: 0,
            answered: 0,
            think_time: 0.0,
            finished: false,
        }
    }

    /// A new round, with new settings if they changed.
    pub fn restart(&mut self, now: f64) {
        self.key = pick_key(self.settings.key, &mut self.rng);
        self.question = question(&self.settings, self.key, &mut self.rng, None);
        self.asked_at = now;
        self.missed = false;
        self.correct = 0;
        self.wrong = 0;
        self.streak = 0;
        self.best_streak = 0;
        self.answered = 0;
        self.think_time = 0.0;
        self.finished = false;
    }

    pub fn press(&mut self, played: u8, now: f64) -> Answer {
        if self.finished {
            return Answer::Ignored;
        }
        if !note_matches(self.question.pitch(), played, self.settings.octave) {
            self.wrong += 1;
            self.streak = 0;
            self.missed = true;
            return Answer::Wrong { played };
        }
        let first_try = !self.missed;
        let seconds = now - self.asked_at;
        if first_try {
            self.correct += 1;
            self.streak += 1;
            self.best_streak = self.best_streak.max(self.streak);
            self.think_time += seconds;
        }
        self.answered += 1;
        if self.settings.round > 0 && self.answered >= self.settings.round {
            self.finished = true;
        } else {
            self.next(now);
        }
        Answer::Right { first_try, seconds }
    }

    /// Moves on without answering.
    pub fn skip(&mut self, now: f64) {
        if !self.finished {
            self.streak = 0;
            self.answered += 1;
            if self.settings.round > 0 && self.answered >= self.settings.round {
                self.finished = true;
            } else {
                self.next(now);
            }
        }
    }

    fn next(&mut self, now: f64) {
        self.question = question(&self.settings, self.key, &mut self.rng, Some(self.question));
        self.asked_at = now;
        self.missed = false;
    }

    /// Notes right the first time, of those done.
    pub fn accuracy(&self) -> Option<f64> {
        (self.answered > 0).then(|| f64::from(self.correct) / f64::from(self.answered))
    }

    /// Average seconds to find a note, over first-try answers.
    pub fn mean_time(&self) -> Option<f64> {
        (self.correct > 0).then(|| self.think_time / f64::from(self.correct))
    }
}

fn pick_key(choice: KeyChoice, rng: &mut Rng) -> KeySig {
    match choice {
        KeyChoice::Fixed(key) => key,
        KeyChoice::Random => KeySig(rng.range(-6, 6) as i8),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_stay_in_range_and_key() {
        let mut rng = Rng::new(1);
        let settings = Settings {
            clefs: Clefs::Treble,
            ledger: 0,
            ..Settings::default()
        };
        for _ in 0..300 {
            let q = question(&settings, KeySig(2), &mut rng, None);
            assert_eq!(q.clef, Clef::Treble);
            let position = q.clef.position(q.note);
            assert!((-1..=9).contains(&position), "{position}");
            // D major: F and C are sharp, nothing else is altered.
            let expected = if matches!(q.note.letter, 0 | 3) { 1 } else { 0 };
            assert_eq!(q.note.accidental, expected);
        }
        let wide = Settings {
            clefs: Clefs::Bass,
            ledger: 3,
            accidentals: true,
            ..Settings::default()
        };
        let mut altered = 0;
        let mut previous = None;
        for _ in 0..300 {
            let q = question(&wide, KeySig::C, &mut rng, previous);
            assert_ne!(
                Some(q.pitch()),
                previous.map(|p: Question| p.pitch()),
                "no repeats"
            );
            let position = q.clef.position(q.note);
            assert!((-7..=15).contains(&position));
            assert!(!matches!(
                (q.note.letter, q.note.accidental),
                (2 | 6, 1) | (0 | 3, -1)
            ));
            altered += usize::from(q.note.accidental != 0);
            previous = Some(q);
        }
        assert!(altered > 50, "accidentals show up: {altered}");
    }

    #[test]
    fn scoring_a_round() {
        let settings = Settings {
            round: 3,
            ..Settings::default()
        };
        let mut r = Reading::new(settings, Rng::new(3), 0.0);
        let first = r.question.pitch();
        assert_eq!(
            r.press(first, 1.5),
            Answer::Right {
                first_try: true,
                seconds: 1.5
            }
        );
        assert_eq!((r.correct, r.streak, r.answered), (1, 1, 1));

        let second = r.question.pitch();
        assert_eq!(
            r.press(second + 1, 2.0),
            Answer::Wrong { played: second + 1 }
        );
        assert_eq!(r.question.pitch(), second, "the note stays until found");
        assert_eq!((r.wrong, r.streak), (1, 0));
        assert!(matches!(
            r.press(second, 3.0),
            Answer::Right {
                first_try: false,
                ..
            }
        ));
        assert_eq!((r.correct, r.answered), (1, 2));

        let third = r.question.pitch();
        r.press(third, 4.0);
        assert!(r.finished);
        assert_eq!(r.press(60, 5.0), Answer::Ignored);
        assert!((r.accuracy().unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(r.best_streak, 1);
        assert!((r.mean_time().unwrap() - 1.25).abs() < 1e-9);
        r.restart(6.0);
        assert!(!r.finished);
        assert_eq!(r.answered, 0);
    }

    #[test]
    fn any_octave_accepts_the_same_name() {
        let settings = Settings {
            octave: Octave::Any,
            ..Settings::default()
        };
        let mut r = Reading::new(settings, Rng::new(9), 0.0);
        let pitch = r.question.pitch();
        let other = if pitch >= 72 { pitch - 12 } else { pitch + 12 };
        assert!(matches!(
            r.press(other, 1.0),
            Answer::Right {
                first_try: true,
                ..
            }
        ));
    }

    #[test]
    fn names_for_the_stats() {
        assert_eq!(
            Settings::default().name(),
            "Reading · Grand staff · 1 ledger line"
        );
        let s = Settings {
            clefs: Clefs::Treble,
            ledger: 0,
            key: KeyChoice::Fixed(KeySig(-2)),
            accidentals: true,
            ..Settings::default()
        };
        assert_eq!(
            s.name(),
            "Reading · Treble · staff · Bb major · accidentals"
        );
        let names = Settings {
            show: Show::Name,
            octave: Octave::Any,
            ..Settings::default()
        };
        assert_eq!(names.name(), "Key names · any octave");
    }
}
