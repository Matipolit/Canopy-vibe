//! Compressor: turns the signal down by `ratio` above `threshold`, with a
//! soft knee, attack and release, makeup gain (set, or worked out from the
//! curve), a dry/wet mix for parallel compression, a high-pass on what the
//! detector hears, and four characters. Its meters are the gain reduction,
//! the input and output levels, and the detector level, which the UI shows
//! as a dot on the transfer curve drawn with [`static_curve`].
//!
//! Feed-forward and in the log domain (after Giannoulis, Massberg and Reiss,
//! "Digital Dynamic Range Compressor Design", 2012): the louder channel's
//! level (or the sidechain's), high-passed so a kick doesn't pump the whole
//! mix, goes through the static curve to give the reduction it calls for.
//! That is held at its peaks and let go at the release rate, which keeps it
//! steady through each cycle of a waveform, and approached at the attack
//! rate, so attack and release are the one-pole time constants of the
//! reduction in dB. Since compression is a gain, the parallel mix is a
//! blend of that gain with 1.
//!
//! The characters ([`Model`]) change the detector, the curve, the timing
//! and the colour; the knobs keep their meaning:
//! - **Clean**: the design above, with the knee as set and no colour.
//! - **FET** (after the 1176): attack five times faster than the knob reads
//!   (its 0.1 ms is the 1176's fastest 20 µs, 4 ms its slowest 800 µs), a
//!   knee a quarter as wide, and second-harmonic distortion that grows with
//!   the reduction, the way a FET gain cell colours more the harder it's
//!   driven.
//! - **Opto** (after the LA-2A): the detector follows the signal's power
//!   over 10 ms, as a light cell glows, so it reacts to body rather than
//!   spikes; the knee is 6 dB softer; and the release stretches the longer
//!   and harder it has been compressing, up to five times the knob after a
//!   few seconds of 10 dB or more, as the cell's memory does. Short bursts
//!   recover at the set release, sustained squashing lets go slowly.
//! - **Vari-Mu** (after the Fairchild and other tube compressors): a knee
//!   that keeps widening as it compresses. The share of the excess taken
//!   away grows as `u / (u + W)` of the ratio's, `u` dB into a knee `W` 6 dB
//!   wider than the knob, so the ratio creeps up from 1:1 and reaches half
//!   its setting `W` dB in, and full only far above: gentle, and gluey on
//!   a mix. A little tube colour on top, less than the FET's.
//!
//! The colour is the squared signal with its running mean taken out, so it
//! adds no DC, divided by the signal's peak envelope, so the harmonic's
//! share depends on how hard the compressor works rather than on how loud
//! the track is: a pure second harmonic for a sine, which leaves the
//! fundamental, and so the static curve, alone. Switching character glides
//! the knee, curve, detector and colour over [`MODEL_MS`], so it doesn't
//! click. Every knob glides, attack and release included, at the knob's
//! pace or, for a modulator, within a block (see "Modulation" in `lib.rs`).

use instruments::ParamDescriptor;

use crate::{Context, Effect, EffectDescriptor, OFF_ON, Values, choice, float};
use dsp::flush;
use dsp::svf::{BUTTERWORTH_Q, Coefs, Shape, SvfState};
use dsp::{Pace, Smoothed};

use crate::dynamics::{DB_PER_LOG2, coefficient, static_reduction};
use crate::meter::{PeakMeter, RmsMeter, block_peak};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.compressor",
    name: "Compressor",
    description: "Compressor with clean, FET, opto and vari-mu characters, sidechain and parallel mix",
    category: crate::Category::Dynamics,
    params: PARAMS,
    meters: &[
        "Gain reduction (dB)",
        "Input peak (dB)",
        "Output peak (dB)",
        "Input RMS (dB)",
        "Output RMS (dB)",
        "Detector level (dB)",
    ],
    sidechain: true,
    create: || Box::new(Compressor::new()),
    ..EffectDescriptor::BASE
};

/// The characters' names, in [`Model::ALL`]'s order: the `model`
/// parameter's labels.
pub const MODELS: &[&str] = &["Clean", "FET", "Opto", "Vari-Mu"];

const PARAMS: &[ParamDescriptor] = &[
    float("threshold", "Threshold", -60.0, 0.0, -18.0, "dB", false),
    float("ratio", "Ratio", 1.0, 20.0, 4.0, ":1", true),
    float("attack", "Attack", 0.1, 100.0, 10.0, "ms", true),
    float("release", "Release", 10.0, 1000.0, 100.0, "ms", true),
    float("knee", "Knee", 0.0, 24.0, 6.0, "dB", false),
    float("makeup", "Makeup", -12.0, 24.0, 0.0, "dB", false),
    float("mix", "Mix", 0.0, 100.0, 100.0, "%", false),
    choice("model", "Character", MODELS, 0),
    float(
        "sc_hpf",
        "Sidechain high-pass",
        20.0,
        500.0,
        20.0,
        "Hz",
        true,
    ),
    choice("auto_makeup", "Auto makeup", OFF_ON, 0),
];

const COUNT: usize = 10;

pub const THRESHOLD: usize = 0;
pub const RATIO: usize = 1;
pub const ATTACK: usize = 2;
pub const RELEASE: usize = 3;
pub const KNEE: usize = 4;
pub const MAKEUP: usize = 5;
pub const MIX: usize = 6;
pub const MODEL: usize = 7;
pub const SC_HPF: usize = 8;
pub const AUTO_MAKEUP: usize = 9;

/// Meter slots, in [`DESCRIPTOR`]'s `meters` order.
pub const METER_REDUCTION: usize = 0;
pub const METER_INPUT_PEAK: usize = 1;
pub const METER_OUTPUT_PEAK: usize = 2;
pub const METER_INPUT_RMS: usize = 3;
pub const METER_OUTPUT_RMS: usize = 4;
pub const METER_DETECTOR: usize = 5;

/// How long threshold, ratio, knee, makeup and mix take to reach a new value.
const SMOOTH_MS: f32 = 20.0;
/// How long a change of character takes to glide over.
const MODEL_MS: f32 = 50.0;
/// A reduction this small (in dB) is none, so a release ends in exact zero.
const NO_REDUCTION: f32 = 1e-6;
/// Caps the reduction so that an infinite input can't stick at -∞ dB.
const MAX_REDUCTION: f32 = 200.0;
/// The opto's light cell: how long its glow takes to follow the power.
const CELL_MS: f32 = 10.0;
/// After long, hard reduction the opto releases up to this many times
/// slower than set.
const OPTO_SLOWEST: f32 = 5.0;
/// Reduction (dB) that, held long enough, charges the opto's memory fully.
const OPTO_FULL_DB: f32 = 10.0;
/// How long the opto's memory takes to charge while compressing, and to
/// fade once it stops.
const OPTO_CHARGE_MS: f32 = 1500.0;
const OPTO_DISCHARGE_MS: f32 = 5000.0;
/// A memory this small is none, so the release returns exactly to the knob.
const NO_MEMORY: f32 = 1e-4;
/// Depth of the second-harmonic colour at full reduction: a sine's second
/// harmonic is half this of its fundamental, so 1.5% (-36 dB) for the FET
/// and 0.5% (-46 dB) for the vari-mu.
const FET_COLOUR: f32 = 0.03;
const VARIMU_COLOUR: f32 = 0.01;
/// The colour is full from this much reduction (dB) on, and fades in
/// proportionally below it.
const COLOUR_FULL_DB: f32 = 12.0;
/// How quickly the colour's DC offset is followed and taken out.
const COLOUR_DC_MS: f32 = 20.0;
/// How quickly the peak envelope the colour is divided by falls back;
/// slower than the DC follower, so the quotient stays bounded.
const COLOUR_FALL_MS: f32 = 50.0;
/// The envelope's floor, so silence doesn't divide by zero.
const COLOUR_FLOOR: f32 = 1e-9;
/// The colour's input is limited to this, so a wild input can't make it
/// explode.
const COLOUR_LIMIT: f32 = 2.0;
/// Auto makeup gives back this share of the reduction a full-scale signal
/// gets: enough to bring the compressed sound back to about the loudness of
/// the original without pushing its peaks over.
const AUTO_MAKEUP_SHARE: f32 = 0.5;
/// The narrowest vari-mu knee (dB), so its curve stays defined while
/// gliding from the FET's hard knee.
const MIN_WIDTH: f32 = 0.01;
/// The attack and release glide, and the opto's memory moves, every this
/// many samples.
const CHUNK: usize = 16;

/// A compressor character; see the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    Clean,
    Fet,
    Opto,
    VariMu,
}

impl Model {
    pub const ALL: [Model; 4] = [Model::Clean, Model::Fet, Model::Opto, Model::VariMu];

    pub fn from_index(index: f32) -> Model {
        Self::ALL[(index.round().max(0.0) as usize).min(Self::ALL.len() - 1)]
    }

    fn character(self) -> Character {
        match self {
            Model::Clean => Character::CLEAN,
            Model::Fet => Character {
                knee_scale: 0.25,
                attack_scale: 0.2,
                colour: FET_COLOUR,
                ..Character::CLEAN
            },
            Model::Opto => Character {
                knee_add: 6.0,
                power: 1.0,
                memory: true,
                ..Character::CLEAN
            },
            Model::VariMu => Character {
                knee_add: 6.0,
                widening: 1.0,
                colour: VARIMU_COLOUR,
                ..Character::CLEAN
            },
        }
    }
}

/// What a [`Model`] does to the design.
#[derive(Debug, Clone, Copy)]
struct Character {
    /// The knee used is `knee_scale · knob + knee_add` dB.
    knee_scale: f32,
    knee_add: f32,
    /// 1 for the vari-mu's widening knee, 0 for the quadratic one.
    widening: f32,
    /// 1 for a detector that follows power (the opto's cell), 0 for peaks.
    power: f32,
    /// The attack used is the knob times this.
    attack_scale: f32,
    /// The release stretches after long reduction (the opto).
    memory: bool,
    /// Second-harmonic depth at full colour.
    colour: f32,
}

impl Character {
    const CLEAN: Character = Character {
        knee_scale: 1.0,
        knee_add: 0.0,
        widening: 0.0,
        power: 0.0,
        attack_scale: 1.0,
        memory: false,
        colour: 0.0,
    };

    fn knee(&self, knob: f32) -> f32 {
        self.knee_scale * knob + self.knee_add
    }
}

/// The reduction in dB at `level_db`: the quadratic knee (see
/// [`static_reduction`]) blended by `widening` into the vari-mu's, whose
/// slope is `slope · u / (u + knee)` at `u` dB past the knee's start.
#[inline]
fn curve_reduction(level_db: f32, threshold: f32, knee: f32, slope: f32, widening: f32) -> f32 {
    let quadratic = if widening < 1.0 {
        static_reduction(level_db, threshold, knee, slope)
    } else {
        0.0
    };
    if widening <= 0.0 {
        return quadratic;
    }
    let width = knee.max(MIN_WIDTH);
    let u = (level_db - threshold + 0.5 * width).max(0.0);
    let wide = slope * (u - width * (u / width).ln_1p());
    quadratic + (wide - quadratic) * widening
}

/// Makeup in dB, set plus automatic, for parameter values given by `value`.
fn makeup_db(value: &dyn Fn(usize) -> f32) -> f32 {
    let set = value(MAKEUP);
    if value(AUTO_MAKEUP) < 0.5 {
        return set;
    }
    let character = Model::from_index(value(MODEL)).character();
    let full_scale = curve_reduction(
        0.0,
        value(THRESHOLD),
        character.knee(value(KNEE)),
        1.0 - 1.0 / value(RATIO),
        character.widening,
    );
    set + AUTO_MAKEUP_SHARE * full_scale.min(MAX_REDUCTION)
}

/// The compressor's steady output level in dB for an input whose level is
/// `input_db`, as the detector reads it (a sine's peak), for parameter
/// values `values` in [`DESCRIPTOR`]'s order: the threshold, ratio and knee
/// as the character shapes them, makeup (set and automatic) and the mix.
/// What the UI draws as the transfer curve, with a dot at the detector
/// meter's level. Missing or NaN values count as defaults.
pub fn static_curve(values: &[f32], input_db: f32) -> f32 {
    let value = |index: usize| crate::param_value(PARAMS, values, index);
    let input_db = if input_db.is_nan() {
        -1000.0
    } else {
        input_db.clamp(-1000.0, 1000.0)
    };
    let character = Model::from_index(value(MODEL)).character();
    let reduction = curve_reduction(
        input_db,
        value(THRESHOLD),
        character.knee(value(KNEE)),
        1.0 - 1.0 / value(RATIO),
        character.widening,
    )
    .min(MAX_REDUCTION);
    let mix = value(MIX) / 100.0;
    let wet = dsp::db_to_gain(makeup_db(&value) - reduction);
    input_db + dsp::gain_to_db(1.0 - mix + mix * wet)
}

pub struct Compressor {
    values: Values<COUNT>,
    sample_rate: f32,
    model: Model,
    threshold: Smoothed,
    /// The knee the character makes of the knob, in dB.
    knee: Smoothed,
    /// `1 - 1 / ratio`: the share of the excess level taken away.
    slope: Smoothed,
    /// Makeup gain, set plus automatic, as an amplitude factor.
    makeup: Smoothed,
    mix: Smoothed,
    /// How much of the vari-mu's widening curve is used, 0 to 1.
    widening: Smoothed,
    /// How much the detector follows power rather than peaks, 0 to 1.
    power: Smoothed,
    /// Second-harmonic depth at full colour.
    colour: Smoothed,
    /// Below this detector level (the bottom of the knee) nothing is
    /// reduced, which skips the logarithm for quiet signals.
    knee_start: f32,
    /// The attack and release as one-pole coefficients, worked out every
    /// [`CHUNK`] samples from the times below while those glide.
    attack: f32,
    release: f32,
    /// The attack the character makes of the knob, and the release set
    /// (which the opto's memory stretches), as log2 ms. They glide rather
    /// than jump: a reduction lagging behind the level would otherwise
    /// catch up all at once when the attack or release is made much
    /// shorter, which a modulated knob does many times a second.
    log_attack: Smoothed,
    log_release: Smoothed,
    /// The detector's high-pass and its memory, per channel.
    high_pass: Coefs,
    high_pass_state: [SvfState; 2],
    /// The opto cell: the detector's mean square, followed over [`CELL_MS`].
    cell: f32,
    /// `1 -` the cell's one-pole coefficient.
    cell_rate: f32,
    /// How charged the opto's memory is, 0 to 1.
    memory: f32,
    memory_charge: f32,
    memory_discharge: f32,
    /// Samples left until the memory is next updated.
    countdown: usize,
    /// Each channel's running mean of the coloured signal's square, which
    /// the colour takes out so as to add no DC.
    colour_dc: [f32; 2],
    colour_rate: f32,
    /// Each channel's peak envelope, which the colour is divided by.
    colour_envelope: [f32; 2],
    colour_fall: f32,
    /// The reduction the level calls for, held at its peaks and let go at
    /// the release rate, in dB.
    held: f32,
    /// `held` approached at the attack rate: the reduction applied, in dB.
    reduction: f32,
    input_peak: PeakMeter,
    output_peak: PeakMeter,
    input_rms: RmsMeter,
    output_rms: RmsMeter,
    /// The level the detector compares with the threshold.
    detector: PeakMeter,
}

impl Compressor {
    pub fn new() -> Compressor {
        let mut compressor = Compressor {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            model: Model::Clean,
            threshold: Smoothed::new(0.0, SMOOTH_MS),
            knee: Smoothed::new(0.0, SMOOTH_MS),
            slope: Smoothed::new(0.0, SMOOTH_MS),
            makeup: Smoothed::new(1.0, SMOOTH_MS),
            mix: Smoothed::new(1.0, SMOOTH_MS),
            widening: Smoothed::new(0.0, MODEL_MS),
            power: Smoothed::new(0.0, MODEL_MS),
            colour: Smoothed::new(0.0, MODEL_MS),
            knee_start: 1.0,
            attack: 0.0,
            release: 0.0,
            log_attack: Smoothed::new(0.0, SMOOTH_MS),
            log_release: Smoothed::new(0.0, SMOOTH_MS),
            high_pass: Coefs::IDENTITY,
            high_pass_state: [SvfState::default(); 2],
            cell: 0.0,
            cell_rate: 1.0,
            memory: 0.0,
            memory_charge: 0.0,
            memory_discharge: 0.0,
            countdown: 0,
            colour_dc: [0.0; 2],
            colour_rate: 1.0,
            colour_envelope: [0.0; 2],
            colour_fall: 0.0,
            held: 0.0,
            reduction: 0.0,
            input_peak: PeakMeter::default(),
            output_peak: PeakMeter::default(),
            input_rms: RmsMeter::default(),
            output_rms: RmsMeter::default(),
            detector: PeakMeter::default(),
        };
        compressor.prepare(48_000.0, 0);
        compressor
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`; a change of character always takes the knob's.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        let character = self.model.character();
        match index {
            THRESHOLD => self.threshold.move_to(value, pace),
            RATIO => self.slope.move_to(1.0 - 1.0 / value, pace),
            ATTACK => self
                .log_attack
                .move_to((value * character.attack_scale).log2(), pace),
            RELEASE => self.log_release.move_to(value.log2(), pace),
            KNEE => self.knee.move_to(character.knee(value), pace),
            MIX => self.mix.move_to(value / 100.0, pace),
            MODEL => {
                self.model = Model::from_index(value);
                let character = self.model.character();
                self.widening.set_target(character.widening);
                self.power.set_target(character.power);
                self.colour.set_target(character.colour);
                self.knee.set_target(character.knee(self.values[KNEE]));
                self.log_attack
                    .set_target((self.values[ATTACK] * character.attack_scale).log2());
            }
            SC_HPF => {
                // The high-pass only shapes what the detector hears, whose
                // output the attack and release smooth, so it can change at
                // once.
                self.high_pass =
                    Coefs::new(&Shape::highpass(value, BUTTERWORTH_Q, self.sample_rate));
            }
            _ => {}
        }
        if matches!(
            index,
            THRESHOLD | RATIO | KNEE | MAKEUP | MODEL | AUTO_MAKEUP
        ) {
            let pace = if index == MODEL { Pace::Knob } else { pace };
            let makeup = makeup_db(&|i| self.values[i]);
            self.makeup.move_to(dsp::db_to_gain(makeup), pace);
        }
    }

    /// The release time with the opto's memory taken into account, in ms.
    fn stretched_release(&self) -> f32 {
        self.log_release.current().exp2() * (1.0 + (OPTO_SLOWEST - 1.0) * self.memory)
    }

    fn smoothers(&mut self) -> [&mut Smoothed; 8] {
        [
            &mut self.threshold,
            &mut self.knee,
            &mut self.slope,
            &mut self.makeup,
            &mut self.mix,
            &mut self.widening,
            &mut self.power,
            &mut self.colour,
        ]
    }

    fn update_knee_start(&mut self) {
        self.knee_start = dsp::db_to_gain(self.threshold.current() - 0.5 * self.knee.current());
    }

    /// Every [`CHUNK`] samples: the attack and release glide towards
    /// their knobs, and the opto's memory charges while it compresses and
    /// fades when it stops, stretching the release.
    fn update_times(&mut self) {
        if self.log_attack.is_smoothing() {
            self.log_attack.tick();
            self.attack = coefficient(self.log_attack.current().exp2(), self.sample_rate);
        }
        let mut release_moved = self.log_release.is_smoothing();
        if release_moved {
            self.log_release.tick();
        }
        let target = if self.model.character().memory {
            (self.reduction / OPTO_FULL_DB).min(1.0)
        } else {
            0.0
        };
        if target != 0.0 || self.memory != 0.0 {
            let rate = if target > self.memory {
                self.memory_charge
            } else {
                self.memory_discharge
            };
            self.memory = target + (self.memory - target) * rate;
            if self.memory < NO_MEMORY {
                self.memory = 0.0;
            }
            release_moved = true;
        }
        if release_moved {
            self.release = coefficient(self.stretched_release(), self.sample_rate);
        }
    }
}

impl Default for Compressor {
    fn default() -> Self {
        Compressor::new()
    }
}

impl Effect for Compressor {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        for smoothed in self.smoothers() {
            smoothed.prepare(sample_rate);
        }
        self.cell_rate = 1.0 - coefficient(CELL_MS, sample_rate);
        self.colour_rate = 1.0 - coefficient(COLOUR_DC_MS, sample_rate);
        self.colour_fall = coefficient(COLOUR_FALL_MS, sample_rate);
        let chunk_rate = sample_rate / CHUNK as f32;
        self.log_attack.prepare_every(sample_rate, CHUNK);
        self.log_release.prepare_every(sample_rate, CHUNK);
        self.memory_charge = coefficient(OPTO_CHARGE_MS, chunk_rate);
        self.memory_discharge = coefficient(OPTO_DISCHARGE_MS, chunk_rate);
        for meter in [
            &mut self.input_peak,
            &mut self.output_peak,
            &mut self.detector,
        ] {
            meter.prepare(sample_rate);
        }
        self.input_rms.prepare(sample_rate);
        self.output_rms.prepare(sample_rate);
        // The model first: the knee and attack depend on it.
        self.apply(MODEL, Pace::Knob);
        for index in 0..COUNT {
            self.apply(index, Pace::Knob);
        }
        self.reset();
    }

    fn reset(&mut self) {
        for smoothed in self.smoothers() {
            smoothed.settle();
        }
        self.update_knee_start();
        self.high_pass_state = [SvfState::default(); 2];
        self.cell = 0.0;
        self.memory = 0.0;
        for smoothed in [&mut self.log_attack, &mut self.log_release] {
            smoothed.settle();
        }
        self.attack = coefficient(self.log_attack.current().exp2(), self.sample_rate);
        self.release = coefficient(self.stretched_release(), self.sample_rate);
        self.countdown = 0;
        self.colour_dc = [0.0; 2];
        self.colour_envelope = [0.0; 2];
        self.held = 0.0;
        self.reduction = 0.0;
        for meter in [
            &mut self.input_peak,
            &mut self.output_peak,
            &mut self.detector,
        ] {
            meter.reset();
        }
        self.input_rms.reset();
        self.output_rms.reset();
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: &Context) {
        self.values.played();
        let len = left.len().min(right.len());
        let (left, right) = (&mut left[..len], &mut right[..len]);
        self.input_peak
            .update(block_peak(left).max(block_peak(right)), len);
        let sidechain = context.sidechain;
        let mut detected = 0.0f32;
        for n in 0..len {
            if self.countdown == 0 {
                self.update_times();
                self.countdown = CHUNK;
            }
            self.countdown -= 1;

            let (l, r) = (left[n], right[n]);
            let (side_l, side_r) = match sidechain {
                Some((side_l, side_r)) => (
                    side_l.get(n).copied().unwrap_or(0.0),
                    side_r.get(n).copied().unwrap_or(0.0),
                ),
                None => (l, r),
            };
            let [state_l, state_r] = &mut self.high_pass_state;
            let side_l = state_l.tick(&self.high_pass, side_l);
            let side_r = state_r.tick(&self.high_pass, side_r);
            let peak = side_l.abs().max(side_r.abs());
            let level = if self.power.current() > 0.0 || self.power.is_smoothing() {
                // The cell reads a sine's peak, like the peak detector, so
                // the threshold means the same in every character.
                let power = self.power.tick();
                let square = (side_l * side_l).max(side_r * side_r);
                self.cell += (square - self.cell) * self.cell_rate;
                peak + ((2.0 * self.cell).sqrt() - peak) * power
            } else {
                peak
            };
            detected = detected.max(level);

            let curve_moves = self.threshold.is_smoothing()
                || self.knee.is_smoothing()
                || self.slope.is_smoothing()
                || self.widening.is_smoothing();
            if curve_moves {
                self.threshold.tick();
                self.knee.tick();
                self.slope.tick();
                self.widening.tick();
                self.update_knee_start();
            }
            let wanted = if level > self.knee_start {
                let level_db = DB_PER_LOG2 * level.log2();
                curve_reduction(
                    level_db,
                    self.threshold.current(),
                    self.knee.current(),
                    self.slope.current(),
                    self.widening.current(),
                )
                .min(MAX_REDUCTION)
            } else {
                0.0
            };

            self.held = if wanted >= self.held {
                wanted
            } else {
                wanted + self.release * (self.held - wanted)
            };
            if self.held < NO_REDUCTION {
                self.held = 0.0;
            }
            self.reduction = if self.held > self.reduction {
                self.held + self.attack * (self.reduction - self.held)
            } else {
                self.held
            };

            let gain = if self.reduction > 0.0 {
                (-self.reduction / DB_PER_LOG2).exp2()
            } else {
                1.0
            };
            let makeup = self.makeup.tick();
            let mix = self.mix.tick();
            let blended = 1.0 - mix + mix * gain * makeup;
            let (mut out_l, mut out_r) = (l * blended, r * blended);
            if self.colour.current() > 0.0 || self.colour.is_smoothing() {
                let depth =
                    self.colour.tick() * (self.reduction / COLOUR_FULL_DB).min(1.0) * mix * makeup;
                let [dc_l, dc_r] = &mut self.colour_dc;
                let [envelope_l, envelope_r] = &mut self.colour_envelope;
                for (out, x, dc, envelope) in [
                    (&mut out_l, l, dc_l, envelope_l),
                    (&mut out_r, r, dc_r, envelope_r),
                ] {
                    let cell = (x * gain).clamp(-COLOUR_LIMIT, COLOUR_LIMIT);
                    let square = cell * cell;
                    *dc += (square - *dc) * self.colour_rate;
                    *envelope = (*envelope * self.colour_fall).max(cell.abs());
                    *out += depth * (square - *dc) / envelope.max(COLOUR_FLOOR);
                }
            }
            left[n] = out_l;
            right[n] = out_r;
            self.input_rms.add(0.5 * (l * l + r * r));
            self.output_rms.add(0.5 * (out_l * out_l + out_r * out_r));
        }

        // Unused detectors start from silence when they come back.
        if self.power.current() == 0.0 && !self.power.is_smoothing() {
            self.cell = 0.0;
        }
        if self.colour.current() == 0.0 && !self.colour.is_smoothing() {
            self.colour_dc = [0.0; 2];
            self.colour_envelope = [0.0; 2];
        }
        for state in &mut self.high_pass_state {
            state.flush();
        }
        self.cell = flush(self.cell);
        for value in self.colour_dc.iter_mut().chain(&mut self.colour_envelope) {
            *value = flush(*value);
        }
        self.input_rms.flush();
        self.output_rms.flush();
        self.output_peak
            .update(block_peak(left).max(block_peak(right)), len);
        self.detector.update(detected, len);
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

    fn meters(&self, out: &mut [f32; crate::MAX_METERS]) {
        out[METER_REDUCTION] = self.reduction;
        out[METER_INPUT_PEAK] = self.input_peak.db();
        out[METER_OUTPUT_PEAK] = self.output_peak.db();
        out[METER_INPUT_RMS] = self.input_rms.db();
        out[METER_OUTPUT_RMS] = self.output_rms.db();
        out[METER_DETECTOR] = self.detector.db();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults;
    use crate::testing::{RATE, db, peak, run, set, sine};

    fn compressor(settings: &[(&str, f32)]) -> Compressor {
        let mut compressor = Compressor::new();
        compressor.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut compressor, id, value);
        }
        compressor.reset();
        compressor
    }

    /// Output level in dB for a steady 1 kHz sine whose peak is at `level_db`.
    fn steady_output_db(settings: &[(&str, f32)], level_db: f32) -> f64 {
        let mut compressor = compressor(settings);
        let input = sine(1000.0, dsp::db_to_gain(level_db), RATE as usize, RATE);
        let (mut left, mut right) = (input.clone(), input);
        run(
            &mut compressor,
            &mut left,
            &mut right,
            64,
            &Context::DEFAULT,
        );
        assert_eq!(left, right);
        db(f64::from(peak(&left[left.len() - 4800..])))
    }

    #[test]
    fn static_curve_follows_threshold_ratio_and_knee() {
        let mut worst = 0.0f64;
        for (threshold, ratio, knee, makeup) in [
            (-20.0, 4.0, 0.0, 0.0),
            (-30.0, 10.0, 12.0, 6.0),
            (-12.0, 2.0, 6.0, -3.0),
            (-40.0, 20.0, 24.0, 0.0),
        ] {
            let settings = [
                ("threshold", threshold),
                ("ratio", ratio),
                ("knee", knee),
                ("makeup", makeup),
                ("attack", 1.0),
                ("release", 100.0),
            ];
            let slope = 1.0 - 1.0 / ratio;
            for step in 0..=12 {
                let level = -48.0 + 4.0 * step as f32;
                let expected = level - static_reduction(level, threshold, knee, slope) + makeup;
                let measured = steady_output_db(&settings, level);
                let error = (measured - f64::from(expected)).abs();
                worst = worst.max(error);
                assert!(
                    error < 0.25,
                    "threshold {threshold}, ratio {ratio}, knee {knee}: in {level} dB, out {measured:.2}, expected {expected:.2}"
                );
            }
        }
        println!("largest static curve error: {worst:.3} dB");

        // The curve itself: hard knee, and the soft knee's edges and middle.
        assert_eq!(static_reduction(-10.0, -20.0, 0.0, 0.75), 7.5);
        assert_eq!(static_reduction(-20.5, -20.0, 0.0, 0.75), 0.0);
        assert_eq!(static_reduction(-23.0, -20.0, 6.0, 0.75), 0.0);
        assert!((static_reduction(-17.0, -20.0, 6.0, 0.75) - 2.25).abs() < 1e-6);
        assert!((static_reduction(-20.0, -20.0, 6.0, 0.75) - 0.5625).abs() < 1e-6);
    }

    /// The meter, every 16 samples, for a 1 kHz sine whose level steps from
    /// `from_db` to `to_db` after half a second.
    fn meter_after_step(settings: &[(&str, f32)], from_db: f32, to_db: f32) -> Vec<f32> {
        let mut compressor = compressor(settings);
        let len = RATE as usize * 2;
        let mut input = sine(1000.0, 1.0, len, RATE);
        for (n, s) in input.iter_mut().enumerate() {
            *s *= dsp::db_to_gain(if n < len / 4 { from_db } else { to_db });
        }
        let mut right = input.clone();
        let mut meter = Vec::new();
        for (l, r) in input.chunks_mut(16).zip(right.chunks_mut(16)) {
            compressor.process(l, r, &Context::DEFAULT);
            meter.push(compressor.meter());
        }
        meter[len / 4 / 16..].to_vec()
    }

    /// Milliseconds until `meter` has moved 63% of the way to `end`.
    fn time_constant_ms(meter: &[f32], end: f32) -> f32 {
        let start = meter[0];
        let mark = start + (end - start) * (1.0 - (-1.0f32).exp());
        let blocks = meter
            .iter()
            .position(|&m| if end > start { m >= mark } else { m <= mark })
            .unwrap();
        (blocks + 1) as f32 * 16.0 / RATE * 1000.0
    }

    #[test]
    fn attack_and_release_take_the_times_set() {
        for (attack, release) in [(10.0, 200.0), (1.0, 50.0), (50.0, 500.0)] {
            let settings = [
                ("threshold", -30.0),
                ("ratio", 10.0),
                ("knee", 0.0),
                ("attack", attack),
                ("release", release),
            ];
            // 30 dB over a -30 dB threshold at 10:1 calls for 27 dB.
            let rising = meter_after_step(&settings, -40.0, 0.0);
            let measured_attack = time_constant_ms(&rising, 27.0);
            let full = *rising.last().unwrap();
            assert!(
                (full - 27.0).abs() < 0.2,
                "meter reads the reduction: {full}"
            );

            let mut compressor = compressor(&settings);
            let loud = sine(1000.0, 1.0, RATE as usize, RATE);
            let (mut l, mut r) = (loud.clone(), loud);
            run(&mut compressor, &mut l, &mut r, 64, &Context::DEFAULT);
            let falling = meter_after_step(&settings, 0.0, -40.0);
            let measured_release = time_constant_ms(&falling, 0.0);
            println!(
                "attack {attack} ms measured {measured_attack:.2} ms, release {release} ms measured {measured_release:.1} ms"
            );
            assert!(
                (measured_attack - attack).abs() < 0.15 * attack + 0.7,
                "{measured_attack}"
            );
            assert!(
                (measured_release - release).abs() < 0.1 * release,
                "{measured_release}"
            );
        }
    }

    #[test]
    fn mix_zero_is_the_dry_signal_and_mix_blends_gains() {
        let input = sine(500.0, 0.9, 24_000, RATE);
        let mut dry = compressor(&[
            ("threshold", -40.0),
            ("ratio", 20.0),
            ("mix", 0.0),
            ("makeup", 12.0),
        ]);
        let (mut left, mut right) = (input.clone(), input.clone());
        run(&mut dry, &mut left, &mut right, 64, &Context::DEFAULT);
        assert_eq!(left, input);
        assert_eq!(right, input);
        assert!(dry.meter() > 30.0, "still measures: {}", dry.meter());

        // Half wet: halfway between the dry level and the compressed one.
        let settings = [
            ("threshold", -20.0),
            ("ratio", 20.0),
            ("knee", 0.0),
            ("attack", 1.0),
        ];
        let level = db(0.9) as f32;
        let wet = dsp::db_to_gain(steady_output_db(&settings, level) as f32 - level);
        let mut half = settings.to_vec();
        half.push(("mix", 50.0));
        let blended = dsp::db_to_gain(steady_output_db(&half, level) as f32 - level);
        assert!(
            (blended - (0.5 + 0.5 * wet)).abs() < 0.01,
            "{blended} against {wet}"
        );
    }

    /// Runs `compressor` over `input` (both channels) with `side` as the
    /// sidechain, in blocks of 64.
    fn run_with_sidechain(compressor: &mut Compressor, input: &[f32], side: &[f32]) -> Vec<f32> {
        let (mut left, mut right) = (input.to_vec(), input.to_vec());
        for start in (0..input.len()).step_by(64) {
            let end = (start + 64).min(input.len());
            let block = Context {
                sidechain: Some((&side[start..end], &side[start..end])),
                ..Context::DEFAULT
            };
            compressor.process(&mut left[start..end], &mut right[start..end], &block);
        }
        left
    }

    #[test]
    fn a_sidechain_drives_the_detector() {
        let mut compressor = compressor(&[("threshold", -30.0), ("ratio", 4.0), ("knee", 0.0)]);
        let quiet = sine(1000.0, 0.01, 24_000, RATE);
        let loud = sine(100.0, 1.0, 24_000, RATE);
        let left = run_with_sidechain(&mut compressor, &quiet, &loud);
        // The quiet input is turned down by what the loud sidechain calls for.
        let gain = db(f64::from(peak(&left[12_000..]) / 0.01));
        assert!((gain + 22.5).abs() < 0.5, "{gain}");
        assert!((compressor.meter() - 22.5).abs() < 0.5);
    }

    #[test]
    fn the_sidechain_high_pass_keeps_a_kick_from_pumping() {
        let quiet = sine(1000.0, 0.1, 24_000, RATE);
        // Faded in, so the filters don't ring at the onset; a slow release
        // keeps the reduction steady through each cycle.
        let mut kick = sine(50.0, 1.0, 24_000, RATE);
        for (n, s) in kick.iter_mut().take(2400).enumerate() {
            *s *= n as f32 / 2400.0;
        }
        let settings = [
            ("threshold", -30.0),
            ("ratio", 4.0),
            ("knee", 0.0),
            ("release", 300.0),
        ];
        let mut reductions = Vec::new();
        for hpf in [20.0, 150.0, 500.0] {
            let mut settings = settings.to_vec();
            settings.push(("sc_hpf", hpf));
            let mut compressor = compressor(&settings);
            run_with_sidechain(&mut compressor, &quiet, &kick);
            reductions.push(compressor.meter());
        }
        println!("reduction from a 50 Hz sidechain, high-pass at 20/150/500 Hz: {reductions:?}");
        // 0 dB is 30 dB over: 22.5 dB at 4:1. A 12 dB/octave high-pass at
        // 150 Hz takes 19 dB off 50 Hz; at 500 Hz, 40 dB, under the threshold.
        assert!((reductions[0] - 22.5).abs() < 0.5);
        assert!((reductions[1] - 0.75 * (30.0 - 19.1)).abs() < 1.0);
        assert!(reductions[2] < 0.01);
    }

    /// The amplitude of `signal`'s component at `freq`, over whole cycles.
    fn amplitude_at(signal: &[f32], freq: f64) -> f64 {
        let step = std::f64::consts::TAU * freq / f64::from(RATE);
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &s) in signal.iter().enumerate() {
            re += f64::from(s) * (step * n as f64).cos();
            im += f64::from(s) * (step * n as f64).sin();
        }
        2.0 * (re * re + im * im).sqrt() / signal.len() as f64
    }

    /// The steady output for a 1 kHz sine at `level_db`: the fundamental's
    /// level in dB, and the second harmonic's relative to it.
    fn steady_fundamental(settings: &[(&str, f32)], level_db: f32) -> (f64, f64) {
        let mut compressor = compressor(settings);
        let input = sine(1000.0, dsp::db_to_gain(level_db), RATE as usize, RATE);
        let (mut left, mut right) = (input.clone(), input);
        run(
            &mut compressor,
            &mut left,
            &mut right,
            64,
            &Context::DEFAULT,
        );
        assert!(left.iter().all(|s| s.is_finite()));
        // 4800 samples: exactly 100 cycles.
        let tail = &left[left.len() - 4800..];
        let fundamental = amplitude_at(tail, 1000.0);
        (
            db(fundamental),
            db(amplitude_at(tail, 2000.0) / fundamental),
        )
    }

    fn values_of(settings: &[(&str, f32)]) -> [f32; COUNT] {
        let mut values: [f32; COUNT] = defaults(PARAMS);
        for &(id, value) in settings {
            values[PARAMS.iter().position(|p| p.id == id).unwrap()] = value;
        }
        values
    }

    #[test]
    fn every_model_follows_static_curve() {
        let mut worst = 0.0f64;
        for model in 0..4 {
            for (threshold, ratio, knee, makeup, auto, mix) in [
                (-20.0, 4.0, 0.0, 0.0, 0.0, 100.0),
                (-30.0, 10.0, 12.0, 6.0, 0.0, 100.0),
                (-24.0, 3.0, 6.0, 0.0, 1.0, 100.0),
                (-36.0, 20.0, 0.0, -3.0, 1.0, 50.0),
            ] {
                let settings = [
                    ("model", model as f32),
                    ("threshold", threshold),
                    ("ratio", ratio),
                    ("knee", knee),
                    ("makeup", makeup),
                    ("auto_makeup", auto),
                    ("mix", mix),
                    ("attack", 1.0),
                    ("release", 100.0),
                ];
                let values = values_of(&settings);
                for step in 0..=12 {
                    let level = -48.0 + 4.0 * step as f32;
                    let expected = f64::from(static_curve(&values, level));
                    let (measured, _) = steady_fundamental(&settings, level);
                    let error = (measured - expected).abs();
                    worst = worst.max(error);
                    assert!(
                        error < 0.25,
                        "{:?} {settings:?}: in {level} dB, out {measured:.2}, curve {expected:.2}",
                        Model::ALL[model]
                    );
                }
            }
        }
        println!("largest difference from static_curve over every model: {worst:.3} dB");
    }

    #[test]
    fn the_characters_measurably_differ() {
        // Timing: the same knobs, a 30 dB step into a -30 dB threshold.
        let timing = |model: Model| {
            let settings = [
                ("model", model as usize as f32),
                ("threshold", -30.0),
                ("ratio", 10.0),
                ("knee", 0.0),
                ("attack", 10.0),
                ("release", 200.0),
            ];
            let rising = meter_after_step(&settings, -40.0, 0.0);
            time_constant_ms(&rising, *rising.last().unwrap())
        };
        let clean = timing(Model::Clean);
        let fet = timing(Model::Fet);
        println!("attack time constant at 10 ms: clean {clean:.2} ms, FET {fet:.2} ms");
        assert!(
            fet < 0.3 * clean,
            "the FET is faster: {fet} against {clean}"
        );

        // The opto lets go slower after long reduction than after a burst.
        let opto_release = |burst_s: f32| {
            let mut compressor = compressor(&[
                ("model", 2.0),
                ("threshold", -30.0),
                ("ratio", 10.0),
                ("attack", 5.0),
                ("release", 100.0),
            ]);
            let burst = (burst_s * RATE) as usize;
            let len = burst + RATE as usize * 2;
            let mut input = sine(1000.0, 1.0, len, RATE);
            for s in &mut input[burst..] {
                *s *= 0.001;
            }
            let mut right = input.clone();
            let mut meter = Vec::new();
            for (l, r) in input.chunks_mut(16).zip(right.chunks_mut(16)) {
                compressor.process(l, r, &Context::DEFAULT);
                meter.push(compressor.meter());
            }
            time_constant_ms(&meter[burst / 16..], 0.0)
        };
        let short = opto_release(0.2);
        let long = opto_release(4.0);
        println!("opto release at 100 ms: {short:.0} ms after 0.2 s, {long:.0} ms after 4 s");
        // The cell's own 10 ms glow adds to the release after a burst.
        assert!(short < 250.0, "{short}");
        assert!(long > 2.0 * short, "{long} against {short}");

        // The vari-mu's knee keeps widening: gentler than clean at the same
        // settings, its slope still growing 30 dB over.
        let curve = |model: Model, level: f32| {
            let values = values_of(&[
                ("model", model as usize as f32),
                ("threshold", -30.0),
                ("ratio", 4.0),
                ("knee", 6.0),
            ]);
            static_curve(&values, level)
        };
        let reduction = |model, level| level - curve(model, level);
        let (vari_mu, clean) = (reduction(Model::VariMu, 0.0), reduction(Model::Clean, 0.0));
        println!("reduction at 0 dB, 4:1 at -30 dB: vari-mu {vari_mu:.1} dB, clean {clean:.1} dB");
        assert!(vari_mu < clean - 5.0);
        let slope = |model, level: f32| 1.0 - (curve(model, level + 1.0) - curve(model, level));
        assert!(slope(Model::VariMu, -24.0) < slope(Model::VariMu, -12.0));
        assert!(slope(Model::VariMu, -12.0) < slope(Model::VariMu, 0.0));
        assert!((slope(Model::Clean, -12.0) - slope(Model::Clean, 0.0)).abs() < 1e-3);

        // Colour: clean has none, the FET the most, the vari-mu a little.
        let harmonic = |model: Model| {
            let settings = [
                ("model", model as usize as f32),
                ("threshold", -30.0),
                ("ratio", 4.0),
                ("makeup", 12.0),
            ];
            steady_fundamental(&settings, -6.0).1
        };
        let colours = Model::ALL.map(harmonic);
        println!("second harmonic, 18 dB reduction (dB below the fundamental): {colours:?}");
        assert!(colours[0] < -100.0);
        assert!(colours[1] > -45.0 && colours[1] < -30.0);
        assert!(colours[3] < colours[1] - 6.0 && colours[3] > -60.0);
    }

    #[test]
    fn meters_read_levels_and_detector() {
        let mut compressor = compressor(&[("threshold", -20.0), ("ratio", 4.0), ("knee", 0.0)]);
        // A -6 dB peak sine: 14 dB over, 10.5 dB reduction. Two seconds, for
        // the RMS meters to settle.
        let input = sine(1000.0, 0.5, RATE as usize * 2, RATE);
        let (mut left, mut right) = (input.clone(), input);
        run(
            &mut compressor,
            &mut left,
            &mut right,
            64,
            &Context::DEFAULT,
        );
        let mut meters = [0.0; crate::MAX_METERS];
        compressor.meters(&mut meters);
        println!("meters: {:?}", &meters[..6]);
        let close = |slot: usize, expected: f32, tolerance: f32| {
            assert!(
                (meters[slot] - expected).abs() < tolerance,
                "slot {slot}: {} against {expected}",
                meters[slot]
            );
        };
        close(METER_REDUCTION, 10.5, 0.1);
        close(METER_INPUT_PEAK, -6.02, 0.05);
        close(METER_OUTPUT_PEAK, -16.52, 0.1);
        close(METER_INPUT_RMS, -9.03, 0.1);
        close(METER_OUTPUT_RMS, -19.53, 0.2);
        close(METER_DETECTOR, -6.02, 0.05);
        // The dot sits on the curve: the curve at the detector level is
        // the output peak.
        let values = values_of(&[("threshold", -20.0), ("ratio", 4.0), ("knee", 0.0)]);
        assert!(
            (static_curve(&values, meters[METER_DETECTOR]) - meters[METER_OUTPUT_PEAK]).abs() < 0.1
        );
    }

    #[test]
    fn silence_stays_silent_in_every_model() {
        for model in 0..4 {
            let mut compressor = compressor(&[
                ("model", model as f32),
                ("makeup", 24.0),
                ("auto_makeup", 1.0),
                ("threshold", -60.0),
            ]);
            let (mut left, mut right) = (vec![0.0; 4800], vec![0.0; 4800]);
            run(
                &mut compressor,
                &mut left,
                &mut right,
                64,
                &Context::DEFAULT,
            );
            assert!(
                left.iter().chain(&right).all(|&s| s == 0.0),
                "model {model}"
            );
            let mut meters = [0.0; crate::MAX_METERS];
            compressor.meters(&mut meters);
            assert!(meters.iter().all(|m| m.is_finite()));
        }
    }
}
