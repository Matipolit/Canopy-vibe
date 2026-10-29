//! Guitar: a physically modelled six-string, as a steel-string acoustic, a
//! nylon-string classical or a solid-body electric.
//!
//! # The model
//!
//! Each string is a digital waveguide (`guitar/waveguide.rs`), an extended
//! Karplus-Strong loop (Jaffe & Smith 1983) in the single-delay-loop form of
//! Karjalainen, Välimäki & Tolonen 1998: a delay line one period long, read
//! with a Lagrange fractional delay, fed back through a loss filter fitted to
//! the fundamental's and the treble's decay times (Välimäki, Huopaniemi,
//! Karjalainen & Jánosy 1996) and a dispersion allpass for the string's
//! stiffness (Van Duyne & Smith 1994; Rauhala & Välimäki 2006). Every note
//! has two such loops, one per polarization (`guitar/voice.rs`): the one
//! vibrating into the top hands its energy to the body faster, so the sum
//! decays in two stages, and the two are tuned a cent or two apart, so the
//! partials beat (Weinreich 1977). A hard pluck stretches the string and
//! starts a few cents sharp (Tolonen, Välimäki & Karjalainen 2000).
//!
//! A pluck (`guitar/excitation.rs`) is the force pulse of a string let go at
//! the pick, as long as the stretch between bridge and pick, so the pluck
//! position's comb is in it, with edges as sharp as the pick or finger lets
//! go, plus the pick's scrape and tick. Pick or fingers, how hard, and how
//! the stroke divides between the polarizations come from the settings and
//! the velocity; every note varies a little, deterministically, from a seed
//! made of the note, so renders repeat exactly.
//!
//! The strings drive the bridge. The horizontal loops all hear the bridge,
//! which is how idle strings ring along (sympathetic resonance) and how a
//! string's polarizations trade energy; nothing feeds back into the
//! vertical loops, so the coupling is unconditionally stable (Karjalainen,
//! Välimäki & Tolonen 1998). An acoustic's bridge drives its body
//! (`guitar/body.rs`, a modal bank with the measured low modes); an
//! electric's pickups read the strings' velocity where they sit
//! (`guitar/pickup.rs`) through their coils' resonance. An optional amp and
//! cabinet follow (`guitar/amp.rs`), the same one the Amp effect uses.
//!
//! Notes go to strings the way a guitarist would put them there
//! (`guitar/fretboard.rs`); a new note on a ringing string cuts it.
//!
//! # Playing it from MIDI
//!
//! - **Velocity**: how hard the string is picked: louder, brighter (the
//!   pick lets go faster), a little sharper at first; and for a chord, how
//!   fast the strum.
//! - **Chords** (notes within 30 ms of each other) are strummed: down or up
//!   as the strum direction setting says (by default the strumming hand
//!   keeps moving in eighth notes of the song's tempo, down on the beats and
//!   up between), faster when played harder, each string a little early or
//!   late.
//! - **Overlapping notes** on one string are played legato: within two
//!   frets a hammer-on or pull-off, further a slide (the legato setting).
//! - **Pitch wheel**: bends the strings a finger holds (up to two
//!   semitones), not the ones left ringing.
//! - **Mod wheel** (CC 1): vibrato, as deep as the vibrato setting; a
//!   steel-string or electric player's vibrato bends the note up and back,
//!   a classical player's rocks it both ways.
//! - **Soft pedal** (CC 67): palm mute, as far as it is pressed.
//! - **Key-switches** below the guitar's range (with the setting on), each
//!   latched until the next: C0 (MIDI 12) open, C#0 (13) palm mute, D0 (14)
//!   harmonics, D#0 (15) dead notes. The articulation setting does the same.
//! - **Note off**: the finger lifts and damps the string within a quarter
//!   of a second, with a little noise of its own.
//!
//! # Parameters, old and new
//!
//! The twelve parameters version 1 shipped keep their ids and places, so
//! older projects load as they were saved. Three mean a little more now:
//! `pick` (shown as Hardness) was "soft thumb to hard pick" and is now how
//! hard whatever plucks is, a pick or a finger; `body` 0 still means no
//! body, and the model's measured body sits at its default; `sustain` is
//! still the open A's ring time, whose default is now the 10 s real
//! guitars measure rather than 5. A project that never set the new model
//! parameter plays the steel-string, which is closest to the old sound;
//! old electric settings (no body, an amp) play its strings undamped by a
//! body, through the amp, as before. The rest are appended: model,
//! technique, pickup and pickup type, humanize, noises, squeaks,
//! resonance, strum direction, legato, key-switches and articulation.
//!
//! # Meters
//!
//! [`Instrument::meters`] describes the strings for the UI to draw, string
//! `s` being 0 (low E) to 5 (high E):
//!
//! | slots                    | meaning |
//! |--------------------------|---------|
//! | [`METER_NOTE`] `+ s`     | MIDI note the string sounds, -1 while it is silent |
//! | [`METER_LEVEL`] `+ s`    | how hard it vibrates, `0..=1` (a full-velocity pluck starts near 1) |
//! | [`METER_FRET`] `+ s`     | where it is stopped: 0 open, 12 the octave; fractional mid-slide; for a harmonic, the fret it is touched over; below 0 for notes under the low E (the string slackened), above [`MAX_FRET`] past the end of the neck. 0 while silent |
//! | [`METER_HELD`] `+ s`     | 1 while a finger holds the note, 0 once released (it may still ring) |
//! | [`METER_BEND`]           | pitch bend plus vibrato, in semitones, on the strings a finger holds |
//! | [`METER_HAND`]           | fret under the fretting hand's index finger; it covers four frets from there |
//! | [`METER_STRUM`]          | last strum: 1 down (low string first), -1 up, 0 none yet |
//! | [`METER_ARTICULATION`]   | the articulation new notes get: 0 open, 1 palm mute, 2 harmonics, 3 dead notes |
//! | [`METER_MUTE`]           | how far the palm is on the strings, `0..=1` |
//! | [`METER_KIND`] `+ s`     | what the string plays: 0 a picked note, 1 palm-muted, 2 a harmonic, 3 a dead note |
//!
//! The rest are 0.

pub mod amp;
mod body;
mod excitation;
mod fretboard;
mod models;
mod noises;
mod oversample;
mod pickup;
mod string;
mod voice;
mod waveguide;

use crate::{OFF_ON, choice, float};
use std::f32::consts::TAU;
use std::sync::{Arc, Mutex};

use dsp::{Gain, Processor, Smoothed, db_to_gain};
use midi::{MOD_WHEEL, NoteEvent, NoteEventKind, PITCH_BEND_RANGE, Pitch};

pub use fretboard::{MAX_FRET, OPEN_NOTES, STRINGS};
pub use models::{ModelKind, PICKUP_LABELS};

use crate::{BuiltInInstrumentDescriptor, DataKind, Instrument, MAX_METERS, ParamDescriptor};
use amp::Amp;
use body::{Body, unit_hash};
use excitation::{Pluck, omega};
use fretboard::{Fretboard, StringView, fret_on};
use noises::Noises;
use pickup::{Electronics, Taps};
use string::{GuitarString, Pending};
use voice::{NoteKind, NoteStart, VoiceControls};
use waveguide::{Jobs, LN_1000, Loop, dispersion_coefficient, matched_partial};

pub const DESCRIPTOR: BuiltInInstrumentDescriptor = BuiltInInstrumentDescriptor {
    id: "canopy.guitar",
    name: "Guitar",
    description: "Modelled six-string: steel-string acoustic, nylon classical or electric, \
                  with body, pickups and amp",
    version: "2.0",
    author: "canopy",
    params: PARAMS,
    envelopes: &[],
    create: || Box::new(Guitar::new()),
    data: DataKind::None,
    groups: crate::no_groups,
};

/// First of six slots with each string's MIDI note, or -1 while silent.
pub const METER_NOTE: usize = 0;
/// First of six slots with each string's level, `0..=1`.
pub const METER_LEVEL: usize = 6;
/// First of six slots with each string's fret (fractional mid-slide).
pub const METER_FRET: usize = 12;
/// First of six slots: 1 while a string's note is held, else 0.
pub const METER_HELD: usize = 18;
/// Pitch bend plus vibrato, in semitones.
pub const METER_BEND: usize = 24;
/// Fret under the fretting hand's index finger.
pub const METER_HAND: usize = 25;
/// Last strum direction: 1 down, -1 up, 0 none yet.
pub const METER_STRUM: usize = 26;
/// The articulation new notes get (see [`ARTICULATIONS`]).
pub const METER_ARTICULATION: usize = 27;
/// How far the palm is on the strings, `0..=1`.
pub const METER_MUTE: usize = 28;
/// First of six slots with what each string plays: 0 picked, 1 palm-muted,
/// 2 harmonic, 3 dead.
pub const METER_KIND: usize = 29;

/// Samples between control updates (smoothed knobs, vibrato, bends,
/// damping). Retuning a string costs a few transcendental functions, and at
/// 48 kHz this still updates 3000 times a second, too fast to hear steps.
const CONTROL_INTERVAL: usize = 16;

/// Voices across the strings: each string's two and its sympathetic one.
const VOICES: usize = 3 * STRINGS;

const TONE: usize = 0;
const BODY: usize = 1;
const PLUCK_POSITION: usize = 2;
const PICK: usize = 3;
const MUTE: usize = 4;
const SUSTAIN: usize = 5;
const STRUM: usize = 6;
const VIBRATO: usize = 7;
const AMP: usize = 8;
const DRIVE: usize = 9;
const AMP_TONE: usize = 10;
const LEVEL: usize = 11;
const MODEL: usize = 12;
const TECHNIQUE: usize = 13;
const PICKUP: usize = 14;
const PICKUP_TYPE: usize = 15;
const HUMANIZE: usize = 16;
const NOISES: usize = 17;
const SQUEAKS: usize = 18;
const RESONANCE: usize = 19;
const STRUM_DIRECTION: usize = 20;
const LEGATO: usize = 21;
const KEY_SWITCHES: usize = 22;
const ARTICULATION: usize = 23;

/// The articulation setting's labels, also what the key-switches pick.
pub const ARTICULATIONS: &[&str] = &["Open", "Palm mute", "Harmonics", "Dead notes"];
const OPEN: usize = 0;
const PALM: usize = 1;
const HARMONICS: usize = 2;
const DEAD: usize = 3;

/// Key-switch notes: [`KEY_SWITCH_FIRST`] plus an [`ARTICULATIONS`] index.
/// The octave below C1 is reserved for them while key-switches are on.
pub const KEY_SWITCH_FIRST: u8 = 12;
/// The last note of the key-switch octave: notes from [`KEY_SWITCH_FIRST`]
/// to here never sound while key-switches are on.
pub const KEY_SWITCH_LAST: u8 = 23;

/// The soft pedal's controller number: palm mute.
pub const SOFT_PEDAL: u8 = 67;

/// The engine addresses parameters by index, so once shipped this list only
/// grows at the end. Projects store parameters by id.
const PARAMS: &[ParamDescriptor] = &[
    // String brightness: how fast the treble dies compared with the
    // fundamental. Old, dull strings to new, bright ones.
    float("tone", "Tone", 0.0, 1.0, 0.5, "", false),
    // 0 is no body: the strings heard straight from the bridge, as an
    // under-saddle pickup hears them. 0.6 is the model's body as measured;
    // above, larger and boomier. An electric has none.
    float("body", "Body", 0.0, 1.0, 0.6, "", false),
    // 0 picks right by the bridge (thin, twangy), 1 over the neck (round, hollow).
    float(
        "pluck_position",
        "Pluck Position",
        0.0,
        1.0,
        0.35,
        "",
        false,
    ),
    // How hard what plucks is: a thin to a heavy pick, the flesh of a
    // finger to its nail. (Shipped as "soft thumb to hard pick".)
    float("pick", "Hardness", 0.0, 1.0, 0.55, "", false),
    float("mute", "Palm Mute", 0.0, 1.0, 0.0, "", false),
    // How long an open A rings, to -60 dB; other notes scale from it.
    // Real guitars' open A strings ring 9 to 11 s.
    float("sustain", "Sustain", 0.3, 30.0, 10.0, "s", true),
    // Time for the pick to cross all six strings at a medium velocity.
    float("strum", "Strum", 0.0, 250.0, 30.0, "ms", false),
    // Vibrato depth with the mod wheel all the way up.
    float("vibrato", "Vibrato", 0.0, 100.0, 30.0, "cents", false),
    choice("amp", "Amp", amp::MODES, 0),
    float("drive", "Drive", 0.0, 1.0, 0.5, "", false),
    float("amp_tone", "Amp Tone", 0.0, 1.0, 0.5, "", false),
    float("level", "Level", -60.0, 6.0, 0.0, "dB", false),
    choice("model", "Model", ModelKind::LABELS, 0),
    // Pick or fingers; Auto plays a classical with the fingers and the
    // others with a pick.
    choice("technique", "Technique", &["Auto", "Pick", "Fingers"], 0),
    choice("pickup", "Pickup", PICKUP_LABELS, 2),
    choice("pickup_type", "Pickups", &["Single coil", "Humbucker"], 0),
    // How much each note varies: timing in a strum, level, tone, tuning.
    float("humanize", "Humanize", 0.0, 1.0, 0.5, "", false),
    // Fingers lifting off, frets, hands moving.
    float("noises", "Noises", 0.0, 1.0, 0.5, "", false),
    // A fingertip squeaking along the wound strings when the hand moves.
    float("squeaks", "Squeaks", 0.0, 1.0, 0.3, "", false),
    // How much the idle strings ring along with the played ones.
    float("resonance", "Resonance", 0.0, 1.0, 0.5, "", false),
    choice(
        "strum_direction",
        "Strum Direction",
        &["Alternate", "By velocity", "Down", "Up"],
        0,
    ),
    choice(
        "legato",
        "Legato",
        &["Auto", "Hammer-on", "Slide", "Off"],
        0,
    ),
    choice("key_switches", "Key Switches", OFF_ON, 1),
    choice("articulation", "Articulation", ARTICULATIONS, 0),
];

/// The lowest note played at its own pitch, on the low E slackened by two
/// octaves. Lower ones move up by octaves.
const LOWEST_NOTE: u8 = 24;

/// Notes are moved down by octaves until their period is at least this many
/// samples, which the loop's filters and interpolator need.
const SHORTEST_PERIOD: f32 = 5.0;

/// The deepest a string is ever pulled down: a full bend down and a
/// classical vibrato.
const LOWEST_PITCH_RATIO: f32 = 0.84;

/// The tone knob moves the treble decay time this many octaves either way.
const TONE_OCTAVES: f32 = 2.0;

/// Decay times with the palm fully on the strings: the fundamental's, and
/// the treble's. Recorded palm mutes (EG-IPT's) fall 18 to 31 dB in the
/// 200 ms after the attack, with the treble gone first.
const PALM_MUTE_T60: f32 = 0.32;
const PALM_MUTE_TREBLE_T60: f32 = 0.02;

/// How far a key-switched palm mute rests on the strings.
const PALM_SWITCH: f32 = 0.8;

/// A dead note: the fretting hand rests on the string without pressing it.
const DEAD_T60: f32 = 0.06;
const DEAD_TREBLE_T60: f32 = 0.012;

/// Pulse edge times, in seconds, of a thin and a heavy pick, and of a
/// finger's flesh and nail.
const PICK_SOFT_S: f32 = 6e-4;
const PICK_HARD_S: f32 = 3e-5;
const FLESH_S: f32 = 1.3e-3;
const NAIL_S: f32 = 1.6e-4;

/// How much more slowly a pluck slips off a wound string than a plain one.
const WOUND_SOFTNESS: f32 = 2.5;

/// Where a fingertip's flesh rolls off, and a nail's.
const FLESH_HZ: f32 = 1600.0;
const NAIL_HZ: f32 = 6000.0;

/// Pluck position, as a share of the open string from the bridge, at the
/// knob's ends.
const PLUCK_NEAR: f32 = 0.05;
const PLUCK_FAR: f32 = 0.45;

/// How quiet the softest note is, in dB below the loudest.
const VELOCITY_RANGE_DB: f32 = 24.0;

/// Notes this close after others belong to the same chord (a keyboard
/// player's fingers never land quite together).
const CHORD_WINDOW_S: f32 = 0.03;

/// A strumming hand that has rested this many beats starts again with a
/// down-stroke.
const PHRASE_BEATS: f64 = 2.0;

/// Single notes this close together are picked alternately down and up.
const ALTERNATE_PICKING_S: f32 = 0.4;

/// Legato: a note this many semitones from a single held note slides there;
/// within the hammer range, it hammers on (up) or pulls off (down). With
/// the legato setting on hammer-ons or slides, these reaches.
const SLIDE_RANGE: i32 = 7;
const HAMMER_RANGE: i32 = 2;
const HAMMER_REACH: i32 = 5;
const SLIDE_REACH: i32 = 12;
const HAMMER_S: f32 = 0.002;
const SLIDE_S: f32 = 0.02;
const SLIDE_PER_FRET_S: f32 = 0.012;
/// The fretting finger's own pluck, relative to a picked note.
const HAMMER_LEVEL: f32 = 0.3;
const PULL_OFF_LEVEL: f32 = 0.45;

const VIBRATO_HZ: f32 = 5.5;

/// A string quieter than this has faded out (-94 dB), unless the amp or the
/// level knob would bring it back up.
const SILENCE: f32 = 2e-5;

/// Below this the body and amp tails are over and are cleared, so the output
/// is exactly zero again.
const TAIL_SILENCE: f32 = 1e-6;

/// Brings one full-velocity note's attack to about -12 dBFS and a
/// full-velocity six-string strum to about -1 dBFS at the default settings.
const TRIM_DB: f32 = -4.5;

/// The electric's pickups are quieter than an acoustic's bridge; this
/// brings them to a similar level.
const PICKUP_GAIN: f32 = 0.45;

/// The sympathetic coupling's band: the bridge moves little below the
/// body's first modes, and the coupling falls off in the treble.
const COUPLING_LOW_HZ: f32 = 70.0;
const COUPLING_HIGH_HZ: f32 = 2500.0;

/// Most notes one group of events can start at once; more are played in turns.
const MAX_GROUP: usize = 16;

/// Ramp for knob changes that act on ringing strings.
const PARAM_RAMP_MS: f32 = 30.0;
/// Pitch-wheel messages come in steps; this smooths them out.
const BEND_RAMP_MS: f32 = 5.0;
const MOD_WHEEL_RAMP_MS: f32 = 20.0;
/// Switching models fades the guitar out and back in this fast.
const SWITCH_MS: f32 = 10.0;

/// Cents to a frequency ratio, for small ratios.
const CENTS: f32 = std::f32::consts::LN_2 / 1200.0;

fn note_hz(note: u8) -> f32 {
    dsp::note_hz(f32::from(note))
}

/// Level of a pluck at `velocity` (`0..=1`).
fn velocity_gain(velocity: f32) -> f32 {
    db_to_gain(-VELOCITY_RANGE_DB * (1.0 - velocity.clamp(0.0, 1.0)).powf(1.5))
}

/// A well-mixed hash of a few numbers, for each note's variations.
fn hash(parts: &[u32]) -> u32 {
    let mut x = 0x9E37_79B9u32;
    for &part in parts {
        x ^= part.wrapping_mul(0x85EB_CA6B);
        x = x.rotate_left(13).wrapping_mul(5).wrapping_add(0xE654_6B64);
        x ^= x >> 16;
        x = x.wrapping_mul(0x7FEB_352D);
        x ^= x >> 15;
    }
    x
}

/// `-1..1` from a seed and an index.
fn signed(seed: u32, index: u32) -> f32 {
    2.0 * unit_hash(seed, index) - 1.0
}

/// First-order highpass at a few Hz. The force a pluck puts on the bridge
/// has a DC part, which the body ignores but the speaker shouldn't get.
#[derive(Debug, Clone, Default)]
struct DcBlocker {
    coefficient: f32,
    x1: f32,
    y1: f32,
}

impl DcBlocker {
    const HZ: f32 = 12.0;

    fn prepare(&mut self, sample_rate: f32) {
        self.coefficient = 1.0 - TAU * Self::HZ / sample_rate;
        self.reset();
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.coefficient * self.y1;
        self.x1 = x;
        self.y1 = if y.abs() < 1e-20 { 0.0 } else { y };
        y
    }
}

/// The strum under way: notes of its chord arriving late join it in order.
#[derive(Debug, Clone, Copy)]
struct Strum {
    /// When the pick met the first string, in samples since `reset`.
    start: u64,
    /// The string it started on.
    first: usize,
    /// 1 down, -1 up.
    direction: f32,
    /// Samples from one string to the next.
    per_string: f32,
}

/// The dispersion coefficient of every note on every string of every
/// model.
type Dispersion = [[[f32; 128]; STRINGS]; 3];

/// [`Dispersion`] at `sample_rate`. The same for every guitar, and a few
/// milliseconds' work (a search per note), so it is worked out once per
/// rate and shared.
fn dispersion(sample_rate: f32) -> Arc<Dispersion> {
    static TABLES: Mutex<Vec<(u32, Arc<Dispersion>)>> = Mutex::new(Vec::new());
    let mut tables = TABLES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, table)) = tables
        .iter()
        .find(|(rate, _)| *rate == sample_rate.to_bits())
    {
        return Arc::clone(table);
    }
    let mut table: Box<Dispersion> = Box::new([[[0.0; 128]; STRINGS]; 3]);
    for (model, table) in table.iter_mut().enumerate() {
        let spec = ModelKind::from_index(model).spec();
        for (string, notes) in table.iter_mut().enumerate() {
            let open = i32::from(OPEN_NOTES[string]);
            for (note, coefficient) in notes.iter_mut().enumerate() {
                let fret = (note as i32 - open).max(0);
                let b = spec.strings[string].inharmonicity * (fret as f32 / 6.0).exp2();
                *coefficient = dispersion_coefficient(b, note_hz(note as u8), sample_rate);
            }
        }
    }
    let table: Arc<Dispersion> = Arc::from(table);
    tables.push((sample_rate.to_bits(), Arc::clone(&table)));
    table
}

/// A modelled six-string guitar. See the module documentation.
pub struct Guitar {
    sample_rate: f32,
    strings: [GuitarString; STRINGS],
    /// Fills the lanes the strings' loops leave free when they run side by
    /// side (see `waveguide::run_together`).
    pad: Loop,
    fretboard: Fretboard,
    body: Body,
    electronics: Electronics,
    amp: Amp,
    noises: Noises,
    dc: [DcBlocker; 2],
    gain: Gain,
    level_gain: f32,
    /// What the body hears from the strings, and what the pickups hear.
    bridge: Vec<f32>,
    pickups: Vec<f32>,
    /// The coupling's band-pass, as a one-pole highpass and lowpass.
    coupling_high: f32,
    coupling_low: f32,
    coupling_state: [f32; 2],
    tone: Smoothed,
    /// In log2 seconds, so a sweep sounds even.
    sustain: Smoothed,
    mute: Smoothed,
    /// In semitones.
    bend: Smoothed,
    mod_wheel: Smoothed,
    /// Depth at full mod wheel, in cents.
    vibrato_cents: Smoothed,
    /// In cycles, `0..1`.
    vibrato_phase: f32,
    /// Bend plus vibrato last control interval, in semitones.
    wheel: f32,
    pluck_position: f32,
    hardness: f32,
    strum_ms: f32,
    humanize: f32,
    noise_level: f32,
    squeaks: f32,
    resonance: f32,
    mute_param: f32,
    mute_pedal: f32,
    model: ModelKind,
    /// A model asked for while sounding, applied once faded out.
    next_model: Option<ModelKind>,
    fade: Smoothed,
    technique: usize,
    pickup_switch: usize,
    humbuckers: bool,
    strum_direction: usize,
    legato: usize,
    key_switches: bool,
    articulation: usize,
    /// How each string sounds while it only rings along, per model.
    open_notes: [NoteStart; STRINGS],
    /// Dispersion for every note on every string of every model, looked up
    /// in `prepare` so a pluck costs nothing extra.
    dispersion: Arc<Dispersion>,
    controls: VoiceControls,
    /// The losses' settings moved and the strings must refit their filters.
    losses_moved: bool,
    /// Pickups switched: the sounding notes move to them.
    retap: bool,
    /// Samples until the next control update. Carries across blocks and
    /// event splits so updates stay evenly spaced.
    control_countdown: usize,
    /// Samples processed since `reset`, to time chords and strums.
    clock: u64,
    /// When the latest notes started.
    last_notes: Option<u64>,
    chord_window: u64,
    strum: Option<Strum>,
    /// The strumming hand's phrase: when it began, and whether it moves in
    /// sixteenths rather than eighths.
    phrase: Option<(u64, bool)>,
    last_strum_time: u64,
    /// The last single note: when, which way it was picked, and on which
    /// string.
    last_single: Option<(u64, f32, usize)>,
    bpm: f64,
    last_strum: f32,
    /// The body or amp is still ringing after the strings stopped.
    ringing: bool,
    meters: [f32; MAX_METERS],
}

impl Guitar {
    pub fn new() -> Self {
        let mut guitar = Self {
            sample_rate: 48_000.0,
            strings: std::array::from_fn(GuitarString::new),
            pad: Loop::new(),
            fretboard: Fretboard::new(),
            body: Body::new(PARAMS[BODY].default_value()),
            electronics: Electronics::default(),
            amp: Amp::new(),
            noises: Noises::new(),
            dc: [DcBlocker::default(), DcBlocker::default()],
            gain: Gain::new(TRIM_DB),
            level_gain: 1.0,
            bridge: Vec::new(),
            pickups: Vec::new(),
            coupling_high: 0.0,
            coupling_low: 0.0,
            coupling_state: [0.0; 2],
            tone: Smoothed::new(0.5, PARAM_RAMP_MS),
            sustain: Smoothed::new(0.0, PARAM_RAMP_MS),
            mute: Smoothed::new(0.0, PARAM_RAMP_MS),
            bend: Smoothed::new(0.0, BEND_RAMP_MS),
            mod_wheel: Smoothed::new(0.0, MOD_WHEEL_RAMP_MS),
            vibrato_cents: Smoothed::new(0.0, PARAM_RAMP_MS),
            vibrato_phase: 0.0,
            wheel: 0.0,
            pluck_position: 0.0,
            hardness: 0.0,
            strum_ms: 0.0,
            humanize: 0.0,
            noise_level: 0.0,
            squeaks: 0.0,
            resonance: 0.0,
            mute_param: 0.0,
            mute_pedal: 0.0,
            model: ModelKind::Steel,
            next_model: None,
            fade: Smoothed::new(1.0, SWITCH_MS),
            technique: 0,
            pickup_switch: 2,
            humbuckers: false,
            strum_direction: 0,
            legato: 0,
            key_switches: true,
            articulation: OPEN,
            open_notes: [NoteStart {
                hz: 110.0,
                partial: 1.0,
                split: 1.0,
                decay: 1.0,
                treble: 1.0,
                tension: 0.0,
                dispersion: [0.0; 2],
                shares: [0.0; 2],
                horizontal_gain: 0.0,
                kind: NoteKind::Sympathetic,
                taps: Taps::default(),
                length: 0.65,
            }; STRINGS],
            dispersion: Arc::new([[[0.0; 128]; STRINGS]; 3]),
            controls: VoiceControls {
                sample_rate: 48_000.0,
                sustain: 5.0,
                treble_scale: 1.0,
                mute_low: 0.0,
                mute_high: 0.0,
                vertical_loss: [1.0; 2],
                bend: 0.0,
                losses_moved: false,
            },
            losses_moved: false,
            retap: false,
            control_countdown: 0,
            clock: 0,
            last_notes: None,
            chord_window: 0,
            strum: None,
            phrase: None,
            last_strum_time: 0,
            last_single: None,
            bpm: 120.0,
            last_strum: 0.0,
            ringing: false,
            meters: [0.0; MAX_METERS],
        };
        for (index, param) in PARAMS.iter().enumerate() {
            guitar.set_param(index, param.default_value());
        }
        // Usable before the host prepares it, at a common rate.
        guitar.prepare(48_000.0, 512);
        guitar
    }

    fn spec(&self) -> &'static models::Model {
        self.model.spec()
    }

    fn electric(&self) -> bool {
        self.spec().pickups.is_some()
    }

    /// Moves a note by octaves until the strings can play it.
    fn playable(&self, note: u8) -> u8 {
        let mut note = note;
        while note < LOWEST_NOTE {
            note += 12;
        }
        while note >= 12 && note_hz(note) * SHORTEST_PERIOD > self.sample_rate {
            note -= 12;
        }
        note
    }

    fn views(&self) -> [StringView; STRINGS] {
        std::array::from_fn(|i| self.strings[i].view())
    }

    fn uses_fingers(&self) -> bool {
        match self.technique {
            1 => false,
            2 => true,
            _ => self.spec().fingers,
        }
    }

    /// The palm mute the strings feel: the knob, the pedal or the
    /// key-switch, whichever is furthest.
    fn update_mute(&mut self) {
        let switched = if self.articulation == PALM {
            PALM_SWITCH
        } else {
            0.0
        };
        let target = self.mute_param.max(self.mute_pedal).max(switched);
        if target != self.mute.target() {
            self.mute.set_target(target);
        }
    }

    fn set_articulation(&mut self, articulation: usize) {
        self.articulation = articulation.min(ARTICULATIONS.len() - 1);
        self.update_mute();
    }

    fn handle_group(&mut self, events: &[NoteEvent], time: u64) {
        let mut ons = [(Pitch::MIN, 0.0f32); MAX_GROUP];
        let mut count = 0;
        for event in events {
            match event.kind {
                NoteEventKind::On { pitch, velocity } => {
                    if self.key_switches
                        && (KEY_SWITCH_FIRST..=KEY_SWITCH_LAST).contains(&pitch.get())
                    {
                        let index = usize::from(pitch.get() - KEY_SWITCH_FIRST);
                        if index < ARTICULATIONS.len() {
                            self.set_articulation(index);
                        }
                        continue;
                    }
                    if count == MAX_GROUP {
                        self.play(&ons, time);
                        count = 0;
                    }
                    if !ons[..count].iter().any(|(p, _)| *p == pitch) {
                        ons[count] = (pitch, velocity.as_f32());
                        count += 1;
                    }
                }
                NoteEventKind::Off { pitch, .. } => {
                    if self.key_switches
                        && (KEY_SWITCH_FIRST..=KEY_SWITCH_LAST).contains(&pitch.get())
                    {
                        continue;
                    }
                    for index in 0..STRINGS {
                        if let Some(released) = self.strings[index].note_off(pitch) {
                            self.release_noise(index, released.fret, released.level);
                        }
                    }
                    self.hands_off();
                }
                NoteEventKind::AllNotesOff => {
                    for string in &mut self.strings {
                        string.release_all();
                    }
                    self.hands_off();
                }
                NoteEventKind::PitchBend(value) => {
                    let semitones = f32::from(value.clamp(-8192, 8191)) / 8192.0 * PITCH_BEND_RANGE;
                    self.bend.set_target(semitones);
                }
                NoteEventKind::Control { controller, value } => {
                    let value = f32::from(value.min(127)) / 127.0;
                    if controller == MOD_WHEEL {
                        self.mod_wheel.set_target(value);
                    } else if controller == SOFT_PEDAL {
                        self.mute_pedal = value;
                        self.update_mute();
                    }
                }
            }
        }
        if count > 0 {
            self.play(&ons[..count], time);
        }
    }

    /// With every note let go, the hands rest on the strings: the idle
    /// ones stop ringing along too.
    fn hands_off(&mut self) {
        let playing = self.strings.iter().any(|s| s.is_held() || s.has_pending());
        if !playing {
            for string in &mut self.strings {
                string.damp_sympathetic();
            }
        }
    }

    /// The direction of a strum starting at `time` with `velocity`.
    fn strum_stroke(&mut self, time: u64, velocity: f32) -> f32 {
        let samples_per_beat = 60.0 / self.bpm * f64::from(self.sample_rate);
        let stroke = match self.strum_direction {
            1 => {
                if velocity >= 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            2 => 1.0,
            3 => -1.0,
            _ => {
                // The hand keeps moving in eighths: down on the beats and
                // eighths, up between; in sixteenths once a strum falls
                // between eighths.
                let rested = time.saturating_sub(self.last_strum_time) as f64
                    > PHRASE_BEATS * samples_per_beat;
                let (start, mut sixteenths) = match self.phrase {
                    Some(phrase) if !rested => phrase,
                    _ => (time, false),
                };
                let eighths = (time - start) as f64 / (samples_per_beat / 2.0);
                if (eighths - eighths.round()).abs() > 0.25 {
                    sixteenths = true;
                }
                self.phrase = Some((start, sixteenths));
                let step = if sixteenths {
                    (eighths * 2.0).round()
                } else {
                    eighths.round()
                };
                if step as i64 % 2 == 0 { 1.0 } else { -1.0 }
            }
        };
        self.last_strum_time = time;
        stroke
    }

    /// Starts `notes` (keys and velocities), which arrived together at `time`.
    fn play(&mut self, notes: &[(Pitch, f32)], time: u64) {
        let continues_chord = self
            .last_notes
            .is_some_and(|last| time.saturating_sub(last) < self.chord_window);
        self.last_notes = Some(time);

        // Low to high by the note each will play.
        let mut group = [(Pitch::MIN, 0u8, 0.0f32); MAX_GROUP];
        for (slot, &(key, velocity)) in group.iter_mut().zip(notes) {
            *slot = (key, self.playable(key.get()), velocity);
        }
        let count = notes.len();
        let group = &mut group[..count];
        group.sort_unstable_by_key(|&(_, played, _)| played);

        let articulation = self.articulation;
        if let [(key, played, velocity)] = *group
            && !continues_chord
            && articulation != HARMONICS
            && articulation != DEAD
            && self.try_legato(key, played, velocity)
        {
            return;
        }

        // Which string, which fret, and what kind of note each will be.
        let hand_before = self.fretboard.hand();
        let views = self.views();
        let mut placed = [(0usize, 0i32, 1u8, 0i32); MAX_GROUP];
        if articulation == HARMONICS {
            let busy: [bool; STRINGS] = std::array::from_fn(|s| views[s].busy);
            let mut used = [false; STRINGS];
            for (slot, &(_, note, _)) in placed.iter_mut().zip(group.iter()) {
                if let Some(h) = fretboard::harmonic(note, self.fretboard.hand(), &used, &busy) {
                    used[h.string] = true;
                    *slot = (h.string, h.fret, h.partial, h.touch);
                    self.fretboard.follow(h.fret);
                } else {
                    let mut out = [0];
                    self.fretboard.assign(&[note], &views, true, &mut out);
                    let fret = fret_on(note, out[0]).unwrap_or(0);
                    used[out[0]] = true;
                    *slot = (out[0], fret, 1, fret);
                }
            }
        } else {
            let mut played = [0u8; MAX_GROUP];
            for (slot, &(_, note, _)) in played.iter_mut().zip(group.iter()) {
                *slot = note;
            }
            let mut strings = [0usize; MAX_GROUP];
            self.fretboard.assign(
                &played[..count],
                &views,
                continues_chord,
                &mut strings[..count],
            );
            for ((slot, &string), &note) in placed.iter_mut().zip(&strings).zip(&played) {
                let fret = fret_on(note, string).unwrap_or(0);
                *slot = (string, fret, 1, fret);
            }
        }
        let kind = match articulation {
            HARMONICS => NoteKind::Harmonic,
            DEAD => NoteKind::Dead,
            _ if self.mute.target() >= 0.3 => NoteKind::PalmMuted,
            _ => NoteKind::Picked,
        };

        // The fretting hand moved a long way: the fingertips may squeak
        // along the wound strings on the way.
        let moved = (self.fretboard.hand() - hand_before).abs();
        if moved >= 3 {
            self.shift_noise(&placed[..count], moved, time);
        }

        // A strum sweeps across the strings, faster when played harder, so
        // a string's delay is its distance from the first string the pick
        // meets, give or take the hand's unevenness.
        let velocity = group.iter().map(|&(_, _, v)| v).sum::<f32>() / count as f32;
        let lowest = placed[..count].iter().map(|p| p.0).min().unwrap_or(0);
        let highest = placed[..count].iter().map(|p| p.0).max().unwrap_or(0);
        let speed = 1.6 - velocity;
        let per_string = self.strum_ms * 0.001 * self.sample_rate / (STRINGS - 1) as f32 * speed;
        let strum = if continues_chord {
            // A note a moment after a single one makes the two a strum,
            // started when the first was picked.
            let first = self.last_single.filter(|_| self.strum.is_none());
            match first {
                Some((start, _, first)) => {
                    let direction = if lowest >= first { 1.0 } else { -1.0 };
                    self.last_strum = direction;
                    let strum = Strum {
                        start,
                        first,
                        direction,
                        per_string,
                    };
                    self.strum = Some(strum);
                    self.last_single = None;
                    Some(strum)
                }
                None => self.strum,
            }
        } else if count > 1 {
            let direction = self.strum_stroke(time, velocity);
            self.last_strum = direction;
            let strum = Strum {
                start: time,
                first: if direction > 0.0 { lowest } else { highest },
                direction,
                per_string,
            };
            self.strum = Some(strum);
            Some(strum)
        } else {
            self.strum = None;
            None
        };
        // Single notes in quick succession are picked down and up in turn.
        let single_stroke = if strum.is_none() {
            let stroke = match self.last_single {
                Some((last, stroke, _))
                    if (time.saturating_sub(last) as f32)
                        < ALTERNATE_PICKING_S * self.sample_rate =>
                {
                    -stroke
                }
                _ => 1.0,
            };
            self.last_single = Some((time, stroke, placed[0].0));
            stroke
        } else {
            1.0
        };

        for (&(key, played, velocity), &(string, fret, partial, touch)) in
            group.iter().zip(&placed[..count])
        {
            let seed = hash(&[
                u32::from(played),
                string as u32,
                self.strings[string].plucks(),
                (velocity * 1000.0) as u32,
            ]);
            let (wait, stroke, velocity) = match strum {
                Some(strum) => {
                    let crossed = (string as f32 - strum.first as f32) * strum.direction;
                    let jitter = self.humanize * 0.35 * strum.per_string * signed(seed, 11);
                    let due = strum.start as f32 + crossed.max(0.0) * strum.per_string + jitter;
                    let wait = (due - time as f32).max(0.0).round() as usize;
                    // The pick loses a little force as it goes; up-strokes
                    // barely catch the lowest strings.
                    let mut level = 1.0 - 0.12 * crossed.max(0.0) / (STRINGS - 1) as f32;
                    if strum.direction < 0.0 && string < 2 {
                        level *= 0.6;
                    }
                    level *= 1.0 + 0.07 * self.humanize * signed(seed, 12);
                    (wait, strum.direction, (velocity * level).clamp(0.0, 1.0))
                }
                None => (0, single_stroke, velocity),
            };
            self.strings[string].schedule(Pending {
                key,
                played,
                velocity,
                wait,
                release: false,
                kind,
                fret,
                partial,
                touch,
                stroke,
            });
        }
    }

    /// Plays `played` as a slide, hammer-on or pull-off from the only held
    /// note, if that is how a guitarist would play it. Returns whether it did.
    fn try_legato(&mut self, key: Pitch, played: u8, velocity: f32) -> bool {
        if self.legato == 3 {
            return false;
        }
        let mut held = None;
        for (index, string) in self.strings.iter().enumerate() {
            if string.has_pending() {
                return false;
            }
            if string.is_held() && held.replace(index).is_some() {
                return false;
            }
        }
        let Some(index) = held else {
            return false;
        };
        let interval = i32::from(played) - i32::from(self.strings[index].played());
        let (hammer, slide) = match self.legato {
            1 => (HAMMER_REACH, 0),
            2 => (0, SLIDE_REACH),
            _ => (HAMMER_RANGE, SLIDE_RANGE),
        };
        if interval == 0 || interval.abs() > hammer.max(slide) {
            return false;
        }
        let Some(fret) = fret_on(played, index).filter(|f| (0..=MAX_FRET).contains(f)) else {
            return false;
        };
        let hz = self.fretted_hz(index, fret, 0.0);
        let taps = self.taps(index, fret, hz);
        let wound = self.spec().strings[index].wound;
        let seed = hash(&[u32::from(played), index as u32, self.clock as u32]);
        if interval.abs() <= hammer {
            let stroke = if interval > 0 { 1.0 } else { -1.0 };
            let mut pluck = self.pluck_for(index, fret, velocity, stroke, NoteKind::Picked, seed);
            pluck.amplitude *= if interval > 0 {
                HAMMER_LEVEL
            } else {
                PULL_OFF_LEVEL
            };
            // Fingertips are soft, and don't scrape or tick like a pick.
            pluck.width = (pluck.width * 3.0).min(pluck.length);
            pluck.scrape *= 0.2;
            pluck.click = 0.0;
            let intervals = self.intervals(HAMMER_S);
            let length = self.length(fret);
            self.strings[index].legato(
                key,
                played,
                fret,
                hz,
                intervals,
                false,
                taps,
                length,
                Some(&pluck),
            );
            // The string slaps onto the fret.
            if interval > 0 {
                let level = 0.01 * self.noise_level * velocity_gain(velocity);
                self.noises.burst(level, 2500.0, 0.008, seed);
            }
        } else {
            let seconds = SLIDE_S + SLIDE_PER_FRET_S * interval.abs() as f32;
            let intervals = self.intervals(seconds);
            let length = self.length(fret);
            self.strings[index].legato(key, played, fret, hz, intervals, true, taps, length, None);
            if wound {
                let level = 0.012 * self.noise_level * velocity_gain(velocity);
                let speed = interval.abs() as f32 / seconds;
                self.noises
                    .squeak(level, 120.0 * speed, ring_hz(index), seconds, seed);
            }
        }
        self.fretboard.follow(fret);
        true
    }

    /// Control intervals in `seconds`, at least one.
    fn intervals(&self, seconds: f32) -> usize {
        ((seconds * self.sample_rate / CONTROL_INTERVAL as f32).round() as usize).max(1)
    }

    /// The frequency of `string` stopped at `fret`, pulled sharp by the
    /// fretting and detuned by `detune` cents.
    fn fretted_hz(&self, string: usize, fret: i32, detune: f32) -> f32 {
        let model = self.spec();
        let note = i32::from(OPEN_NOTES[string]) + fret;
        let sharpening = model.fret_sharpening * (fret.clamp(0, 12) as f32 / 12.0);
        dsp::note_hz(note as f32) * (1.0 + (sharpening + detune) * CENTS)
    }

    /// The string's vibrating length when stopped at `fret`, metres.
    fn length(&self, fret: i32) -> f32 {
        self.spec().scale * (-(fret.max(0) as f32) / 12.0).exp2()
    }

    /// Where the pickups hear a note on `string` at `fret` (`hz`); none on
    /// an acoustic.
    fn taps(&self, string: usize, fret: i32, hz: f32) -> Taps {
        let _ = string;
        if !self.electric() {
            return Taps::default();
        }
        let spec = models::pickups(self.humbuckers);
        pickup::taps(spec, self.pickup_switch, self.length(fret), hz)
    }

    /// Everything about a new note on `string` at `fret` sounding partial
    /// `partial`, from the model, the settings and `seed`.
    fn note_start(
        &self,
        string: usize,
        fret: i32,
        partial: u8,
        kind: NoteKind,
        velocity: f32,
        seed: u32,
    ) -> NoteStart {
        let model = self.spec();
        let spec = model.strings[string];
        let detune = 1.2 * self.humanize * signed(seed, 1);
        let hz = self.fretted_hz(string, fret, detune);
        let n = f32::from(partial.max(1));
        let open = note_hz(OPEN_NOTES[string]);
        let mut decay = spec.decay
            * (open / hz).powf(model.fret_decay)
            * self.body.dead_spot(hz * n)
            * (1.0 + 0.08 * self.humanize * signed(seed, 2));
        if partial > 1 {
            decay /= n.sqrt();
        }
        // The two polarizations' tuning is the string's and the bridge's,
        // not the player's: it depends only on where the note is.
        let place = hash(&[string as u32, fret as u32, self.model as u32]);
        let split_cents = model.split_cents * (0.5 + unit_hash(place, 3));
        let note = (i32::from(OPEN_NOTES[string]) + fret).clamp(0, 127) as usize;
        let b = spec.inharmonicity * (fret.max(0) as f32 / 6.0).exp2() * n * n;
        let vertical = if partial > 1 {
            dispersion_coefficient(b, hz * n, self.sample_rate)
        } else {
            self.dispersion[self.model as usize][string][note]
        };
        // The horizontal plane's partials sit a constant few hundredths of
        // a hertz above the vertical's, not a constant ratio: up to the
        // partial the stiffness is matched at, that is a little less
        // stiffness for the ratio its whole loop is tuned up by.
        let split = split_cents * CENTS;
        let matched = matched_partial(hz * n);
        let horizontal_b = (b - 1.2 * split * (1.0 - 1.0 / matched) / (matched * matched)).max(0.0);
        let horizontal = if vertical == 0.0 {
            0.0
        } else {
            dispersion_coefficient(horizontal_b, hz * n * (1.0 + 0.5 * split), self.sample_rate)
        };
        let angle = if self.uses_fingers() {
            model.finger_angle
        } else {
            model.pick_angle
        } + 0.3 * self.humanize * signed(seed, 4);
        let energy = velocity_gain(velocity).powi(2);
        let tension = model.tension_cents
            * energy
            * match kind {
                NoteKind::PalmMuted | NoteKind::Dead => 0.4,
                NoteKind::Harmonic => 0.3,
                _ => 1.0,
            };
        NoteStart {
            hz,
            partial: n,
            split: 1.0 + split_cents * CENTS,
            decay,
            treble: spec.treble,
            tension,
            dispersion: [vertical, horizontal],
            shares: [angle.sin(), angle.cos()],
            horizontal_gain: model.horizontal_gain,
            kind,
            taps: self.taps(string, fret, hz),
            length: self.length(fret),
        }
    }

    /// How each open string sounds while it only rings along.
    fn update_open_notes(&mut self) {
        for (string, &note) in OPEN_NOTES.iter().enumerate() {
            let mut open = self.note_start(string, 0, 1, NoteKind::Sympathetic, 0.0, 0);
            open.hz = note_hz(note);
            open.tension = 0.0;
            open.dispersion = [0.0; 2];
            open.shares = [0.0; 2];
            self.open_notes[string] = open;
        }
    }

    /// The excitation for a note on `string` stopped at `fret`, at
    /// `velocity`, picked in direction `stroke`, from the current
    /// technique, position and mute settings.
    fn pluck_for(
        &self,
        string: usize,
        fret: i32,
        velocity: f32,
        stroke: f32,
        kind: NoteKind,
        seed: u32,
    ) -> Pluck {
        let rate = self.sample_rate;
        let wound = self.spec().strings[string].wound;
        let fingers = self.uses_fingers();
        let hz = self.fretted_hz(string, fret, 0.0);
        let period = rate / hz;
        // The pick stays put while the fretted string gets shorter, so it is
        // relatively further from the bridge on higher frets.
        let position = (PLUCK_NEAR + (PLUCK_FAR - PLUCK_NEAR) * self.pluck_position)
            * (1.0 + 0.12 * self.humanize * signed(seed, 5))
            * (fret.max(0) as f32 / 12.0).exp2();
        let length = (position.clamp(0.02, 0.5) * period).max(1.0);
        let mute = self.mute.current();
        let hardness = (self.hardness + 0.1 * self.humanize * signed(seed, 6)).clamp(0.0, 1.0);
        // Harder playing lets go faster: a brighter attack. The palm on the
        // strings softens it.
        let softness = (1.7 - 0.9 * velocity)
            * (1.0 + 1.5 * mute)
            * match kind {
                NoteKind::Harmonic => 1.5,
                NoteKind::Dead => 1.3,
                _ => 1.0,
            };
        let (soft, hard) = if fingers {
            (FLESH_S, NAIL_S)
        } else {
            (PICK_SOFT_S, PICK_HARD_S)
        };
        // A wound string slips off more slowly and keeps less treble: its
        // windings rub and its mass rounds the corner the pick leaves
        // (recordings show wound strings 10 to 25 dB darker above 2 kHz
        // than an ideal pluck would leave them).
        let wound_softness = if wound { WOUND_SOFTNESS } else { 1.0 };
        let width_s = soft * (hard / soft).powf(hardness) * softness * wound_softness;
        let flesh = if fingers {
            let hz = FLESH_HZ * (NAIL_HZ / FLESH_HZ).powf(hardness) * (0.6 + 0.6 * velocity)
                / wound_softness;
            (-TAU * hz / rate).exp()
        } else {
            0.0
        };
        let level = velocity_gain(velocity)
            * (1.0 - 0.25 * mute)
            * (1.0 + 0.1 * self.humanize * signed(seed, 7))
            * match kind {
                NoteKind::Harmonic => 0.7,
                NoteKind::Dead => 0.8,
                _ => 1.0,
            };
        let dead = if kind == NoteKind::Dead { 1.6 } else { 1.0 };
        let (scrape, scrape_hz, click, click_hz) = if fingers {
            (
                0.012 * hardness * dead * if wound { 2.0 } else { 1.0 },
                3200.0,
                0.035 * hardness * hardness,
                5500.0,
            )
        } else {
            (
                (0.035 + 0.1 * hardness)
                    * (0.4 + 0.6 * velocity)
                    * dead
                    * if wound { 2.2 } else { 1.0 },
                if wound { 1800.0 } else { 3600.0 },
                (0.015 + 0.05 * hardness) * (0.3 + 0.7 * velocity),
                3000.0 + 2500.0 * hardness,
            )
        };
        Pluck {
            amplitude: level * stroke,
            length,
            width: (width_s * rate).clamp(1.0, length),
            flesh,
            scrape,
            scrape_time: 0.0025 * rate,
            scrape_w: omega(scrape_hz, rate),
            click,
            click_w: omega(click_hz, rate),
            click_time: 0.0004 * rate,
            seed,
        }
    }

    /// The thump of a finger lifting off a note on `string` at `fret`
    /// that was ringing at `level`.
    fn release_noise(&mut self, string: usize, fret: i32, level: f32) {
        let amount = 0.04 * self.noise_level * level.min(1.0);
        if amount <= 1e-5 {
            return;
        }
        let seed = hash(&[string as u32, fret as u32, self.clock as u32]);
        let hz = if fret > 0 { 900.0 } else { 500.0 };
        self.noises.burst(amount, hz, 0.04, seed);
        // A fingertip leaving a wound string sometimes squeaks a little.
        let squeaky = self.spec().strings[string].wound && fret > 0;
        if squeaky && unit_hash(seed, 1) < 0.35 * self.squeaks {
            self.noises
                .squeak(0.3 * amount, 900.0, ring_hz(string), 0.05, seed ^ 0x55);
        }
    }

    /// The hand moved `frets` to play `placed`: a squeak along a wound
    /// string it lands on, now and then.
    fn shift_noise(&mut self, placed: &[(usize, i32, u8, i32)], frets: i32, time: u64) {
        let Some(&(string, _, _, _)) = placed
            .iter()
            .filter(|p| self.spec().strings[p.0].wound && p.1 > 0)
            .min_by_key(|p| p.0)
        else {
            return;
        };
        let seed = hash(&[string as u32, frets as u32, time as u32]);
        if unit_hash(seed, 0) > 0.25 + 0.75 * self.squeaks {
            return;
        }
        let level = 0.02 * self.squeaks * (frets as f32 / 7.0).min(1.0);
        let seconds = 0.06 + 0.012 * frets as f32;
        // Bumping over the windings: the faster the hand, the higher.
        let speed = frets as f32 / seconds;
        self.noises
            .squeak(level, 100.0 * speed, ring_hz(string), seconds, seed);
    }

    /// Plucks every string whose turn in a strum has come.
    fn fire_due(&mut self) {
        for index in 0..STRINGS {
            let Some(pending) = self.strings[index].take_due() else {
                continue;
            };
            let seed = hash(&[
                u32::from(pending.played),
                index as u32,
                self.strings[index].plucks(),
                (pending.velocity * 1000.0) as u32,
            ]);
            let note = self.note_start(
                index,
                pending.fret,
                pending.partial,
                pending.kind,
                pending.velocity,
                seed,
            );
            let pluck = self.pluck_for(
                index,
                pending.fret,
                pending.velocity,
                pending.stroke,
                pending.kind,
                seed,
            );
            let dead = (pending.kind == NoteKind::Dead)
                .then_some([LN_1000 / DEAD_T60, LN_1000 / DEAD_TREBLE_T60]);
            let controls = VoiceControls {
                bend: self.wheel,
                ..self.controls
            };
            for string in &mut self.strings {
                string.free_sympathetic();
            }
            let string = &mut self.strings[index];
            string.pluck(&pending, &note, &pluck, &controls, dead);
            if pending.release {
                string.release();
            }
        }
    }

    fn read_controls(&mut self) {
        let model = self.spec();
        let tone = self.tone.current();
        let mute = self.mute.current();
        self.controls = VoiceControls {
            sample_rate: self.sample_rate,
            sustain: self.sustain.current().exp2(),
            treble_scale: ((tone - 0.5) * 2.0 * TONE_OCTAVES).exp2(),
            mute_low: mute.powf(1.5) * LN_1000 / PALM_MUTE_T60,
            mute_high: mute * LN_1000 / PALM_MUTE_TREBLE_T60,
            vertical_loss: model.vertical_loss,
            bend: self.wheel,
            losses_moved: false,
        };
    }

    fn silence(&self) -> f32 {
        SILENCE / (self.amp.quiet_gain() * self.level_gain).max(1.0)
    }

    fn smoothers(&mut self) -> [&mut Smoothed; 6] {
        [
            &mut self.tone,
            &mut self.sustain,
            &mut self.mute,
            &mut self.bend,
            &mut self.mod_wheel,
            &mut self.vibrato_cents,
        ]
    }

    fn control_tick(&mut self) {
        let losses_moved = self.losses_moved
            || self.tone.is_smoothing()
            || self.sustain.is_smoothing()
            || self.mute.is_smoothing();
        self.losses_moved = false;
        for smoothed in self.smoothers() {
            smoothed.tick();
        }
        let step = CONTROL_INTERVAL as f32 / self.sample_rate;
        self.vibrato_phase = (self.vibrato_phase + VIBRATO_HZ * step).fract();
        let depth = self.mod_wheel.current() * self.vibrato_cents.current() / 100.0;
        // A steel-string or electric player's vibrato bends the note up and
        // back; a classical player's rocks it both ways.
        let swing = if self.model == ModelKind::Nylon {
            0.5 * (TAU * self.vibrato_phase).sin()
        } else {
            0.5 - 0.5 * (TAU * self.vibrato_phase).cos()
        };
        self.wheel = self.bend.current() + depth * swing;
        self.read_controls();
        self.controls.losses_moved = losses_moved;
        let armed = self.resonance > 0.0 && self.strings.iter().any(GuitarString::is_active);
        if self.retap {
            self.retap = false;
            self.update_open_notes();
            if self.electric() {
                let (spec, switch) = (models::pickups(self.humbuckers), self.pickup_switch);
                let taps = move |length: f32, hz: f32| pickup::taps(spec, switch, length, hz);
                for string in &mut self.strings {
                    string.retap(taps);
                }
            }
        }
        for (string, open) in self.strings.iter_mut().zip(&self.open_notes) {
            string.control_tick(&self.controls, self.wheel, armed, open);
        }
    }

    /// Nothing is sounding: jump the controls to their targets, so the next
    /// note starts at the settings as they are now.
    fn settle(&mut self) {
        for smoothed in self.smoothers() {
            smoothed.settle();
        }
        self.wheel = self.bend.current();
        self.read_controls();
        self.control_countdown = 0;
    }

    /// Renders the strings into the bridge and pickup buffers, from `start`
    /// to `end`, plucking strummed strings on time. Returns whether any
    /// string sounded.
    fn render_strings(&mut self, start: usize, end: usize) -> bool {
        let mut sounded = false;
        let mut position = start;
        let electric = self.electric();
        let coupling_gain = 2.0 * self.resonance * self.spec().coupling;
        loop {
            self.fire_due();
            let busy = self
                .strings
                .iter()
                .any(|s| s.is_ringing() || s.has_pending())
                || self.noises.is_active();
            if !busy {
                self.settle();
                return sounded;
            }
            if position >= end {
                return sounded;
            }
            if self.control_countdown == 0 {
                self.control_tick();
                self.control_countdown = CONTROL_INTERVAL;
            }
            // Up to the next control update or the next strummed string.
            let mut n = self.control_countdown.min(end - position);
            for string in &self.strings {
                if let Some(wait) = string.wait() {
                    n = n.min(wait);
                }
            }
            let span = position..position + n;
            let armed = self.strings.iter().any(GuitarString::is_active);
            let silence = self.silence();
            // The vertical planes first: they move the bridge, which drives
            // every horizontal plane in the second pass.
            let mut vertical = [0.0f32; CONTROL_INTERVAL];
            let mut jobs = Jobs::<VOICES>::new();
            for string in &mut self.strings {
                sounded |= string.is_ringing();
                string.vertical_jobs(n, &mut jobs);
            }
            jobs.run(&mut self.pad);
            for string in &self.strings {
                string.finish_vertical(n, &mut self.bridge[span.clone()], &mut vertical[..n]);
            }
            let mut coupling = [0.0f32; CONTROL_INTERVAL];
            let [mut high, mut low] = self.coupling_state;
            for (c, &v) in coupling[..n].iter_mut().zip(&vertical[..n]) {
                high = v + (high - v) * self.coupling_high;
                let band = v - high;
                low = band + (low - band) * self.coupling_low;
                *c = low * coupling_gain;
            }
            self.coupling_state = [
                if high.abs() < 1e-20 { 0.0 } else { high },
                if low.abs() < 1e-20 { 0.0 } else { low },
            ];
            let mut jobs = Jobs::<VOICES>::new();
            for string in &mut self.strings {
                string.horizontal_jobs(n, &coupling[..n], &mut jobs);
            }
            jobs.run(&mut self.pad);
            for string in &mut self.strings {
                string.finish_horizontal(n, &mut self.bridge[span.clone()], silence, armed);
                if electric {
                    string.render_pickups(n, &mut self.pickups[span.clone()]);
                }
                string.advance(n);
            }
            if electric {
                self.noises.render(&mut self.pickups[span.clone()]);
            } else {
                self.noises.render(&mut self.bridge[span.clone()]);
            }
            self.control_countdown -= n;
            position += n;
        }
    }

    /// One stretch of at most `bridge.len()` samples, `offset` into the block.
    fn process_chunk(
        &mut self,
        events: &[NoteEvent],
        offset: usize,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let n = left.len();
        self.bridge[..n].fill(0.0);
        self.pickups[..n].fill(0.0);
        let mut sounded = false;
        let mut start = 0;
        let mut i = 0;
        while i < events.len() {
            let frame = (events[i].frame as usize)
                .saturating_sub(offset)
                .clamp(start, n);
            sounded |= self.render_strings(start, frame);
            start = frame;
            let mut j = i + 1;
            while j < events.len() && events[j].frame == events[i].frame {
                j += 1;
            }
            self.handle_group(&events[i..j], self.clock + (offset + frame) as u64);
            i = j;
        }
        sounded |= self.render_strings(start, n);
        self.finish(left, right, sounded);
    }

    /// Body or pickups, amp and level, while anything is sounding or still
    /// ringing.
    fn finish(&mut self, left: &mut [f32], right: &mut [f32], sounded: bool) {
        if !sounded && !self.ringing {
            // Silence needs no processing; finish any level ramp instantly instead.
            self.gain.reset();
            self.apply_next_model();
            return;
        }
        let n = left.len();
        if self.electric() {
            self.electronics.process(&mut self.pickups[..n]);
            for ((l, r), &x) in left
                .iter_mut()
                .zip(right.iter_mut())
                .zip(&self.pickups[..n])
            {
                *l = x * PICKUP_GAIN;
                *r = x * PICKUP_GAIN;
            }
        } else {
            self.body.process(&self.bridge[..n], left, right);
        }
        for (dc, channel) in self.dc.iter_mut().zip([&mut *left, &mut *right]) {
            for sample in channel.iter_mut() {
                *sample = dc.process(*sample);
            }
        }
        self.amp.process(left, right);
        self.gain.process(left, right);
        if self.fade.is_smoothing() || self.fade.current() < 1.0 {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                let g = self.fade.tick();
                *l *= g;
                *r *= g;
            }
            if self.fade.current() == 0.0 {
                self.apply_next_model();
            }
        }
        let peak = left
            .iter()
            .chain(right.iter())
            .fold(0.0f32, |m, s| m.max(s.abs()));
        self.ringing = sounded || peak > TAIL_SILENCE;
        if !self.ringing {
            self.body.reset();
            self.amp.reset();
            self.electronics.reset();
            for dc in &mut self.dc {
                dc.reset();
            }
            self.apply_next_model();
        }
    }

    /// Switches to a model asked for while the guitar was sounding, now
    /// that it is faded out or silent, and fades back in.
    fn apply_next_model(&mut self) {
        let Some(model) = self.next_model.take() else {
            return;
        };
        self.model = model;
        self.body.set_spec(model.spec().body);
        self.update_electronics();
        self.update_open_notes();
        self.losses_moved = true;
        self.fade.set_target(1.0);
    }

    fn update_electronics(&mut self) {
        let spec = models::pickups(self.humbuckers);
        self.electronics
            .set(spec.resonance_hz, spec.resonance_q, self.sample_rate);
    }

    fn update_meters(&mut self) {
        for (index, string) in self.strings.iter().enumerate() {
            let meter = string.meter();
            self.meters[METER_NOTE + index] = meter.note;
            self.meters[METER_LEVEL + index] = meter.level;
            self.meters[METER_FRET + index] = meter.fret;
            self.meters[METER_HELD + index] = meter.held;
            self.meters[METER_KIND + index] = meter.kind;
        }
        self.meters[METER_BEND] = self.wheel;
        self.meters[METER_HAND] = self.fretboard.hand() as f32;
        self.meters[METER_STRUM] = self.last_strum;
        self.meters[METER_ARTICULATION] = self.articulation as f32;
        self.meters[METER_MUTE] = self.mute.current();
    }
}

/// The level, in dB, at which `model`'s body (with the body knob at
/// `body`) radiates the bridge's force at each of `hz`, into `out`: what the
/// panel draws. 0 dB for the electric, which has none.
pub fn body_response_db(model: usize, body: f32, sample_rate: f32, hz: &[f32], out: &mut [f32]) {
    let mut shape = Body::new(body);
    shape.prepare(sample_rate);
    shape.set_spec(ModelKind::from_index(model).spec().body);
    for (out, &hz) in out.iter_mut().zip(hz) {
        *out = 20.0 * shape.response(hz).max(1e-6).log10();
    }
}

/// Where a wound string's squeak rings: its windings' longitudinal
/// resonance, higher on the thinner strings (Pakarinen, Penttinen & Bank
/// 2007 measured them between one and two kilohertz).
fn ring_hz(string: usize) -> f32 {
    1150.0 + 180.0 * string as f32
}

impl Default for Guitar {
    fn default() -> Self {
        Self::new()
    }
}

impl Instrument for Guitar {
    fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        self.sample_rate = sample_rate;
        let max_delay = sample_rate / (note_hz(LOWEST_NOTE) * LOWEST_PITCH_RATIO) + 64.0;
        for string in &mut self.strings {
            string.prepare(sample_rate, max_delay);
        }
        self.pad.prepare(max_delay);
        self.dispersion = dispersion(sample_rate);
        self.body.prepare(sample_rate);
        self.body.set_spec(self.spec().body);
        self.amp.prepare(sample_rate);
        self.noises.prepare(sample_rate);
        self.update_electronics();
        for dc in &mut self.dc {
            dc.prepare(sample_rate);
        }
        self.gain.prepare(sample_rate);
        self.fade.prepare(sample_rate);
        self.bridge = vec![0.0; max_block.max(1)];
        self.pickups = vec![0.0; max_block.max(1)];
        self.coupling_high = (-TAU * COUPLING_LOW_HZ / sample_rate).exp();
        self.coupling_low = (-TAU * COUPLING_HIGH_HZ / sample_rate).exp();
        let control_rate = sample_rate / CONTROL_INTERVAL as f32;
        for smoothed in self.smoothers() {
            smoothed.prepare(control_rate);
        }
        self.chord_window = (CHORD_WINDOW_S * sample_rate) as u64;
        self.reset();
    }

    fn reset(&mut self) {
        for string in &mut self.strings {
            string.silence();
        }
        self.fretboard.reset();
        self.apply_next_model();
        self.fade.set_immediate(1.0);
        self.body.reset();
        self.amp.reset();
        self.noises.reset();
        self.electronics.reset();
        for dc in &mut self.dc {
            dc.reset();
        }
        self.coupling_state = [0.0; 2];
        self.gain.reset();
        self.bend.set_immediate(0.0);
        self.mod_wheel.set_immediate(0.0);
        self.mute_pedal = 0.0;
        self.update_mute();
        for smoothed in self.smoothers() {
            smoothed.settle();
        }
        self.vibrato_phase = 0.0;
        self.wheel = 0.0;
        self.read_controls();
        self.update_open_notes();
        self.control_countdown = 0;
        self.clock = 0;
        self.last_notes = None;
        self.strum = None;
        self.phrase = None;
        self.last_strum_time = 0;
        self.last_single = None;
        self.last_strum = 0.0;
        self.ringing = false;
        self.update_meters();
    }

    fn process(&mut self, events: &[NoteEvent], left: &mut [f32], right: &mut [f32]) {
        left.fill(0.0);
        right.fill(0.0);
        let len = left.len().min(right.len());
        // Blocks are never longer than prepared for, but if one were, it is
        // processed in pieces rather than overrunning the bridge buffer.
        let chunk = self.bridge.len();
        let mut offset = 0;
        let mut next = 0;
        while offset < len {
            let end = (offset + chunk).min(len);
            let first = next;
            while next < events.len() && (events[next].frame as usize) < end {
                next += 1;
            }
            if end == len {
                next = events.len();
            }
            self.process_chunk(
                &events[first..next],
                offset,
                &mut left[offset..end],
                &mut right[offset..end],
            );
            offset = end;
        }
        self.clock += len as u64;
        self.update_meters();
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(value) = crate::checked_value(PARAMS, index, value) else {
            return;
        };
        match index {
            TONE => self.tone.set_target(value),
            BODY => self.body.set_character(value),
            PLUCK_POSITION => self.pluck_position = value,
            PICK => self.hardness = value,
            MUTE => {
                self.mute_param = value;
                self.update_mute();
            }
            SUSTAIN => self.sustain.set_target(value.log2()),
            STRUM => self.strum_ms = value,
            VIBRATO => self.vibrato_cents.set_target(value),
            AMP => self.amp.set_mode(value as usize),
            DRIVE => self.amp.set_drive(value),
            AMP_TONE => self.amp.set_tone(value),
            LEVEL => {
                self.gain.set_db(value + TRIM_DB);
                self.level_gain = db_to_gain(value);
            }
            MODEL => {
                let model = ModelKind::from_index(value as usize);
                if model != self.model || self.next_model.is_some() {
                    self.next_model = Some(model);
                    if self.ringing {
                        self.fade.set_target(0.0);
                    } else {
                        self.apply_next_model();
                        self.fade.set_immediate(1.0);
                    }
                }
            }
            TECHNIQUE => self.technique = value as usize,
            PICKUP => {
                self.pickup_switch = value as usize;
                self.retap = true;
            }
            PICKUP_TYPE => {
                self.humbuckers = value >= 0.5;
                self.update_electronics();
                self.retap = true;
            }
            HUMANIZE => self.humanize = value,
            NOISES => self.noise_level = value,
            SQUEAKS => self.squeaks = value,
            RESONANCE => self.resonance = value,
            STRUM_DIRECTION => self.strum_direction = value as usize,
            LEGATO => self.legato = value as usize,
            KEY_SWITCHES => self.key_switches = value >= 0.5,
            ARTICULATION => self.set_articulation(value as usize),
            _ => {}
        }
    }

    fn set_tempo(&mut self, bpm: f64) {
        if bpm.is_finite() && bpm > 0.0 {
            self.bpm = bpm;
        }
    }

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        *out = self.meters;
    }
}
