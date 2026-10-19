//! Standard MIDI Files (.mid), formats 0 and 1: chunks, events and the
//! tempo map that turns ticks into seconds. Format 2 files are read as if
//! their tracks played together.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct Smf {
    pub format: u16,
    pub timing: Timing,
    pub tracks: Vec<Track>,
}

/// What a tick is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Timing {
    /// Ticks per quarter note; the tempo says how long a quarter is.
    PerQuarter(u16),
    /// Ticks per second, fixed: frames per second times ticks per frame.
    Timecode { fps: u8, ticks_per_frame: u8 },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Track {
    pub name: Option<String>,
    /// In order, at absolute ticks.
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    pub tick: u64,
    pub kind: EventKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EventKind {
    NoteOn {
        channel: u8,
        key: u8,
        velocity: u8,
    },
    /// Includes Note Ons with velocity 0, which mean Note Off.
    NoteOff {
        channel: u8,
        key: u8,
    },
    Controller {
        channel: u8,
        controller: u8,
        value: u8,
    },
    Program {
        channel: u8,
        program: u8,
    },
    /// Microseconds per quarter note from here on.
    Tempo(u32),
    TimeSignature {
        numerator: u8,
        denominator: u8,
    },
    KeySignature {
        sharps: i8,
        minor: bool,
    },
    EndOfTrack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmfError {
    NotMidi,
    Truncated,
    BadHeader(String),
    BadEvent { track: usize, offset: usize },
}

impl fmt::Display for SmfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmfError::NotMidi => write!(f, "not a MIDI file"),
            SmfError::Truncated => write!(f, "the file is cut short"),
            SmfError::BadHeader(why) => write!(f, "bad header: {why}"),
            SmfError::BadEvent { track, offset } => {
                write!(
                    f,
                    "unreadable event in track {} at byte {offset}",
                    track + 1
                )
            }
        }
    }
}

impl std::error::Error for SmfError {}

/// Default tempo when a file sets none: 120 BPM.
pub const DEFAULT_TEMPO: u32 = 500_000;

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn u8(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A variable-length quantity: seven bits a byte, high bit set on all
    /// but the last, at most four bytes.
    fn vlq(&mut self) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..4 {
            let b = self.u8()?;
            value = (value << 7) | u32::from(b & 0x7F);
            if b & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }
}

pub fn parse(bytes: &[u8]) -> Result<Smf, SmfError> {
    let mut r = Reader::new(bytes);
    // Some files come wrapped in a RIFF "RMID" container.
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"RMID") {
        if let Some(start) = bytes.windows(4).position(|w| w == b"MThd") {
            return parse(&bytes[start..]);
        }
        return Err(SmfError::NotMidi);
    }
    if r.take(4) != Some(b"MThd") {
        return Err(SmfError::NotMidi);
    }
    let length = r.u32().ok_or(SmfError::Truncated)? as usize;
    if length < 6 {
        return Err(SmfError::BadHeader(format!("header is {length} bytes")));
    }
    let header = r.take(length).ok_or(SmfError::Truncated)?;
    let mut h = Reader::new(header);
    let format = h.u16().ok_or(SmfError::Truncated)?;
    let count = h.u16().ok_or(SmfError::Truncated)?;
    let division = h.u16().ok_or(SmfError::Truncated)?;
    if format > 2 {
        return Err(SmfError::BadHeader(format!("format {format}")));
    }
    let timing = if division & 0x8000 != 0 {
        let fps = (-((division >> 8) as u8 as i8)) as u8;
        let ticks_per_frame = (division & 0xFF) as u8;
        if fps == 0 || ticks_per_frame == 0 {
            return Err(SmfError::BadHeader("timecode division of zero".into()));
        }
        Timing::Timecode {
            fps,
            ticks_per_frame,
        }
    } else {
        if division == 0 {
            return Err(SmfError::BadHeader("zero ticks per quarter".into()));
        }
        Timing::PerQuarter(division)
    };

    let mut tracks = Vec::new();
    while !r.done() && tracks.len() < usize::from(count) {
        let Some(id) = r.take(4) else { break };
        let Some(length) = r.u32() else { break };
        // A last track that claims more than the file holds is read as far as it goes.
        let available = bytes.len() - r.pos;
        let data = r
            .take((length as usize).min(available))
            .ok_or(SmfError::Truncated)?;
        if id == b"MTrk" {
            tracks.push(parse_track(data, tracks.len())?);
        }
        // Other chunk types are allowed and skipped.
    }
    if tracks.is_empty() && count > 0 {
        return Err(SmfError::Truncated);
    }
    Ok(Smf {
        format,
        timing,
        tracks,
    })
}

fn parse_track(data: &[u8], index: usize) -> Result<Track, SmfError> {
    let mut r = Reader::new(data);
    let mut track = Track::default();
    let mut tick = 0u64;
    let mut running: Option<u8> = None;
    let bad = |r: &Reader| SmfError::BadEvent {
        track: index,
        offset: r.pos,
    };
    while !r.done() {
        // A track missing its End of Track ends where its bytes do.
        let Some(delta) = r.vlq() else { break };
        tick += u64::from(delta);
        let first = r.peek().ok_or_else(|| bad(&r))?;
        let status = if first & 0x80 != 0 {
            r.pos += 1;
            first
        } else {
            // Running status: the data byte belongs to the last channel message.
            running.ok_or_else(|| bad(&r))?
        };
        let kind = match status {
            0x80..=0xEF => {
                running = Some(status);
                let channel = status & 0x0F;
                let a = r.u8().ok_or_else(|| bad(&r))? & 0x7F;
                let two = !matches!(status & 0xF0, 0xC0 | 0xD0);
                let b = if two {
                    r.u8().ok_or_else(|| bad(&r))? & 0x7F
                } else {
                    0
                };
                match status & 0xF0 {
                    0x80 => Some(EventKind::NoteOff { channel, key: a }),
                    0x90 if b == 0 => Some(EventKind::NoteOff { channel, key: a }),
                    0x90 => Some(EventKind::NoteOn {
                        channel,
                        key: a,
                        velocity: b,
                    }),
                    0xB0 => Some(EventKind::Controller {
                        channel,
                        controller: a,
                        value: b,
                    }),
                    0xC0 => Some(EventKind::Program {
                        channel,
                        program: a,
                    }),
                    _ => None,
                }
            }
            0xF0 | 0xF7 => {
                // SysEx: skipped. Cancels running status.
                running = None;
                let length = r.vlq().ok_or_else(|| bad(&r))?;
                r.take(length as usize).ok_or_else(|| bad(&r))?;
                None
            }
            0xFF => {
                running = None;
                let kind = r.u8().ok_or_else(|| bad(&r))?;
                let length = r.vlq().ok_or_else(|| bad(&r))?;
                let body = r.take(length as usize).ok_or_else(|| bad(&r))?;
                match (kind, body) {
                    (0x03, name) if track.name.is_none() => {
                        let name = String::from_utf8_lossy(name).trim().to_owned();
                        if !name.is_empty() {
                            track.name = Some(name);
                        }
                        None
                    }
                    (0x51, [a, b, c]) => Some(EventKind::Tempo(
                        (u32::from(*a) << 16) | (u32::from(*b) << 8) | u32::from(*c),
                    )),
                    (0x58, [n, d, ..]) => Some(EventKind::TimeSignature {
                        numerator: *n,
                        denominator: 1u8.checked_shl(u32::from(*d)).unwrap_or(4),
                    }),
                    (0x59, [sf, mi]) => Some(EventKind::KeySignature {
                        sharps: *sf as i8,
                        minor: *mi != 0,
                    }),
                    (0x2F, _) => Some(EventKind::EndOfTrack),
                    _ => None,
                }
            }
            // System common or real-time bytes don't belong in a file.
            _ => return Err(bad(&r)),
        };
        if let Some(kind) = kind {
            track.events.push(Event { tick, kind });
            if kind == EventKind::EndOfTrack {
                break;
            }
        }
    }
    Ok(track)
}

/// Turns ticks into seconds, following every tempo change in the file.
#[derive(Debug, Clone, PartialEq)]
pub struct TempoMap {
    timing: Timing,
    /// (tick, seconds at that tick, microseconds per quarter from there).
    segments: Vec<(u64, f64, u32)>,
}

impl TempoMap {
    /// Tempo changes are read from every track: format 1 files keep them
    /// in the first, but not all files follow the rule.
    pub fn new(smf: &Smf) -> TempoMap {
        let mut changes: Vec<(u64, u32)> = smf
            .tracks
            .iter()
            .flat_map(|t| t.events.iter())
            .filter_map(|e| match e.kind {
                EventKind::Tempo(micros) if micros > 0 => Some((e.tick, micros)),
                _ => None,
            })
            .collect();
        changes.sort_by_key(|c| c.0);
        let mut segments = vec![(0u64, 0.0f64, DEFAULT_TEMPO)];
        for (tick, micros) in changes {
            let &(start, seconds, tempo) = segments.last().expect("never empty");
            let at = seconds + Self::span(smf.timing, tick - start, tempo);
            if tick == start {
                // Several changes at one tick: the last one counts.
                segments.last_mut().expect("never empty").2 = micros;
            } else {
                segments.push((tick, at, micros));
            }
        }
        TempoMap {
            timing: smf.timing,
            segments,
        }
    }

    fn span(timing: Timing, ticks: u64, micros: u32) -> f64 {
        match timing {
            Timing::PerQuarter(ppq) => ticks as f64 * f64::from(micros) / 1e6 / f64::from(ppq),
            Timing::Timecode {
                fps,
                ticks_per_frame,
            } => {
                // 29 means 29.97 drop-frame.
                let fps = if fps == 29 { 29.97 } else { f64::from(fps) };
                ticks as f64 / (fps * f64::from(ticks_per_frame))
            }
        }
    }

    pub fn seconds(&self, tick: u64) -> f64 {
        let i = self
            .segments
            .partition_point(|s| s.0 <= tick)
            .saturating_sub(1);
        let (start, seconds, micros) = self.segments[i];
        seconds + Self::span(self.timing, tick - start, micros)
    }

    /// Microseconds per quarter at `tick`.
    pub fn tempo_at(&self, tick: u64) -> u32 {
        let i = self
            .segments
            .partition_point(|s| s.0 <= tick)
            .saturating_sub(1);
        self.segments[i].2
    }
}

/// Builds Standard MIDI Files: for the built-in songs and for tests.
#[derive(Debug, Clone)]
pub struct Writer {
    pub ppq: u16,
    tracks: Vec<Vec<(u64, Vec<u8>)>>,
}

impl Writer {
    pub fn new(ppq: u16) -> Writer {
        Writer {
            ppq,
            tracks: Vec::new(),
        }
    }

    /// Starts a new track; events go to the last one started.
    pub fn track(&mut self, name: &str) -> &mut Writer {
        let mut events = Vec::new();
        if !name.is_empty() {
            let mut meta = vec![0xFF, 0x03];
            push_vlq(&mut meta, name.len() as u32);
            meta.extend_from_slice(name.as_bytes());
            events.push((0, meta));
        }
        self.tracks.push(events);
        self
    }

    fn push(&mut self, tick: u64, bytes: Vec<u8>) {
        if self.tracks.is_empty() {
            self.track("");
        }
        self.tracks.last_mut().expect("a track").push((tick, bytes));
    }

    pub fn tempo(&mut self, tick: u64, bpm: f64) -> &mut Writer {
        let micros = (60e6 / bpm).round() as u32;
        let b = micros.to_be_bytes();
        self.push(tick, vec![0xFF, 0x51, 0x03, b[1], b[2], b[3]]);
        self
    }

    pub fn time_signature(&mut self, tick: u64, numerator: u8, denominator: u8) -> &mut Writer {
        let power = denominator.max(1).trailing_zeros() as u8;
        self.push(tick, vec![0xFF, 0x58, 0x04, numerator, power, 24, 8]);
        self
    }

    /// A note from `start` for `len` ticks.
    pub fn note(
        &mut self,
        channel: u8,
        start: u64,
        len: u64,
        key: u8,
        velocity: u8,
    ) -> &mut Writer {
        self.push(start, vec![0x90 | channel, key, velocity.max(1)]);
        self.push(start + len, vec![0x80 | channel, key, 64]);
        self
    }

    /// The file, format 1 (format 0 if there's one track). Events are
    /// sorted, Note Offs before Note Ons at the same tick, and channel
    /// messages use running status.
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        let format: u16 = if self.tracks.len() == 1 { 0 } else { 1 };
        out.extend_from_slice(&format.to_be_bytes());
        out.extend_from_slice(&(self.tracks.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.ppq.to_be_bytes());
        for track in &self.tracks {
            let mut events = track.clone();
            // Offs (0x80) sort before Ons (0x90) at the same tick; meta first.
            events.sort_by_key(|(tick, bytes)| (*tick, bytes[0] != 0xFF, bytes[0] & 0xF0));
            let mut data = Vec::new();
            let mut last = 0u64;
            let mut running = None;
            for (tick, bytes) in events {
                push_vlq(&mut data, (tick - last) as u32);
                last = tick;
                let status = bytes[0];
                if status < 0xF0 && running == Some(status) {
                    data.extend_from_slice(&bytes[1..]);
                } else {
                    data.extend_from_slice(&bytes);
                }
                running = (status < 0xF0).then_some(status);
            }
            data.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
            out.extend_from_slice(b"MTrk");
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(&data);
        }
        out
    }
}

fn push_vlq(out: &mut Vec<u8>, value: u32) {
    let mut stack = vec![(value & 0x7F) as u8];
    let mut v = value >> 7;
    while v > 0 {
        stack.push(((v & 0x7F) as u8) | 0x80);
        v >>= 7;
    }
    out.extend(stack.iter().rev());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(format: u16, tracks: u16, division: u16) -> Vec<u8> {
        let mut h = b"MThd".to_vec();
        h.extend_from_slice(&6u32.to_be_bytes());
        h.extend_from_slice(&format.to_be_bytes());
        h.extend_from_slice(&tracks.to_be_bytes());
        h.extend_from_slice(&division.to_be_bytes());
        h
    }

    fn chunk(data: &[u8]) -> Vec<u8> {
        let mut c = b"MTrk".to_vec();
        c.extend_from_slice(&(data.len() as u32).to_be_bytes());
        c.extend_from_slice(data);
        c
    }

    fn notes(track: &Track) -> Vec<(u64, EventKind)> {
        track
            .events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::NoteOn { .. } | EventKind::NoteOff { .. }))
            .map(|e| (e.tick, e.kind))
            .collect()
    }

    #[test]
    fn variable_length_quantities() {
        for (bytes, value) in [
            (&[0x00][..], 0),
            (&[0x7F][..], 127),
            (&[0x81, 0x00][..], 128),
            (&[0xFF, 0x7F][..], 16383),
            (&[0x81, 0x80, 0x00][..], 16384),
            (&[0xFF, 0xFF, 0xFF, 0x7F][..], 0x0FFF_FFFF),
        ] {
            assert_eq!(Reader::new(bytes).vlq(), Some(value));
            let mut out = Vec::new();
            push_vlq(&mut out, value);
            assert_eq!(out, bytes);
        }
        assert_eq!(Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x00]).vlq(), None);
        assert_eq!(Reader::new(&[0x81]).vlq(), None);
    }

    #[test]
    fn format_0_with_running_status_and_velocity_zero_offs() {
        let mut file = header(0, 1, 96);
        file.extend(chunk(&[
            0x00, 0x90, 60, 100, // C on
            0x00, 64, 90, // E on, running status
            0x60, 60, 0, // C off as a velocity-0 Note On
            0x00, 64, 0, // E off
            0x00, 0x91, 67, 80, // G on, channel 2
            0x30, 0x81, 67, 40, // G off
            0x00, 0xFF, 0x2F, 0x00,
        ]));
        let smf = parse(&file).unwrap();
        assert_eq!(smf.format, 0);
        assert_eq!(smf.timing, Timing::PerQuarter(96));
        assert_eq!(
            notes(&smf.tracks[0]),
            [
                (
                    0,
                    EventKind::NoteOn {
                        channel: 0,
                        key: 60,
                        velocity: 100
                    }
                ),
                (
                    0,
                    EventKind::NoteOn {
                        channel: 0,
                        key: 64,
                        velocity: 90
                    }
                ),
                (
                    96,
                    EventKind::NoteOff {
                        channel: 0,
                        key: 60
                    }
                ),
                (
                    96,
                    EventKind::NoteOff {
                        channel: 0,
                        key: 64
                    }
                ),
                (
                    96,
                    EventKind::NoteOn {
                        channel: 1,
                        key: 67,
                        velocity: 80
                    }
                ),
                (
                    144,
                    EventKind::NoteOff {
                        channel: 1,
                        key: 67
                    }
                ),
            ]
        );
    }

    #[test]
    fn meta_and_sysex_cancel_running_status_and_are_skipped() {
        let mut file = header(0, 1, 480);
        file.extend(chunk(&[
            0x00, 0xFF, 0x03, 0x05, b'P', b'i', b'a', b'n', b'o', // name
            0x00, 0xF0, 0x03, 0x7E, 0x7F, 0xF7, // sysex
            0x00, 0xC0, 0x05, // program change: one data byte
            0x00, 0x90, 60, 100, 0x10, 0xFF, 0x01, 0x02, b'h', b'i', // text, ignored
            0x10, 0x80, 60, 0,
        ]));
        let smf = parse(&file).unwrap();
        let track = &smf.tracks[0];
        assert_eq!(track.name.as_deref(), Some("Piano"));
        assert!(track.events.contains(&Event {
            tick: 0,
            kind: EventKind::Program {
                channel: 0,
                program: 5
            }
        }));
        assert_eq!(notes(track).len(), 2);
        assert_eq!(
            notes(track)[1].0,
            0x20,
            "deltas add up across skipped events"
        );

        // A data byte right after a meta event has no status to run on.
        let mut broken = header(0, 1, 480);
        broken.extend(chunk(&[
            0x00, 0x90, 60, 100, 0x00, 0xFF, 0x01, 0x00, 0x00, 62, 100,
        ]));
        assert!(matches!(parse(&broken), Err(SmfError::BadEvent { .. })));
    }

    #[test]
    fn format_1_tempo_map_from_the_first_track() {
        let mut file = header(1, 2, 480);
        file.extend(chunk(&[
            0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, // 500000: 120 BPM
            0x00, 0xFF, 0x58, 0x04, 3, 2, 24, 8, // 3/4
            0x83, 0x60, 0xFF, 0x51, 0x03, 0x0F, 0x42, 0x40, // tick 480: 1000000: 60 BPM
            0x00, 0xFF, 0x2F, 0x00,
        ]));
        file.extend(chunk(&[
            0x00, 0x90, 60, 100, 0x87, 0x40, 0x80, 60, 0, 0x00, 0xFF, 0x2F, 0x00,
        ]));
        let smf = parse(&file).unwrap();
        assert_eq!(smf.tracks.len(), 2);
        assert!(smf.tracks[0].events.iter().any(|e| e.kind
            == EventKind::TimeSignature {
                numerator: 3,
                denominator: 4
            }));
        let map = TempoMap::new(&smf);
        assert_eq!(map.seconds(0), 0.0);
        assert!((map.seconds(240) - 0.25).abs() < 1e-9);
        assert!((map.seconds(480) - 0.5).abs() < 1e-9);
        assert!(
            (map.seconds(960) - 1.5).abs() < 1e-9,
            "a second per quarter after the change"
        );
        assert_eq!(map.tempo_at(479), 500_000);
        assert_eq!(map.tempo_at(480), 1_000_000);
        assert_eq!(notes(&smf.tracks[1])[1].0, 960);
    }

    #[test]
    fn timecode_division_ignores_tempo() {
        // 25 fps, 40 ticks per frame: 1000 ticks a second.
        let division = (((-25i8) as u8 as u16) << 8) | 40;
        let mut file = header(0, 1, division);
        file.extend(chunk(&[
            0x00, 0xFF, 0x51, 0x03, 0x0F, 0x42, 0x40, 0x00, 0xFF, 0x2F, 0x00,
        ]));
        let smf = parse(&file).unwrap();
        assert_eq!(
            smf.timing,
            Timing::Timecode {
                fps: 25,
                ticks_per_frame: 40
            }
        );
        assert!((TempoMap::new(&smf).seconds(500) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn tolerates_missing_end_of_track_and_alien_chunks() {
        let mut file = header(1, 1, 96);
        file.extend_from_slice(b"XFIH");
        file.extend_from_slice(&3u32.to_be_bytes());
        file.extend_from_slice(&[1, 2, 3]);
        file.extend(chunk(&[0x00, 0x90, 60, 100, 0x60, 0x80, 60, 0]));
        let smf = parse(&file).unwrap();
        assert_eq!(notes(&smf.tracks[0]).len(), 2);
    }

    #[test]
    fn rejects_what_isnt_midi() {
        assert_eq!(parse(b"RIFF....WAVE"), Err(SmfError::NotMidi));
        assert_eq!(parse(b""), Err(SmfError::NotMidi));
        assert_eq!(parse(b"MThd\0\0"), Err(SmfError::Truncated));
        let mut zero = header(0, 1, 0);
        zero.extend(chunk(&[0x00, 0xFF, 0x2F, 0x00]));
        assert!(matches!(parse(&zero), Err(SmfError::BadHeader(_))));
    }

    #[test]
    fn writer_round_trips() {
        let mut w = Writer::new(480);
        w.track("Conductor").tempo(0, 90.0).time_signature(0, 6, 8);
        w.track("Right")
            .note(0, 0, 480, 72, 100)
            .note(0, 480, 480, 74, 100);
        w.track("Left").note(1, 0, 960, 48, 80);
        let smf = parse(&w.bytes()).unwrap();
        assert_eq!(smf.format, 1);
        assert_eq!(smf.tracks.len(), 3);
        assert_eq!(smf.tracks[1].name.as_deref(), Some("Right"));
        // Off before On at tick 480, sent with running status off / on.
        assert_eq!(
            notes(&smf.tracks[1]),
            [
                (
                    0,
                    EventKind::NoteOn {
                        channel: 0,
                        key: 72,
                        velocity: 100
                    }
                ),
                (
                    480,
                    EventKind::NoteOff {
                        channel: 0,
                        key: 72
                    }
                ),
                (
                    480,
                    EventKind::NoteOn {
                        channel: 0,
                        key: 74,
                        velocity: 100
                    }
                ),
                (
                    960,
                    EventKind::NoteOff {
                        channel: 0,
                        key: 74
                    }
                ),
            ]
        );
        let map = TempoMap::new(&smf);
        assert!((map.seconds(480) - 60.0 / 90.0).abs() < 1e-6);
        let mut single = Writer::new(96);
        single.note(0, 0, 96, 60, 1);
        assert_eq!(parse(&single.bytes()).unwrap().format, 0);
    }
}
