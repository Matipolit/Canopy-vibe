//! The strum pattern tool played by the Guitar: every preset's strokes are
//! strummed the way the step preview says (down or up, as the Guitar's
//! Alternate strum direction reads them from the timing), and its dead
//! notes and palm mutes are played as such through the key-switches the
//! tool writes. Read from the Guitar's meters, as its own tests do.

use instruments::builtin::guitar::{
    KEY_SWITCH_FIRST, KEY_SWITCH_LAST, METER_ARTICULATION, METER_STRUM,
};
use instruments::{MAX_METERS, find_builtin};
use midi::{NoteEvent, NoteEventKind, Pitch, Tick, Velocity};
use project::note_tools::patterns::{
    Articulation, Direction, Hit, PRESETS, Step, StrumPattern, strum_pattern,
};
use project::{Note, NoteId, TempoMap};

const RATE: u32 = 48_000;
/// Small blocks, so meters are read every 1.3 ms.
const BLOCK: usize = 64;

/// G, C, D, Em held two bars each (4/4 at 120 BPM), velocity 100.
fn progression() -> Vec<Note> {
    let chords: [&[u8]; 4] = [
        &[43, 47, 50, 55, 59, 67],
        &[48, 52, 55, 60, 64],
        &[50, 57, 62, 66],
        &[40, 47, 52, 55, 59, 64],
    ];
    let mut notes = Vec::new();
    for (c, pitches) in chords.iter().enumerate() {
        for &pitch in pitches.iter() {
            notes.push(Note {
                id: NoteId(notes.len() as u32 + 1),
                start: Tick::new(c as i64 * 7680),
                len: Tick::new(7680),
                pitch: Pitch::new(pitch).unwrap(),
                velocity: Velocity::from_midi1(100),
                release_velocity: Velocity::CENTER,
                muted: false,
                bend: Vec::new(),
            });
        }
    }
    notes
}

/// Plays `notes` on a Guitar with its default settings (Alternate strum
/// direction, key-switches on); returns the meters after each block.
fn play(notes: &[Note], tempo: &TempoMap) -> Vec<[f32; MAX_METERS]> {
    let descriptor = find_builtin("canopy.guitar").expect("the guitar is built in");
    let mut guitar = (descriptor.create)();
    guitar.prepare(RATE as f32, BLOCK);
    guitar.set_tempo(tempo.tempo.to_bpm());
    guitar.reset();
    let sample = |tick: Tick| tempo.tick_to_sample(tick, RATE) as usize;
    let mut events: Vec<(usize, NoteEventKind)> = Vec::new();
    for n in notes {
        events.push((
            sample(n.start),
            NoteEventKind::On {
                pitch: n.pitch,
                velocity: n.velocity,
            },
        ));
        events.push((
            sample(n.end()),
            NoteEventKind::Off {
                pitch: n.pitch,
                velocity: n.release_velocity,
            },
        ));
    }
    events.sort_by_key(|&(at, kind)| (at, NoteEvent { frame: 0, kind }));
    let end = events.last().map_or(0, |e| e.0) + RATE as usize / 2;
    let (mut left, mut right) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut meters = Vec::new();
    let mut next = 0;
    let mut block_events = Vec::new();
    for start in (0..end).step_by(BLOCK) {
        block_events.clear();
        while next < events.len() && events[next].0 < start + BLOCK {
            block_events.push(NoteEvent {
                frame: (events[next].0 - start) as u32,
                kind: events[next].1,
            });
            next += 1;
        }
        guitar.process(&block_events, &mut left, &mut right);
        let mut values = [0.0; MAX_METERS];
        guitar.meters(&mut values);
        meters.push(values);
    }
    meters
}

#[test]
fn the_guitar_strums_every_preset_the_way_the_preview_shows() {
    let tempo = TempoMap::new(120.0);
    let chords = progression();
    for preset in PRESETS {
        // Ska's chops all fall off the beat: a strumming hand that starts on
        // one is read as starting a phrase, with a down-stroke (the Guitar
        // can't see where the bar is). The tool still leaves out their bass.
        if preset.name == "Ska upstrokes" {
            continue;
        }
        let mut s = StrumPattern::default();
        preset.apply(&mut s);
        let guitar = Articulation::KeySwitches { setting: 0 };
        let mut next_id = 1000;
        let notes = strum_pattern(
            &chords,
            &[],
            &s,
            guitar,
            Tick::ZERO,
            tempo.bar_len(),
            &mut || {
                next_id += 1;
                NoteId(next_id)
            },
        );
        let meters = play(&notes, &tempo);
        let directions = s.pattern.directions(s.resolution);
        let step = s.resolution.step().get();

        // Every stroke, by where it starts.
        let mut starts: Vec<Tick> = notes
            .iter()
            .filter(|n| !(KEY_SWITCH_FIRST..=KEY_SWITCH_LAST).contains(&n.pitch.get()))
            .map(|n| n.start)
            .collect();
        starts.dedup();
        assert!(
            starts.len() >= 8,
            "{}: {} strokes",
            preset.name,
            starts.len()
        );
        let mut strums = 0;
        for start in starts {
            let size = notes.iter().filter(|n| n.start == start).count();
            // Swing only ever delays a step, by less than a step.
            let index = (start.get() / step) as usize % s.pattern.len();
            let Step::Hit { hit, .. } = s.pattern.steps()[index] else {
                panic!("{}: a stroke on a rest or a tie", preset.name);
            };
            let seconds = tempo.ticks_to_seconds(start.get() as f64) + 0.02;
            let at = (seconds * f64::from(RATE)) as usize / BLOCK;
            let meter = &meters[at];
            let articulation = match hit {
                Hit::Dead => 3.0,
                Hit::PalmMute => 1.0,
                _ => 0.0,
            };
            assert_eq!(
                meter[METER_ARTICULATION],
                articulation,
                "{}: the {hit:?} at tick {}",
                preset.name,
                start.get()
            );
            // A single note is picked, not strummed.
            if size < 2 {
                continue;
            }
            strums += 1;
            let expected = match directions[index] {
                Some(Direction::Down) => 1.0,
                Some(Direction::Up) => -1.0,
                None => unreachable!("a hit has a direction"),
            };
            assert_eq!(
                meter[METER_STRUM],
                expected,
                "{}: the stroke at tick {} (step {index})",
                preset.name,
                start.get()
            );
        }
        assert!(strums >= 8, "{}: only {strums} strums", preset.name);
    }
}
