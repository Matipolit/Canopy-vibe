//! Effects that delay their sound say by how much
//! ([`effects::EffectDescriptor::latency`]), and say it exactly: the engine
//! delays every other track by that many samples to keep them in time.

mod common;

use common::{Random, declared_latency, effect};
use effects::Context;

const RATE: f32 = 48_000.0;

/// Runs `input` through the built-in `id`, set as `settings` say, at
/// `rate`; returns the left channel and the latency the effect declares.
fn run(id: &str, settings: &[(&str, f32)], rate: f32, input: &[f32]) -> (Vec<f32>, usize) {
    let mut effect = effect(id, rate, 64, settings);
    let mut left = input.to_vec();
    let mut right = input.to_vec();
    for (l, r) in left.chunks_mut(64).zip(right.chunks_mut(64)) {
        effect.process(l, r, &Context::DEFAULT);
    }
    (left, declared_latency(id, settings, rate))
}

/// How late an impulse comes out of the built-in `id` set as `settings`
/// say, and how late it says it plays.
fn impulse_delay(id: &str, settings: &[(&str, f32)], rate: f32) -> (usize, usize) {
    let mut input = vec![0.0f32; 8192];
    input[1000] = 0.25;
    let (out, declared) = run(id, settings, rate, &input);
    let peak = out
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap()
        .0;
    (peak - 1000, declared)
}

#[test]
fn the_limiter_says_how_late_it_is() {
    let (measured, declared) = impulse_delay("canopy.limiter", &[], RATE);
    assert!(declared > 0);
    assert_eq!(measured, declared);
}

#[test]
fn the_ducker_is_as_late_as_its_lookahead() {
    for ms in [0.0, 2.5, 10.0] {
        let (measured, declared) = impulse_delay("canopy.ducker", &[("lookahead", ms)], RATE);
        assert_eq!(declared, (ms / 1000.0 * RATE) as usize);
        assert_eq!(measured, declared, "{ms} ms");
    }
}

#[test]
fn most_effects_add_no_latency() {
    // (effect, settings): the filter half dry, so its own group delay,
    // which isn't latency, doesn't move the peak.
    for (id, settings) in [
        ("canopy.eq", &[][..]),
        ("canopy.compressor", &[]),
        ("canopy.reverb", &[]),
        ("canopy.delay", &[]),
        ("canopy.analyzer", &[]),
        ("canopy.utility", &[]),
        ("canopy.transient", &[]),
        ("canopy.filter", &[("mix", 50.0)]),
        ("canopy.distortion", &[("oversampling", 0.0), ("mix", 50.0)]),
        ("canopy.hyper", &[]),
        ("canopy.frequency_shifter", &[("mix", 0.0)]),
        ("canopy.convolve", &[("mix", 50.0)]),
    ] {
        assert_eq!(impulse_delay(id, settings, RATE), (0, 0), "{id}");
    }
}

/// Every one of the clipper's oversampling choices, with each safety.
fn clipper_settings() -> Vec<[(&'static str, f32); 2]> {
    (0..=4)
        .flat_map(|oversampling| {
            (0..=2).map(move |safety| {
                [
                    ("oversampling", oversampling as f32),
                    ("safety", safety as f32),
                ]
            })
        })
        .collect()
}

#[test]
fn the_clipper_says_how_late_it_is_at_every_setting_and_rate() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        for settings in clipper_settings() {
            let (measured, declared) = impulse_delay("canopy.clipper", &settings, rate);
            assert_eq!(measured, declared, "{settings:?} at {rate} Hz");
        }
    }
}

/// Noise low-passed at 0.4 of the sample rate by a long Kaiser-windowed
/// sinc, at -12 dBFS: under the clipper's ceiling, and within the band its
/// oversampling keeps.
fn quiet_noise(len: usize) -> Vec<f32> {
    const HALF: isize = 128;
    let cutoff = 0.4;
    let i0 = |x: f64| {
        let (mut sum, mut term) = (1.0, 1.0);
        for k in 1..60 {
            term *= x / 2.0 / k as f64;
            sum += term * term;
        }
        sum
    };
    let kernel: Vec<f64> = (-HALF..=HALF)
        .map(|k| {
            let t = k as f64;
            let sinc = if k == 0 {
                2.0 * cutoff
            } else {
                (std::f64::consts::TAU * cutoff * t).sin() / (std::f64::consts::PI * t)
            };
            let edge = 1.0 - (t / (HALF + 1) as f64).powi(2);
            sinc * i0(10.0 * edge.sqrt()) / i0(10.0)
        })
        .collect();
    let mut random = Random::new(0x1234_5678);
    let white: Vec<f64> = (0..len + 2 * HALF as usize)
        .map(|_| f64::from(random.bipolar()))
        .collect();
    let filtered: Vec<f64> = (0..len)
        .map(|n| {
            kernel
                .iter()
                .enumerate()
                .map(|(k, w)| white[n + k] * w)
                .sum()
        })
        .collect();
    let loudest = filtered.iter().fold(0.0f64, |m, s| m.max(s.abs()));
    filtered
        .iter()
        .map(|s| (0.25 * s / loudest) as f32)
        .collect()
}

/// The largest difference between `out` and `input` moved `shift` later,
/// in dB relative to full scale.
fn residual_db(input: &[f32], out: &[f32], shift: usize) -> f32 {
    let worst = (2000..input.len())
        .map(|n| (out[n] - input[n - shift]).abs())
        .fold(0.0f32, f32::max);
    20.0 * worst.max(1e-12).log10()
}

/// What the clipper doesn't clip cancels against what went in at exactly
/// the offset it declares, and only there.
#[test]
fn what_the_clipper_doesnt_clip_cancels_at_exactly_the_declared_offset() {
    let input = quiet_noise(12_000);
    for settings in clipper_settings() {
        let (out, declared) = run("canopy.clipper", &settings, RATE, &input);
        let exact = residual_db(&input, &out, declared);
        let late = residual_db(&input, &out, declared + 1);
        let early = declared
            .checked_sub(1)
            .map(|s| residual_db(&input, &out, s));
        println!(
            "{settings:?}: {declared} samples; residual {exact:.1} dB there, {late:.1} dB a sample later"
        );
        assert!(exact < -95.0, "{settings:?}: {exact}");
        assert!(late > -30.0, "{settings:?}");
        assert!(early.is_none_or(|early| early > -30.0), "{settings:?}");
    }
}
