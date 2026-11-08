//! Runs one effect over a recording with its knobs set, to hold the result
//! against another program's render of the same recording.
//!
//! ```sh
//! cargo run --release -p effects --example apply_effect -- canopy.reverb 44100 \
//!     in.f32 out.f32 [param=value ...] [--tail seconds]
//! ```
//!
//! Both files are stereo, interleaved little-endian f32, at the rate given.
//! Choices take their index. The knobs are set before the first sample and
//! the effect reset, so nothing glides into place; `--tail` adds that many
//! seconds of silence after the recording, for what rings on.

use effects::{Context, find_builtin};

const BLOCK: usize = 64;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [id, rate, input, output, rest @ ..] = args.as_slice() else {
        eprintln!("usage: apply_effect <id> <rate> <in.f32> <out.f32> [param=value ...]");
        std::process::exit(2);
    };
    let rate: f32 = rate.parse().expect("a sample rate");
    let descriptor = find_builtin(id).unwrap_or_else(|| panic!("no built-in effect {id}"));
    let mut effect = (descriptor.create)();
    effect.prepare(rate, BLOCK);
    let mut tail = 0.0f32;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        if arg == "--tail" {
            tail = rest.next().and_then(|s| s.parse().ok()).expect("--tail seconds");
            continue;
        }
        let (param, value) = arg.split_once('=').expect("param=value");
        let index = effect
            .params()
            .iter()
            .position(|p| p.id == param)
            .unwrap_or_else(|| panic!("{id} has no parameter {param}"));
        effect.set_param(index, value.parse().expect("a number"));
    }
    effect.reset();

    let bytes = std::fs::read(input).expect("read the input");
    let samples: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    let extra = (tail * rate) as usize;
    let mut left: Vec<f32> = samples.iter().step_by(2).copied().collect();
    let mut right: Vec<f32> = samples.iter().skip(1).step_by(2).copied().collect();
    left.resize(left.len() + extra, 0.0);
    right.resize(right.len() + extra, 0.0);
    for (block, (l, r)) in left
        .chunks_mut(BLOCK)
        .zip(right.chunks_mut(BLOCK))
        .enumerate()
    {
        let at = block * BLOCK;
        let context = Context {
            beat: at as f64 / f64::from(rate) * 2.0,
            ..Context::DEFAULT
        };
        effect.process(l, r, &context);
    }
    let out: Vec<u8> = left
        .iter()
        .zip(&right)
        .flat_map(|(l, r)| [*l, *r])
        .flat_map(f32::to_le_bytes)
        .collect();
    std::fs::write(output, out).expect("write the output");
}
