//! One note: three unison wavetable oscillators, a sub, noise, two
//! filters, six envelopes and eight LFOs, with the mod matrix applied per
//! voice.
//!
//! Each source adds into one of three buses: straight out, into filter 1
//! or into filter 2. In serial, filter 1's output then joins filter 2's
//! bus; in parallel the two filters' outputs are summed. A filter that is
//! off passes its bus through untouched.
//!
//! LFOs and envelopes switched to mono aren't the voice's own: the synth
//! runs them once and hands their values in with the other instrument-wide
//! sources ([`Controls::globals`]); the voice only adds what is its own
//! (its envelopes, LFOs, velocity, note and randoms) before running the
//! matrix ([`run_matrix`]).

use std::f32::consts::FRAC_PI_4;

use dsp::{AdsrDesc, DcBlocker};
use midi::Pitch;

use super::env::{EnvMods, Envelope};
use super::filter::{self, VoiceFilter};
use super::halfband::Decimator;
use super::lfo::{Lfo, LfoMods, LfoSettings, MODE_FREE};
use super::morph::{self, MorphState};
use super::osc::{
    self, BLOCK, Frames, MAX_UNISON, OscTargets, Remap, Unison, UnisonOsc, fm_depth, is_cross_mod,
    is_remap, remap_speed, warp_speed,
};
use super::params::*;
use super::tables::{Bank, Table, level_for};

/// Gain of each oscillator at full level, before the voice's amp. Unison
/// stacks are power-normalised, so this is roughly one plain saw's level.
const OSC_GAIN: f32 = 0.5;
const SUB_GAIN: f32 = 0.5;
const NOISE_GAIN: f32 = 0.35;
/// Equal-power pan at center.
const CENTER: f32 = std::f32::consts::FRAC_1_SQRT_2;
/// The note an oscillator with key tracking off plays: C4.
const FIXED_NOTE: f32 = 60.0;

/// Where a source's sound goes (see the module docs).
pub const BUS_DIRECT: usize = 0;
pub const BUS_FILTER1: usize = 1;
pub const BUS_FILTER2: usize = 2;

/// Below this the voices take out what their sources send (before the
/// filters): the DC that asymmetric warps (Asym, Squeeze, PWM, Flip,
/// Drive, Remap) leave in a wave, which would otherwise thump at every
/// note's start and end and push the filters' drive off center. Vital's
/// voices filter DC out the same way.
pub const DC_BLOCK_HZ: f32 = 5.0;

/// The DC blockers' pole for [`DC_BLOCK_HZ`] at `sample_rate`.
pub fn dc_pole(sample_rate: f32) -> f32 {
    dsp::one_pole::pole(DC_BLOCK_HZ, sample_rate.max(1.0))
}

/// Where the sub's and the noise's modulation buffers sit in the scratch,
/// after the oscillators'.
pub const SCRATCH_SUB: usize = OSC_COUNT;
pub const SCRATCH_NOISE: usize = OSC_COUNT + 1;
pub const SCRATCH_COUNT: usize = OSC_COUNT + 2;

/// One oscillator's settings for a control tick, before modulation.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct OscControls {
    pub on: bool,
    /// The table parameter as set: a built-in table or [`USER_TABLE`]. The
    /// table it plays is resolved by the synth, which holds the user tables.
    pub table: usize,
    pub position: f32,
    pub warp_mode: usize,
    pub warp: f32,
    pub warp2_mode: usize,
    pub warp2: f32,
    /// How steep the oscillator's remap curve gets, for the band limit of
    /// the remap warps (see [`osc::remap_slope`]).
    pub remap_slope: f32,
    /// Octave and semitone, in semitones.
    pub transpose: f32,
    /// Cents.
    pub fine: f32,
    pub unison: usize,
    /// Cents of the outermost unison voices.
    pub detune: f32,
    pub detune_mode: usize,
    pub blend: f32,
    pub width: f32,
    /// One of [`STACKS`].
    pub stack: usize,
    pub warp_spread: f32,
    pub position_spread: f32,
    pub morph_mode: usize,
    pub morph: f32,
    /// One of [`MOD_SOURCES`].
    pub mod_source: usize,
    pub keytrack: bool,
    pub phase: f32,
    pub random: f32,
    pub level: f32,
    pub pan: f32,
    /// One of the `BUS_*` constants.
    pub bus: usize,
    /// The on/off switch, faded: `0..=1`, applied after modulation so
    /// switching off always ends in silence.
    pub fade: f32,
}

impl OscControls {
    /// Whether either warp slot listens to something else.
    fn listens(&self) -> bool {
        is_cross_mod(self.warp_mode) || is_cross_mod(self.warp2_mode)
    }
}

/// What oscillator `o` (with settings `oc`) listens to, as one of
/// [`MOD_SOURCES`] resolved: [`MOD_SOURCE_OSC`] + an oscillator,
/// [`MOD_SOURCE_SUB`] or [`MOD_SOURCE_NOISE`].
fn listened(o: usize, oc: &OscControls) -> usize {
    match oc.mod_source {
        MOD_SOURCE_AUTO => MOD_SOURCE_OSC + MODULATORS[o],
        source => source.min(MOD_SOURCES.len() - 1),
    }
}

/// One filter's settings for a control tick, before modulation.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FilterControls {
    /// On, or still fading out.
    pub on: bool,
    /// The on/off switch, faded, `0..=1`. It scales the mix, so switching
    /// off fades to dry; once there, skipping the filter sounds the same.
    pub fade: f32,
    pub kind: usize,
    pub cutoff_log2: f32,
    pub resonance: f32,
    pub drive: f32,
    pub keytrack: f32,
    pub mix: f32,
    pub blend: f32,
}

/// One mod matrix slot in use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModSlot {
    pub source: usize,
    pub dest: usize,
    pub amount: f32,
    /// Scales the slot by this source's value, unless it is `SRC_NONE`.
    pub aux: usize,
    /// The polarity remap, `source × scale + offset`.
    pub scale: f32,
    pub offset: f32,
    /// The curve's coefficients (see [`curve_coefficients`]).
    pub curve_k: f32,
    pub curve_norm: f32,
}

impl Default for ModSlot {
    fn default() -> Self {
        Self {
            source: SRC_NONE,
            dest: DEST_NONE,
            amount: 0.0,
            aux: SRC_NONE,
            scale: 1.0,
            offset: 0.0,
            curve_k: 0.0,
            curve_norm: 1.0,
        }
    }
}

impl ModSlot {
    /// The slot as its parameters set it: `polarity` one of
    /// [`POLARITIES`], `curve` the slot's curve.
    pub fn new(
        source: usize,
        dest: usize,
        amount: f32,
        aux: usize,
        curve: f32,
        polarity: usize,
    ) -> Self {
        let (scale, offset) = match (polarity, is_bipolar(source)) {
            (POLARITY_UNIPOLAR, true) => (0.5, 0.5),
            (POLARITY_BIPOLAR, false) => (2.0, -1.0),
            _ => (1.0, 0.0),
        };
        let (curve_k, curve_norm) = curve_coefficients(curve);
        ModSlot {
            source,
            dest,
            amount,
            aux,
            scale,
            offset,
            curve_k,
            curve_norm,
        }
    }

    /// The slot's output, `amount × curve(source) × aux`, for these source
    /// values; the source remapped to the slot's polarity first.
    #[inline]
    pub fn value(&self, sources: &[f32; SOURCE_COUNT]) -> f32 {
        let aux = if self.aux == SRC_NONE {
            1.0
        } else {
            sources[self.aux]
        };
        let x = sources[self.source] * self.scale + self.offset;
        self.amount * apply_curve(x, self.curve_k, self.curve_norm) * aux
    }
}

/// Runs the matrix: `slots` read `sources`, whose macros hold the macro
/// knobs. Slots aimed at macros run first and move them (clamped to their
/// range), so everything else reads the macros as moved; the moved values
/// are left in `sources`. Returns each destination's modulation in its own
/// unit (the slots' sum times [`DESTINATION_SCALES`]).
pub fn run_matrix(slots: &[ModSlot], sources: &mut [f32; SOURCE_COUNT]) -> [f32; DEST_COUNT] {
    let mut mods = [0.0f32; DEST_COUNT];
    let macros = DEST_MACRO1..DEST_MACRO1 + MACRO_COUNT;
    let mut any_macro = false;
    for slot in slots.iter().filter(|s| macros.contains(&s.dest)) {
        mods[slot.dest] += slot.value(sources);
        any_macro = true;
    }
    if any_macro {
        for m in 0..MACRO_COUNT {
            let source = macro_source(m);
            sources[source] = (sources[source] + mods[DEST_MACRO1 + m]).clamp(0.0, 1.0);
        }
    }
    for slot in slots.iter().filter(|s| !macros.contains(&s.dest)) {
        mods[slot.dest] += slot.value(sources);
    }
    for (d, scale) in mods.iter_mut().zip(DESTINATION_SCALES) {
        *d *= scale;
    }
    mods
}

/// Every setting the voices read, gathered once per control tick from the
/// smoothed parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Controls {
    pub osc: [OscControls; OSC_COUNT],
    pub sub_on: bool,
    pub sub_shape: usize,
    pub sub_transpose: f32,
    pub sub_level: f32,
    pub sub_bus: usize,
    pub sub_fade: f32,
    pub noise_on: bool,
    pub noise_kind: usize,
    pub noise_level: f32,
    pub noise_bus: usize,
    pub noise_fade: f32,
    pub filters: [FilterControls; 2],
    /// Filters in parallel rather than filter 1 into filter 2.
    pub parallel: bool,
    /// Each envelope's delay and hold, in milliseconds.
    pub env_delay: [f32; ENV_COUNT],
    pub env_hold: [f32; ENV_COUNT],
    /// Envelopes the synth runs once for every voice (never the amp's).
    pub env_mono: [bool; ENV_COUNT],
    pub lfo: [LfoSettings; LFO_COUNT],
    /// The slots with both a source and a destination and not bypassed,
    /// packed at the front.
    pub slots: [ModSlot; MOD_SLOTS],
    pub slot_count: usize,
    /// The LFOs some slot reads (as its source or scaling). The rest only
    /// run in the newest voice, whose LFOs the panel shows.
    pub lfo_used: [bool; LFO_COUNT],
    /// The instrument-wide sources this tick, indexed like [`SOURCES`]:
    /// the macro knobs, the wheels, aftertouch, slide, Fixed, and the mono
    /// LFOs and envelopes. Each voice starts from these.
    pub globals: [f32; SOURCE_COUNT],
    /// The pitch wheel, `-1..=1`.
    pub bend: f32,
    /// Semitones at full bend.
    pub bend_range: f32,
    pub velocity_sens: f32,
    /// The Volume knob, in dB, which the Volume destination moves from.
    pub volume_db: f32,
    /// The glide's bend (see [`GLIDE_CURVE`]).
    pub glide_curve: f32,
    /// Oscillators render at twice the rate.
    pub oversample: bool,
    pub sample_rate: f32,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            osc: [OscControls::default(); OSC_COUNT],
            sub_on: false,
            sub_shape: 0,
            sub_transpose: 0.0,
            sub_level: 0.0,
            sub_bus: BUS_DIRECT,
            sub_fade: 0.0,
            noise_on: false,
            noise_kind: 0,
            noise_level: 0.0,
            noise_bus: BUS_DIRECT,
            noise_fade: 0.0,
            filters: [FilterControls::default(); 2],
            parallel: false,
            env_delay: [0.0; ENV_COUNT],
            env_hold: [0.0; ENV_COUNT],
            env_mono: [false; ENV_COUNT],
            lfo: [LfoSettings::default(); LFO_COUNT],
            slots: [ModSlot::default(); MOD_SLOTS],
            slot_count: 0,
            lfo_used: [true; LFO_COUNT],
            globals: [0.0; SOURCE_COUNT],
            bend: 0.0,
            bend_range: 2.0,
            velocity_sens: 0.0,
            volume_db: 0.0,
            glide_curve: 0.0,
            oversample: false,
            sample_rate: 48_000.0,
        }
    }
}

/// Coloured noise, one generator per channel so the two sides are unrelated.
#[derive(Debug, Clone, Copy)]
struct Noise {
    seed: [u32; 2],
    /// Paul Kellet's economy pink-noise filter: three one-poles per channel.
    pink: [[f32; 3]; 2],
    brown: [f32; 2],
    last: [f32; 2],
    gain: f32,
    gain_step: f32,
    /// A third generator, for oscillators listening to the noise, so
    /// listening leaves what is heard unchanged.
    modulation: u32,
}

impl Noise {
    fn new(seed: u32) -> Self {
        Self {
            seed: [seed | 1, seed.rotate_left(16) | 1],
            pink: [[0.0; 3]; 2],
            brown: [0.0; 2],
            last: [0.0; 2],
            gain: 0.0,
            gain_step: 0.0,
            modulation: seed.rotate_left(8) | 1,
        }
    }

    #[inline(always)]
    fn white(seed: &mut u32) -> f32 {
        dsp::noise::bipolar(dsp::xorshift32(seed))
    }

    /// Adds `left.len()` samples of noise of `kind` (see [`NOISE_TYPES`]).
    fn render(&mut self, kind: usize, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mut out = [0.0; 2];
            for (ch, out) in out.iter_mut().enumerate() {
                let white = Self::white(&mut self.seed[ch]);
                *out = match kind {
                    1 => {
                        let p = &mut self.pink[ch];
                        p[0] = 0.99765 * p[0] + white * 0.099_046;
                        p[1] = 0.963 * p[1] + white * 0.296_516_4;
                        p[2] = 0.57 * p[2] + white * 1.052_691_3;
                        (p[0] + p[1] + p[2] + white * 0.1848) * 0.25
                    }
                    2 => {
                        let b = &mut self.brown[ch];
                        *b = (*b + 0.02 * white) / 1.02;
                        *b * 3.5
                    }
                    3 => {
                        let blue = (white - self.last[ch]) * 0.5;
                        self.last[ch] = white;
                        blue
                    }
                    _ => white * 0.6,
                };
            }
            *l += out[0] * self.gain;
            *r += out[1] * self.gain;
            self.gain += self.gain_step;
        }
    }
}

/// The sub oscillator: a plain shape from the bank's sub table.
#[derive(Debug, Clone, Copy, Default)]
struct Sub {
    phase: f32,
    inc: f32,
    inc_step: f32,
    gain: f32,
    gain_step: f32,
    level: usize,
}

/// Unison voice `u` of `n`'s place in the spread, `-1..=1` from lowest to highest pitch.
fn spread(u: usize, n: usize) -> f32 {
    if n < 2 {
        0.0
    } else {
        2.0 * u as f32 / (n - 1) as f32 - 1.0
    }
}

/// Unison voice `u` of `n`'s place in the detune, `-1..=1`, for detune
/// mode `mode` (see [`DETUNE_MODES`]): as spread evenly, or drawn toward
/// the note (Super, Exp) or away from it (Inverse), or fixed per note at
/// random (`random`, `-1..=1` per voice).
pub fn detune_place(mode: usize, u: usize, n: usize, random: f32) -> f32 {
    let x = spread(u, n);
    match mode {
        DETUNE_SUPER => x * x.abs().sqrt(),
        DETUNE_EXP => x * x.abs() * x.abs(),
        DETUNE_INVERSE => x.signum() * x.abs().sqrt(),
        DETUNE_RANDOM if n > 1 => random,
        _ => x,
    }
}

/// Unison voice `u` of `n`'s stereo position, `-1..=1`. Counting pairs out
/// from the center, every other pair swaps sides, so neither side gets all
/// the sharp voices and the stack sounds wide instead of tilted.
fn pan_spread(u: usize, n: usize) -> f32 {
    let s = spread(u, n);
    let r = (2 * u as i32 - (n as i32 - 1)).unsigned_abs();
    let pair = if n % 2 == 1 { r / 2 } else { r.div_ceil(2) };
    if pair % 2 == 0 { -s } else { s }
}

/// Whether unison voice `u` of `n` is a center voice, which Blend leaves at full level.
fn is_center(u: usize, n: usize) -> bool {
    (2 * u as i32 - (n as i32 - 1)).abs() <= 1
}

/// The interval, in semitones, that stack `stack` (one of [`STACKS`])
/// gives unison voice `u` of `n`. Voices take the stack's intervals in
/// turn, lined up so the center voice plays the note itself; each layer
/// then gets voices from across the detune, mostly from both sides.
pub fn stack_interval(stack: usize, u: usize, n: usize) -> f32 {
    let intervals = STACK_INTERVALS[stack.min(STACK_INTERVALS.len() - 1)];
    let layers = intervals.len();
    if layers == 1 {
        return intervals[0];
    }
    let center = n.saturating_sub(1) / 2;
    intervals[(u + layers - center % layers) % layers]
}

/// LFO `lfo`'s modulation in `mods` (a matrix run's destinations).
pub fn lfo_mods(mods: &[f32; DEST_COUNT], lfo: usize) -> LfoMods {
    LfoMods {
        phase: mods[lfo_dest(lfo, LFO_DEST_PHASE)],
        smooth: mods[lfo_dest(lfo, LFO_DEST_SMOOTH)],
        rise: mods[lfo_dest(lfo, LFO_DEST_RISE)],
        delay: mods[lfo_dest(lfo, LFO_DEST_DELAY)],
    }
}

/// Envelope `env`'s modulation in `mods` (a matrix run's destinations).
pub fn env_mods(mods: &[f32; DEST_COUNT], env: usize) -> EnvMods {
    EnvMods {
        attack: mods[env_dest(env, ENV_DEST_ATTACK)],
        decay: mods[env_dest(env, ENV_DEST_DECAY)],
        sustain: mods[env_dest(env, ENV_DEST_SUSTAIN)],
        release: mods[env_dest(env, ENV_DEST_RELEASE)],
    }
}

/// The gain the Volume destination's `modulation` (dB) adds to the Volume
/// knob at `knob_db`, the knob's range kept.
fn volume_gain(knob_db: f32, modulation: f32) -> f32 {
    if modulation == 0.0 {
        return 1.0;
    }
    let db = (knob_db + modulation).clamp(VOLUME_MIN_DB, VOLUME_MAX_DB) - knob_db;
    (db * (std::f32::consts::LN_10 / 20.0)).exp()
}

/// Envelope time in samples.
pub fn samples(ms: f32, sample_rate: f32) -> u32 {
    (ms.max(0.0) * sample_rate / 1000.0) as u32
}

/// A glide from one note to another over some control ticks, bent by the
/// glide curve.
#[derive(Debug, Clone, Copy)]
struct Glide {
    from: f32,
    to: f32,
    /// Ticks done and to do.
    done: f32,
    length: f32,
}

impl Glide {
    fn at(&self, curve: f32) -> f32 {
        if self.done >= self.length {
            return self.to;
        }
        let t = self.done / self.length;
        self.from + (self.to - self.from) * dsp::stage_shape(t, curve)
    }
}

/// What starts a note (see [`Voice::start`]).
#[derive(Debug, Clone, Copy)]
pub struct NoteStart<'a> {
    pub pitch: Pitch,
    pub velocity: f32,
    /// The note's two random values, `-1..=1`.
    pub random: [f32; 2],
    /// The note to slide from, if any, over `glide_ticks` control ticks.
    pub from: Option<f32>,
    pub glide_ticks: f32,
    /// Per-unison random numbers in `0..1`: each oscillator's start
    /// phases, then its random detune.
    pub randoms: &'a [[[f32; MAX_UNISON]; OSC_COUNT]; 2],
    /// Whether the envelopes and LFOs start again (they always do from
    /// silence; legato slides don't).
    pub retrigger: bool,
}

/// What the voices read as they render: the bank (for the sub), each
/// oscillator's table and its remap curve (see [`osc::Remap`]).
#[derive(Clone, Copy)]
pub struct Waves<'a> {
    pub bank: &'a Bank,
    pub tables: [&'a Table; OSC_COUNT],
    pub remaps: &'a [Remap; OSC_COUNT],
}

pub struct Voice {
    pub osc: [UnisonOsc; OSC_COUNT],
    morph: [MorphState; OSC_COUNT],
    /// Each oscillator's decimators (left and right), for twice the rate.
    decimators: [[Decimator; 2]; OSC_COUNT],
    sub: Sub,
    noise: Noise,
    pub filters: [VoiceFilter; 2],
    /// Each bus's DC blocker (left and right; see [`DC_BLOCK_HZ`]).
    dc: [[DcBlocker; 2]; 3],
    /// Whether each filter ran at the last tick.
    filter_on: [bool; 2],
    /// Env 1 is the amp; the rest feed the mod matrix.
    pub env: [Envelope; ENV_COUNT],
    pub lfo: [Lfo; LFO_COUNT],
    /// Each LFO's rate modulation in octaves, from the last tick.
    lfo_rate_mods: [f32; LFO_COUNT],
    /// Velocity and the Amp destination, ramped; the amp envelope is applied per sample.
    amp: f32,
    amp_step: f32,
    /// The voice's balance gains (the Pan destination), ramped.
    pan: [f32; 2],
    pan_step: [f32; 2],
    /// The key that started the voice (in mono, the key it plays now).
    pub pitch: Pitch,
    /// The note sounding, in semitones, while gliding.
    pub note: f32,
    glide: Glide,
    /// The note's own bend, in semitones, on top of the wheel's and the
    /// mod matrix's: followed at each tick, so it glides there over one
    /// control interval as they do.
    pub bend: f32,
    /// Started and not yet rendered: whether it started from silence.
    /// A bend arriving now is where the note starts, not a move to glide
    /// through.
    pub fresh: Option<bool>,
    pub velocity: f32,
    /// The release velocity, once let go.
    pub lift: f32,
    /// Fixed per note, `-1..=1`.
    pub random: f32,
    pub random2: f32,
    /// Each oscillator's unison voices' places in a random detune.
    detune_random: [[f32; MAX_UNISON]; OSC_COUNT],
    /// Seconds since the envelopes last started, for the LFOs' delay and rise.
    age: f32,
    /// Key still down. Note-off clears it, so a voice is released only once.
    pub held: bool,
    /// The note counter at the last note-on; the lowest is the oldest note.
    pub started: u64,
    /// The mod sources at the last tick (macros as moved), for the meters.
    pub sources: [f32; SOURCE_COUNT],
    /// Each filter's modulated cutoff at the last tick, in Hz.
    pub cutoff: [f32; 2],
    /// Each oscillator's modulated spectral morph at the last tick.
    pub morph_amount: [f32; OSC_COUNT],
}

impl Voice {
    pub fn new(seed: u32) -> Self {
        Self {
            osc: std::array::from_fn(|_| UnisonOsc::default()),
            morph: std::array::from_fn(|_| MorphState::default()),
            decimators: std::array::from_fn(|_| [Decimator::default(), Decimator::default()]),
            sub: Sub::default(),
            noise: Noise::new(seed),
            filters: [VoiceFilter::new(), VoiceFilter::new()],
            dc: [[DcBlocker::default(); 2]; 3],
            filter_on: [true; 2],
            env: std::array::from_fn(|_| Envelope::new(AdsrDesc::default(), 48_000.0)),
            lfo: std::array::from_fn(|i| Lfo::new(seed ^ (i as u32 + 1).wrapping_mul(0x2545_F491))),
            lfo_rate_mods: [0.0; LFO_COUNT],
            amp: 0.0,
            amp_step: 0.0,
            pan: [1.0; 2],
            pan_step: [0.0; 2],
            pitch: Pitch::A4,
            note: 69.0,
            glide: Glide {
                from: 69.0,
                to: 69.0,
                done: 0.0,
                length: 0.0,
            },
            bend: 0.0,
            fresh: None,
            velocity: 0.0,
            lift: 0.0,
            random: 0.0,
            random2: 0.0,
            detune_random: [[0.0; MAX_UNISON]; OSC_COUNT],
            age: 0.0,
            held: false,
            started: 0,
            sources: [0.0; SOURCE_COUNT],
            cutoff: [1000.0; 2],
            morph_amount: [0.0; OSC_COUNT],
        }
    }

    pub fn prepare(&mut self, sample_rate: f32, envs: [AdsrDesc; ENV_COUNT]) {
        for filter in &mut self.filters {
            filter.prepare(sample_rate);
        }
        for (env, desc) in self.env.iter_mut().zip(envs) {
            *env = Envelope::new(desc, sample_rate);
        }
        self.silence();
    }

    pub fn is_active(&self) -> bool {
        self.env[0].is_active()
    }

    pub fn silence(&mut self) {
        for env in &mut self.env {
            env.reset();
        }
        self.held = false;
        for filter in &mut self.filters {
            filter.reset();
        }
        self.dc = [[DcBlocker::default(); 2]; 3];
        for morph in &mut self.morph {
            morph.reset();
        }
        for decimator in self.decimators.iter_mut().flatten() {
            decimator.reset();
        }
        self.amp = 0.0;
        self.amp_step = 0.0;
    }

    /// Starts (or retriggers) a note as `note` says. Free-running LFOs
    /// take up the instrument's clocks, `global_lfos`.
    pub fn start(&mut self, note: &NoteStart, controls: &Controls, global_lfos: &[Lfo; LFO_COUNT]) {
        let NoteStart {
            pitch,
            velocity,
            random,
            from,
            glide_ticks,
            randoms,
            retrigger: retrigger_envs,
        } = *note;
        let active = self.is_active();
        if !active {
            // From silence: fresh phases and filters. A retriggered or stolen
            // voice keeps them, so the output doesn't jump.
            self.silence();
            for (osc, (settings, randoms)) in self
                .osc
                .iter_mut()
                .zip(controls.osc.iter().zip(&randoms[0]))
            {
                for (phase, r) in osc.phase.iter_mut().zip(randoms) {
                    *phase = (settings.phase + settings.random * r).rem_euclid(1.0);
                }
            }
            self.sub.phase = 0.0;
        }
        for (detune, randoms) in self.detune_random.iter_mut().zip(&randoms[1]) {
            for (d, r) in detune.iter_mut().zip(randoms) {
                *d = 2.0 * r - 1.0;
            }
        }
        self.pitch = pitch;
        self.held = true;
        self.bend = 0.0;
        self.fresh = Some(!active);
        self.velocity = velocity;
        self.lift = 0.0;
        [self.random, self.random2] = random;
        // A voice from silence with nothing to glide from starts on its note,
        // not wherever it last played.
        let note = pitch.get() as f32;
        let from = if active { from } else { from.or(Some(note)) };
        self.glide_to(note, from, glide_ticks, controls.glide_curve);
        if retrigger_envs || !active {
            self.age = 0.0;
            let fs = controls.sample_rate;
            for (i, env) in self.env.iter_mut().enumerate() {
                if controls.env_mono[i] {
                    continue;
                }
                env.start(
                    samples(controls.env_delay[i], fs),
                    samples(controls.env_hold[i], fs),
                );
            }
            for (i, lfo) in self.lfo.iter_mut().enumerate() {
                let settings = &controls.lfo[i];
                if settings.mono {
                    continue;
                }
                if settings.mode == MODE_FREE {
                    *lfo = global_lfos[i];
                } else {
                    lfo.reseed((self.started as u32) << 3 | i as u32);
                    lfo.restart(settings);
                }
            }
        }
    }

    /// Heads for `note`, from `from` (or where it is now) over `ticks`
    /// control ticks, bent by `curve`; with no glide (`ticks` < 1) it jumps.
    pub fn glide_to(&mut self, note: f32, from: Option<f32>, ticks: f32, curve: f32) {
        let start = from.unwrap_or(self.note);
        if ticks < 1.0 || !start.is_finite() {
            self.note = note;
            self.glide = Glide {
                from: note,
                to: note,
                done: 0.0,
                length: 0.0,
            };
        } else {
            self.note = start;
            self.glide = Glide {
                from: start,
                to: note,
                done: 0.0,
                length: ticks,
            };
        }
        self.note = self.glide.at(curve);
    }

    pub fn release(&mut self, lift: f32) {
        self.held = false;
        self.lift = lift;
        for env in &mut self.env {
            env.release();
        }
    }

    /// Updates the mod sources and every ramp target for the next control
    /// interval. `tables` are what each oscillator plays now. `advance`
    /// moves the envelopes, LFOs and glide on by one interval; a voice just
    /// started passes false and `immediate`. `shown`: the voice's LFOs are
    /// on display, so even the ones no slot reads run.
    pub fn tick(
        &mut self,
        c: &Controls,
        tables: &[&Table; OSC_COUNT],
        scratch: &mut morph::Scratch,
        advance: bool,
        immediate: bool,
        shown: bool,
    ) {
        let fs = c.sample_rate;
        let interval = BLOCK as f32 / fs;
        if advance {
            self.age += interval;
            for (env, mono) in self.env[1..].iter_mut().zip(&c.env_mono[1..]) {
                if !mono {
                    env.advance(BLOCK as u32);
                }
            }
            self.glide.done += 1.0;
            self.note = self.glide.at(c.glide_curve);
            // Rate modulation from the last tick: a control interval late,
            // which no one hears, and the sources below then hold the LFO
            // values the meters show.
            for (i, lfo) in self.lfo.iter_mut().enumerate() {
                let settings = &c.lfo[i];
                if settings.mono
                    || !settings.running(self.age, &lfo.mods)
                    || !(shown || c.lfo_used[i])
                {
                    continue;
                }
                let rate = settings.rate_hz * self.lfo_rate_mods[i].exp2();
                lfo.advance(settings, rate, interval, self.held);
            }
        }

        let s = &mut self.sources;
        *s = c.globals;
        for (i, env) in self.env.iter().enumerate() {
            if !c.env_mono[i] {
                s[env_source(i)] = env.level();
            }
        }
        for (i, lfo) in self.lfo.iter().enumerate() {
            let settings = &c.lfo[i];
            if !settings.mono {
                s[lfo_source(i)] = lfo.value * settings.rise_gain(self.age, &lfo.mods);
            }
        }
        s[SRC_VELOCITY] = self.velocity;
        s[SRC_NOTE] = ((self.pitch.get() as f32 - 64.0) / 63.5).clamp(-1.0, 1.0);
        s[SRC_RANDOM] = self.random;
        s[SRC_RANDOM2] = self.random2;
        s[SRC_LIFT] = self.lift;

        let mods = run_matrix(&c.slots[..c.slot_count], &mut self.sources);
        for (i, rate) in self.lfo_rate_mods.iter_mut().enumerate() {
            *rate = mods[lfo_rate_dest(i)];
        }
        for (i, lfo) in self.lfo.iter_mut().enumerate() {
            lfo.mods = lfo_mods(&mods, i);
        }
        for (e, env) in self.env.iter_mut().enumerate() {
            if !c.env_mono[e] {
                env.set_mods(env_mods(&mods, e));
            }
        }

        let mut pitch_mod = c.bend * c.bend_range + mods[DEST_GLOBAL_PITCH];
        if self.bend != 0.0 {
            pitch_mod += self.bend;
        }
        let note = self.note + pitch_mod;
        // Twice the rate halves every increment and doubles the samples
        // per interval; the band limits can then go a level richer.
        let rate = if c.oversample { 2.0 * fs } else { fs };
        let steps = if c.oversample { 2 * BLOCK } else { BLOCK };
        // Off oscillators are skipped from here on: they cost nothing.
        let base_hz: [f32; OSC_COUNT] = std::array::from_fn(|o| {
            let oc = &c.osc[o];
            if !oc.on {
                return 0.0;
            }
            let d = osc_dest(o);
            let key = if oc.keytrack {
                note
            } else {
                FIXED_NOTE + pitch_mod
            };
            dsp::note_hz(
                key + oc.transpose + (oc.fine + mods[d + DEST_FINE]) / 100.0 + mods[d + DEST_PITCH],
            )
        });
        let sub_hz = dsp::note_hz(note + c.sub_transpose);
        for o in 0..OSC_COUNT {
            let oc = &c.osc[o];
            let d = osc_dest(o);
            let frames = tables[o].frames;
            if !oc.on {
                self.osc[o].park(frames);
                self.morph[o].reset();
                continue;
            }
            let count = oc.unison.clamp(1, MAX_UNISON);
            let detune = (oc.detune + mods[d + DEST_DETUNE]).clamp(0.0, 100.0);
            let blend = (oc.blend + mods[d + DEST_BLEND]).clamp(0.0, 1.0);
            let level = (oc.level + mods[d + DEST_LEVEL]).clamp(0.0, 1.0) * oc.fade;
            let pan = (oc.pan + mods[d + DEST_PAN]).clamp(-1.0, 1.0);
            let width = (oc.width + mods[DEST_WIDTH_A + o]).clamp(0.0, 1.0);
            let warp = (oc.warp + mods[d + DEST_WARP]).clamp(0.0, 1.0);
            let warp2 = (oc.warp2 + mods[warp2_dest(o)]).clamp(0.0, 1.0);
            let position = (oc.position + mods[d + DEST_POSITION]).clamp(0.0, 1.0);
            let morph_amount = (oc.morph + mods[DEST_MORPH_A + o]).clamp(0.0, 1.0);
            let warp_spread = (oc.warp_spread + mods[DEST_WARP_SPREAD_A + o]).clamp(0.0, 1.0);
            let position_spread =
                (oc.position_spread + mods[DEST_POSITION_SPREAD_A + o]).clamp(0.0, 1.0);
            self.morph_amount[o] = morph_amount;

            let mut targets = OscTargets {
                count,
                position,
                warp_amounts: [warp, warp2],
                ..OscTargets::default()
            };
            let weights: [f32; MAX_UNISON] = std::array::from_fn(|u| {
                if u >= count || is_center(u, count) {
                    1.0
                } else {
                    blend
                }
            });
            let power: f32 = weights[..count].iter().map(|w| w * w).sum();
            let norm = level * OSC_GAIN / power.max(1e-6).sqrt();
            let nyquist_inc = 0.49;
            // FM reads ahead by up to its depth times the modulator's own
            // rate of change: another oscillator's pitch, the sub's, or for
            // noise, as fast as anything can change.
            let fm_hz = match listened(o, oc) {
                MOD_SOURCE_SUB => sub_hz,
                MOD_SOURCE_NOISE => 0.5 * fs,
                source => {
                    let m = source - MOD_SOURCE_OSC;
                    if m != o && c.osc[m].on {
                        base_hz[m]
                    } else {
                        0.0
                    }
                }
            } * std::f32::consts::TAU;
            let modes = [oc.warp_mode, oc.warp2_mode];
            // Band-limit each voice for the fastest it reads the table: its
            // pitch plus FM's reach, sped up by both warps.
            let reach = |amounts: [f32; 2]| {
                let mut speed = 1.0;
                let mut fm = 0.0;
                for (mode, amount) in modes.into_iter().zip(amounts) {
                    speed *= if is_remap(mode) {
                        remap_speed(amount, oc.remap_slope)
                    } else {
                        warp_speed(mode, amount)
                    };
                    if mode == osc::WARP_FM {
                        fm += fm_hz * fm_depth(amount);
                    }
                }
                (speed, fm)
            };
            let spreading = warp_spread > 0.0;
            let (mut speed, mut fm) = reach([warp, warp2]);
            for u in 0..MAX_UNISON {
                targets.warp[0][u] = warp;
                targets.warp[1][u] = warp2;
                targets.positions[u] = position;
            }
            for (u, weight) in weights.iter().enumerate().take(count) {
                let place = spread(u, count);
                let tuning = detune_place(oc.detune_mode, u, count, self.detune_random[o][u]);
                let interval = stack_interval(oc.stack, u, count);
                let voice_hz = base_hz[o] * (detune * tuning / 1200.0 + interval / 12.0).exp2();
                targets.inc[u] = (voice_hz / rate).clamp(0.0, nyquist_inc);
                if spreading {
                    // Warp spread fans the amounts out across the detune.
                    let offset = warp_spread * 0.5 * place;
                    let amounts = [warp, warp2].map(|a| (a + offset).clamp(0.0, 1.0));
                    targets.warp[0][u] = amounts[0];
                    targets.warp[1][u] = amounts[1];
                    (speed, fm) = reach(amounts);
                }
                if position_spread > 0.0 {
                    targets.positions[u] =
                        (position + position_spread * 0.5 * place).clamp(0.0, 1.0);
                }
                targets.level[u] = level_for((voice_hz + fm) * speed, rate) as u8;
                let place = (pan + width * pan_spread(u, count)).clamp(-1.0, 1.0);
                let angle = (place + 1.0) * FRAC_PI_4;
                let (sin, cos) = angle.sin_cos();
                targets.gain_l[u] = cos * weight * norm;
                targets.gain_r[u] = sin * weight * norm;
            }
            if morph::is_identity(oc.morph_mode, morph_amount) {
                self.morph[o].reset();
            } else {
                // One band limit for every voice: the plainest any needs.
                let plainest = targets.level[..count].iter().copied().max().unwrap_or(0);
                self.morph[o].tick(
                    scratch,
                    tables[o],
                    oc.morph_mode,
                    morph_amount,
                    position,
                    usize::from(plainest),
                    base_hz[o],
                    steps,
                    immediate,
                );
            }
            self.osc[o].set_targets(&targets, frames, steps, immediate);
        }

        let sub_gain = if c.sub_on {
            (c.sub_level + mods[DEST_SUB_LEVEL]).clamp(0.0, 1.0) * c.sub_fade * SUB_GAIN
        } else {
            0.0
        };
        let sub_inc = (sub_hz / fs).clamp(0.0, 0.49);
        self.sub.level = level_for(sub_hz, fs);
        let noise_gain = if c.noise_on {
            (c.noise_level + mods[DEST_NOISE_LEVEL]).clamp(0.0, 1.0) * c.noise_fade * NOISE_GAIN
        } else {
            0.0
        };
        let velocity_gain = 1.0 - c.velocity_sens + c.velocity_sens * self.velocity;
        let amp = velocity_gain
            * (1.0 + mods[DEST_AMP]).clamp(0.0, 2.0)
            * volume_gain(c.volume_db, mods[DEST_VOLUME]);
        let voice_pan = mods[DEST_VOICE_PAN].clamp(-1.0, 1.0);
        let pan = [(1.0 - voice_pan).min(1.0), (1.0 + voice_pan).min(1.0)];

        let octaves_from_c4 = (self.note - 60.0) / 12.0;
        for (f, fc) in c.filters.iter().enumerate() {
            let [cutoff, resonance, drive, mix, blend, keytrack] = FILTER_DESTINATIONS[f];
            let keytrack = (fc.keytrack + mods[keytrack]).clamp(0.0, 1.0);
            let cutoff_log2 = fc.cutoff_log2 + mods[cutoff] + keytrack * octaves_from_c4;
            self.cutoff[f] = cutoff_log2.clamp(4.0, 15.0).exp2();
            // Coming back on, start clean rather than from whatever was
            // ringing when it went off; the mix fades it in from dry.
            let returns = fc.on && !self.filter_on[f];
            self.filter_on[f] = fc.on;
            if !fc.on {
                continue;
            }
            let resonance = (fc.resonance + mods[resonance]).clamp(0.0, 1.0);
            let drive = (fc.drive + mods[drive]).clamp(0.0, 1.0);
            let mix = (fc.mix + mods[mix]).clamp(0.0, 1.0) * fc.fade;
            let blend = (fc.blend + mods[blend]).clamp(0.0, 1.0);
            let filter = &mut self.filters[f];
            filter.set_kind(fc.kind);
            if returns {
                filter.reset();
            }
            let coef = filter::coefficients(fc.kind, self.cutoff[f], resonance, blend, fs);
            filter.set_targets(coef, drive, mix, BLOCK, immediate || returns);
        }

        if !c.sub_on {
            self.sub.gain = 0.0;
            self.sub.gain_step = 0.0;
        }
        if !c.noise_on {
            self.noise.gain = 0.0;
            self.noise.gain_step = 0.0;
        }
        if immediate {
            self.sub.inc = sub_inc;
            self.sub.inc_step = 0.0;
            self.sub.gain = sub_gain;
            self.sub.gain_step = 0.0;
            self.noise.gain = noise_gain;
            self.noise.gain_step = 0.0;
            self.amp = amp;
            self.amp_step = 0.0;
            self.pan = pan;
            self.pan_step = [0.0; 2];
        } else {
            let inv = 1.0 / BLOCK as f32;
            self.sub.inc_step = (sub_inc - self.sub.inc) * inv;
            self.sub.gain_step = (sub_gain - self.sub.gain) * inv;
            self.noise.gain_step = (noise_gain - self.noise.gain) * inv;
            self.amp_step = (amp - self.amp) * inv;
            self.pan_step = [(pan[0] - self.pan[0]) * inv, (pan[1] - self.pan[1]) * inv];
        }
    }

    /// The order the oscillators render in, and what each listens to (an
    /// index into the scratch: an oscillator, the sub or the noise) if
    /// anything. Something listened to renders first; when oscillators
    /// listen to each other in a ring, the first in A, B, C order keeps
    /// listening and the one it listens to doesn't.
    fn listening(c: &Controls) -> ([usize; OSC_COUNT], [Option<usize>; OSC_COUNT]) {
        let mut heard: [Option<usize>; OSC_COUNT] = std::array::from_fn(|o| {
            let oc = &c.osc[o];
            if !oc.on || !oc.listens() {
                return None;
            }
            match listened(o, oc) {
                MOD_SOURCE_SUB => c.sub_on.then_some(SCRATCH_SUB),
                MOD_SOURCE_NOISE => c.noise_on.then_some(SCRATCH_NOISE),
                source => {
                    let m = source - MOD_SOURCE_OSC;
                    (m != o && c.osc[m].on).then_some(m)
                }
            }
        });
        // Break rings: following who listens to whom from each oscillator,
        // coming back to it means a ring.
        for o in 0..OSC_COUNT {
            let mut at = o;
            for _ in 0..OSC_COUNT {
                match heard[at] {
                    Some(next) if next < OSC_COUNT => at = next,
                    _ => break,
                }
                if at == o {
                    if let Some(next) = heard[o] {
                        heard[next] = None;
                    }
                    break;
                }
            }
        }
        let mut order = [0; OSC_COUNT];
        let mut placed = [false; OSC_COUNT];
        let mut n = 0;
        // With the rings broken each pass places at least one, so this many
        // passes place them all; bounded all the same, on the audio thread.
        for _ in 0..OSC_COUNT {
            for o in 0..OSC_COUNT {
                let ready = match heard[o] {
                    Some(m) if m < OSC_COUNT => placed[m],
                    _ => true,
                };
                if !placed[o] && ready {
                    placed[o] = true;
                    order[n] = o;
                    n += 1;
                }
            }
        }
        debug_assert_eq!(n, OSC_COUNT, "every oscillator renders");
        (order, heard)
    }

    /// Fills the sub's or the noise's modulation buffer for oscillators
    /// listening to it: one lane, unscaled, at the oscillators' rate.
    fn modulation_source(
        &mut self,
        bank: &Bank,
        c: &Controls,
        which: usize,
        out: &mut Unison,
        len: usize,
    ) {
        let lane = &mut out[0];
        if which == SCRATCH_SUB {
            let level = &bank.sub.levels[self.sub.level];
            let shape = level.frame(c.sub_shape.min(bank.sub.frames - 1));
            let (mut phase, mut inc) = (self.sub.phase, self.sub.inc);
            let div = if c.oversample { 0.5 } else { 1.0 };
            let step = self.sub.inc_step * div;
            for x in lane.iter_mut().take(len) {
                *x = super::tables::read(shape, level.size, level.mask, phase);
                phase += inc * div;
                if phase >= 1.0 {
                    phase -= 1.0;
                }
                inc += step;
            }
        } else {
            for x in lane.iter_mut().take(len) {
                *x = Noise::white(&mut self.noise.modulation);
            }
        }
    }

    /// Adds this voice to `left` and `right`, at most [`BLOCK`] samples,
    /// playing `waves`; `scratch` carries what oscillators listen to (each
    /// oscillator's unison outputs, the sub's and the noise's) for
    /// cross-modulation.
    pub fn render(
        &mut self,
        c: &Controls,
        waves: &Waves,
        scratch: &mut [Unison; SCRATCH_COUNT],
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let Waves {
            bank,
            tables,
            remaps,
        } = *waves;
        let len = left.len().min(right.len()).min(BLOCK);
        if len > 0 {
            self.fresh = None;
        }
        // Samples each oscillator renders: twice as many at twice the rate.
        let osc_len = if c.oversample { 2 * len } else { len };
        // Straight out, into filter 1, into filter 2 (see `BUS_*`).
        let mut buses = [[[0.0f32; BLOCK]; 2]; 3];
        let mut used = [false; 3];

        let (order, heard) = Self::listening(c);
        let listened_to = |m: usize| heard.contains(&Some(m));
        for which in [SCRATCH_SUB, SCRATCH_NOISE] {
            if listened_to(which) {
                self.modulation_source(bank, c, which, &mut scratch[which], osc_len);
            }
        }
        for o in order {
            let oc = &c.osc[o];
            if !oc.on {
                continue;
            }
            used[oc.bus] = true;
            let modes = [oc.warp_mode, oc.warp2_mode];
            let (before, rest) = scratch.split_at_mut(o);
            let (own, after) = rest.split_at_mut(1);
            let store = listened_to(o).then_some(&mut own[0]);
            let modulator = heard[o].map(|m| {
                let buffers: &Unison = if m < o { &before[m] } else { &after[m - o - 1] };
                let count = if m < OSC_COUNT {
                    self.osc[m].count()
                } else {
                    1
                };
                (buffers, count)
            });
            let frames = if self.morph[o].is_ready() {
                let (level, t, t_step) = self.morph[o].frames();
                Frames::Morphed { level, t, t_step }
            } else {
                Frames::Table(tables[o])
            };
            let [bus_l, bus_r] = &mut buses[oc.bus];
            if c.oversample {
                let mut fast = [[0.0f32; 2 * BLOCK]; 2];
                let [fast_l, fast_r] = &mut fast;
                self.osc[o].render(
                    frames,
                    modes,
                    &remaps[o],
                    modulator,
                    store,
                    &mut fast_l[..osc_len],
                    &mut fast_r[..osc_len],
                );
                let [dec_l, dec_r] = &mut self.decimators[o];
                dec_l.decimate_add(&fast_l[..osc_len], &mut bus_l[..len]);
                dec_r.decimate_add(&fast_r[..osc_len], &mut bus_r[..len]);
            } else {
                self.osc[o].render(
                    frames,
                    modes,
                    &remaps[o],
                    modulator,
                    store,
                    &mut bus_l[..len],
                    &mut bus_r[..len],
                );
            }
            self.morph[o].advance(osc_len);
        }

        if c.sub_on {
            used[c.sub_bus] = true;
            let [bus_l, bus_r] = &mut buses[c.sub_bus];
            let level = &bank.sub.levels[self.sub.level];
            let shape = level.frame(c.sub_shape.min(bank.sub.frames - 1));
            let sub = &mut self.sub;
            for n in 0..len {
                let x = super::tables::read(shape, level.size, level.mask, sub.phase)
                    * sub.gain
                    * CENTER;
                bus_l[n] += x;
                bus_r[n] += x;
                sub.phase += sub.inc;
                if sub.phase >= 1.0 {
                    sub.phase -= 1.0;
                }
                sub.inc += sub.inc_step;
                sub.gain += sub.gain_step;
            }
        }

        if c.noise_on {
            used[c.noise_bus] = true;
            let [bus_l, bus_r] = &mut buses[c.noise_bus];
            self.noise
                .render(c.noise_kind, &mut bus_l[..len], &mut bus_r[..len]);
        }

        // DC out of every bus in use, before the filters.
        let r = dc_pole(c.sample_rate);
        for ((bus, dc), used) in buses.iter_mut().zip(&mut self.dc).zip(used) {
            if used {
                for (channel, dc) in bus.iter_mut().zip(dc) {
                    dc.process(&mut channel[..len], r);
                }
            } else {
                *dc = [DcBlocker::default(); 2];
            }
        }

        let [direct, into1, into2] = &mut buses;
        let [filter1, filter2] = &mut self.filters;
        if c.filters[0].on {
            let [l, r] = &mut *into1;
            filter1.process(&mut l[..len], &mut r[..len]);
        }
        // Serial: filter 1's output goes on into filter 2's bus.
        let chain = !c.parallel && c.filters[1].on;
        if chain || used[BUS_FILTER2] {
            if chain && used[BUS_FILTER1] {
                add(into2, into1, len);
            }
            if c.filters[1].on {
                let [l, r] = &mut *into2;
                filter2.process(&mut l[..len], &mut r[..len]);
            }
            add(direct, into2, len);
        }
        if used[BUS_FILTER1] && !chain {
            add(direct, into1, len);
        }

        for n in 0..len {
            let gain = self.env[0].next_sample() * self.amp;
            self.amp += self.amp_step;
            left[n] += direct[0][n] * gain * self.pan[0];
            right[n] += direct[1][n] * gain * self.pan[1];
            self.pan[0] += self.pan_step[0];
            self.pan[1] += self.pan_step[1];
        }
    }
}

/// Adds the first `len` samples of both channels of `from` into `to`.
#[inline]
fn add(to: &mut [[f32; BLOCK]; 2], from: &[[f32; BLOCK]; 2], len: usize) {
    for (to, from) in to.iter_mut().zip(from) {
        for (t, f) in to[..len].iter_mut().zip(&from[..len]) {
            *t += f;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacks_keep_the_note_in_the_center_and_use_every_layer() {
        for (stack, intervals) in STACK_INTERVALS.iter().enumerate() {
            for n in 1..=MAX_UNISON {
                let center = (n - 1) / 2;
                assert_eq!(stack_interval(stack, center, n), 0.0, "stack {stack}, {n}");
                if n >= intervals.len() {
                    for interval in *intervals {
                        let found = (0..n).any(|u| stack_interval(stack, u, n) == *interval);
                        assert!(found, "stack {stack}, {n} voices: no {interval}");
                    }
                }
            }
        }
    }

    #[test]
    fn detune_modes_keep_the_ends_and_order() {
        for mode in [DETUNE_LINEAR, DETUNE_SUPER, DETUNE_EXP, DETUNE_INVERSE] {
            for n in 2..=MAX_UNISON {
                assert_eq!(detune_place(mode, 0, n, 0.0), -1.0, "mode {mode}");
                assert_eq!(detune_place(mode, n - 1, n, 0.0), 1.0, "mode {mode}");
                for u in 1..n {
                    assert!(detune_place(mode, u, n, 0.0) > detune_place(mode, u - 1, n, 0.0));
                }
            }
        }
        // Super and Exp crowd the voices toward the note, Inverse away.
        let inner = |mode| detune_place(mode, 4, 7, 0.0).abs();
        assert!(inner(DETUNE_EXP) < inner(DETUNE_SUPER));
        assert!(inner(DETUNE_SUPER) < inner(DETUNE_LINEAR));
        assert!(inner(DETUNE_INVERSE) > inner(DETUNE_LINEAR));
        assert_eq!(detune_place(DETUNE_RANDOM, 3, 7, 0.42), 0.42);
        assert_eq!(detune_place(DETUNE_RANDOM, 0, 1, 0.42), 0.0);
    }

    fn controls_listening(sources: [usize; OSC_COUNT]) -> Controls {
        let mut c = Controls::default();
        for (o, source) in sources.into_iter().enumerate() {
            c.osc[o].on = true;
            c.osc[o].warp_mode = osc::WARP_FM;
            c.osc[o].mod_source = source;
        }
        c.sub_on = true;
        c
    }

    #[test]
    fn rings_of_listeners_are_broken_and_the_heard_render_first() {
        // Auto: A hears B, B would hear A (off), C hears A.
        let (order, heard) = Voice::listening(&controls_listening([0, 0, 0]));
        assert_eq!(heard, [Some(1), None, Some(0)]);
        assert_eq!(order, [1, 0, 2]);
        // A ring of three: A hears B, B hears C, C hears A. A keeps
        // listening; B, whom A hears, stops.
        let osc = |o: usize| MOD_SOURCE_OSC + o;
        let (order, heard) = Voice::listening(&controls_listening([osc(1), osc(2), osc(0)]));
        assert_eq!(heard, [Some(1), None, Some(0)]);
        assert_eq!(order, [1, 0, 2]);
        // The sub needs nothing rendered first.
        let (order, heard) =
            Voice::listening(&controls_listening([MOD_SOURCE_SUB, osc(0), osc(1)]));
        assert_eq!(heard, [Some(SCRATCH_SUB), Some(0), Some(1)]);
        assert_eq!(order, [0, 1, 2]);
        // Listening to itself, or to something off, is nothing.
        let mut c = controls_listening([osc(0), MOD_SOURCE_NOISE, osc(1)]);
        c.osc[1].on = false;
        let (_, heard) = Voice::listening(&c);
        assert_eq!(heard, [None, None, None]);
    }

    #[test]
    fn macros_aimed_at_run_first_and_stay_in_range() {
        let mut sources = [0.0f32; SOURCE_COUNT];
        sources[macro_source(0)] = 0.5;
        sources[SRC_VELOCITY] = 1.0;
        let slots = [
            // Macro 1 drives the cutoff...
            ModSlot::new(
                macro_source(0),
                DEST_CUTOFF,
                1.0,
                SRC_NONE,
                0.0,
                POLARITY_SOURCE,
            ),
            // ...and velocity pushes macro 1 up, past its top.
            ModSlot::new(
                SRC_VELOCITY,
                DEST_MACRO1,
                0.8,
                SRC_NONE,
                0.0,
                POLARITY_SOURCE,
            ),
        ];
        let mods = run_matrix(&slots, &mut sources);
        assert_eq!(sources[macro_source(0)], 1.0);
        assert_eq!(mods[DEST_CUTOFF], 10.0, "a full macro is ten octaves");
    }
}
