//! Patcher: a chain of effects, patched as a graph, wrapped up as one
//! effect with a few knobs of its own, as FL Studio's Patcher does.
//!
//! What it holds lives in the project (`project::Patch`: its effects, its
//! graph and what each macro drives); the engine plays those effects in
//! its place, as if they stood in the chain where it stands. This effect
//! is only its outside: eight macro knobs, which automate, link to a
//! controller and take presets like any other, and which the project
//! turns into values of the knobs inside. So the instance itself passes
//! the sound on untouched, and is never what the engine runs.

use instruments::ParamDescriptor;

use crate::{Category, Context, Effect, EffectDescriptor, float};
use dsp::Pace;

/// How many macros a Patcher has.
pub const MACROS: usize = 8;

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.patcher",
    name: "Patcher",
    description: "Effects patched together as one, with macro knobs",
    category: Category::Routing,
    params: PARAMS,
    create: || Box::new(Patcher),
    ..EffectDescriptor::BASE
};

/// The macros, in percent of the way from each mapped knob's minimum to
/// its maximum.
pub const PARAMS: &[ParamDescriptor] = &[
    float("macro_1", "Macro 1", 0.0, 100.0, 0.0, "%", false),
    float("macro_2", "Macro 2", 0.0, 100.0, 0.0, "%", false),
    float("macro_3", "Macro 3", 0.0, 100.0, 0.0, "%", false),
    float("macro_4", "Macro 4", 0.0, 100.0, 0.0, "%", false),
    float("macro_5", "Macro 5", 0.0, 100.0, 0.0, "%", false),
    float("macro_6", "Macro 6", 0.0, 100.0, 0.0, "%", false),
    float("macro_7", "Macro 7", 0.0, 100.0, 0.0, "%", false),
    float("macro_8", "Macro 8", 0.0, 100.0, 0.0, "%", false),
];

/// The instance: the sound passes through (see the module's header).
struct Patcher;

impl Effect for Patcher {
    fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}

    fn reset(&mut self) {}

    fn process(&mut self, _left: &mut [f32], _right: &mut [f32], _context: &Context) {}

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn receive(&mut self, _index: usize, _value: f32, _pace: Pace) {}
}
