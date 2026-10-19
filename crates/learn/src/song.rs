//! Songs: a MIDI file's notes in seconds, which hand plays each, and the
//! player that runs a song in Wait, Play-along or Listen mode.
//!
//! The player is driven with clock times (seconds) and returns the notes
//! it plays itself; it knows nothing of audio or drawing.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::grading::{Rating, Timing, timing_error_ms};
use crate::smf::{self, EventKind, Smf, TempoMap, Timing as SmfTiming};
use crate::theory::Hand;

#[derive(Debug, Clone, PartialEq)]
pub struct SongNote {
    pub start: f64,
    pub end: f64,
    pub pitch: u8,
    pub velocity: u8,
    /// Index into [`Song::parts`].
    pub part: usize,
}

/// A track (or, in a one-track file, a channel) of the song.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub name: String,
    pub notes: usize,
    pub mean_pitch: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Song {
    pub title: String,
    /// Sorted by start, then pitch.
    pub notes: Vec<SongNote>,
    pub parts: Vec<Part>,
    /// When each bar starts, and each beat.
    pub bars: Vec<f64>,
    pub beats: Vec<f64>,
    /// When the last note ends.
    pub duration: f64,
    pub bpm: f64,
}

/// Notes shorter than this are stretched to it, so they can be seen.
const MIN_NOTE: f64 = 0.05;
/// MIDI channel 10, which General MIDI keeps for drums.
const DRUM_CHANNEL: u8 = 9;

impl Song {
    pub fn load(path: &Path) -> Result<Song, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let smf = smf::parse(&bytes).map_err(|e| e.to_string())?;
        let title = path.file_stem().map_or_else(
            || "Untitled".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        );
        Song::from_smf(&smf, &title)
    }

    pub fn from_smf(smf: &Smf, title: &str) -> Result<Song, String> {
        let map = TempoMap::new(smf);
        // (track, channel, key, start tick, end tick, velocity)
        let mut raw: Vec<(usize, u8, u8, u64, u64, u8)> = Vec::new();
        for (index, track) in smf.tracks.iter().enumerate() {
            let mut open: HashMap<(u8, u8), Vec<(u64, u8)>> = HashMap::new();
            let mut last = 0;
            for event in &track.events {
                last = event.tick;
                match event.kind {
                    EventKind::NoteOn {
                        channel,
                        key,
                        velocity,
                    } if channel != DRUM_CHANNEL => {
                        open.entry((channel, key))
                            .or_default()
                            .push((event.tick, velocity));
                    }
                    EventKind::NoteOff { channel, key } => {
                        // Overlapping notes on one key end first in, first out.
                        if let Some(stack) = open.get_mut(&(channel, key))
                            && !stack.is_empty()
                        {
                            let (start, velocity) = stack.remove(0);
                            raw.push((index, channel, key, start, event.tick, velocity));
                        }
                    }
                    _ => {}
                }
            }
            for ((channel, key), stack) in open {
                for (start, velocity) in stack {
                    raw.push((index, channel, key, start, last.max(start), velocity));
                }
            }
        }
        if raw.is_empty() {
            return Err("the file has no notes".into());
        }

        // Parts are tracks, unless all the notes are on one track: then
        // they're its channels (format 0 files keep hands apart that way).
        let mut tracks: Vec<usize> = raw.iter().map(|n| n.0).collect();
        tracks.sort_unstable();
        tracks.dedup();
        let by_channel = tracks.len() == 1;
        let key_of =
            |n: &(usize, u8, u8, u64, u64, u8)| if by_channel { usize::from(n.1) } else { n.0 };
        let mut keys: Vec<usize> = raw.iter().map(key_of).collect();
        keys.sort_unstable();
        keys.dedup();
        let parts: Vec<Part> = keys
            .iter()
            .map(|&k| {
                let pitches: Vec<f64> = raw
                    .iter()
                    .filter(|n| key_of(n) == k)
                    .map(|n| f64::from(n.2))
                    .collect();
                let name = if by_channel {
                    format!("Channel {}", k + 1)
                } else {
                    smf.tracks[k]
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("Track {}", k + 1))
                };
                Part {
                    name,
                    notes: pitches.len(),
                    mean_pitch: pitches.iter().sum::<f64>() / pitches.len() as f64,
                }
            })
            .collect();

        let mut notes: Vec<SongNote> = raw
            .iter()
            .map(|n| {
                let start = map.seconds(n.3);
                let end = map.seconds(n.4).max(start + MIN_NOTE);
                SongNote {
                    start,
                    end,
                    pitch: n.2,
                    velocity: n.5.max(1),
                    part: keys.iter().position(|k| *k == key_of(n)).unwrap_or(0),
                }
            })
            .collect();
        notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
        let duration = notes.iter().map(|n| n.end).fold(0.0, f64::max);
        let last_tick = raw.iter().map(|n| n.4).max().unwrap_or(0);
        let (bars, beats) = grid(smf, &map, last_tick);
        Ok(Song {
            title: title.to_owned(),
            notes,
            parts,
            bars,
            beats,
            duration,
            bpm: 60e6 / f64::from(map.tempo_at(0)),
        })
    }

    /// Which hand plays which part, as a teacher would guess: the highest
    /// part is the right hand's, the lowest the left's, the rest by
    /// register. One part is split at middle C.
    pub fn default_split(&self) -> HandSplit {
        if self.parts.len() < 2 {
            return HandSplit::At(60);
        }
        let highest = (0..self.parts.len())
            .max_by(|&a, &b| {
                self.parts[a]
                    .mean_pitch
                    .total_cmp(&self.parts[b].mean_pitch)
            })
            .unwrap_or(0);
        let lowest = (0..self.parts.len())
            .min_by(|&a, &b| {
                self.parts[a]
                    .mean_pitch
                    .total_cmp(&self.parts[b].mean_pitch)
            })
            .unwrap_or(0);
        HandSplit::Parts(
            (0..self.parts.len())
                .map(|i| {
                    if i == highest {
                        Hand::Right
                    } else if i == lowest || self.parts[i].mean_pitch < 60.0 {
                        Hand::Left
                    } else {
                        Hand::Right
                    }
                })
                .collect(),
        )
    }

    /// The lowest and highest pitch played.
    pub fn range(&self) -> (u8, u8) {
        let low = self.notes.iter().map(|n| n.pitch).min().unwrap_or(60);
        let high = self.notes.iter().map(|n| n.pitch).max().unwrap_or(72);
        (low, high)
    }
}

/// Bar and beat times from the time signatures, up to `end`.
fn grid(smf: &Smf, map: &TempoMap, end: u64) -> (Vec<f64>, Vec<f64>) {
    let SmfTiming::PerQuarter(ppq) = smf.timing else {
        return (Vec::new(), Vec::new());
    };
    let mut signatures: Vec<(u64, u8, u8)> = smf
        .tracks
        .iter()
        .flat_map(|t| t.events.iter())
        .filter_map(|e| match e.kind {
            EventKind::TimeSignature {
                numerator,
                denominator,
            } if numerator > 0 && denominator > 0 => Some((e.tick, numerator, denominator)),
            _ => None,
        })
        .collect();
    signatures.sort_by_key(|s| s.0);
    let (mut bars, mut beats) = (Vec::new(), Vec::new());
    let mut tick = 0u64;
    let (mut numerator, mut denominator) = (4u8, 4u8);
    let mut next = 0;
    while tick <= end && bars.len() < 10_000 {
        while next < signatures.len() && signatures[next].0 <= tick {
            (numerator, denominator) = (signatures[next].1, signatures[next].2);
            next += 1;
        }
        let beat = (u64::from(ppq) * 4 / u64::from(denominator)).max(1);
        bars.push(map.seconds(tick));
        for b in 0..u64::from(numerator) {
            beats.push(map.seconds(tick + b * beat));
        }
        tick += beat * u64::from(numerator);
    }
    (bars, beats)
}

/// How notes are given to hands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HandSplit {
    /// Each part to a hand.
    Parts(Vec<Hand>),
    /// Below this key the left hand, from it up the right.
    At(u8),
}

impl HandSplit {
    pub fn hand(&self, note: &SongNote) -> Hand {
        match self {
            HandSplit::Parts(hands) => hands.get(note.part).copied().unwrap_or(Hand::Right),
            HandSplit::At(split) => {
                if note.pitch < *split {
                    Hand::Left
                } else {
                    Hand::Right
                }
            }
        }
    }
}

/// Songs that come with the app, written as MIDI files and read back like
/// any other.
pub fn builtin(name: &str) -> Option<Song> {
    let (title, bytes) = BUILTIN.iter().find(|(n, _)| *n == name)?;
    let smf = smf::parse(&bytes()).ok()?;
    Song::from_smf(&smf, title).ok()
}

/// Writes a built-in song as a MIDI file.
pub type Builder = fn() -> Vec<u8>;

pub const BUILTIN: &[(&str, Builder)] =
    &[("Ode to Joy", ode_to_joy), ("Twinkle, Twinkle", twinkle)];

/// Beethoven's theme in C, with whole-note roots in the left hand.
pub fn ode_to_joy() -> Vec<u8> {
    const Q: u64 = 480;
    let melody: &[(u8, u64)] = &[
        (64, 2),
        (64, 2),
        (65, 2),
        (67, 2),
        (67, 2),
        (65, 2),
        (64, 2),
        (62, 2),
        (60, 2),
        (60, 2),
        (62, 2),
        (64, 2),
        (64, 3),
        (62, 1),
        (62, 4),
        (64, 2),
        (64, 2),
        (65, 2),
        (67, 2),
        (67, 2),
        (65, 2),
        (64, 2),
        (62, 2),
        (60, 2),
        (60, 2),
        (62, 2),
        (64, 2),
        (62, 3),
        (60, 1),
        (60, 4),
    ];
    let bass: &[(u8, u64)] = &[
        (48, 8),
        (43, 8),
        (48, 8),
        (43, 8),
        (48, 8),
        (43, 8),
        (48, 8),
        (43, 4),
        (48, 4),
    ];
    write_two_hands(100.0, Q / 2, melody, bass)
}

/// The nursery tune in C, a chord root under every half bar.
pub fn twinkle() -> Vec<u8> {
    const Q: u64 = 480;
    let phrase_a: &[(u8, u64)] = &[
        (60, 1),
        (60, 1),
        (67, 1),
        (67, 1),
        (69, 1),
        (69, 1),
        (67, 2),
        (65, 1),
        (65, 1),
        (64, 1),
        (64, 1),
        (62, 1),
        (62, 1),
        (60, 2),
    ];
    let phrase_b: &[(u8, u64)] = &[
        (67, 1),
        (67, 1),
        (65, 1),
        (65, 1),
        (64, 1),
        (64, 1),
        (62, 2),
    ];
    let melody: Vec<(u8, u64)> = [phrase_a, phrase_b, phrase_b, phrase_a].concat();
    let bass_a: &[(u8, u64)] = &[
        (48, 4),
        (53, 2),
        (48, 2),
        (53, 2),
        (48, 2),
        (43, 2),
        (48, 2),
    ];
    let bass_b: &[(u8, u64)] = &[(48, 2), (53, 2), (48, 2), (43, 2)];
    let bass: Vec<(u8, u64)> = [bass_a, bass_b, bass_b, bass_a].concat();
    write_two_hands(96.0, Q, &melody, &bass)
}

/// A two-track file: melody, then bass, each as (key, length in `unit`s).
fn write_two_hands(bpm: f64, unit: u64, melody: &[(u8, u64)], bass: &[(u8, u64)]) -> Vec<u8> {
    let mut w = smf::Writer::new(480);
    w.track("").tempo(0, bpm).time_signature(0, 4, 4);
    for (name, notes, velocity) in [("Right hand", melody, 90), ("Left hand", bass, 70)] {
        w.track(name);
        let mut at = 0;
        for &(key, len) in notes {
            let ticks = len * unit;
            // A little gap, as a player would leave.
            w.note(0, at, ticks.saturating_sub(unit / 8).max(1), key, velocity);
            at += ticks;
        }
    }
    w.bytes()
}

/// How a song is practised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Mode {
    /// The song waits until the right notes are played.
    #[default]
    Wait,
    /// The song keeps time; notes are scored on accuracy and timing.
    PlayAlong,
    /// The app plays everything.
    Listen,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Wait, Mode::PlayAlong, Mode::Listen];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Wait => "Wait",
            Mode::PlayAlong => "Play along",
            Mode::Listen => "Listen",
        }
    }
}

/// Which hands the player plays; the app plays the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Hands {
    Left,
    Right,
    #[default]
    Both,
}

impl Hands {
    pub const ALL: [Hands; 3] = [Hands::Left, Hands::Right, Hands::Both];

    pub fn label(self) -> &'static str {
        match self {
            Hands::Left => "Left",
            Hands::Right => "Right",
            Hands::Both => "Both",
        }
    }

    pub fn includes(self, hand: Hand) -> bool {
        match self {
            Hands::Both => true,
            Hands::Left => hand == Hand::Left,
            Hands::Right => hand == Hand::Right,
        }
    }
}

/// How a note of the song is going.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NoteState {
    /// The app plays it.
    Auto,
    /// The player's, not yet played.
    Pending,
    /// The player's, played; with the timing error in Play-along.
    Hit(Option<f64>),
    /// The player's, and it went by unplayed.
    Missed,
}

/// A note the app plays, for the audio engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    On { pitch: u8, velocity: u8 },
    Off { pitch: u8 },
}

/// What a key press meant to the song.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Judgment {
    /// Not graded (Listen).
    None,
    /// One of the notes being waited for, or due.
    Right {
        error_ms: Option<f64>,
        rating: Option<Rating>,
    },
    /// Not a note of the song here.
    Wrong,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    pub hits: u32,
    pub misses: u32,
    pub wrong: u32,
    pub timing: Timing,
}

impl Stats {
    /// Wait: right presses out of all presses. Play-along: notes hit out
    /// of notes due, less a little for every wrong note.
    pub fn accuracy(&self, mode: Mode) -> Option<f64> {
        match mode {
            Mode::Listen => None,
            Mode::Wait => {
                let total = self.hits + self.wrong;
                (total > 0).then(|| f64::from(self.hits) / f64::from(total))
            }
            Mode::PlayAlong => {
                let due = self.hits + self.misses;
                (due > 0).then(|| {
                    let penalty = f64::from(self.wrong) * 0.5;
                    ((f64::from(self.hits) - penalty) / f64::from(due)).clamp(0.0, 1.0)
                })
            }
        }
    }
}

/// How far ahead of a note it counts as played, in Wait mode, in real
/// seconds: for players who get there before the song does.
const WAIT_LOOKAHEAD: f64 = 0.25;
/// How far from its time a note can be played in Play-along, real seconds.
pub const HIT_WINDOW: f64 = 0.2;
/// Real seconds of count-in before the first note.
pub const LEAD_IN: f64 = 2.0;
/// Notes starting this close together (song seconds) are one chord to wait for.
const CHORD_SPREAD: f64 = 0.04;

pub struct Player {
    song: Song,
    split: HandSplit,
    pub mode: Mode,
    pub hands: Hands,
    /// Playback speed, 1.0 as written.
    pub speed: f64,
    /// A section to repeat, in song seconds.
    pub looped: Option<(f64, f64)>,
    position: f64,
    playing: bool,
    finished: bool,
    states: Vec<NoteState>,
    hand: Vec<Hand>,
    /// The player's notes grouped into chords, in order.
    groups: Vec<Vec<usize>>,
    /// The first group not yet played (Wait).
    group: usize,
    /// The next note the app might start.
    auto_next: usize,
    /// The next note Play-along might give up on.
    judge_next: usize,
    sounding: Vec<usize>,
    /// The clock time `position` was last brought up to.
    clock: Option<f64>,
    pub stats: Stats,
    /// Played from the top with no seeking, so the score counts.
    complete: bool,
}

impl Player {
    pub fn new(song: Song, split: HandSplit, mode: Mode, hands: Hands) -> Player {
        let mut player = Player {
            song,
            split,
            mode,
            hands,
            speed: 1.0,
            looped: None,
            position: -LEAD_IN,
            playing: false,
            finished: false,
            states: Vec::new(),
            hand: Vec::new(),
            groups: Vec::new(),
            group: 0,
            auto_next: 0,
            judge_next: 0,
            sounding: Vec::new(),
            clock: None,
            stats: Stats::default(),
            complete: true,
        };
        player.configure(&mut Vec::new());
        player
    }

    pub fn song(&self) -> &Song {
        &self.song
    }

    pub fn split(&self) -> &HandSplit {
        &self.split
    }

    pub fn position(&self) -> f64 {
        self.position
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Played start to end without seeking or looping, so its score is
    /// worth keeping.
    pub fn is_complete_run(&self) -> bool {
        self.complete && self.looped.is_none()
    }

    pub fn state(&self, note: usize) -> NoteState {
        self.states.get(note).copied().unwrap_or(NoteState::Auto)
    }

    pub fn hand(&self, note: usize) -> Hand {
        self.hand.get(note).copied().unwrap_or(Hand::Right)
    }

    /// Changes who plays what. Keeps the position; the score starts over.
    pub fn set_split(&mut self, split: HandSplit, out: &mut Vec<Output>) {
        self.split = split;
        self.configure(out);
    }

    pub fn set_mode(&mut self, mode: Mode, hands: Hands, out: &mut Vec<Output>) {
        self.mode = mode;
        self.hands = hands;
        self.configure(out);
    }

    fn configure(&mut self, out: &mut Vec<Output>) {
        self.hand = self.song.notes.iter().map(|n| self.split.hand(n)).collect();
        let listen = self.mode == Mode::Listen;
        self.states = self
            .hand
            .iter()
            .map(|h| {
                if !listen && self.hands.includes(*h) {
                    NoteState::Pending
                } else {
                    NoteState::Auto
                }
            })
            .collect();
        self.groups.clear();
        for (i, note) in self.song.notes.iter().enumerate() {
            if self.states[i] != NoteState::Pending {
                continue;
            }
            match self.groups.last_mut() {
                Some(group) if note.start - self.song.notes[group[0]].start <= CHORD_SPREAD => {
                    group.push(i);
                }
                _ => self.groups.push(vec![i]),
            }
        }
        self.stats = Stats::default();
        let position = self.position;
        self.jump(position, out);
    }

    /// Starts or pauses. `now` is the clock time.
    pub fn set_playing(&mut self, playing: bool, now: f64, out: &mut Vec<Output>) {
        if playing && self.finished {
            self.restart(out);
        }
        if !playing {
            self.silence(out);
        }
        self.playing = playing;
        self.clock = Some(now);
    }

    /// Back to the count-in before the first note (or the loop's start).
    pub fn restart(&mut self, out: &mut Vec<Output>) {
        let start = match self.looped {
            Some((a, _)) => a - LEAD_IN * self.speed,
            None => -LEAD_IN * self.speed,
        };
        self.jump(start, out);
        self.stats = Stats::default();
        self.complete = self.looped.is_none();
        self.finished = false;
    }

    /// Moves to `position` (song seconds). The score starts over and won't
    /// count as a full run.
    pub fn seek(&mut self, position: f64, out: &mut Vec<Output>) {
        let position = position.clamp(-LEAD_IN * self.speed, self.song.duration);
        self.jump(position, out);
        self.stats = Stats::default();
        self.complete = false;
        self.finished = false;
    }

    fn jump(&mut self, position: f64, out: &mut Vec<Output>) {
        self.silence(out);
        self.position = position;
        let notes = &self.song.notes;
        // The app plays the bars before a loop as a count-in, but the
        // player is only asked for what's inside it.
        let asked_from = match self.looped {
            Some((a, _)) => position.max(a),
            None => position,
        };
        self.auto_next = notes.partition_point(|n| n.start < position);
        self.judge_next = notes.partition_point(|n| n.start < asked_from);
        for i in self.auto_next..notes.len() {
            if self.states[i] != NoteState::Auto {
                self.states[i] = NoteState::Pending;
            }
        }
        self.group = self
            .groups
            .partition_point(|g| notes[g[0]].start < asked_from);
    }

    fn silence(&mut self, out: &mut Vec<Output>) {
        for i in self.sounding.drain(..) {
            out.push(Output::Off {
                pitch: self.song.notes[i].pitch,
            });
        }
    }

    /// The notes the song is waiting for (Wait mode, when it's stopped at
    /// them), or the next ones due.
    pub fn waiting_for(&self) -> Vec<u8> {
        if self.mode != Mode::Wait {
            return Vec::new();
        }
        let Some(group) = self.groups.get(self.group) else {
            return Vec::new();
        };
        let start = self.song.notes[group[0]].start;
        if self.position < start - 1e-9 {
            return Vec::new();
        }
        group
            .iter()
            .filter(|&&i| self.states[i] == NoteState::Pending)
            .map(|&i| self.song.notes[i].pitch)
            .collect()
    }

    /// True while Wait mode holds the song at notes not yet played.
    pub fn is_waiting(&self) -> bool {
        self.playing && !self.waiting_for().is_empty()
    }

    /// The player's notes coming up within `ahead` song seconds, for hints.
    pub fn upcoming(&self, ahead: f64) -> Vec<(u8, Hand)> {
        let notes = &self.song.notes;
        let from = notes.partition_point(|n| n.start < self.position - 0.05);
        notes[from..]
            .iter()
            .enumerate()
            .take_while(|(_, n)| n.start <= self.position + ahead)
            .filter(|(k, _)| self.states[from + k] == NoteState::Pending)
            .map(|(k, n)| (n.pitch, self.hand[from + k]))
            .collect()
    }

    /// Brings the song up to clock time `now`.
    pub fn advance(&mut self, now: f64, out: &mut Vec<Output>) {
        let dt = self.clock.map_or(0.0, |c| (now - c).max(0.0));
        self.clock = Some(now);
        if !self.playing {
            return;
        }
        let mut target = self.position + dt * self.speed;
        if self.mode == Mode::Wait
            && let Some(group) = self.groups.get(self.group)
        {
            target = target.min(self.song.notes[group[0]].start);
        }
        if let Some((a, b)) = self.looped
            && b > a
            && target >= b
        {
            self.move_to(b, out);
            self.jump(a, out);
            return;
        }
        self.move_to(target, out);
        if self.looped.is_none() && self.position >= self.song.duration + 0.5 {
            self.silence(out);
            self.playing = false;
            self.finished = true;
        }
    }

    fn move_to(&mut self, t: f64, out: &mut Vec<Output>) {
        self.position = t;
        let notes = &self.song.notes;
        while self.auto_next < notes.len() && notes[self.auto_next].start < t {
            let i = self.auto_next;
            if self.states[i] == NoteState::Auto {
                let n = &notes[i];
                // Restrike a key that's already sounding.
                if let Some(k) = self
                    .sounding
                    .iter()
                    .position(|&s| notes[s].pitch == n.pitch)
                {
                    self.sounding.remove(k);
                    out.push(Output::Off { pitch: n.pitch });
                }
                out.push(Output::On {
                    pitch: n.pitch,
                    velocity: n.velocity,
                });
                self.sounding.push(i);
            }
            self.auto_next += 1;
        }
        self.sounding.retain(|&i| {
            let ended = notes[i].end <= t;
            if ended {
                out.push(Output::Off {
                    pitch: notes[i].pitch,
                });
            }
            !ended
        });
        if self.mode == Mode::PlayAlong {
            let window = HIT_WINDOW * self.speed;
            while self.judge_next < notes.len() && notes[self.judge_next].start + window < t {
                if self.states[self.judge_next] == NoteState::Pending {
                    self.states[self.judge_next] = NoteState::Missed;
                    self.stats.misses += 1;
                }
                self.judge_next += 1;
            }
        }
    }

    /// The player pressed `pitch` at clock time `now`.
    pub fn press(&mut self, pitch: u8, now: f64, out: &mut Vec<Output>) -> Judgment {
        self.advance(now, out);
        match self.mode {
            Mode::Listen => Judgment::None,
            Mode::Wait => self.press_wait(pitch),
            Mode::PlayAlong => self.press_along(pitch),
        }
    }

    fn press_wait(&mut self, pitch: u8) -> Judgment {
        let reach = self.position + WAIT_LOOKAHEAD * self.speed;
        let notes = &self.song.notes;
        let found = self.groups[self.group.min(self.groups.len())..]
            .iter()
            .take_while(|g| notes[g[0]].start <= reach)
            .flat_map(|g| g.iter())
            .copied()
            .find(|&i| notes[i].pitch == pitch && self.states[i] == NoteState::Pending);
        let Some(i) = found else {
            // Only a wrong note once the song has stopped for the player or
            // is about to: playing along with the app's hand isn't wrong.
            if self.playing || !self.waiting_for().is_empty() {
                self.stats.wrong += 1;
                return Judgment::Wrong;
            }
            return Judgment::None;
        };
        self.states[i] = NoteState::Hit(None);
        self.stats.hits += 1;
        while self
            .groups
            .get(self.group)
            .is_some_and(|g| g.iter().all(|&i| self.states[i] != NoteState::Pending))
        {
            self.group += 1;
        }
        Judgment::Right {
            error_ms: None,
            rating: None,
        }
    }

    fn press_along(&mut self, pitch: u8) -> Judgment {
        let window = HIT_WINDOW * self.speed;
        let notes = &self.song.notes;
        let from = notes.partition_point(|n| n.start < self.position - window);
        let best = notes[from..]
            .iter()
            .enumerate()
            .take_while(|(_, n)| n.start <= self.position + window)
            .filter(|(k, n)| n.pitch == pitch && self.states[from + k] == NoteState::Pending)
            .min_by(|a, b| {
                (a.1.start - self.position)
                    .abs()
                    .total_cmp(&(b.1.start - self.position).abs())
            })
            .map(|(k, _)| from + k);
        let Some(i) = best else {
            self.stats.wrong += 1;
            return Judgment::Wrong;
        };
        // Song seconds to real milliseconds.
        let error = timing_error_ms(self.position, notes[i].start) / self.speed;
        self.states[i] = NoteState::Hit(Some(error));
        self.stats.hits += 1;
        self.stats.timing.add(error);
        Judgment::Right {
            error_ms: Some(error),
            rating: Some(Rating::of(error)),
        }
    }

    /// The notes the app is sounding now, and whose hand they are.
    pub fn sounding(&self) -> impl Iterator<Item = (u8, Hand)> + '_ {
        self.sounding
            .iter()
            .map(|&i| (self.song.notes[i].pitch, self.hand[i]))
    }

    /// Notes of the player's still to come or missed, and done: (done, total).
    pub fn progress(&self) -> (usize, usize) {
        let mine = self.states.iter().filter(|s| **s != NoteState::Auto);
        let total = mine.clone().count();
        let done = mine.filter(|s| matches!(s, NoteState::Hit(_))).count();
        (done, total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Right hand C D E on beats 1-3, left hand C3 under the first, at 120 BPM
    /// (half a second a beat).
    fn little_song() -> Song {
        let mut w = smf::Writer::new(480);
        w.track("tempo").tempo(0, 120.0).time_signature(0, 4, 4);
        w.track("Melody")
            .note(0, 0, 400, 72, 100)
            .note(0, 480, 400, 74, 100)
            .note(0, 960, 400, 76, 100);
        w.track("Bass").note(0, 0, 1800, 48, 80);
        Song::from_smf(&smf::parse(&w.bytes()).unwrap(), "little").unwrap()
    }

    fn ons(out: &[Output]) -> Vec<u8> {
        out.iter()
            .filter_map(|o| match o {
                Output::On { pitch, .. } => Some(*pitch),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn reads_notes_parts_and_bars() {
        let song = little_song();
        assert_eq!(song.notes.len(), 4);
        assert_eq!(song.parts.len(), 2);
        assert_eq!(song.parts[0].name, "Melody");
        assert_eq!(song.notes[0].pitch, 48, "sorted by start, then pitch");
        assert!((song.notes[2].start - 0.5).abs() < 1e-9);
        assert!((song.notes[3].end - (1.0 + 400.0 / 960.0)).abs() < 1e-9);
        assert!((song.bpm - 120.0).abs() < 1e-6);
        assert_eq!(song.bars, [0.0], "one bar of 4/4 covers it");
        assert_eq!(song.beats.len(), 4);
        assert_eq!(
            song.default_split(),
            HandSplit::Parts(vec![Hand::Right, Hand::Left])
        );
        assert_eq!(song.range(), (48, 76));
    }

    #[test]
    fn one_track_files_split_by_channel_and_skip_drums() {
        let mut w = smf::Writer::new(96);
        w.note(0, 0, 96, 72, 100)
            .note(1, 0, 96, 40, 100)
            .note(9, 0, 96, 36, 100);
        let song = Song::from_smf(&smf::parse(&w.bytes()).unwrap(), "x").unwrap();
        assert_eq!(song.notes.len(), 2, "no drums");
        assert_eq!(song.parts.len(), 2);
        assert_eq!(song.parts[1].name, "Channel 2");
        let mut lonely = smf::Writer::new(96);
        lonely.note(0, 0, 96, 50, 100).note(0, 96, 96, 70, 100);
        let song = Song::from_smf(&smf::parse(&lonely.bytes()).unwrap(), "y").unwrap();
        assert_eq!(song.default_split(), HandSplit::At(60));
        let split = song.default_split();
        assert_eq!(split.hand(&song.notes[0]), Hand::Left);
        assert_eq!(split.hand(&song.notes[1]), Hand::Right);
        let empty = smf::Writer::new(96);
        let mut bytes = empty.bytes();
        bytes[11] = 0; // no tracks
        assert!(Song::from_smf(&smf::parse(&bytes).unwrap(), "z").is_err());
    }

    #[test]
    fn builtin_songs_load() {
        for (name, _) in BUILTIN {
            let song = builtin(name).unwrap();
            assert!(song.notes.len() > 20, "{name}");
            assert_eq!(song.parts.len(), 2);
            assert!(song.bars.len() >= 8);
        }
    }

    #[test]
    fn wait_mode_holds_until_the_right_notes() {
        let song = little_song();
        let split = song.default_split();
        let mut out = Vec::new();
        let mut p = Player::new(song, split, Mode::Wait, Hands::Right);
        p.set_playing(true, 0.0, &mut out);
        // Count-in, then the song stops at the first right-hand note.
        p.advance(1.0, &mut out);
        assert!(p.position() < 0.0);
        assert!(!p.is_waiting());
        p.advance(3.0, &mut out);
        assert_eq!(p.position(), 0.0);
        assert!(p.is_waiting());
        assert_eq!(p.waiting_for(), [72]);
        assert!(ons(&out).is_empty(), "the left hand waits too");

        // A wrong note changes nothing.
        assert_eq!(p.press(71, 3.5, &mut out), Judgment::Wrong);
        p.advance(4.0, &mut out);
        assert_eq!(p.position(), 0.0);
        assert_eq!(p.stats.wrong, 1);

        // The right one lets it go on, and the left hand sounds.
        assert!(matches!(p.press(72, 4.0, &mut out), Judgment::Right { .. }));
        p.advance(4.25, &mut out);
        assert!((p.position() - 0.25).abs() < 1e-9);
        assert_eq!(ons(&out), [48]);
        // It stops again at D.
        p.advance(5.0, &mut out);
        assert_eq!(p.position(), 0.5);
        assert_eq!(p.waiting_for(), [74]);

        // Playing just ahead of the song counts.
        assert!(matches!(p.press(74, 5.1, &mut out), Judgment::Right { .. }));
        p.advance(5.2, &mut out);
        assert!(p.position() > 0.5);
        assert!(
            matches!(p.press(76, 5.4, &mut out), Judgment::Right { .. }),
            "0.2 s early"
        );
        p.advance(9.0, &mut out);
        assert!(p.is_finished());
        assert_eq!(p.stats.hits, 3);
        assert_eq!(p.progress(), (3, 3));
        assert!((p.stats.accuracy(Mode::Wait).unwrap() - 0.75).abs() < 1e-9);
        assert!(p.is_complete_run());
    }

    #[test]
    fn wait_mode_chords_need_every_note() {
        let mut w = smf::Writer::new(480);
        w.track("chords")
            .note(0, 0, 480, 60, 90)
            .note(0, 0, 480, 64, 90)
            .note(0, 0, 480, 67, 90);
        let song = Song::from_smf(&smf::parse(&w.bytes()).unwrap(), "c").unwrap();
        let mut out = Vec::new();
        let mut p = Player::new(song, HandSplit::At(0), Mode::Wait, Hands::Both);
        p.set_playing(true, 0.0, &mut out);
        p.advance(5.0, &mut out);
        assert_eq!(p.waiting_for(), [60, 64, 67]);
        p.press(64, 5.0, &mut out);
        p.press(60, 5.1, &mut out);
        assert_eq!(p.waiting_for(), [67]);
        p.press(67, 5.2, &mut out);
        assert!(p.waiting_for().is_empty());
    }

    #[test]
    fn play_along_scores_timing_and_misses() {
        let song = little_song();
        let split = song.default_split();
        let mut out = Vec::new();
        let mut p = Player::new(song, split, Mode::PlayAlong, Hands::Right);
        p.set_playing(true, 0.0, &mut out);
        // Song time = clock - 2 s of count-in.
        let judged = p.press(72, 2.03, &mut out);
        let Judgment::Right {
            error_ms: Some(e),
            rating,
        } = judged
        else {
            panic!("{judged:?}")
        };
        assert!((e - 30.0).abs() < 1e-6);
        assert_eq!(rating, Some(Rating::Perfect));
        assert_eq!(ons(&out), [48], "the left hand plays itself");
        // A note no one wrote.
        assert_eq!(p.press(80, 2.2, &mut out), Judgment::Wrong);
        // D goes by unplayed.
        p.advance(2.8, &mut out);
        assert_eq!(p.stats.misses, 1);
        assert_eq!(p.state(2), NoteState::Missed);
        // E a little early.
        let Judgment::Right {
            error_ms: Some(e), ..
        } = p.press(76, 2.94, &mut out)
        else {
            panic!()
        };
        assert!((e + 60.0).abs() < 1e-6);
        assert_eq!(p.stats.hits, 2);
        assert!((p.stats.timing.mean_abs().unwrap() - 45.0).abs() < 1e-6);
        // Two of three hit, half a note off for the wrong one.
        assert!((p.stats.accuracy(Mode::PlayAlong).unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn half_speed_stretches_time_but_not_the_judging() {
        let song = little_song();
        let split = song.default_split();
        let mut out = Vec::new();
        let mut p = Player::new(song, split, Mode::PlayAlong, Hands::Right);
        p.speed = 0.5;
        p.restart(&mut out);
        p.set_playing(true, 0.0, &mut out);
        // Count-in lasts two real seconds whatever the speed; D is at song
        // 0.5 s, one real second later.
        let Judgment::Right {
            error_ms: Some(e), ..
        } = p.press(74, 3.04, &mut out)
        else {
            panic!("{:?}", p.position())
        };
        assert!((e - 40.0).abs() < 1e-6, "{e}");
    }

    #[test]
    fn listen_plays_everything_and_releases_on_pause() {
        let song = little_song();
        let split = song.default_split();
        let mut out = Vec::new();
        let mut p = Player::new(song, split, Mode::Listen, Hands::Right);
        p.set_playing(true, 0.0, &mut out);
        p.advance(2.6, &mut out);
        assert_eq!(ons(&out), [48, 72, 74]);
        assert!(out.contains(&Output::Off { pitch: 72 }));
        out.clear();
        p.set_playing(false, 2.7, &mut out);
        assert_eq!(out.len(), 2, "D and the bass stop: {out:?}");
        assert_eq!(p.press(60, 2.8, &mut out), Judgment::None);
    }

    #[test]
    fn loops_repeat_a_section() {
        let song = little_song();
        let split = song.default_split();
        let mut out = Vec::new();
        let mut p = Player::new(song, split, Mode::PlayAlong, Hands::Right);
        p.looped = Some((0.4, 0.9));
        p.restart(&mut out);
        assert!((p.position() - (0.4 - LEAD_IN)).abs() < 1e-9);
        p.set_playing(true, 0.0, &mut out);
        p.press(74, 2.1, &mut out);
        assert_eq!(p.stats.hits, 1);
        p.advance(2.55, &mut out);
        assert!(
            (p.position() - 0.4).abs() < 1e-9,
            "wrapped: {}",
            p.position()
        );
        assert_eq!(p.state(2), NoteState::Pending, "D can be played again");
        p.advance(3.0, &mut out);
        assert_eq!(p.stats.misses, 1);
        assert!(!p.is_finished());
        assert!(!p.is_complete_run());
    }

    #[test]
    fn seeking_resets_the_score_and_cuts_notes() {
        let song = little_song();
        let split = song.default_split();
        let mut out = Vec::new();
        let mut p = Player::new(song, split, Mode::PlayAlong, Hands::Right);
        p.set_playing(true, 0.0, &mut out);
        p.advance(2.1, &mut out);
        assert_eq!(ons(&out), [48]);
        out.clear();
        p.seek(0.75, &mut out);
        assert_eq!(out, [Output::Off { pitch: 48 }]);
        assert_eq!(p.stats.misses, 0);
        assert_eq!(p.state(2), NoteState::Pending);
        assert!(!p.is_complete_run());
        // Changing hands keeps the place.
        p.set_mode(Mode::PlayAlong, Hands::Both, &mut out);
        assert_eq!(p.position(), 0.75);
        assert_eq!(
            p.state(0),
            NoteState::Pending,
            "the bass is the player's now"
        );
        assert!(p.stats == Stats::default());
    }
}
