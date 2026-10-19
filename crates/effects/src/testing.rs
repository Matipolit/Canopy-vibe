//! Signals and measurements for the effects' tests.

use instruments::ParamDescriptor;

use crate::{Context, Effect};

pub(crate) const RATE: f32 = 48_000.0;

/// Reproducible white noise in `-1..1`.
pub(crate) fn noise(len: usize, seed: u32) -> Vec<f32> {
    let mut state = seed.max(1);
    (0..len)
        .map(|_| dsp::xorshift32(&mut state) as f32 / u32::MAX as f32 * 2.0 - 1.0)
        .collect()
}

pub(crate) fn sine(freq: f64, amplitude: f32, len: usize, sample_rate: f32) -> Vec<f32> {
    let step = std::f64::consts::TAU * freq / f64::from(sample_rate);
    (0..len)
        .map(|n| amplitude * (step * n as f64).sin() as f32)
        .collect()
}

/// Runs `effect` over a stereo signal in place, `block` frames at a time.
pub(crate) fn run(
    effect: &mut dyn Effect,
    left: &mut [f32],
    right: &mut [f32],
    block: usize,
    context: &Context,
) {
    for (l, r) in left.chunks_mut(block).zip(right.chunks_mut(block)) {
        effect.process(l, r, context);
    }
}

pub(crate) fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

pub(crate) fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |p, s| p.max(s.abs()))
}

pub(crate) fn db(gain: f64) -> f64 {
    20.0 * gain.max(1e-12).log10()
}

/// Sets the parameter called `id`.
pub(crate) fn set(effect: &mut dyn Effect, id: &str, value: f32) {
    let index = effect
        .params()
        .iter()
        .position(|p| p.id == id)
        .unwrap_or_else(|| panic!("no parameter {id}"));
    effect.set_param(index, value);
}

/// The values of `params`: their defaults, but for the `(id, value)` pairs
/// in `settings`.
pub(crate) fn values(params: &[ParamDescriptor], settings: &[(&str, f32)]) -> Vec<f32> {
    let mut values: Vec<f32> = params.iter().map(|p| p.default_value()).collect();
    for &(id, value) in settings {
        let index = params.iter().position(|p| p.id == id);
        values[index.unwrap_or_else(|| panic!("no parameter {id}"))] = value;
    }
    values
}
