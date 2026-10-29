//! Piano roll tools: quantizing, naturalizing, chopping, arpeggiating,
//! strumming and the rest, plus the chords that note entry can stamp.
//! Strum patterns, held chords played in a rhythm, are in [`patterns`].
//!
//! Each tool takes a clip's notes and the ids of the ones to change (all of
//! them if `ids` is empty) and returns the clip's new notes. Positions are
//! the notes' own (from the clip's content start); `origin` is where that
//! is on the timeline, so grids line up with the song's bars.

pub mod patterns;

use midi::{Pitch, Tick, Velocity};

use crate::{BendPoint, Note, NoteId, Scale};

pub use patterns::strum_pattern;

/// Notes starting this close together count as one chord.
const CHORD_TOLERANCE: i64 = 30;

/// The notes to work on and the rest, by `ids` (everything if empty).
fn split(notes: &[Note], ids: &[NoteId]) -> (Vec<Note>, Vec<Note>) {
    notes
        .iter()
        .cloned()
        .partition(|n| ids.is_empty() || ids.contains(&n.id))
}

fn join(mut rest: Vec<Note>, changed: Vec<Note>) -> Vec<Note> {
    rest.extend(changed);
    rest.sort_by_key(|n| (n.start, n.pitch));
    rest
}

/// Notes grouped into chords: those starting within a few ticks of the
/// group's first note. Sorted by start.
fn chords(mut notes: Vec<Note>) -> Vec<Vec<Note>> {
    notes.sort_by_key(|n| (n.start, n.pitch));
    let mut groups: Vec<Vec<Note>> = Vec::new();
    for note in notes {
        match groups.last_mut() {
            Some(group) if (note.start - group[0].start).get() <= CHORD_TOLERANCE => {
                group.push(note);
            }
            _ => groups.push(vec![note]),
        }
    }
    groups
}

/// A small, seeded random number generator, so a seed gives the same result
/// every time (the preview and the applied edit agree).
struct Random(u64);

impl Random {
    fn new(seed: u64) -> Random {
        Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// Uniform in `-1..1`.
    fn signed(&mut self) -> f64 {
        (dsp::noise::xorshift64(&mut self.0) >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    fn below(&mut self, n: usize) -> usize {
        ((self.signed() + 1.0) / 2.0 * n as f64) as usize % n.max(1)
    }
}

fn snap(tick: Tick, grid: Tick, origin: Tick) -> Tick {
    let grid = grid.get().max(1) as f64;
    let at = (origin + tick).get() as f64;
    Tick::new(((at / grid).round() * grid) as i64) - origin
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quantize {
    pub grid: Tick,
    /// How far to move, `0..=1`: 1 lands on the grid.
    pub strength: f32,
    /// Move the starts (keeping lengths).
    pub starts: bool,
    /// Move the ends too.
    pub ends: bool,
}

impl Default for Quantize {
    fn default() -> Self {
        Quantize {
            grid: Tick::new(240),
            strength: 1.0,
            starts: true,
            ends: false,
        }
    }
}

pub fn quantize(notes: &[Note], ids: &[NoteId], q: Quantize, origin: Tick) -> Vec<Note> {
    let (selected, rest) = split(notes, ids);
    let toward = |from: Tick, to: Tick| {
        from + Tick::new(
            ((to - from).get() as f64 * f64::from(q.strength.clamp(0.0, 1.0))).round() as i64,
        )
    };
    let changed = selected
        .into_iter()
        .map(|mut n| {
            let end = n.end();
            if q.starts {
                n.start = toward(n.start, snap(n.start, q.grid, origin));
            }
            n.len = if q.ends {
                // An end that would land on the start gets a whole step.
                let mut target = snap(end, q.grid, origin);
                if target <= n.start {
                    target = n.start + q.grid;
                }
                (toward(end, target) - n.start).max(Tick::new(1))
            } else {
                n.len
            };
            n
        })
        .collect();
    join(rest, changed)
}

/// Random nudges, so programmed parts sound played.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Naturalize {
    /// The most a start moves, either way.
    pub timing: Tick,
    /// The most a velocity changes, as a fraction of itself, either way.
    pub velocity: f32,
    /// The most a length changes, as a fraction of itself, either way.
    pub length: f32,
    pub seed: u64,
}

impl Default for Naturalize {
    fn default() -> Self {
        Naturalize {
            timing: Tick::new(15),
            velocity: 0.15,
            length: 0.1,
            seed: 1,
        }
    }
}

pub fn naturalize(notes: &[Note], ids: &[NoteId], n: Naturalize) -> Vec<Note> {
    let (selected, rest) = split(notes, ids);
    let mut random = Random::new(n.seed);
    let changed = selected
        .into_iter()
        .map(|mut note| {
            let shift = (random.signed() * n.timing.get() as f64).round() as i64;
            let start = note.start + Tick::new(shift);
            // Notes inside the clip stay inside it.
            note.start = if note.start >= Tick::ZERO {
                start.max(Tick::ZERO)
            } else {
                start
            };
            let velocity = note.velocity.as_f32() * (1.0 + n.velocity * random.signed() as f32);
            note.velocity = Velocity::new(
                (velocity.clamp(1.0 / 127.0, 1.0) * f32::from(u16::MAX)).round() as u16,
            );
            let len = note.len.get() as f64 * (1.0 + f64::from(n.length) * random.signed());
            note.len = Tick::new(len.round() as i64).max(Tick::new(1));
            note
        })
        .collect();
    join(rest, changed)
}

/// Cuts every note at each grid line inside it.
pub fn chop(
    notes: &[Note],
    ids: &[NoteId],
    step: Tick,
    origin: Tick,
    new_id: &mut dyn FnMut() -> NoteId,
) -> Vec<Note> {
    let (selected, rest) = split(notes, ids);
    let step = step.get().max(1);
    let mut changed = Vec::new();
    for note in selected {
        let (start, end) = ((origin + note.start).get(), (origin + note.end()).get());
        let mut cuts: Vec<i64> = Vec::new();
        let mut line = (start.div_euclid(step) + 1) * step;
        while line < end && cuts.len() < 4096 {
            cuts.push(line);
            line += step;
        }
        let mut from = start;
        for (i, to) in cuts.into_iter().chain([end]).enumerate() {
            let (offset, len) = (Tick::new(from - start), Tick::new(to - from));
            changed.push(Note {
                id: if i == 0 { note.id } else { new_id() },
                start: Tick::new(from) - origin,
                len,
                bend: bend_between(&note, offset, len),
                ..note.clone()
            });
            from = to;
        }
    }
    join(rest, changed)
}

/// The part of `note`'s bend from `offset` (from its start) for `len`, as
/// a note starting there would have it: its points in that span, moved to
/// the part's start, with a point at each cut at the value the curve had
/// there, so the parts together bend as the whole did. (The first part
/// keeps its leading points as they are: before its first point a note
/// isn't bent yet.)
fn bend_between(note: &Note, offset: Tick, len: Tick) -> Vec<BendPoint> {
    if note.bend.is_empty() {
        return Vec::new();
    }
    let end = offset + len;
    let cut = |at: Tick, semitones: f32| BendPoint {
        at,
        semitones,
        curve: 0.0,
    };
    let mut points: Vec<BendPoint> = Vec::new();
    if offset > Tick::ZERO {
        points.push(cut(Tick::ZERO, note.bend_at(offset)));
    }
    points.extend(
        note.bend
            .iter()
            .filter(|p| p.at >= offset && p.at <= end)
            .filter(|p| offset == Tick::ZERO || p.at > offset)
            .map(|p| BendPoint {
                at: p.at - offset,
                ..*p
            }),
    );
    if note.bend.iter().any(|p| p.at > end) {
        points.push(cut(len, note.bend_at(end)));
    }
    points
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArpPattern {
    #[default]
    Up,
    Down,
    UpDown,
    Random,
}

impl ArpPattern {
    pub const ALL: [ArpPattern; 4] = [
        ArpPattern::Up,
        ArpPattern::Down,
        ArpPattern::UpDown,
        ArpPattern::Random,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ArpPattern::Up => "Up",
            ArpPattern::Down => "Down",
            ArpPattern::UpDown => "Up and down",
            ArpPattern::Random => "Random",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arpeggio {
    /// One note every `step`.
    pub step: Tick,
    pub pattern: ArpPattern,
    /// How much of each step a note lasts, `0..=1`.
    pub gate: f32,
    /// How many octaves the chord climbs through.
    pub octaves: u8,
    pub seed: u64,
}

impl Default for Arpeggio {
    fn default() -> Self {
        Arpeggio {
            step: Tick::new(240),
            pattern: ArpPattern::Up,
            gate: 0.9,
            octaves: 1,
            seed: 1,
        }
    }
}

/// Plays each chord's notes one after another, for as long as the chord
/// lasts.
pub fn arpeggiate(
    notes: &[Note],
    ids: &[NoteId],
    a: Arpeggio,
    new_id: &mut dyn FnMut() -> NoteId,
) -> Vec<Note> {
    let (selected, rest) = split(notes, ids);
    let step = a.step.max(Tick::new(1));
    let len = Tick::new(((step.get() as f32 * a.gate.clamp(0.05, 1.0)).round() as i64).max(1));
    let mut random = Random::new(a.seed);
    let mut changed = Vec::new();
    for chord in chords(selected) {
        let start = chord[0].start;
        let end = chord.iter().map(Note::end).max().unwrap_or(start);
        let mut voices: Vec<&Note> = chord.iter().collect();
        voices.sort_by_key(|n| n.pitch);
        // Every note in every octave, low to high, with the note it came from.
        let mut up: Vec<(Pitch, &Note)> = Vec::new();
        for octave in 0..a.octaves.clamp(1, 4) {
            for voice in &voices {
                if let Some(pitch) = voice
                    .pitch
                    .get()
                    .checked_add(12 * octave)
                    .and_then(Pitch::new)
                {
                    up.push((pitch, voice));
                }
            }
        }
        let sequence: Vec<(Pitch, &Note)> = match a.pattern {
            ArpPattern::Up | ArpPattern::Random => up.clone(),
            ArpPattern::Down => up.iter().rev().cloned().collect(),
            ArpPattern::UpDown => {
                let mut both = up.clone();
                if up.len() > 2 {
                    both.extend(up[1..up.len() - 1].iter().rev().cloned());
                } else if up.len() == 2 {
                    both = up.clone();
                }
                both
            }
        };
        if sequence.is_empty() {
            continue;
        }
        let mut at = start;
        let mut k = 0;
        // A step plays if at least half a note fits: chord notes starting a
        // few ticks late don't leave a sliver at the end.
        while at + Tick::new(len.get() / 2) < end && k < 4096 {
            let index = if a.pattern == ArpPattern::Random {
                random.below(sequence.len())
            } else {
                k % sequence.len()
            };
            let (pitch, from) = sequence[index];
            changed.push(Note {
                id: new_id(),
                start: at,
                len: len.min(end - at),
                pitch,
                ..from.clone()
            });
            at += step;
            k += 1;
        }
    }
    join(rest, changed)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Strum {
    /// How much later each next note of a chord starts.
    pub delay: Tick,
    /// Lowest note first, or highest.
    pub upward: bool,
}

impl Default for Strum {
    fn default() -> Self {
        Strum {
            delay: Tick::new(20),
            upward: true,
        }
    }
}

/// Spreads each chord's notes out in time, like a strummed guitar. Ends stay
/// where they were.
pub fn strum(notes: &[Note], ids: &[NoteId], s: Strum) -> Vec<Note> {
    let (selected, rest) = split(notes, ids);
    let mut changed = Vec::new();
    for mut chord in chords(selected) {
        chord.sort_by_key(|n| n.pitch);
        if !s.upward {
            chord.reverse();
        }
        let start = chord.iter().map(|n| n.start).min().unwrap_or(Tick::ZERO);
        for (i, mut note) in chord.into_iter().enumerate() {
            let end = note.end();
            note.start = (start + s.delay * i as i64).min(end - Tick::new(1));
            note.len = end - note.start;
            changed.push(note);
        }
    }
    join(rest, changed)
}

/// Stretches every note to where the next one (of the ones worked on)
/// starts; the last chord keeps its length.
pub fn legato(notes: &[Note], ids: &[NoteId]) -> Vec<Note> {
    let (selected, rest) = split(notes, ids);
    let groups = chords(selected);
    let starts: Vec<Tick> = groups.iter().map(|g| g[0].start).collect();
    let mut changed = Vec::new();
    for (i, group) in groups.into_iter().enumerate() {
        for mut note in group {
            if let Some(&next) = starts.get(i + 1) {
                note.len = (next - note.start).max(Tick::new(1));
            }
            changed.push(note);
        }
    }
    join(rest, changed)
}

/// Joins notes of the same pitch that touch or overlap into one.
pub fn glue(notes: &[Note], ids: &[NoteId]) -> Vec<Note> {
    let (mut selected, rest) = split(notes, ids);
    selected.sort_by_key(|n| (n.pitch, n.start));
    let mut changed: Vec<Note> = Vec::new();
    for note in selected {
        match changed.last_mut() {
            Some(last) if last.pitch == note.pitch && note.start <= last.end() => {
                join_bends(last, &note);
                last.len = last.end().max(note.end()) - last.start;
            }
            _ => changed.push(note),
        }
    }
    join(rest, changed)
}

/// Gives `first` (about to be lengthened over `next`) `next`'s bend after
/// its own: `first`'s curve up to where `next` starts, held there, then
/// `next`'s from its own starting value.
fn join_bends(first: &mut Note, next: &Note) {
    if first.bend.is_empty() && next.bend.is_empty() {
        return;
    }
    let join = next.start - first.start;
    let held = first.bend_at(join);
    first.bend.retain(|p| p.at < join);
    for semitones in [held, next.bend_at(Tick::ZERO)] {
        first.bend.push(BendPoint {
            at: join,
            semitones,
            curve: 0.0,
        });
    }
    first.bend.extend(next.bend.iter().map(|p| BendPoint {
        at: p.at + join,
        ..*p
    }));
}

/// Moves every note to the nearest note of `scale`.
pub fn fit_to_scale(notes: &[Note], ids: &[NoteId], scale: Scale) -> Vec<Note> {
    let (selected, rest) = split(notes, ids);
    let changed = selected
        .into_iter()
        .map(|mut n| {
            n.pitch = scale.snap(n.pitch);
            n
        })
        .collect();
    join(rest, changed)
}

/// Plays the notes backwards: mirrored in time across the span they cover.
pub fn reverse(notes: &[Note], ids: &[NoteId]) -> Vec<Note> {
    let (selected, rest) = split(notes, ids);
    let (Some(first), Some(last)) = (
        selected.iter().map(|n| n.start).min(),
        selected.iter().map(Note::end).max(),
    ) else {
        return notes.to_vec();
    };
    let changed = selected
        .into_iter()
        .map(|mut n| {
            n.start = first + (last - n.end());
            // Its curve plays backwards too: each point mirrored across the
            // note, curves turned to match.
            let len = n.len;
            n.bend = n
                .bend
                .iter()
                .rev()
                .map(|p| BendPoint {
                    at: len - p.at,
                    curve: -p.curve,
                    ..*p
                })
                .collect();
            n
        })
        .collect();
    join(rest, changed)
}

/// Turns the notes upside down: the highest becomes the lowest.
pub fn flip(notes: &[Note], ids: &[NoteId]) -> Vec<Note> {
    let (selected, rest) = split(notes, ids);
    let (Some(low), Some(high)) = (
        selected.iter().map(|n| n.pitch.get()).min(),
        selected.iter().map(|n| n.pitch.get()).max(),
    ) else {
        return notes.to_vec();
    };
    let changed = selected
        .into_iter()
        .map(|mut n| {
            n.pitch = Pitch::new(low + high - n.pitch.get()).unwrap_or(n.pitch);
            // Upside down, a rise falls.
            for point in &mut n.bend {
                point.semitones = -point.semitones;
            }
            n
        })
        .collect();
    join(rest, changed)
}

/// What a click in the piano roll writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChordKind {
    /// Just the note.
    #[default]
    Single,
    Triad,
    Seventh,
    Ninth,
    Sus2,
    Sus4,
    Power,
    Octave,
}

impl ChordKind {
    pub const ALL: [ChordKind; 8] = [
        ChordKind::Single,
        ChordKind::Triad,
        ChordKind::Seventh,
        ChordKind::Ninth,
        ChordKind::Sus2,
        ChordKind::Sus4,
        ChordKind::Power,
        ChordKind::Octave,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ChordKind::Single => "Note",
            ChordKind::Triad => "Triad",
            ChordKind::Seventh => "7th",
            ChordKind::Ninth => "9th",
            ChordKind::Sus2 => "Sus2",
            ChordKind::Sus4 => "Sus4",
            ChordKind::Power => "Power",
            ChordKind::Octave => "Octave",
        }
    }

    /// Scale degrees above the root.
    fn degrees(self) -> &'static [i32] {
        match self {
            ChordKind::Single => &[0],
            ChordKind::Triad => &[0, 2, 4],
            ChordKind::Seventh => &[0, 2, 4, 6],
            ChordKind::Ninth => &[0, 2, 4, 6, 8],
            ChordKind::Sus2 => &[0, 1, 4],
            ChordKind::Sus4 => &[0, 3, 4],
            ChordKind::Power => &[0, 4],
            ChordKind::Octave => &[0],
        }
    }
}

/// The pitches of a `kind` chord on `root`, built from `scale`'s own notes
/// (C major's without one), so it always fits the key. The root moves to
/// the nearest scale note first.
pub fn chord(root: Pitch, kind: ChordKind, scale: Option<Scale>) -> Vec<Pitch> {
    let scale = scale.unwrap_or(Scale::new(0, crate::ScaleMode::Major));
    let root = scale.snap(root);
    let mut pitches: Vec<Pitch> = kind
        .degrees()
        .iter()
        .filter_map(|&d| scale.step(root, d))
        .collect();
    if kind == ChordKind::Octave
        && let Some(up) = Pitch::new(root.get() + 12).filter(|_| root.get() <= 115)
    {
        pitches.push(up);
    }
    pitches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ScaleMode;

    fn note(id: u32, start: i64, len: i64, pitch: u8) -> Note {
        Note {
            id: NoteId(id),
            start: Tick::new(start),
            len: Tick::new(len),
            pitch: Pitch::new(pitch).unwrap(),
            velocity: Velocity::from_midi1(100),
            release_velocity: Velocity::CENTER,
            muted: false,
            bend: Vec::new(),
        }
    }

    fn bent(id: u32, start: i64, len: i64, points: &[(i64, f32)]) -> Note {
        Note {
            bend: points
                .iter()
                .map(|&(at, semitones)| crate::BendPoint {
                    at: Tick::new(at),
                    semitones,
                    curve: 0.0,
                })
                .collect(),
            ..note(id, start, len, 60)
        }
    }

    fn points(note: &Note) -> Vec<(i64, f32)> {
        note.bend.iter().map(|p| (p.at.get(), p.semitones)).collect()
    }

    #[test]
    fn chopping_a_bent_note_splits_its_bend() {
        // A rise from 0 to 4 semitones over 960 ticks, chopped at 480.
        let notes = [bent(1, 0, 960, &[(0, 0.0), (960, 4.0)])];
        let out = chop(&notes, &[], Tick::new(480), Tick::ZERO, &mut ids());
        assert_eq!(spans(&out), [(0, 480, 60), (480, 480, 60)]);
        assert_eq!(points(&out[0]), [(0, 0.0), (480, 2.0)]);
        assert_eq!(points(&out[1]), [(0, 2.0), (480, 4.0)]);
        // Each half bends as that part of the whole did.
        assert_eq!(out[0].bend_at(Tick::new(240)), 1.0);
        assert_eq!(out[1].bend_at(Tick::new(240)), 3.0);
    }

    #[test]
    fn gluing_joins_bends() {
        let notes = [
            bent(1, 0, 480, &[(0, 0.0), (480, 2.0)]),
            bent(2, 480, 480, &[(240, -1.0)]),
        ];
        let out = glue(&notes, &[]);
        assert_eq!(spans(&out), [(0, 960, 60)]);
        // The first's curve, its value held up to the join, then the
        // second's points after it.
        assert_eq!(points(&out[0]), [(0, 0.0), (480, 2.0), (480, 0.0), (720, -1.0)]);
    }

    #[test]
    fn reversing_mirrors_a_bend() {
        let notes = [bent(1, 0, 960, &[(0, 0.0), (240, 3.0)])];
        let out = reverse(&notes, &[]);
        assert_eq!(points(&out[0]), [(720, 3.0), (960, 0.0)]);
    }

    #[test]
    fn flipping_negates_a_bend() {
        let notes = [bent(1, 0, 960, &[(0, 0.0), (480, 2.0)])];
        let out = flip(&notes, &[]);
        assert_eq!(points(&out[0]), [(0, 0.0), (480, -2.0)]);
    }

    fn spans(notes: &[Note]) -> Vec<(i64, i64, u8)> {
        notes
            .iter()
            .map(|n| (n.start.get(), n.len.get(), n.pitch.get()))
            .collect()
    }

    fn ids() -> impl FnMut() -> NoteId {
        let mut next = 100;
        move || {
            next += 1;
            NoteId(next)
        }
    }

    #[test]
    fn quantize_moves_starts_by_strength_and_respects_the_origin() {
        let notes = [note(1, 250, 100, 60), note(2, 470, 100, 62)];
        let q = Quantize::default();
        assert_eq!(
            spans(&quantize(&notes, &[], q, Tick::ZERO)),
            [(240, 100, 60), (480, 100, 62)]
        );
        let half = Quantize { strength: 0.5, ..q };
        assert_eq!(
            spans(&quantize(&notes, &[], half, Tick::ZERO)),
            [(245, 100, 60), (475, 100, 62)]
        );
        // A clip starting 10 ticks after a grid line: grid lines are at 230, 470.
        assert_eq!(
            spans(&quantize(&notes, &[], q, Tick::new(10))),
            [(230, 100, 60), (470, 100, 62)]
        );
        // Only the selected one.
        assert_eq!(
            spans(&quantize(&notes, &[NoteId(2)], q, Tick::ZERO)),
            [(250, 100, 60), (480, 100, 62)]
        );
        // Ends too: never to nothing.
        let ends = Quantize { ends: true, ..q };
        assert_eq!(
            spans(&quantize(&[note(1, 250, 50, 60)], &[], ends, Tick::ZERO)),
            [(240, 240, 60)]
        );
    }

    #[test]
    fn naturalize_stays_close_and_repeats_with_the_seed() {
        let notes: Vec<Note> = (0..16).map(|i| note(i, i as i64 * 240, 200, 60)).collect();
        let n = Naturalize::default();
        let a = naturalize(&notes, &[], n);
        assert_eq!(a, naturalize(&notes, &[], n));
        assert_ne!(a, naturalize(&notes, &[], Naturalize { seed: 2, ..n }));
        for before in &notes {
            let moved = a.iter().find(|y| y.id == before.id).unwrap();
            assert!((moved.start - before.start).get().abs() <= 15);
            assert!(moved.start >= Tick::ZERO);
            assert!((moved.len.get() - 200).abs() <= 20);
        }
        assert!(a.iter().any(|x| x.velocity != notes[0].velocity));
    }

    #[test]
    fn chop_cuts_at_grid_lines() {
        let mut new_id = ids();
        let chopped = chop(
            &[note(1, 100, 500, 60)],
            &[],
            Tick::new(240),
            Tick::ZERO,
            &mut new_id,
        );
        assert_eq!(
            spans(&chopped),
            [(100, 140, 60), (240, 240, 60), (480, 120, 60)]
        );
        assert_eq!(
            chopped[0].id,
            NoteId(1),
            "the first piece is the note itself"
        );
        assert_ne!(chopped[1].id, chopped[2].id);
    }

    #[test]
    fn arpeggios_walk_the_chord() {
        let chord = [
            note(1, 0, 960, 60),
            note(2, 5, 960, 64),
            note(3, 0, 960, 67),
        ];
        let a = Arpeggio::default();
        let up = arpeggiate(&chord, &[], a, &mut ids());
        assert_eq!(
            spans(&up),
            [(0, 216, 60), (240, 216, 64), (480, 216, 67), (720, 216, 60)]
        );
        let down = arpeggiate(
            &chord,
            &[],
            Arpeggio {
                pattern: ArpPattern::Down,
                ..a
            },
            &mut ids(),
        );
        assert_eq!(
            down.iter().map(|n| n.pitch.get()).collect::<Vec<_>>(),
            [67, 64, 60, 67]
        );
        let two_octaves = Arpeggio {
            pattern: ArpPattern::UpDown,
            octaves: 2,
            step: Tick::new(120),
            ..a
        };
        let pitches: Vec<u8> = arpeggiate(&chord, &[], two_octaves, &mut ids())
            .iter()
            .map(|n| n.pitch.get())
            .collect();
        assert_eq!(pitches, [60, 64, 67, 72, 76, 79, 76, 72]);
        let random = Arpeggio {
            pattern: ArpPattern::Random,
            ..a
        };
        let r = arpeggiate(&chord, &[], random, &mut ids());
        assert!(r.iter().all(|n| [60, 64, 67].contains(&n.pitch.get())));
    }

    #[test]
    fn strum_legato_glue_reverse_flip() {
        let chord = [
            note(1, 0, 480, 60),
            note(2, 0, 480, 64),
            note(3, 0, 480, 67),
        ];
        let strummed = strum(&chord, &[], Strum::default());
        assert_eq!(
            spans(&strummed),
            [(0, 480, 60), (20, 460, 64), (40, 440, 67)]
        );
        let down = strum(
            &chord,
            &[],
            Strum {
                upward: false,
                ..Strum::default()
            },
        );
        assert_eq!(
            down.iter().find(|n| n.pitch.get() == 67).unwrap().start,
            Tick::ZERO
        );

        let gaps = [
            note(1, 0, 100, 60),
            note(2, 480, 100, 62),
            note(3, 960, 100, 64),
        ];
        assert_eq!(
            spans(&legato(&gaps, &[])),
            [(0, 480, 60), (480, 480, 62), (960, 100, 64)]
        );

        let pieces = [
            note(1, 0, 240, 60),
            note(2, 240, 240, 60),
            note(3, 600, 100, 60),
            note(4, 100, 100, 62),
        ];
        assert_eq!(
            spans(&glue(&pieces, &[])),
            [(0, 480, 60), (100, 100, 62), (600, 100, 60)]
        );

        let line = [note(1, 0, 100, 60), note(2, 200, 300, 62)];
        assert_eq!(spans(&reverse(&line, &[])), [(0, 300, 62), (400, 100, 60)]);
        assert_eq!(spans(&flip(&line, &[])), [(0, 100, 62), (200, 300, 60)]);
    }

    #[test]
    fn chords_fit_the_scale() {
        let p = |n: u8| Pitch::new(n).unwrap();
        let names = |pitches: Vec<Pitch>| pitches.iter().map(|x| x.get()).collect::<Vec<_>>();
        // C major: on D, a triad is D F A (minor), on G a 7th is G B D F.
        assert_eq!(names(chord(p(62), ChordKind::Triad, None)), [62, 65, 69]);
        assert_eq!(
            names(chord(p(67), ChordKind::Seventh, None)),
            [67, 71, 74, 77]
        );
        // A minor: on C#, the root moves to C (ties go down), giving C E G.
        let a_minor = Some(Scale::new(9, ScaleMode::Minor));
        assert_eq!(names(chord(p(61), ChordKind::Triad, a_minor)), [60, 64, 67]);
        assert_eq!(names(chord(p(69), ChordKind::Sus4, a_minor)), [69, 74, 76]);
        assert_eq!(names(chord(p(60), ChordKind::Octave, None)), [60, 72]);
        assert_eq!(names(chord(p(60), ChordKind::Single, None)), [60]);
    }

    #[test]
    fn fit_to_scale_moves_notes_in() {
        let notes = [note(1, 0, 100, 61), note(2, 0, 100, 66)];
        let fitted = fit_to_scale(&notes, &[], Scale::new(0, ScaleMode::Major));
        assert_eq!(
            fitted.iter().map(|n| n.pitch.get()).collect::<Vec<_>>(),
            [60, 65]
        );
    }
}
