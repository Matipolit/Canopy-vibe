//! Wavetable synth in the spirit of Serum 2 and Vital: three unison
//! wavetable oscillators (A, B and C) with two warp slots each, spectral
//! morphs, unison stacking and detune modes, a sub and a noise oscillator,
//! two multi-mode filters in serial or parallel, six envelopes, eight LFOs
//! (stock shapes, drawn steps, drawn points with curves, chaos), eight
//! macros and a 64-slot mod matrix with a curve, polarity and aux source
//! per slot. Nearly every knob is a destination: besides the oscillators,
//! filters and amp, the envelopes' times and sustain, the LFOs' rate,
//! phase, smoothing, rise and delay, the unison spreads, key tracking and
//! the volume (see [`param_dest`]).
//!
//! How it is put together:
//! - `tables` generates the built-in tables once per process, each frame
//!   band-limited per octave so high notes don't alias, and keeps every
//!   frame's harmonics for the spectral morphs (`morph`).
//! - Each oscillator can also play its own table, read from a WAV file
//!   (`user`): the table choice [`USER_TABLE`] plays it. The host loads the
//!   files on a worker thread ([`load_user_table`]) and hands them over as
//!   one [`UserTables`] with [`Instrument::set_data`] ([`DataKind::Wavetables`]).
//! - Each voice (`voice::Voice`) owns its oscillators (`osc`), filters
//!   (`filter`), envelopes (`env`) and LFOs (`lfo`). Every 32 samples (a
//!   control tick) the synth gathers the smoothed parameters into
//!   `voice::Controls`, runs what is instrument-wide (the mono LFOs and
//!   envelopes, the macros as the matrix moves them), and each voice runs
//!   its mod matrix and sets ramp targets for the next interval; between
//!   ticks everything that moves (pitches, gains, table position, warps,
//!   filter coefficients) ramps per sample, so modulation never zippers.
//! - Parameters sit in contiguous sections (see [`SECTIONS`]) so the UI can
//!   lay them out as panels; [`tables`](fn@tables), [`frame`],
//!   [`frame_warped`], [`TableSource`], [`lfo_shape_at`], [`filter_curve_db`],
//!   [`matrix_curve`] and [`live_modulation`] let it draw waveforms, LFOs,
//!   filter curves, slot curves and where modulation has everything now,
//!   and [`Instrument::meters`] (slots at `METER_*`) says what to animate.
//! - What is instrument-wide (the mono LFOs and envelopes, the macros, the
//!   wheels) is one value per block, which [`Wavetable::global_source`]
//!   reads and [`Instrument::mod_outputs`] exports ([`MOD_OUTPUTS`]) for
//!   modulating the track's effects; the per-voice modulators export the
//!   newest voice's value, as the meters show them.

mod env;
mod filter;
mod halfband;
mod lfo;
mod morph;
mod osc;
mod params;
mod tables;
mod user;
mod voice;

use std::sync::Arc;

use dsp::{AdsrDesc, Gain, Processor, Smoothed};
use midi::{MOD_WHEEL, NoteEvent, NoteEventKind, Pitch};

use crate::{
    BuiltInInstrumentDescriptor, DataKind, Instrument, InstrumentData, MAX_METERS, ModOutput,
    ParamDescriptor, ParameterKind,
};

pub use env::Stage as EnvStage;
pub use filter::{
    ACID, BP12, BP24, COMB_NEG, COMB_POS, DIRTY, FILTER_TYPE_COUNT, FLANGER, FORMANT, HIGH_SHELF,
    HP12, HP24, LADDER, LOW_SHELF, LP12, LP24, MORPH, NOTCH, PEAK, PHASER,
};
pub use lfo::{
    MODE_ENV, MODE_FREE, MODE_LOOP, MODE_RETRIG, MODE_SUSTAIN, Nodes, SHAPE_CUSTOM, SHAPE_LORENZ,
    SHAPE_NODES, SHAPE_ROSSLER, SHAPE_SAMPLE_HOLD, SHAPE_SAW_DOWN, SHAPE_SAW_UP, SHAPE_SINE,
    SHAPE_SMOOTH_RANDOM, SHAPE_SQUARE, SHAPE_TRIANGLE, chaos_trace, custom_shape, is_chaos,
    lfo_value,
};
pub use morph::{
    MORPH_COUNT, MORPH_FORMANT_SCALE, MORPH_HARMONIC_STRETCH, MORPH_HIGH_PASS,
    MORPH_INHARMONIC_STRETCH, MORPH_LOW_PASS, MORPH_OFF, MORPH_PHASE_DISPERSE,
    MORPH_RANDOM_AMPLITUDES, MORPH_SHEPARD, MORPH_SKEW, MORPH_SMEAR, MORPH_VOCODE,
};
pub use osc::{
    MAX_UNISON, Remap, STRAIGHT_REMAP, WARP_AM, WARP_ASYM_BI, WARP_ASYM_NEG, WARP_ASYM_POS,
    WARP_BEND_BI, WARP_BEND_NEG, WARP_BEND_POS, WARP_COUNT, WARP_DRIVE, WARP_FLIP, WARP_FM,
    WARP_FOLD, WARP_MIRROR, WARP_OFF, WARP_PWM, WARP_QUANTIZE, WARP_REMAP, WARP_REMAP_MIRROR,
    WARP_REMAP_X4, WARP_RM, WARP_SQUEEZE, WARP_SYNC, WARP_SYNC_WIN, is_cross_mod, is_remap,
};
pub use params::*;
pub use tables::{FRAME_COUNT, MAX_HARMONICS, MAX_USER_FRAMES, TABLE_NAMES};
pub use user::{
    DEFAULT_FRAME_SIZE, UserTable, UserTables, clm_frame_size, load_user_table, wav_frame_size,
};
pub use voice::{DC_BLOCK_HZ, ModSlot, detune_place, run_matrix, stack_interval};

use env::Envelope;
use lfo::{Lfo, LfoMods, LfoSettings};
use osc::{BLOCK, Unison};
use tables::{Bank, Table};
use voice::{
    BUS_DIRECT, BUS_FILTER1, Controls, FilterControls, NoteStart, OscControls, SCRATCH_COUNT,
    Voice, Waves, samples,
};

pub const DESCRIPTOR: BuiltInInstrumentDescriptor = BuiltInInstrumentDescriptor {
    id: "canopy.wavetable",
    name: "Wavetable",
    description: "Wavetable synth with unison, spectral morphs, filters, envelopes, LFOs, macros \
                  and a mod matrix",
    version: "1.0",
    author: "canopy",
    params: PARAMS,
    envelopes: &ENVELOPE_LAYOUTS,
    create: || Box::new(Wavetable::new()),
    data: DataKind::Wavetables,
    groups: params::groups,
};

/// The modulators the synth exports for the track's routes (see
/// [`Instrument::mod_outputs`]), in this order: the eight LFOs, the six
/// envelopes, the eight macros (as the matrix moves them), the mod wheel,
/// the pitch wheel, and the newest note's velocity and key. Mono LFOs and
/// envelopes, the macros and the wheels are the instrument's own values;
/// the rest are the newest voice's, held when the last one ends (a free
/// LFO keeps running). Ids are stable forever.
pub const MOD_OUTPUTS: &[ModOutput] = &[
    output("lfo1", "LFO 1", true),
    output("lfo2", "LFO 2", true),
    output("lfo3", "LFO 3", true),
    output("lfo4", "LFO 4", true),
    output("lfo5", "LFO 5", true),
    output("lfo6", "LFO 6", true),
    output("lfo7", "LFO 7", true),
    output("lfo8", "LFO 8", true),
    output("env1", "Amp Env", false),
    output("env2", "Env 2", false),
    output("env3", "Env 3", false),
    output("env4", "Env 4", false),
    output("env5", "Env 5", false),
    output("env6", "Env 6", false),
    output("macro1", "Macro 1", false),
    output("macro2", "Macro 2", false),
    output("macro3", "Macro 3", false),
    output("macro4", "Macro 4", false),
    output("macro5", "Macro 5", false),
    output("macro6", "Macro 6", false),
    output("macro7", "Macro 7", false),
    output("macro8", "Macro 8", false),
    output("mod_wheel", "Mod Wheel", false),
    output("pitch_bend", "Pitch Bend", true),
    output("velocity", "Velocity", false),
    output("note", "Note", true),
    output("aftertouch", "Aftertouch", false),
    output("slide", "Slide", false),
];

/// The source each of [`MOD_OUTPUTS`] reads.
const OUTPUT_SOURCES: [usize; 28] = [
    lfo_source(0),
    lfo_source(1),
    lfo_source(2),
    lfo_source(3),
    lfo_source(4),
    lfo_source(5),
    lfo_source(6),
    lfo_source(7),
    env_source(0),
    env_source(1),
    env_source(2),
    env_source(3),
    env_source(4),
    env_source(5),
    macro_source(0),
    macro_source(1),
    macro_source(2),
    macro_source(3),
    macro_source(4),
    macro_source(5),
    macro_source(6),
    macro_source(7),
    SRC_MOD_WHEEL,
    SRC_PITCH_BEND,
    SRC_VELOCITY,
    SRC_NOTE,
    SRC_AFTERTOUCH,
    SRC_SLIDE,
];
const _: () = assert!(OUTPUT_SOURCES.len() == MOD_OUTPUTS.len());

/// What source `source` (one of [`SOURCES`]) is exported as, if it is
/// one of [`MOD_OUTPUTS`]: what a drag handle beside it carries.
pub fn source_output(source: usize) -> Option<&'static ModOutput> {
    OUTPUT_SOURCES
        .iter()
        .position(|&s| s == source)
        .map(|i| &MOD_OUTPUTS[i])
}

const fn output(id: &'static str, name: &'static str, bipolar: bool) -> ModOutput {
    ModOutput { id, name, bipolar }
}

// What each meter slot holds (see `Instrument::meters`). Per-voice values
// come from the newest sounding voice; with none sounding, the mono LFOs
// and envelopes show where they are, the other LFOs rest at their start
// (at 0) and the rest show the unmodulated settings, so an idle panel is
// still.
/// Sounding voices.
pub const METER_VOICES: usize = 0;
/// LFO 1..8 output as the matrix sees it (after delay and rise),
/// `-1..=1`, in slots 1..=8.
pub const METER_LFO_VALUE: usize = 1;
/// LFO 1..8 phase, `0..=1`, in slots 9..=16: where to put the playhead on
/// its curve. A chaos LFO puts its attractor's second coordinate here
/// instead, `0..=1`, to draw its dot against the output.
pub const METER_LFO_PHASE: usize = METER_LFO_VALUE + LFO_COUNT;
/// Env 1..6 level, `0..=1`, in slots 17..=22.
pub const METER_ENV: usize = METER_LFO_PHASE + LFO_COUNT;
/// Env 1..6 playhead, in slots 23..=28: its [`EnvStage`] number plus how
/// far through that stage (see `env::Envelope::playhead`).
pub const METER_ENV_STAGE: usize = METER_ENV + ENV_COUNT;
/// The newest note's velocity, key (`-1..=1`), randoms, release velocity;
/// aftertouch, slide and the wheels (pitch bend `-1..=1`): the sources
/// besides LFOs, envelopes and macros, in slots 29..=37 (see
/// [`meter_sources`]).
pub const METER_VELOCITY: usize = METER_ENV_STAGE + ENV_COUNT;
pub const METER_NOTE: usize = METER_VELOCITY + 1;
pub const METER_RANDOM: usize = METER_VELOCITY + 2;
pub const METER_RANDOM2: usize = METER_VELOCITY + 3;
pub const METER_AFTERTOUCH: usize = METER_VELOCITY + 4;
pub const METER_SLIDE: usize = METER_VELOCITY + 5;
pub const METER_LIFT: usize = METER_VELOCITY + 6;
pub const METER_MOD_WHEEL: usize = METER_VELOCITY + 7;
pub const METER_BEND: usize = METER_VELOCITY + 8;
/// The newest voice's sounding note number (glide included), for key-tracked
/// displays, in slot 38.
pub const METER_PITCH: usize = METER_BEND + 1;
/// Osc A, B and C's modulated table position, `0..=1`, in slots 39..=41.
pub const METER_POSITION: usize = METER_PITCH + 1;
/// Osc A, B and C's modulated first warp amount, `0..=1`, in slots 42..=44.
pub const METER_WARP: usize = METER_POSITION + OSC_COUNT;
/// The output's peak, falling at about 20 dB a second, in slot 45.
pub const METER_LEVEL: usize = METER_WARP + OSC_COUNT;
/// One cycle of the output at the newest note's pitch, as
/// [`SCOPE_POINTS`] samples from a rising zero crossing, in slots 46..=61:
/// a steady oscilloscope.
pub const METER_SCOPE: usize = METER_LEVEL + 1;
pub const SCOPE_POINTS: usize = 16;
const _: () = assert!(METER_SCOPE + SCOPE_POINTS <= MAX_METERS, "a meter per slot");

/// The controller number hosts send channel pressure (aftertouch) as, in a
/// [`NoteEventKind::Control`] event: above MIDI's control changes, so it
/// can't be mistaken for one.
pub const PRESSURE: u8 = 128;
/// MPE's slide (the vertical move on a key), CC 74.
pub const SLIDE: u8 = 74;

/// Most voices sounding at once (the Polyphony parameter can lower it).
const VOICES: usize = 32;
/// Ramp for parameter changes, so knob moves and automation don't zipper.
const PARAM_RAMP_MS: f32 = 20.0;
/// The mod wheel's 7-bit steps would otherwise be audible.
const MOD_WHEEL_RAMP_MS: f32 = 10.0;
const BEND_RAMP_MS: f32 = 5.0;
/// Where the random sequence (start phases, per-note random) begins after
/// `reset`, so a render from the top always sounds the same.
const SEED: u32 = 0x9E37_79B9;
/// Where osc C's start phases begin (see `Wavetable::seed_c`).
const SEED_C: u32 = 0x85EB_CA6B;
/// Where the per-note values added since (the second random, random
/// detune) begin, kept apart so the older sequences stay as they were.
const SEED_EXTRA: u32 = 0xC2B2_AE35;
/// Notes the mono modes remember, for falling back to a still-held key.
const HELD_NOTES: usize = 16;
/// Output samples kept for the scope: enough for a cycle of a low note.
const SCOPE_RING: usize = 2048;
/// The level meter's fall per sample: about 20 dB a second at 48 kHz.
const LEVEL_FALL: f32 = 0.999_952;
/// On/off switches that fade over the parameter ramp instead of cutting.
const FADING_SWITCHES: [usize; 7] = [
    OSC_A.start + OSC_ON,
    OSC_B.start + OSC_ON,
    OSC_C.start + OSC_ON,
    SUB_ON,
    NOISE_ON,
    FILTER_ON,
    FILTER2_ON,
];

/// Names of the built-in tables, by their place in the bank (the table
/// parameters' labels hold them around [`USER_TABLE`], see [`table_of`]).
pub fn tables() -> &'static [&'static str] {
    &TABLE_NAMES
}

/// What each oscillator plays: its user table when its table choice is
/// [`USER_TABLE`] and one is loaded, else the built-in table chosen (the
/// first one for a "User" with nothing loaded, so it never goes silent).
fn osc_tables<'a>(
    bank: &'a Bank,
    user: Option<&'a UserTables>,
    controls: &Controls,
) -> [&'a Table; OSC_COUNT] {
    std::array::from_fn(|o| match table_of(controls.osc[o].table) {
        Some(table) => &bank.tables[table.min(bank.tables.len() - 1)],
        None => match user.and_then(|u| u.tables[o].as_deref()) {
            Some(table) => table.table(),
            None => &bank.tables[0],
        },
    })
}

/// Draws one cycle of `table` at `position` (`0..=1`) with warp mode
/// `warp_mode` (see [`WARPS_A`]) at `warp_amount`, into `out` (one point
/// per element). Cross-modulation warps draw with a sine standing in for
/// the other oscillator. The first call generates the tables, so call it
/// from the UI thread.
pub fn frame(table: usize, position: f32, warp_mode: usize, warp_amount: f32, out: &mut [f32]) {
    frame_warped(
        table,
        position,
        [(warp_mode, warp_amount), (WARP_OFF, 0.0)],
        out,
    );
}

/// [`frame`] through both warp slots, `(mode, amount)` each: the second
/// warps what the first made, as the oscillators do.
pub fn frame_warped(table: usize, position: f32, warps: [(usize, f32); 2], out: &mut [f32]) {
    if table >= tables::bank().tables.len() {
        out.fill(0.0);
        return;
    }
    TableSource::BuiltIn(table).draw(position, (MORPH_OFF, 0.0, 0.0), warps, out);
}

/// A table an oscillator's display draws: a built-in one, by its place in
/// [`tables`](fn@tables), or a user table. Drawing happens on the UI
/// thread (it may allocate, and the first use generates the tables).
#[derive(Clone, Copy)]
pub enum TableSource<'a> {
    BuiltIn(usize),
    User(&'a UserTable),
}

impl TableSource<'_> {
    fn table(&self) -> &Table {
        match self {
            TableSource::BuiltIn(index) => {
                let bank = tables::bank();
                &bank.tables[(*index).min(bank.tables.len() - 1)]
            }
            TableSource::User(table) => table.table(),
        }
    }

    /// Frames in the table.
    pub fn frames(&self) -> usize {
        self.table().frames
    }

    /// Draws one cycle at `position` (`0..=1`) into `out` (one point per
    /// element): spectrally morphed by `morph` (`(mode, amount, note Hz)`,
    /// see [`MORPHS`]), then through both warp slots (`(mode, amount)`
    /// each), as the oscillator plays it. Cross-modulation warps draw with
    /// a sine standing in for what they listen to; the remap warps read
    /// through a straight curve (see [`Self::draw_remapped`]).
    pub fn draw(
        &self,
        position: f32,
        morph: (usize, f32, f32),
        warps: [(usize, f32); 2],
        out: &mut [f32],
    ) {
        self.draw_remapped(position, morph, warps, &STRAIGHT_REMAP, out);
    }

    /// [`Self::draw`] with the remap warps reading through `remap` (see
    /// [`remap_table`]).
    pub fn draw_remapped(
        &self,
        position: f32,
        morph: (usize, f32, f32),
        warps: [(usize, f32); 2],
        remap: &Remap,
        out: &mut [f32],
    ) {
        let table = self.table();
        let n = out.len().max(1) as f32;
        let (mode, amount, hz) = morph;
        if morph::is_identity(mode, amount) {
            for (i, sample) in out.iter_mut().enumerate() {
                let p = i as f32 / n;
                *sample = osc::warped_sample(table, position, warps, remap, p, osc::fast_sin(p));
            }
        } else {
            let level = morph::morphed_level(table, position, mode, amount, hz);
            for (i, sample) in out.iter_mut().enumerate() {
                let p = i as f32 / n;
                *sample =
                    osc::warped_level_sample(&level, 2, 1.0, warps, remap, p, osc::fast_sin(p));
            }
        }
    }

    /// The cycle's harmonic levels at `position` after `morph` (as in
    /// [`Self::draw`]), harmonic 1 first, as many as `out` holds.
    pub fn spectrum(&self, position: f32, morph: (usize, f32, f32), out: &mut [f32]) {
        let (mode, amount, hz) = morph;
        morph::spectrum(self.table(), position, mode, amount, hz, out);
    }
}

/// Parameter `index` of `values` (indexed like [`PARAMS`]), clamped; its
/// default if missing or NaN.
fn value_in(values: &[f32], index: usize) -> f32 {
    crate::param_value(PARAMS, values, index)
}

/// Filter 1's gain in dB at `freq` Hz for the parameter values `values`
/// (indexed like [`PARAMS`]; missing ones take their defaults), without
/// drive, key tracking or modulation. 0 dB when the filter is off.
pub fn filter_response_db(values: &[f32], freq: f32, sample_rate: f32) -> f32 {
    filter_curve_db(0, values, freq, sample_rate)
}

/// [`filter_response_db`] for filter `filter` (0 or 1). In serial, what goes
/// into filter 1 hears the sum of both curves in dB.
pub fn filter_curve_db(filter: usize, values: &[f32], freq: f32, sample_rate: f32) -> f32 {
    let value = |index: usize| value_in(values, filter_param(filter.min(1), index));
    if value(FILTER_ON) < 0.5 || sample_rate.is_nan() || sample_rate <= 0.0 {
        return 0.0;
    }
    filter::response_db(
        value(FILTER_TYPE) as usize,
        value(FILTER_CUTOFF),
        value(FILTER_RESONANCE),
        value(FILTER_BLEND),
        value(FILTER_MIX),
        freq,
        sample_rate,
    )
}

/// `signal` at `sample_rate` without its DC, as the voices take it out of
/// their sources before the filters (see [`DC_BLOCK_HZ`]): for checking
/// what plays against a wave worked out by hand.
pub fn dc_blocked(signal: &[f32], sample_rate: f32) -> Vec<f32> {
    let mut out = signal.to_vec();
    dsp::DcBlocker::default().process(&mut out, voice::dc_pole(sample_rate));
    out
}

/// Oscillator `osc`'s remap curve (see [`WARP_REMAP`]) as nodes, as the
/// parameter values `values` (indexed like [`PARAMS`]) draw it.
pub fn remap_nodes(values: &[f32], osc: usize) -> Nodes {
    let osc = osc.min(OSC_COUNT - 1);
    let value = |offset: usize| value_in(values, osc_param(osc, offset));
    let count = (value(OSC_REMAP_NODES) as usize + 2).min(MAX_REMAP_NODES);
    let mut nodes = [(0.0, 0.0, 0.0); MAX_REMAP_NODES];
    for (i, node) in nodes.iter_mut().enumerate().take(count) {
        *node = (
            value(OSC_REMAP_X + i),
            value(OSC_REMAP_Y + i),
            value(OSC_REMAP_CURVE + i),
        );
    }
    Nodes::new(&nodes[..count])
}

/// Oscillator `osc`'s remap curve as the table its remap warps read, for
/// the parameter values `values` (indexed like [`PARAMS`]).
pub fn remap_table(values: &[f32], osc: usize) -> Remap {
    let nodes = remap_nodes(values, osc);
    std::array::from_fn(|i| nodes.value(i as f32 / osc::REMAP_SIZE as f32))
}

/// The oscillator whose remap curve parameter `index` shapes, if it is one.
fn remap_osc(index: usize) -> Option<usize> {
    (0..OSC_COUNT)
        .find(|&o| (osc_param(o, OSC_REMAP_NODES)..osc_param(o, OSC_PARAMS)).contains(&index))
}

/// LFO `lfo`'s Points shape, as the parameter values `values` (indexed
/// like [`PARAMS`]) draw it.
pub fn lfo_nodes(values: &[f32], lfo: usize) -> Nodes {
    let lfo = lfo.min(LFO_COUNT - 1);
    let value = |offset: usize| value_in(values, lfo_param(lfo, offset));
    let count = value(LFO_NODES) as usize + 2;
    let mut nodes = [(0.0, 0.0, 0.0); MAX_NODES];
    for (i, node) in nodes.iter_mut().enumerate().take(count) {
        *node = (
            value(LFO_NODE_X + i),
            value(LFO_NODE_Y + i),
            value(LFO_NODE_CURVE + i),
        );
    }
    Nodes::new(&nodes[..count])
}

/// LFO `lfo`'s shape at `phase` (whole cycles), `-1..=1`, for the parameter
/// values `values` (indexed like [`PARAMS`]): [`lfo_value`] for the fixed
/// shapes, the drawn points with their glide for the custom one, the
/// nodes for the Points shape. The envelope-like modes play the cycle
/// once, so past 1 they stay at the end. The chaos shapes have no cycle:
/// see [`chaos_trace`]; here they are flat.
pub fn lfo_shape_at(values: &[f32], lfo: usize, phase: f32) -> f32 {
    let lfo = lfo.min(LFO_COUNT - 1);
    let value = |offset: usize| value_in(values, lfo_param(lfo, offset));
    let shape = value(LFO_SHAPE) as usize;
    let mode = value(LFO_MODE) as usize;
    let one_shot = !lfo::repeats(mode);
    match shape {
        SHAPE_CUSTOM => {
            let points = std::array::from_fn(|i| value(LFO_POINT + i));
            let phase = if one_shot { phase.min(1.0) } else { phase };
            2.0 * custom_shape(&points, phase, value(LFO_GLIDE), !one_shot) - 1.0
        }
        SHAPE_NODES => {
            let phase = if one_shot && phase >= 1.0 { 1.0 } else { phase };
            2.0 * lfo_nodes(values, lfo).value(phase) - 1.0
        }
        _ => lfo_value(shape, phase),
    }
}

/// Mod slot `slot` as the voices run it, from parameter values `values`
/// (indexed like [`PARAMS`]); `None` if it's unused or bypassed.
pub fn slot_from(values: &[f32], slot: usize) -> Option<ModSlot> {
    let value = |offset: usize| value_in(values, slot_param(slot, offset));
    let source = value(SLOT_SOURCE) as usize;
    let dest = value(SLOT_DEST) as usize;
    if source == SRC_NONE || dest == DEST_NONE || value(SLOT_BYPASS) >= 0.5 {
        return None;
    }
    Some(ModSlot::new(
        source,
        dest,
        value(SLOT_AMOUNT),
        value(SLOT_AUX) as usize,
        value(SLOT_CURVE),
        value(SLOT_POLARITY) as usize,
    ))
}

/// Every mod source as the meters (`meters`, see the `METER_*` slots)
/// show it now, with the macros from the parameter values `values`.
pub fn meter_sources(values: &[f32], meters: &[f32; MAX_METERS]) -> [f32; SOURCE_COUNT] {
    let mut s = [0.0; SOURCE_COUNT];
    for i in 0..LFO_COUNT {
        s[lfo_source(i)] = meters[METER_LFO_VALUE + i];
    }
    for i in 0..ENV_COUNT {
        s[env_source(i)] = meters[METER_ENV + i];
    }
    for i in 0..MACRO_COUNT {
        s[macro_source(i)] = value_in(values, macro_param(i));
    }
    s[SRC_VELOCITY] = meters[METER_VELOCITY];
    s[SRC_NOTE] = meters[METER_NOTE];
    s[SRC_RANDOM] = meters[METER_RANDOM];
    s[SRC_RANDOM2] = meters[METER_RANDOM2];
    s[SRC_AFTERTOUCH] = meters[METER_AFTERTOUCH];
    s[SRC_SLIDE] = meters[METER_SLIDE];
    s[SRC_LIFT] = meters[METER_LIFT];
    s[SRC_MOD_WHEEL] = meters[METER_MOD_WHEEL];
    s[SRC_PITCH_BEND] = meters[METER_BEND];
    s[SRC_FIXED] = 1.0;
    s
}

/// Where modulation has everything now, for drawing: each destination's
/// modulation in its own unit (see [`DESTINATION_SCALES`]), every source
/// (macros as the matrix moves them) and each slot's output (`-1..=1`
/// times its amount, before the destination's scale), from the parameter
/// values `values` and the meters `meters`. The synth smooths knob moves,
/// which this doesn't, so mid-gesture it can lead by a few milliseconds.
pub fn live_modulation(values: &[f32], meters: &[f32; MAX_METERS]) -> LiveModulation {
    let mut sources = meter_sources(values, meters);
    let mut slots = [ModSlot::default(); MOD_SLOTS];
    let mut used = [usize::MAX; MOD_SLOTS];
    let mut count = 0;
    for slot in 0..MOD_SLOTS {
        if let Some(mod_slot) = slot_from(values, slot) {
            slots[count] = mod_slot;
            used[count] = slot;
            count += 1;
        }
    }
    let destinations = run_matrix(&slots[..count], &mut sources);
    let mut outputs = [0.0; MOD_SLOTS];
    for (mod_slot, slot) in slots[..count].iter().zip(used) {
        outputs[slot] = mod_slot.value(&sources);
    }
    LiveModulation {
        destinations,
        sources,
        slots: outputs,
    }
}

/// Whether destination `dest` moves its knob in octaves (the cutoffs, LFO
/// rates and envelope times) rather than by adding to it.
fn in_octaves(dest: usize) -> bool {
    let env_times = DEST_ENV1_ATTACK..env_dest(ENV_COUNT, 0);
    matches!(dest, DEST_CUTOFF | DEST_CUTOFF2)
        || (0..LFO_COUNT).any(|lfo| lfo_rate_dest(lfo) == dest)
        || (env_times.contains(&dest) && (dest - DEST_ENV1_ATTACK) % ENV_DESTS != ENV_DEST_SUSTAIN)
}

/// Parameter `index`'s value `base` moved by `modulation` of its knob's
/// destination (see [`param_dest`]), in the destination's own unit, as the
/// voices apply it: added for most, in octaves for the cutoffs, LFO
/// rates and envelope times; kept in the parameter's range (a choice isn't rounded, so a
/// semitone knob's live dot glides). `base` if modulation can't move it.
pub fn modulated_param(index: usize, base: f32, modulation: f32) -> f32 {
    let (Some(dest), Some(param)) = (param_dest(index), PARAMS.get(index)) else {
        return base;
    };
    let value = if in_octaves(dest) {
        base * modulation.exp2()
    } else {
        base + modulation
    };
    match param.kind {
        ParameterKind::Float { min, max, .. } => value.clamp(min, max),
        ParameterKind::Enum { labels, .. } => {
            value.clamp(0.0, labels.len().saturating_sub(1) as f32)
        }
    }
}

/// How far, in parameter `index`'s knob's normalized units (`0..=1` the
/// whole range), a slot at amount 1 moves it when its source is at 1: what
/// a modulation ring's arc per unit of the slot's amount is (it may be
/// more than the whole range, as for a pitch slot on a semitone knob). 0 if
/// modulation can't move it.
pub fn ring_unit(index: usize) -> f32 {
    let (Some(dest), Some(param)) = (param_dest(index), PARAMS.get(index)) else {
        return 0.0;
    };
    let scale = DESTINATION_SCALES[dest];
    match param.kind {
        ParameterKind::Float { min, max, .. } if in_octaves(dest) && min > 0.0 => {
            scale / (max / min).log2()
        }
        ParameterKind::Float { min, max, .. } if max > min => scale / (max - min),
        ParameterKind::Enum { labels, .. } if labels.len() > 1 => scale / (labels.len() - 1) as f32,
        _ => 0.0,
    }
}

/// What [`live_modulation`] finds.
#[derive(Debug, Clone, Copy)]
pub struct LiveModulation {
    pub destinations: [f32; DEST_COUNT],
    pub sources: [f32; SOURCE_COUNT],
    pub slots: [f32; MOD_SLOTS],
}

fn is_logarithmic(param: &ParamDescriptor) -> bool {
    matches!(
        param.kind,
        ParameterKind::Float {
            logarithmic: true,
            ..
        }
    )
}

/// Three-oscillator wavetable synth, up to 32 voices. See the module docs.
pub struct Wavetable {
    bank: &'static Bank,
    /// The oscillators' own tables, as the host loaded them.
    user: Option<Arc<UserTables>>,
    voices: Vec<Voice>,
    /// Every parameter as last set, clamped.
    values: [f32; PARAM_COUNT],
    /// The same, smoothed at the control rate. Logarithmic parameters are
    /// smoothed in octaves, so sweeps sound even.
    smooth: Vec<Smoothed>,
    /// The parameters whose smoothers are ramping, so a control tick steps
    /// only those rather than all thousand. Room for every parameter is
    /// reserved up front, so adding one never allocates.
    moving: Vec<usize>,
    is_moving: [bool; PARAM_COUNT],
    /// Parameters a modulator moves ([`Instrument::modulate`]): their
    /// smoothers ramp over one control tick rather than the knob's ramp.
    fast: [bool; PARAM_COUNT],
    envs: [AdsrDesc; ENV_COUNT],
    controls: Controls,
    /// Whether `controls` must be gathered again at the next control tick
    /// because a setting, the tempo or a remap curve changed. Smoothers
    /// still ramping (and the pitch wheel) are checked at the tick itself.
    /// Gathering them is a good part of a quiet patch's cost, so it only
    /// happens when something they're made from has moved.
    controls_stale: bool,
    /// The free-running LFOs that voices in Free mode copy when they
    /// start, and the mono LFOs every voice reads.
    global_lfos: [Lfo; LFO_COUNT],
    /// The mono envelopes (the amp's is never one).
    mono_envs: [Envelope; ENV_COUNT],
    /// Seconds since the last note started, for the mono LFOs' delay and rise.
    mono_age: f32,
    /// The instrument-wide sources as the last tick left them, macros
    /// moved by the instrument-wide slots: what is exported.
    globals: [f32; SOURCE_COUNT],
    /// Each mono LFO's rate modulation in octaves, from the last tick.
    mono_rate_mods: [f32; LFO_COUNT],
    /// What each oscillator, the sub and the noise send the oscillators
    /// listening to them, per voice as it renders.
    scratch: Box<[Unison; SCRATCH_COUNT]>,
    /// Working space for the spectral morphs.
    morph_scratch: Box<morph::Scratch>,
    /// Each oscillator's remap curve as a table, rebuilt at a control tick
    /// after its nodes change (`remap_changed`), and how steep it gets.
    remaps: Box<[Remap; OSC_COUNT]>,
    remap_slopes: [f32; OSC_COUNT],
    remap_changed: [bool; OSC_COUNT],
    mod_wheel: Smoothed,
    /// The pitch wheel, `-1..=1`.
    bend: Smoothed,
    aftertouch: Smoothed,
    slide: Smoothed,
    bpm: f32,
    sample_rate: f32,
    /// Samples until the next control tick. Carries across blocks and event
    /// splits so ticks stay evenly spaced.
    countdown: usize,
    notes_started: u64,
    /// Xorshift state for start phases and per-note random values.
    seed: u32,
    /// The same for osc C's start phases, kept apart so that adding C left
    /// A's and B's (and so the default patch) as they were.
    seed_c: u32,
    /// The same for the per-note values added later.
    seed_extra: u32,
    gain: Gain,
    /// Keys held in the mono modes, oldest first.
    held: [Pitch; HELD_NOTES],
    held_count: usize,
    /// Every key down, whatever the voice mode, for the mono LFOs and
    /// envelopes and for glides that only happen between held notes.
    down: [bool; 128],
    keys_down: usize,
    /// The last note played, where the next one glides from.
    last_note: Option<f32>,
    /// The last output samples (both sides mixed), for the scope.
    scope: Box<[f32; SCOPE_RING]>,
    scope_pos: usize,
    /// The output's peak, falling.
    level: f32,
    /// Exported values of the per-voice modulators, held when the last
    /// voice ends.
    held_outputs: [f32; SOURCE_COUNT],
}

impl Wavetable {
    pub fn new() -> Self {
        morph::prepare();
        halfband::prepare();
        let mut synth = Self {
            bank: tables::bank(),
            user: None,
            voices: (0..VOICES)
                .map(|i| Voice::new(SEED ^ (i as u32 + 1).wrapping_mul(0x632B_E5AB)))
                .collect(),
            values: [0.0; PARAM_COUNT],
            smooth: (0..PARAM_COUNT)
                .map(|_| Smoothed::new(0.0, PARAM_RAMP_MS))
                .collect(),
            moving: Vec::with_capacity(PARAM_COUNT),
            is_moving: [false; PARAM_COUNT],
            fast: [false; PARAM_COUNT],
            envs: [AdsrDesc::default(); ENV_COUNT],
            controls: Controls::default(),
            controls_stale: true,
            global_lfos: std::array::from_fn(|i| Lfo::new(SEED.rotate_left(i as u32 * 7))),
            mono_envs: std::array::from_fn(|_| Envelope::new(AdsrDesc::default(), 48_000.0)),
            mono_age: 0.0,
            globals: [0.0; SOURCE_COUNT],
            mono_rate_mods: [0.0; LFO_COUNT],
            scratch: Box::new([[[0.0; 2 * BLOCK]; MAX_UNISON]; SCRATCH_COUNT]),
            morph_scratch: Box::default(),
            remaps: Box::new([STRAIGHT_REMAP; OSC_COUNT]),
            remap_slopes: [1.0; OSC_COUNT],
            remap_changed: [true; OSC_COUNT],
            mod_wheel: Smoothed::new(0.0, MOD_WHEEL_RAMP_MS),
            bend: Smoothed::new(0.0, BEND_RAMP_MS),
            aftertouch: Smoothed::new(0.0, MOD_WHEEL_RAMP_MS),
            slide: Smoothed::new(0.0, MOD_WHEEL_RAMP_MS),
            bpm: 120.0,
            sample_rate: 48_000.0,
            countdown: 0,
            notes_started: 0,
            seed: SEED,
            seed_c: SEED_C,
            seed_extra: SEED_EXTRA,
            gain: Gain::new(0.0),
            held: [Pitch::A4; HELD_NOTES],
            held_count: 0,
            down: [false; 128],
            keys_down: 0,
            last_note: None,
            scope: Box::new([0.0; SCOPE_RING]),
            scope_pos: 0,
            level: 0.0,
            held_outputs: [0.0; SOURCE_COUNT],
        };
        for (index, param) in PARAMS.iter().enumerate() {
            synth.set_param(index, param.default_value());
        }
        synth.prepare(48_000.0, 512);
        synth
    }

    /// Every parameter's value as last set, indexed like [`PARAMS`]: what
    /// [`live_modulation`] reads alongside the meters.
    pub fn param_values(&self) -> &[f32] {
        &self.values
    }

    /// Source `source`'s value as of the last block, if it is
    /// instrument-wide (a mono LFO or envelope, a macro as the matrix moves
    /// it, a wheel, aftertouch, slide, Fixed): one value for every voice,
    /// which something outside the synth (a track's effect) can follow.
    pub fn global_source(&self, source: usize) -> Option<f32> {
        self.is_global(source).then(|| self.globals[source])
    }

    /// Whether source `source` is instrument-wide (see
    /// [`Self::global_source`]) with the settings as they are.
    pub fn is_global(&self, source: usize) -> bool {
        is_global_source(source, &self.controls)
    }

    fn random(&mut self) -> f32 {
        next_random(&mut self.seed)
    }

    /// How much envelope `env`'s times stretch: 1, or with tempo sync on,
    /// 120 BPM over the song's tempo (the times are as set at 120).
    fn env_time_scale(&self, env: usize) -> f32 {
        if self.on(env_sync_param(env)) {
            120.0 / self.bpm
        } else {
            1.0
        }
    }

    /// Envelope `env`'s stages as the voices play them: as set, stretched
    /// by tempo sync.
    fn env_desc(&self, env: usize) -> AdsrDesc {
        let mut desc = self.envs[env];
        let scale = self.env_time_scale(env);
        for stage in [&mut desc.attack, &mut desc.decay, &mut desc.release] {
            stage.time_ms *= scale;
        }
        desc
    }

    /// Hands every envelope's stages to the voices and the mono envelopes.
    fn push_envelopes(&mut self) {
        for env in 0..ENV_COUNT {
            let desc = self.env_desc(env);
            for voice in &mut self.voices {
                voice.env[env].set_desc(desc);
            }
            self.mono_envs[env].set_desc(desc);
        }
    }

    fn enum_value(&self, index: usize) -> usize {
        self.values[index] as usize
    }

    fn on(&self, index: usize) -> bool {
        self.values[index] >= 0.5
    }

    fn smoothed(&self, index: usize) -> f32 {
        self.smooth[index].current()
    }

    /// Whether a fading switch is on or still fading out.
    fn fading_in(&self, index: usize) -> bool {
        self.smooth[index].current() > 0.0
    }

    /// Where source `route` (an index into [`SOURCE_ROUTES`]) goes: one of
    /// the `voice::BUS_*` constants.
    fn bus(&self, route: usize) -> usize {
        let (switch, target) = SOURCE_ROUTES[route];
        if self.on(switch) {
            BUS_FILTER1 + self.enum_value(target).min(1)
        } else {
            BUS_DIRECT
        }
    }

    /// Mod slot `slot` as the voices run it, if it's in use.
    fn mod_slot(&self, slot: usize) -> Option<ModSlot> {
        let p = |offset| slot_param(slot, offset);
        let source = self.enum_value(p(SLOT_SOURCE));
        let dest = self.enum_value(p(SLOT_DEST));
        if source == SRC_NONE || dest == DEST_NONE || self.on(p(SLOT_BYPASS)) {
            return None;
        }
        Some(ModSlot::new(
            source,
            dest,
            self.smoothed(p(SLOT_AMOUNT)),
            self.enum_value(p(SLOT_AUX)),
            self.smoothed(p(SLOT_CURVE)),
            self.enum_value(p(SLOT_POLARITY)),
        ))
    }

    /// LFO `i`'s settings as the voices run them.
    fn lfo_settings(&self, i: usize) -> LfoSettings {
        let p = |offset| lfo_param(i, offset);
        let rate_hz = if self.on(p(LFO_SYNC)) {
            let division = self.enum_value(p(LFO_DIVISION));
            self.bpm / 60.0 / DIVISION_BEATS[division.min(DIVISION_BEATS.len() - 1)]
        } else {
            self.smoothed(p(LFO_RATE)).exp2()
        };
        let shape = self.enum_value(p(LFO_SHAPE));
        // Only the drawn shapes read their points or nodes.
        let points = if shape == SHAPE_CUSTOM {
            std::array::from_fn(|k| self.smoothed(p(LFO_POINT + k)))
        } else {
            [0.0; LFO_POINTS]
        };
        let nodes = if shape == SHAPE_NODES {
            let count = self.enum_value(p(LFO_NODES)) + 2;
            let mut nodes = [(0.0, 0.0, 0.0); MAX_NODES];
            for (k, node) in nodes.iter_mut().enumerate().take(count) {
                *node = (
                    self.smoothed(p(LFO_NODE_X + k)),
                    self.smoothed(p(LFO_NODE_Y + k)),
                    self.smoothed(p(LFO_NODE_CURVE + k)),
                );
            }
            Nodes::new(&nodes[..count])
        } else {
            Nodes::default()
        };
        LfoSettings {
            shape,
            mode: self.enum_value(p(LFO_MODE)),
            rate_hz,
            start_phase: self.values[p(LFO_PHASE)],
            smooth: self.smoothed(p(LFO_SMOOTH)),
            rise: self.values[p(LFO_RISE)],
            delay: self.values[p(LFO_DELAY)],
            loop_point: self.smoothed(p(LFO_LOOP)),
            mono: self.on(p(LFO_MONO)),
            points,
            glide: self.smoothed(p(LFO_GLIDE)),
            nodes,
        }
    }

    /// Gathers the smoothed parameters into what the voices read.
    fn build_controls(&self) -> Controls {
        let osc: [OscControls; OSC_COUNT] = std::array::from_fn(|o| {
            let p = |offset| osc_param(o, offset);
            OscControls {
                on: self.fading_in(p(OSC_ON)),
                table: self.enum_value(p(OSC_TABLE)),
                position: self.smoothed(p(OSC_POSITION)),
                warp_mode: self.enum_value(p(OSC_WARP_MODE)),
                warp: self.smoothed(p(OSC_WARP)),
                warp2_mode: self.enum_value(p(OSC_WARP2_MODE)),
                warp2: self.smoothed(p(OSC_WARP2)),
                remap_slope: self.remap_slopes[o],
                transpose: (self.enum_value(p(OSC_OCTAVE)) as f32 - 4.0) * 12.0
                    + (self.enum_value(p(OSC_SEMI)) as f32 - 12.0),
                fine: self.smoothed(p(OSC_FINE)),
                unison: self.enum_value(p(OSC_UNISON)) + 1,
                detune: self.smoothed(p(OSC_DETUNE)),
                detune_mode: self.enum_value(p(OSC_DETUNE_MODE)),
                blend: self.smoothed(p(OSC_BLEND)),
                width: self.smoothed(p(OSC_WIDTH)),
                stack: self.enum_value(p(OSC_STACK)),
                warp_spread: self.smoothed(p(OSC_WARP_SPREAD)),
                position_spread: self.smoothed(p(OSC_POSITION_SPREAD)),
                morph_mode: self.enum_value(p(OSC_MORPH_MODE)),
                morph: self.smoothed(p(OSC_MORPH)),
                mod_source: self.enum_value(p(OSC_MOD_SOURCE)),
                keytrack: self.on(p(OSC_KEYTRACK)),
                phase: self.values[p(OSC_PHASE)],
                random: self.values[p(OSC_RANDOM)],
                level: self.smoothed(p(OSC_LEVEL)),
                pan: self.smoothed(p(OSC_PAN)),
                bus: self.bus(OSC_ROUTES[o]),
                fade: self.smoothed(p(OSC_ON)),
            }
        });
        let filters: [FilterControls; 2] = std::array::from_fn(|f| {
            let p = |index| filter_param(f, index);
            FilterControls {
                on: self.fading_in(p(FILTER_ON)),
                fade: self.smoothed(p(FILTER_ON)),
                kind: self.enum_value(p(FILTER_TYPE)),
                cutoff_log2: self.smoothed(p(FILTER_CUTOFF)),
                resonance: self.smoothed(p(FILTER_RESONANCE)),
                drive: self.smoothed(p(FILTER_DRIVE)),
                keytrack: self.smoothed(p(FILTER_KEYTRACK)),
                mix: self.smoothed(p(FILTER_MIX)),
                blend: self.smoothed(p(FILTER_BLEND)),
            }
        });
        let mut slots = [ModSlot::default(); MOD_SLOTS];
        let mut slot_count = 0;
        let mut lfo_used = [false; LFO_COUNT];
        for slot in 0..MOD_SLOTS {
            if let Some(mod_slot) = self.mod_slot(slot) {
                for source in [mod_slot.source, mod_slot.aux] {
                    if let Some(lfo) = source_lfo(source) {
                        lfo_used[lfo] = true;
                    }
                }
                slots[slot_count] = mod_slot;
                slot_count += 1;
            }
        }
        Controls {
            osc,
            sub_on: self.fading_in(SUB_ON),
            sub_shape: self.enum_value(SUB_SHAPE),
            sub_transpose: (self.enum_value(SUB_OCTAVE) as f32 - 4.0) * 12.0,
            sub_level: self.smoothed(SUB_LEVEL),
            sub_bus: self.bus(SUB_ROUTE),
            sub_fade: self.smoothed(SUB_ON),
            noise_on: self.fading_in(NOISE_ON),
            noise_kind: self.enum_value(NOISE_TYPE),
            noise_level: self.smoothed(NOISE_LEVEL),
            noise_bus: self.bus(NOISE_ROUTE),
            noise_fade: self.smoothed(NOISE_ON),
            filters,
            parallel: self.enum_value(FILTER_ROUTING) == 1,
            env_delay: std::array::from_fn(|e| {
                self.values[env_param(e, ENV_DELAY)] * self.env_time_scale(e)
            }),
            env_hold: std::array::from_fn(|e| {
                self.values[env_param(e, ENV_HOLD)] * self.env_time_scale(e)
            }),
            env_mono: std::array::from_fn(|e| e > 0 && self.on(env_mono_param(e))),
            lfo: std::array::from_fn(|i| self.lfo_settings(i)),
            slots,
            slot_count,
            lfo_used,
            globals: self.globals,
            bend: self.bend.current(),
            bend_range: self.smoothed(BEND_RANGE),
            velocity_sens: self.smoothed(VELOCITY_SENS),
            volume_db: self.smoothed(MASTER_VOLUME),
            glide_curve: self.values[GLIDE_CURVE],
            oversample: self.enum_value(QUALITY) == 1,
            sample_rate: self.sample_rate,
        }
    }

    /// Runs the instrument-wide modulators on by `seconds`: the free LFO
    /// clocks, the mono LFOs and envelopes, and the macros as the slots
    /// reading only instrument-wide sources move them. Writes the result
    /// into the controls the voices read next.
    fn run_globals(&mut self, seconds: f32) {
        let held = self.keys_down > 0;
        self.mono_age += seconds;
        for (i, lfo) in self.global_lfos.iter_mut().enumerate() {
            let settings = &self.controls.lfo[i];
            if settings.mono {
                if settings.running(self.mono_age, &lfo.mods) {
                    let rate = settings.rate_hz * self.mono_rate_mods[i].exp2();
                    lfo.advance(settings, rate, seconds, held);
                }
            } else {
                // Free mode is what the shared clocks always run in; the
                // per-note modes only apply to voices.
                let free = LfoSettings {
                    mode: MODE_FREE,
                    ..*settings
                };
                lfo.advance(&free, settings.rate_hz, seconds, held);
            }
        }
        let samples = (seconds * self.sample_rate).round() as u32;
        for (i, env) in self.mono_envs.iter_mut().enumerate() {
            if self.controls.env_mono[i] {
                env.advance(samples);
            }
        }
        let c = &self.controls;
        let g = &mut self.globals;
        *g = [0.0; SOURCE_COUNT];
        for m in 0..MACRO_COUNT {
            g[macro_source(m)] = self.smooth[macro_param(m)].current();
        }
        g[SRC_MOD_WHEEL] = self.mod_wheel.current();
        g[SRC_PITCH_BEND] = self.bend.current();
        g[SRC_AFTERTOUCH] = self.aftertouch.current();
        g[SRC_SLIDE] = self.slide.current();
        g[SRC_FIXED] = 1.0;
        for (i, lfo) in self.global_lfos.iter().enumerate() {
            if c.lfo[i].mono {
                g[lfo_source(i)] = lfo.value * c.lfo[i].rise_gain(self.mono_age, &lfo.mods);
            }
        }
        for (i, env) in self.mono_envs.iter().enumerate() {
            if c.env_mono[i] {
                g[env_source(i)] = env.level();
            }
        }
        // The slots that read only instrument-wide values run here too,
        // moving the macros (for everyone) and the mono LFOs' rates.
        let mut slots = [ModSlot::default(); MOD_SLOTS];
        let mut count = 0;
        for slot in &c.slots[..c.slot_count] {
            if is_global_source(slot.source, c) && is_global_source(slot.aux, c) {
                slots[count] = *slot;
                count += 1;
            }
        }
        let mods = run_matrix(&slots[..count], g);
        for (i, rate) in self.mono_rate_mods.iter_mut().enumerate() {
            *rate = mods[lfo_rate_dest(i)];
        }
        // The rest of what moves the mono LFOs and envelopes; the shared
        // clocks of the per-voice LFOs stay as set.
        for (i, lfo) in self.global_lfos.iter_mut().enumerate() {
            lfo.mods = if c.lfo[i].mono {
                voice::lfo_mods(&mods, i)
            } else {
                LfoMods::default()
            };
        }
        for (e, env) in self.mono_envs.iter_mut().enumerate() {
            if c.env_mono[e] {
                env.set_mods(voice::env_mods(&mods, e));
            }
        }
        // The voices start from the knobs: their own pass moves the macros.
        let mut start = *g;
        for m in 0..MACRO_COUNT {
            start[macro_source(m)] = self.smooth[macro_param(m)].current();
        }
        self.controls.globals = start;
    }

    fn control_tick(&mut self) {
        // Whether anything the controls are made from moves this tick: a
        // ramping smoother (checked before the step, so a ramp's last
        // step still gathers its final value) or the pitch wheel.
        let moving = !self.moving.is_empty() || self.bend.is_smoothing();
        let mut i = 0;
        while i < self.moving.len() {
            let index = self.moving[i];
            let smoothed = &mut self.smooth[index];
            smoothed.tick();
            if smoothed.is_smoothing() {
                i += 1;
            } else {
                self.is_moving[index] = false;
                self.moving.swap_remove(i);
            }
        }
        for wheel in [
            &mut self.mod_wheel,
            &mut self.bend,
            &mut self.aftertouch,
            &mut self.slide,
        ] {
            wheel.tick();
        }
        self.update_remaps();
        if moving || self.controls_stale {
            self.controls = self.build_controls();
            self.controls_stale = false;
        } else {
            debug_assert!(
                {
                    let mut fresh = self.build_controls();
                    fresh.globals = self.controls.globals;
                    fresh == self.controls
                },
                "the cached controls fell behind a setting"
            );
        }
        self.run_globals(BLOCK as f32 / self.sample_rate);
        let tables = osc_tables(self.bank, self.user.as_deref(), &self.controls);
        let newest = self.newest_voice().map(|v| v.started);
        for voice in self.voices.iter_mut().filter(|v| v.is_active()) {
            // The newest voice's LFOs are on display: all of them run there.
            let shown = Some(voice.started) == newest;
            voice.tick(
                &self.controls,
                &tables,
                &mut self.morph_scratch,
                true,
                false,
                shown,
            );
        }
    }

    /// Rebuilds the remap curves whose nodes changed since the last tick.
    fn update_remaps(&mut self) {
        for o in 0..OSC_COUNT {
            if std::mem::take(&mut self.remap_changed[o]) {
                self.remaps[o] = remap_table(&self.values, o);
                self.remap_slopes[o] = osc::remap_slope(&self.remaps[o]);
                self.controls_stale = true;
            }
        }
    }

    /// Ramps parameter `index`'s smoother toward `target`: over one control
    /// tick if a modulator moves it (`fast`), else over the knob's ramp.
    fn glide_param(&mut self, index: usize, target: f32, fast: bool) {
        if self.fast[index] != fast {
            self.fast[index] = fast;
            let control_rate = self.sample_rate / BLOCK as f32;
            let ramp_ms = if fast {
                1000.0 / control_rate
            } else {
                PARAM_RAMP_MS
            };
            self.smooth[index].set_ramp(ramp_ms, control_rate);
        }
        self.smooth[index].set_target(target);
        if !self.is_moving[index] {
            self.is_moving[index] = true;
            self.moving.push(index);
        }
    }

    /// Every smoothed setting: the parameters and the controllers.
    fn smoothers(&mut self) -> impl Iterator<Item = &mut Smoothed> {
        let wheels = [
            &mut self.mod_wheel,
            &mut self.bend,
            &mut self.aftertouch,
            &mut self.slide,
        ];
        self.smooth.iter_mut().chain(wheels)
    }

    /// Jumps every smoothed setting to its target.
    fn settle(&mut self) {
        for index in self.moving.drain(..) {
            let smoothed = &mut self.smooth[index];
            smoothed.settle();
            self.is_moving[index] = false;
        }
        for wheel in [
            &mut self.mod_wheel,
            &mut self.bend,
            &mut self.aftertouch,
            &mut self.slide,
        ] {
            wheel.settle();
        }
        self.update_remaps();
        self.controls = self.build_controls();
        self.controls_stale = false;
        self.controls.globals = self.globals;
        for m in 0..MACRO_COUNT {
            self.controls.globals[macro_source(m)] = self.smooth[macro_param(m)].current();
        }
    }

    /// Adds every sounding voice into `left` and `right`. Returns whether any was sounding.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        if !self.voices.iter().any(Voice::is_active) {
            // Nothing to ramp for, so the next note starts at the settings
            // as they are now. The instrument-wide modulators keep time
            // regardless.
            self.settle();
            if !left.is_empty() {
                self.run_globals(left.len() as f32 / self.sample_rate);
            }
            return false;
        }
        let mut start = 0;
        while start < left.len() {
            if self.countdown == 0 {
                self.control_tick();
                self.countdown = BLOCK;
            }
            let end = start + self.countdown.min(left.len() - start);
            let waves = Waves {
                bank: self.bank,
                tables: osc_tables(self.bank, self.user.as_deref(), &self.controls),
                remaps: &self.remaps,
            };
            for voice in self.voices.iter_mut().filter(|v| v.is_active()) {
                voice.render(
                    &self.controls,
                    &waves,
                    &mut self.scratch,
                    &mut left[start..end],
                    &mut right[start..end],
                );
            }
            self.countdown -= end - start;
            start = end;
        }
        true
    }

    fn handle(&mut self, kind: NoteEventKind) {
        match kind {
            NoteEventKind::On { pitch, velocity } => self.note_on(pitch, velocity.as_f32()),
            NoteEventKind::Off { pitch, velocity } => self.note_off(pitch, velocity.as_f32()),
            NoteEventKind::AllNotesOff => {
                self.held_count = 0;
                self.down = [false; 128];
                self.keys_down = 0;
                for voice in self.voices.iter_mut().filter(|v| v.held) {
                    voice.release(0.0);
                }
                for env in &mut self.mono_envs {
                    env.release();
                }
            }
            NoteEventKind::PitchBend(value) => {
                self.bend
                    .set_target((f32::from(value) / 8192.0).clamp(-1.0, 1.0));
            }
            NoteEventKind::Control { controller, value } => {
                let value = f32::from(value.min(127)) / 127.0;
                match controller {
                    MOD_WHEEL => self.mod_wheel.set_target(value),
                    SLIDE => self.slide.set_target(value),
                    PRESSURE => self.aftertouch.set_target(value),
                    _ => {}
                }
            }
        }
    }

    /// Control ticks a glide over `interval` semitones takes.
    fn glide_ticks(&self, interval: f32) -> f32 {
        let ms = if self.on(GLIDE_SCALED) {
            // The time is per octave.
            self.values[GLIDE] * interval.abs() / 12.0
        } else {
            self.values[GLIDE]
        };
        ms / 1000.0 * self.sample_rate / BLOCK as f32
    }

    fn is_mono(&self) -> bool {
        self.enum_value(VOICE_MODE) != 0
    }

    fn note_on(&mut self, pitch: Pitch, velocity: f32) {
        let random = [
            self.random() * 2.0 - 1.0,
            next_random(&mut self.seed_extra) * 2.0 - 1.0,
        ];
        let mut randoms = [[[0.0; MAX_UNISON]; OSC_COUNT]; 2];
        for phase in randoms[0][..2].iter_mut().flatten() {
            *phase = self.random();
        }
        for phase in &mut randoms[0][2] {
            *phase = next_random(&mut self.seed_c);
        }
        for detune in randoms[1].iter_mut().flatten() {
            *detune = next_random(&mut self.seed_extra);
        }
        // The controls as they are now: the last tick's, or the targets if idle.
        if !self.voices.iter().any(Voice::is_active) {
            self.settle();
        }
        let others_down = self.keys_down > 0;
        let key = usize::from(pitch.get());
        if !self.down[key] {
            self.down[key] = true;
            self.keys_down += 1;
        }
        // Instrument-wide modulators restart with every note.
        self.mono_age = 0.0;
        for (i, lfo) in self.global_lfos.iter_mut().enumerate() {
            let settings = &self.controls.lfo[i];
            if settings.mono && settings.mode != MODE_FREE {
                lfo.restart(settings);
            }
        }
        for (i, env) in self.mono_envs.iter_mut().enumerate() {
            if self.controls.env_mono[i] {
                let fs = self.sample_rate;
                env.start(
                    samples(self.controls.env_delay[i], fs),
                    samples(self.controls.env_hold[i], fs),
                );
            }
        }
        let note = pitch.get() as f32;
        let from_note = self.last_note.unwrap_or(note);
        let glide_ticks = if self.on(GLIDE_ALWAYS) || others_down {
            self.glide_ticks(note - from_note)
        } else {
            0.0
        };
        let gliding = glide_ticks >= 1.0;
        let curve = self.values[GLIDE_CURVE];
        self.notes_started += 1;
        let (index, from, retrigger) = if self.is_mono() {
            let legato = self.enum_value(VOICE_MODE) == 2;
            let others_held = self.held_count > 0;
            self.push_held(pitch);
            let voice = &self.voices[0];
            let active = voice.is_active();
            if legato && others_held && active && voice.held {
                // Legato: the sounding note slides over, envelopes carry on
                // (but those set to retrigger anyway).
                let fs = self.sample_rate;
                let retrigger: [bool; ENV_COUNT] = std::array::from_fn(|i| {
                    self.on(env_retrigger_param(i)) && !self.controls.env_mono[i]
                });
                let voice = &mut self.voices[0];
                voice.pitch = pitch;
                voice.glide_to(note, None, glide_ticks, curve);
                for (i, env) in voice.env.iter_mut().enumerate() {
                    if retrigger[i] {
                        env.start(
                            samples(self.controls.env_delay[i], fs),
                            samples(self.controls.env_hold[i], fs),
                        );
                    }
                }
                self.last_note = Some(note);
                return;
            }
            let from = if !gliding {
                None
            } else if active {
                Some(voice.note)
            } else {
                self.last_note
            };
            (0, from, true)
        } else {
            let polyphony = (self.enum_value(POLYPHONY) + 1).min(VOICES);
            let voices = &self.voices[..polyphony];
            let index = voices
                .iter()
                .position(|v| v.is_active() && v.pitch == pitch)
                .or_else(|| voices.iter().position(|v| !v.is_active()))
                .unwrap_or_else(|| self.voice_to_steal(polyphony));
            (index, self.last_note.filter(|_| gliding), true)
        };
        let was_active = self.voices[index].is_active();
        let voice = &mut self.voices[index];
        voice.started = self.notes_started;
        let start = NoteStart {
            pitch,
            velocity,
            random,
            from,
            glide_ticks,
            randoms: &randoms,
            retrigger,
        };
        voice.start(&start, &self.controls, &self.global_lfos);
        let tables = osc_tables(self.bank, self.user.as_deref(), &self.controls);
        voice.tick(
            &self.controls,
            &tables,
            &mut self.morph_scratch,
            false,
            !was_active,
            true,
        );
        self.last_note = Some(note);
    }

    fn note_off(&mut self, pitch: Pitch, lift: f32) {
        let key = usize::from(pitch.get());
        if self.down[key] {
            self.down[key] = false;
            self.keys_down -= 1;
        }
        if self.keys_down == 0 {
            for env in &mut self.mono_envs {
                env.release();
            }
        }
        if self.is_mono() {
            self.remove_held(pitch);
            let fallback = self.held_count.checked_sub(1).map(|i| self.held[i]);
            let curve = self.values[GLIDE_CURVE];
            let glide_ticks = fallback.map_or(0.0, |previous| {
                self.glide_ticks(previous.get() as f32 - pitch.get() as f32)
            });
            let voice = &mut self.voices[0];
            if voice.held && voice.pitch == pitch {
                match fallback {
                    // Back to the key still held, without a new attack.
                    Some(previous) => {
                        voice.pitch = previous;
                        voice.glide_to(previous.get() as f32, None, glide_ticks, curve);
                    }
                    None => voice.release(lift),
                }
            }
            for voice in self.voices[1..]
                .iter_mut()
                .filter(|v| v.held && v.pitch == pitch)
            {
                voice.release(lift);
            }
        } else {
            for voice in self
                .voices
                .iter_mut()
                .filter(|v| v.held && v.pitch == pitch)
            {
                voice.release(lift);
            }
        }
    }

    fn push_held(&mut self, pitch: Pitch) {
        self.remove_held(pitch);
        if self.held_count == HELD_NOTES {
            self.held.copy_within(1.., 0);
            self.held_count -= 1;
        }
        self.held[self.held_count] = pitch;
        self.held_count += 1;
    }

    fn remove_held(&mut self, pitch: Pitch) {
        let held = &self.held[..self.held_count];
        if let Some(i) = held.iter().position(|p| *p == pitch) {
            self.held.copy_within(i + 1..self.held_count, i);
            self.held_count -= 1;
        }
    }

    /// The quietest released voice, else the oldest, among the first `polyphony`.
    fn voice_to_steal(&self, polyphony: usize) -> usize {
        let voices = self.voices[..polyphony].iter().enumerate();
        super::voice_to_steal(voices, |v| !v.held, |v| v.env[0].level(), |v| v.started).unwrap_or(0)
    }

    /// The newest sounding voice, for the meters.
    fn newest_voice(&self) -> Option<&Voice> {
        self.voices
            .iter()
            .filter(|v| v.is_active())
            .max_by_key(|v| v.started)
    }

    /// Every source as exported and metered: the newest voice's, else the
    /// instrument-wide values and the per-voice ones it held on to.
    fn current_sources(&self) -> [f32; SOURCE_COUNT] {
        match self.newest_voice() {
            Some(voice) => voice.sources,
            None => {
                let mut sources = self.held_outputs;
                for (source, value) in sources.iter_mut().enumerate() {
                    if self.is_global(source) {
                        *value = self.globals[source];
                    }
                }
                // Free LFOs keep their clock.
                for (i, lfo) in self.global_lfos.iter().enumerate() {
                    let settings = &self.controls.lfo[i];
                    if !settings.mono && settings.mode == MODE_FREE {
                        sources[lfo_source(i)] = lfo.value;
                    }
                }
                sources
            }
        }
    }

    /// One cycle of the output at the newest voice's pitch, from the last
    /// rising zero crossing that leaves a whole cycle after it.
    fn scope_cycle(&self, out: &mut [f32]) {
        out.fill(0.0);
        let Some(voice) = self.newest_voice() else {
            return;
        };
        let hz = dsp::note_hz(voice.note);
        let period = (self.sample_rate / hz).clamp(8.0, (SCOPE_RING / 2) as f32);
        let at = |back: usize| self.scope[(self.scope_pos + SCOPE_RING - back) % SCOPE_RING];
        let first = period.ceil() as usize + 1;
        let start = (first..SCOPE_RING - 1)
            .find(|&back| at(back + 1) < 0.0 && at(back) >= 0.0)
            .unwrap_or(first);
        let n = out.len().max(1) as f32;
        for (i, x) in out.iter_mut().enumerate() {
            let place = start as f32 - period * i as f32 / n;
            let back = place.max(0.0) as usize;
            let t = place - back as f32;
            *x = at(back) + (at(back + 1) - at(back)) * t;
        }
    }
}

/// Whether source `source` is instrument-wide with controls `c`: none, a
/// macro, a wheel, aftertouch, slide, Fixed, or a mono LFO or envelope.
fn is_global_source(source: usize, c: &Controls) -> bool {
    if let Some(lfo) = source_lfo(source) {
        return c.lfo[lfo].mono;
    }
    if let Some(env) = source_env(source) {
        return c.env_mono[env];
    }
    source_macro(source).is_some()
        || matches!(
            source,
            SRC_NONE | SRC_MOD_WHEEL | SRC_PITCH_BEND | SRC_AFTERTOUCH | SRC_SLIDE | SRC_FIXED
        )
}

impl Default for Wavetable {
    fn default() -> Self {
        Self::new()
    }
}

/// The next of a xorshift sequence, `0..1`.
fn next_random(seed: &mut u32) -> f32 {
    dsp::noise::unit(dsp::xorshift32(seed))
}

impl Instrument for Wavetable {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.controls_stale = true;
        let envs: [AdsrDesc; ENV_COUNT] = std::array::from_fn(|e| self.env_desc(e));
        for voice in &mut self.voices {
            voice.prepare(sample_rate, envs);
        }
        for (env, desc) in self.mono_envs.iter_mut().zip(envs) {
            *env = Envelope::new(desc, sample_rate);
        }
        // Settings are ticked once per control interval, not per sample.
        let control_rate = sample_rate / BLOCK as f32;
        for smoothed in self.smoothers() {
            smoothed.prepare(control_rate);
        }
        self.fast = [false; PARAM_COUNT];
        self.gain.prepare(sample_rate);
        self.reset();
    }

    fn reset(&mut self) {
        for voice in &mut self.voices {
            voice.silence();
        }
        for env in &mut self.mono_envs {
            env.reset();
        }
        self.bend.set_immediate(0.0);
        self.settle();
        self.gain.reset();
        self.countdown = 0;
        self.seed = SEED;
        self.seed_c = SEED_C;
        self.seed_extra = SEED_EXTRA;
        self.notes_started = 0;
        self.held_count = 0;
        self.down = [false; 128];
        self.keys_down = 0;
        self.last_note = None;
        self.mono_age = 0.0;
        self.mono_rate_mods = [0.0; LFO_COUNT];
        self.held_outputs = [0.0; SOURCE_COUNT];
        self.scope.fill(0.0);
        self.level = 0.0;
        for (i, lfo) in self.global_lfos.iter_mut().enumerate() {
            *lfo = Lfo::new(SEED.rotate_left(i as u32 * 7));
            lfo.restart(&self.controls.lfo[i]);
        }
        self.run_globals(0.0);
    }

    fn process(&mut self, events: &[NoteEvent], left: &mut [f32], right: &mut [f32]) {
        left.fill(0.0);
        right.fill(0.0);
        let len = left.len().min(right.len());
        let (left, right) = (&mut left[..len], &mut right[..len]);
        let mut sounded = false;
        for (span, event) in crate::segments(events, len) {
            sounded |= self.render(&mut left[span.clone()], &mut right[span]);
            if let Some(kind) = event {
                self.handle(kind);
            }
        }
        if sounded {
            self.gain.process(left, right);
        } else {
            // Silence needs no scaling; finish any gain ramp instantly instead.
            self.gain.reset();
        }
        // The scope's ring and the level meter.
        for (l, r) in left.iter().zip(right.iter()) {
            let x = 0.5 * (l + r);
            self.scope[self.scope_pos] = x;
            self.scope_pos = (self.scope_pos + 1) % SCOPE_RING;
            self.level = (self.level * LEVEL_FALL).max(l.abs().max(r.abs()));
        }
        if let Some(voice) = self.newest_voice() {
            self.held_outputs = voice.sources;
        }
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        self.set(index, value, false);
    }

    /// Reaches `value` within one control tick (32 samples), not the
    /// knob's 20 ms, so a modulator moving it lands within the engine's
    /// block; the ramp per sample between ticks keeps it from stepping.
    fn modulate(&mut self, index: usize, value: f32) {
        self.set(index, value, true);
    }

    fn set_tempo(&mut self, bpm: f64) {
        if bpm.is_finite() && bpm > 0.0 {
            let bpm = (bpm as f32).clamp(1.0, 1000.0);
            if bpm != self.bpm {
                self.bpm = bpm;
                self.controls_stale = true;
                if (0..ENV_COUNT).any(|e| self.on(env_sync_param(e))) {
                    self.push_envelopes();
                }
            }
        }
    }

    /// See the `METER_*` constants for what each slot holds.
    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out.fill(0.0);
        let newest = self.newest_voice();
        out[METER_VOICES] = self.voices.iter().filter(|v| v.is_active()).count() as f32;
        let sources = self.current_sources();
        for i in 0..LFO_COUNT {
            let settings = &self.controls.lfo[i];
            let lfo = if settings.mono {
                Some(&self.global_lfos[i])
            } else {
                newest.map(|v| &v.lfo[i])
            };
            match lfo {
                Some(lfo) => {
                    out[METER_LFO_VALUE + i] = sources[lfo_source(i)];
                    out[METER_LFO_PHASE + i] = if is_chaos(settings.shape) {
                        0.5 + 0.5 * lfo.chaos_y(settings.shape)
                    } else {
                        lfo.read_phase(settings.mode)
                    };
                }
                None => {
                    // Still while nothing plays, at its start, so an idle
                    // panel stops redrawing (a free one keeps its clock for
                    // what it exports, see `mod_outputs`).
                    out[METER_LFO_VALUE + i] = 0.0;
                    out[METER_LFO_PHASE + i] = settings.start_phase;
                }
            }
        }
        for i in 0..ENV_COUNT {
            let env = if self.controls.env_mono[i] {
                Some(&self.mono_envs[i])
            } else {
                newest.map(|v| &v.env[i])
            };
            if let Some(env) = env {
                out[METER_ENV + i] = env.level();
                out[METER_ENV_STAGE + i] = env.playhead();
            }
        }
        out[METER_VELOCITY] = sources[SRC_VELOCITY];
        out[METER_NOTE] = sources[SRC_NOTE];
        out[METER_RANDOM] = sources[SRC_RANDOM];
        out[METER_RANDOM2] = sources[SRC_RANDOM2];
        out[METER_AFTERTOUCH] = self.aftertouch.current();
        out[METER_SLIDE] = self.slide.current();
        out[METER_LIFT] = sources[SRC_LIFT];
        out[METER_MOD_WHEEL] = self.mod_wheel.current();
        out[METER_BEND] = self.bend.current();
        match newest {
            Some(voice) => {
                out[METER_PITCH] = voice.note;
                for o in 0..OSC_COUNT {
                    out[METER_POSITION + o] = voice.osc[o].position();
                    out[METER_WARP + o] = voice.osc[o].warps()[0];
                }
            }
            None => {
                out[METER_PITCH] = self.last_note.unwrap_or(60.0);
                for o in 0..OSC_COUNT {
                    out[METER_POSITION + o] = self.smoothed(osc_param(o, OSC_POSITION));
                    out[METER_WARP + o] = self.smoothed(osc_param(o, OSC_WARP));
                }
            }
        }
        out[METER_LEVEL] = self.level;
        self.scope_cycle(&mut out[METER_SCOPE..METER_SCOPE + SCOPE_POINTS]);
    }

    /// See [`MOD_OUTPUTS`].
    fn mod_outputs(&self, out: &mut [f32]) {
        let sources = self.current_sources();
        for (value, (source, output)) in out.iter_mut().zip(OUTPUT_SOURCES.iter().zip(MOD_OUTPUTS))
        {
            let range = if output.bipolar {
                -1.0..=1.0
            } else {
                0.0..=1.0
            };
            *value = sources[*source].clamp(*range.start(), *range.end());
        }
    }

    /// Takes the oscillators' user tables: an `Arc<UserTables>` (anything
    /// else is ignored, leaving none). Never called on the audio thread, so
    /// the old ones are dropped here, off it.
    fn set_data(&mut self, data: InstrumentData) {
        self.user = data.downcast::<UserTables>().ok();
    }
}

impl Wavetable {
    /// [`Instrument::set_param`], or [`Instrument::modulate`] if `fast`.
    fn set(&mut self, index: usize, value: f32, fast: bool) {
        let Some(value) = crate::checked_value(PARAMS, index, value) else {
            return;
        };
        let param = &PARAMS[index];
        self.values[index] = value;
        self.controls_stale = true;
        match param.kind {
            ParameterKind::Float { .. } => {
                let target = if is_logarithmic(param) {
                    value.log2()
                } else {
                    value
                };
                self.glide_param(index, target, fast);
            }
            // On/off switches fade, so a section doesn't cut in or out with a click.
            ParameterKind::Enum { .. } if FADING_SWITCHES.contains(&index) => {
                self.glide_param(index, value, fast)
            }
            ParameterKind::Enum { .. } => self.smooth[index].set_immediate(value),
        }
        if index == MASTER_VOLUME {
            self.gain.set_db(value);
        }
        if let Some(o) = remap_osc(index) {
            self.remap_changed[o] = true;
        }
        if ENVELOPES.contains(&index) {
            let mut changed = (0..ENV_COUNT).any(|e| index == env_sync_param(e));
            for (i, layout) in ENVELOPE_LAYOUTS.iter().enumerate() {
                changed |= layout.apply(index, value, &mut self.envs[i]);
            }
            if changed {
                self.push_envelopes();
            }
        }
        if index == VOICE_MODE {
            self.held_count = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use midi::Velocity;

    use super::*;

    const RATE: f32 = 48_000.0;

    fn on(note: u8) -> NoteEvent {
        NoteEvent {
            frame: 0,
            kind: NoteEventKind::On {
                pitch: Pitch::new(note).unwrap(),
                velocity: Velocity::MAX,
            },
        }
    }

    fn run(synth: &mut Wavetable, events: &[NoteEvent], seconds: f32) {
        let (mut l, mut r) = ([0.0; 256], [0.0; 256]);
        for block in 0..(seconds * RATE / 256.0) as usize {
            synth.process(if block == 0 { events } else { &[] }, &mut l, &mut r);
        }
    }

    fn cutoff(synth: &Wavetable) -> f32 {
        synth.newest_voice().unwrap().cutoff[0]
    }

    #[test]
    fn the_mod_wheel_opens_the_default_filter() {
        let mut synth = Wavetable::new();
        synth.prepare(RATE, 256);
        run(&mut synth, &[on(48)], 0.6);
        let closed = cutoff(&synth);
        let wheel = NoteEvent {
            frame: 0,
            kind: NoteEventKind::Control {
                controller: MOD_WHEEL,
                value: 127,
            },
        };
        run(&mut synth, &[wheel], 0.6);
        let open = cutoff(&synth);
        println!("cutoff {closed} -> {open}");
        assert!((open / closed).log2() > 2.9, "{closed} -> {open}");
    }

    #[test]
    fn knobs_move_as_the_voices_apply_modulation() {
        // A full slot on the cutoff is ten octaves: half the 20 Hz..20 kHz knob.
        let cutoff = ring_unit(FILTER_CUTOFF);
        assert!((cutoff - 10.0 / 1000f32.log2()).abs() < 1e-4, "{cutoff}");
        assert_eq!(modulated_param(FILTER_CUTOFF, 1000.0, 1.0), 2000.0);
        // Positions move by the slot's amount; a semitone knob by 24 semitones.
        assert!((ring_unit(osc_param(0, OSC_POSITION)) - 1.0).abs() < 1e-6);
        assert!((ring_unit(osc_param(0, OSC_SEMI)) - 1.0).abs() < 1e-6);
        assert_eq!(modulated_param(osc_param(0, OSC_POSITION), 0.8, 0.5), 1.0);
        // Envelope times and the volume: a whole knob at amount 1.
        assert!((ring_unit(ENV1.attack) - 1.0).abs() < 1e-4);
        assert!((ring_unit(ENV2.decay) - 1.0).abs() < 1e-4);
        assert!((ring_unit(ENV3.release) - 1.0).abs() < 1e-4);
        assert!((ring_unit(MASTER_VOLUME) - 1.0).abs() < 1e-6);
        assert_eq!(modulated_param(ENV2.decay, 100.0, -1.0), 50.0);
        assert_eq!(modulated_param(ENV2.sustain, 0.5, 0.25), 0.75);
        assert_eq!(ring_unit(GLIDE), 0.0, "no knob destination");
    }

    #[test]
    fn the_mod_outputs_list_is_sound() {
        let mut ids: Vec<&str> = MOD_OUTPUTS.iter().map(|o| o.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), MOD_OUTPUTS.len());
        for (output, source) in MOD_OUTPUTS.iter().zip(OUTPUT_SOURCES) {
            assert_eq!(output.bipolar, is_bipolar(source), "{}", output.id);
        }
    }
}
