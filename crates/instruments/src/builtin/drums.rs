//! Drum Synth: a synthesized kit in the spirit of the TR-808 and TR-909,
//! where every sound can be taken apart and rebuilt, played from the
//! General MIDI drum map.
//!
//! Each of the sixteen pads ([`voices`]) is up to [`LAYERS`] layers
//! ([`Layer`]): a [`Source`] (oscillators, a drum head's or a bell's
//! modes, six kinds of noise, an impulse) with its own pitch envelope,
//! drive, filter and filter envelope, amplitude envelope (with bursts, for
//! claps) and level and pan. Each can be switched off, or heard only through
//! the FM it puts on the layer below. The pads' own parameters ([`Voice`])
//! tune, lengthen, filter, drive, level and pan the sum, and put pads in
//! choke groups. The factory settings ([`patches`]) rebuild the classic
//! machines' recipes from these parts. Three kit parameters come first
//! ([`KIT_PARAMS`]).
//!
//! Hits are one-shots. A pad plays one hit at a time: hitting it again
//! fades out the previous hit within a few milliseconds, as on a drum
//! machine, and pads in the same choke group cut each other off the same
//! way (the hats, out of the box). Note Offs are ignored, and so is All
//! Notes Off: stopping the transport lets cymbals ring out, and editing
//! notes during playback (which sends one) doesn't cut them off.
//! [`Instrument::reset`] silences everything.

mod layer;
mod pad;
mod params;
mod parts;
mod patches;
mod source;

use std::ops::Range;

use dsp::{Gain, Processor, Smoothed};
use midi::{NoteEvent, NoteEventKind};

pub use self::layer::{FilterKind, Layer, Mode};
use self::pad::{Pad, Scratch};
pub use self::params::CHOKES;
use self::params::{PAD_PARAMS, layer_names, pad_names};
use self::parts::Shaper;
pub use self::source::Source;
use crate::{BuiltInInstrumentDescriptor, DataKind, Instrument, MAX_METERS, ParamDescriptor};

pub const DESCRIPTOR: BuiltInInstrumentDescriptor = BuiltInInstrumentDescriptor {
    id: "canopy.drums",
    name: "Drum Synth",
    description: "Synthesized drum kit: kicks, snares, hats, claps, toms and percussion, \
                  each built from layers of oscillators, noise and filters",
    version: "2.0",
    author: "canopy",
    params: PARAMS,
    envelopes: &[],
    create: || Box::new(Drums::new()),
    data: DataKind::None,
    groups,
};

/// Menus group the parameters by pad, and a pad's by layer.
fn groups(index: usize) -> [&'static str; 2] {
    const LAYER_NAMES: [&str; LAYERS] = [
        "Layer 1", "Layer 2", "Layer 3", "Layer 4", "Layer 5", "Layer 6",
    ];
    let Some(pad) = index.checked_sub(KIT.len()) else {
        return ["Kit", ""];
    };
    let Some(voice) = VOICES.get(pad / PAD_PARAMS) else {
        return ["", ""];
    };
    let layer = (pad % PAD_PARAMS)
        .checked_sub(Voice::COMMON)
        .map_or("", |offset| LAYER_NAMES[offset / Layer::COUNT]);
    [voice.name, layer]
}

/// How many layers each pad has.
pub const LAYERS: usize = 6;

/// One sound of the kit, as the UI shows it: a pad with its knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    pub name: &'static str,
    /// The MIDI notes that play it, from the General MIDI drum map. The first
    /// is the main one, which its pad should send. Some others play it a few
    /// semitones off, so that GM's six toms and four crashes stay distinct.
    pub notes: &'static [u8],
    /// Its parameters, as indices into [`DESCRIPTOR`]'s `params`: its own
    /// ([`Voice::TUNE`] to [`Voice::VARY`]), then each layer's (see
    /// [`Voice::layer_param`]). Their names begin with the voice's name,
    /// which [`Voice::label`] takes off for a knob on the voice's pad.
    pub params: Range<usize>,
}

impl Voice {
    /// In semitones, ±24: moves every layer's pitch, and the cutoffs of
    /// layers that track it.
    pub const TUNE: usize = 0;
    /// In %: stretches every layer's hold and decay.
    pub const LENGTH: usize = 1;
    /// In dB, after drive.
    pub const LEVEL: usize = 2;
    /// `-1` (left) to `1` (right), constant power.
    pub const PAN: usize = 3;
    /// `0..=1`: saturation that thickens the sound without raising its peaks.
    pub const DRIVE: usize = 4;
    /// `0..=1`: how much velocity changes the level. 0 ignores it, 0.5 makes
    /// half velocity −6 dB, 1 makes it −12 dB.
    pub const VELOCITY: usize = 5;
    /// Its choke group ([`CHOKES`]): 0 for none. Playing a pad cuts off the
    /// others in its group.
    pub const CHOKE: usize = 6;
    /// [`FilterKind`] of the filter on the pad's sum.
    pub const FILTER: usize = 7;
    /// In Hz.
    pub const CUTOFF: usize = 8;
    /// `0..=1`.
    pub const RESONANCE: usize = 9;
    /// Off (0): every hit is the same, noise and all. On (1): each hit's
    /// noise, metal partials' phases and burst timing come out differently,
    /// as on an analog machine, though a render from the top still always
    /// sounds the same.
    pub const VARY: usize = 10;
    /// How many parameters a pad has of its own; its layers' follow.
    pub const COMMON: usize = 11;

    /// The index (into [`DESCRIPTOR`]'s `params`) of the parameter at
    /// `offset` into this voice's, such as [`Voice::LEVEL`].
    pub fn param(&self, offset: usize) -> usize {
        self.params.start + offset
    }

    /// The index of layer `layer`'s parameter at `offset`, such as
    /// [`Layer::CUTOFF`].
    pub fn layer_param(&self, layer: usize, offset: usize) -> usize {
        self.params.start + Voice::COMMON + layer * Layer::COUNT + offset
    }

    /// Layer `layer`'s parameters.
    pub fn layer(&self, layer: usize) -> Range<usize> {
        let start = self.layer_param(layer, 0);
        start..start + Layer::COUNT
    }

    /// A parameter's name without the voice's name: "Decay" for "Kick Decay".
    pub fn label(&self, param: &ParamDescriptor) -> &'static str {
        param
            .name
            .strip_prefix(self.name)
            .map_or(param.name, str::trim_start)
    }
}

/// The kit's voices, in the order of their meters (see [`Drums`]).
pub fn voices() -> &'static [Voice] {
    &VOICES
}

/// The voice a MIDI note plays, by index into [`voices`], if any.
pub fn voice_for_note(note: u8) -> Option<usize> {
    NOTE_VOICES
        .get(usize::from(note))
        .filter(|&&voice| voice != NO_VOICE)
        .map(|&voice| usize::from(voice))
}

/// How far `note` transposes the voice it plays, in semitones.
pub fn note_shift(note: u8) -> f32 {
    NOTE_SHIFT.get(usize::from(note)).copied().unwrap_or(0.0)
}

/// The kit's own parameters, before every voice's.
pub const KIT_PARAMS: Range<usize> = 0..KIT.len();
/// In dB, after everything.
pub const KIT_LEVEL: usize = 0;
/// Transposes every voice, ±12 semitones.
pub const KIT_TUNE: usize = 1;
/// Saturation on the whole kit, `0..=1`. Driven, the kit never goes past
/// full scale (before Kit Level), however many voices pile up.
pub const KIT_DRIVE: usize = 2;

const KIT: [ParamDescriptor; 3] = [
    parts::level(("kit_level", "Kit Level"), 0.0),
    parts::semis(("kit_tune", "Kit Tune"), -12.0, 12.0, 0.0),
    parts::amount(("kit_drive", "Kit Drive"), 0.0),
];

const VOICE_COUNT: usize = 16;

const _: () = assert!(VOICE_COUNT <= MAX_METERS, "a meter per voice");

/// Each voice's parameters, in voice order. Only read at compile time,
/// into [`PARAM_TABLE`].
#[allow(clippy::large_const_arrays)]
const PADS: [[ParamDescriptor; PAD_PARAMS]; VOICE_COUNT] = [
    params::pad_params(&pad_names!("kick", "Kick"), &patches::KICK),
    params::pad_params(&pad_names!("rim", "Rimshot"), &patches::RIMSHOT),
    params::pad_params(&pad_names!("snare", "Snare"), &patches::SNARE),
    params::pad_params(&pad_names!("clap", "Clap"), &patches::CLAP),
    params::pad_params(
        &pad_names!("closed_hat", "Closed Hat"),
        &patches::CLOSED_HAT,
    ),
    params::pad_params(&pad_names!("pedal_hat", "Pedal Hat"), &patches::PEDAL_HAT),
    params::pad_params(&pad_names!("open_hat", "Open Hat"), &patches::OPEN_HAT),
    params::pad_params(&pad_names!("low_tom", "Low Tom"), &patches::LOW_TOM),
    params::pad_params(&pad_names!("mid_tom", "Mid Tom"), &patches::MID_TOM),
    params::pad_params(&pad_names!("high_tom", "High Tom"), &patches::HIGH_TOM),
    params::pad_params(&pad_names!("crash", "Crash"), &patches::CRASH),
    params::pad_params(&pad_names!("ride", "Ride"), &patches::RIDE),
    params::pad_params(&pad_names!("cowbell", "Cowbell"), &patches::COWBELL),
    params::pad_params(&pad_names!("shaker", "Shaker"), &patches::SHAKER),
    params::pad_params(&pad_names!("conga", "Conga"), &patches::CONGA),
    params::pad_params(&pad_names!("clave", "Clave"), &patches::CLAVE),
];

/// Each pad's place in the kit's balance: the gain that puts a
/// full-velocity hit at 0 dB where it would sit in a mix.
const BALANCE: [f32; VOICE_COUNT] = [
    0.6, 0.35, 0.55, 0.45, 0.2, 0.18, 0.2, 0.5, 0.5, 0.5, 0.3, 0.3, 0.3, 0.2, 0.45, 0.3,
];

const VOICE_TABLE: [Voice; VOICE_COUNT] = [
    voice("Kick", &[36, 35], 0),
    voice("Rimshot", &[37], 1),
    voice("Snare", &[38, 40], 2),
    voice("Clap", &[39], 3),
    voice("Closed Hat", &[42], 4),
    voice("Pedal Hat", &[44], 5),
    voice("Open Hat", &[46], 6),
    voice("Low Tom", &[41, 43], 7),
    voice("Mid Tom", &[45, 47], 8),
    voice("High Tom", &[48, 50], 9),
    // Crash 2, splash and China.
    voice("Crash", &[49, 57, 55, 52], 10),
    // Ride 2 and the ride's bell.
    voice("Ride", &[51, 59, 53], 11),
    voice("Cowbell", &[56], 12),
    // Maracas, shaker, cabasa and tambourine (turn on its second layer).
    voice("Shaker", &[70, 82, 69, 54], 13),
    // Mute high conga, low conga, and the bongos.
    voice("Conga", &[63, 62, 64, 60, 61], 14),
    // The wood blocks.
    voice("Clave", &[75, 76, 77], 15),
];

static VOICES: [Voice; VOICE_COUNT] = VOICE_TABLE;

/// Notes that play their voice transposed, in semitones: the second note of
/// each tom pair is the higher tom in GM, the alternative crashes and ride
/// are other sizes of cymbal, and the bongos and wood blocks are other
/// sizes of conga and clave.
const TRANSPOSED: &[(u8, f32)] = &[
    (43, 3.0),
    (47, 2.0),
    (50, 3.0),
    (57, 2.0),
    (55, 4.0),
    (52, -2.0),
    (59, -2.0),
    (64, -5.0),
    (60, 7.0),
    (61, 4.0),
    (76, -14.0),
    (77, -17.0),
];

const fn voice(name: &'static str, notes: &'static [u8], index: usize) -> Voice {
    let start = KIT.len() + index * PAD_PARAMS;
    Voice {
        name,
        notes,
        params: start..start + PAD_PARAMS,
    }
}

const PARAM_COUNT: usize = KIT.len() + VOICE_COUNT * PAD_PARAMS;

/// The kit's parameters, then each voice's in turn.
static PARAM_TABLE: [ParamDescriptor; PARAM_COUNT] = {
    let mut table = [KIT[0]; PARAM_COUNT];
    let mut index = 0;
    while index < KIT.len() {
        table[index] = KIT[index];
        index += 1;
    }
    let mut voice = 0;
    while voice < VOICE_COUNT {
        let mut i = 0;
        while i < PAD_PARAMS {
            table[index] = PADS[voice][i];
            index += 1;
            i += 1;
        }
        voice += 1;
    }
    table
};

const PARAMS: &[ParamDescriptor] = &PARAM_TABLE;

const NO_VOICE: u8 = u8::MAX;

/// The voice each MIDI note plays, or [`NO_VOICE`].
const NOTE_VOICES: [u8; 128] = {
    let mut map = [NO_VOICE; 128];
    let mut voice = 0;
    while voice < VOICE_COUNT {
        let notes = VOICE_TABLE[voice].notes;
        let mut i = 0;
        while i < notes.len() {
            assert!(map[notes[i] as usize] == NO_VOICE, "a note plays one voice");
            map[notes[i] as usize] = voice as u8;
            i += 1;
        }
        voice += 1;
    }
    map
};

/// How far each note transposes its voice, in semitones.
const NOTE_SHIFT: [f32; 128] = {
    let mut shift = [0.0; 128];
    let mut i = 0;
    while i < TRANSPOSED.len() {
        let (note, semitones) = TRANSPOSED[i];
        assert!(NOTE_VOICES[note as usize] != NO_VOICE, "transposes a voice");
        shift[note as usize] = semitones;
        i += 1;
    }
    shift
};

/// Ramp for Kit Drive, so turning it doesn't zipper.
const DRIVE_RAMP_MS: f32 = 20.0;

/// The sixteen-pad drum synth.
///
/// Meters: slot `i`, for each voice `i` of [`voices`], is that voice's
/// envelope after velocity and its Level, before its pan and the kit's
/// settings: its audible layers' envelopes, weighted by their levels.
/// Linear: a full-velocity hit at 0 dB reads 1 for the block it starts in,
/// however short it is, then follows its layers as they decay; 0 when silent. Enough to flash a pad as it plays and
/// fade it as it rings. The remaining slots are 0.
///
/// Layer parameters, and a pad's Tune, Length, Velocity and Choke, are read
/// as a hit starts, so turning them never makes a ringing hit jump; the
/// next hit plays the new value. A pad's level, pan, drive and filter, and
/// the kit's, ramp over 20 ms.
pub struct Drums {
    /// Every parameter's value, clamped.
    values: Box<[f32]>,
    pads: Vec<Pad>,
    scratch: Scratch,
    level: Gain,
    drive: Smoothed,
    shaper: Shaper,
    /// Kit Tune, as a frequency ratio.
    tune: f32,
    /// Whether any voice sounded in the last block. If not, the kit's level
    /// and drive jump to their settings as the next block starts, so that a
    /// change made in silence applies to the next hit in full.
    sounding: bool,
}

impl Drums {
    pub fn new() -> Self {
        // Different seeds keep the pads' noise unrelated.
        let seed = |voice: usize| 0x9E37_79B9u32.wrapping_mul(voice as u32 + 1);
        let pads = BALANCE
            .iter()
            .enumerate()
            .map(|(i, &balance)| Pad::new(balance, seed(i)))
            .collect();
        let mut drums = Self {
            values: vec![0.0; PARAM_COUNT].into_boxed_slice(),
            pads,
            scratch: Scratch::new(512),
            level: Gain::new(0.0),
            drive: Smoothed::new(0.0, DRIVE_RAMP_MS),
            shaper: Shaper::new(),
            tune: 1.0,
            sounding: false,
        };
        for (index, param) in PARAMS.iter().enumerate() {
            drums.set_param(index, param.default_value());
        }
        drums.prepare(48_000.0, 512);
        drums
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let Some(index) = voice_for_note(note) else {
            return;
        };
        let voice = &VOICES[index];
        let group = self.values[voice.param(Voice::CHOKE)] as usize;
        if group > 0 {
            for (other, pad) in VOICES.iter().enumerate() {
                if other != index && self.values[pad.param(Voice::CHOKE)] as usize == group {
                    self.pads[other].choke();
                }
            }
        }
        let ratio = self.tune * parts::semitones(NOTE_SHIFT[usize::from(note)]);
        self.pads[index].trigger(note, &self.values[voice.params.clone()], velocity, ratio);
    }

    /// The hit `note` started is now bent `cents` from its own pitch.
    fn note_bend(&mut self, note: u8, cents: i16) {
        if let Some(index) = voice_for_note(note) {
            self.pads[index].bend(note, NoteEventKind::note_bend_ratio(cents));
        }
    }

    /// Adds every sounding voice into `left` and `right`. Returns whether any was sounding.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let mut sounded = false;
        let chunk = self.scratch.len();
        for (left, right) in left.chunks_mut(chunk).zip(right.chunks_mut(chunk)) {
            for pad in &mut self.pads {
                sounded |= pad.render(&mut self.scratch, left, right);
            }
        }
        sounded
    }

    /// Jumps the kit's level and drive to their settings.
    fn settle(&mut self) {
        self.level.reset();
        self.drive.settle();
        self.shaper.set(self.drive.target());
    }
}

impl Default for Drums {
    fn default() -> Self {
        Self::new()
    }
}

impl Instrument for Drums {
    fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        self.scratch = Scratch::new(max_block.max(1));
        for pad in &mut self.pads {
            pad.prepare(sample_rate);
        }
        self.level.prepare(sample_rate);
        self.drive.prepare(sample_rate);
        self.reset();
    }

    fn reset(&mut self) {
        for pad in &mut self.pads {
            pad.reset();
        }
        self.settle();
        self.sounding = false;
    }

    fn process(&mut self, events: &[NoteEvent], left: &mut [f32], right: &mut [f32]) {
        left.fill(0.0);
        right.fill(0.0);
        if !self.sounding {
            self.settle();
        }
        let mut sounded = false;
        for (span, event) in crate::segments(events, left.len()) {
            sounded |= self.render(&mut left[span.clone()], &mut right[span]);
            match event {
                Some(NoteEventKind::On { pitch, velocity }) => {
                    self.note_on(pitch.get(), velocity.as_f32());
                }
                Some(NoteEventKind::NoteBend { pitch, cents }) => {
                    self.note_bend(pitch.get(), cents);
                }
                _ => {}
            }
        }
        self.sounding = sounded;
        if !sounded {
            // Silence needs no processing.
            return;
        }
        if self.drive.is_smoothing() || self.drive.target() > 0.0 {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                self.shaper.set(self.drive.tick());
                *l = self.shaper.process(*l);
                *r = self.shaper.process(*r);
            }
        }
        self.level.process(left, right);
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(value) = crate::checked_value(PARAMS, index, value) else {
            return;
        };
        self.values[index] = value;
        match index {
            KIT_LEVEL => self.level.set_db(value),
            KIT_TUNE => self.tune = parts::semitones(value),
            KIT_DRIVE => self.drive.set_target(value),
            _ => {
                let voice = (index - KIT.len()) / PAD_PARAMS;
                let params = VOICES[voice].params.clone();
                let offset = index - params.start;
                if matches!(
                    offset,
                    Voice::LEVEL
                        | Voice::PAN
                        | Voice::DRIVE
                        | Voice::FILTER
                        | Voice::CUTOFF
                        | Voice::RESONANCE
                ) {
                    self.pads[voice].mix(&self.values[params]);
                }
            }
        }
    }

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out.fill(0.0);
        for (slot, pad) in out.iter_mut().zip(&self.pads) {
            *slot = pad.meter();
        }
    }
}

/// One hit of voice `voice` at full velocity, rendered on its own for a
/// display: mono, at `sample_rate`, until it dies away or `max_seconds`
/// have passed. `values` are the whole kit's parameters, as [`DESCRIPTOR`]
/// lists them (Kit Tune applies, Kit Level and Drive don't). With `solo`,
/// only that layer is heard (even if it's FM only); the one above it still
/// modulates it.
pub fn render_hit(
    values: &[f32],
    voice: usize,
    solo: Option<usize>,
    sample_rate: f32,
    max_seconds: f32,
) -> Vec<f32> {
    let Some(info) = VOICES.get(voice) else {
        return Vec::new();
    };
    let value = |index: usize| {
        let param = &PARAMS[index];
        values
            .get(index)
            .copied()
            .filter(|v| !v.is_nan())
            .map_or(param.default_value(), |v| param.clamp(v))
    };
    let mut p: Vec<f32> = info.params.clone().map(value).collect();
    if let Some(solo) = solo {
        for layer in 0..LAYERS {
            let mode = &mut p[Voice::COMMON + layer * Layer::COUNT + Layer::MODE];
            match Mode::from_value(*mode) {
                Mode::On if layer != solo => *mode = Mode::FmOnly as usize as f32,
                // A modulator, heard for once.
                Mode::FmOnly if layer == solo => *mode = Mode::On as usize as f32,
                _ => {}
            }
        }
    }
    const BLOCK: usize = 256;
    let mut pad = Pad::new(1.0, 0x9E37_79B9u32.wrapping_mul(voice as u32 + 1));
    pad.prepare(sample_rate);
    pad.mix(&p);
    pad.reset();
    let mut scratch = Scratch::new(BLOCK);
    pad.trigger(0, &p, 1.0, parts::semitones(value(KIT_TUNE)));
    let limit = (max_seconds * sample_rate) as usize;
    let mut out = Vec::new();
    let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
    while out.len() < limit {
        left.fill(0.0);
        right.fill(0.0);
        if !pad.render(&mut scratch, &mut left, &mut right) {
            break;
        }
        out.extend(left.iter().zip(&right).map(|(l, r)| 0.5 * (l + r)));
    }
    out.truncate(limit);
    out
}
