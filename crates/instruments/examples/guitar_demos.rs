//! Renders the Guitar's demo files: single notes on every string of each
//! model, chords, strums, fingerpicking, a lead line with bends, slides and
//! vibrato, palm-muted chugs, harmonics, dead notes, and the electric
//! through its amp.
//!
//! ```text
//! cargo run --release -p instruments --example guitar_demos            # into demos/guitar/
//! cargo run --release -p instruments --example guitar_demos -- <dir>   # somewhere else
//! cargo run --release -p instruments --example guitar_demos -- <dir> strum   # only names containing "strum"
//! ```
//!
//! Each file is 24-bit stereo WAV at 48 kHz. Renders are deterministic: the
//! same build always writes the same samples.

use std::path::{Path, PathBuf};

use instruments::{Instrument, find_builtin};
use midi::{MOD_WHEEL, NoteEvent, NoteEventKind, Pitch, Velocity};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 256;

/// Key-switches (see the Guitar's module documentation).
const OPEN: u8 = 12;
const PALM_MUTE: u8 = 13;
const HARMONICS: u8 = 14;
const DEAD: u8 = 15;

/// Notes and controller moves in seconds.
#[derive(Default)]
struct Score {
    events: Vec<(f64, NoteEventKind)>,
    length: f64,
}

impl Score {
    fn new(length: f64) -> Self {
        Self {
            events: Vec::new(),
            length,
        }
    }

    /// A note at `at` seconds, `len` long, at MIDI velocity `velocity`.
    fn note(&mut self, at: f64, len: f64, note: u8, velocity: u8) {
        let pitch = Pitch::new(note).unwrap();
        self.events.push((
            at,
            NoteEventKind::On {
                pitch,
                velocity: Velocity::from_midi1(velocity),
            },
        ));
        self.events.push((
            at + len,
            NoteEventKind::Off {
                pitch,
                velocity: Velocity::CENTER,
            },
        ));
    }

    /// Notes of a chord, all at once.
    fn chord(&mut self, at: f64, len: f64, notes: &[u8], velocity: u8) {
        for &note in notes {
            self.note(at, len, note, velocity);
        }
    }

    /// A latched key-switch: a short note below the guitar's range.
    fn switch(&mut self, at: f64, key: u8) {
        self.note(at, 0.05, key, 100);
    }

    fn bend(&mut self, at: f64, semitones: f64) {
        let value = (semitones / 2.0 * 8192.0).round().clamp(-8192.0, 8191.0) as i16;
        self.events.push((at, NoteEventKind::PitchBend(value)));
    }

    /// A bend from `from` to `to` semitones over `time` seconds, eased in
    /// and out as a finger pushes a string.
    fn bend_curve(&mut self, at: f64, time: f64, from: f64, to: f64) {
        let steps = (time * 200.0).ceil().max(1.0) as usize;
        for step in 0..=steps {
            let t = step as f64 / steps as f64;
            let eased = 0.5 - 0.5 * (std::f64::consts::PI * t).cos();
            self.bend(at + t * time, from + (to - from) * eased);
        }
    }

    fn mod_wheel(&mut self, at: f64, value: u8) {
        self.events.push((
            at,
            NoteEventKind::Control {
                controller: MOD_WHEEL,
                value,
            },
        ));
    }
}

fn guitar(settings: &[(&str, f32)]) -> Box<dyn Instrument> {
    let descriptor = find_builtin("canopy.guitar").expect("the guitar is built in");
    let mut guitar = (descriptor.create)();
    guitar.prepare(RATE, BLOCK);
    for &(id, value) in settings {
        // Settings the build doesn't have are skipped, so this also renders
        // older versions of the instrument for comparison.
        if let Some(index) = descriptor.params.iter().position(|p| p.id == id) {
            guitar.set_param(index, value);
        }
    }
    guitar.reset();
    guitar
}

fn has_param(id: &str) -> bool {
    find_builtin("canopy.guitar")
        .unwrap()
        .params
        .iter()
        .any(|p| p.id == id)
}

/// Plays `score`; returns left and right. Builds without key-switches skip
/// them rather than play them as notes.
fn render(settings: &[(&str, f32)], score: &Score) -> (Vec<f32>, Vec<f32>) {
    let mut guitar = guitar(settings);
    let switches = has_param("key_switches");
    let samples = (score.length * f64::from(RATE)) as usize;
    let mut events: Vec<(usize, NoteEventKind)> = score
        .events
        .iter()
        .filter(|(_, kind)| match kind {
            NoteEventKind::On { pitch, .. } | NoteEventKind::Off { pitch, .. } => {
                switches || pitch.get() >= 24
            }
            _ => true,
        })
        .map(|&(at, kind)| ((at * f64::from(RATE)).round() as usize, kind))
        .collect();
    events.sort_by_key(|&(at, kind)| (at, NoteEvent { frame: 0, kind }));
    let (mut left, mut right) = (vec![0.0; samples], vec![0.0; samples]);
    let mut block_events = Vec::new();
    let mut next = 0;
    for start in (0..samples).step_by(BLOCK) {
        let end = (start + BLOCK).min(samples);
        block_events.clear();
        while next < events.len() && events[next].0 < end {
            block_events.push(NoteEvent {
                frame: (events[next].0.max(start) - start) as u32,
                kind: events[next].1,
            });
            next += 1;
        }
        guitar.process(&block_events, &mut left[start..end], &mut right[start..end]);
    }
    (left, right)
}

/// Writes 24-bit stereo PCM, normalised so the loudest sample is at
/// `peak_db` if it would be louder; quiet takes are left alone.
fn write_wav(path: &Path, left: &[f32], right: &[f32]) {
    let peak = left.iter().chain(right).fold(0.0f32, |m, s| m.max(s.abs()));
    let limit = 10f32.powf(-1.0 / 20.0);
    let scale = if peak > limit { limit / peak } else { 1.0 };
    let frames = left.len();
    let data_len = frames * 2 * 3;
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&(RATE as u32).to_le_bytes());
    bytes.extend_from_slice(&((RATE as u32) * 6).to_le_bytes());
    bytes.extend_from_slice(&6u16.to_le_bytes());
    bytes.extend_from_slice(&24u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    for (l, r) in left.iter().zip(right) {
        for sample in [l, r] {
            let value = (sample * scale * 8_388_607.0)
                .round()
                .clamp(-8_388_608.0, 8_388_607.0) as i32;
            bytes.extend_from_slice(&value.to_le_bytes()[..3]);
        }
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
}

/// One demo: a file name, what it shows, the guitar's settings, the score.
struct Demo {
    name: &'static str,
    about: &'static str,
    settings: Vec<(&'static str, f32)>,
    score: Score,
}

const STEEL: f32 = 0.0;
const NYLON: f32 = 1.0;
const ELECTRIC: f32 = 2.0;

/// Open strings low to high, then a few fretted notes (5th and 12th frets
/// of the low E, A and G strings, 5th of the B), each left to ring.
fn single_notes() -> Score {
    let notes = [40, 45, 50, 55, 59, 64, 45, 52, 50, 57, 67, 64];
    let mut score = Score::new(notes.len() as f64 * 2.6 + 1.0);
    for (i, &note) in notes.iter().enumerate() {
        score.note(i as f64 * 2.6, 2.4, note, 100);
    }
    score
}

/// G, C, D, Em, each strummed once and left to ring.
fn open_chords() -> Score {
    let chords: [&[u8]; 4] = [
        &[43, 47, 50, 55, 59, 67],
        &[48, 52, 55, 60, 64],
        &[50, 57, 62, 66],
        &[40, 47, 52, 55, 59, 64],
    ];
    let mut score = Score::new(chords.len() as f64 * 3.0 + 1.0);
    for (i, chord) in chords.iter().enumerate() {
        score.chord(i as f64 * 3.0, 2.8, chord, 105);
    }
    score
}

/// A folk strumming pattern (D D U U D U) over G, Em, C, D at 96 bpm, two
/// bars each: accents on the beats, lighter up-strokes.
fn strum_pattern() -> Score {
    let beat = 60.0 / 96.0;
    let chords: [&[u8]; 4] = [
        &[43, 47, 50, 55, 59, 67],
        &[40, 47, 52, 55, 59, 64],
        &[48, 52, 55, 60, 64],
        &[50, 57, 62, 66],
    ];
    // (eighth of the bar, velocity)
    let pattern = [(0, 110), (2, 96), (3, 70), (5, 74), (6, 100), (7, 72)];
    // Four chords, two bars each.
    let bars = (chords.len() * 2) as f64;
    let mut score = Score::new(bars * 4.0 * beat + 2.5);
    for (c, chord) in chords.iter().enumerate() {
        for bar in 0..2 {
            let start = ((c * 2 + bar) * 4) as f64 * beat;
            for (k, &(eighth, velocity)) in pattern.iter().enumerate() {
                let at = start + eighth as f64 * beat / 2.0;
                let next = pattern.get(k + 1).map_or(8.0, |&(e, _)| f64::from(e as u8));
                let len = (next - eighth as f64) * beat / 2.0 - 0.02;
                score.chord(at, len, chord, velocity);
            }
        }
    }
    // The last chord rings.
    score.chord(bars * 4.0 * beat, 2.4, chords[0], 100);
    score
}

/// p-i-m-a-m-i arpeggios over Am, C, G, E (classical fingerpicking), 84 bpm.
fn fingerpicking() -> Score {
    let beat = 60.0 / 84.0;
    // Bass note, then three treble notes picked i-m-a and back.
    let shapes: [[u8; 4]; 4] = [
        [45, 57, 60, 64],
        [48, 55, 60, 64],
        [43, 55, 59, 67],
        [40, 56, 59, 64],
    ];
    let mut score = Score::new(0.0);
    let mut at = 0.0;
    for shape in shapes.iter().chain(shapes.iter()) {
        let order = [0, 1, 2, 3, 2, 1];
        for (k, &i) in order.iter().enumerate() {
            let velocity = if k == 0 { 92 } else { 70 + (k as u8 * 5) % 14 };
            // Fingers stay down: each note rings until the shape changes.
            let len = (6 - k) as f64 * beat / 3.0;
            score.note(at, len, shape[i], velocity);
            at += beat / 3.0;
        }
    }
    score.chord(at, 3.0, &[45, 57, 60, 64], 85);
    score.length = at + 3.5;
    score
}

/// A blues lick in A: a bend up a whole tone and back, a slide, hammer-ons
/// and pull-offs, and a long note with vibrato from the mod wheel.
fn lead_line() -> Score {
    let mut score = Score::new(9.0);
    let beat = 60.0 / 90.0;
    // A minor pentatonic around the 5th fret.
    score.note(0.0, beat * 0.5, 69, 100); // A4
    score.note(beat * 0.5, beat * 0.5, 72, 96); // C5
    // Bend D5 (7th fret, G string) up to E5 and back.
    score.note(beat, beat * 1.5, 74, 110);
    score.bend_curve(beat + 0.08, 0.22, 0.0, 2.0);
    score.bend_curve(beat + 0.7, 0.25, 2.0, 0.0);
    // Hammer-on C5 -> D5, pull-off D5 -> C5 -> A4 (overlapping notes).
    let t = beat * 2.5;
    score.note(t, beat * 0.3, 72, 96);
    score.note(t + beat * 0.25, beat * 0.3, 74, 90);
    score.note(t + beat * 0.5, beat * 0.3, 72, 90);
    score.note(t + beat * 0.75, beat * 0.5, 69, 92);
    // Slide up from G4 to A4, then E4 (open), then a slide up to E5.
    let t = beat * 4.0;
    score.note(t, beat * 0.55, 67, 100);
    score.note(t + beat * 0.5, beat * 0.6, 69, 96);
    score.note(t + beat * 1.25, beat * 0.4, 64, 90);
    score.note(t + beat * 1.75, beat * 0.4, 72, 96);
    score.note(t + beat * 2.1, beat * 3.0, 76, 104);
    // Vibrato on the long note.
    let t = t + beat * 2.6;
    for k in 0..20 {
        score.mod_wheel(t + k as f64 * 0.03, (k * 6) as u8);
    }
    score.mod_wheel(t + beat * 2.4, 0);
    score
}

/// Down-picked eighth-note chugs on an E5 power chord, palm-muted, with
/// open accents on the 4 and an A5 to finish.
fn palm_mute_chugs() -> Score {
    let beat = 60.0 / 120.0;
    let e5 = [40, 47, 52];
    let a5 = [45, 52, 57];
    let mut score = Score::new(0.0);
    for bar in 0..3 {
        let start = 0.1 + f64::from(bar) * 4.0 * beat;
        // Three beats of muted eighths, accented on the beat...
        score.switch(start - 0.02, PALM_MUTE);
        for eighth in 0..6 {
            let velocity = if eighth % 2 == 0 { 112 } else { 90 };
            score.chord(
                start + f64::from(eighth) * beat / 2.0,
                beat * 0.45,
                &e5,
                velocity,
            );
        }
        // ...then the palm comes off for an open chord on the fourth.
        score.switch(start + 3.0 * beat - 0.02, OPEN);
        score.chord(start + 3.0 * beat, beat * 0.95, &e5, 115);
    }
    let end = 0.1 + 12.0 * beat;
    score.chord(end, 2.2, &a5, 118);
    score.length = end + 2.8;
    score
}

/// Natural harmonics at the 12th, 7th and 5th frets on the low strings,
/// then a few on the high strings.
fn harmonics() -> Score {
    let mut score = Score::new(12.0);
    score.switch(0.0, HARMONICS);
    // 12th fret harmonics sound an octave up, 7th an octave and a fifth,
    // 5th two octaves.
    let notes = [52, 57, 62, 59, 64, 69, 64, 69, 74, 76];
    for (i, &note) in notes.iter().enumerate() {
        score.note(0.1 + i as f64 * 1.0, 0.9, note, 96);
    }
    score.switch(10.5, OPEN);
    score
}

/// A funk rhythm in E9: sixteenths with dead-note scratches between
/// chord stabs.
fn dead_notes_funk() -> Score {
    let sixteenth = 60.0 / 100.0 / 4.0;
    let e9 = [52, 56, 62, 66, 71];
    let mut score = Score::new(32.0 * sixteenth + 1.5);
    // 1 = chord, 0 = dead scratch, rest otherwise.
    let pattern = [1, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 1, 0, 0, 1, 0];
    for bar in 0..2 {
        for (k, &hit) in pattern.iter().enumerate() {
            let at = (bar * 16 + k) as f64 * sixteenth;
            if hit == 1 {
                score.switch(at - 0.005, OPEN);
                score.chord(at + 0.002, sixteenth * 0.8, &e9, 108);
            } else {
                score.switch(at - 0.005, DEAD);
                score.chord(at + 0.002, sixteenth * 0.6, &e9, 70 + (k as u8 * 7) % 20);
            }
        }
    }
    score.switch(32.0 * sixteenth, OPEN);
    score
}

/// The same note eight times: every pluck is a little different.
fn repeated_notes() -> Score {
    let mut score = Score::new(5.0);
    for k in 0..8 {
        score.note(k as f64 * 0.5, 0.45, 57, 100);
    }
    score
}

fn demos(full: bool) -> Vec<Demo> {
    let electric = |pickup: f32, extra: &[(&'static str, f32)]| {
        let mut settings = vec![("model", ELECTRIC), ("pickup", pickup)];
        settings.extend_from_slice(extra);
        settings
    };
    let mut demos = vec![
        Demo {
            name: "01_single_notes_steel",
            about: "steel-string acoustic: the six open strings, then fretted notes",
            settings: vec![("model", STEEL)],
            score: single_notes(),
        },
        Demo {
            name: "02_single_notes_nylon",
            about: "nylon-string classical, fingers: the same notes",
            settings: vec![("model", NYLON)],
            score: single_notes(),
        },
        Demo {
            name: "03_single_notes_electric_di",
            about: "electric, middle pickup, straight into the desk (no amp)",
            settings: electric(2.0, &[]),
            score: single_notes(),
        },
        Demo {
            name: "04_open_chords_steel",
            about: "steel-string acoustic: G, C, D and Em strummed and left to ring",
            settings: vec![("model", STEEL)],
            score: open_chords(),
        },
        Demo {
            name: "05_strum_pattern_steel",
            about: "steel-string acoustic: a D D U U D U pattern over G Em C D",
            settings: vec![("model", STEEL)],
            score: strum_pattern(),
        },
        Demo {
            name: "06_fingerpicking_nylon",
            about: "nylon-string classical: p-i-m-a arpeggios over Am C G E",
            settings: vec![("model", NYLON)],
            score: fingerpicking(),
        },
        Demo {
            name: "07_lead_electric_clean_amp",
            about: "electric, neck pickup, clean amp: bends, hammer-ons, pull-offs, slides, vibrato",
            settings: electric(0.0, &[("amp", 1.0), ("drive", 0.4), ("vibrato", 35.0)]),
            score: lead_line(),
        },
        Demo {
            name: "08_lead_electric_lead_amp",
            about: "the same lick, bridge pickup into the lead channel",
            settings: electric(4.0, &[("amp", 3.0), ("drive", 0.55), ("vibrato", 40.0)]),
            score: lead_line(),
        },
        Demo {
            name: "09_palm_mute_chugs_crunch",
            about: "electric, bridge pickup, crunch amp: palm-muted chugs with open accents",
            settings: electric(4.0, &[("amp", 2.0), ("drive", 0.7)]),
            score: palm_mute_chugs(),
        },
        Demo {
            name: "10_chords_electric_clean_amp",
            about: "electric, neck and middle pickups, clean amp: the strum pattern",
            settings: electric(1.0, &[("amp", 1.0), ("drive", 0.3)]),
            score: strum_pattern(),
        },
        Demo {
            name: "11_lead_steel_acoustic",
            about: "the lead lick on the steel-string acoustic",
            settings: vec![("model", STEEL), ("vibrato", 30.0)],
            score: lead_line(),
        },
        Demo {
            name: "12_repeated_notes_steel",
            about: "one note picked eight times: no two plucks alike",
            settings: vec![("model", STEEL)],
            score: repeated_notes(),
        },
    ];
    if full {
        demos.extend([
            Demo {
                name: "13_harmonics_steel",
                about: "natural harmonics at the 12th, 7th and 5th frets (key-switch D0)",
                settings: vec![("model", STEEL)],
                score: harmonics(),
            },
            Demo {
                name: "14_dead_notes_funk_electric",
                about: "electric, middle and bridge pickups, clean amp: chord stabs and dead-note scratches (key-switch D#0)",
                settings: electric(3.0, &[("amp", 1.0), ("drive", 0.35)]),
                score: dead_notes_funk(),
            },
        ]);
    }
    demos
}

/// One note, left to ring for `seconds`, for comparing with a recording.
fn single(note: u8, velocity: u8, seconds: f64) -> Score {
    let mut score = Score::new(seconds);
    score.note(0.05, seconds - 0.5, note, velocity);
    score
}

/// Single notes of each model, one file each, to measure next to
/// recordings of real guitars (or listen to beside them): the six open
/// strings of the steel-string, the classical (fingers) and the electric
/// (neck pickup, no amp), each left to ring.
fn comparison(out: &Path, filter: Option<&str>) {
    let models: [(&str, Vec<(&'static str, f32)>); 3] = [
        ("steel", vec![("model", STEEL)]),
        ("nylon", vec![("model", NYLON)]),
        (
            "electric_neck_di",
            vec![("model", ELECTRIC), ("pickup", 0.0)],
        ),
    ];
    for (name, settings) in &models {
        for note in [40u8, 45, 50, 55, 59, 64] {
            let file = format!("{name}_{note}");
            if filter.is_some_and(|f| !file.contains(f)) {
                continue;
            }
            let (left, right) = render(settings, &single(note, 100, 6.0));
            write_wav(&out.join(format!("{file}.wav")), &left, &right);
            println!("{file}");
        }
    }
    // Palm-muted notes on the electric, key-switched.
    for note in [40u8, 45, 50] {
        let file = format!("electric_neck_di_palm_mute_{note}");
        if filter.is_some_and(|f| !file.contains(f)) {
            continue;
        }
        let mut score = single(note, 100, 2.0);
        score.switch(0.0, PALM_MUTE);
        let settings = [("model", ELECTRIC), ("pickup", 0.0)];
        let (left, right) = render(&settings, &score);
        write_wav(&out.join(format!("{file}.wav")), &left, &right);
        println!("{file}");
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out: PathBuf = args.next().map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demos/guitar"),
        PathBuf::from,
    );
    let filter = args.next();
    std::fs::create_dir_all(&out).expect("creating the output folder");
    if filter.as_deref() == Some("--compare") {
        comparison(&out, args.next().as_deref());
        return;
    }
    // Articulations need the key-switches; older builds play only the rest.
    let full = has_param("key_switches");
    for demo in demos(full) {
        if filter.as_deref().is_some_and(|f| !demo.name.contains(f)) {
            continue;
        }
        let (left, right) = render(&demo.settings, &demo.score);
        let path = out.join(format!("{}.wav", demo.name));
        write_wav(&path, &left, &right);
        let peak = left
            .iter()
            .chain(&right)
            .fold(0.0f32, |m, s| m.max(s.abs()));
        println!(
            "{:<34} peak {:>6.1} dBFS  {}",
            demo.name,
            20.0 * peak.max(1e-9).log10(),
            demo.about
        );
    }
    let _ = (OPEN, PALM_MUTE, HARMONICS, DEAD);
}
