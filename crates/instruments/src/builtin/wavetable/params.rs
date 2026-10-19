//! The wavetable synth's parameters: where each one sits, which section of
//! the panel it belongs to, and what the mod matrix can read and drive.
//!
//! Projects store parameters by id, so an id keeps its meaning, range and
//! default once shipped; indices may move between versions, since the host
//! looks each id up when it loads a project. Choices are stored as label
//! positions, so label lists such as [`DESTINATIONS`] and [`TABLE_LABELS`]
//! only grow at the end. Sections are contiguous index ranges so the UI can
//! lay them out as panels without knowing every parameter by name.
//!
//! Everything added after the first release defaults to what the synth did
//! before it existed (keytracking on, linear unison detune, an empty slot,
//! a switched-off LFO feature...), so older projects, which only store the
//! values they changed, sound as they did.

use std::ops::Range;

use crate::ParamDescriptor;
use crate::envelope::{self, EnvelopeParams};

use super::tables::{FIRST_TABLES, TABLE_NAMES};
use crate::{choice, float};

// Offsets within an oscillator section (see `osc_param`).
pub const OSC_ON: usize = 0;
/// Enum (see [`TABLE_LABELS`]): a built-in table, or [`USER_TABLE`].
pub const OSC_TABLE: usize = 1;
/// Where in the table, `0..=1` from the first frame to the last.
pub const OSC_POSITION: usize = 2;
pub const OSC_WARP_MODE: usize = 3;
pub const OSC_WARP: usize = 4;
/// Enum: label index 4 is octave 0.
pub const OSC_OCTAVE: usize = 5;
/// Enum: label index 12 is semitone 0.
pub const OSC_SEMI: usize = 6;
pub const OSC_FINE: usize = 7;
/// Enum: label index `n` is `n + 1` voices.
pub const OSC_UNISON: usize = 8;
/// Cents between the note and the outermost unison voices.
pub const OSC_DETUNE: usize = 9;
pub const OSC_BLEND: usize = 10;
pub const OSC_WIDTH: usize = 11;
pub const OSC_PHASE: usize = 12;
pub const OSC_RANDOM: usize = 13;
pub const OSC_LEVEL: usize = 14;
pub const OSC_PAN: usize = 15;
/// The second warp slot, applied to what the first one made.
pub const OSC_WARP2_MODE: usize = 16;
pub const OSC_WARP2: usize = 17;
/// Enum: the intervals the unison voices are stacked over (see [`STACKS`]).
pub const OSC_STACK: usize = 18;
/// How far the unison voices' warp amounts fan out around the warp knobs,
/// `0..=1`: at 1 the outermost voices sit half the range below and above.
pub const OSC_WARP_SPREAD: usize = 19;
/// Enum (see [`MORPHS`]): a spectral morph, Vital's, which reshapes the
/// frame's harmonics before it plays.
pub const OSC_MORPH_MODE: usize = 20;
/// How far the spectral morph goes, `0..=1`.
pub const OSC_MORPH: usize = 21;
/// Enum (see [`DETUNE_MODES`]): how the unison voices spread over the
/// detune, Serum's unison tuning modes.
pub const OSC_DETUNE_MODE: usize = 22;
/// How far the unison voices' table positions fan out around the position
/// knob, `0..=1`, as warp spread does for the warps (Serum's unison WT Pos).
pub const OSC_POSITION_SPREAD: usize = 23;
/// Enum (see [`MOD_SOURCES`]): what the cross-modulation warps (FM, AM,
/// RM) listen to. "Auto" is the classic pairing of [`MODULATORS`].
pub const OSC_MOD_SOURCE: usize = 24;
/// Off/On: the oscillator follows the note. Off, it plays C4 (moved by its
/// octave, semitone and fine controls, and the pitch wheel) whatever the key.
pub const OSC_KEYTRACK: usize = 25;
/// Enum (see [`REMAP_NODE_COUNTS`]): how many of the remap curve's nodes
/// the Remap warps use, from 2. The curve maps where in the cycle the
/// oscillator is to where it reads the wave (Serum's Remap): node `i`'s
/// place is `OSC_REMAP_X + i`, `0..=1`, its value `OSC_REMAP_Y + i`, and
/// the bend of the segment after it `OSC_REMAP_CURVE + i`, as an LFO's
/// Points shape is drawn.
pub const OSC_REMAP_NODES: usize = 26;
pub const OSC_REMAP_X: usize = OSC_REMAP_NODES + 1;
pub const OSC_REMAP_Y: usize = OSC_REMAP_X + MAX_REMAP_NODES;
pub const OSC_REMAP_CURVE: usize = OSC_REMAP_Y + MAX_REMAP_NODES;
pub const OSC_PARAMS: usize = OSC_REMAP_CURVE + MAX_REMAP_NODES;
/// Most nodes a remap curve has.
pub const MAX_REMAP_NODES: usize = 8;

/// The wavetable oscillators: A, B and C, in that order everywhere a list
/// is kept per oscillator (sections, routes, meters, user tables).
pub const OSC_COUNT: usize = 3;
pub const OSC_A: Range<usize> = 0..OSC_PARAMS;
pub const OSC_B: Range<usize> = OSC_A.end..OSC_A.end + OSC_PARAMS;
/// Osc C, a full peer of A and B (ids `osc_c_*`). It starts switched off,
/// so the default patch sounds as it did with two oscillators.
pub const OSC_C: Range<usize> = OSC_B.end..OSC_B.end + OSC_PARAMS;

pub const SUB: Range<usize> = OSC_C.end..OSC_C.end + 4;
pub const SUB_ON: usize = SUB.start;
pub const SUB_SHAPE: usize = SUB.start + 1;
/// Enum: label index 4 is octave 0.
pub const SUB_OCTAVE: usize = SUB.start + 2;
pub const SUB_LEVEL: usize = SUB.start + 3;

pub const NOISE: Range<usize> = SUB.end..SUB.end + 3;
pub const NOISE_ON: usize = NOISE.start;
pub const NOISE_TYPE: usize = NOISE.start + 1;
pub const NOISE_LEVEL: usize = NOISE.start + 2;

pub const FILTER: Range<usize> = NOISE.end..NOISE.end + 13;
pub const FILTER_ON: usize = FILTER.start;
pub const FILTER_TYPE: usize = FILTER.start + 1;
pub const FILTER_CUTOFF: usize = FILTER.start + 2;
pub const FILTER_RESONANCE: usize = FILTER.start + 3;
pub const FILTER_DRIVE: usize = FILTER.start + 4;
pub const FILTER_KEYTRACK: usize = FILTER.start + 5;
pub const FILTER_MIX: usize = FILTER.start + 6;
/// `0..=1`, what Serum calls Var and Vital Blend: the gain of the peak and
/// shelves (0.5 is flat), where Morph sits between low-, band- and
/// high-pass, the phaser's notch spread, the flanger's polarity. The other
/// types leave it alone.
pub const FILTER_BLEND: usize = FILTER.start + 7;
/// Routing switches: which sources go into the filters. The rest bypass
/// both. Which filter each one goes into is its `FILTER_TARGET_*`.
pub const FILTER_OSC_A: usize = FILTER.start + 8;
pub const FILTER_OSC_B: usize = FILTER.start + 9;
pub const FILTER_SUB: usize = FILTER.start + 10;
pub const FILTER_NOISE: usize = FILTER.start + 11;
pub const FILTER_OSC_C: usize = FILTER.start + 12;

/// Filter 2: the same controls as filter 1, in the same order (see
/// [`filter_param`]), then how the two are connected and where each source
/// goes in.
pub const FILTER2: Range<usize> = FILTER.end..FILTER.end + 14;
pub const FILTER2_ON: usize = FILTER2.start;
pub const FILTER2_TYPE: usize = FILTER2.start + 1;
pub const FILTER2_CUTOFF: usize = FILTER2.start + 2;
pub const FILTER2_RESONANCE: usize = FILTER2.start + 3;
pub const FILTER2_DRIVE: usize = FILTER2.start + 4;
pub const FILTER2_KEYTRACK: usize = FILTER2.start + 5;
pub const FILTER2_MIX: usize = FILTER2.start + 6;
pub const FILTER2_BLEND: usize = FILTER2.start + 7;
/// Enum (see [`FILTER_ROUTINGS`]): serial, filter 1 feeding filter 2, or
/// parallel, each filtering its own sources and the two summed. A filter
/// that is off passes its input through either way.
pub const FILTER_ROUTING: usize = FILTER2.start + 8;
/// Enum (see [`FILTER_TARGETS`]): which filter a source switched into the
/// filters goes into. In serial, sound going into filter 1 then goes
/// through filter 2 as well.
pub const FILTER_TARGET_OSC_A: usize = FILTER2.start + 9;
pub const FILTER_TARGET_OSC_B: usize = FILTER2.start + 10;
pub const FILTER_TARGET_SUB: usize = FILTER2.start + 11;
pub const FILTER_TARGET_NOISE: usize = FILTER2.start + 12;
pub const FILTER_TARGET_OSC_C: usize = FILTER2.start + 13;

/// Each source's routing switch and filter choice: osc A, osc B, sub,
/// noise and osc C (added last, so the first four keep their places; see
/// [`OSC_ROUTES`], [`SUB_ROUTE`] and [`NOISE_ROUTE`]).
pub const SOURCE_ROUTES: [(usize, usize); 5] = [
    (FILTER_OSC_A, FILTER_TARGET_OSC_A),
    (FILTER_OSC_B, FILTER_TARGET_OSC_B),
    (FILTER_SUB, FILTER_TARGET_SUB),
    (FILTER_NOISE, FILTER_TARGET_NOISE),
    (FILTER_OSC_C, FILTER_TARGET_OSC_C),
];
/// Where each oscillator's route sits in [`SOURCE_ROUTES`].
pub const OSC_ROUTES: [usize; OSC_COUNT] = [0, 1, 4];
pub const SUB_ROUTE: usize = 2;
pub const NOISE_ROUTE: usize = 3;

/// Envelopes: the amp envelope and five more for the mod matrix, as many
/// as Vital has (Serum 2 has four).
pub const ENV_COUNT: usize = 6;
/// One envelope's parameters: the seven [`EnvelopeParams`] knows, then its
/// delay and hold. Envelopes 2 to 6 also have a mono switch, kept together
/// after all six (see [`env_mono_param`]), and then every envelope's tempo
/// sync and legato retrigger switches (see [`env_sync_param`]).
const ENV_PARAMS: usize = 9;
const ENV_SWITCHES: usize = ENV_COUNT * ENV_PARAMS + ENV_COUNT - 1;
pub const ENVELOPES: Range<usize> = FILTER2.end..FILTER2.end + ENV_SWITCHES + 2 * ENV_COUNT;
/// Env 1 is the amplitude envelope; 2 to 6 are free for the mod matrix.
pub const ENV1: EnvelopeParams = EnvelopeParams::starting_at("Amp", ENVELOPES.start);
pub const ENV2: EnvelopeParams = EnvelopeParams::starting_at("Env 2", ENVELOPES.start + ENV_PARAMS);
pub const ENV3: EnvelopeParams =
    EnvelopeParams::starting_at("Env 3", ENVELOPES.start + 2 * ENV_PARAMS);
pub const ENV4: EnvelopeParams =
    EnvelopeParams::starting_at("Env 4", ENVELOPES.start + 3 * ENV_PARAMS);
pub const ENV5: EnvelopeParams =
    EnvelopeParams::starting_at("Env 5", ENVELOPES.start + 4 * ENV_PARAMS);
pub const ENV6: EnvelopeParams =
    EnvelopeParams::starting_at("Env 6", ENVELOPES.start + 5 * ENV_PARAMS);
pub const ENVELOPE_LAYOUTS: [EnvelopeParams; ENV_COUNT] = [ENV1, ENV2, ENV3, ENV4, ENV5, ENV6];
// Offsets of the stages `EnvelopeParams` has no room for (see `env_param`).
/// Milliseconds from the note to the start of the attack.
pub const ENV_DELAY: usize = 0;
/// Milliseconds at full level between the attack and the decay.
pub const ENV_HOLD: usize = 1;

pub const LFO_COUNT: usize = 8;
pub const LFOS: Range<usize> = ENVELOPES.end..ENVELOPES.end + LFO_COUNT * LFO_PARAMS;
// Offsets within one LFO's parameters (see `lfo_param`).
pub const LFO_SHAPE: usize = 0;
/// Free, retrigger per note, one-shot envelope, looping or sustaining
/// envelope (see [`LFO_MODES`]).
pub const LFO_MODE: usize = 1;
/// Off: `LFO_RATE` in Hz. On: `LFO_DIVISION` of the song's tempo.
pub const LFO_SYNC: usize = 2;
pub const LFO_RATE: usize = 3;
pub const LFO_DIVISION: usize = 4;
pub const LFO_PHASE: usize = 5;
pub const LFO_SMOOTH: usize = 6;
/// Seconds for the LFO to fade in after each note starts (delayed vibrato).
pub const LFO_RISE: usize = 7;
/// How the custom shape gets from point to point, `0..=1`: 0 holds each
/// point (steps), more glides to the next, 1 is one smooth curve.
pub const LFO_GLIDE: usize = 8;
/// The custom shape's first point; point `i` is `LFO_POINT + i`, `0..=1`
/// from the bottom (-1) to the top (+1), evenly spread over the cycle.
pub const LFO_POINT: usize = 9;
pub const LFO_POINTS: usize = 32;
/// Seconds after each note before the LFO starts (and its rise begins).
pub const LFO_DELAY: usize = LFO_POINT + LFO_POINTS;
/// Off/On: one LFO for the whole instrument rather than one per voice, as
/// Serum's Mono switch. Every voice hears the same value, and the LFO's
/// value is the instrument's, for whatever else wants it.
pub const LFO_MONO: usize = LFO_DELAY + 1;
/// `0..=1`: where the Loop mode loops back to and the Sustain mode holds,
/// as a share of the cycle.
pub const LFO_LOOP: usize = LFO_DELAY + 2;
/// Enum (see [`NODE_COUNTS`]): how many of the drawn nodes the Points
/// shape uses, from 2.
pub const LFO_NODES: usize = LFO_DELAY + 3;
/// Node `i`'s place in the cycle is `LFO_NODE_X + i`, `0..=1`; its value
/// `LFO_NODE_Y + i`, `0..=1` from -1 to +1; and the bend of the segment
/// after it `LFO_NODE_CURVE + i`, `-1..=1` as in envelope stages. The
/// first node sits at 0 and the last one used at 1, whatever theirs say.
pub const LFO_NODE_X: usize = LFO_NODES + 1;
pub const LFO_NODE_Y: usize = LFO_NODE_X + MAX_NODES;
pub const LFO_NODE_CURVE: usize = LFO_NODE_Y + MAX_NODES;
pub const LFO_PARAMS: usize = LFO_NODE_CURVE + MAX_NODES;
/// Most nodes a Points shape has.
pub const MAX_NODES: usize = 16;

pub const MOD_SLOTS: usize = 64;
pub const MATRIX: Range<usize> = LFOS.end..LFOS.end + MOD_SLOTS * SLOT_PARAMS;
// Offsets within one mod slot's parameters (see `slot_param`).
pub const SLOT_SOURCE: usize = 0;
pub const SLOT_DEST: usize = 1;
/// `-1..=1`; at 1 a full-scale source moves the destination by its
/// [`DESTINATION_SCALES`] entry.
pub const SLOT_AMOUNT: usize = 2;
/// A second source that scales the slot (None leaves it alone), e.g. the
/// mod wheel setting how much vibrato an LFO adds.
pub const SLOT_AUX: usize = 3;
/// `-1..=1`: bends the source's response like an envelope stage's curve
/// (see [`matrix_curve`]). Negative rises fast then levels off, positive
/// starts slow.
pub const SLOT_CURVE: usize = 4;
/// Enum (see [`POLARITIES`]): the source's own range, or remapped to
/// `0..=1` or `-1..=1`.
pub const SLOT_POLARITY: usize = 5;
/// Off/On: the slot is set aside, doing nothing until switched back.
pub const SLOT_BYPASS: usize = 6;
pub const SLOT_PARAMS: usize = 7;

pub const MACRO_COUNT: usize = 8;
pub const MACROS: Range<usize> = MATRIX.end..MATRIX.end + MACRO_COUNT;

pub const VOICE: Range<usize> = MACROS.end..MACROS.end + 9;
pub const VOICE_MODE: usize = VOICE.start;
/// Enum: label index `n` is `n + 1` voices.
pub const POLYPHONY: usize = VOICE.start + 1;
pub const GLIDE: usize = VOICE.start + 2;
pub const BEND_RANGE: usize = VOICE.start + 3;
/// How much velocity scales the amplitude: 0 ignores it, 1 is fully velocity-driven.
pub const VELOCITY_SENS: usize = VOICE.start + 4;
/// Off/On: glide on every note (Serum's "Always"). Off, only a note played
/// while another is held glides.
pub const GLIDE_ALWAYS: usize = VOICE.start + 5;
/// `-1..=1`: the glide's shape. 0 is a straight line in semitones,
/// negative leaves fast and settles slowly (Serum's convex default),
/// positive leaves slowly.
pub const GLIDE_CURVE: usize = VOICE.start + 6;
/// Off/On: the glide time is per octave, so short intervals glide faster
/// (Serum's "Scaled").
pub const GLIDE_SCALED: usize = VOICE.start + 7;
/// Enum (see [`QUALITIES`]): High renders the oscillators at twice the
/// rate, which tames the aliasing that hard-edged warps (sync, PWM, flip,
/// quantize, fold, drive) and deep FM add, for about twice their cost.
pub const QUALITY: usize = VOICE.start + 8;

pub const MASTER: Range<usize> = VOICE.end..VOICE.end + 1;
pub const MASTER_VOLUME: usize = MASTER.start;

pub const PARAM_COUNT: usize = MASTER.end;

/// Every section, in panel order, with the parameter indices it owns.
pub const SECTIONS: [(&str, Range<usize>); 13] = [
    ("Osc A", OSC_A),
    ("Osc B", OSC_B),
    ("Osc C", OSC_C),
    ("Sub", SUB),
    ("Noise", NOISE),
    ("Filter", FILTER),
    ("Filter 2", FILTER2),
    ("Envelopes", ENVELOPES),
    ("LFOs", LFOS),
    ("Matrix", MATRIX),
    ("Macros", MACROS),
    ("Voice", VOICE),
    ("Master", MASTER),
];

/// Index of parameter `offset` (e.g. [`OSC_POSITION`]) of oscillator `osc`
/// (0 = A, 1 = B, 2 = C).
pub const fn osc_param(osc: usize, offset: usize) -> usize {
    OSC_A.start + osc * OSC_PARAMS + offset
}

/// Filter `filter`'s (0 or 1) counterpart of filter 1's parameter `index`,
/// one of [`FILTER_ON`] to [`FILTER_BLEND`].
pub const fn filter_param(filter: usize, index: usize) -> usize {
    if filter == 0 {
        index
    } else {
        index - FILTER_ON + FILTER2_ON
    }
}

/// Index of envelope `env`'s (0-based) [`ENV_DELAY`] or [`ENV_HOLD`].
pub const fn env_param(env: usize, offset: usize) -> usize {
    ENVELOPES.start + env * ENV_PARAMS + 7 + offset
}

/// Index of envelope `env`'s (1 to 5: env 2 to 6) mono switch: one
/// envelope for the whole instrument, started by every note and released
/// with the last key, rather than one per voice. The amp envelope has none.
pub const fn env_mono_param(env: usize) -> usize {
    ENVELOPES.start + ENV_COUNT * ENV_PARAMS + env - 1
}

/// Index of envelope `env`'s (0-based) tempo sync switch: on, its times
/// are as set at 120 BPM, stretching and shrinking with the song's tempo
/// (Serum 2's BPM envelopes).
pub const fn env_sync_param(env: usize) -> usize {
    ENVELOPES.start + ENV_SWITCHES + env
}

/// Index of envelope `env`'s (0-based) legato retrigger switch: on, it
/// starts again on every note even when legato slides the voice over
/// (Serum 2's Invert Legato).
pub const fn env_retrigger_param(env: usize) -> usize {
    ENVELOPES.start + ENV_SWITCHES + ENV_COUNT + env
}

/// Index of parameter `offset` (e.g. [`LFO_RATE`]) of LFO `lfo` (0-based).
pub const fn lfo_param(lfo: usize, offset: usize) -> usize {
    LFOS.start + lfo * LFO_PARAMS + offset
}

/// Index of parameter `offset` (e.g. [`SLOT_AMOUNT`]) of mod slot `slot` (0-based).
pub const fn slot_param(slot: usize, offset: usize) -> usize {
    MATRIX.start + slot * SLOT_PARAMS + offset
}

/// Index of macro `n` (0-based).
pub const fn macro_param(n: usize) -> usize {
    MACROS.start + n
}

pub use crate::OFF_ON;

/// The table menus' choices: the first built-in tables, [`USER_TABLE`],
/// then the built-in tables added since (see [`table_of`]).
pub const TABLE_LABELS: [&str; TABLE_NAMES.len() + 1] = {
    let mut labels = ["User"; TABLE_NAMES.len() + 1];
    let mut i = 0;
    while i < TABLE_NAMES.len() {
        labels[choice_of(i)] = TABLE_NAMES[i];
        i += 1;
    }
    labels
};
/// The table choice "User": the oscillator's own table, loaded from a WAV
/// file (see `UserTables`). With none loaded the oscillator plays the
/// first built-in table instead, so it never falls silent. It sits after
/// the tables there were when it was added, where projects store it.
pub const USER_TABLE: usize = FIRST_TABLES;

/// The built-in table (its place in [`TABLE_NAMES`]) a table choice
/// plays, or `None` for [`USER_TABLE`].
pub const fn table_of(choice: usize) -> Option<usize> {
    if choice < USER_TABLE {
        Some(choice)
    } else if choice == USER_TABLE {
        None
    } else {
        Some(choice - 1)
    }
}

/// The table choice that plays built-in table `table`: [`table_of`]'s inverse.
pub const fn choice_of(table: usize) -> usize {
    if table < USER_TABLE { table } else { table + 1 }
}

/// Warp modes, the same for every oscillator except the cross-modulation
/// ones, which name the oscillator listened to by default (see
/// [`MODULATORS`] and [`OSC_MOD_SOURCE`]). The `WARP_*` constants say what
/// each does. Both warp slots offer them all.
pub const WARPS_A: &[&str] = &[
    "Off",
    "Sync",
    "Sync Win",
    "Bend+",
    "Bend-",
    "PWM",
    "Asym+",
    "Asym-",
    "Mirror",
    "Flip",
    "Quantize",
    "FM from B",
    "AM from B",
    "RM from B",
    "Bend ±",
    "Asym ±",
    "Fold",
    "Drive",
    "Squeeze",
    "Remap",
    "Remap Mirror",
    "Remap ×4",
];
pub const WARPS_B: &[&str] = &[
    "Off",
    "Sync",
    "Sync Win",
    "Bend+",
    "Bend-",
    "PWM",
    "Asym+",
    "Asym-",
    "Mirror",
    "Flip",
    "Quantize",
    "FM from A",
    "AM from A",
    "RM from A",
    "Bend ±",
    "Asym ±",
    "Fold",
    "Drive",
    "Squeeze",
    "Remap",
    "Remap Mirror",
    "Remap ×4",
];
/// Osc C listens to A, as B does.
pub const WARPS_C: &[&str] = WARPS_B;
/// Which oscillator each one's cross-modulation warps (FM, AM, RM) listen
/// to when its [`OSC_MOD_SOURCE`] is "Auto": A to B, B to A, C to A. When
/// two listen to each other, the first in A, B, C order hears the other
/// and the other's cross-modulation is off.
pub const MODULATORS: [usize; OSC_COUNT] = [1, 0, 0];
/// What a cross-modulation warp can listen to.
pub const MOD_SOURCES: &[&str] = &["Auto", "Osc A", "Osc B", "Osc C", "Sub", "Noise"];
pub const MOD_SOURCE_AUTO: usize = 0;
/// The first oscillator; the others follow in order.
pub const MOD_SOURCE_OSC: usize = 1;
pub const MOD_SOURCE_SUB: usize = 4;
pub const MOD_SOURCE_NOISE: usize = 5;

/// Vital's spectral morphs (see `morph`): each works on the frame's
/// harmonics, so it stays band-limited however far it goes.
pub const MORPHS: &[&str] = &[
    "Off",
    "Vocode",
    "Formant Scale",
    "Harmonic Stretch",
    "Inharmonic Stretch",
    "Smear",
    "Random Amplitudes",
    "Low Pass",
    "High Pass",
    "Phase Disperse",
    "Shepard",
    "Skew",
];
/// Serum's unison tuning modes: how the voices spread over the detune.
pub const DETUNE_MODES: &[&str] = &["Linear", "Super", "Exp", "Inverse", "Random"];
pub const DETUNE_LINEAR: usize = 0;
pub const DETUNE_SUPER: usize = 1;
pub const DETUNE_EXP: usize = 2;
pub const DETUNE_INVERSE: usize = 3;
/// A fresh random spread for every note.
pub const DETUNE_RANDOM: usize = 4;

pub const OCTAVES: &[&str] = &["-4", "-3", "-2", "-1", "0", "+1", "+2", "+3", "+4"];
pub const SEMITONES: &[&str] = &[
    "-12", "-11", "-10", "-9", "-8", "-7", "-6", "-5", "-4", "-3", "-2", "-1", "0", "+1", "+2",
    "+3", "+4", "+5", "+6", "+7", "+8", "+9", "+10", "+11", "+12",
];
pub const UNISON: &[&str] = &[
    "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
];
pub const VOICE_COUNTS: &[&str] = &[
    "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16", "17",
    "18", "19", "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "30", "31", "32",
];
/// Unison stacks, named by the intervals (in semitones) added to the note.
pub const STACKS: &[&str] = &[
    "Off", "+12", "+12 +24", "+7", "+7 +12", "+12 +19", "-12", "-12 +12",
];
/// The intervals of each of [`STACKS`], in semitones, the note itself
/// first. Unison voices take them in turn across the stack, lined up so
/// the center voice plays the note (see `voice::stack_interval`).
pub const STACK_INTERVALS: [&[f32]; 8] = [
    &[0.0],
    &[0.0, 12.0],
    &[0.0, 12.0, 24.0],
    &[0.0, 7.0],
    &[0.0, 7.0, 12.0],
    &[0.0, 12.0, 19.0],
    &[0.0, -12.0],
    &[0.0, -12.0, 12.0],
];

pub const SUB_SHAPES: &[&str] = &["Sine", "Triangle", "Saw", "Square"];
pub const NOISE_TYPES: &[&str] = &["White", "Pink", "Brown", "Blue"];
/// Filter types; the `filter::*` constants say what each is.
pub const FILTER_TYPES: &[&str] = &[
    "LP 12",
    "LP 24",
    "HP 12",
    "HP 24",
    "BP 12",
    "Notch",
    "Comb +",
    "Comb -",
    "Formant",
    "Ladder",
    "BP 24",
    "Peak",
    "Low Shelf",
    "High Shelf",
    "Morph",
    "Acid",
    "Dirty",
    "Phaser",
    "Flanger",
];
pub const FILTER_ROUTINGS: &[&str] = &["Serial", "Parallel"];
pub const FILTER_TARGETS: &[&str] = &["Filter 1", "Filter 2"];
pub const LFO_SHAPES: &[&str] = &[
    "Sine",
    "Triangle",
    "Saw Up",
    "Saw Down",
    "Square",
    "S&H",
    "Smooth Random",
    "Custom",
    "Points",
    "Lorenz",
    "Rossler",
];
pub const LFO_MODES: &[&str] = &["Free", "Retrig", "Env", "Loop", "Sustain"];
/// How many nodes a remap curve uses: label index `n` is `n + 2`.
pub const REMAP_NODE_COUNTS: &[&str] = &["2", "3", "4", "5", "6", "7", "8"];
/// How many nodes a Points shape uses: label index `n` is `n + 2`.
pub const NODE_COUNTS: &[&str] = &[
    "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
];
/// Tempo-synced LFO cycle lengths, as fractions of a 4/4 bar. D is dotted
/// (1.5 times as long), T is a triplet (two thirds).
pub const DIVISIONS: &[&str] = &[
    "8/1", "4/1", "2/1", "1/1", "1/2D", "1/2", "1/2T", "1/4D", "1/4", "1/4T", "1/8D", "1/8",
    "1/8T", "1/16D", "1/16", "1/16T", "1/32", "1/64",
];
/// Beats (quarter notes) per LFO cycle for each of [`DIVISIONS`].
pub const DIVISION_BEATS: [f32; 18] = [
    32.0,
    16.0,
    8.0,
    4.0,
    3.0,
    2.0,
    4.0 / 3.0,
    1.5,
    1.0,
    2.0 / 3.0,
    0.75,
    0.5,
    1.0 / 3.0,
    0.375,
    0.25,
    1.0 / 6.0,
    0.125,
    0.0625,
];
pub const VOICE_MODES: &[&str] = &["Poly", "Mono", "Legato"];
pub const QUALITIES: &[&str] = &["Normal", "High"];

/// What a mod slot can read. Envelopes, velocity, the wheels' mod wheel,
/// macros, aftertouch, slide, lift and Fixed are `0..=1`; LFOs, note,
/// pitch bend and the randoms are `-1..=1` (see [`is_bipolar`]). New ones
/// go at the end, since projects store a slot's source by its place here;
/// the UI groups them (see [`SOURCE_GROUPS`]).
pub const SOURCES: &[&str] = &[
    "None",
    "Env 1",
    "Env 2",
    "Env 3",
    "LFO 1",
    "LFO 2",
    "LFO 3",
    "LFO 4",
    "Velocity",
    "Note",
    "Mod Wheel",
    "Pitch Bend",
    "Macro 1",
    "Macro 2",
    "Macro 3",
    "Macro 4",
    "Random",
    "LFO 5",
    "LFO 6",
    "LFO 7",
    "LFO 8",
    "Env 4",
    "Env 5",
    "Env 6",
    "Macro 5",
    "Macro 6",
    "Macro 7",
    "Macro 8",
    "Random 2",
    "Aftertouch",
    "Slide",
    "Lift",
    "Fixed",
];
pub const SRC_NONE: usize = 0;
pub const SRC_ENV1: usize = 1;
pub const SRC_LFO1: usize = 4;
pub const SRC_VELOCITY: usize = 8;
pub const SRC_NOTE: usize = 9;
pub const SRC_MOD_WHEEL: usize = 10;
pub const SRC_PITCH_BEND: usize = 11;
pub const SRC_MACRO1: usize = 12;
/// A random value fixed per note, `-1..=1` (Serum's Note-On Random 1).
pub const SRC_RANDOM: usize = 16;
pub const SRC_LFO5: usize = 17;
pub const SRC_ENV4: usize = 21;
pub const SRC_MACRO5: usize = 24;
/// A second random value per note, unrelated to the first.
pub const SRC_RANDOM2: usize = 28;
/// Channel pressure, `0..=1`.
pub const SRC_AFTERTOUCH: usize = 29;
/// MPE's slide, the vertical move on the key: CC 74, `0..=1`.
pub const SRC_SLIDE: usize = 30;
/// The note's release velocity, `0..=1` (Vital's Lift): 0 until the key
/// is let go.
pub const SRC_LIFT: usize = 31;
/// Always 1, so a slot's amount offsets its destination (scaled by the aux
/// source, if any).
pub const SRC_FIXED: usize = 32;
pub const SOURCE_COUNT: usize = 33;

/// LFO `i`'s (0-based) source.
pub const fn lfo_source(i: usize) -> usize {
    if i < 4 {
        SRC_LFO1 + i
    } else {
        SRC_LFO5 + i - 4
    }
}

/// Envelope `i`'s (0-based) source.
pub const fn env_source(i: usize) -> usize {
    if i < 3 {
        SRC_ENV1 + i
    } else {
        SRC_ENV4 + i - 3
    }
}

/// Macro `i`'s (0-based) source.
pub const fn macro_source(i: usize) -> usize {
    if i < 4 {
        SRC_MACRO1 + i
    } else {
        SRC_MACRO5 + i - 4
    }
}

/// The LFO a source is, if it is one.
pub const fn source_lfo(source: usize) -> Option<usize> {
    if source >= SRC_LFO1 && source < SRC_LFO1 + 4 {
        Some(source - SRC_LFO1)
    } else if source >= SRC_LFO5 && source < SRC_LFO5 + 4 {
        Some(source - SRC_LFO5 + 4)
    } else {
        None
    }
}

/// The envelope a source is, if it is one.
pub const fn source_env(source: usize) -> Option<usize> {
    if source >= SRC_ENV1 && source < SRC_ENV1 + 3 {
        Some(source - SRC_ENV1)
    } else if source >= SRC_ENV4 && source < SRC_ENV4 + 3 {
        Some(source - SRC_ENV4 + 3)
    } else {
        None
    }
}

/// The macro a source is, if it is one.
pub const fn source_macro(source: usize) -> Option<usize> {
    if source >= SRC_MACRO1 && source < SRC_MACRO1 + 4 {
        Some(source - SRC_MACRO1)
    } else if source >= SRC_MACRO5 && source < SRC_MACRO5 + 4 {
        Some(source - SRC_MACRO5 + 4)
    } else {
        None
    }
}

/// The sources as the UI groups them in menus and on its drag tiles: every
/// source but None once, in order within each group.
pub const SOURCE_GROUPS: [(&str, &[usize]); 5] = [
    (
        "Envelopes",
        &[
            env_source(0),
            env_source(1),
            env_source(2),
            env_source(3),
            env_source(4),
            env_source(5),
        ],
    ),
    (
        "LFOs",
        &[
            lfo_source(0),
            lfo_source(1),
            lfo_source(2),
            lfo_source(3),
            lfo_source(4),
            lfo_source(5),
            lfo_source(6),
            lfo_source(7),
        ],
    ),
    (
        "Macros",
        &[
            macro_source(0),
            macro_source(1),
            macro_source(2),
            macro_source(3),
            macro_source(4),
            macro_source(5),
            macro_source(6),
            macro_source(7),
        ],
    ),
    (
        "Note",
        &[SRC_VELOCITY, SRC_NOTE, SRC_LIFT, SRC_RANDOM, SRC_RANDOM2],
    ),
    (
        "Controllers",
        &[
            SRC_MOD_WHEEL,
            SRC_PITCH_BEND,
            SRC_AFTERTOUCH,
            SRC_SLIDE,
            SRC_FIXED,
        ],
    ),
];

/// Whether source `source` swings both ways (`-1..=1`) rather than `0..=1`.
pub const fn is_bipolar(source: usize) -> bool {
    source_lfo(source).is_some()
        || matches!(source, SRC_NOTE | SRC_PITCH_BEND | SRC_RANDOM | SRC_RANDOM2)
}

/// A mod slot's polarity: the source as it comes, or remapped.
pub const POLARITIES: &[&str] = &["Source", "Unipolar", "Bipolar"];
pub const POLARITY_SOURCE: usize = 0;
/// `0..=1`: a bipolar source's -1 becomes 0 and its 0 becomes one half.
pub const POLARITY_UNIPOLAR: usize = 1;
/// `-1..=1`: a unipolar source's 0 becomes -1 and its one half becomes 0.
pub const POLARITY_BIPOLAR: usize = 2;

/// What a mod slot can drive. New destinations go at the end, since
/// projects store a slot's destination by its place in this list.
pub const DESTINATIONS: &[&str] = &[
    "None",
    "A Position",
    "A Warp",
    "A Level",
    "A Pan",
    "A Pitch",
    "A Fine",
    "A Detune",
    "A Blend",
    "B Position",
    "B Warp",
    "B Level",
    "B Pan",
    "B Pitch",
    "B Fine",
    "B Detune",
    "B Blend",
    "Sub Level",
    "Noise Level",
    "Pitch",
    "Cutoff",
    "Resonance",
    "Drive",
    "Filter Mix",
    "LFO 1 Rate",
    "LFO 2 Rate",
    "LFO 3 Rate",
    "LFO 4 Rate",
    "Amp",
    "A Warp 2",
    "B Warp 2",
    "Cutoff 2",
    "Resonance 2",
    "Drive 2",
    "Filter 2 Mix",
    "C Position",
    "C Warp",
    "C Level",
    "C Pan",
    "C Pitch",
    "C Fine",
    "C Detune",
    "C Blend",
    "C Warp 2",
    "LFO 5 Rate",
    "LFO 6 Rate",
    "LFO 7 Rate",
    "LFO 8 Rate",
    "Macro 1",
    "Macro 2",
    "Macro 3",
    "Macro 4",
    "Macro 5",
    "Macro 6",
    "Macro 7",
    "Macro 8",
    "A Morph",
    "B Morph",
    "C Morph",
    "A Width",
    "B Width",
    "C Width",
    "Filter Blend",
    "Filter 2 Blend",
    "Pan",
    "A Warp Spread",
    "B Warp Spread",
    "C Warp Spread",
    "A Pos Spread",
    "B Pos Spread",
    "C Pos Spread",
    "Key Track",
    "Key Track 2",
    "Amp Attack",
    "Amp Decay",
    "Amp Sustain",
    "Amp Release",
    "Env 2 Attack",
    "Env 2 Decay",
    "Env 2 Sustain",
    "Env 2 Release",
    "Env 3 Attack",
    "Env 3 Decay",
    "Env 3 Sustain",
    "Env 3 Release",
    "Env 4 Attack",
    "Env 4 Decay",
    "Env 4 Sustain",
    "Env 4 Release",
    "Env 5 Attack",
    "Env 5 Decay",
    "Env 5 Sustain",
    "Env 5 Release",
    "Env 6 Attack",
    "Env 6 Decay",
    "Env 6 Sustain",
    "Env 6 Release",
    "LFO 1 Phase",
    "LFO 1 Smooth",
    "LFO 1 Rise",
    "LFO 1 Delay",
    "LFO 2 Phase",
    "LFO 2 Smooth",
    "LFO 2 Rise",
    "LFO 2 Delay",
    "LFO 3 Phase",
    "LFO 3 Smooth",
    "LFO 3 Rise",
    "LFO 3 Delay",
    "LFO 4 Phase",
    "LFO 4 Smooth",
    "LFO 4 Rise",
    "LFO 4 Delay",
    "LFO 5 Phase",
    "LFO 5 Smooth",
    "LFO 5 Rise",
    "LFO 5 Delay",
    "LFO 6 Phase",
    "LFO 6 Smooth",
    "LFO 6 Rise",
    "LFO 6 Delay",
    "LFO 7 Phase",
    "LFO 7 Smooth",
    "LFO 7 Rise",
    "LFO 7 Delay",
    "LFO 8 Phase",
    "LFO 8 Smooth",
    "LFO 8 Rise",
    "LFO 8 Delay",
    "Volume",
];
/// How far each destination moves when a slot at amount 1 sees a source at
/// 1, in the destination's own unit: fractions of the range for positions,
/// warps, levels, blends, morphs, widths, pans, mixes, spreads, key
/// tracking, sustain levels, LFO phases and smoothing, and macros;
/// semitones for pitch; cents for fine and detune; octaves for cutoff, LFO
/// rates and envelope times (a time's whole knob range at 1); seconds for
/// LFO rise and delay; dB for Volume (its whole knob range at 1); and for
/// Amp, a gain factor added to 1 (so -1 silences and +1 doubles).
pub const DESTINATION_SCALES: [f32; DEST_COUNT] = {
    let mut scales = [1.0; DEST_COUNT];
    scales[DEST_NONE] = 0.0;
    let mut osc = 0;
    while osc < OSC_COUNT {
        scales[osc_dest(osc) + DEST_PITCH] = 24.0;
        scales[osc_dest(osc) + DEST_FINE] = 100.0;
        scales[osc_dest(osc) + DEST_DETUNE] = 100.0;
        osc += 1;
    }
    scales[DEST_GLOBAL_PITCH] = 24.0;
    scales[DEST_CUTOFF] = 10.0;
    scales[DEST_CUTOFF2] = 10.0;
    let mut lfo = 0;
    while lfo < LFO_COUNT {
        scales[lfo_rate_dest(lfo)] = 8.0;
        scales[lfo_dest(lfo, LFO_DEST_RISE)] = 10.0;
        scales[lfo_dest(lfo, LFO_DEST_DELAY)] = 10.0;
        lfo += 1;
    }
    // Envelope times move in octaves, a whole knob's range at amount 1.
    let mut env = 0;
    while env < ENV_COUNT {
        scales[env_dest(env, ENV_DEST_ATTACK)] = ATTACK_OCTAVES;
        scales[env_dest(env, ENV_DEST_DECAY)] = DECAY_OCTAVES;
        scales[env_dest(env, ENV_DEST_RELEASE)] = RELEASE_OCTAVES;
        env += 1;
    }
    scales[DEST_VOLUME] = VOLUME_RANGE_DB;
    scales
};
/// The envelope time knobs' ranges in octaves (0.5 ms to 10 s, 1 ms to
/// 10 s, 1 ms to 20 s), what an envelope time slot at amount 1 sweeps.
const ATTACK_OCTAVES: f32 = 14.287_712;
const DECAY_OCTAVES: f32 = 13.287_712;
const RELEASE_OCTAVES: f32 = 14.287_712;
/// The Volume knob's range.
pub const VOLUME_MIN_DB: f32 = -60.0;
pub const VOLUME_MAX_DB: f32 = 6.0;
const VOLUME_RANGE_DB: f32 = VOLUME_MAX_DB - VOLUME_MIN_DB;
pub const DEST_NONE: usize = 0;
/// First destination of oscillator A; B's start [`DEST_OSC_STRIDE`] later,
/// C's at [`DEST_OSC_C`] (see [`osc_dest`]). Offsets from it: position,
/// warp, level, pan, pitch, fine, detune, blend.
pub const DEST_OSC_A: usize = 1;
pub const DEST_OSC_STRIDE: usize = 8;
pub const DEST_POSITION: usize = 0;
pub const DEST_WARP: usize = 1;
pub const DEST_LEVEL: usize = 2;
pub const DEST_PAN: usize = 3;
pub const DEST_PITCH: usize = 4;
pub const DEST_FINE: usize = 5;
pub const DEST_DETUNE: usize = 6;
pub const DEST_BLEND: usize = 7;
pub const DEST_SUB_LEVEL: usize = 17;
pub const DEST_NOISE_LEVEL: usize = 18;
pub const DEST_GLOBAL_PITCH: usize = 19;
pub const DEST_CUTOFF: usize = 20;
pub const DEST_RESONANCE: usize = 21;
pub const DEST_DRIVE: usize = 22;
pub const DEST_FILTER_MIX: usize = 23;
/// LFO 1's rate; 2 to 4 follow, 5 to 8 start at [`DEST_LFO5_RATE`] (see
/// [`lfo_rate_dest`]).
pub const DEST_LFO1_RATE: usize = 24;
pub const DEST_AMP: usize = 28;
/// Oscillator A's second warp; B's is the next one.
pub const DEST_WARP2_A: usize = 29;
pub const DEST_CUTOFF2: usize = 31;
pub const DEST_RESONANCE2: usize = 32;
pub const DEST_DRIVE2: usize = 33;
pub const DEST_FILTER2_MIX: usize = 34;
/// First destination of oscillator C, in the same order as A's.
pub const DEST_OSC_C: usize = 35;
/// Oscillator C's second warp.
pub const DEST_WARP2_C: usize = 43;
pub const DEST_LFO5_RATE: usize = 44;
/// Macro 1's value; the others follow. A macro modulated this way moves
/// what it drives as if turned (Serum 2's macros as destinations).
pub const DEST_MACRO1: usize = 48;
/// Oscillator A's spectral morph; B's and C's follow.
pub const DEST_MORPH_A: usize = 56;
/// Oscillator A's unison stereo width; B's and C's follow.
pub const DEST_WIDTH_A: usize = 59;
pub const DEST_FILTER_BLEND: usize = 62;
pub const DEST_FILTER2_BLEND: usize = 63;
/// The voice's place between left and right, `-1..=1` (balance, so the
/// center is the sound as it is).
pub const DEST_VOICE_PAN: usize = 64;
/// Oscillator A's unison warp spread; B's and C's follow.
pub const DEST_WARP_SPREAD_A: usize = 65;
/// Oscillator A's unison position spread; B's and C's follow.
pub const DEST_POSITION_SPREAD_A: usize = 68;
/// Filter 1's key tracking; filter 2's follows.
pub const DEST_KEYTRACK: usize = 71;
pub const DEST_KEYTRACK2: usize = 72;
/// The amp envelope's attack: each envelope has [`ENV_DESTS`] in a row,
/// envelope by envelope (see [`env_dest`]). Times move in octaves, the
/// sustain level by adding to it. A time takes effect over the stage it
/// is in, stretching or shrinking what is left of it.
pub const DEST_ENV1_ATTACK: usize = 73;
pub const ENV_DEST_ATTACK: usize = 0;
pub const ENV_DEST_DECAY: usize = 1;
pub const ENV_DEST_SUSTAIN: usize = 2;
pub const ENV_DEST_RELEASE: usize = 3;
pub const ENV_DESTS: usize = 4;
/// LFO 1's phase: each LFO has [`LFO_DESTS`] in a row, LFO by LFO (see
/// [`lfo_dest`]). The phase shifts where in its cycle the LFO reads (a
/// whole cycle at 1), the smooth adds to its smoothing, and the rise and
/// delay add seconds.
pub const DEST_LFO1_PHASE: usize = DEST_ENV1_ATTACK + ENV_COUNT * ENV_DESTS;
pub const LFO_DEST_PHASE: usize = 0;
pub const LFO_DEST_SMOOTH: usize = 1;
pub const LFO_DEST_RISE: usize = 2;
pub const LFO_DEST_DELAY: usize = 3;
pub const LFO_DESTS: usize = 4;
/// The output volume in dB, per voice (a whole knob's range at amount 1).
pub const DEST_VOLUME: usize = DEST_LFO1_PHASE + LFO_COUNT * LFO_DESTS;
pub const DEST_COUNT: usize = DEST_VOLUME + 1;

/// First destination of oscillator `osc` (0 = A, 1 = B, 2 = C): add
/// [`DEST_POSITION`] to [`DEST_BLEND`] for the others.
pub const fn osc_dest(osc: usize) -> usize {
    if osc < 2 {
        DEST_OSC_A + osc * DEST_OSC_STRIDE
    } else {
        DEST_OSC_C
    }
}

/// Oscillator `osc`'s second-warp destination.
pub const fn warp2_dest(osc: usize) -> usize {
    if osc < 2 {
        DEST_WARP2_A + osc
    } else {
        DEST_WARP2_C
    }
}

/// Envelope `env`'s (0-based) destination `offset` (e.g. [`ENV_DEST_DECAY`]).
pub const fn env_dest(env: usize, offset: usize) -> usize {
    DEST_ENV1_ATTACK + env * ENV_DESTS + offset
}

/// LFO `lfo`'s (0-based) destination `offset` (e.g. [`LFO_DEST_SMOOTH`]).
pub const fn lfo_dest(lfo: usize, offset: usize) -> usize {
    DEST_LFO1_PHASE + lfo * LFO_DESTS + offset
}

/// LFO `lfo`'s (0-based) rate destination.
pub const fn lfo_rate_dest(lfo: usize) -> usize {
    if lfo < 4 {
        DEST_LFO1_RATE + lfo
    } else {
        DEST_LFO5_RATE + lfo - 4
    }
}

/// Filter `filter`'s (0 or 1) cutoff, resonance, drive, mix, blend and
/// key tracking destinations.
pub const FILTER_DESTINATIONS: [[usize; 6]; 2] = [
    [
        DEST_CUTOFF,
        DEST_RESONANCE,
        DEST_DRIVE,
        DEST_FILTER_MIX,
        DEST_FILTER_BLEND,
        DEST_KEYTRACK,
    ],
    [
        DEST_CUTOFF2,
        DEST_RESONANCE2,
        DEST_DRIVE2,
        DEST_FILTER2_MIX,
        DEST_FILTER2_BLEND,
        DEST_KEYTRACK2,
    ],
];

/// The destination a parameter's knob stands for, if modulation can move
/// it: what dragging a source onto the knob routes to, and what the rings
/// around it show. Pitch destinations go on the semitone knobs; Amp and the
/// global pitch and pan have no knob of their own.
pub fn param_dest(index: usize) -> Option<usize> {
    if index < OSC_C.end {
        let (osc, offset) = (index / OSC_PARAMS, index % OSC_PARAMS);
        let d = osc_dest(osc);
        return Some(match offset {
            OSC_POSITION => d + DEST_POSITION,
            OSC_WARP => d + DEST_WARP,
            OSC_WARP2 => warp2_dest(osc),
            OSC_LEVEL => d + DEST_LEVEL,
            OSC_PAN => d + DEST_PAN,
            OSC_SEMI => d + DEST_PITCH,
            OSC_FINE => d + DEST_FINE,
            OSC_DETUNE => d + DEST_DETUNE,
            OSC_BLEND => d + DEST_BLEND,
            OSC_MORPH => DEST_MORPH_A + osc,
            OSC_WIDTH => DEST_WIDTH_A + osc,
            OSC_WARP_SPREAD => DEST_WARP_SPREAD_A + osc,
            OSC_POSITION_SPREAD => DEST_POSITION_SPREAD_A + osc,
            _ => return None,
        });
    }
    for (filter, dests) in FILTER_DESTINATIONS.iter().enumerate() {
        let p = |i| filter_param(filter, i);
        let dest = match index {
            i if i == p(FILTER_CUTOFF) => dests[0],
            i if i == p(FILTER_RESONANCE) => dests[1],
            i if i == p(FILTER_DRIVE) => dests[2],
            i if i == p(FILTER_MIX) => dests[3],
            i if i == p(FILTER_BLEND) => dests[4],
            i if i == p(FILTER_KEYTRACK) => dests[5],
            _ => continue,
        };
        return Some(dest);
    }
    match index {
        SUB_LEVEL => return Some(DEST_SUB_LEVEL),
        NOISE_LEVEL => return Some(DEST_NOISE_LEVEL),
        MASTER_VOLUME => return Some(DEST_VOLUME),
        _ => {}
    }
    if MACROS.contains(&index) {
        return Some(DEST_MACRO1 + index - MACROS.start);
    }
    if LFOS.contains(&index) {
        let (lfo, offset) = (
            (index - LFOS.start) / LFO_PARAMS,
            (index - LFOS.start) % LFO_PARAMS,
        );
        return match offset {
            LFO_RATE => Some(lfo_rate_dest(lfo)),
            LFO_PHASE => Some(lfo_dest(lfo, LFO_DEST_PHASE)),
            LFO_SMOOTH => Some(lfo_dest(lfo, LFO_DEST_SMOOTH)),
            LFO_RISE => Some(lfo_dest(lfo, LFO_DEST_RISE)),
            LFO_DELAY => Some(lfo_dest(lfo, LFO_DEST_DELAY)),
            _ => None,
        };
    }
    for (env, layout) in ENVELOPE_LAYOUTS.iter().enumerate() {
        let offset = match index {
            i if i == layout.attack => ENV_DEST_ATTACK,
            i if i == layout.decay => ENV_DEST_DECAY,
            i if i == layout.sustain => ENV_DEST_SUSTAIN,
            i if i == layout.release => ENV_DEST_RELEASE,
            _ => continue,
        };
        return Some(env_dest(env, offset));
    }
    None
}

/// The parameter whose knob destination `dest` is shown on (the inverse of
/// [`param_dest`]), if it has one.
pub fn dest_param(dest: usize) -> Option<usize> {
    (0..PARAM_COUNT).find(|&index| param_dest(index) == Some(dest))
}

/// A slot's curve as `(k, norm)`, as an envelope stage with the same curve
/// bends: worked out once per control tick, so each voice only pays for
/// one `exp`.
pub use dsp::curve_coefficients;

/// A mod slot's curve applied to `x` (`-1..=1`): the shape of an envelope
/// stage with the same curve (`dsp::stage_shape`), mirrored for negative
/// `x` so bipolar sources bend the same both ways. 0 and ±1 stay put.
pub fn matrix_curve(x: f32, curve: f32) -> f32 {
    let (k, norm) = curve_coefficients(curve);
    apply_curve(x, k, norm)
}

#[inline]
pub(super) fn apply_curve(x: f32, k: f32, norm: f32) -> f32 {
    if k == 0.0 {
        x
    } else {
        ((1.0 - (-k * x.abs()).exp()) * norm).copysign(x)
    }
}

/// Defaults that differ between the oscillators.
struct OscDefaults {
    on: usize,
    table: usize,
    position: f32,
    octave: usize,
    fine: f32,
    unison: usize,
    detune: f32,
    blend: f32,
    width: f32,
    level: f32,
}

macro_rules! osc_params {
    ($id:literal, $name:literal, $warps:expr, $d:expr) => {
        [
            choice(
                concat!("osc_", $id, "_on"),
                concat!($name, " On"),
                OFF_ON,
                $d.on,
            ),
            choice(
                concat!("osc_", $id, "_table"),
                concat!($name, " Table"),
                &TABLE_LABELS,
                $d.table,
            ),
            float(
                concat!("osc_", $id, "_position"),
                concat!($name, " Position"),
                0.0,
                1.0,
                $d.position,
                "",
                false,
            ),
            choice(
                concat!("osc_", $id, "_warp_mode"),
                concat!($name, " Warp Mode"),
                $warps,
                0,
            ),
            float(
                concat!("osc_", $id, "_warp"),
                concat!($name, " Warp"),
                0.0,
                1.0,
                0.0,
                "",
                false,
            ),
            choice(
                concat!("osc_", $id, "_octave"),
                concat!($name, " Octave"),
                OCTAVES,
                $d.octave,
            ),
            choice(
                concat!("osc_", $id, "_semi"),
                concat!($name, " Semi"),
                SEMITONES,
                12,
            ),
            float(
                concat!("osc_", $id, "_fine"),
                concat!($name, " Fine"),
                -100.0,
                100.0,
                $d.fine,
                "cents",
                false,
            ),
            choice(
                concat!("osc_", $id, "_unison"),
                concat!($name, " Unison"),
                UNISON,
                $d.unison,
            ),
            float(
                concat!("osc_", $id, "_detune"),
                concat!($name, " Detune"),
                0.0,
                100.0,
                $d.detune,
                "cents",
                false,
            ),
            float(
                concat!("osc_", $id, "_blend"),
                concat!($name, " Blend"),
                0.0,
                1.0,
                $d.blend,
                "",
                false,
            ),
            float(
                concat!("osc_", $id, "_width"),
                concat!($name, " Width"),
                0.0,
                1.0,
                $d.width,
                "",
                false,
            ),
            float(
                concat!("osc_", $id, "_phase"),
                concat!($name, " Phase"),
                0.0,
                1.0,
                0.0,
                "",
                false,
            ),
            float(
                concat!("osc_", $id, "_random"),
                concat!($name, " Random Phase"),
                0.0,
                1.0,
                1.0,
                "",
                false,
            ),
            float(
                concat!("osc_", $id, "_level"),
                concat!($name, " Level"),
                0.0,
                1.0,
                $d.level,
                "",
                false,
            ),
            float(
                concat!("osc_", $id, "_pan"),
                concat!($name, " Pan"),
                -1.0,
                1.0,
                0.0,
                "",
                false,
            ),
            choice(
                concat!("osc_", $id, "_warp2_mode"),
                concat!($name, " Warp 2 Mode"),
                $warps,
                0,
            ),
            float(
                concat!("osc_", $id, "_warp2"),
                concat!($name, " Warp 2"),
                0.0,
                1.0,
                0.0,
                "",
                false,
            ),
            choice(
                concat!("osc_", $id, "_stack"),
                concat!($name, " Unison Stack"),
                STACKS,
                0,
            ),
            float(
                concat!("osc_", $id, "_warp_spread"),
                concat!($name, " Warp Spread"),
                0.0,
                1.0,
                0.0,
                "",
                false,
            ),
            choice(
                concat!("osc_", $id, "_morph_mode"),
                concat!($name, " Morph Mode"),
                MORPHS,
                0,
            ),
            float(
                concat!("osc_", $id, "_morph"),
                concat!($name, " Morph"),
                0.0,
                1.0,
                0.0,
                "",
                false,
            ),
            choice(
                concat!("osc_", $id, "_detune_mode"),
                concat!($name, " Detune Mode"),
                DETUNE_MODES,
                DETUNE_LINEAR,
            ),
            float(
                concat!("osc_", $id, "_position_spread"),
                concat!($name, " Position Spread"),
                0.0,
                1.0,
                0.0,
                "",
                false,
            ),
            choice(
                concat!("osc_", $id, "_mod_source"),
                concat!($name, " Mod Source"),
                MOD_SOURCES,
                MOD_SOURCE_AUTO,
            ),
            choice(
                concat!("osc_", $id, "_keytrack"),
                concat!($name, " Key Track"),
                OFF_ON,
                1,
            ),
        ]
    };
}

/// The remap curve the oscillators start with, `(x, y, curve)` per node:
/// an S, easing into the middle of the wave and out of it, so a Remap warp
/// does something as soon as it's picked.
pub const DEFAULT_REMAP: [(f32, f32, f32); 3] =
    [(0.0, 0.0, 0.6), (0.5, 0.5, -0.6), (1.0, 1.0, 0.0)];

/// A remap curve's parameters: `ids` and `names` hold its node count's,
/// then, for each node, its x, y and curve parameters'.
const fn remap(
    count: (&'static str, &'static str),
    ids: [[&'static str; 3]; MAX_REMAP_NODES],
    names: [[&'static str; 3]; MAX_REMAP_NODES],
) -> [ParamDescriptor; 1 + 3 * MAX_REMAP_NODES] {
    let mut out = [float("", "", 0.0, 1.0, 0.0, "", false); 1 + 3 * MAX_REMAP_NODES];
    out[0] = choice(count.0, count.1, REMAP_NODE_COUNTS, DEFAULT_REMAP.len() - 2);
    let mut i = 0;
    while i < MAX_REMAP_NODES {
        let (x, y, curve) = if i < DEFAULT_REMAP.len() {
            DEFAULT_REMAP[i]
        } else {
            (1.0, 1.0, 0.0)
        };
        out[1 + i] = float(ids[i][0], names[i][0], 0.0, 1.0, x, "", false);
        out[1 + MAX_REMAP_NODES + i] = float(ids[i][1], names[i][1], 0.0, 1.0, y, "", false);
        out[1 + 2 * MAX_REMAP_NODES + i] =
            float(ids[i][2], names[i][2], -1.0, 1.0, curve, "", false);
        i += 1;
    }
    out
}

/// One remap node's ids and names, as [`remap`] takes them.
macro_rules! remap_node {
    ($id:literal, $name:literal, $n:literal, $shown:literal) => {
        (
            [
                concat!("osc_", $id, "_rx", $n),
                concat!("osc_", $id, "_ry", $n),
                concat!("osc_", $id, "_rc", $n),
            ],
            [
                concat!($name, " Remap Node ", $shown, " X"),
                concat!($name, " Remap Node ", $shown, " Y"),
                concat!($name, " Remap Node ", $shown, " Curve"),
            ],
        )
    };
}

/// Oscillator `$id`'s remap curve: `osc_<id>_remap_nodes`, then
/// `osc_<id>_rx0`, `_ry0`, `_rc0` to `_rx7`, `_ry7`, `_rc7`.
macro_rules! remap_params {
    ($id:literal, $name:literal) => {{
        let all = [
            remap_node!($id, $name, "0", "1"),
            remap_node!($id, $name, "1", "2"),
            remap_node!($id, $name, "2", "3"),
            remap_node!($id, $name, "3", "4"),
            remap_node!($id, $name, "4", "5"),
            remap_node!($id, $name, "5", "6"),
            remap_node!($id, $name, "6", "7"),
            remap_node!($id, $name, "7", "8"),
        ];
        let mut ids = [[""; 3]; MAX_REMAP_NODES];
        let mut names = [[""; 3]; MAX_REMAP_NODES];
        let mut i = 0;
        while i < MAX_REMAP_NODES {
            ids[i] = all[i].0;
            names[i] = all[i].1;
            i += 1;
        }
        remap(
            (
                concat!("osc_", $id, "_remap_nodes"),
                concat!($name, " Remap Nodes"),
            ),
            ids,
            names,
        )
    }};
}

/// Defaults for the LFOs.
struct LfoDefaults {
    shape: usize,
    mode: usize,
    sync: usize,
    rate: f32,
    division: usize,
    rise: f32,
}

/// One LFO's settings other than its custom shape's points.
macro_rules! lfo_params {
    ($id:literal, $d:expr) => {
        [
            choice(
                concat!("lfo", $id, "_shape"),
                concat!("LFO ", $id, " Shape"),
                LFO_SHAPES,
                $d.shape,
            ),
            choice(
                concat!("lfo", $id, "_mode"),
                concat!("LFO ", $id, " Mode"),
                LFO_MODES,
                $d.mode,
            ),
            choice(
                concat!("lfo", $id, "_sync"),
                concat!("LFO ", $id, " Sync"),
                OFF_ON,
                $d.sync,
            ),
            float(
                concat!("lfo", $id, "_rate"),
                concat!("LFO ", $id, " Rate"),
                0.01,
                50.0,
                $d.rate,
                "Hz",
                true,
            ),
            choice(
                concat!("lfo", $id, "_division"),
                concat!("LFO ", $id, " Division"),
                DIVISIONS,
                $d.division,
            ),
            float(
                concat!("lfo", $id, "_phase"),
                concat!("LFO ", $id, " Phase"),
                0.0,
                1.0,
                0.0,
                "",
                false,
            ),
            float(
                concat!("lfo", $id, "_smooth"),
                concat!("LFO ", $id, " Smooth"),
                0.0,
                1.0,
                0.0,
                "",
                false,
            ),
            float(
                concat!("lfo", $id, "_rise"),
                concat!("LFO ", $id, " Rise"),
                0.0,
                10.0,
                $d.rise,
                "s",
                false,
            ),
            float(
                concat!("lfo", $id, "_glide"),
                concat!("LFO ", $id, " Point Glide"),
                0.0,
                1.0,
                1.0,
                "",
                false,
            ),
        ]
    };
}

/// One LFO's settings added after its points: delay, mono, the loop point
/// and how many nodes the Points shape has.
macro_rules! lfo_extra_params {
    ($id:literal) => {
        [
            float(
                concat!("lfo", $id, "_delay"),
                concat!("LFO ", $id, " Delay"),
                0.0,
                10.0,
                0.0,
                "s",
                false,
            ),
            choice(
                concat!("lfo", $id, "_mono"),
                concat!("LFO ", $id, " Mono"),
                OFF_ON,
                0,
            ),
            float(
                concat!("lfo", $id, "_loop"),
                concat!("LFO ", $id, " Loop Point"),
                0.0,
                1.0,
                0.5,
                "",
                false,
            ),
            choice(
                concat!("lfo", $id, "_nodes"),
                concat!("LFO ", $id, " Nodes"),
                NODE_COUNTS,
                DEFAULT_NODES.len() - 2,
            ),
        ]
    };
}

/// The custom shape the LFOs start with: a curved fall from the top to the
/// bottom, a decaying sweep that is easy to redraw into something else.
const CUSTOM_SHAPE: [f32; LFO_POINTS] = {
    let mut points = [0.0; LFO_POINTS];
    let mut i = 0;
    while i < LFO_POINTS {
        let rest = 1.0 - i as f32 / LFO_POINTS as f32;
        points[i] = rest * rest;
        i += 1;
    }
    points
};

/// The Points shape the LFOs start with, `(x, y, curve)` per node: a
/// triangle from the bottom up to the top and back, Vital's first shape.
pub const DEFAULT_NODES: [(f32, f32, f32); 3] = [(0.0, 0.0, 0.0), (0.5, 1.0, 0.0), (1.0, 0.0, 0.0)];

const fn points(
    ids: [&'static str; LFO_POINTS],
    names: [&'static str; LFO_POINTS],
) -> [ParamDescriptor; LFO_POINTS] {
    let mut out = [float("", "", 0.0, 1.0, 0.0, "", false); LFO_POINTS];
    let mut i = 0;
    while i < LFO_POINTS {
        out[i] = float(ids[i], names[i], 0.0, 1.0, CUSTOM_SHAPE[i], "", false);
        i += 1;
    }
    out
}

/// The 32 points of LFO `$id`'s custom shape, called `lfo<id>_p0` to
/// `lfo<id>_p31` and named "LFO <id> Point 1" to "LFO <id> Point 32".
macro_rules! lfo_points {
    ($id:literal) => {
        points(
            [
                concat!("lfo", $id, "_p0"),
                concat!("lfo", $id, "_p1"),
                concat!("lfo", $id, "_p2"),
                concat!("lfo", $id, "_p3"),
                concat!("lfo", $id, "_p4"),
                concat!("lfo", $id, "_p5"),
                concat!("lfo", $id, "_p6"),
                concat!("lfo", $id, "_p7"),
                concat!("lfo", $id, "_p8"),
                concat!("lfo", $id, "_p9"),
                concat!("lfo", $id, "_p10"),
                concat!("lfo", $id, "_p11"),
                concat!("lfo", $id, "_p12"),
                concat!("lfo", $id, "_p13"),
                concat!("lfo", $id, "_p14"),
                concat!("lfo", $id, "_p15"),
                concat!("lfo", $id, "_p16"),
                concat!("lfo", $id, "_p17"),
                concat!("lfo", $id, "_p18"),
                concat!("lfo", $id, "_p19"),
                concat!("lfo", $id, "_p20"),
                concat!("lfo", $id, "_p21"),
                concat!("lfo", $id, "_p22"),
                concat!("lfo", $id, "_p23"),
                concat!("lfo", $id, "_p24"),
                concat!("lfo", $id, "_p25"),
                concat!("lfo", $id, "_p26"),
                concat!("lfo", $id, "_p27"),
                concat!("lfo", $id, "_p28"),
                concat!("lfo", $id, "_p29"),
                concat!("lfo", $id, "_p30"),
                concat!("lfo", $id, "_p31"),
            ],
            [
                concat!("LFO ", $id, " Point 1"),
                concat!("LFO ", $id, " Point 2"),
                concat!("LFO ", $id, " Point 3"),
                concat!("LFO ", $id, " Point 4"),
                concat!("LFO ", $id, " Point 5"),
                concat!("LFO ", $id, " Point 6"),
                concat!("LFO ", $id, " Point 7"),
                concat!("LFO ", $id, " Point 8"),
                concat!("LFO ", $id, " Point 9"),
                concat!("LFO ", $id, " Point 10"),
                concat!("LFO ", $id, " Point 11"),
                concat!("LFO ", $id, " Point 12"),
                concat!("LFO ", $id, " Point 13"),
                concat!("LFO ", $id, " Point 14"),
                concat!("LFO ", $id, " Point 15"),
                concat!("LFO ", $id, " Point 16"),
                concat!("LFO ", $id, " Point 17"),
                concat!("LFO ", $id, " Point 18"),
                concat!("LFO ", $id, " Point 19"),
                concat!("LFO ", $id, " Point 20"),
                concat!("LFO ", $id, " Point 21"),
                concat!("LFO ", $id, " Point 22"),
                concat!("LFO ", $id, " Point 23"),
                concat!("LFO ", $id, " Point 24"),
                concat!("LFO ", $id, " Point 25"),
                concat!("LFO ", $id, " Point 26"),
                concat!("LFO ", $id, " Point 27"),
                concat!("LFO ", $id, " Point 28"),
                concat!("LFO ", $id, " Point 29"),
                concat!("LFO ", $id, " Point 30"),
                concat!("LFO ", $id, " Point 31"),
                concat!("LFO ", $id, " Point 32"),
            ],
        )
    };
}

/// The nodes of a Points shape: `ids` and `names` hold, for each of the
/// [`MAX_NODES`] nodes, its x, y and curve parameters' in turn.
const fn nodes(
    ids: [[&'static str; 3]; MAX_NODES],
    names: [[&'static str; 3]; MAX_NODES],
) -> [ParamDescriptor; 3 * MAX_NODES] {
    let mut out = [float("", "", 0.0, 1.0, 0.0, "", false); 3 * MAX_NODES];
    let mut i = 0;
    while i < MAX_NODES {
        // Nodes past the default shape wait at the end, flat.
        let (x, y, curve) = if i < DEFAULT_NODES.len() {
            DEFAULT_NODES[i]
        } else {
            (1.0, 0.0, 0.0)
        };
        out[i] = float(ids[i][0], names[i][0], 0.0, 1.0, x, "", false);
        out[MAX_NODES + i] = float(ids[i][1], names[i][1], 0.0, 1.0, y, "", false);
        out[2 * MAX_NODES + i] = float(ids[i][2], names[i][2], -1.0, 1.0, curve, "", false);
        i += 1;
    }
    out
}

/// One node's ids and names, as [`nodes`] takes them.
macro_rules! node {
    ($id:literal, $n:literal, $name:literal) => {
        (
            [
                concat!("lfo", $id, "_x", $n),
                concat!("lfo", $id, "_y", $n),
                concat!("lfo", $id, "_c", $n),
            ],
            [
                concat!("LFO ", $id, " Node ", $name, " X"),
                concat!("LFO ", $id, " Node ", $name, " Y"),
                concat!("LFO ", $id, " Node ", $name, " Curve"),
            ],
        )
    };
}

/// The nodes of LFO `$id`'s Points shape, called `lfo<id>_x0`, `_y0`,
/// `_c0` to `_x15`, `_y15`, `_c15`, named "LFO <id> Node 1 X" and so on.
macro_rules! lfo_nodes {
    ($id:literal) => {{
        let all = [
            node!($id, "0", "1"),
            node!($id, "1", "2"),
            node!($id, "2", "3"),
            node!($id, "3", "4"),
            node!($id, "4", "5"),
            node!($id, "5", "6"),
            node!($id, "6", "7"),
            node!($id, "7", "8"),
            node!($id, "8", "9"),
            node!($id, "9", "10"),
            node!($id, "10", "11"),
            node!($id, "11", "12"),
            node!($id, "12", "13"),
            node!($id, "13", "14"),
            node!($id, "14", "15"),
            node!($id, "15", "16"),
        ];
        let mut ids = [[""; 3]; MAX_NODES];
        let mut names = [[""; 3]; MAX_NODES];
        let mut i = 0;
        while i < MAX_NODES {
            ids[i] = all[i].0;
            names[i] = all[i].1;
            i += 1;
        }
        nodes(ids, names)
    }};
}

macro_rules! slot_params {
    ($id:literal, $source:expr, $dest:expr, $amount:expr) => {
        slot_params!($id, $source, $dest, $amount, SRC_NONE)
    };
    ($id:literal, $source:expr, $dest:expr, $amount:expr, $aux:expr) => {
        [
            choice(
                concat!("mod", $id, "_source"),
                concat!("Mod ", $id, " Source"),
                SOURCES,
                $source,
            ),
            choice(
                concat!("mod", $id, "_dest"),
                concat!("Mod ", $id, " Destination"),
                DESTINATIONS,
                $dest,
            ),
            float(
                concat!("mod", $id, "_amount"),
                concat!("Mod ", $id, " Amount"),
                -1.0,
                1.0,
                $amount,
                "",
                false,
            ),
            choice(
                concat!("mod", $id, "_aux"),
                concat!("Mod ", $id, " Aux Source"),
                SOURCES,
                $aux,
            ),
            float(
                concat!("mod", $id, "_curve"),
                concat!("Mod ", $id, " Curve"),
                -1.0,
                1.0,
                0.0,
                "",
                false,
            ),
            choice(
                concat!("mod", $id, "_polarity"),
                concat!("Mod ", $id, " Polarity"),
                POLARITIES,
                POLARITY_SOURCE,
            ),
            choice(
                concat!("mod", $id, "_bypass"),
                concat!("Mod ", $id, " Bypass"),
                OFF_ON,
                0,
            ),
        ]
    };
}

/// One envelope's delay and hold.
macro_rules! delay_hold {
    ($id:literal, $name:literal) => {
        [
            float(
                concat!("env", $id, "_delay"),
                concat!($name, " Delay"),
                0.0,
                2000.0,
                0.0,
                "ms",
                false,
            ),
            float(
                concat!("env", $id, "_hold"),
                concat!($name, " Hold"),
                0.0,
                2000.0,
                0.0,
                "ms",
                false,
            ),
        ]
    };
}

/// Copies `part` into `all` from index `at`, for assembling [`PARAMS`] in a
/// const context.
const fn place<const N: usize>(
    mut all: [ParamDescriptor; PARAM_COUNT],
    at: usize,
    part: [ParamDescriptor; N],
) -> [ParamDescriptor; PARAM_COUNT] {
    let mut i = 0;
    while i < N {
        all[at + i] = part[i];
        i += 1;
    }
    all
}

const UNSET: ParamDescriptor = float("", "", 0.0, 1.0, 0.0, "", false);

/// LFO `$n`'s (1-based) parameters, all of them, placed in `$all`.
macro_rules! place_lfo {
    ($all:ident, $n:literal, $id:literal, $d:expr) => {
        $all = place($all, lfo_param($n - 1, 0), lfo_params!($id, $d));
        $all = place($all, lfo_param($n - 1, LFO_POINT), lfo_points!($id));
        $all = place($all, lfo_param($n - 1, LFO_DELAY), lfo_extra_params!($id));
        $all = place($all, lfo_param($n - 1, LFO_NODE_X), lfo_nodes!($id));
    };
}

/// A static, so the descriptors exist once rather than being copied
/// wherever the list is used.
static ALL: [ParamDescriptor; PARAM_COUNT] = {
    let mut all = [UNSET; PARAM_COUNT];
    // The default patch: a warm, slowly moving detuned saw pad-lead. A is a
    // five-voice saw, B a quieter, narrower square-ish layer an octave down.
    all = place(
        all,
        OSC_A.start,
        osc_params!(
            "a",
            "A",
            WARPS_A,
            OscDefaults {
                on: 1,
                table: 0,
                position: 2.0 / 3.0,
                octave: 4,
                fine: 0.0,
                unison: 4,
                detune: 14.0,
                blend: 0.75,
                width: 0.8,
                level: 0.7,
            }
        ),
    );
    all = place(
        all,
        OSC_B.start,
        osc_params!(
            "b",
            "B",
            WARPS_B,
            OscDefaults {
                on: 1,
                table: 0,
                position: 0.85,
                octave: 3,
                fine: 4.0,
                unison: 2,
                detune: 8.0,
                blend: 0.6,
                width: 0.5,
                level: 0.4,
            }
        ),
    );
    all = place(all, osc_param(0, OSC_REMAP_NODES), remap_params!("a", "A"));
    all = place(all, osc_param(1, OSC_REMAP_NODES), remap_params!("b", "B"));
    // C starts off; switched on it is a plain saw at the note.
    all = place(
        all,
        OSC_C.start,
        osc_params!(
            "c",
            "C",
            WARPS_C,
            OscDefaults {
                on: 0,
                table: 0,
                position: 2.0 / 3.0,
                octave: 4,
                fine: 0.0,
                unison: 0,
                detune: 10.0,
                blend: 0.75,
                width: 0.5,
                level: 0.5,
            }
        ),
    );
    all = place(all, osc_param(2, OSC_REMAP_NODES), remap_params!("c", "C"));
    all = place(
        all,
        SUB.start,
        [
            choice("sub_on", "Sub On", OFF_ON, 0),
            choice("sub_shape", "Sub Shape", SUB_SHAPES, 0),
            choice("sub_octave", "Sub Octave", OCTAVES, 3),
            float("sub_level", "Sub Level", 0.0, 1.0, 0.5, "", false),
        ],
    );
    all = place(
        all,
        NOISE.start,
        [
            choice("noise_on", "Noise On", OFF_ON, 0),
            choice("noise_type", "Noise Type", NOISE_TYPES, 0),
            float("noise_level", "Noise Level", 0.0, 1.0, 0.2, "", false),
        ],
    );
    all = place(
        all,
        FILTER.start,
        [
            choice("filter_on", "Filter On", OFF_ON, 1),
            choice("filter_type", "Filter Type", FILTER_TYPES, 1),
            float(
                "filter_cutoff",
                "Cutoff",
                20.0,
                20_000.0,
                1800.0,
                "Hz",
                true,
            ),
            float("filter_resonance", "Resonance", 0.0, 1.0, 0.2, "", false),
            float("filter_drive", "Drive", 0.0, 1.0, 0.15, "", false),
            float("filter_keytrack", "Key Track", 0.0, 1.0, 0.4, "", false),
            float("filter_mix", "Filter Mix", 0.0, 1.0, 1.0, "", false),
            float("filter_blend", "Filter Blend", 0.0, 1.0, 0.5, "", false),
            choice("filter_osc_a", "Filter Osc A", OFF_ON, 1),
            choice("filter_osc_b", "Filter Osc B", OFF_ON, 1),
            choice("filter_sub", "Filter Sub", OFF_ON, 1),
            choice("filter_noise", "Filter Noise", OFF_ON, 1),
            choice("filter_osc_c", "Filter Osc C", OFF_ON, 1),
        ],
    );
    // Filter 2 starts off. Switched on it is a gentle high-pass after
    // filter 1, thinning out the bottom: a common second filter, and one
    // that is heard at once.
    all = place(
        all,
        FILTER2.start,
        [
            choice("filter2_on", "Filter 2 On", OFF_ON, 0),
            choice("filter2_type", "Filter 2 Type", FILTER_TYPES, 2),
            float(
                "filter2_cutoff",
                "Cutoff 2",
                20.0,
                20_000.0,
                150.0,
                "Hz",
                true,
            ),
            float("filter2_resonance", "Resonance 2", 0.0, 1.0, 0.1, "", false),
            float("filter2_drive", "Drive 2", 0.0, 1.0, 0.0, "", false),
            float("filter2_keytrack", "Key Track 2", 0.0, 1.0, 0.0, "", false),
            float("filter2_mix", "Filter 2 Mix", 0.0, 1.0, 1.0, "", false),
            float("filter2_blend", "Filter 2 Blend", 0.0, 1.0, 0.5, "", false),
            choice("filter_routing", "Filter Routing", FILTER_ROUTINGS, 0),
            choice("filter_osc_a_target", "Osc A Into", FILTER_TARGETS, 0),
            choice("filter_osc_b_target", "Osc B Into", FILTER_TARGETS, 0),
            choice("filter_sub_target", "Sub Into", FILTER_TARGETS, 0),
            choice("filter_noise_target", "Noise Into", FILTER_TARGETS, 0),
            choice("filter_osc_c_target", "Osc C Into", FILTER_TARGETS, 0),
        ],
    );
    all = place(
        all,
        ENV1.attack,
        [
            envelope::attack("env1_attack", 8.0),
            envelope::decay("env1_decay", 900.0),
            envelope::sustain("env1_sustain", 0.75),
            envelope::release("env1_release", 450.0),
            envelope::curve("env1_attack_curve", "Attack Curve", -0.3),
            envelope::curve("env1_decay_curve", "Decay Curve", -0.6),
            envelope::curve("env1_release_curve", "Release Curve", -0.6),
        ],
    );
    all = place(all, env_param(0, 0), delay_hold!("1", "Amp"));
    all = place(
        all,
        ENV2.attack,
        [
            envelope::attack("env2_attack", 2.0),
            envelope::decay("env2_decay", 700.0),
            envelope::sustain("env2_sustain", 0.25),
            envelope::release("env2_release", 500.0),
            envelope::curve("env2_attack_curve", "Attack Curve", -0.5),
            envelope::curve("env2_decay_curve", "Decay Curve", -0.7),
            envelope::curve("env2_release_curve", "Release Curve", -0.7),
        ],
    );
    all = place(all, env_param(1, 0), delay_hold!("2", "Env 2"));
    all = place(
        all,
        ENV3.attack,
        [
            envelope::attack("env3_attack", 300.0),
            envelope::decay("env3_decay", 1000.0),
            envelope::sustain("env3_sustain", 0.5),
            envelope::release("env3_release", 600.0),
            envelope::curve("env3_attack_curve", "Attack Curve", 0.0),
            envelope::curve("env3_decay_curve", "Decay Curve", -0.5),
            envelope::curve("env3_release_curve", "Release Curve", -0.5),
        ],
    );
    all = place(all, env_param(2, 0), delay_hold!("3", "Env 3"));
    // Envelopes 4 to 6: a pluck, a slow swell and a mid-length decay, so
    // each is useful as it comes.
    all = place(
        all,
        ENV4.attack,
        [
            envelope::attack("env4_attack", 1.0),
            envelope::decay("env4_decay", 250.0),
            envelope::sustain("env4_sustain", 0.0),
            envelope::release("env4_release", 250.0),
            envelope::curve("env4_attack_curve", "Attack Curve", -0.5),
            envelope::curve("env4_decay_curve", "Decay Curve", -0.7),
            envelope::curve("env4_release_curve", "Release Curve", -0.7),
        ],
    );
    all = place(all, env_param(3, 0), delay_hold!("4", "Env 4"));
    all = place(
        all,
        ENV5.attack,
        [
            envelope::attack("env5_attack", 1500.0),
            envelope::decay("env5_decay", 2000.0),
            envelope::sustain("env5_sustain", 1.0),
            envelope::release("env5_release", 1500.0),
            envelope::curve("env5_attack_curve", "Attack Curve", 0.3),
            envelope::curve("env5_decay_curve", "Decay Curve", -0.5),
            envelope::curve("env5_release_curve", "Release Curve", -0.5),
        ],
    );
    all = place(all, env_param(4, 0), delay_hold!("5", "Env 5"));
    all = place(
        all,
        ENV6.attack,
        [
            envelope::attack("env6_attack", 10.0),
            envelope::decay("env6_decay", 600.0),
            envelope::sustain("env6_sustain", 0.4),
            envelope::release("env6_release", 400.0),
            envelope::curve("env6_attack_curve", "Attack Curve", -0.3),
            envelope::curve("env6_decay_curve", "Decay Curve", -0.6),
            envelope::curve("env6_release_curve", "Release Curve", -0.6),
        ],
    );
    all = place(all, env_param(5, 0), delay_hold!("6", "Env 6"));
    all = place(
        all,
        env_mono_param(1),
        [
            choice("env2_mono", "Env 2 Mono", OFF_ON, 0),
            choice("env3_mono", "Env 3 Mono", OFF_ON, 0),
            choice("env4_mono", "Env 4 Mono", OFF_ON, 0),
            choice("env5_mono", "Env 5 Mono", OFF_ON, 0),
            choice("env6_mono", "Env 6 Mono", OFF_ON, 0),
        ],
    );
    all = place(
        all,
        env_sync_param(0),
        [
            choice("env1_sync", "Amp Sync", OFF_ON, 0),
            choice("env2_sync", "Env 2 Sync", OFF_ON, 0),
            choice("env3_sync", "Env 3 Sync", OFF_ON, 0),
            choice("env4_sync", "Env 4 Sync", OFF_ON, 0),
            choice("env5_sync", "Env 5 Sync", OFF_ON, 0),
            choice("env6_sync", "Env 6 Sync", OFF_ON, 0),
            choice("env1_retrigger", "Amp Legato Retrigger", OFF_ON, 0),
            choice("env2_retrigger", "Env 2 Legato Retrigger", OFF_ON, 0),
            choice("env3_retrigger", "Env 3 Legato Retrigger", OFF_ON, 0),
            choice("env4_retrigger", "Env 4 Legato Retrigger", OFF_ON, 0),
            choice("env5_retrigger", "Env 5 Legato Retrigger", OFF_ON, 0),
            choice("env6_retrigger", "Env 6 Legato Retrigger", OFF_ON, 0),
        ],
    );
    place_lfo!(
        all,
        1,
        "1",
        LfoDefaults {
            shape: 0,
            mode: 0,
            sync: 0,
            rate: 0.35,
            division: 8,
            rise: 0.0,
        }
    );
    place_lfo!(
        all,
        2,
        "2",
        LfoDefaults {
            shape: 1,
            mode: 1,
            sync: 0,
            rate: 5.0,
            division: 8,
            rise: 0.4,
        }
    );
    place_lfo!(
        all,
        3,
        "3",
        LfoDefaults {
            shape: 5,
            mode: 0,
            sync: 1,
            rate: 2.0,
            division: 14,
            rise: 0.0,
        }
    );
    place_lfo!(
        all,
        4,
        "4",
        LfoDefaults {
            shape: 6,
            mode: 0,
            sync: 0,
            rate: 0.8,
            division: 8,
            rise: 0.0,
        }
    );
    // LFOs 5 to 8: a synced drawn shape restarting with each note, a
    // synced saw, a slow triangle and a chaotic drift, ready to route.
    place_lfo!(
        all,
        5,
        "5",
        LfoDefaults {
            shape: 8,
            mode: 1,
            sync: 1,
            rate: 1.0,
            division: 8,
            rise: 0.0,
        }
    );
    place_lfo!(
        all,
        6,
        "6",
        LfoDefaults {
            shape: 3,
            mode: 1,
            sync: 1,
            rate: 1.0,
            division: 11,
            rise: 0.0,
        }
    );
    place_lfo!(
        all,
        7,
        "7",
        LfoDefaults {
            shape: 1,
            mode: 0,
            sync: 0,
            rate: 0.2,
            division: 3,
            rise: 0.0,
        }
    );
    place_lfo!(
        all,
        8,
        "8",
        LfoDefaults {
            shape: 9,
            mode: 0,
            sync: 0,
            rate: 0.5,
            division: 3,
            rise: 0.0,
        }
    );
    // Env 2 opens the filter on each note, LFO 1 slowly moves A through the
    // table, the mod wheel brightens and adds vibrato (LFO 2, fading in),
    // and velocity adds a little bite.
    all = place(
        all,
        slot_param(0, 0),
        slot_params!("1", SRC_ENV1 + 1, DEST_CUTOFF, 0.25),
    );
    all = place(
        all,
        slot_param(1, 0),
        slot_params!("2", SRC_LFO1, DEST_OSC_A + DEST_POSITION, 0.06),
    );
    all = place(
        all,
        slot_param(2, 0),
        slot_params!("3", SRC_MOD_WHEEL, DEST_CUTOFF, 0.3),
    );
    all = place(
        all,
        slot_param(3, 0),
        slot_params!("4", SRC_VELOCITY, DEST_CUTOFF, 0.1),
    );
    all = place(
        all,
        slot_param(4, 0),
        slot_params!(
            "5",
            SRC_LFO1 + 1,
            DEST_GLOBAL_PITCH,
            0.3 / 24.0,
            SRC_MOD_WHEEL
        ),
    );
    all = place(all, slot_param(5, 0), slot_params!("6", 0, 0, 0.0));
    all = place(all, slot_param(6, 0), slot_params!("7", 0, 0, 0.0));
    all = place(all, slot_param(7, 0), slot_params!("8", 0, 0, 0.0));
    all = place(all, slot_param(8, 0), slot_params!("9", 0, 0, 0.0));
    all = place(all, slot_param(9, 0), slot_params!("10", 0, 0, 0.0));
    all = place(all, slot_param(10, 0), slot_params!("11", 0, 0, 0.0));
    all = place(all, slot_param(11, 0), slot_params!("12", 0, 0, 0.0));
    all = place(all, slot_param(12, 0), slot_params!("13", 0, 0, 0.0));
    all = place(all, slot_param(13, 0), slot_params!("14", 0, 0, 0.0));
    all = place(all, slot_param(14, 0), slot_params!("15", 0, 0, 0.0));
    all = place(all, slot_param(15, 0), slot_params!("16", 0, 0, 0.0));
    all = place(all, slot_param(16, 0), slot_params!("17", 0, 0, 0.0));
    all = place(all, slot_param(17, 0), slot_params!("18", 0, 0, 0.0));
    all = place(all, slot_param(18, 0), slot_params!("19", 0, 0, 0.0));
    all = place(all, slot_param(19, 0), slot_params!("20", 0, 0, 0.0));
    all = place(all, slot_param(20, 0), slot_params!("21", 0, 0, 0.0));
    all = place(all, slot_param(21, 0), slot_params!("22", 0, 0, 0.0));
    all = place(all, slot_param(22, 0), slot_params!("23", 0, 0, 0.0));
    all = place(all, slot_param(23, 0), slot_params!("24", 0, 0, 0.0));
    all = place(all, slot_param(24, 0), slot_params!("25", 0, 0, 0.0));
    all = place(all, slot_param(25, 0), slot_params!("26", 0, 0, 0.0));
    all = place(all, slot_param(26, 0), slot_params!("27", 0, 0, 0.0));
    all = place(all, slot_param(27, 0), slot_params!("28", 0, 0, 0.0));
    all = place(all, slot_param(28, 0), slot_params!("29", 0, 0, 0.0));
    all = place(all, slot_param(29, 0), slot_params!("30", 0, 0, 0.0));
    all = place(all, slot_param(30, 0), slot_params!("31", 0, 0, 0.0));
    all = place(all, slot_param(31, 0), slot_params!("32", 0, 0, 0.0));
    all = place(all, slot_param(32, 0), slot_params!("33", 0, 0, 0.0));
    all = place(all, slot_param(33, 0), slot_params!("34", 0, 0, 0.0));
    all = place(all, slot_param(34, 0), slot_params!("35", 0, 0, 0.0));
    all = place(all, slot_param(35, 0), slot_params!("36", 0, 0, 0.0));
    all = place(all, slot_param(36, 0), slot_params!("37", 0, 0, 0.0));
    all = place(all, slot_param(37, 0), slot_params!("38", 0, 0, 0.0));
    all = place(all, slot_param(38, 0), slot_params!("39", 0, 0, 0.0));
    all = place(all, slot_param(39, 0), slot_params!("40", 0, 0, 0.0));
    all = place(all, slot_param(40, 0), slot_params!("41", 0, 0, 0.0));
    all = place(all, slot_param(41, 0), slot_params!("42", 0, 0, 0.0));
    all = place(all, slot_param(42, 0), slot_params!("43", 0, 0, 0.0));
    all = place(all, slot_param(43, 0), slot_params!("44", 0, 0, 0.0));
    all = place(all, slot_param(44, 0), slot_params!("45", 0, 0, 0.0));
    all = place(all, slot_param(45, 0), slot_params!("46", 0, 0, 0.0));
    all = place(all, slot_param(46, 0), slot_params!("47", 0, 0, 0.0));
    all = place(all, slot_param(47, 0), slot_params!("48", 0, 0, 0.0));
    all = place(all, slot_param(48, 0), slot_params!("49", 0, 0, 0.0));
    all = place(all, slot_param(49, 0), slot_params!("50", 0, 0, 0.0));
    all = place(all, slot_param(50, 0), slot_params!("51", 0, 0, 0.0));
    all = place(all, slot_param(51, 0), slot_params!("52", 0, 0, 0.0));
    all = place(all, slot_param(52, 0), slot_params!("53", 0, 0, 0.0));
    all = place(all, slot_param(53, 0), slot_params!("54", 0, 0, 0.0));
    all = place(all, slot_param(54, 0), slot_params!("55", 0, 0, 0.0));
    all = place(all, slot_param(55, 0), slot_params!("56", 0, 0, 0.0));
    all = place(all, slot_param(56, 0), slot_params!("57", 0, 0, 0.0));
    all = place(all, slot_param(57, 0), slot_params!("58", 0, 0, 0.0));
    all = place(all, slot_param(58, 0), slot_params!("59", 0, 0, 0.0));
    all = place(all, slot_param(59, 0), slot_params!("60", 0, 0, 0.0));
    all = place(all, slot_param(60, 0), slot_params!("61", 0, 0, 0.0));
    all = place(all, slot_param(61, 0), slot_params!("62", 0, 0, 0.0));
    all = place(all, slot_param(62, 0), slot_params!("63", 0, 0, 0.0));
    all = place(all, slot_param(63, 0), slot_params!("64", 0, 0, 0.0));
    all = place(
        all,
        MACROS.start,
        [
            float("macro1", "Macro 1", 0.0, 1.0, 0.0, "", false),
            float("macro2", "Macro 2", 0.0, 1.0, 0.0, "", false),
            float("macro3", "Macro 3", 0.0, 1.0, 0.0, "", false),
            float("macro4", "Macro 4", 0.0, 1.0, 0.0, "", false),
            float("macro5", "Macro 5", 0.0, 1.0, 0.0, "", false),
            float("macro6", "Macro 6", 0.0, 1.0, 0.0, "", false),
            float("macro7", "Macro 7", 0.0, 1.0, 0.0, "", false),
            float("macro8", "Macro 8", 0.0, 1.0, 0.0, "", false),
        ],
    );
    all = place(
        all,
        VOICE.start,
        [
            choice("voice_mode", "Voice Mode", VOICE_MODES, 0),
            choice("polyphony", "Polyphony", VOICE_COUNTS, 15),
            float("glide", "Glide", 0.0, 2000.0, 0.0, "ms", false),
            float("bend_range", "Bend Range", 0.0, 24.0, 2.0, "st", false),
            float("velocity", "Velocity", 0.0, 1.0, 0.6, "", false),
            choice("glide_always", "Glide Always", OFF_ON, 1),
            float("glide_curve", "Glide Curve", -1.0, 1.0, 0.0, "", false),
            choice("glide_scaled", "Glide Scaled", OFF_ON, 0),
            choice("quality", "Quality", QUALITIES, 0),
        ],
    );
    all = place(
        all,
        MASTER.start,
        [float(
            "master_volume",
            "Volume",
            VOLUME_MIN_DB,
            VOLUME_MAX_DB,
            -3.0,
            "dB",
            false,
        )],
    );
    all
};

pub const PARAMS: &[ParamDescriptor] = &ALL;

/// Where parameter `index` sits in menus that list every parameter (see
/// `BuiltInInstrumentDescriptor::groups`): its section, and for LFOs'
/// drawn shapes and the matrix, a submenu of its own.
pub fn groups(index: usize) -> [&'static str; 2] {
    const OSCS: [&str; OSC_COUNT] = ["Osc A", "Osc B", "Osc C"];
    const ENVS: [&str; ENV_COUNT] = ["Amp", "Env 2", "Env 3", "Env 4", "Env 5", "Env 6"];
    const LFO_NAMES: [&str; LFO_COUNT] = [
        "LFO 1", "LFO 2", "LFO 3", "LFO 4", "LFO 5", "LFO 6", "LFO 7", "LFO 8",
    ];
    const STEPS: [&str; LFO_COUNT] = [
        "LFO 1 Point",
        "LFO 2 Point",
        "LFO 3 Point",
        "LFO 4 Point",
        "LFO 5 Point",
        "LFO 6 Point",
        "LFO 7 Point",
        "LFO 8 Point",
    ];
    const NODES: [&str; LFO_COUNT] = [
        "LFO 1 Node",
        "LFO 2 Node",
        "LFO 3 Node",
        "LFO 4 Node",
        "LFO 5 Node",
        "LFO 6 Node",
        "LFO 7 Node",
        "LFO 8 Node",
    ];
    const SLOTS: [&str; MOD_SLOTS] = [
        "Mod 1", "Mod 2", "Mod 3", "Mod 4", "Mod 5", "Mod 6", "Mod 7", "Mod 8", "Mod 9", "Mod 10",
        "Mod 11", "Mod 12", "Mod 13", "Mod 14", "Mod 15", "Mod 16", "Mod 17", "Mod 18", "Mod 19",
        "Mod 20", "Mod 21", "Mod 22", "Mod 23", "Mod 24", "Mod 25", "Mod 26", "Mod 27", "Mod 28",
        "Mod 29", "Mod 30", "Mod 31", "Mod 32", "Mod 33", "Mod 34", "Mod 35", "Mod 36", "Mod 37",
        "Mod 38", "Mod 39", "Mod 40", "Mod 41", "Mod 42", "Mod 43", "Mod 44", "Mod 45", "Mod 46",
        "Mod 47", "Mod 48", "Mod 49", "Mod 50", "Mod 51", "Mod 52", "Mod 53", "Mod 54", "Mod 55",
        "Mod 56", "Mod 57", "Mod 58", "Mod 59", "Mod 60", "Mod 61", "Mod 62", "Mod 63", "Mod 64",
    ];
    if index < OSC_C.end {
        return [OSCS[index / OSC_PARAMS], ""];
    }
    if ENVELOPES.contains(&index) {
        let offset = index - ENVELOPES.start;
        let env = if offset < ENV_COUNT * ENV_PARAMS {
            offset / ENV_PARAMS
        } else if offset < ENV_SWITCHES {
            offset - ENV_COUNT * ENV_PARAMS + 1
        } else {
            (offset - ENV_SWITCHES) % ENV_COUNT
        };
        return ["Envelopes", ENVS[env.min(ENV_COUNT - 1)]];
    }
    if LFOS.contains(&index) {
        let (lfo, offset) = (
            (index - LFOS.start) / LFO_PARAMS,
            (index - LFOS.start) % LFO_PARAMS,
        );
        let sub = if (LFO_POINT..LFO_POINT + LFO_POINTS).contains(&offset) {
            STEPS[lfo]
        } else if offset >= LFO_NODE_X {
            NODES[lfo]
        } else {
            ""
        };
        return [LFO_NAMES[lfo], sub];
    }
    if MATRIX.contains(&index) {
        return ["Matrix", SLOTS[(index - MATRIX.start) / SLOT_PARAMS]];
    }
    for (name, range) in SECTIONS {
        if range.contains(&index) {
            return [name, ""];
        }
    }
    ["", ""]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ParameterKind;

    #[test]
    fn every_parameter_is_placed_once_in_its_section() {
        for (index, param) in PARAMS.iter().enumerate() {
            assert!(!param.id.is_empty(), "parameter {index} was never placed");
        }
        assert_eq!(SECTIONS.last().unwrap().1.end, PARAM_COUNT);
        assert_eq!(ENVELOPES.end, LFOS.start);
        assert_eq!(lfo_param(LFO_COUNT, 0), LFOS.end);
        assert_eq!(slot_param(MOD_SLOTS, 0), MATRIX.end);
        assert_eq!(osc_param(2, 0), OSC_B.end);
        assert_eq!(osc_param(OSC_COUNT, 0), OSC_C.end);
        assert_eq!(SECTIONS[0].1.start, 0);
        for (i, (_, range)) in SECTIONS.iter().enumerate().skip(1) {
            assert_eq!(SECTIONS[i - 1].1.end, range.start, "{i}");
        }
        assert_eq!(env_mono_param(ENV_COUNT), env_sync_param(0));
        assert_eq!(env_retrigger_param(ENV_COUNT), ENVELOPES.end);
        assert_eq!(ENV6.attack + 7, env_param(5, 0));
        assert_eq!(env_param(1, 0), ENV2.attack + 7);
        for env in 0..ENV_COUNT {
            let n = env + 1;
            let layout = ENVELOPE_LAYOUTS[env];
            assert_eq!(PARAMS[layout.attack].id, format!("env{n}_attack"));
            assert_eq!(
                PARAMS[layout.curves.unwrap()[2]].id,
                format!("env{n}_release_curve")
            );
            assert_eq!(
                PARAMS[env_param(env, ENV_DELAY)].id,
                format!("env{n}_delay")
            );
            assert_eq!(PARAMS[env_param(env, ENV_HOLD)].id, format!("env{n}_hold"));
            if env > 0 {
                assert_eq!(PARAMS[env_mono_param(env)].id, format!("env{n}_mono"));
            }
            assert_eq!(PARAMS[env_sync_param(env)].id, format!("env{n}_sync"));
            assert_eq!(
                PARAMS[env_retrigger_param(env)].id,
                format!("env{n}_retrigger")
            );
        }
        for lfo in 0..LFO_COUNT {
            let n = lfo + 1;
            assert_eq!(
                PARAMS[lfo_param(lfo, LFO_SHAPE)].id,
                format!("lfo{n}_shape")
            );
            assert_eq!(
                PARAMS[lfo_param(lfo, LFO_POINT + 31)].id,
                format!("lfo{n}_p31")
            );
            assert_eq!(
                PARAMS[lfo_param(lfo, LFO_DELAY)].id,
                format!("lfo{n}_delay")
            );
            assert_eq!(
                PARAMS[lfo_param(lfo, LFO_NODES)].id,
                format!("lfo{n}_nodes")
            );
            for node in 0..MAX_NODES {
                let x = PARAMS[lfo_param(lfo, LFO_NODE_X + node)].id;
                let c = PARAMS[lfo_param(lfo, LFO_NODE_CURVE + node)].id;
                assert_eq!(x, format!("lfo{n}_x{node}"));
                assert_eq!(c, format!("lfo{n}_c{node}"));
            }
            assert_eq!(SOURCES[lfo_source(lfo)], format!("LFO {n}"));
            assert_eq!(DESTINATIONS[lfo_rate_dest(lfo)], format!("LFO {n} Rate"));
            assert_eq!(source_lfo(lfo_source(lfo)), Some(lfo));
        }
        for env in 0..ENV_COUNT {
            assert_eq!(SOURCES[env_source(env)], format!("Env {}", env + 1));
            assert_eq!(source_env(env_source(env)), Some(env));
        }
        for m in 0..MACRO_COUNT {
            assert_eq!(SOURCES[macro_source(m)], format!("Macro {}", m + 1));
            assert_eq!(DESTINATIONS[DEST_MACRO1 + m], format!("Macro {}", m + 1));
            assert_eq!(PARAMS[macro_param(m)].id, format!("macro{}", m + 1));
            assert_eq!(source_macro(macro_source(m)), Some(m));
        }
        for slot in 0..MOD_SLOTS {
            let n = slot + 1;
            assert_eq!(
                PARAMS[slot_param(slot, SLOT_SOURCE)].id,
                format!("mod{n}_source")
            );
            assert_eq!(
                PARAMS[slot_param(slot, SLOT_BYPASS)].id,
                format!("mod{n}_bypass")
            );
        }
        assert_eq!(SOURCES.len(), SOURCE_COUNT);
        let mut grouped: Vec<usize> = SOURCE_GROUPS
            .iter()
            .flat_map(|(_, group)| group.iter().copied())
            .collect();
        grouped.sort_unstable();
        assert_eq!(grouped, (1..SOURCE_COUNT).collect::<Vec<_>>());
        assert_eq!(STACKS.len(), STACK_INTERVALS.len());
        assert_eq!(DESTINATIONS.len(), DEST_COUNT);
        // Projects store a slot's destination by its place, so the ones
        // added later follow "Pan", where the list used to end; and the
        // importers find them by name, so names are unique.
        assert_eq!(DESTINATIONS[DEST_VOICE_PAN], "Pan");
        assert_eq!(DEST_WARP_SPREAD_A, DEST_VOICE_PAN + 1);
        let mut names = DESTINATIONS.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), DEST_COUNT, "destination names are unique");
        for (env, layout) in ENVELOPE_LAYOUTS.iter().enumerate() {
            let stages = ["Attack", "Decay", "Sustain", "Release"];
            for (offset, stage) in stages.iter().enumerate() {
                let name = format!("{} {stage}", layout.name);
                assert_eq!(DESTINATIONS[env_dest(env, offset)], name);
            }
        }
        for lfo in 0..LFO_COUNT {
            for (offset, what) in ["Phase", "Smooth", "Rise", "Delay"].iter().enumerate() {
                let name = format!("LFO {} {what}", lfo + 1);
                assert_eq!(DESTINATIONS[lfo_dest(lfo, offset)], name);
            }
        }
        assert_eq!(DESTINATIONS[DEST_VOLUME], "Volume");
        assert_eq!(TABLE_LABELS[USER_TABLE], "User");
        assert_eq!(&TABLE_LABELS[..USER_TABLE], &TABLE_NAMES[..USER_TABLE]);
        for (table, name) in TABLE_NAMES.iter().enumerate() {
            assert_eq!(TABLE_LABELS[choice_of(table)], *name);
            assert_eq!(table_of(choice_of(table)), Some(table));
        }
        assert_eq!(table_of(USER_TABLE), None);
        assert_eq!(USER_TABLE, 14, "where projects store \"User\"");
        assert_eq!(WARPS_A.len(), WARPS_B.len());
        // Osc C mirrors A and B: ids, names, ranges and destinations.
        for offset in 0..OSC_PARAMS {
            let a = &PARAMS[osc_param(0, offset)];
            let c = &PARAMS[osc_param(2, offset)];
            assert_eq!(c.id, a.id.replacen("osc_a_", "osc_c_", 1));
            assert_eq!(c.name, a.name.replacen("A ", "C ", 1));
            if let (
                ParameterKind::Float { min, max, unit, .. },
                ParameterKind::Float {
                    min: c_min,
                    max: c_max,
                    unit: c_unit,
                    ..
                },
            ) = (a.kind, c.kind)
            {
                assert_eq!((min, max, unit), (c_min, c_max, c_unit), "{}", c.id);
            }
        }
        for (osc, letter) in ["A", "B", "C"].iter().enumerate() {
            for offset in 0..DEST_OSC_STRIDE {
                let name = DESTINATIONS[osc_dest(osc) + offset];
                assert!(name.starts_with(letter), "{name}");
                assert_eq!(
                    DESTINATION_SCALES[osc_dest(osc) + offset],
                    DESTINATION_SCALES[DEST_OSC_A + offset]
                );
            }
            assert_eq!(DESTINATIONS[warp2_dest(osc)], format!("{letter} Warp 2"));
            assert_eq!(DESTINATIONS[DEST_MORPH_A + osc], format!("{letter} Morph"));
            assert_eq!(DESTINATIONS[DEST_WIDTH_A + osc], format!("{letter} Width"));
            let (switch, target) = SOURCE_ROUTES[OSC_ROUTES[osc]];
            let id = format!("filter_osc_{}", letter.to_lowercase());
            assert_eq!(PARAMS[switch].id, id);
            assert_eq!(PARAMS[target].id, format!("{id}_target"));
        }
        assert_eq!(PARAMS[SOURCE_ROUTES[SUB_ROUTE].0].id, "filter_sub");
        assert_eq!(
            PARAMS[SOURCE_ROUTES[NOISE_ROUTE].1].id,
            "filter_noise_target"
        );
        for index in FILTER_ON..=FILTER_BLEND {
            let first = PARAMS[index].id.trim_start_matches("filter_");
            let second = PARAMS[filter_param(1, index)].id;
            assert_eq!(second, format!("filter2_{first}"));
        }
    }

    #[test]
    fn knobs_and_destinations_meet() {
        // Every destination with a knob maps back to it, and the knob's
        // destination names it.
        for (dest, name) in DESTINATIONS.iter().enumerate().skip(1) {
            if let Some(index) = dest_param(dest) {
                assert_eq!(param_dest(index), Some(dest), "{name}");
            }
        }
        assert_eq!(param_dest(osc_param(1, OSC_POSITION)), Some(DEST_OSC_A + 8));
        assert_eq!(param_dest(FILTER2_CUTOFF), Some(DEST_CUTOFF2));
        assert_eq!(param_dest(lfo_param(6, LFO_RATE)), Some(lfo_rate_dest(6)));
        assert_eq!(param_dest(macro_param(7)), Some(DEST_MACRO1 + 7));
        assert_eq!(
            param_dest(lfo_param(6, LFO_PHASE)),
            Some(lfo_dest(6, LFO_DEST_PHASE))
        );
        assert_eq!(param_dest(lfo_param(6, LFO_GLIDE)), None);
        assert_eq!(
            param_dest(ENV4.release),
            Some(env_dest(3, ENV_DEST_RELEASE))
        );
        assert_eq!(param_dest(FILTER2_KEYTRACK), Some(DEST_KEYTRACK2));
        assert_eq!(param_dest(MASTER_VOLUME), Some(DEST_VOLUME));
        assert_eq!(dest_param(DEST_AMP), None);
        // Groups name every parameter's section.
        for (index, param) in PARAMS.iter().enumerate() {
            assert!(!groups(index)[0].is_empty(), "{}", param.id);
        }
        assert_eq!(
            groups(lfo_param(2, LFO_NODE_Y + 3)),
            ["LFO 3", "LFO 3 Node"]
        );
        assert_eq!(groups(env_mono_param(4)), ["Envelopes", "Env 5"]);
        assert_eq!(groups(slot_param(20, SLOT_AMOUNT)), ["Matrix", "Mod 21"]);
    }

    #[test]
    fn curves_bend_both_ways_and_keep_their_ends() {
        for curve in [-1.0, -0.4, 0.0, 0.5, 1.0] {
            assert_eq!(matrix_curve(0.0, curve), 0.0);
            assert!((matrix_curve(1.0, curve) - 1.0).abs() < 1e-6);
            assert!((matrix_curve(-1.0, curve) + 1.0).abs() < 1e-6);
            let half = matrix_curve(0.5, curve);
            assert!((half - dsp::stage_shape(0.5, curve)).abs() < 1e-5);
            assert_eq!(matrix_curve(-0.5, curve), -half);
        }
        assert!(matrix_curve(0.5, -0.5) > 0.7, "negative rises fast");
        assert!(matrix_curve(0.5, 0.5) < 0.3, "positive starts slow");
    }
}
