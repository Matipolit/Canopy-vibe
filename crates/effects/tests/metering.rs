//! `Context::metering` lets an effect skip the work that only feeds its
//! meters while no one reads them. It must never change the sound: every
//! built-in is played twice over the same noise, once with its meters on
//! throughout and once with them switched off and back on part way, and
//! the two must agree to the bit.

mod common;

use common::{effect, noise};
use effects::{Context, MAX_METERS, builtins};

/// Plays `input` (left) and it reversed (right) through `effect` in blocks
/// of `block`, switching the meters off from block `off` until block `on`
/// and reading them after every block, as the engine does.
fn play(
    effect: &mut dyn effects::Effect,
    input: &[f32],
    block: usize,
    (off, on): (usize, usize),
) -> (Vec<f32>, Vec<f32>) {
    let mut left = input.to_vec();
    let mut right: Vec<f32> = input.iter().rev().copied().collect();
    let mut meters = [0.0; MAX_METERS];
    for (index, start) in (0..input.len()).step_by(block).enumerate() {
        let end = (start + block).min(input.len());
        let context = Context {
            sidechain: Some((&input[start..end], &input[start..end])),
            metering: !(off..on).contains(&index),
            ..Context::DEFAULT
        };
        effect.process(&mut left[start..end], &mut right[start..end], &context);
        effect.meters(&mut meters);
    }
    (left, right)
}

#[test]
fn meters_switched_off_never_change_the_sound() {
    let input = noise(48_000, 11, 1.5);
    let mut cases: Vec<(&str, Vec<(&str, f32)>)> = builtins()
        .iter()
        .map(|descriptor| (descriptor.id, Vec::new()))
        .collect();
    // The clipper's safety and oversampling each take another path.
    for safety in [0.0, 1.0, 2.0] {
        for oversampling in [0.0, 1.0, 3.0] {
            cases.push((
                "canopy.clipper",
                vec![
                    ("input", 12.0),
                    ("safety", safety),
                    ("oversampling", oversampling),
                ],
            ));
        }
    }
    for (id, settings) in cases {
        let what = format!("{id} {settings:?}");
        let mut always = effect(id, 48_000.0, 64, &settings);
        let mut switched = effect(id, 48_000.0, 64, &settings);
        let heard = play(always.as_mut(), &input, 64, (usize::MAX, usize::MAX));
        let skipped = play(switched.as_mut(), &input, 64, (40, 500));
        assert!(heard == skipped, "{what}: the meters changed the sound");
    }
}
