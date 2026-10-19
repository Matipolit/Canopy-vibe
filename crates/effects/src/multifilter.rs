//! Filter: one multimode filter as an effect, with the families people
//! reach for in Serum's and Vital's filter sections, all built to be swept
//! hard: cutoff and resonance can be moved every sample, at audio rate, by
//! a modulator, and the filter stays stable and free of clicks.
//!
//! **Types** ([`Kind`]), in families:
//! - **Clean**: the trapezoidal state-variable filter of `filter.rs`
//!   (Simper's), as low, high and band passes of 12 or 24 dB/octave, a
//!   notch, a bell, low and high shelves, and Morph, which blends low →
//!   band → high as Var turns (as Vital's blend does). Resonance 0 is a
//!   Butterworth response; 100% a Q of [`Q_MAX`]. The 24 dB types are two
//!   filters in a row, the first a fixed Butterworth stage, the second
//!   carrying the resonance, so 0 is a flat 4-pole Butterworth.
//! - **Ladder**: the transistor ladder of the Minimoog (Serum's "MG"),
//!   four one-poles in a row with the last fed back, in the
//!   zero-delay-feedback form (Zavalishin, "The Art of VA Filter Design",
//!   ch. 5): the feedback is solved exactly for the linear filter, then the
//!   input to the first stage goes through a tanh, which bounds it and
//!   makes it self-oscillate cleanly at the top of the resonance. Low 24
//!   and 12, band and high taps mix the stages as the Oberheim Xpander did.
//!   Var is Fat: it gives back the bass the resonance takes.
//! - **Acid**: after the TB-303. Its diode ladder has a high-pass in the
//!   resonance loop, so the resonance squelches without thinning the bass,
//!   and is usually described as an 18 dB/octave filter: this is the
//!   ladder with a one-pole high-pass in its feedback (solved with it) and
//!   the third stage as the output, saturating asymmetrically. Var is
//!   Squelch: the loop's high-pass corner, 40 Hz to 1 kHz.
//! - **Dirty**: the state-variable filter with its input and its two
//!   integrators saturating (a tanh on each), driven: loud or resonant
//!   sound bends and screams instead of growing. Var is Scream: how early
//!   they saturate.
//! - **Comb** and **Flanger**, each with positive or negative feedback, as
//!   Vital's comb styles: a delay of one period of the cutoff, fed back.
//!   The comb is the loop itself (peaks at the cutoff's harmonics, or with
//!   negative feedback its odd ones, an octave down); the flanger mixes the
//!   loop's output with the input (notches between them). The delay is
//!   read between samples with a cubic, so the cutoff sweeps smoothly, and
//!   never moves by more than [`MAX_COMB_SLOPE`] samples a sample, so a
//!   fast sweep bends the pitch at most that far rather than racing
//!   through the line. Resonance is the feedback; Var is Damp, a low-pass
//!   in the loop.
//! - **Phaser**, positive or negative: [`PHASER_STAGES`] first-order
//!   all-passes around the cutoff with feedback, mixed with the input.
//!   Var is Spread: how far apart the stages are, up to four octaves.
//! - **Formant**: a voice's three formants, band-passes at the frequencies
//!   of a sung vowel, Var morphing A, E, I, O, U (after the tables in the
//!   Csound manual's appendix of formant values, tenor), the cutoff
//!   shifting them all (1 kHz leaves them where a voice has them), the
//!   resonance narrowing them.
//!
//! **Drive** pushes the sound into the filter: into the ladders' and the
//! dirty filter's own saturation, and into a tanh before the others (none
//! at 0 dB). **Spread** moves the left cutoff down and the right one up by
//! half its amount each, for stereo movement. **Gain** is the bell's and
//! the shelves'. **Mix** blends with the dry sound and **Output** sets the
//! level.
//!
//! **Modulation** (see `lib.rs`): every float glides, at the knob's pace
//! or within a block for a modulator, and every filter here keeps its
//! coefficients worked out afresh each sample while anything moves (a
//! polynomial `tan`, so it's cheap), in forms that stay stable whatever
//! the speed: trapezoidal integrators, a feedback solved without delay or
//! bounded by saturation, loops whose gain stays under one and that are
//! bent down smoothly if they get loud. Switching the type crossfades over
//! [`FADE_MS`], from a second filter that starts afresh.
//!
//! [`response`] gives the curve the panel draws: the linear filter's,
//! drive left out. The meters are the output and input peaks and where
//! the cutoffs and resonance are right now, modulation included.

use std::f32::consts::PI;

use instruments::ParamDescriptor;

use crate::line::{Ring, contain};
use crate::meter::PeakMeter;
use crate::{Category, Context, Effect, EffectDescriptor, MAX_METERS, Values, choice, float};
use dsp::one_pole::{DcBlocker, pole, tpt_lowpass};
use dsp::settle;
use dsp::svf::tan_fast as tan;
use dsp::{Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.filter",
    name: "Filter",
    description: "Multimode filter: clean, ladder, acid, dirty, comb, phaser and formant",
    category: Category::Eq,
    params: PARAMS,
    meters: &[
        "Output peak (linear, 1 = 0 dBFS)",
        "Input peak (linear, 1 = 0 dBFS)",
        "Cutoff applied, left (Hz)",
        "Cutoff applied, right (Hz)",
        "Resonance applied (0 to 1)",
    ],
    tap: true,
    create: || Box::new(Filter::new()),
    ..EffectDescriptor::BASE
};

/// The filter types, in the order of the `type` parameter's labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Low12,
    Low24,
    High12,
    High24,
    Band12,
    Band24,
    Notch,
    Morph,
    Bell,
    LowShelf,
    HighShelf,
    LadderLow24,
    LadderLow12,
    LadderBand,
    LadderHigh,
    Acid,
    DirtyLow,
    DirtyBand,
    DirtyHigh,
    CombPlus,
    CombMinus,
    FlangerPlus,
    FlangerMinus,
    PhaserPlus,
    PhaserMinus,
    Formant,
}

/// The families of [`Kind`], which share how they work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Clean,
    Ladder,
    Acid,
    Dirty,
    Comb,
    Phaser,
    Formant,
}

impl Kind {
    pub const ALL: [Kind; 26] = [
        Kind::Low12,
        Kind::Low24,
        Kind::High12,
        Kind::High24,
        Kind::Band12,
        Kind::Band24,
        Kind::Notch,
        Kind::Morph,
        Kind::Bell,
        Kind::LowShelf,
        Kind::HighShelf,
        Kind::LadderLow24,
        Kind::LadderLow12,
        Kind::LadderBand,
        Kind::LadderHigh,
        Kind::Acid,
        Kind::DirtyLow,
        Kind::DirtyBand,
        Kind::DirtyHigh,
        Kind::CombPlus,
        Kind::CombMinus,
        Kind::FlangerPlus,
        Kind::FlangerMinus,
        Kind::PhaserPlus,
        Kind::PhaserMinus,
        Kind::Formant,
    ];

    pub fn from_index(index: f32) -> Kind {
        Self::ALL[(index.round().max(0.0) as usize).min(Self::ALL.len() - 1)]
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&k| k == self).unwrap_or(0)
    }

    pub fn family(self) -> Family {
        match self {
            Kind::LadderLow24 | Kind::LadderLow12 | Kind::LadderBand | Kind::LadderHigh => {
                Family::Ladder
            }
            Kind::Acid => Family::Acid,
            Kind::DirtyLow | Kind::DirtyBand | Kind::DirtyHigh => Family::Dirty,
            Kind::CombPlus | Kind::CombMinus | Kind::FlangerPlus | Kind::FlangerMinus => {
                Family::Comb
            }
            Kind::PhaserPlus | Kind::PhaserMinus => Family::Phaser,
            Kind::Formant => Family::Formant,
            _ => Family::Clean,
        }
    }

    /// What Var does for this type, if anything: the panel's label for it.
    pub fn var_name(self) -> Option<&'static str> {
        match self.family() {
            Family::Clean if self == Kind::Morph => Some("Morph"),
            Family::Clean => None,
            Family::Ladder => Some("Fat"),
            Family::Acid => Some("Squelch"),
            Family::Dirty => Some("Scream"),
            Family::Comb => Some("Damp"),
            Family::Phaser => Some("Spread"),
            Family::Formant => Some("Vowel"),
        }
    }

    /// Whether Gain does anything for this type.
    pub fn has_gain(self) -> bool {
        matches!(self, Kind::Bell | Kind::LowShelf | Kind::HighShelf)
    }

    /// Whether the cutoff is a pitch (the combs' delay), rather than a
    /// corner.
    pub fn is_comb(self) -> bool {
        self.family() == Family::Comb
    }
}

/// The type labels, grouped by family as the panel's menu shows them.
pub const TYPES: &[&str] = &[
    "Low 12",
    "Low 24",
    "High 12",
    "High 24",
    "Band 12",
    "Band 24",
    "Notch",
    "Morph",
    "Bell",
    "Low shelf",
    "High shelf",
    "Ladder low 24",
    "Ladder low 12",
    "Ladder band",
    "Ladder high",
    "Acid",
    "Dirty low",
    "Dirty band",
    "Dirty high",
    "Comb +",
    "Comb -",
    "Flanger +",
    "Flanger -",
    "Phaser +",
    "Phaser -",
    "Formant",
];

const PARAMS: &[ParamDescriptor] = &[
    choice("type", "Type", TYPES, 1),
    float("cutoff", "Cutoff", 20.0, 20_000.0, 1000.0, "Hz", true),
    float("resonance", "Resonance", 0.0, 100.0, 20.0, "%", false),
    float("drive", "Drive", 0.0, 36.0, 0.0, "dB", false),
    float("var", "Var", 0.0, 100.0, 25.0, "%", false),
    float("gain", "Gain", -24.0, 24.0, 6.0, "dB", false),
    float("spread", "Stereo spread", -24.0, 24.0, 0.0, "st", false),
    float("mix", "Mix", 0.0, 100.0, 100.0, "%", false),
    float("output", "Output", -24.0, 24.0, 0.0, "dB", false),
];

const COUNT: usize = 9;

pub const TYPE: usize = 0;
pub const CUTOFF: usize = 1;
pub const RESONANCE: usize = 2;
pub const DRIVE: usize = 3;
/// What it does depends on the type: see [`Kind::var_name`].
pub const VAR: usize = 4;
pub const GAIN: usize = 5;
/// Semitones between the left and right cutoffs (the left lower).
pub const SPREAD: usize = 6;
pub const MIX: usize = 7;
pub const OUTPUT: usize = 8;

/// Meter slots, in [`DESCRIPTOR`]'s `meters` order. The output peak comes
/// first so that silence and a reset read 0, as [`Effect::meter`] expects.
pub const METER_OUTPUT: usize = 0;
pub const METER_INPUT: usize = 1;
pub const METER_CUTOFF_LEFT: usize = 2;
pub const METER_CUTOFF_RIGHT: usize = 3;
pub const METER_RESONANCE: usize = 4;

/// The highest Q the clean and formant filters reach at full resonance.
pub const Q_MAX: f32 = 25.0;
/// ...and the dirty one, whose saturation keeps it bounded.
const DIRTY_Q_MAX: f32 = 60.0;
/// Q of a Butterworth response, and of a 4-pole Butterworth's two stages.
const BUTTERWORTH_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;
const BUTTERWORTH_4: [f32; 2] = [0.541_196_1, 1.306_563];
/// The ladders' feedback at full resonance: a little past where they start
/// to oscillate on their own (4).
const LADDER_K: f32 = 4.1;
const ACID_K: f32 = 4.3;
/// How far the acid's saturation leans one way, for its even harmonics.
const ACID_LEAN: f32 = 0.25;
/// The acid's loop high-pass at no squelch and at full.
const ACID_HP_HZ: (f32, f32) = (40.0, 1000.0);
/// The dirty filter's states saturate around this level with no scream,
/// and this share of it at full scream.
const DIRTY_LEVEL: f32 = 1.6;
const DIRTY_SCREAM: f32 = 0.15;
/// The combs' feedback at full resonance, and their loop's damping corner
/// at full Damp (none at 0).
const COMB_FEEDBACK: f32 = 0.99;
/// While the tuning moves, the phaser's and the formants' corners (many
/// tangents) are worked out once every this many samples; the cutoff itself
/// follows every sample, and the move's last sample is always exact.
const RETUNE_EVERY: u32 = 8;
const COMB_DAMP_HZ: f32 = 400.0;
/// The phasers' stages, feedback at full resonance and widest spread.
pub const PHASER_STAGES: usize = 8;
const PHASER_FEEDBACK: f32 = 0.9;
const PHASER_SPREAD_OCTAVES: f32 = 4.0;
/// Feedback louder than this is bent down smoothly, as in the chorus.
const CEILING: f32 = 8.0;
/// The high-pass in the comb and phaser loops, against DC building up.
const DC_HZ: f32 = 10.0;
/// The lowest cutoff any filter is tuned to, and the highest as a share of
/// the sample rate (the prewarped gain grows without bound at Nyquist).
const LOWEST_HZ: f32 = 10.0;
const HIGHEST: f32 = 0.45;
/// The longest comb delay is a period of this.
const LOWEST_COMB_HZ: f32 = 20.0;
/// The fastest a comb's delay moves, in samples per sample.
const MAX_COMB_SLOPE: f32 = 0.25;
/// Drive this many dB in brings the clean filters' tanh all the way in.
const CLEAN_DRIVE_DB: f32 = 6.0;
/// How long cutoff and resonance glide for a knob: short, so a fast hand
/// is followed closely.
const TUNE_MS: f32 = 5.0;
/// How long the other values glide.
const SMOOTH_MS: f32 = 20.0;
/// How long switching the type crossfades.
pub const FADE_MS: f32 = 20.0;

/// Formants of a tenor singing A, E, I, O, U: frequency (Hz), level (dB)
/// and bandwidth (Hz) of each of the first three.
const VOWELS: [[(f32, f32, f32); 3]; 5] = [
    [
        (650.0, 0.0, 80.0),
        (1080.0, -6.0, 90.0),
        (2650.0, -7.0, 120.0),
    ],
    [
        (400.0, 0.0, 70.0),
        (1700.0, -14.0, 80.0),
        (2600.0, -12.0, 100.0),
    ],
    [
        (290.0, 0.0, 40.0),
        (1870.0, -15.0, 90.0),
        (2800.0, -18.0, 100.0),
    ],
    [
        (400.0, 0.0, 40.0),
        (800.0, -10.0, 80.0),
        (2600.0, -12.0, 100.0),
    ],
    [
        (350.0, 0.0, 40.0),
        (600.0, -20.0, 60.0),
        (2700.0, -17.0, 100.0),
    ],
];
/// The formant filter's level, so a vowel sounds about as loud as what
/// went in.
const FORMANT_LEVEL: f32 = 2.0;
/// The vowel names, for the panel.
pub const VOWEL_NAMES: [&str; 5] = ["A", "E", "I", "O", "U"];

/// `hz` as filters here are tuned to it at `sample_rate`.
#[inline]
fn tuned(hz: f32, sample_rate: f32) -> f32 {
    // max first, so NaN becomes the lowest.
    hz.max(LOWEST_HZ).min(HIGHEST * sample_rate)
}

/// The trapezoidal integrator gain for `hz`: `tan(π·hz/rate)`.
#[inline]
fn prewarp(hz: f32, sample_rate: f32) -> f32 {
    tan(PI * tuned(hz, sample_rate) / sample_rate)
}

/// Resonance 0 to 1 as a Q, from `base` (no resonance) to `top`, evenly on
/// a log scale.
#[inline]
fn q_of(resonance: f32, base: f32, top: f32) -> f32 {
    base * (top / base).powf(resonance)
}

/// The comb loop's feedback at resonance `resonance` (0 to 1).
#[inline]
fn comb_feedback(resonance: f32) -> f32 {
    COMB_FEEDBACK * resonance.max(0.0).sqrt()
}

/// The share of a feedback loop's sound kept at feedback `g`, so the loop
/// sounds about as loud on broadband sound whatever the feedback.
#[inline]
fn compensation(g: f32) -> f32 {
    (1.0 - g * g).max(0.0).sqrt()
}

/// The acid's lopsided saturation: a tanh shifted so it still passes 0,
/// scaled so it rises at 1 there as a plain tanh does.
#[inline]
fn lean_tanh(x: f32) -> f32 {
    let lean = ACID_LEAN.tanh();
    ((x + ACID_LEAN).tanh() - lean) / (1.0 - lean * lean)
}

/// One step of Simper's trapezoidal state-variable filter with tuning `g`
/// and damping `k`: the (band, low) outputs; high is `x - k·band - low`.
#[inline]
fn svf(state: &mut [f32; 2], g: f32, k: f32, x: f32) -> (f32, f32) {
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    let a3 = g * a2;
    let v3 = x - state[1];
    let v1 = a1 * state[0] + a2 * v3;
    let v2 = state[1] + a2 * state[0] + a3 * v3;
    state[0] = 2.0 * v1 - state[0];
    state[1] = 2.0 * v2 - state[1];
    (v1, v2)
}

/// Where the formants are and how loud and wide, Var `vowel` (0 to 1)
/// along A, E, I, O, U: (Hz, linear level, bandwidth Hz) each.
fn formants(vowel: f32) -> [(f32, f32, f32); 3] {
    let at = vowel.clamp(0.0, 1.0) * (VOWELS.len() - 1) as f32;
    let first = (at as usize).min(VOWELS.len() - 2);
    let t = at - first as f32;
    std::array::from_fn(|i| {
        let (f0, l0, b0) = VOWELS[first][i];
        let (f1, l1, b1) = VOWELS[first + 1][i];
        let hz = (f0.ln() + t * (f1.ln() - f0.ln())).exp();
        let level = dsp::db_to_gain(l0 + t * (l1 - l0));
        (hz, level, b0 + t * (b1 - b0))
    })
}

/// How far the formants move for a cutoff of `hz`: a third of its octaves
/// away from 1 kHz.
#[inline]
fn formant_shift(hz: f32) -> f32 {
    (hz / 1000.0).max(1e-3).cbrt()
}

/// The phaser's stage corners for a cutoff of `hz` and Var `spread`.
#[inline]
fn phaser_corner(hz: f32, spread: f32, stage: usize) -> f32 {
    let at = stage as f32 / (PHASER_STAGES - 1) as f32 - 0.5;
    hz * (PHASER_SPREAD_OCTAVES * spread * at).exp2()
}

/// What every channel and every type needs to know this sample.
#[derive(Debug, Clone, Copy, Default)]
struct Frame {
    /// The cutoff (Hz) and its prewarped tuning, per side.
    hz: [f32; 2],
    g: [f32; 2],
    resonance: f32,
    var: f32,
    gain_db: f32,
    /// Drive as a gain, and how far the clean filters' tanh is in.
    drive: f32,
    saturation: f32,
    /// Per side, worked out only while a phaser or the formant filter is
    /// heard: each phaser stage's one-pole gain, and each formant's level,
    /// damping and tuning.
    phaser: [[f32; PHASER_STAGES]; 2],
    formants: [[(f32, f32, f32); 3]; 2],
}

/// What depends only on the sample rate.
#[derive(Debug, Clone, Copy, Default)]
struct Rates {
    sample_rate: f32,
    /// The loops' DC blocker pole.
    dc_pole: f32,
    /// The longest comb delay, in samples.
    longest: f32,
}

/// One channel's memory, for every family (only the playing type's is in
/// use).
#[derive(Debug, Clone, Default)]
struct Channel {
    svf: [[f32; 2]; 3],
    ladder: [f32; 4],
    loop_high_pass: f32,
    comb: Ring,
    /// Where the comb's line is read (samples), moving towards a period of
    /// the cutoff no faster than [`MAX_COMB_SLOPE`]; and whether it has
    /// been placed since the line was cleared.
    comb_delay: f32,
    comb_placed: bool,
    damp: f32,
    /// DC blockers' last input and output: the comb's and the phaser's.
    comb_dc: DcBlocker,
    phaser_dc: DcBlocker,
    phaser: [f32; PHASER_STAGES],
    phaser_out: f32,
}

impl Channel {
    fn clear(&mut self) {
        self.svf = [[0.0; 2]; 3];
        self.ladder = [0.0; 4];
        self.loop_high_pass = 0.0;
        self.comb.clear();
        self.comb_placed = false;
        self.damp = 0.0;
        self.comb_dc = DcBlocker::default();
        self.phaser_dc = DcBlocker::default();
        self.phaser = [0.0; PHASER_STAGES];
        self.phaser_out = 0.0;
    }

    fn settle(&mut self) {
        for x in self
            .svf
            .iter_mut()
            .flatten()
            .chain(&mut self.ladder)
            .chain(&mut self.phaser)
        {
            *x = settle(*x);
        }
        self.comb_dc.flush();
        self.phaser_dc.flush();
        for x in [
            &mut self.loop_high_pass,
            &mut self.damp,
            &mut self.phaser_out,
        ] {
            *x = settle(*x);
        }
    }

    /// One sample of `kind` on `side`.
    #[inline]
    fn tick(&mut self, kind: Kind, x: f32, side: usize, f: &Frame, rates: &Rates) -> f32 {
        let g = f.g[side];
        let r = f.resonance;
        // The clean families' drive: a tanh blended in.
        let clean = |x: f32| {
            if f.saturation > 0.0 {
                x + f.saturation * ((f.drive * x).tanh() - x)
            } else {
                x
            }
        };
        match kind.family() {
            Family::Clean => self.clean(kind, clean(x), g, f),
            Family::Ladder => {
                let k = LADDER_K * r;
                // The first stage's input and the four stages' outputs,
                // mixed as the Xpander's modes are.
                let [a, b, c, d, e] = ladder(&mut self.ladder, g, k, f.drive * x, f32::tanh);
                let fat = 1.0 + f.var * k;
                match kind {
                    Kind::LadderLow24 => fat * e,
                    Kind::LadderLow12 => fat * c,
                    Kind::LadderBand => 4.0 * (c - 2.0 * d + e),
                    _ => a - 4.0 * b + 6.0 * c - 4.0 * d + e,
                }
            }
            Family::Acid => {
                let k = ACID_K * r;
                let corner = ACID_HP_HZ.0 * (ACID_HP_HZ.1 / ACID_HP_HZ.0).powf(f.var);
                let gh = prewarp(corner, rates.sample_rate);
                acid(
                    &mut self.ladder,
                    &mut self.loop_high_pass,
                    g,
                    gh,
                    k,
                    f.drive * x,
                )
            }
            Family::Dirty => {
                let k = 1.0 / q_of(r, BUTTERWORTH_Q, DIRTY_Q_MAX);
                let level = DIRTY_LEVEL * (1.0 - (1.0 - DIRTY_SCREAM) * f.var);
                let x = level * (f.drive * x / level).tanh();
                let (band, low) = svf(&mut self.svf[0], g, k, x);
                for state in &mut self.svf[0] {
                    *state = level * (*state / level).tanh();
                }
                match kind {
                    Kind::DirtyLow => low,
                    Kind::DirtyBand => k * band,
                    _ => x - k * band - low,
                }
            }
            Family::Comb => self.comb(kind, clean(x), f.hz[side], f, rates),
            Family::Phaser => self.phaser(kind, clean(x), side, f, rates),
            Family::Formant => self.formant(clean(x), side, f),
        }
    }

    #[inline]
    fn clean(&mut self, kind: Kind, x: f32, g: f32, f: &Frame) -> f32 {
        let r = f.resonance;
        match kind {
            Kind::Low24 | Kind::High24 => {
                let k1 = 1.0 / BUTTERWORTH_4[0];
                let k2 = 1.0 / q_of(r, BUTTERWORTH_4[1], Q_MAX);
                let [first, second, _] = &mut self.svf;
                let (b1, l1) = svf(first, g, k1, x);
                if kind == Kind::Low24 {
                    svf(second, g, k2, l1).1
                } else {
                    let h1 = x - k1 * b1 - l1;
                    let (b2, l2) = svf(second, g, k2, h1);
                    h1 - k2 * b2 - l2
                }
            }
            Kind::Band24 => {
                let k = 1.0 / q_of(r, BUTTERWORTH_Q, Q_MAX);
                let [first, second, _] = &mut self.svf;
                let b1 = k * svf(first, g, k, x).0;
                k * svf(second, g, k, b1).0
            }
            Kind::Bell => {
                let a = dsp::db_to_gain(f.gain_db * 0.5);
                let k = 1.0 / (q_of(r, 0.5, 16.0) * a);
                let band = svf(&mut self.svf[0], g, k, x).0;
                x + k * (a * a - 1.0) * band
            }
            Kind::LowShelf | Kind::HighShelf => {
                let a = dsp::db_to_gain(f.gain_db * 0.5);
                let k = 1.0 / q_of(r, BUTTERWORTH_Q, 4.0);
                let root = a.sqrt();
                if kind == Kind::LowShelf {
                    let (band, low) = svf(&mut self.svf[0], g / root, k, x);
                    x + k * (a - 1.0) * band + (a * a - 1.0) * low
                } else {
                    let (band, low) = svf(&mut self.svf[0], g * root, k, x);
                    a * a * x + k * (1.0 - a) * a * band + (1.0 - a * a) * low
                }
            }
            _ => {
                let k = 1.0 / q_of(r, BUTTERWORTH_Q, Q_MAX);
                let (band, low) = svf(&mut self.svf[0], g, k, x);
                let high = x - k * band - low;
                match kind {
                    Kind::Low12 => low,
                    Kind::High12 => high,
                    Kind::Band12 => k * band,
                    Kind::Notch => x - k * band,
                    _ => {
                        // Morph: low, then band, then high.
                        let t = 2.0 * f.var;
                        if t <= 1.0 {
                            low + t * (k * band - low)
                        } else {
                            k * band + (t - 1.0) * (high - k * band)
                        }
                    }
                }
            }
        }
    }

    #[inline]
    fn comb(&mut self, kind: Kind, x: f32, hz: f32, f: &Frame, rates: &Rates) -> f32 {
        let wanted = (rates.sample_rate / hz.max(LOWEST_COMB_HZ)).clamp(2.0, rates.longest);
        self.comb_delay = if self.comb_placed {
            self.comb_delay + (wanted - self.comb_delay).clamp(-MAX_COMB_SLOPE, MAX_COMB_SLOPE)
        } else {
            wanted
        };
        self.comb_placed = true;
        let delayed = self.comb.delayed(self.comb_delay);
        let damping = COMB_DAMP_HZ * (HIGHEST * rates.sample_rate / COMB_DAMP_HZ).powf(1.0 - f.var);
        let gd = prewarp(damping, rates.sample_rate);
        let damped = tpt_lowpass(&mut self.damp, gd / (1.0 + gd), delayed);
        let blocked = self.comb_dc.tick(damped, rates.dc_pole);
        let g = comb_feedback(f.resonance);
        let kept = compensation(g);
        let sign = if matches!(kind, Kind::CombPlus | Kind::FlangerPlus) {
            1.0
        } else {
            -1.0
        };
        let fed = x + sign * g * blocked;
        self.comb.push(settle(contain(fed, CEILING)));
        match kind {
            Kind::CombPlus | Kind::CombMinus => kept * fed,
            _ => 0.5 * kept * (x + sign * damped),
        }
    }

    #[inline]
    fn phaser(&mut self, kind: Kind, x: f32, side: usize, f: &Frame, rates: &Rates) -> f32 {
        let sign = if kind == Kind::PhaserPlus { 1.0 } else { -1.0 };
        let feedback = sign * PHASER_FEEDBACK * f.resonance;
        let returned = self.phaser_dc.tick(self.phaser_out, rates.dc_pole);
        let mut y = x + feedback * returned;
        for (state, &gain) in self.phaser.iter_mut().zip(&f.phaser[side]) {
            let low = tpt_lowpass(state, gain, y);
            y = 2.0 * low - y;
        }
        self.phaser_out = contain(y, CEILING);
        0.5 * compensation(feedback) * (x + sign * y)
    }

    #[inline]
    fn formant(&mut self, x: f32, side: usize, f: &Frame) -> f32 {
        let mut sum = 0.0;
        for (state, &(level, k, g)) in self.svf.iter_mut().zip(&f.formants[side]) {
            sum += level * k * svf(state, g, k, x).0;
        }
        FORMANT_LEVEL * sum
    }
}

/// One step of the zero-delay-feedback ladder: four trapezoidal one-poles
/// with tuning `g`, the last fed back by `k`; the feedback is solved for
/// the linear filter, then the first stage's input goes through
/// `saturate`. Returns that input and the four stages' outputs.
#[inline]
fn ladder(state: &mut [f32; 4], g: f32, k: f32, x: f32, saturate: impl Fn(f32) -> f32) -> [f32; 5] {
    let big_g = g / (1.0 + g);
    let rest = 1.0 - big_g;
    let (g2, g3) = (big_g * big_g, big_g * big_g * big_g);
    let g4 = g2 * g2;
    // Each stage is `G·in + (1-G)·state`, so the last is `G⁴·u + sum`.
    let sum = rest * (g3 * state[0] + g2 * state[1] + big_g * state[2] + state[3]);
    let last = (g4 * x + sum) / (1.0 + k * g4);
    let u = saturate(x - k * last);
    let mut out = [u, 0.0, 0.0, 0.0, 0.0];
    let mut y = u;
    for (i, s) in state.iter_mut().enumerate() {
        y = tpt_lowpass(s, big_g, y);
        out[i + 1] = y;
    }
    out
}

/// One step of the acid ladder: [`ladder`] with a one-pole high-pass
/// (tuning `gh`, memory `high_pass`) in its feedback, solved with it, a
/// lopsided saturation, and the third stage out.
#[inline]
fn acid(state: &mut [f32; 4], high_pass: &mut f32, g: f32, gh: f32, k: f32, x: f32) -> f32 {
    let big_g = g / (1.0 + g);
    let rest = 1.0 - big_g;
    let (g2, g3) = (big_g * big_g, big_g * big_g * big_g);
    let g4 = g2 * g2;
    let sum = rest * (g3 * state[0] + g2 * state[1] + big_g * state[2] + state[3]);
    let big_gh = gh / (1.0 + gh);
    // The high-pass of the last stage is `(1-Gh)·(last - state)`.
    let pass = k * (1.0 - big_gh);
    let u = lean_tanh((x - pass * (sum - *high_pass)) / (1.0 + pass * g4));
    let mut y = u;
    let mut third = 0.0;
    for (i, s) in state.iter_mut().enumerate() {
        y = tpt_lowpass(s, big_g, y);
        if i == 2 {
            third = y;
        }
    }
    tpt_lowpass(high_pass, big_gh, y);
    third
}

/// The response of `kind` at these parameter values (in [`DESCRIPTOR`]'s
/// order; missing or NaN ones count as defaults), as a complex gain at
/// `freq` Hz with the cutoff at `cutoff` Hz: the linear filter, drive, mix
/// and output left out.
fn filter_gain(values: &[f32], cutoff: f32, freq: f32, sample_rate: f32) -> (f64, f64) {
    type C = (f64, f64);
    let mul = |a: C, b: C| (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0);
    let div = |a: C, b: C| {
        let d = b.0 * b.0 + b.1 * b.1;
        ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
    };
    let add = |a: C, b: C| (a.0 + b.0, a.1 + b.1);
    let scale = |a: C, s: f64| (a.0 * s, a.1 * s);
    let one: C = (1.0, 0.0);

    let value = |index: usize| value_of(values, index);
    let kind = Kind::from_index(value(TYPE));
    let r = value(RESONANCE) / 100.0;
    let var = value(VAR) / 100.0;
    let rate = f64::from(sample_rate);
    let omega = std::f64::consts::TAU * f64::from(freq.clamp(0.0, 0.4999 * sample_rate)) / rate;
    // The bilinear transform's frequency, against a tuning `g`.
    let t = (omega / 2.0).tan();
    let s_of = |hz: f32| -> C { (0.0, t / f64::from(prewarp(hz, sample_rate))) };
    // A state-variable filter's output mix `(m0, m1, m2)` with damping `k`.
    let svf = |s: C, k: f64, m: [f64; 3]| -> C {
        let s2 = mul(s, s);
        let den = add(add(s2, scale(s, k)), one);
        let num = add(add(scale(den, m[0]), scale(s, m[1])), (m[2], 0.0));
        div(num, den)
    };
    let s = s_of(cutoff);
    match kind.family() {
        Family::Clean | Family::Dirty => {
            let top = if kind.family() == Family::Dirty {
                DIRTY_Q_MAX
            } else {
                Q_MAX
            };
            let k = f64::from(1.0 / q_of(r, BUTTERWORTH_Q, top));
            let low = [0.0, 0.0, 1.0];
            let band = [0.0, k, 0.0];
            let high = [1.0, -k, -1.0];
            match kind {
                Kind::Low12 | Kind::DirtyLow => svf(s, k, low),
                Kind::High12 | Kind::DirtyHigh => svf(s, k, high),
                Kind::Band12 | Kind::DirtyBand => svf(s, k, band),
                Kind::Notch => svf(s, k, [1.0, -k, 0.0]),
                Kind::Band24 => mul(svf(s, k, band), svf(s, k, band)),
                Kind::Low24 | Kind::High24 => {
                    let k1 = f64::from(1.0 / BUTTERWORTH_4[0]);
                    let k2 = f64::from(1.0 / q_of(r, BUTTERWORTH_4[1], Q_MAX));
                    if kind == Kind::Low24 {
                        mul(svf(s, k1, [0.0, 0.0, 1.0]), svf(s, k2, [0.0, 0.0, 1.0]))
                    } else {
                        mul(svf(s, k1, [1.0, -k1, -1.0]), svf(s, k2, [1.0, -k2, -1.0]))
                    }
                }
                Kind::Bell => {
                    let a = f64::from(dsp::db_to_gain(value(GAIN) * 0.5));
                    let k = 1.0 / (f64::from(q_of(r, 0.5, 16.0)) * a);
                    svf(s, k, [1.0, k * (a * a - 1.0), 0.0])
                }
                Kind::LowShelf | Kind::HighShelf => {
                    let a = f64::from(dsp::db_to_gain(value(GAIN) * 0.5));
                    let k = f64::from(1.0 / q_of(r, BUTTERWORTH_Q, 4.0));
                    if kind == Kind::LowShelf {
                        svf(scale(s, a.sqrt()), k, [1.0, k * (a - 1.0), a * a - 1.0])
                    } else {
                        let m = [a * a, k * (1.0 - a) * a, 1.0 - a * a];
                        svf(scale(s, 1.0 / a.sqrt()), k, m)
                    }
                }
                _ => {
                    let t = 2.0 * f64::from(var);
                    let m = if t <= 1.0 {
                        [0.0, t * k, 1.0 - t]
                    } else {
                        [t - 1.0, k * (1.0 - (t - 1.0)) - k * (t - 1.0), -(t - 1.0)]
                    };
                    svf(s, k, m)
                }
            }
        }
        Family::Ladder | Family::Acid => {
            let l = div(one, add(one, s));
            let l2 = mul(l, l);
            let l4 = mul(l2, l2);
            if kind == Kind::Acid {
                let k = f64::from(ACID_K * r);
                let corner = ACID_HP_HZ.0 * (ACID_HP_HZ.1 / ACID_HP_HZ.0).powf(var);
                let sh = s_of(corner);
                let hp = div(sh, add(one, sh));
                let u = div(one, add(one, scale(mul(l4, hp), k)));
                return mul(mul(l2, l), u);
            }
            let k = f64::from(LADDER_K * r);
            let u = div(one, add(one, scale(l4, k)));
            let fat = 1.0 + f64::from(var) * k;
            let rest = add(one, scale(l, -1.0));
            let rest2 = mul(rest, rest);
            match kind {
                Kind::LadderLow24 => scale(mul(l4, u), fat),
                Kind::LadderLow12 => scale(mul(l2, u), fat),
                Kind::LadderBand => scale(mul(mul(l2, rest2), u), 4.0),
                _ => mul(mul(rest2, rest2), u),
            }
        }
        Family::Comb => {
            let delay = (sample_rate / cutoff.max(LOWEST_COMB_HZ)).max(2.0);
            let late = (
                (-omega * f64::from(delay)).cos(),
                (-omega * f64::from(delay)).sin(),
            );
            let damping = COMB_DAMP_HZ * (HIGHEST * sample_rate / COMB_DAMP_HZ).powf(1.0 - var);
            let sd = s_of(damping);
            let damped = mul(late, div(one, add(one, sd)));
            let blocked = mul(damped, dc_blocker(omega, sample_rate));
            let g = f64::from(comb_feedback(r));
            let kept = f64::from(compensation(g as f32));
            let sign = if matches!(kind, Kind::CombPlus | Kind::FlangerPlus) {
                1.0
            } else {
                -1.0
            };
            let fed = div(one, add(one, scale(blocked, -sign * g)));
            match kind {
                Kind::CombPlus | Kind::CombMinus => scale(fed, kept),
                _ => scale(add(one, scale(mul(damped, fed), sign)), 0.5 * kept),
            }
        }
        Family::Phaser => {
            let sign = if kind == Kind::PhaserPlus { 1.0 } else { -1.0 };
            let feedback = sign * f64::from(PHASER_FEEDBACK * r);
            let mut chain = one;
            for stage in 0..PHASER_STAGES {
                let si = s_of(phaser_corner(cutoff, var, stage));
                chain = mul(chain, div(add(one, scale(si, -1.0)), add(one, si)));
            }
            let late = mul((omega.cos(), -omega.sin()), dc_blocker(omega, sample_rate));
            let looped = div(chain, add(one, scale(mul(chain, late), -feedback)));
            scale(
                add(one, scale(looped, sign)),
                0.5 * f64::from(compensation(feedback as f32)),
            )
        }
        Family::Formant => {
            let shift = formant_shift(cutoff);
            let narrow = 1.0 - 0.8 * r;
            let mut sum = (0.0, 0.0);
            for (center, level, width) in formants(var) {
                let center = tuned(center * shift, sample_rate);
                let k = f64::from(width * narrow / center);
                sum = add(
                    sum,
                    scale(svf(s_of(center), k, [0.0, k, 0.0]), f64::from(level)),
                );
            }
            scale(sum, f64::from(FORMANT_LEVEL))
        }
    }
}

/// The loops' DC blocker, `(1 - z⁻¹) / (1 - p·z⁻¹)`, at `omega` radians a
/// sample.
fn dc_blocker(omega: f64, sample_rate: f32) -> (f64, f64) {
    let pole = f64::from((-std::f32::consts::TAU * DC_HZ / sample_rate).exp());
    let (c, s) = (omega.cos(), omega.sin());
    // z⁻¹ = c - j·s.
    let num = (1.0 - c, s);
    let den = (1.0 - pole * c, pole * s);
    let d = den.0 * den.0 + den.1 * den.1;
    (
        (num.0 * den.0 + num.1 * den.1) / d,
        (num.1 * den.0 - num.0 * den.1) / d,
    )
}

/// Parameter `index`'s value from `values`, or its default where it's
/// missing or NaN.
fn value_of(values: &[f32], index: usize) -> f32 {
    crate::param_value(PARAMS, values, index)
}

/// The gain of the left side at `freq` Hz, at these parameter values (in
/// [`DESCRIPTOR`]'s order), with the cutoff at `cutoff` Hz (the knob's,
/// or where a modulator has it): the linear filter (drive left out), mix
/// and output included. What the panel draws.
pub fn response(values: &[f32], cutoff: f32, freq: f32, sample_rate: f32) -> f32 {
    let (re, im) = filter_gain(values, cutoff, freq, sample_rate);
    let mix = f64::from(value_of(values, MIX) / 100.0);
    let (re, im) = (1.0 - mix + mix * re, mix * im);
    let output = f64::from(dsp::db_to_gain(value_of(values, OUTPUT)));
    ((re * re + im * im).sqrt() * output) as f32
}

/// One filter, with a type and both channels' memory.
#[derive(Debug, Clone)]
struct Engine {
    kind: Kind,
    channels: [Channel; 2],
}

impl Engine {
    fn clear(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
    }
}

pub struct Filter {
    values: Values<COUNT>,
    rates: Rates,
    /// The filter heard, and the one fading out while the type switches.
    engines: [Engine; 2],
    active: usize,
    fading: bool,
    /// How far the switch has got (0 to 1) and its step per sample.
    fade: f32,
    fade_step: f32,
    /// The cutoff (log2 Hz), resonance (0 to 1), Var (0 to 1), gain (dB),
    /// drive (dB), spread (semitones), mix and output (a gain).
    log_cutoff: Smoothed,
    resonance: Smoothed,
    var: Smoothed,
    gain: Smoothed,
    drive: Smoothed,
    spread: Smoothed,
    mix: Smoothed,
    output: Smoothed,
    /// This sample's tuning, kept while nothing moves.
    frame: Frame,
    /// Samples until the phaser's and formants' corners are next worked out.
    retune: u32,
    input_peak: PeakMeter,
    output_peak: PeakMeter,
}

impl Filter {
    pub fn new() -> Filter {
        let engine = Engine {
            kind: Kind::Low24,
            channels: [Channel::default(), Channel::default()],
        };
        let mut filter = Filter {
            values: Values::new(PARAMS),
            rates: Rates::default(),
            engines: [engine.clone(), engine],
            active: 0,
            fading: false,
            fade: 0.0,
            fade_step: 1.0,
            log_cutoff: Smoothed::new(10.0, TUNE_MS),
            resonance: Smoothed::new(0.0, TUNE_MS),
            var: Smoothed::new(0.0, SMOOTH_MS),
            gain: Smoothed::new(0.0, SMOOTH_MS),
            drive: Smoothed::new(0.0, SMOOTH_MS),
            spread: Smoothed::new(0.0, SMOOTH_MS),
            mix: Smoothed::new(1.0, SMOOTH_MS),
            output: Smoothed::new(1.0, SMOOTH_MS),
            frame: Frame::default(),
            retune: 0,
            input_peak: PeakMeter::default(),
            output_peak: PeakMeter::default(),
        };
        filter.prepare(48_000.0, 0);
        filter
    }

    fn smoothers(&mut self) -> [&mut Smoothed; 8] {
        [
            &mut self.log_cutoff,
            &mut self.resonance,
            &mut self.var,
            &mut self.gain,
            &mut self.drive,
            &mut self.spread,
            &mut self.mix,
            &mut self.output,
        ]
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`. The type switches in `process`, with a crossfade.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            CUTOFF => self.log_cutoff.move_to(value.log2(), pace),
            RESONANCE => self.resonance.move_to(value / 100.0, pace),
            VAR => self.var.move_to(value / 100.0, pace),
            GAIN => self.gain.move_to(value, pace),
            DRIVE => self.drive.move_to(value, pace),
            SPREAD => self.spread.move_to(value, pace),
            MIX => self.mix.move_to(value / 100.0, pace),
            OUTPUT => self.output.move_to(dsp::db_to_gain(value), pace),
            _ => {}
        }
    }

    fn wanted_kind(&self) -> Kind {
        Kind::from_index(self.values[TYPE])
    }

    /// Works this sample's tuning out from the smoothed values.
    fn update_frame(&mut self) {
        let sample_rate = self.rates.sample_rate;
        let cutoff = self.log_cutoff.current();
        let half = self.spread.current() / 24.0;
        let hz = [(cutoff - half).exp2(), (cutoff + half).exp2()];
        let drive_db = self.drive.current();
        let (resonance, var) = (
            self.resonance.current().clamp(0.0, 1.0),
            self.var.current().clamp(0.0, 1.0),
        );
        // The many corners only every few samples of a move.
        let retune = self.retune == 0 || !self.tuning_moves();
        self.retune = if retune {
            RETUNE_EVERY - 1
        } else {
            self.retune - 1
        };
        // The families heard: the playing type's, and while switching the
        // one fading out.
        let heard = |family: Family| {
            self.engines[self.active].kind.family() == family
                || (self.fading && self.engines[1 - self.active].kind.family() == family)
        };
        let (mut phaser, mut formants) = (self.frame.phaser, self.frame.formants);
        if retune {
            phaser = [[0.0; PHASER_STAGES]; 2];
            formants = [[(0.0, 0.0, 0.0); 3]; 2];
        }
        if retune && heard(Family::Phaser) {
            for (side, gains) in phaser.iter_mut().enumerate() {
                for (stage, gain) in gains.iter_mut().enumerate() {
                    let g = prewarp(phaser_corner(hz[side], var, stage), sample_rate);
                    *gain = g / (1.0 + g);
                }
            }
        }
        if retune && heard(Family::Formant) {
            let narrow = 1.0 - 0.8 * resonance;
            for (side, tuning) in formants.iter_mut().enumerate() {
                let shift = formant_shift(hz[side]);
                for (tuning, (center, level, width)) in tuning.iter_mut().zip(self::formants(var)) {
                    let center = tuned(center * shift, sample_rate);
                    let k = width * narrow / center;
                    *tuning = (level, k, prewarp(center, sample_rate));
                }
            }
        }
        self.frame = Frame {
            hz,
            g: hz.map(|hz| prewarp(hz, sample_rate)),
            resonance,
            var,
            gain_db: self.gain.current(),
            drive: dsp::db_to_gain(drive_db),
            saturation: (drive_db / CLEAN_DRIVE_DB).clamp(0.0, 1.0),
            phaser,
            formants,
        };
    }

    /// Whether any value that tunes the filter is moving.
    fn tuning_moves(&self) -> bool {
        self.log_cutoff.is_smoothing()
            || self.resonance.is_smoothing()
            || self.var.is_smoothing()
            || self.gain.is_smoothing()
            || self.drive.is_smoothing()
            || self.spread.is_smoothing()
    }
}

impl Default for Filter {
    fn default() -> Self {
        Filter::new()
    }
}

impl Effect for Filter {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.rates = Rates {
            sample_rate,
            dc_pole: pole(DC_HZ, sample_rate),
            longest: (sample_rate / LOWEST_COMB_HZ).ceil(),
        };
        let len = self.rates.longest as usize + 4;
        for engine in &mut self.engines {
            for channel in &mut engine.channels {
                channel.comb.allocate(len);
            }
        }
        for smoothed in self.smoothers() {
            smoothed.prepare(sample_rate);
        }
        self.fade_step = 1.0 / (FADE_MS * 0.001 * sample_rate).max(1.0);
        self.input_peak.prepare(sample_rate);
        self.output_peak.prepare(sample_rate);
        for index in 0..COUNT {
            self.apply(index, Pace::Knob);
        }
        self.reset();
    }

    fn reset(&mut self) {
        for smoothed in self.smoothers() {
            smoothed.settle();
        }
        let kind = self.wanted_kind();
        for engine in &mut self.engines {
            engine.kind = kind;
            engine.clear();
        }
        self.active = 0;
        self.fading = false;
        self.fade = 0.0;
        self.retune = 0;
        self.update_frame();
        self.input_peak.reset();
        self.output_peak.reset();
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.values.played();
        // A switch of type starts when the last one has ended.
        let wanted = self.wanted_kind();
        if !self.fading && wanted != self.engines[self.active].kind {
            let next = 1 - self.active;
            self.engines[next].kind = wanted;
            self.engines[next].clear();
            self.active = next;
            self.fading = true;
            self.fade = 0.0;
            // Tuned for both.
            self.retune = 0;
            self.update_frame();
        }
        let rates = self.rates;
        let len = left.len().min(right.len());
        let (mut input_peak, mut output_peak) = (0.0f32, 0.0f32);
        // A copy of the tuning, taken again only when it moves: it holds
        // every family's coefficients, more than is worth copying a sample.
        let mut frame = self.frame;
        for n in 0..len {
            if self.tuning_moves() {
                for smoothed in [
                    &mut self.log_cutoff,
                    &mut self.resonance,
                    &mut self.var,
                    &mut self.gain,
                    &mut self.drive,
                    &mut self.spread,
                ] {
                    smoothed.tick();
                }
                self.update_frame();
                frame = self.frame;
            }
            let mix = self.mix.tick();
            let output = self.output.tick();
            // A non-finite sample would stick in the filters.
            let clean = |x: f32| if x.is_finite() { x } else { 0.0 };
            let dry = [clean(left[n]), clean(right[n])];
            input_peak = input_peak.max(dry[0].abs()).max(dry[1].abs());
            let mut wet = [0.0f32; 2];
            let [first, second] = &mut self.engines;
            let (heard, fading) = if self.active == 0 {
                (first, second)
            } else {
                (second, first)
            };
            for (side, wet) in wet.iter_mut().enumerate() {
                *wet = heard.channels[side].tick(heard.kind, dry[side], side, &frame, &rates);
            }
            if self.fading {
                self.fade = (self.fade + self.fade_step).min(1.0);
                for (side, wet) in wet.iter_mut().enumerate() {
                    let old =
                        fading.channels[side].tick(fading.kind, dry[side], side, &frame, &rates);
                    *wet = old + self.fade * (*wet - old);
                }
                if self.fade >= 1.0 {
                    self.fading = false;
                }
            }
            for side in 0..2 {
                let out = output * (dry[side] + mix * (wet[side] - dry[side]));
                output_peak = output_peak.max(out.abs());
                if side == 0 {
                    left[n] = out;
                } else {
                    right[n] = out;
                }
            }
        }
        for engine in &mut self.engines {
            for channel in &mut engine.channels {
                channel.settle();
            }
        }
        self.input_peak.update(input_peak, len);
        self.output_peak.update(output_peak, len);
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn receive(&mut self, index: usize, value: f32, pace: Pace) {
        if self.values.set(index, value) {
            self.apply(index, pace);
            if self.values.is_fresh() {
                self.reset();
            }
        }
    }

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out[METER_OUTPUT] = self.output_peak.level();
        out[METER_INPUT] = self.input_peak.level();
        out[METER_CUTOFF_LEFT] = self.frame.hz[0];
        out[METER_CUTOFF_RIGHT] = self.frame.hz[1];
        out[METER_RESONANCE] = self.frame.resonance;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, db, noise, peak, rms, run, set, sine, values};

    fn filter(settings: &[(&str, f32)]) -> Filter {
        let mut filter = Filter::new();
        filter.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut filter, id, value);
        }
        filter.reset();
        filter
    }

    /// The level of a quiet sine at `freq` Hz through `filter` (settled),
    /// in dB against the input: quiet enough that the saturating types are
    /// as good as linear.
    fn sine_gain(filter: &mut Filter, freq: f64) -> f64 {
        let input = sine(freq, 0.01, 48_000, RATE);
        let (mut left, mut right) = (input.clone(), input.clone());
        run(filter, &mut left, &mut right, 64, &Context::DEFAULT);
        db(rms(&left[38_400..]) / rms(&input[38_400..]))
    }

    #[test]
    fn parameters_are_laid_out_as_documented() {
        assert_eq!(PARAMS.len(), COUNT);
        let ids = [
            (TYPE, "type"),
            (CUTOFF, "cutoff"),
            (RESONANCE, "resonance"),
            (DRIVE, "drive"),
            (VAR, "var"),
            (GAIN, "gain"),
            (SPREAD, "spread"),
            (MIX, "mix"),
            (OUTPUT, "output"),
        ];
        for (index, id) in ids {
            assert_eq!(PARAMS[index].id, id);
        }
        assert_eq!(TYPES.len(), Kind::ALL.len());
        for (index, kind) in Kind::ALL.iter().enumerate() {
            assert_eq!(Kind::from_index(index as f32), *kind);
            assert_eq!(kind.index(), index);
        }
    }

    #[test]
    fn the_polynomial_tan_is_close_enough() {
        for i in 1..=1000 {
            let x = 1.45 * i as f32 / 1000.0;
            let error = (tan(x) / x.tan() - 1.0).abs();
            assert!(error < 2e-5, "{x}: {error}");
        }
    }

    #[test]
    fn every_type_sounds_as_its_curve_is_drawn() {
        for kind in Kind::ALL {
            let cutoff = if kind.is_comb() { 220.0 } else { 1000.0 };
            let settings = [
                ("type", kind.index() as f32),
                ("cutoff", cutoff),
                ("resonance", 30.0),
                ("var", 40.0),
                ("gain", 9.0),
            ];
            let drawn = values(PARAMS, &settings);
            let mut worst = 0.0f64;
            for freq in [90.0, 300.0, 700.0, 1000.0, 1500.0, 3000.0, 8000.0] {
                let measured = sine_gain(&mut filter(&settings), freq);
                let expected = db(f64::from(response(&drawn, cutoff, freq as f32, RATE)));
                if expected > -40.0 {
                    worst = worst.max((measured - expected).abs());
                    assert!(
                        (measured - expected).abs() < 0.5,
                        "{kind:?} at {freq} Hz: measured {measured:.2} dB, drawn {expected:.2}"
                    );
                }
            }
            println!("{kind:?}: within {worst:.3} dB of its curve");
        }
    }

    #[test]
    fn the_basic_shapes_pass_and_stop_what_they_should() {
        let gain = |kind: Kind, settings: &[(&str, f32)], freq: f64| {
            let mut all = vec![("type", kind.index() as f32), ("resonance", 0.0)];
            all.extend_from_slice(settings);
            sine_gain(&mut filter(&all), freq)
        };
        // At no resonance, 12 and 24 dB/octave Butterworths: -3 dB at the
        // cutoff, and -24 or -48 dB two octaves past it.
        for (kind, stop_db) in [(Kind::Low12, -24.0), (Kind::Low24, -48.0)] {
            assert!((gain(kind, &[], 1000.0) + 3.0).abs() < 0.2);
            assert!(gain(kind, &[], 100.0).abs() < 0.1);
            assert!((gain(kind, &[], 4000.0) - stop_db).abs() < 1.5, "{kind:?}");
        }
        for (kind, stop_db) in [(Kind::High12, -24.0), (Kind::High24, -48.0)] {
            assert!((gain(kind, &[], 1000.0) + 3.0).abs() < 0.2);
            assert!(gain(kind, &[], 10_000.0).abs() < 0.2);
            assert!((gain(kind, &[], 250.0) - stop_db).abs() < 1.5, "{kind:?}");
        }
        assert!(gain(Kind::Notch, &[], 1000.0) < -40.0);
        assert!(gain(Kind::Band12, &[], 1000.0).abs() < 0.1);
        assert!((gain(Kind::Bell, &[("gain", 9.0)], 1000.0) - 9.0).abs() < 0.1);
        assert!((gain(Kind::LowShelf, &[("gain", -12.0)], 40.0) + 12.0).abs() < 0.3);
        assert!((gain(Kind::HighShelf, &[("gain", 6.0)], 15_000.0) - 6.0).abs() < 0.3);
        // The ladder's resonance thins its passband; Fat gives it back.
        let thin = gain(
            Kind::LadderLow24,
            &[("resonance", 75.0), ("var", 0.0)],
            100.0,
        );
        let fat = gain(
            Kind::LadderLow24,
            &[("resonance", 75.0), ("var", 100.0)],
            100.0,
        );
        println!("ladder at 75%: {thin:.1} dB in the bass, {fat:.1} dB fat");
        assert!(thin < -8.0 && fat.abs() < 0.5);
        // The acid's doesn't, below the high-pass in its loop, which keeps
        // the bass out of the resonance.
        let acid = gain(Kind::Acid, &[("resonance", 75.0), ("var", 100.0)], 100.0);
        assert!(acid > -2.0, "{acid}");
    }

    #[test]
    fn combs_ring_at_the_cutoffs_harmonics() {
        let at = |kind: Kind, freq: f64| {
            sine_gain(
                &mut filter(&[
                    ("type", kind.index() as f32),
                    ("cutoff", 200.0),
                    ("resonance", 90.0),
                    ("var", 0.0),
                ]),
                freq,
            )
        };
        // Positive: peaks at 200 Hz and its multiples, troughs between.
        assert!(at(Kind::CombPlus, 400.0) > at(Kind::CombPlus, 300.0) + 15.0);
        // Negative: an octave down, its peaks at the odd multiples of 100.
        assert!(at(Kind::CombMinus, 300.0) > at(Kind::CombMinus, 400.0) + 15.0);
        // Flangers notch between the peaks.
        assert!(at(Kind::FlangerPlus, 300.0) < -20.0);
        assert!(at(Kind::FlangerMinus, 400.0) < -20.0);
    }

    #[test]
    fn the_formant_filter_says_its_vowels() {
        for (vowel, (first, second, third)) in [
            (0.0, (650.0, 1080.0, 2650.0)),
            (100.0, (350.0, 600.0, 2700.0)),
        ] {
            let settings = [
                ("type", Kind::Formant.index() as f32),
                ("cutoff", 1000.0),
                ("var", vowel),
            ];
            let drawn = values(PARAMS, &settings);
            let level = |freq: f32| db(f64::from(response(&drawn, 1000.0, freq, RATE)));
            // The formants stand above the space around them, and a moved
            // cutoff moves them.
            assert!(level(first) > level(first / 2.0) + 6.0, "{vowel}");
            let between = f32::sqrt(second * third);
            assert!(level(second) > level(between) + 6.0, "{vowel}");
            let shifted = |freq: f32| db(f64::from(response(&drawn, 8000.0, freq, RATE)));
            assert!(shifted(2.0 * first) > shifted(first) + 6.0, "{vowel}");
        }
    }

    #[test]
    fn spread_tunes_the_sides_apart() {
        let mut spread = filter(&[("type", 1.0), ("spread", 24.0), ("resonance", 0.0)]);
        let input = sine(1000.0, 0.01, 48_000, RATE);
        let (mut left, mut right) = (input.clone(), input.clone());
        run(&mut spread, &mut left, &mut right, 64, &Context::DEFAULT);
        let mut meters = [0.0; MAX_METERS];
        spread.meters(&mut meters);
        assert!((meters[METER_CUTOFF_LEFT] - 500.0).abs() < 0.5);
        assert!((meters[METER_CUTOFF_RIGHT] - 2000.0).abs() < 1.0);
        // An octave either way of a 24 dB low-pass at 1 kHz.
        let (l, r) = (
            db(rms(&left[24_000..]) / 0.007),
            db(rms(&right[24_000..]) / 0.007),
        );
        println!("1 kHz: left {l:.1} dB, right {r:.1} dB");
        assert!(l < -20.0 && r > -0.5);
    }

    #[test]
    fn mix_zero_passes_the_input_unchanged() {
        for kind in Kind::ALL {
            let mut filter = filter(&[
                ("type", kind.index() as f32),
                ("mix", 0.0),
                ("resonance", 100.0),
                ("drive", 24.0),
            ]);
            let input = noise(4800, 3);
            let mut right: Vec<f32> = input.iter().map(|s| -0.5 * s).collect();
            let expected_right = right.clone();
            let mut left = input.clone();
            run(&mut filter, &mut left, &mut right, 64, &Context::DEFAULT);
            assert_eq!(left, input, "{kind:?}");
            assert_eq!(right, expected_right, "{kind:?}");
        }
    }

    /// `input` through a filter of `kind` at full resonance whose cutoff
    /// a modulator sweeps over the whole range at `hz`, retuned every
    /// `every` samples, with the resonance swept too.
    fn swept(kind: Kind, input: &[f32], hz: f64, every: usize, drive: f32) -> Vec<f32> {
        let mut filter = filter(&[
            ("type", kind.index() as f32),
            ("resonance", 100.0),
            ("drive", drive),
        ]);
        let (mut left, mut right) = (input.to_vec(), input.to_vec());
        let cutoff = &PARAMS[CUTOFF];
        for (block, (l, r)) in left
            .chunks_mut(every)
            .zip(right.chunks_mut(every))
            .enumerate()
        {
            let t = (block * every) as f64 / f64::from(RATE);
            let phase = std::f64::consts::TAU * hz * t;
            filter.modulate(
                CUTOFF,
                cutoff.from_normalized((0.5 + 0.5 * phase.sin()) as f32),
            );
            filter.modulate(RESONANCE, (75.0 + 25.0 * (1.7 * phase).cos()) as f32);
            filter.process(l, r, &Context::DEFAULT);
        }
        left
    }

    #[test]
    fn audio_rate_sweeps_stay_stable() {
        // Loud noise through every type with the cutoff swept over ten
        // octaves at 200 Hz, retuned every block; then every sample at
        // 1 kHz. Nothing may run away or stop being a number, and a type
        // that doesn't oscillate on its own falls silent after.
        let mut input = noise(24_000, 9);
        input.iter_mut().for_each(|s| *s *= 2.0);
        input.extend(std::iter::repeat_n(0.0, 48_000));
        for kind in Kind::ALL {
            for (hz, every) in [(200.0, 64), (1000.0, 1)] {
                for drive in [0.0, 36.0] {
                    let out = swept(kind, &input, hz, every, drive);
                    assert!(out.iter().all(|s| s.is_finite()), "{kind:?}");
                    let loudest = peak(&out);
                    assert!(loudest < 64.0, "{kind:?} at {hz} Hz: {loudest}");
                    let ringing = matches!(kind.family(), Family::Ladder | Family::Acid);
                    if !ringing {
                        let tail = peak(&out[out.len() - 4800..]);
                        assert!(tail < 1e-3, "{kind:?} at {hz} Hz rings on: {tail}");
                    }
                }
            }
        }
    }

    #[test]
    fn self_oscillation_stays_bounded() {
        // The ladders sing on their own at full resonance, bounded by their
        // saturation, at any cutoff.
        for kind in [Kind::LadderLow24, Kind::Acid] {
            for cutoff in [50.0, 1000.0, 15_000.0] {
                let mut filter = filter(&[
                    ("type", kind.index() as f32),
                    ("resonance", 100.0),
                    ("cutoff", cutoff),
                ]);
                let mut left = noise(480, 2);
                left.extend(std::iter::repeat_n(0.0, 96_000));
                let mut right = left.clone();
                run(&mut filter, &mut left, &mut right, 64, &Context::DEFAULT);
                let level = peak(&left[48_000..]);
                println!("{kind:?} at {cutoff} Hz rings at {level:.2}");
                assert!(level.is_finite() && level < 4.0, "{kind:?} {cutoff}");
            }
        }
    }

    #[test]
    fn switching_type_does_not_click() {
        let input = sine(110.0, 0.4, 48_000, RATE);
        let largest_step = |out: &[f32]| {
            out.windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max)
        };
        let settled = |kind: Kind| {
            let mut filter = filter(&[("type", kind.index() as f32), ("cutoff", 2000.0)]);
            let (mut left, mut right) = (input.clone(), input.clone());
            run(&mut filter, &mut left, &mut right, 64, &Context::DEFAULT);
            largest_step(&left[4800..])
        };
        let calmest = Kind::ALL.map(settled).iter().fold(0.0f32, |m, &s| m.max(s));
        let mut switching = filter(&[("cutoff", 2000.0)]);
        let (mut left, mut right) = (input.clone(), input);
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            if block % 25 == 10 {
                let kind = Kind::ALL[(block / 25 * 7) % Kind::ALL.len()];
                switching.set_param(TYPE, kind.index() as f32);
            }
            switching.process(l, r, &Context::DEFAULT);
        }
        let worst = largest_step(&left[4800..]);
        println!("largest step switching {worst:.4}, settled at most {calmest:.4}");
        assert!(worst < 1.5 * calmest, "{worst} against {calmest}");
    }

    #[test]
    fn reset_silences_and_a_bad_sample_passes() {
        for kind in Kind::ALL {
            let mut filter = filter(&[("type", kind.index() as f32), ("resonance", 30.0)]);
            let mut input = noise(9600, 5);
            input[100] = f32::NAN;
            input[200] = f32::INFINITY;
            let (mut left, mut right) = (input.clone(), input);
            run(&mut filter, &mut left, &mut right, 64, &Context::DEFAULT);
            assert!(left.iter().all(|s| s.is_finite()), "{kind:?}");
            assert!(rms(&left[4800..]) > 0.01, "{kind:?} still sounds");
            filter.reset();
            assert_eq!(filter.meter(), 0.0);
            let (mut left, mut right) = (vec![0.0; 4800], vec![0.0; 4800]);
            run(&mut filter, &mut left, &mut right, 64, &Context::DEFAULT);
            assert!(left.iter().all(|&s| s == 0.0), "{kind:?}");
        }
    }
}
