//! Parameters set after `prepare` or `reset` and before the next block take
//! effect at once: an effect built from a project starts the way it was
//! saved, instead of gliding there from the defaults, which the engine's
//! "prepare, then set every parameter" would otherwise make audible.

mod common;

use effects::{Context, Effect, builtins};
use instruments::ParameterKind;

/// A value two thirds of the way through each parameter's range.
fn set_all(effect: &mut dyn Effect) {
    for (index, param) in effect.params().iter().enumerate() {
        let value = match param.kind {
            ParameterKind::Float { min, max, .. } => min + (max - min) * 2.0 / 3.0,
            ParameterKind::Enum { labels, .. } => ((labels.len() - 1) * 2 / 3) as f32,
        };
        effect.set_param(index, value);
    }
}

fn output(effect: &mut dyn Effect, input: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let (mut left, mut right) = (
        input.to_vec(),
        input.iter().map(|s| -0.5 * s).collect::<Vec<_>>(),
    );
    for (l, r) in left.chunks_mut(64).zip(right.chunks_mut(64)) {
        effect.process(l, r, &Context::DEFAULT);
    }
    (left, right)
}

#[test]
fn parameters_set_before_the_first_block_apply_at_once() {
    let input = common::noise(9600, 0x51ed_270b, 0.5);
    for descriptor in builtins() {
        // What the engine does: prepare, then set the saved values.
        let mut built = (descriptor.create)();
        built.prepare(48_000.0, 64);
        set_all(built.as_mut());
        // The reference: the same values, settled by a reset.
        let mut settled = (descriptor.create)();
        settled.prepare(48_000.0, 64);
        set_all(settled.as_mut());
        settled.reset();
        assert_eq!(
            output(built.as_mut(), &input),
            output(settled.as_mut(), &input),
            "{}",
            descriptor.id
        );

        // Once running, after a reset (say, coming back from bypass).
        let mut running = (descriptor.create)();
        running.prepare(48_000.0, 64);
        output(running.as_mut(), &input);
        running.reset();
        set_all(running.as_mut());
        let mut fresh = (descriptor.create)();
        fresh.prepare(48_000.0, 64);
        set_all(fresh.as_mut());
        assert_eq!(
            output(running.as_mut(), &input),
            output(fresh.as_mut(), &input),
            "{} after reset",
            descriptor.id
        );
    }
}
