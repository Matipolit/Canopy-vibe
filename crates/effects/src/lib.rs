//! The [`Effect`] trait and the built-in effects. An effect changes a stereo
//! signal in place on the audio thread: a track's, after its instrument or
//! clips and before its fader, or the master's.
//!
//! Parameters are described with [`instruments::ParamDescriptor`], so the UI,
//! automation and MIDI controller links treat them like instrument ones.
//!
//! # Modulation
//!
//! Any parameter may be moved continuously, not just by a hand on a knob,
//! and there are two ways in:
//!
//! - [`Effect::set_param`] is the knob's way, and automation's and a MIDI
//!   controller's: the value glides there over about 20 ms (each effect's
//!   own smoothing), which hides the steps of a hand or a coarse lane.
//! - [`Effect::modulate`] is a modulator's (a synth's LFO or envelope, a
//!   macro): called at most once per parameter per engine block of 64
//!   frames (`dsp::MAX_BLOCK`), it must reach the value within that block,
//!   so a fast LFO comes through with its speed and depth intact. Ramping
//!   linearly from the last value to the new one across the block joins
//!   the modulator's steps into straight lines: no stairs, no clicks.
//!
//! Every effect keeps to these rules, which `tests/modulation.rs` checks
//! for every built-in by driving each float parameter through a slow and a
//! fast LFO, through both ways in:
//!
//! - **Both are cheap and constant-time.** They store the value and set a
//!   target. Anything heavier (filter coefficients, band layouts, tables)
//!   is worked out in `process`, at most once per sample or per short chunk
//!   while a value moves, and only for what moved; the vocoder's attack,
//!   say, retunes its envelopes, never its filters.
//! - **Continuous values glide** (`dsp::Smoothed`), on the scale
//!   they're heard on: gains as gains or dB, frequencies and times as log2.
//!   `set_target` takes the knob's time, `modulate` one block at most.
//! - **Time constants glide too.** An envelope lagging behind its target
//!   would otherwise catch up all at once when an attack or release is
//!   turned much shorter, which reads as a click.
//! - **Filters stay stable while their coefficients move every sample.**
//!   Use the trapezoidal state-variable and one-pole forms of `dsp::svf`
//!   and `dsp::one_pole`
//!   (or their nonlinear relatives), which tolerate any speed of change,
//!   never direct-form biquads, and interpolate their tuning rather than
//!   jumping it.
//! - **Delay taps never jump.** A knob crossfades them to a new time
//!   (`line.rs`'s `Tap`); a modulator glides them, reading between samples,
//!   so the pitch bends as a tape's would, with the slope capped so it
//!   never runs backwards.
//! - **Switches crossfade.** Enum parameters (a mode, a shape, a band's
//!   type) switch the same way through either door, fading from the old
//!   sound to the new over a few milliseconds; a switch that arrives while
//!   one is fading waits for it to end.
//! - **Feedback stays bounded.** Loops whose gain a modulator can push
//!   about are bent down smoothly when they get far louder than full
//!   scale, and lose DC, so no combination of settings at any speed runs
//!   away.

pub mod amp;
pub mod analyzer;
pub mod chorus;
pub mod clipper;
pub mod compressor;
pub mod convolve;
pub mod delay;
pub mod distortion;
pub mod ducker;
mod dynamics;
pub mod eq;
pub mod freqshift;
mod halfband;
pub mod hyper;
pub mod limiter;
mod line;
mod lookahead;
mod meter;
pub mod multiband;
pub mod multifilter;
pub mod reverb;
pub mod shaper;
mod shaping;
#[cfg(test)]
mod testing;
pub mod transient;
mod truepeak;
pub mod utility;
pub mod vocoder;
pub mod waveshaper;

pub use dsp::Pace;
use instruments::ParamDescriptor;
pub use instruments::{Format, MAX_METERS};
pub(crate) use instruments::{OFF_ON, choice, float, param_value};
use serde::{Deserialize, Serialize};

/// What an effect may need besides its own input, for one block.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The song's tempo, for tempo-synced times.
    pub bpm: f64,
    /// Where the song is, in quarter notes from its start, at the block's
    /// first sample, for effects that lock to the beat (an LFO shaper).
    pub beat: f64,
    /// Whether the transport is rolling.
    pub playing: bool,
    /// Another track's sound for the effect to listen to (a compressor's or
    /// vocoder's sidechain), when one is routed to it; same length as the
    /// block.
    pub sidechain: Option<(&'a [f32], &'a [f32])>,
}

impl Context<'_> {
    pub const DEFAULT: Context<'static> = Context {
        bpm: 120.0,
        beat: 0.0,
        playing: false,
        sidechain: None,
    };
}

/// Where the effect picker lists an effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Eq,
    Dynamics,
    Distortion,
    Modulation,
    Space,
    /// Meters and scopes: they show the sound and leave it alone.
    Analysis,
}

impl Category {
    pub const ALL: [Category; 6] = [
        Category::Eq,
        Category::Dynamics,
        Category::Distortion,
        Category::Modulation,
        Category::Space,
        Category::Analysis,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Category::Eq => "EQ & Filters",
            Category::Dynamics => "Dynamics",
            Category::Distortion => "Distortion",
            Category::Modulation => "Modulation",
            Category::Space => "Space",
            Category::Analysis => "Analyzers",
        }
    }
}

/// An audio effect.
///
/// Built and prepared on the UI thread, then moved to the audio thread, where
/// only `process`, `set_param`, `reset` and `meters` are called. Those must be
/// real-time safe: no allocation, locks or I/O.
pub trait Effect: Send {
    /// Called before the effect goes to the audio thread, and again if the
    /// sample rate changes. Allocate delay lines and buffers here, for the
    /// longest settings the parameters allow. `process` is never called
    /// with blocks longer than `max_block`.
    fn prepare(&mut self, sample_rate: f32, max_block: usize);

    /// Forget everything ringing: delay lines, filter state, envelopes.
    fn reset(&mut self);

    /// Process one block in place (`left` and `right` are the same length).
    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: &Context);

    fn params(&self) -> &'static [ParamDescriptor];

    /// Set parameter `index` (its position in [`Self::params`]) to `value`,
    /// in its own units; enum parameters take the label index. Out-of-range
    /// values are clamped, and NaN and unknown indices ignored. Cheap enough
    /// to call every block for every parameter. `pace` is how the change
    /// arrives (see "Modulation" above):
    ///
    /// - [`Pace::Knob`], for a knob, a controller or automation: it glides
    ///   (about 20 ms), so that moving a knob doesn't click.
    /// - [`Pace::Modulation`], for a value a modulator moves: called at
    ///   most once per parameter per engine block (`MAX_BLOCK` frames),
    ///   before `process`. Reach the value within that block (a ramp of at
    ///   most a block, not the knob's smoothing) and stay stable however
    ///   fast it moves. Enum parameters just switch.
    fn receive(&mut self, index: usize, value: f32, pace: Pace);

    /// [`Self::receive`] at a knob's pace.
    fn set_param(&mut self, index: usize, value: f32) {
        self.receive(index, value, Pace::Knob)
    }

    /// [`Self::receive`] at a modulator's pace.
    fn modulate(&mut self, index: usize, value: f32) {
        self.receive(index, value, Pace::Modulation)
    }

    /// Audio thread, when the engine stops running this instance (removed, or
    /// replaced) — before it goes back to the UI thread to be dropped.
    /// Plugins call stop_processing here.
    fn retire(&mut self) {}

    /// Values for the UI to show, like a compressor's input and output levels
    /// and gain reduction: what each slot means is listed in the effect's
    /// [`EffectDescriptor::meters`]. Called after `process`; real-time safe.
    fn meters(&self, _out: &mut [f32; MAX_METERS]) {}

    /// The first meter alone (a compressor's gain reduction).
    fn meter(&self) -> f32 {
        let mut out = [0.0; MAX_METERS];
        self.meters(&mut out);
        out[0]
    }
}

pub struct EffectDescriptor {
    /// Stable name used in project files. Never rename one that has shipped.
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub version: &'static str,
    pub category: Category,
    /// The same list [`Effect::params`] returns, available without an instance.
    pub params: &'static [ParamDescriptor],
    /// Settings that only change how the UI shows things (an analyzer's
    /// range, colors, what it draws). Stored with the effect's parameters
    /// but never sent to the audio thread or offered for automation.
    pub settings: &'static [ParamDescriptor],
    /// What each [`Effect::meters`] slot means, in order, for the UI.
    pub meters: &'static [&'static str],
    /// The UI wants the effect's output as audio, for scopes and spectra:
    /// the engine copies it to the UI after each block.
    pub tap: bool,
    /// The effect listens to a sidechain ([`Context::sidechain`]), so the UI
    /// offers to route another track to it.
    pub sidechain: bool,
    /// How many samples late its output is at these parameter values (in
    /// [`Self::params`] order) and sample rate: a limiter's lookahead. The
    /// engine delays other tracks by as much, so everything stays in time.
    pub latency: fn(values: &[f32], sample_rate: f32) -> usize,
    pub create: fn() -> Box<dyn Effect>,
}

/// [`EffectDescriptor::latency`] for effects that add none.
pub fn no_latency(_values: &[f32], _sample_rate: f32) -> usize {
    0
}

impl EffectDescriptor {
    /// What a descriptor leaves out takes from here, through
    /// `..EffectDescriptor::BASE`: version 1.0, no UI settings, no tap, no
    /// sidechain, no latency. The identity, the parameters, the meters and
    /// `create` every descriptor gives itself.
    pub const BASE: EffectDescriptor = EffectDescriptor {
        id: "",
        name: "",
        description: "",
        version: "1.0",
        category: Category::Analysis,
        params: &[],
        settings: &[],
        meters: &[],
        tap: false,
        sidechain: false,
        latency: no_latency,
        create: || unreachable!("every descriptor has its own `create`"),
    };
}

/// Which effect a device is, as stored in a project file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffectRef {
    /// Built-in or a plugin; only plugins write it.
    #[serde(default, skip_serializing_if = "Format::is_builtin")]
    pub format: Format,
    /// A built-in's id; a CLAP plugin's id; a VST3 class UID as 32 hex
    /// digits; a VST2 unique id as 8.
    pub id: String,
    pub version: String,
}

impl EffectRef {
    /// The built-in effect `id`, at its current version.
    pub fn builtin(id: &str) -> EffectRef {
        EffectRef {
            format: Format::BuiltIn,
            id: id.to_owned(),
            version: find_builtin(id).map_or("", |d| d.version).to_owned(),
        }
    }

    /// The description of this effect, if it's one we have: a built-in, or
    /// a plugin whose description was registered with [`register_external`].
    pub fn descriptor(&self) -> Option<&'static EffectDescriptor> {
        match self.format {
            Format::BuiltIn => find_builtin(&self.id),
            format => find_external(format, &self.id),
        }
    }

    /// A fresh instance, or `None` if the effect isn't available. Call
    /// [`Effect::prepare`] before handing it to the audio thread.
    pub fn create(&self) -> Option<Box<dyn Effect>> {
        self.descriptor().map(|d| (d.create)())
    }
}

static BUILTINS: &[EffectDescriptor] = &[
    eq::DESCRIPTOR,
    multifilter::DESCRIPTOR,
    compressor::DESCRIPTOR,
    limiter::DESCRIPTOR,
    clipper::DESCRIPTOR,
    multiband::DESCRIPTOR,
    ducker::DESCRIPTOR,
    transient::DESCRIPTOR,
    waveshaper::DESCRIPTOR,
    distortion::DESCRIPTOR,
    amp::DESCRIPTOR,
    shaper::DESCRIPTOR,
    chorus::DESCRIPTOR,
    hyper::DESCRIPTOR,
    freqshift::DESCRIPTOR,
    vocoder::DESCRIPTOR,
    reverb::DESCRIPTOR,
    convolve::DESCRIPTOR,
    delay::DESCRIPTOR,
    utility::DESCRIPTOR,
    analyzer::DESCRIPTOR,
];

/// Every built-in effect, in the order the effect picker lists them.
pub fn builtins() -> &'static [EffectDescriptor] {
    BUILTINS
}

pub fn find_builtin(id: &str) -> Option<&'static EffectDescriptor> {
    BUILTINS.iter().find(|d| d.id == id)
}

/// Plugin effects' descriptions, registered by the plugin host at run time
/// (see `instruments::Registry`).
static EXTERNAL: instruments::Registry<EffectDescriptor> = instruments::Registry::new();

/// Registers (or replaces) the description of plugin effect `id`, and
/// returns it, leaked: descriptors are `&'static` everywhere. A replaced
/// one stays valid for whoever still holds it. Never on the audio thread.
pub fn register_external(
    format: Format,
    id: &str,
    descriptor: EffectDescriptor,
) -> &'static EffectDescriptor {
    EXTERNAL.register(format, id, descriptor)
}

/// The registered description of plugin effect `id`, if any.
pub fn find_external(format: Format, id: &str) -> Option<&'static EffectDescriptor> {
    EXTERNAL.find(format, id)
}

/// A parameter's current value, by index, for effects that keep them all
/// in an array.
pub(crate) fn defaults<const N: usize>(params: &[ParamDescriptor]) -> [f32; N] {
    let mut values = [0.0; N];
    for (value, param) in values.iter_mut().zip(params) {
        *value = param.default_value();
    }
    values
}

/// An effect's parameter values, in the order of its parameters, as last
/// set (clamped); and whether the effect is fresh: nothing processed since
/// it was made or reset. A fresh effect takes values at once rather than
/// gliding to them, so one set up from a project starts as saved.
#[derive(Debug, Clone)]
pub(crate) struct Values<const N: usize> {
    values: [f32; N],
    params: &'static [ParamDescriptor],
    fresh: bool,
}

impl<const N: usize> Values<N> {
    /// Every parameter of `params` at its default; fresh.
    pub fn new(params: &'static [ParamDescriptor]) -> Self {
        Values {
            values: defaults(params),
            params,
            fresh: true,
        }
    }

    /// Stores `value` for parameter `index`, clamped to its range. False,
    /// storing nothing, for NaN or an index past the end.
    #[inline]
    pub fn set(&mut self, index: usize, value: f32) -> bool {
        if value.is_nan() {
            return false;
        }
        match (self.values.get_mut(index), self.params.get(index)) {
            (Some(slot), Some(param)) => {
                *slot = param.clamp(value);
                true
            }
            _ => false,
        }
    }

    pub fn is_fresh(&self) -> bool {
        self.fresh
    }

    /// A block was processed: values glide from now on.
    #[inline]
    pub fn played(&mut self) {
        self.fresh = false;
    }

    /// The effect was reset: values are taken at once again.
    pub fn refresh(&mut self) {
        self.fresh = true;
    }
}

impl<const N: usize> std::ops::Deref for Values<N> {
    type Target = [f32];

    fn deref(&self) -> &[f32] {
        &self.values
    }
}

impl<const N: usize> std::ops::DerefMut for Values<N> {
    fn deref_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_is_consistent_and_starts_clean() {
        let mut ids = std::collections::HashSet::new();
        for descriptor in builtins() {
            assert!(ids.insert(descriptor.id), "duplicate id {}", descriptor.id);
            let named = [descriptor.id, descriptor.name, descriptor.description];
            assert!(!named.contains(&""), "{named:?} left blank");
            let mut effect = (descriptor.create)();
            assert_eq!(effect.params(), descriptor.params, "{}", descriptor.id);
            let mut param_ids = std::collections::HashSet::new();
            for param in descriptor.params {
                assert!(
                    param_ids.insert(param.id),
                    "{}: duplicate {}",
                    descriptor.id,
                    param.id
                );
            }
            effect.prepare(48_000.0, 64);
            let (mut left, mut right) = ([0.0f32; 64], [0.0f32; 64]);
            effect.process(&mut left, &mut right, &Context::DEFAULT);
            assert!(
                left.iter().chain(&right).all(|s| s.abs() < 1e-6),
                "{} makes sound from silence",
                descriptor.id
            );
            let reference = EffectRef::builtin(descriptor.id);
            assert_eq!(reference.version, descriptor.version);
            assert!(reference.create().is_some());
        }
    }
}
