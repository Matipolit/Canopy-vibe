//! Aborts the test binary if a built-in effect allocates or frees memory on
//! the audio thread. Allocation there can block on a lock inside the
//! allocator, which shows up as random dropouts that are hard to trace.
//!
//! Every effect goes through `process`, `set_param`, `modulate`, `reset`,
//! `meter` and `retire`; then each group through the paths its own effects
//! have: the creative effects with a sidechain in every block, the
//! transport starting and stopping and every mode switching mid-stream;
//! the dynamics with every band dynamic and character on and sidechains of
//! odd lengths; the clipper with its oversampling, safety and Delta
//! switching, and garbage input.
//!
//! Only meaningful in debug builds: assert_no_alloc has no allocator to
//! install under --release, so there this file is empty.

#![cfg(debug_assertions)]

mod common;

use common::{CREATIVE, DYNAMICS, Random, noise, range};
use effects::{Context, Effect, MAX_METERS, builtins};
use instruments::ParameterKind;

#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const BLOCK: usize = 256;

fn create(id: &str, rate: f32) -> Box<dyn Effect> {
    common::effect(id, rate, BLOCK, &[])
}

#[test]
fn builtins_never_allocate_on_the_audio_thread() {
    let noise = noise(BLOCK, 0x9e37_79b9, 1.0);
    let sidechain = Context {
        bpm: 97.0,
        sidechain: Some((&noise, &noise)),
        ..Context::DEFAULT
    };
    for descriptor in builtins() {
        let mut effect = create(descriptor.id, 48_000.0);
        let (mut left, mut right) = (noise.clone(), noise.clone());
        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..20 {
                effect.process(&mut left, &mut right, &Context::DEFAULT);
                left.copy_from_slice(&noise);
                right.copy_from_slice(&noise);
            }
            for (index, param) in effect.params().iter().enumerate() {
                let (min, max) = range(param);
                for value in [min, max, (min + max) / 2.0, f32::NAN, param.default_value()] {
                    effect.set_param(index, value);
                    effect.process(&mut left, &mut right, &Context::DEFAULT);
                    std::hint::black_box(effect.meter());
                    effect.modulate(index, value);
                    effect.process(&mut left, &mut right, &Context::DEFAULT);
                }
            }
            effect.set_param(usize::MAX, 0.0);
            effect.process(&mut left, &mut right, &sidechain);
            effect.process(&mut left[..17], &mut right[..17], &Context::DEFAULT);
            effect.process(&mut [], &mut [], &Context::DEFAULT);
            effect.reset();
            effect.process(&mut left, &mut right, &Context::DEFAULT);
            std::hint::black_box(effect.meter());
            effect.reset();
            effect.retire();
        });
    }
}

#[test]
fn creative_effects_never_allocate_on_the_audio_thread() {
    let input = noise(BLOCK, 0x1357_9bdf, 1.0);
    let side = noise(BLOCK, 0x2468_ace0, 1.0);
    for id in CREATIVE {
        let mut effect = create(id, 48_000.0);
        let (mut left, mut right) = (input.clone(), input.clone());
        let mut meters = [0.0; MAX_METERS];
        assert_no_alloc::assert_no_alloc(|| {
            for round in 0..120 {
                // Every parameter somewhere new each block: shapes, curves,
                // oversampling and band counts switch while sound plays.
                for (index, param) in effect.params().iter().enumerate() {
                    let (min, max) = range(param);
                    let share = ((round * 7 + index * 3) % 11) as f32 / 10.0;
                    effect.set_param(index, min + (max - min) * share);
                }
                let context = Context {
                    bpm: 90.0 + round as f64,
                    beat: round as f64 * 0.37,
                    playing: round % 40 >= 10,
                    sidechain: (round % 30 < 25).then_some((&side[..], &side[..])),
                };
                left.copy_from_slice(&input);
                right.copy_from_slice(&input);
                effect.process(&mut left, &mut right, &context);
                effect.meters(&mut meters);
                std::hint::black_box(&meters);
            }
            effect.reset();
            effect.process(&mut left, &mut right, &Context::DEFAULT);
            effect.meters(&mut meters);
        });
    }
}

#[test]
fn dynamics_never_allocate_on_the_audio_thread() {
    let mut random = Random::new(0x2f6b_1a3d);
    let noise = noise(BLOCK, 0x3c6e_f372, 1.0);
    let side: Vec<f32> = (0..BLOCK).map(|n| (n as f32 * 0.13).sin()).collect();
    for id in DYNAMICS {
        let mut effect = create(id, 44_100.0);
        let params = effect.params();
        let (mut left, mut right) = (noise.clone(), noise.clone());
        let mut meters = [0.0; MAX_METERS];
        assert_no_alloc::assert_no_alloc(|| {
            // Every band dynamic, every character: everything switched on.
            for (index, param) in params.iter().enumerate() {
                if let ParameterKind::Enum { labels, .. } = param.kind {
                    effect.set_param(index, (labels.len() - 1) as f32);
                }
            }
            for round in 0..40 {
                let sidechain = match round % 3 {
                    0 => Some((&side[..], &side[..])),
                    1 => Some((&side[..100], &side[..100])),
                    _ => None,
                };
                let context = Context {
                    sidechain,
                    ..Context::DEFAULT
                };
                let index = random.index(params.len());
                effect.set_param(index, random.value(&params[index]));
                let len = [BLOCK, 1, 17, 0, 64][round % 5];
                effect.process(&mut left[..len], &mut right[..len], &context);
                effect.meters(&mut meters);
                left.copy_from_slice(&noise);
                right.copy_from_slice(&noise);
            }
            effect.reset();
            effect.process(&mut left, &mut right, &Context::DEFAULT);
            effect.meters(&mut meters);
            std::hint::black_box(&meters);
        });
    }
}

#[test]
fn the_clipper_never_allocates_on_the_audio_thread() {
    let mut random = Random::new(0x7a5c_31e9);
    let noise = noise(BLOCK, 0x6a09_e667, 3.0);
    let garbage = [f32::NAN, f32::INFINITY, -1e30, 0.5, f32::NEG_INFINITY];
    for rate in [44_100.0, 96_000.0] {
        let mut effect = create("canopy.clipper", rate);
        let params = effect.params();
        let (mut left, mut right) = (noise.clone(), noise.clone());
        let mut meters = [0.0; MAX_METERS];
        assert_no_alloc::assert_no_alloc(|| {
            for round in 0..160 {
                // A few parameters somewhere new each block: oversampling,
                // safety, shape and Delta switch while sound plays.
                for _ in 0..3 {
                    let index = random.index(params.len());
                    effect.set_param(index, random.value(&params[index]));
                }
                let len = [BLOCK, 1, 17, 0, 64, 255][round % 6];
                effect.process(&mut left[..len], &mut right[..len], &Context::DEFAULT);
                effect.meters(&mut meters);
                std::hint::black_box(&meters);
                left.copy_from_slice(&noise);
                right.copy_from_slice(&noise);
                if round % 40 == 39 {
                    left[..garbage.len()].copy_from_slice(&garbage);
                }
            }
            effect.reset();
            effect.process(&mut left, &mut right, &Context::DEFAULT);
            effect.meters(&mut meters);
            std::hint::black_box(&meters);
        });
    }
}
