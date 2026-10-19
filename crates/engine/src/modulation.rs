//! Modulation in the engine: each track's modulators tick once a block
//! (`MAX_BLOCK` frames, the control rate), and its routes move parameters
//! on top of their base values.
//!
//! A track's modulation compiles (UI thread, [`ModState::compile`]) to a
//! [`ModProgram`] in its node. Each block the render loop calls it at fixed
//! points:
//!
//! 1. Automation, while playing, and live commands (knobs, faders) offer
//!    each value to [`ModProgram::set_base`] first: a modulated parameter
//!    keeps it as its base instead of the processor getting it.
//! 2. [`ModProgram::pre`], before the track's sound renders: modulators
//!    tick, and instrument parameters move.
//! 3. The instrument or clips render.
//! 4. [`ModProgram::post`]: the instrument's exported modulators are read,
//!    followers measure the track's sound, and the volume, pan and effect
//!    parameters move (so they can follow this block's instrument LFO).
//! 5. The chain runs, then the fader.
//!
//! A moved parameter reaches its processor through `modulate` (a ramp of at
//! most one block), not `set_param` (knob smoothing). At a swap
//! ([`ModProgram::adopt`]) modulators carry over by id, so an LFO keeps its
//! phase through edits.
//!
//! For now nothing is modulated: every program is empty and every hook does
//! nothing, so every parameter takes the path it always did.

use midi::NoteEvent;
use project::{AutomationTarget, ModRouteId, ModulatorId, Track, TrackId};

use crate::arrangement::{AutomationKind, Processors};
use crate::chain::EffectSlot;
use crate::pool::Sends;

/// A parameter modulation can move, as the render loop addresses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sink {
    /// The fader's volume, in dB.
    Volume,
    /// The fader's pan, `-1..=1`.
    Pan,
    /// Instrument parameter `.0`, in its own units.
    Instrument(u16),
    /// Parameter `param` of the effect in slot `slot`, in its own units.
    Effect { slot: u16, param: u16 },
    /// Parameter `param` of the modulator in slot `slot`, in its own units.
    Modulator { slot: u16, param: u16 },
}

/// Indices past `u16` are never modulated.
fn index(i: usize) -> u16 {
    u16::try_from(i).unwrap_or(u16::MAX)
}

impl Sink {
    pub fn instrument(param: usize) -> Sink {
        Sink::Instrument(index(param))
    }

    pub fn effect(slot: usize, param: usize) -> Sink {
        Sink::Effect {
            slot: index(slot),
            param: index(param),
        }
    }

    pub fn modulator(slot: usize, param: usize) -> Sink {
        Sink::Modulator {
            slot: index(slot),
            param: index(param),
        }
    }
}

/// What one block's modulation can see.
#[allow(dead_code)] // read once modulators tick
pub(crate) struct ModContext<'a> {
    /// The block's length, at most `MAX_BLOCK`.
    pub frames: usize,
    pub bpm: f64,
    /// Where the song is at the block's start, in quarter notes.
    pub beat: f64,
    pub playing: bool,
    /// The track's note events this block (MIDI tracks; empty otherwise),
    /// for envelopes and note-triggered LFOs.
    pub events: &'a [NoteEvent],
    /// The send buses, for followers listening to another track.
    pub sends: Sends<'a>,
}

/// A track's compiled modulation. Empty for a track with none, which costs
/// nothing.
#[derive(Default)]
pub(crate) struct ModProgram {}

impl ModProgram {
    /// Before the track's sound renders: tick the modulators, move
    /// instrument parameters.
    pub fn pre(&mut self, _cx: &ModContext, _processors: &mut Processors) {}

    /// After the track's sound (`input`) rendered: read the instrument's
    /// exported modulators, let followers measure, move the volume, pan and
    /// effect parameters.
    pub fn post(
        &mut self,
        _cx: &ModContext,
        _input: (&[f32], &[f32]),
        _processors: &mut Processors,
        _effects: &mut [EffectSlot],
    ) {
    }

    /// A new base value for `sink` (from automation, a knob or a fader), in
    /// its own units. True if `sink` is modulated: the program keeps it as
    /// the base and sees that it reaches the processor; false, and the
    /// caller sets it as always.
    pub fn set_base(&mut self, _sink: Sink, _value: f32) -> bool {
        false
    }

    /// Whether this program moves `sink`, so a swap leaves it to the
    /// program rather than pushing the project's value.
    pub fn modulates(&self, _sink: Sink) -> bool {
        false
    }

    /// A live change of a route's amount ([`crate::Command::SetModAmount`]).
    pub fn set_amount(&mut self, _route: ModRouteId, _amount: f32) {}

    /// A live change of a modulator's parameter
    /// ([`crate::Command::SetModulatorParam`]). False if the modulator
    /// isn't on this track.
    pub fn set_modulator_param(
        &mut self,
        _modulator: ModulatorId,
        _index: usize,
        _value: f32,
    ) -> bool {
        false
    }

    /// When this program's snapshot arrives, on the audio thread: takes the
    /// modulators this track's previous program has (by id), so LFOs keep
    /// their phase. Pointer moves only.
    pub fn adopt(&mut self, _old: &mut ModProgram) {}

    /// On the audio thread, when the snapshot holding this goes back to be
    /// dropped: what the next program didn't adopt is done.
    pub fn retire(&mut self) {}
}

/// What compiling a track's modulation needs.
#[allow(dead_code)] // read once there's modulation to compile
pub(crate) struct ModInput<'a> {
    pub track: &'a Track,
    /// The track's instrument, if it has one this build knows: its
    /// parameters, and the modulators it exports.
    pub instrument: Option<&'static instruments::BuiltInInstrumentDescriptor>,
    pub sample_rate: u32,
}

/// What the compiler remembers about modulation between snapshots: which
/// modulators the engine already runs, and where the audio thread
/// publishes modulated values for the UI.
#[derive(Default)]
pub(crate) struct ModState {}

impl ModState {
    /// Before a snapshot's tracks are compiled.
    pub fn begin(&mut self) {}

    /// Compiles one track's modulation.
    pub fn compile(&mut self, _input: ModInput) -> ModProgram {
        ModProgram::default()
    }

    /// After a snapshot's tracks are compiled: forget what it didn't use.
    pub fn finish(&mut self) {}

    /// Forget everything: the engine's modulators are gone (a snapshot
    /// couldn't be sent).
    pub fn invalidate(&mut self) {}

    /// Where `target` on `track` is right now, modulation and all, on the
    /// `0..=1` scale; `None` if it isn't modulated.
    pub fn modulated_value(&self, _track: TrackId, _target: &AutomationTarget) -> Option<f32> {
        None
    }

    /// A modulator's latest output, and its phase (`0..1`) if it has one.
    pub fn modulator_output(&self, _id: ModulatorId) -> Option<(f32, f32)> {
        None
    }
}

/// How automation of modulator `modulator`'s parameter `param` on `track`
/// compiles ([`AutomationKind::Modulator`]): `None` if there's no such
/// parameter.
pub(crate) fn automation_kind(
    _track: &Track,
    _modulator: ModulatorId,
    _param: &str,
) -> Option<AutomationKind> {
    None
}
