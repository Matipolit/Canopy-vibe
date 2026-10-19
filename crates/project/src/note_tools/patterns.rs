//! Strum patterns: held chords played in a rhythm, the way a strumming
//! hand plays them.
//!
//! # The idea
//!
//! Chords are drawn as long notes, one per string, lasting as long as the
//! chord does. [`strum_pattern`] replaces each one with strokes on a
//! pattern's grid: eighths, sixteenths or eighth triplets of the song.
//!
//! The hand keeps time. The pattern runs from the bar line the first chord
//! is in and carries on across chord changes, so a chord that changes in
//! the middle of the pattern picks it up where it has got to, as a
//! guitarist's would ([`StrumPattern::restart`] starts it again on each
//! chord instead). A chord lasts until the next one starts; one that isn't
//! a whole number of patterns long gets as much of the pattern as fits.
//!
//! Down or up is never written. As on a real guitar it follows from where
//! a stroke falls: down on the beats and the eighths, up between; and once
//! the pattern strikes between eighths, the hand moves in sixteenths (down
//! on every eighth, up on the sixteenths between). Eighth triplets go down
//! on the beat and up after it. The Guitar's Alternate strum direction reads
//! the same thing from the timing, so this tool only places notes and the
//! guitar plays the directions: [`Pattern::directions`] is what the piano
//! roll's step preview draws.
//!
//! # Syntax
//!
//! One character per step. Upper case is accented, lower case not:
//!
//! | step     | plays |
//! |----------|-------|
//! | `D` `d`, `U` `u` | the whole chord, strummed (`D` and `U` mean the same: the direction comes from where the step is, so write whichever reads better) |
//! | `C` `c`  | the chord without its lowest note: fingers, while a thumb has the bass |
//! | `B` `b`  | only the lowest note: a bass note |
//! | `X` `x`  | a dead-note chuck: the strings muted by the fretting hand |
//! | `P` `p`  | a palm-muted strum |
//! | `-`      | a rest: the hand moves past without touching the strings, and nothing sounds |
//! | `_`      | a tie: the stroke before rings on through this step |
//!
//! Spaces and `|` are ignored, for grouping steps into beats or bars. A
//! pattern is as long as its steps, so it can be a bar of any meter (six
//! eighths for 3/4) or several bars. [`Pattern::parse`] says what is wrong
//! with one that can't be played ([`PatternError`]).
//!
//! # Lengths, swing and the rest
//!
//! A stroke sounds for [`StrumPattern::gate`] of its step, plus every tie
//! after it; it always stops where the next stroke starts, as a strumming
//! hand's strings do. Swing delays every off-beat step (the second of each
//! pair) by a share of a step, as groove templates do. Upstrokes can leave
//! out the lowest notes ([`StrumPattern::upstroke_skip`]), since an
//! upstroke mostly catches the top strings. Velocities are shares of each
//! chord note's own velocity, so a voicing keeps its balance.
//!
//! # Articulations
//!
//! On the Guitar with its key-switches on ([`Articulation::KeySwitches`]),
//! dead-note steps are played as dead notes and palm-mute steps palm-muted:
//! a key-switch note (D#0 or C#0) goes just before the step, and C0 before
//! the next open one. Key-switches latch, so one goes in only where the
//! articulation changes, and the clip is left in the articulation it had
//! after the chords. Any other instrument ([`Articulation::Shortened`])
//! plays a dead note as a very short, quiet strum and a palm mute as a
//! short, softer one, so the tool works for piano comping and synth stabs
//! too.
//!
//! # Presets
//!
//! [`PRESETS`] are common patterns as guitarists play them; their sources
//! are listed there.

use std::fmt;

use instruments::builtin::guitar::{self, ARTICULATIONS, KEY_SWITCH_FIRST, KEY_SWITCH_LAST};
use midi::{PPQ, Pitch, Tick, Velocity};

use super::{Random, chords, join, split};
use crate::{Note, NoteId, Track, TrackKind};

/// The longest pattern: four bars of sixteenths.
pub const MAX_STEPS: usize = 64;

/// The most swing: an off-beat step three quarters of a step late.
pub const MAX_SWING: f32 = 0.75;

/// The shortest gate, so a stroke always sounds.
pub const MIN_GATE: f32 = 0.05;

/// How long before its step a key-switch note goes, so the guitar has
/// switched by the time the chord arrives.
const SWITCH_LEAD: i64 = 10;
/// How long a key-switch note is (the guitar ignores its end).
const SWITCH_LEN: i64 = 30;

/// At full humanize, the most a stroke moves (about 10 ms at 120 BPM)...
const HUMANIZE_TICKS: f64 = 20.0;
/// ...and the most its velocity changes, as a share of itself.
const HUMANIZE_VELOCITY: f32 = 0.12;

/// Off the Guitar: a dead note is this quiet and this much of its step
/// long...
const DEAD_LEVEL: f32 = 0.5;
const DEAD_LENGTH: f32 = 0.25;
/// ...and a palm mute this soft and at most this long.
const PALM_LEVEL: f32 = 0.8;
const PALM_LENGTH: f32 = 0.5;

/// The Guitar's articulations, by their place in [`ARTICULATIONS`].
const OPEN: usize = 0;
const PALM_MUTE: usize = 1;
const DEAD: usize = 3;

/// What a stroke plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// Every note of the chord.
    Strum,
    /// The chord without its lowest note.
    Chord,
    /// Only the lowest note.
    Bass,
    /// A dead-note chuck.
    Dead,
    /// A palm-muted strum.
    PalmMute,
}

/// One step of a pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A stroke, accented or not.
    Hit { hit: Hit, accent: bool },
    /// The hand moves past without touching the strings: silence.
    Rest,
    /// The stroke before rings on through this step.
    Tie,
}

/// Which way the strumming hand moves through a stroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Low strings first.
    Down,
    /// High strings first.
    Up,
}

/// What is wrong with a pattern's text. Positions count characters from 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    /// No steps at all.
    Empty,
    /// A character that isn't a step.
    Unknown { found: char, at: usize },
    /// Only rests and ties: nothing to play.
    NoStrokes,
    /// A tie with nothing to hold: it follows a rest.
    LooseTie { at: usize },
    /// More than [`MAX_STEPS`] steps.
    TooLong { steps: usize },
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatternError::Empty => write!(f, "Type a pattern: D strums, - rests, _ holds"),
            PatternError::Unknown { found, at } => write!(
                f,
                "'{found}' (character {at}) isn't a step. Steps are D/U strum, C chord without \
                 bass, B bass, X dead note, P palm mute, - rest, _ hold"
            ),
            PatternError::NoStrokes => write!(f, "Nothing to play: only rests and holds"),
            PatternError::LooseTie { at } => write!(
                f,
                "The _ at character {at} follows a rest, so there's nothing to hold"
            ),
            PatternError::TooLong { steps } => {
                write!(f, "{steps} steps is too long: at most {MAX_STEPS}")
            }
        }
    }
}

impl std::error::Error for PatternError {}

/// A rhythm of steps, parsed from its text (see the module's syntax).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// As typed, spaces and all.
    text: String,
    steps: Vec<Step>,
}

impl Pattern {
    /// Reads a pattern, or says what is wrong with it.
    pub fn parse(text: &str) -> Result<Pattern, PatternError> {
        let hit = |hit, accent| Step::Hit { hit, accent };
        let mut steps = Vec::new();
        // Where each step was typed, for errors.
        let mut columns = Vec::new();
        for (i, c) in text.chars().enumerate() {
            let step = match c {
                c if c.is_whitespace() || c == '|' => continue,
                'D' | 'U' => hit(Hit::Strum, true),
                'd' | 'u' => hit(Hit::Strum, false),
                'C' => hit(Hit::Chord, true),
                'c' => hit(Hit::Chord, false),
                'B' => hit(Hit::Bass, true),
                'b' => hit(Hit::Bass, false),
                'X' => hit(Hit::Dead, true),
                'x' => hit(Hit::Dead, false),
                'P' => hit(Hit::PalmMute, true),
                'p' => hit(Hit::PalmMute, false),
                '-' => Step::Rest,
                '_' => Step::Tie,
                found => return Err(PatternError::Unknown { found, at: i + 1 }),
            };
            steps.push(step);
            columns.push(i + 1);
        }
        if steps.is_empty() {
            return Err(PatternError::Empty);
        }
        if steps.len() > MAX_STEPS {
            return Err(PatternError::TooLong { steps: steps.len() });
        }
        if !steps.iter().any(|s| matches!(s, Step::Hit { .. })) {
            return Err(PatternError::NoStrokes);
        }
        // A tie holds the stroke before it, round the end of the pattern
        // (it repeats): back over any other ties to what that is.
        let n = steps.len();
        for (i, step) in steps.iter().enumerate() {
            if *step != Step::Tie {
                continue;
            }
            let held = (1..n)
                .map(|back| steps[(i + n - back) % n])
                .find(|s| *s != Step::Tie);
            if held == Some(Step::Rest) {
                return Err(PatternError::LooseTie { at: columns[i] });
            }
        }
        Ok(Pattern {
            text: text.to_owned(),
            steps,
        })
    }

    /// The text it was read from.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// How many steps it has.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Never true: a pattern has at least one stroke.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Which way the hand moves on each step at `resolution`, starting on a
    /// beat (`None` for rests and ties): what the step preview draws.
    pub fn directions(&self, resolution: Resolution) -> Vec<Option<Direction>> {
        let sixteenths =
            self.steps.iter().enumerate().any(|(i, step)| {
                matches!(step, Step::Hit { .. }) && resolution.off_eighth(i as i64)
            });
        self.steps
            .iter()
            .enumerate()
            .map(|(i, step)| match step {
                Step::Hit { .. } => Some(resolution.direction(i as i64, sixteenths)),
                _ => None,
            })
            .collect()
    }
}

impl Default for Pattern {
    /// The first preset's.
    fn default() -> Self {
        PRESETS[0].steps()
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::str::FromStr for Pattern {
    type Err = PatternError;

    fn from_str(text: &str) -> Result<Pattern, PatternError> {
        Pattern::parse(text)
    }
}

/// The grid a pattern's steps sit on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Resolution {
    #[default]
    Eighth,
    Sixteenth,
    EighthTriplet,
}

impl Resolution {
    pub const ALL: [Resolution; 3] = [
        Resolution::Eighth,
        Resolution::Sixteenth,
        Resolution::EighthTriplet,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Resolution::Eighth => "1/8",
            Resolution::Sixteenth => "1/16",
            Resolution::EighthTriplet => "1/8 triplet",
        }
    }

    /// Steps in a beat (a quarter note).
    pub fn per_beat(self) -> i64 {
        match self {
            Resolution::Eighth => 2,
            Resolution::Sixteenth => 4,
            Resolution::EighthTriplet => 3,
        }
    }

    /// How long a step is.
    pub fn step(self) -> Tick {
        Tick::new(PPQ / self.per_beat())
    }

    /// Whether swing moves its steps: triplets already swing.
    pub fn swings(self) -> bool {
        self != Resolution::EighthTriplet
    }

    /// Whether step `k` (counted from a beat) falls between eighths.
    fn off_eighth(self, k: i64) -> bool {
        match self {
            Resolution::Eighth => false,
            Resolution::Sixteenth => k.rem_euclid(2) == 1,
            Resolution::EighthTriplet => k.rem_euclid(3) != 0,
        }
    }

    /// The hand's direction on step `k` (counted from a beat), moving in
    /// sixteenths if `sixteenths`, else in eighths. The same reading as the
    /// Guitar's Alternate strum direction.
    fn direction(self, k: i64, sixteenths: bool) -> Direction {
        let down = match self {
            Resolution::Eighth => k.rem_euclid(2) == 0,
            Resolution::Sixteenth if sixteenths => k.rem_euclid(2) == 0,
            Resolution::Sixteenth => k.div_euclid(2).rem_euclid(2) == 0,
            // Between beats the hand is on its way up.
            Resolution::EighthTriplet => k.rem_euclid(3) == 0,
        };
        if down { Direction::Down } else { Direction::Up }
    }
}

/// How dead-note and palm-mute steps are played.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Articulation {
    /// By any instrument: a dead note as a very short, quiet strum, a palm
    /// mute as a short, softer one.
    #[default]
    Shortened,
    /// By the Guitar's key-switches. `setting` is its Articulation
    /// setting, what it plays before any key-switch.
    KeySwitches { setting: usize },
}

impl Articulation {
    /// How `track`'s instrument plays them: by key-switches on the Guitar
    /// with them on, else shortened.
    pub fn for_track(track: &Track) -> Articulation {
        let TrackKind::Midi { instrument, .. } = &track.kind else {
            return Articulation::Shortened;
        };
        let switches = track
            .instrument_param("key_switches")
            .is_some_and(|on| on >= 0.5);
        if instrument.id != guitar::DESCRIPTOR.id || !switches {
            return Articulation::Shortened;
        }
        let setting = track.instrument_param("articulation").unwrap_or(0.0);
        Articulation::KeySwitches {
            setting: setting.max(0.0) as usize,
        }
    }
}

/// The strum pattern tool's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct StrumPattern {
    pub pattern: Pattern,
    pub resolution: Resolution,
    /// How late every off-beat step comes, as a share of a step, up to
    /// [`MAX_SWING`]: 0 is straight, a third a triplet shuffle, a half a
    /// dotted one. Triplets don't swing.
    pub swing: f32,
    /// How much of its (last) step a stroke sounds for, [`MIN_GATE`] to 1:
    /// 1 rings up to the next step, 0.3 is staccato. Ties add whole steps.
    pub gate: f32,
    /// Accented strokes' velocity, as a share of each chord note's own.
    pub accent: f32,
    /// The other strokes' velocity, likewise.
    pub normal: f32,
    /// How many of the lowest notes upstrokes leave out (0 to 2). A chord
    /// keeps at least two notes.
    pub upstroke_skip: u8,
    /// Start the pattern again on each chord, instead of carrying on from
    /// the bar.
    pub restart: bool,
    /// Timing and velocity variation, 0 to 1. Light: the Guitar varies its
    /// strums itself.
    pub humanize: f32,
    /// What the variation is made from: the same seed, the same result.
    pub seed: u64,
}

impl Default for StrumPattern {
    fn default() -> Self {
        let mut settings = StrumPattern {
            pattern: Pattern::default(),
            resolution: Resolution::Eighth,
            swing: 0.0,
            gate: 1.0,
            accent: 1.0,
            normal: 0.8,
            upstroke_skip: 0,
            restart: false,
            humanize: 0.0,
            seed: 1,
        };
        PRESETS[0].apply(&mut settings);
        settings
    }
}

impl StrumPattern {
    /// The preset these settings play, if any: the same steps on the same
    /// grid (the gate and swing may have been changed since).
    pub fn preset(&self) -> Option<&'static Preset> {
        PRESETS.iter().find(|p| {
            p.resolution == self.resolution
                && Pattern::parse(p.pattern).is_ok_and(|q| q.steps == self.pattern.steps)
        })
    }
}

/// A chord the pattern plays: its grid steps `first..end` and its notes,
/// low to high.
struct Chord {
    first: i64,
    end: i64,
    /// Where its earliest note started (timeline ticks).
    drawn: i64,
    notes: Vec<Note>,
}

/// A stroke: on grid step `k`, with `ties` tied steps after it, of chord
/// `chord`.
struct Stroke {
    k: i64,
    hit: Hit,
    accent: bool,
    ties: i64,
    chord: usize,
}

/// `value / step`, rounded to the nearest (halves up).
fn nearest(value: i64, step: i64) -> i64 {
    (2 * value + step).div_euclid(2 * step)
}

fn scaled(velocity: Velocity, by: f32) -> Velocity {
    let value = (velocity.as_f32() * by).clamp(1.0 / 127.0, 1.0);
    Velocity::new((value * f32::from(u16::MAX)).round() as u16)
}

fn articulation_switch(note: &Note) -> Option<usize> {
    let index = note.pitch.get().checked_sub(KEY_SWITCH_FIRST)?;
    Some(usize::from(index)).filter(|i| *i < ARTICULATIONS.len())
}

/// Plays the chords among `notes` (those in `ids`, or all if it's empty)
/// in pattern `s`; returns the clip's new notes.
///
/// Chords are notes starting together, on the nearest step of the grid;
/// each lasts until the next one starts. `origin` is where the notes'
/// zero is on the timeline and `bar` how long a bar is, so the grid and the
/// pattern line up with the song's bars. Dead notes and palm mutes are
/// played as `articulation` says. See the module documentation for the
/// rest.
pub fn strum_pattern(
    notes: &[Note],
    ids: &[NoteId],
    s: &StrumPattern,
    articulation: Articulation,
    origin: Tick,
    bar: Tick,
    new_id: &mut dyn FnMut() -> NoteId,
) -> Vec<Note> {
    let (selected, mut rest) = split(notes, ids);
    let switching = matches!(articulation, Articulation::KeySwitches { .. });
    // The Guitar's key-switch notes are never part of a chord.
    let (switches, played): (Vec<Note>, Vec<Note>) = selected
        .into_iter()
        .partition(|n| switching && (KEY_SWITCH_FIRST..=KEY_SWITCH_LAST).contains(&n.pitch.get()));
    let origin = origin.get();
    let step = s.resolution.step().get();
    let pattern = s.pattern.steps();
    let len = pattern.len() as i64;

    // Chords on the grid, each until the next.
    let mut placed: Vec<Chord> = Vec::new();
    for group in chords(played) {
        let drawn = origin + group[0].start.get();
        let end = group.iter().map(|n| origin + n.end().get()).max();
        let first = nearest(drawn, step);
        let end = nearest(end.unwrap_or(drawn), step).max(first + 1);
        match placed.last_mut() {
            // Started on the same step: one chord.
            Some(chord) if chord.first == first => {
                chord.notes.extend(group);
                chord.end = chord.end.max(end);
            }
            _ => placed.push(Chord {
                first,
                end,
                drawn,
                notes: group,
            }),
        }
    }
    if placed.is_empty() {
        return notes.to_vec();
    }
    for i in 1..placed.len() {
        let next = placed[i].first;
        placed[i - 1].end = placed[i - 1].end.min(next);
    }
    for chord in &mut placed {
        chord.notes.sort_by_key(|n| n.pitch);
        chord.notes.dedup_by_key(|n| n.pitch);
    }

    // The pattern starts on the bar line at or before the first chord.
    let bar = bar.get().max(1);
    let bar_line = (placed[0].first * step).div_euclid(bar) * bar;
    let start = (bar_line + step - 1).div_euclid(step);

    let mut strokes = Vec::new();
    for (index, chord) in placed.iter().enumerate() {
        let zero = if s.restart { chord.first } else { start };
        // A chord as long as a few hundred bars is still finite work.
        for k in chord.first..chord.end.min(chord.first + 16_384) {
            let at = (k - zero).rem_euclid(len) as usize;
            let Step::Hit { hit, accent } = pattern[at] else {
                continue;
            };
            let ties = (1..chord.end - k)
                .take_while(|t| pattern[(at + *t as usize) % pattern.len()] == Step::Tie)
                .count() as i64;
            strokes.push(Stroke {
                k,
                hit,
                accent,
                ties,
                chord: index,
            });
        }
    }
    // The hand moves in sixteenths if any stroke falls between eighths.
    let sixteenths = strokes.iter().any(|st| s.resolution.off_eighth(st.k));

    let swing = if s.resolution.swings() {
        (f64::from(s.swing.clamp(0.0, MAX_SWING)) * step as f64).round() as i64
    } else {
        0
    };
    let time = |k: i64| k * step + if k.rem_euclid(2) == 1 { swing } else { 0 };
    let gate = f64::from(s.gate.clamp(MIN_GATE, 1.0));
    let humanize = f64::from(s.humanize.clamp(0.0, 1.0));
    let mut random = Random::new(s.seed);

    // Each stroke's span on the timeline, what it plays and how loud.
    let mut spans: Vec<(i64, i64)> = Vec::with_capacity(strokes.len());
    let mut voices_of: Vec<(Vec<&Note>, f32)> = Vec::with_capacity(strokes.len());
    for stroke in &strokes {
        let chord = &placed[stroke.chord];
        let last = stroke.k + stroke.ties;
        let grid = time(stroke.k);
        let own_step = (time(stroke.k + 1) - grid) as f64;
        let mut end = time(last) + (gate * (time(last + 1) - time(last)) as f64).round() as i64;
        let mut voices: Vec<&Note> = match stroke.hit {
            Hit::Bass => chord.notes.iter().take(1).collect(),
            Hit::Chord if chord.notes.len() > 1 => chord.notes.iter().skip(1).collect(),
            _ => chord.notes.iter().collect(),
        };
        let direction = s.resolution.direction(stroke.k, sixteenths);
        if direction == Direction::Up && stroke.hit != Hit::Bass {
            let drop = usize::from(s.upstroke_skip).min(voices.len().saturating_sub(2));
            voices.drain(..drop);
        }
        let mut level = if stroke.accent { s.accent } else { s.normal };
        if !switching {
            match stroke.hit {
                Hit::Dead => {
                    level *= DEAD_LEVEL;
                    end = grid + (own_step * f64::from(DEAD_LENGTH)).round() as i64;
                }
                Hit::PalmMute => {
                    level *= PALM_LEVEL;
                    end = end.min(grid + (own_step * f64::from(PALM_LENGTH)).round() as i64);
                }
                _ => {}
            }
        }
        // Drawn every time, so the amount only scales the same variation.
        let nudge = (random.signed() * humanize * HUMANIZE_TICKS).round() as i64;
        let louder = 1.0 + (humanize * random.signed()) as f32 * HUMANIZE_VELOCITY;
        // Never earlier than both the grid and the chord as drawn.
        let start = (grid + nudge).max(grid.min(chord.drawn));
        let end = (end + nudge).min(time(chord.end)).max(start + 1);
        spans.push((start, end));
        voices_of.push((voices, level * louder));
    }
    // A stroke stops where the next starts, as strummed strings do.
    for i in 1..spans.len() {
        let next = spans[i].0;
        let (start, end) = &mut spans[i - 1];
        *end = (*end).min(next).max(*start + 1);
    }

    let mut made = Vec::new();
    for ((start, end), (voices, level)) in spans.iter().zip(&voices_of) {
        for voice in voices {
            made.push(Note {
                id: new_id(),
                start: Tick::new(start - origin),
                len: Tick::new(end - start),
                velocity: scaled(voice.velocity, *level),
                ..(*voice).clone()
            });
        }
    }

    if let Articulation::KeySwitches { setting } = articulation {
        let range_end = time(placed[placed.len() - 1].end);
        // Selected key-switches among the chords are the pattern's to
        // write; the rest stay, and are followed.
        let original: Vec<(i64, usize)> = switch_times(notes, origin);
        rest.extend(
            switches
                .into_iter()
                .filter(|n| origin + n.start.get() >= range_end),
        );
        let kept = switch_times(&rest, origin);
        let setting = setting.min(ARTICULATIONS.len() - 1);
        let mut current = setting;
        let mut next_kept = 0;
        let mut previous = i64::MIN;
        let mut switch = |at: i64, articulation: usize| Note {
            id: new_id(),
            start: Tick::new(at - origin),
            len: Tick::new(SWITCH_LEN),
            pitch: Pitch::new(KEY_SWITCH_FIRST + articulation as u8)
                .expect("key-switches are notes"),
            velocity: Velocity::from_midi1(100),
            release_velocity: Velocity::CENTER,
            muted: false,
        };
        for (stroke, &(start, _)) in strokes.iter().zip(&spans) {
            while next_kept < kept.len() && kept[next_kept].0 <= start {
                current = kept[next_kept].1;
                next_kept += 1;
            }
            let want = match stroke.hit {
                Hit::Dead => DEAD,
                Hit::PalmMute => PALM_MUTE,
                _ => OPEN,
            };
            if want != current {
                // Just before the stroke, but not before the one before it
                // or before the chord was drawn.
                let drawn = placed[stroke.chord].drawn.min(start);
                let at = (start - SWITCH_LEAD).max(previous + 1).max(drawn);
                made.push(switch(at, want));
                current = want;
            }
            previous = start;
        }
        // Leave the guitar as the clip had it after the chords.
        while next_kept < kept.len() && kept[next_kept].0 <= range_end {
            current = kept[next_kept].1;
            next_kept += 1;
        }
        let expected = original
            .iter()
            .take_while(|(at, _)| *at <= range_end)
            .last()
            .map_or(setting, |&(_, a)| a);
        if current != expected {
            let at = (range_end - SWITCH_LEAD).max(previous + 1);
            made.push(switch(at, expected));
        }
    }
    join(rest, made)
}

/// The articulation key-switches among `notes`: (timeline tick, which),
/// in time order.
fn switch_times(notes: &[Note], origin: i64) -> Vec<(i64, usize)> {
    let mut times: Vec<(i64, usize)> = notes
        .iter()
        .filter_map(|n| articulation_switch(n).map(|a| (origin + n.start.get(), a)))
        .collect();
    times.sort_by_key(|&(at, _)| at);
    times
}

/// A pattern as guitarists play it, with the grid and feel it goes with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    pub name: &'static str,
    /// The steps (spaces only group them for reading).
    pub pattern: &'static str,
    pub resolution: Resolution,
    pub swing: f32,
    pub gate: f32,
    pub upstroke_skip: u8,
    /// The meter it's a bar (or two) of: (beats, note value).
    pub meter: (u8, u8),
    /// What it is, for a tooltip.
    pub about: &'static str,
}

impl Preset {
    /// Its steps, parsed.
    pub fn steps(&self) -> Pattern {
        Pattern::parse(self.pattern).expect("presets are valid (tested)")
    }

    /// Sets `s` to play it; the velocities, restart and humanize stay.
    pub fn apply(&self, s: &mut StrumPattern) {
        s.pattern = self.steps();
        s.resolution = self.resolution;
        s.swing = self.swing;
        s.gate = self.gate;
        s.upstroke_skip = self.upstroke_skip;
    }
}

/// The factory patterns. Where they come from:
///
/// - Folk / pop is "Old Faithful", d du udu: Wikipedia, "Strum"; JustinGuitar,
///   "THE Strumming Pattern" (the missed down-stroke on 3, the hand kept
///   moving); accents on 1 and 4 as Aloha Ukulele's "Classic Island".
/// - Straight eighths: Wikipedia, "Strum" (dudududu); Guitar Tool Hub, "8
///   Guitar Strumming Patterns Every Guitarist Should Know".
/// - Driving sixteenths and Funk sixteenths: Premier Guitar, "Where to
///   Start with Funk Guitar" (1 and & always down, e and a always up; stabs
///   among muted scratches); Jimmy Nolen's "chicken scratch" (Wikipedia).
/// - Island strum: Old Faithful with chucks on the backbeat, Ukulele
///   Magazine, "Chuck and Chunk the Backbeat".
/// - Reggae skank: the chop on 2 and 4 at reggae tempos (JamPlay,
///   "Discovering Ska"), often a down-stroke (Muzcafe, after Guitar World).
/// - Ska upstrokes: clipped up-strokes on the treble strings on every
///   off-beat (Wikipedia, "Ska stroke"; JamPlay).
/// - Rock palm-mute chug: an open power chord, then three beats of
///   palm-muted eighths (NYC Guitar School, "An Intro to Punk Guitar").
/// - Pop punk: eight even eighths, all down-strokes (NYC Guitar School;
///   Riffhard, "How to Get a Punk Rock Guitar Sound").
/// - Shuffle: eighths swung to a triplet feel, backbeat accented.
/// - Country boom-chicka: Wikipedia, "Strum" (d dud du, Merle Haggard's
///   "Silver Wings"): bass on 1 and 3, chord down-up after.
/// - Bossa nova: João Gilberto's comping, the thumb on 1 and 3 under
///   syncopated chords, two bars (Jazzguitar.be, "Blue Bossa"; Anyone Can
///   Play Guitar, "Bossa Nova Essentials").
/// - Waltz: down on the numbers, up on the ands (Garret's Guitar Lessons,
///   "3/4 & 6/8 Rhythms").
/// - 6/8 strum: D – U D, D – U D (JustinGuitar's 6/8 pattern; Riffhard, "How
///   to Strum in 6/8"; Strumming.com).
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "Folk / pop",
        pattern: "D_du_uDu",
        resolution: Resolution::Eighth,
        swing: 0.0,
        gate: 1.0,
        upstroke_skip: 0,
        meter: (4, 4),
        about: "Old Faithful, the most common acoustic strum: D, DU, (miss) U, DU. \
                The chord rings through the missed down-stroke on 3.",
    },
    Preset {
        name: "Straight eighths",
        pattern: "DuDuDuDu",
        resolution: Resolution::Eighth,
        swing: 0.0,
        gate: 1.0,
        upstroke_skip: 0,
        meter: (4, 4),
        about: "Down on every beat, up on every \"and\": the hand's steady pendulum.",
    },
    Preset {
        name: "Driving sixteenths",
        pattern: "Dudu Dudu Dudu Dudu",
        resolution: Resolution::Sixteenth,
        swing: 0.0,
        gate: 0.9,
        upstroke_skip: 0,
        meter: (4, 4),
        about: "Every sixteenth: down on the beats and \"and\"s, up on the \"e\"s and \"a\"s.",
    },
    Preset {
        name: "Island strum",
        pattern: "D_Xu_uXu",
        resolution: Resolution::Eighth,
        swing: 0.0,
        gate: 1.0,
        upstroke_skip: 0,
        meter: (4, 4),
        about: "Old Faithful with a chuck on the backbeat (2 and 4), \
                as ukulele players play it.",
    },
    Preset {
        name: "Reggae skank",
        pattern: "--D---D-",
        resolution: Resolution::Eighth,
        swing: 0.0,
        gate: 0.3,
        upstroke_skip: 0,
        meter: (4, 4),
        about: "Short chops on 2 and 4, choked at once. For reggae tempos (70 to 90 BPM).",
    },
    Preset {
        name: "Ska upstrokes",
        pattern: "-U-U-U-U",
        resolution: Resolution::Eighth,
        swing: 0.0,
        gate: 0.3,
        upstroke_skip: 2,
        meter: (4, 4),
        about: "Clipped up-strokes on every off-beat, on the top strings.",
    },
    Preset {
        name: "Funk sixteenths",
        pattern: "DxxU xxDx xUxx DxDx",
        resolution: Resolution::Sixteenth,
        swing: 0.1,
        gate: 0.4,
        upstroke_skip: 0,
        meter: (4, 4),
        about: "Chicken scratch: staccato chord stabs among muted scratches, \
                the hand never stopping.",
    },
    Preset {
        name: "Rock palm-mute chug",
        pattern: "D_PpPpPp",
        resolution: Resolution::Eighth,
        swing: 0.0,
        gate: 0.9,
        upstroke_skip: 0,
        meter: (4, 4),
        about: "An open chord on 1, then three beats of palm-muted eighths.",
    },
    Preset {
        name: "Pop punk",
        pattern: "DDDDDDDD",
        resolution: Resolution::Eighth,
        swing: 0.0,
        gate: 0.8,
        upstroke_skip: 0,
        meter: (4, 4),
        about: "Eight even, hard eighths. Punk players down-pick them all: \
                set the Guitar's Strum Direction to Down for that.",
    },
    Preset {
        name: "Shuffle",
        pattern: "duDuduDu",
        resolution: Resolution::Eighth,
        swing: 0.33,
        gate: 0.9,
        upstroke_skip: 0,
        meter: (4, 4),
        about: "Eighths swung long-short, a triplet feel, with 2 and 4 accented.",
    },
    Preset {
        name: "Country boom-chicka",
        pattern: "B_cub_cu",
        resolution: Resolution::Eighth,
        swing: 0.0,
        gate: 0.9,
        upstroke_skip: 1,
        meter: (4, 4),
        about: "A bass note on 1 and 3 (boom), then the chord down and up (chick-a).",
    },
    Preset {
        name: "Bossa nova",
        pattern: "d__cb_c_ bc__d_c_",
        resolution: Resolution::Eighth,
        swing: 0.0,
        gate: 0.8,
        upstroke_skip: 0,
        meter: (4, 4),
        about: "Two bars of João Gilberto's comping: the thumb's bass on 1 and 3 \
                under syncopated chords.",
    },
    Preset {
        name: "Ballad",
        pattern: "D___d___",
        resolution: Resolution::Eighth,
        swing: 0.0,
        gate: 1.0,
        upstroke_skip: 0,
        meter: (4, 4),
        about: "Half notes left to ring, for slow songs.",
    },
    Preset {
        name: "Waltz (3/4)",
        pattern: "D_dudu",
        resolution: Resolution::Eighth,
        swing: 0.0,
        gate: 1.0,
        upstroke_skip: 0,
        meter: (3, 4),
        about: "A bar of 3/4: down on 1, 2 and 3, up on the \"and\"s of 2 and 3.",
    },
    Preset {
        name: "6/8 strum",
        pattern: "D__ud_ D__ud_",
        resolution: Resolution::Sixteenth,
        swing: 0.0,
        gate: 1.0,
        upstroke_skip: 0,
        meter: (6, 8),
        about: "A bar of 6/8, ONE-two-and-three FOUR-five-and-six: \
                the common 6/8 strum.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    const BAR: Tick = Tick::new(3840);

    fn note(id: u32, start: i64, len: i64, pitch: u8, velocity: u8) -> Note {
        Note {
            id: NoteId(id),
            start: Tick::new(start),
            len: Tick::new(len),
            pitch: Pitch::new(pitch).unwrap(),
            velocity: Velocity::from_midi1(velocity),
            release_velocity: Velocity::CENTER,
            muted: false,
        }
    }

    /// A chord of `pitches` at `start`, `len` long, velocity 100.
    fn chord(first_id: u32, start: i64, len: i64, pitches: &[u8]) -> Vec<Note> {
        pitches
            .iter()
            .enumerate()
            .map(|(i, &p)| note(first_id + i as u32, start, len, p, 100))
            .collect()
    }

    fn ids() -> impl FnMut() -> NoteId {
        let mut next = 1000;
        move || {
            next += 1;
            NoteId(next)
        }
    }

    fn settings(pattern: &str) -> StrumPattern {
        StrumPattern {
            pattern: Pattern::parse(pattern).unwrap(),
            resolution: Resolution::Eighth,
            swing: 0.0,
            gate: 1.0,
            accent: 1.0,
            normal: 0.5,
            upstroke_skip: 0,
            restart: false,
            humanize: 0.0,
            seed: 1,
        }
    }

    fn strum(notes: &[Note], s: &StrumPattern) -> Vec<Note> {
        strum_pattern(
            notes,
            &[],
            s,
            Articulation::Shortened,
            Tick::ZERO,
            BAR,
            &mut ids(),
        )
    }

    /// Each stroke as (start, length, pitches low to high).
    fn strokes(notes: &[Note]) -> Vec<(i64, i64, Vec<u8>)> {
        let mut out: Vec<(i64, i64, Vec<u8>)> = Vec::new();
        for n in notes {
            match out.last_mut() {
                Some(last) if last.0 == n.start.get() => last.2.push(n.pitch.get()),
                _ => out.push((n.start.get(), n.len.get(), vec![n.pitch.get()])),
            }
        }
        out
    }

    fn starts(notes: &[Note]) -> Vec<i64> {
        strokes(notes).iter().map(|s| s.0).collect()
    }

    #[test]
    fn the_syntax_reads_every_step_and_ignores_spacing() {
        let p = Pattern::parse("Dd Uu|Cc Bb Xx Pp _ -").unwrap();
        let hit = |hit, accent| Step::Hit { hit, accent };
        assert_eq!(
            p.steps(),
            [
                hit(Hit::Strum, true),
                hit(Hit::Strum, false),
                hit(Hit::Strum, true),
                hit(Hit::Strum, false),
                hit(Hit::Chord, true),
                hit(Hit::Chord, false),
                hit(Hit::Bass, true),
                hit(Hit::Bass, false),
                hit(Hit::Dead, true),
                hit(Hit::Dead, false),
                hit(Hit::PalmMute, true),
                hit(Hit::PalmMute, false),
                Step::Tie,
                Step::Rest,
            ]
        );
        assert_eq!(p.len(), 14);
        assert_eq!(p.to_string(), "Dd Uu|Cc Bb Xx Pp _ -", "kept as typed");
        assert_eq!("D-du".parse::<Pattern>().unwrap().len(), 4);
    }

    #[test]
    fn syntax_errors_say_what_and_where() {
        assert_eq!(Pattern::parse("  "), Err(PatternError::Empty));
        assert_eq!(
            Pattern::parse("D-q"),
            Err(PatternError::Unknown { found: 'q', at: 3 })
        );
        assert_eq!(
            Pattern::parse("D d Z"),
            Err(PatternError::Unknown { found: 'Z', at: 5 }),
            "spaces count as characters"
        );
        assert_eq!(Pattern::parse("-_-"), Err(PatternError::NoStrokes));
        assert_eq!(
            Pattern::parse("D-__"),
            Err(PatternError::LooseTie { at: 3 })
        );
        // Round the end: the first step holds the last.
        assert_eq!(Pattern::parse("_D-"), Err(PatternError::LooseTie { at: 1 }));
        assert!(Pattern::parse("_D_u").is_ok(), "holds the u round the end");
        let long = "d".repeat(MAX_STEPS + 1);
        assert_eq!(
            Pattern::parse(&long),
            Err(PatternError::TooLong { steps: 65 })
        );
        let message = PatternError::Unknown { found: 'q', at: 3 }.to_string();
        assert!(message.contains("'q'") && message.contains("character 3"));
    }

    #[test]
    fn directions_follow_the_hand() {
        use Direction::{Down as D, Up as U};
        let of = |text: &str, r: Resolution| -> Vec<Direction> {
            Pattern::parse(text)
                .unwrap()
                .directions(r)
                .into_iter()
                .flatten()
                .collect()
        };
        // Old Faithful: D DU UDU.
        assert_eq!(of("D_du_uDu", Resolution::Eighth), [D, D, U, U, D, U]);
        // Eighths written on a sixteenth grid: the hand moves in eighths...
        assert_eq!(of("D-D-D-D-", Resolution::Sixteenth), [D, U, D, U]);
        // ...until a stroke falls between them.
        assert_eq!(
            of("D-DU D-D-", Resolution::Sixteenth),
            [D, D, U, D, D],
            "the \"and\" goes down in sixteenths"
        );
        // 6/8: D - U D, D - U D.
        assert_eq!(
            of("D__ud_ D__ud_", Resolution::Sixteenth),
            [D, U, D, D, U, D]
        );
        // Triplets: down on the beat, up after it.
        assert_eq!(of("D-uD-u", Resolution::EighthTriplet), [D, U, D, U]);
        assert_eq!(
            Pattern::parse("D-").unwrap().directions(Resolution::Eighth),
            [Some(D), None]
        );
    }

    #[test]
    fn notes_starting_together_are_one_chord_on_the_grid() {
        // A chord whose notes land a little apart and a little late, and one
        // note of it longer: one chord, from the beat, as long as its longest.
        let notes = [
            note(1, 12, 900, 60, 100),
            note(2, 30, 960, 64, 100),
            note(3, 20, 940, 67, 100),
        ];
        let out = strum(&notes, &settings("Du"));
        assert_eq!(
            strokes(&out),
            [(0, 480, vec![60, 64, 67]), (480, 480, vec![60, 64, 67])]
        );
        // Notes a sixteenth apart start on the same eighth: still one chord.
        let notes = [note(1, 0, 960, 60, 100), note(2, 200, 760, 64, 100)];
        let out = strum(&notes, &settings("d"));
        assert_eq!(strokes(&out)[0].2, [60, 64]);
        // The same pitch twice plays once.
        let notes = [note(1, 0, 480, 60, 100), note(2, 0, 480, 60, 100)];
        assert_eq!(strum(&notes, &settings("d")).len(), 1);
    }

    #[test]
    fn a_chord_lasts_until_the_next_and_the_pattern_keeps_the_bars_time() {
        // C for a bar and a half, then G for half a bar.
        let mut notes = chord(1, 0, 5760, &[60, 64, 67]);
        notes.extend(chord(10, 5760, 1920, &[55, 59, 62]));
        let s = settings("D_du_uDu");
        let out = strum(&notes, &s);
        let c: Vec<i64> = strokes(&out)
            .iter()
            .filter(|s| s.2[0] == 60)
            .map(|s| s.0)
            .collect();
        let g: Vec<i64> = strokes(&out)
            .iter()
            .filter(|s| s.2[0] == 55)
            .map(|s| s.0)
            .collect();
        // Bar 1 whole, then the first half of bar 2 (D_du).
        assert_eq!(
            c,
            [0, 960, 1440, 2400, 2880, 3360, 3840, 4800, 5280],
            "a bar and a half"
        );
        // G picks the pattern up at beat 3: (miss) u D u.
        assert_eq!(g, [6240, 6720, 7200]);
        // The first stroke rings through its tie; the one before G stops
        // where G starts.
        assert_eq!(strokes(&out)[0].1, 960);
        assert_eq!(strokes(&out)[8].1, 480);

        // Restarting on each chord, G starts the pattern: D_du.
        let restart = StrumPattern { restart: true, ..s };
        let g: Vec<i64> = strokes(&strum(&notes, &restart))
            .iter()
            .filter(|s| s.2[0] == 55)
            .map(|s| s.0)
            .collect();
        assert_eq!(g, [5760, 6720, 7200]);
    }

    #[test]
    fn the_pattern_starts_on_the_bar_of_the_first_chord() {
        // A chord from beat 3 of bar 2, in a clip that starts at bar 2:
        // it picks up the second half of the pattern.
        let notes = chord(1, 1920, 1920, &[60, 64]);
        let out = strum_pattern(
            &notes,
            &[],
            &settings("D-------d-------"),
            Articulation::Shortened,
            BAR,
            BAR,
            &mut ids(),
        );
        // Steps 4.. of a two-bar pattern from bar 2: only the d at step 8.
        assert_eq!(starts(&out), Vec::<i64>::new(), "nothing in its half bar");
        let out = strum_pattern(
            &notes,
            &[],
            &settings("D---d---"),
            Articulation::Shortened,
            BAR,
            BAR,
            &mut ids(),
        );
        assert_eq!(starts(&out), [1920], "the d on beat 3");
    }

    #[test]
    fn swing_delays_the_off_beats() {
        let notes = chord(1, 0, 1920, &[60, 64]);
        let swung = StrumPattern {
            swing: 1.0 / 3.0,
            ..settings("dudu")
        };
        let out = strum(&notes, &swung);
        assert_eq!(starts(&out), [0, 640, 960, 1600]);
        // Long-short: each stroke rings up to the next.
        let lengths: Vec<i64> = strokes(&out).iter().map(|s| s.1).collect();
        assert_eq!(lengths, [640, 320, 640, 320]);
        // Sixteenths swing their e and a.
        let sixteenths = StrumPattern {
            resolution: Resolution::Sixteenth,
            swing: 0.5,
            ..settings("dudu")
        };
        assert_eq!(
            starts(&strum(&notes, &sixteenths)),
            [0, 360, 480, 840, 960, 1320, 1440, 1800]
        );
        // Triplets don't swing; swing is capped.
        let triplets = StrumPattern {
            resolution: Resolution::EighthTriplet,
            swing: 0.5,
            ..settings("duu")
        };
        assert_eq!(starts(&strum(&notes, &triplets))[..3], [0, 320, 640]);
        let wild = StrumPattern {
            swing: 2.0,
            ..settings("du")
        };
        assert_eq!(starts(&strum(&notes, &wild))[1], 480 + 360);
    }

    #[test]
    fn ties_rests_gate_and_velocities() {
        let notes = [note(1, 0, 1920, 60, 100), note(2, 0, 1920, 64, 80)];
        let s = StrumPattern {
            gate: 0.5,
            ..settings("D_-u")
        };
        let out = strum(&notes, &s);
        // D rings through its tie and half its last step; the rest is
        // silent; u is half a step.
        assert_eq!(
            strokes(&out),
            [(0, 720, vec![60, 64]), (1440, 240, vec![60, 64])]
        );
        // Accents at the notes' own velocity, the rest at half, each note
        // keeping its own.
        let v = |n: &Note| n.velocity.to_midi1();
        assert_eq!(v(&out[0]), 100);
        assert_eq!(v(&out[1]), 80);
        assert_eq!(v(&out[2]), 50);
        assert_eq!(v(&out[3]), 40);
        // Gate never reaches nothing.
        let tiny = StrumPattern {
            gate: 0.0,
            ..settings("d")
        };
        assert!(strum(&notes, &tiny).iter().all(|n| n.len.get() >= 24));
    }

    #[test]
    fn upstrokes_can_skip_the_bass() {
        let notes = chord(1, 0, 960, &[43, 47, 50, 55, 59, 67]);
        let s = StrumPattern {
            upstroke_skip: 2,
            ..settings("du")
        };
        let out = strokes(&strum(&notes, &s));
        assert_eq!(out[0].2, [43, 47, 50, 55, 59, 67], "down: all six");
        assert_eq!(out[1].2, [50, 55, 59, 67], "up: the top four");
        // A chord keeps two notes; a bass step is never thinned.
        let power = chord(1, 0, 960, &[40, 47, 52]);
        let out = strokes(&strum(&power, &s));
        assert_eq!(out[1].2, [47, 52]);
        let bass = StrumPattern {
            upstroke_skip: 2,
            ..settings("-b")
        };
        assert_eq!(strokes(&strum(&power, &bass))[0].2, [40]);
    }

    #[test]
    fn bass_and_chord_steps_split_the_voicing() {
        let notes = chord(1, 0, 960, &[48, 55, 60, 64]);
        let out = strokes(&strum(&notes, &settings("bc")));
        assert_eq!(out[0].2, [48]);
        assert_eq!(out[1].2, [55, 60, 64]);
    }

    #[test]
    fn off_the_guitar_dead_notes_and_palm_mutes_are_short_and_quiet() {
        let notes = chord(1, 0, 960, &[60, 64]);
        let out = strum(&notes, &settings("xp"));
        let shape: Vec<(i64, i64, u8)> = out
            .iter()
            .map(|n| (n.start.get(), n.len.get(), n.velocity.to_midi1()))
            .collect();
        // A quarter of a step at half the (normal) level; half a step at 0.8.
        assert_eq!(
            shape,
            [(0, 120, 25), (0, 120, 25), (480, 240, 40), (480, 240, 40)]
        );
        assert!(!out.iter().any(|n| n.pitch.get() < 24), "no key-switches");
    }

    #[test]
    fn on_the_guitar_key_switches_go_in_where_the_articulation_changes() {
        let notes = chord(1, 0, 3840, &[40, 47, 52]);
        let guitar = Articulation::KeySwitches { setting: OPEN };
        let s = settings("DxxD PpPd");
        let out = strum_pattern(&notes, &[], &s, guitar, Tick::ZERO, BAR, &mut ids());
        let switches: Vec<(i64, u8)> = out
            .iter()
            .filter(|n| n.pitch.get() < 24)
            .map(|n| (n.start.get(), n.pitch.get()))
            .collect();
        // Dead before the first x (one for both), open before D, palm
        // before P (one for three), open before d; nothing to restore.
        assert_eq!(switches, [(470, 15), (1430, 12), (1910, 13), (3350, 12)]);
        // The guitar plays them, so they keep the gate's length.
        let dead = out.iter().find(|n| n.start.get() == 480).unwrap();
        assert_eq!(dead.len.get(), 480);

        // Ending palm-muted, the clip goes back to open after the chord; a
        // guitar set to palm mute needs no switch for a first palm step.
        let s = settings("Pp");
        let palm = Articulation::KeySwitches { setting: PALM_MUTE };
        let out = strum_pattern(&notes, &[], &s, guitar, Tick::ZERO, BAR, &mut ids());
        let switches: Vec<(i64, u8)> = out
            .iter()
            .filter(|n| n.pitch.get() < 24)
            .map(|n| (n.start.get(), n.pitch.get()))
            .collect();
        assert_eq!(switches, [(0, 13), (3830, 12)], "not before the chord");
        let out = strum_pattern(&notes, &[], &s, palm, Tick::ZERO, BAR, &mut ids());
        assert!(!out.iter().any(|n| n.pitch.get() < 24));

        // Not for other instruments.
        let out = strum(&notes, &settings("xp"));
        assert!(!out.iter().any(|n| n.pitch.get() < 24));
    }

    #[test]
    fn key_switches_already_in_the_clip_are_followed_or_rewritten() {
        let mut notes = chord(1, 0, 960, &[40, 47]);
        // The user's palm mute before the chord, not selected: followed, so
        // a palm step needs no switch and an open one does.
        notes.push(note(9, 0, 30, KEY_SWITCH_FIRST + PALM_MUTE as u8, 100));
        let chord_ids = [NoteId(1), NoteId(2)];
        let guitar = Articulation::KeySwitches { setting: OPEN };
        let out = strum_pattern(
            &notes,
            &chord_ids,
            &settings("pd"),
            guitar,
            Tick::ZERO,
            BAR,
            &mut ids(),
        );
        let switches: Vec<(i64, u8)> = out
            .iter()
            .filter(|n| n.pitch.get() < 24)
            .map(|n| (n.start.get(), n.pitch.get()))
            .collect();
        // Theirs, open before the d, and palm again at the end as it was.
        assert_eq!(switches, [(0, 13), (470, 12), (950, 13)]);
        // Selected with the chord, it's the pattern's to rewrite.
        let out = strum_pattern(
            &notes,
            &[],
            &settings("pd"),
            guitar,
            Tick::ZERO,
            BAR,
            &mut ids(),
        );
        let switches: Vec<(i64, u8)> = out
            .iter()
            .filter(|n| n.pitch.get() < 24)
            .map(|n| (n.start.get(), n.pitch.get()))
            .collect();
        assert_eq!(switches, [(0, 13), (470, 12), (950, 13)]);
        assert!(!out.iter().any(|n| n.id == NoteId(9)), "replaced");
    }

    #[test]
    fn only_the_selected_chords_change() {
        let mut notes = chord(1, 0, 960, &[60, 64]);
        notes.push(note(5, 960, 480, 72, 90));
        let out = strum_pattern(
            &notes,
            &[NoteId(1), NoteId(2)],
            &settings("du"),
            Articulation::Shortened,
            Tick::ZERO,
            BAR,
            &mut ids(),
        );
        assert!(
            out.iter()
                .any(|n| n.id == NoteId(5) && n.start.get() == 960)
        );
        assert_eq!(out.len(), 5);
        assert!(
            !out.iter().any(|n| n.id == NoteId(1)),
            "the chord was replaced"
        );
    }

    #[test]
    fn humanize_varies_a_little_and_the_same_way_each_time() {
        let notes = chord(1, 0, 3840, &[60, 64, 67]);
        let s = StrumPattern {
            humanize: 1.0,
            ..settings("dudu dudu")
        };
        let a = strum(&notes, &s);
        assert_eq!(a, strum(&notes, &s), "repeatable");
        assert_ne!(
            a,
            strum(
                &notes,
                &StrumPattern {
                    seed: 9,
                    ..s.clone()
                }
            )
        );
        let exact = strum(
            &notes,
            &StrumPattern {
                humanize: 0.0,
                ..s.clone()
            },
        );
        let mut moved = false;
        for (h, e) in strokes(&a).iter().zip(strokes(&exact)) {
            assert!((h.0 - e.0).abs() <= 20, "{h:?} {e:?}");
            moved |= h.0 != e.0;
        }
        assert!(moved);
        assert!(starts(&a)[0] >= 0, "never before the chord");
        // A stroke still stops where the next starts.
        for pair in strokes(&a).windows(2) {
            assert!(pair[0].0 + pair[0].1 <= pair[1].0);
        }
    }

    #[test]
    fn the_guitar_articulations_are_where_the_tool_expects() {
        assert_eq!(ARTICULATIONS[OPEN], "Open");
        assert_eq!(ARTICULATIONS[PALM_MUTE], "Palm mute");
        assert_eq!(ARTICULATIONS[DEAD], "Dead notes");
    }

    #[test]
    fn presets_are_valid_and_a_whole_bar_of_their_meter() {
        let mut names = std::collections::HashSet::new();
        for preset in PRESETS {
            assert!(names.insert(preset.name), "{} twice", preset.name);
            let pattern =
                Pattern::parse(preset.pattern).unwrap_or_else(|e| panic!("{}: {e}", preset.name));
            let (beats, value) = preset.meter;
            let bar = PPQ * 4 / i64::from(value) * i64::from(beats);
            let ticks = pattern.len() as i64 * preset.resolution.step().get();
            assert!(
                ticks % bar == 0 && ticks <= 2 * bar,
                "{}: {ticks} ticks against a bar of {bar}",
                preset.name
            );
            assert!((0.0..=MAX_SWING).contains(&preset.swing));
            assert!((MIN_GATE..=1.0).contains(&preset.gate));
            assert!(preset.upstroke_skip <= 2);
            assert!(!preset.about.is_empty());
            let mut s = StrumPattern::default();
            preset.apply(&mut s);
            assert_eq!(s.preset().map(|p| p.name), Some(preset.name));
        }
        assert!((12..=16).contains(&PRESETS.len()));
        assert_eq!(StrumPattern::default().preset(), Some(&PRESETS[0]));
    }
}
