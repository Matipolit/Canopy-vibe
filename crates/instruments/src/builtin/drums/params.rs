//! How a pad's parameters are described: its own eleven, then each layer's
//! twenty-five, with ids and names made by macros (only macros can join
//! strings at compile time) and defaults from the pad's [`PadPatch`].

use super::layer::{FilterKind, Layer, Mode};
use super::parts::{amount, choice, float, hz, level, ms, pan, semis};
use super::patches::{LayerPatch, PadPatch};
use super::source::Source;
use super::{LAYERS, Voice};
use crate::ParamDescriptor;

/// A parameter's id and name.
pub(super) type Names = (&'static str, &'static str);

/// How many parameters a pad has.
pub(super) const PAD_PARAMS: usize = Voice::COMMON + LAYERS * Layer::COUNT;

/// Every id and name of one pad's parameters, from [`pad_names!`].
pub(super) struct PadNames {
    pub pad: [Names; Voice::COMMON],
    pub layers: [[Names; Layer::COUNT]; LAYERS],
}

/// The ids and names of layer `$n` of the pad with id `$id` and name
/// `$name`: "kick_l1_decay", "Kick Layer 1 Decay". In [`Layer`]'s order.
macro_rules! layer_names {
    ($id:literal, $name:literal, $n:literal) => {
        layer_names!(@ $id, $name, $n;
            mode "Mode", source "Source", level "Level", pan "Pan",
            velocity "Velocity", pitch "Pitch", shape "Shape", phase "Phase", fm "FM",
            pitch_env "Pitch Env", pitch_time "Pitch Time", delay "Delay",
            attack "Attack", hold "Hold", decay "Decay", curve "Curve",
            bursts "Bursts", gap "Burst Gap", filter "Filter", cutoff "Cutoff",
            resonance "Resonance", filter_env "Filter Env", filter_time "Filter Time",
            track "Key Track", drive "Drive")
    };
    (@ $id:literal, $name:literal, $n:literal; $($suffix:ident $label:literal),*) => {
        [$((
            concat!($id, "_l", $n, "_", stringify!($suffix)),
            concat!($name, " Layer ", $n, " ", $label),
        )),*]
    };
}

/// The ids and names of a pad's parameters, in [`Voice`]'s order, then
/// its layers'.
macro_rules! pad_names {
    ($id:literal, $name:literal) => {
        $crate::builtin::drums::params::PadNames {
            pad: pad_names!(@ $id, $name;
                transpose "Tune", length "Length", level "Level", pan "Pan",
                drive "Drive", velocity "Velocity", choke "Choke", filter "Filter",
                cutoff "Cutoff", resonance "Resonance", vary "Vary Hits"),
            layers: [
                layer_names!($id, $name, "1"),
                layer_names!($id, $name, "2"),
                layer_names!($id, $name, "3"),
                layer_names!($id, $name, "4"),
                layer_names!($id, $name, "5"),
                layer_names!($id, $name, "6"),
            ],
        }
    };
    (@ $id:literal, $name:literal; $($suffix:ident $label:literal),*) => {
        [$((concat!($id, "_", stringify!($suffix)), concat!($name, " ", $label))),*]
    };
}

pub(super) use {layer_names, pad_names};

const _: () = assert!(LAYERS == 6, "pad_names! spells out six layers");

/// Choke groups: a pad cuts off the others in its group when it plays.
pub const CHOKES: &[&str] = &["None", "1", "2", "3", "4"];

const BURSTS: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8"];

/// A pad's parameters, named by `names`, with defaults from `patch`.
pub(super) const fn pad_params(
    names: &PadNames,
    patch: &PadPatch,
) -> [ParamDescriptor; PAD_PARAMS] {
    let n = &names.pad;
    let own = [
        semis(n[Voice::TUNE], -24.0, 24.0, patch.tune),
        float(n[Voice::LENGTH], 10.0, 1000.0, patch.length, "%", true),
        level(n[Voice::LEVEL], patch.level),
        pan(n[Voice::PAN], patch.pan),
        amount(n[Voice::DRIVE], patch.drive),
        amount(n[Voice::VELOCITY], patch.velocity),
        choice(n[Voice::CHOKE], CHOKES, patch.choke),
        choice(n[Voice::FILTER], &FilterKind::LABELS, patch.filter as usize),
        float(n[Voice::CUTOFF], 20.0, 20_000.0, patch.cutoff, "Hz", true),
        amount(n[Voice::RESONANCE], patch.resonance),
        choice(n[Voice::VARY], crate::OFF_ON, 0),
    ];
    let mut table = [own[0]; PAD_PARAMS];
    let mut i = 0;
    while i < Voice::COMMON {
        table[i] = own[i];
        i += 1;
    }
    let mut layer = 0;
    while layer < LAYERS {
        let params = layer_params(&names.layers[layer], &patch.layers[layer]);
        let mut j = 0;
        while j < Layer::COUNT {
            table[Voice::COMMON + layer * Layer::COUNT + j] = params[j];
            j += 1;
        }
        layer += 1;
    }
    table
}

const fn layer_params(
    n: &[Names; Layer::COUNT],
    l: &LayerPatch,
) -> [ParamDescriptor; Layer::COUNT] {
    [
        choice(n[Layer::MODE], &Mode::LABELS, l.mode as usize),
        choice(n[Layer::SOURCE], &Source::LABELS, l.source as usize),
        float(n[Layer::LEVEL], -60.0, 24.0, l.level, "dB", false),
        pan(n[Layer::PAN], l.pan),
        amount(n[Layer::VELOCITY], l.velocity),
        hz(n[Layer::PITCH], l.pitch),
        amount(n[Layer::SHAPE], l.shape),
        float(n[Layer::PHASE], 0.0, 360.0, l.phase, "°", false),
        amount(n[Layer::FM], l.fm),
        semis(n[Layer::PITCH_ENV], -48.0, 48.0, l.pitch_env),
        ms(n[Layer::PITCH_TIME], 1.0, 2000.0, l.pitch_time),
        float(n[Layer::DELAY], 0.0, 500.0, l.delay, "ms", false),
        // From zero: a hit that starts at full level.
        ms(n[Layer::ATTACK], 0.0, 2000.0, l.attack),
        ms(n[Layer::HOLD], 0.0, 2000.0, l.hold),
        ms(n[Layer::DECAY], 1.0, 20_000.0, l.decay),
        amount(n[Layer::CURVE], l.curve),
        choice(n[Layer::BURSTS], BURSTS, l.bursts - 1),
        ms(n[Layer::GAP], 1.0, 200.0, l.gap),
        choice(n[Layer::FILTER], &FilterKind::LABELS, l.filter as usize),
        float(n[Layer::CUTOFF], 20.0, 20_000.0, l.cutoff, "Hz", true),
        amount(n[Layer::RESONANCE], l.resonance),
        float(n[Layer::FILTER_ENV], -8.0, 8.0, l.filter_env, "oct", false),
        ms(n[Layer::FILTER_TIME], 1.0, 5000.0, l.filter_time),
        amount(n[Layer::TRACK], l.track),
        amount(n[Layer::DRIVE], l.drive),
    ]
}
