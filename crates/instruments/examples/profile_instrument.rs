//! Plays one instrument for a while, to time it and to profile it
//! (`tools/sample.py`), and can save what it rendered so a change can be
//! checked against what the instrument did before.
//!
//! ```sh
//! cargo run --release -p instruments --example profile_instrument -- canopy.wavetable [seconds] \
//!     [param=value ...] [--notes 36,38,42] [--out file.f32] [--compare file.f32]
//! ```
//!
//! Notes (default: a six-note chord) are struck every half second and
//! held for a third of a second, so attacks, sustain and release all run.
//! `--out` writes the stereo output as interleaved little-endian f32;
//! `--compare` reads such a file and reports the largest difference.

use std::time::Instant;

use instruments::find_builtin;
use midi::{NoteEvent, NoteEventKind, Pitch, Velocity};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 64;

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args
        .next()
        .expect("an instrument id, e.g. canopy.wavetable");
    let mut seconds = 20.0f32;
    let mut settings = Vec::new();
    let mut notes = vec![48u8, 55, 60, 64, 67, 72];
    let mut out = None;
    let mut compare = None;
    let mut repeat = 1usize;
    while let Some(arg) = args.next() {
        if arg == "--repeat" {
            // Runs the same stretch over again, for a profiler that needs
            // seconds of work without the memory of a long render.
            repeat = args.next().and_then(|n| n.parse().ok()).expect("a count");
        } else if arg == "--out" {
            out = args.next();
        } else if arg == "--compare" {
            compare = args.next();
        } else if arg == "--notes" {
            notes = args
                .next()
                .expect("notes")
                .split(',')
                .map(|n| n.parse().expect("a MIDI note"))
                .collect();
        } else if let Some((param, value)) = arg.split_once('=') {
            settings.push((param.to_owned(), value.parse::<f32>().expect("a number")));
        } else {
            seconds = arg.parse().expect("seconds");
        }
    }
    let descriptor = find_builtin(&id).expect("a built-in instrument");
    let mut instrument = (descriptor.create)();
    instrument.prepare(RATE, BLOCK);
    for (param, value) in &settings {
        let index = instrument
            .params()
            .iter()
            .position(|p| p.id == param)
            .unwrap_or_else(|| panic!("{id} has no parameter {param}"));
        instrument.set_param(index, *value);
    }
    instrument.reset();
    let len = (RATE * seconds) as usize;
    let mut left = vec![0.0f32; len];
    let mut right = vec![0.0f32; len];
    let on_every = RATE as usize / 2;
    let hold = RATE as usize / 3;
    let start = Instant::now();
    for _ in 0..repeat {
        for (block, (l, r)) in left
            .chunks_mut(BLOCK)
            .zip(right.chunks_mut(BLOCK))
            .enumerate()
        {
            let at = block * BLOCK;
            let mut events = Vec::new();
            for frame in at..at + l.len() {
                let phase = frame % on_every;
                if phase == 0 || phase == hold {
                    for (i, note) in notes.iter().enumerate() {
                        // Velocities vary a little, so round-robins and
                        // velocity layers run.
                        let kind = if phase == 0 {
                            NoteEventKind::On {
                                pitch: Pitch::new(*note).unwrap(),
                                velocity: Velocity::from_midi1(90 + (i as u8 * 7) % 30),
                            }
                        } else {
                            NoteEventKind::Off {
                                pitch: Pitch::new(*note).unwrap(),
                                velocity: Velocity::CENTER,
                            }
                        };
                        events.push(NoteEvent {
                            frame: (frame - at) as u32,
                            kind,
                        });
                    }
                }
            }
            instrument.process(&events, l, r);
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let rendered = f64::from(seconds) * repeat as f64;
    println!(
        "{id}: {rendered} s of audio in {:.3} s, {:.3}% of one core",
        elapsed,
        100.0 * elapsed / rendered
    );
    let interleaved: Vec<f32> = left
        .iter()
        .zip(&right)
        .flat_map(|(l, r)| [*l, *r])
        .collect();
    if let Some(path) = out {
        let bytes: Vec<u8> = interleaved.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(&path, bytes).expect("write the output");
        println!("wrote {path}");
    }
    if let Some(path) = compare {
        let bytes = std::fs::read(&path).expect("read the comparison");
        let before: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        assert_eq!(before.len(), interleaved.len(), "different lengths");
        let (mut worst, mut at) = (0.0f32, 0);
        for (i, (a, b)) in before.iter().zip(&interleaved).enumerate() {
            let d = (a - b).abs();
            if d > worst {
                worst = d;
                at = i;
            }
        }
        println!(
            "largest difference from {path}: {worst:e} at frame {}{}",
            at / 2,
            if worst == 0.0 { " (identical)" } else { "" }
        );
    }
}
