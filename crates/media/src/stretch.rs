//! Time stretching with Signalsmith Stretch, lined up so the result has no
//! delay.
//!
//! Signalsmith Stretch (MIT) is a phase vocoder with phase locking across
//! both time and frequency. On tonal material it sounds clearly better than
//! the pure-Rust options, and it keeps hits in time; they do smear a little
//! at big ratios, which is what Beats mode is for.

use ssstretch::Stretch;

/// Analysis block and hop, in seconds. Shorter than the library's default
/// (120 and 30 ms): on test melodies this tracks note changes better and
/// leaves less noise between partials, at twice the work, which is still
/// well under a second for 30 s of audio.
const BLOCK: f64 = 0.09;
const INTERVAL: f64 = 0.015;

/// The stretcher randomizes phases a little when stretching more than about
/// twice; a fixed seed makes the same settings render the same audio.
const SEED: i64 = 0x5eed;

/// Output frames handed to the stretcher per call. It places its blocks in
/// single precision from the start of each call, so very long calls would
/// lose sub-sample accuracy.
const CHUNK: usize = 1 << 14;

/// Plays `material` `speed` times faster at its original pitch: output frame
/// `n` is centred on material position `(from + n) * speed`.
///
/// `material(start, len)` returns `len` frames of both channels from frame
/// `start`, which may be negative. The stretcher looks a block or two (90 ms
/// each) past either end of what it plays.
///
/// Transposing is left to the caller (resampling the result). The library
/// can shift pitch itself, but measured on steady tones it lands a few cents
/// off, by an amount that varies with the shift and the note.
pub(crate) fn stretch(
    rate: u32,
    speed: f64,
    from: i64,
    frames: usize,
    material: impl FnOnce(i64, usize) -> (Vec<f32>, Vec<f32>),
) -> (Vec<f32>, Vec<f32>) {
    assert!(speed > 0.0 && speed.is_finite(), "speed must be positive");
    if frames == 0 {
        return (Vec::new(), Vec::new());
    }
    let mut stretch = Stretch::with_seed(SEED);
    let rate = rate as f64;
    stretch.configure(2, (BLOCK * rate) as i32, (INTERVAL * rate) as i32);
    let block = stretch.block_samples() as usize;
    let interval = stretch.interval_samples() as usize;
    let input_latency = stretch.input_latency() as i64;
    let output_latency = stretch.output_latency() as usize;

    // After a seek the stretcher's processing time is `input_latency` frames
    // before the end of what it was given, and the block it builds there
    // comes out `output_latency` frames later. Start half a block and a hop
    // early, so the first frames we keep overlap blocks of real audio like
    // all the others, and drop what comes out before them.
    let lead = block / 2 + interval;
    let skip = output_latency + lead;
    let start = ((from - lead as i64) as f64 * speed).round() as i64 + input_latency;
    let seek = (block + interval) as i64;
    let total = skip + frames;
    let input_end = |output: usize| (output as f64 * speed).round() as usize;
    let (left, right) = material(start - seek, seek as usize + input_end(total));

    let (seek, input) = (seek as usize, [left, right]);
    let preroll = [input[0][..seek].to_vec(), input[1][..seek].to_vec()];
    stretch.seek_vec(&preroll, seek as i32, speed);

    let mut out_left = Vec::with_capacity(total);
    let mut out_right = Vec::with_capacity(total);
    let mut outputs = [Vec::new(), Vec::new()];
    for chunk in (0..total).step_by(CHUNK) {
        let end = (chunk + CHUNK).min(total);
        let (a, b) = (seek + input_end(chunk), seek + input_end(end));
        let inputs = [input[0][a..b].to_vec(), input[1][a..b].to_vec()];
        stretch.process_vec(&inputs, (b - a) as i32, &mut outputs, (end - chunk) as i32);
        out_left.extend_from_slice(&outputs[0]);
        out_right.extend_from_slice(&outputs[1]);
    }
    out_left.drain(..skip);
    out_right.drain(..skip);
    (out_left, out_right)
}
