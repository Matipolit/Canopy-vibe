//! Runs one effect over noise for a while, to time it and to profile it
//! (`tools/sample.py`), and can save what it rendered so a change can be
//! checked against what the effect did before.
//!
//! ```sh
//! cargo run --release -p effects --example profile_effect -- canopy.reverb [seconds] \
//!     [param=value ...] [--out file.f32] [--compare file.f32]
//! ```
//!
//! `--out` writes the stereo output as interleaved little-endian f32;
//! `--compare` reads such a file and reports the largest difference from
//! it, so an optimisation can show it changed nothing (or how little).

use std::time::Instant;

use effects::{Context, find_builtin};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 64;

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().expect("an effect id, e.g. canopy.reverb");
    let mut seconds = 20.0f32;
    let mut settings = Vec::new();
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
        } else if let Some((param, value)) = arg.split_once('=') {
            settings.push((param.to_owned(), value.parse::<f32>().expect("a number")));
        } else {
            seconds = arg.parse().expect("seconds");
        }
    }
    let descriptor = find_builtin(&id).expect("a built-in effect");
    let mut effect = (descriptor.create)();
    effect.prepare(RATE, BLOCK);
    for (param, value) in &settings {
        let index = effect
            .params()
            .iter()
            .position(|p| p.id == param)
            .unwrap_or_else(|| panic!("{id} has no parameter {param}"));
        effect.set_param(index, *value);
    }
    let len = (RATE * seconds) as usize;
    let mut seed = 0x2545_f491u32;
    let mut noise = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        0.5 * (seed as f32 / u32::MAX as f32 * 2.0 - 1.0)
    };
    // Noise with some tone in it, so dynamics and filters have something
    // to follow, and a stereo difference.
    let mut left: Vec<f32> = (0..len)
        .map(|i| 0.6 * noise() + 0.3 * (i as f32 * 0.02).sin())
        .collect();
    let mut right: Vec<f32> = (0..len)
        .map(|i| 0.6 * noise() + 0.3 * (i as f32 * 0.021).sin())
        .collect();
    let sidechain_l: Vec<f32> = (0..len).map(|_| noise()).collect();
    let sidechain_r: Vec<f32> = (0..len).map(|_| noise()).collect();
    let (dry_l, dry_r) = (left.clone(), right.clone());
    let start = Instant::now();
    for pass in 0..repeat {
        if pass > 0 {
            left.copy_from_slice(&dry_l);
            right.copy_from_slice(&dry_r);
        }
        for (block, (l, r)) in left
            .chunks_mut(BLOCK)
            .zip(right.chunks_mut(BLOCK))
            .enumerate()
        {
            let at = block * BLOCK;
            let context = Context {
                beat: at as f64 / RATE as f64 * 2.0,
                sidechain: descriptor.sidechain.then(|| {
                    (
                        &sidechain_l[at..at + l.len()],
                        &sidechain_r[at..at + r.len()],
                    )
                }),
                ..Context::DEFAULT
            };
            effect.process(l, r, &context);
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
