//! What the effects' integration tests share: reproducible noise and
//! random settings, effects built from named settings, runs that check
//! every sample, and CPU timings.

#![allow(dead_code)]

use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use effects::{Context, Effect, MAX_METERS, find_builtin};
use instruments::{ParamDescriptor, ParameterKind};

/// The creative effects, which the shared tests reach less of.
pub const CREATIVE: [&str; 10] = [
    "canopy.waveshaper",
    "canopy.shaper",
    "canopy.chorus",
    "canopy.vocoder",
    "canopy.analyzer",
    "canopy.filter",
    "canopy.distortion",
    "canopy.hyper",
    "canopy.frequency_shifter",
    "canopy.convolve",
];

/// The dynamics effects.
pub const DYNAMICS: [&str; 5] = [
    "canopy.eq",
    "canopy.compressor",
    "canopy.limiter",
    "canopy.multiband",
    "canopy.transient",
];

/// Reproducible random numbers (xorshift).
pub struct Random(u32);

impl Random {
    pub fn new(seed: u32) -> Random {
        Random(seed.max(1))
    }

    /// In `0..=1`.
    pub fn unit(&mut self) -> f32 {
        dsp::xorshift32(&mut self.0) as f32 / u32::MAX as f32
    }

    /// In `-1..=1`.
    pub fn bipolar(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }

    /// Somewhere in `param`'s range.
    pub fn value(&mut self, param: &ParamDescriptor) -> f32 {
        let (min, max) = range(param);
        min + (max - min) * self.unit()
    }

    /// One of `len` indices.
    pub fn index(&mut self, len: usize) -> usize {
        (self.unit() * len as f32) as usize % len
    }
}

/// `len` samples of white noise from `seed`, up to `level`.
pub fn noise(len: usize, seed: u32, level: f32) -> Vec<f32> {
    let mut random = Random::new(seed);
    (0..len).map(|_| level * random.bipolar()).collect()
}

/// The lowest and highest value `param` takes.
pub fn range(param: &ParamDescriptor) -> (f32, f32) {
    match param.kind {
        ParameterKind::Float { min, max, .. } => (min, max),
        ParameterKind::Enum { labels, .. } => (0.0, (labels.len() - 1) as f32),
    }
}

/// Sets the parameter called `id`.
pub fn set(effect: &mut dyn Effect, id: &str, value: f32) {
    let index = effect
        .params()
        .iter()
        .position(|p| p.id == id)
        .unwrap_or_else(|| panic!("no parameter {id}"));
    effect.set_param(index, value);
}

/// The built-in `id`, prepared at `rate` for blocks of up to `block`, set
/// as `settings` say.
pub fn effect(id: &str, rate: f32, block: usize, settings: &[(&str, f32)]) -> Box<dyn Effect> {
    let mut effect = (find_builtin(id).unwrap_or_else(|| panic!("no {id}")).create)();
    effect.prepare(rate, block);
    for &(param, value) in settings {
        set(effect.as_mut(), param, value);
    }
    effect
}

/// The latency the built-in `id` declares at `rate` when set as
/// `settings` say.
pub fn declared_latency(id: &str, settings: &[(&str, f32)], rate: f32) -> usize {
    let descriptor = find_builtin(id).unwrap();
    let mut values: Vec<f32> = descriptor
        .params
        .iter()
        .map(|p| p.default_value())
        .collect();
    for (param, value) in settings {
        values[descriptor
            .params
            .iter()
            .position(|p| p.id == *param)
            .unwrap()] = *value;
    }
    (descriptor.latency)(&values, rate)
}

/// Processes `input` on the left and reversed on the right in blocks of
/// `block`, with `side` as the sidechain if there is one, asserting that
/// every sample is finite and within `limit` and every meter finite.
pub fn run_checked(
    effect: &mut dyn Effect,
    input: &[f32],
    side: Option<&[f32]>,
    block: usize,
    context: &Context,
    limit: f32,
    what: &str,
) -> (Vec<f32>, Vec<f32>) {
    let mut left = input.to_vec();
    let mut right: Vec<f32> = input.iter().rev().copied().collect();
    let mut meters = [0.0; MAX_METERS];
    for start in (0..input.len()).step_by(block) {
        let end = (start + block).min(input.len());
        let context = Context {
            sidechain: side.map(|s| (&s[start..end], &s[start..end])),
            ..*context
        };
        effect.process(&mut left[start..end], &mut right[start..end], &context);
        for s in left[start..end].iter().chain(&right[start..end]) {
            assert!(s.is_finite() && s.abs() <= limit, "{what}: {s}");
        }
        effect.meters(&mut meters);
        assert!(
            meters.iter().all(|m| m.is_finite()),
            "{what}: meters {meters:?}"
        );
    }
    (left, right)
}

/// The loudest sample in `left` and `right`.
pub fn loudest(left: &[f32], right: &[f32]) -> f32 {
    left.iter().chain(right).fold(0.0f32, |p, s| p.max(s.abs()))
}

/// Holds timings in one test binary apart: two at once would slow each
/// other down.
pub fn timing() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether timing limits count: only release builds time anything worth
/// checking.
pub const RELEASE: bool = !cfg!(debug_assertions);

/// The seconds of noise [`load`] times.
pub const SECONDS: usize = 5;

/// Seconds of processing per second of audio, as a percentage: the best of
/// three runs of [`SECONDS`] of stereo noise at 48 kHz in 64-frame blocks
/// through `id` set as `settings` say, with a noise sidechain if
/// `sidechained` and the transport rolling if `playing`.
pub fn load(id: &str, settings: &[(&str, f32)], sidechained: bool, playing: bool) -> f64 {
    const RATE: f32 = 48_000.0;
    const BLOCK: usize = 64;
    let len = RATE as usize * SECONDS;
    let input = noise(len, 0x2545_f491, 0.5);
    let side = noise(len, 0x1b87_3593, 0.5);
    (0..3)
        .map(|_| {
            let mut effect = effect(id, RATE, BLOCK, settings);
            let (mut left, mut right) = (input.clone(), input.clone());
            let start = Instant::now();
            for (n, (l, r)) in left
                .chunks_mut(BLOCK)
                .zip(right.chunks_mut(BLOCK))
                .enumerate()
            {
                let at = n * BLOCK;
                let side = &side[at..at + l.len()];
                let context = Context {
                    bpm: 120.0,
                    beat: at as f64 / f64::from(RATE) * 2.0,
                    playing,
                    sidechain: sidechained.then_some((side, side)),
                };
                effect.process(l, r, &context);
            }
            let elapsed = start.elapsed().as_secs_f64();
            std::hint::black_box(&left);
            100.0 * elapsed / SECONDS as f64
        })
        .fold(f64::INFINITY, f64::min)
}

/// Prints which build the timings come from.
pub fn print_build() {
    println!(
        "{} build: CPU for {SECONDS} s of stereo noise at 48 kHz in 64-frame blocks",
        if RELEASE { "release" } else { "debug" }
    );
}
