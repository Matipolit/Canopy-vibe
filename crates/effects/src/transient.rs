//! Transient Shaper: turns the start of each hit (its attack) and what
//! rings on after it (its sustain) up or down, by the same amount however
//! loud the hit is; dips the sound just after each hit (pump), for punch
//! without louder peaks; can leave everything under a split frequency
//! alone; and can soft-clip the output under 0 dBFS. At its defaults it
//! passes the sound on bit for bit. Its meters are the gain change, the
//! most boost and cut lately, and the input and output peaks.
//!
//! **Detector**: the classic differential-envelope design (as in SPL's
//! Transient Designer), on the louder side's level, so both sides get the
//! same gain and the image stays put.
//! - The **level** is the loudest sample over the last one to two windows
//!   of [`WINDOW_MS`] (two running maxima, the older dropped as a new one
//!   starts), let go over [`LEVEL_RELEASE_MS`]. It rises the moment the
//!   sound does, and holds still through every cycle of a tone above
//!   50 Hz, so a steady sound reads steady and its gain isn't modulated.
//! - **Attack** reads how far the level is above a follower that rises
//!   after it over [`ATTACK_MS`] and falls with it at once: 0 dB while the
//!   sound holds or falls, and at each onset the jump it made, dying away
//!   as the follower catches up.
//! - **Sustain** reads how far the level is below a follower that rises
//!   with it at once and falls over [`SUSTAIN_MS`]: 0 dB while the sound
//!   holds or grows, and growing through any decay faster than that.
//! - **Pump** reads how far the attack reading has fallen from its peak,
//!   which it lets go over [`PUMP_MS`]: nothing at the onset, the most
//!   once the onset has passed, and gone again as the sound settles.
//!
//! Every follower is a one-pole on the level itself and every reading a
//! ratio of two of them, so a hit 24 dB quieter reads the same; only
//! silence, which has no ratio, reads as an endless jump, and every reading
//! is capped at [`RANGE_DB`]. Speed divides every follower's time; the
//! window stays, being what keeps tones steady.
//!
//! **Gain**: attack and sustain scale their readings, ±100% giving their
//! whole reading in dB either way: at +100% attack an onset is lifted by
//! as much as it jumped, and at +100% sustain a tail dies away only as
//! fast as the sustain follower. Pump takes off [`PUMP_DEPTH`] of its
//! reading at 100%. The sum is capped at ±[`RANGE_DB`], then the mix
//! blends it with 1: a parallel blend that keeps the phase. The gain needs
//! no smoothing of its own: every reading moves only as fast as the level,
//! which jumps only where the sound does, so a jump in the gain lands on
//! the onset that made it. Smoothing it would let a tail's boost or an
//! onset's cut arrive a few samples late, on the next hit's first peak.
//!
//! **Split** runs the sound through a Linkwitz-Riley 24 dB/octave crossover
//! (two Butterworth filters in series, as in the multiband), as FL
//! Studio's Transient Processor does: only the highs get the gain, as a
//! second detector that hears only them calls for it, and the lows pass
//! under it. With nothing shaped the two halves sum flat.
//!
//! **Signal path**, per sample: the crossover; both detectors; the gain, on
//! the whole sound or the highs; the output gain; the clip, straight up to
//! [`CLIP_KNEE`] and a tanh above it, which nears 0 dBFS and never passes
//! it.
//!
//! **Switching**: the amounts, mix and output glide over [`SMOOTH_MS`];
//! split and clip crossfade, the crossover and both detectors running all
//! along so that the split fades between two settled gains (one detector
//! whose input faded from the highs to everything would read the fade as
//! an onset); the split frequency glides on a log scale; speed glides too,
//! on a log scale, and changes the followers' times, never their state. A
//! modulator moves any of them within a block instead (see "Modulation" in
//! `lib.rs`). Stages that do nothing are skipped, which is what keeps the
//! defaults exact.
//!
//! No latency: nothing looks ahead, so an onset's first samples are shaped
//! by what the detector has heard so far.

use instruments::ParamDescriptor;

use crate::dynamics::{DB_PER_LOG2, coefficient};
use crate::meter::{PeakMeter, block_peak};
use crate::{Category, Context, Effect, EffectDescriptor, OFF_ON, Values, choice, float};
use dsp::flush;
use dsp::svf::{Coefs, Crossover};
use dsp::{Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.transient",
    name: "Transient Shaper",
    description: "Boosts or cuts each hit's attack and sustain at any level, with pump, a split and a soft clip",
    category: Category::Dynamics,
    params: PARAMS,
    meters: &[
        "Gain change: the largest in the block, up or down (dB)",
        "Most boost, falling back (dB)",
        "Most cut, falling back (dB, 0 or below)",
        "Input peak (dB)",
        "Output peak (dB)",
    ],
    create: || Box::new(Transient::new()),
    ..EffectDescriptor::BASE
};

const PARAMS: &[ParamDescriptor] = &[
    float("attack", "Attack", -100.0, 100.0, 0.0, "%", false),
    float("sustain", "Sustain", -100.0, 100.0, 0.0, "%", false),
    float("pump", "Pump", 0.0, 100.0, 0.0, "%", false),
    float("speed", "Speed", MIN_SPEED, MAX_SPEED, 1.0, "×", true),
    choice("split", "Split", OFF_ON, 0),
    float(
        "split_freq",
        "Split frequency",
        20.0,
        5000.0,
        250.0,
        "Hz",
        true,
    ),
    float("output", "Output", -24.0, 12.0, 0.0, "dB", false),
    choice("clip", "Clip", OFF_ON, 0),
    float("mix", "Mix", 0.0, 100.0, 100.0, "%", false),
];

const COUNT: usize = 9;

/// -100% (cut each onset by as much as it jumped) to 100% (lift it by as
/// much).
pub const ATTACK: usize = 0;
/// -100% (a tail dies away twice as fast, less the sustain follower's
/// rate) to 100% (only as fast as the sustain follower).
pub const SUSTAIN: usize = 1;
/// 0 to 100%: how deep the dip after each onset is.
pub const PUMP: usize = 2;
/// Divides every follower's time: 2× is twice as snappy, 0.5× smoother.
pub const SPEED: usize = 3;
/// Shape only what is above [`SPLIT_FREQ`].
pub const SPLIT: usize = 4;
pub const SPLIT_FREQ: usize = 5;
pub const OUTPUT: usize = 6;
/// Soft-clip the output under 0 dBFS.
pub const CLIP: usize = 7;
pub const MIX: usize = 8;

/// [`SPEED`]'s range.
pub const MIN_SPEED: f32 = 0.25;
pub const MAX_SPEED: f32 = 4.0;

/// Meter slots, in [`DESCRIPTOR`]'s `meters` order. The gain change comes
/// first so that silence and a reset read 0, as [`Effect::meter`] expects.
pub const METER_GAIN: usize = 0;
pub const METER_BOOST: usize = 1;
pub const METER_CUT: usize = 2;
pub const METER_INPUT_PEAK: usize = 3;
pub const METER_OUTPUT_PEAK: usize = 4;

/// The level is the loudest sample over the last one to two windows this
/// long: steady through every cycle of a tone above 50 Hz.
pub const WINDOW_MS: f32 = 10.0;
/// At 1× speed: how long the level takes to fall once the window lets go.
pub const LEVEL_RELEASE_MS: f32 = 10.0;
/// At 1× speed: how long the attack follower takes to rise to the level,
/// which is how long a boosted or cut onset lasts.
pub const ATTACK_MS: f32 = 15.0;
/// At 1× speed: how long the sustain follower takes to fall to the level,
/// which sets how much of a decay counts as a tail.
pub const SUSTAIN_MS: f32 = 250.0;
/// At 1× speed: how long the pump takes to let go after an onset.
pub const PUMP_MS: f32 = 100.0;
/// The share of the pump reading taken off at 100%.
pub const PUMP_DEPTH: f32 = 0.5;
/// Each reading, and the gain change they make together, is at most this
/// many dB either way.
pub const RANGE_DB: f32 = 24.0;
/// Where the clip starts to bend, as an amplitude (-6 dBFS).
pub const CLIP_KNEE: f32 = 0.5;
/// How long the amounts, mix and output glide, and split and clip fade.
const SMOOTH_MS: f32 = 20.0;
/// Samples between the split frequency's and the speed's glide steps, each
/// of which works out the filters' or the followers' coefficients again.
const CHUNK: usize = 16;
/// How fast the boost and cut meters fall back, in dB per second: fast
/// enough to show each hit, slow enough that a UI reading them at its own
/// frame rate sees every one.
const METER_FALL_DB_PER_S: f32 = 60.0;
/// The detector takes no sample louder than this (+120 dB) and NaN as
/// silence, so garbage can't poison its followers.
const LOUDEST: f32 = 1e6;
/// Added to both sides of a ratio so that silence over silence reads 1.
const TINY: f32 = 1e-20;
/// The rate [`hit_response`] works at, and the longest span it covers.
const HIT_RATE: f32 = 8000.0;
const HIT_MAX_MS: f32 = 10_000.0;
/// How long [`hit_level`]'s hit takes to die away by 60 dB.
pub const HIT_DECAY_MS: f32 = 300.0;

/// `over / under` in dB, from 0 to [`RANGE_DB`].
#[inline]
fn ratio_db(over: f32, under: f32) -> f32 {
    (DB_PER_LOG2 * ((over + TINY) / (under + TINY)).log2()).clamp(0.0, RANGE_DB)
}

/// The louder side's magnitude, as the detector takes it: NaN as silence
/// and anything huge as [`LOUDEST`].
#[inline]
fn magnitude(l: f32, r: f32) -> f32 {
    let level = l.abs().max(r.abs());
    if level < LOUDEST {
        level
    } else if level >= LOUDEST {
        LOUDEST
    } else {
        0.0
    }
}

/// The clip: `x` as it is up to [`CLIP_KNEE`], then a tanh that bends
/// it towards 1 with the same slope where it starts.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= CLIP_KNEE {
        return x;
    }
    let width = 1.0 - CLIP_KNEE;
    (CLIP_KNEE + width * ((a - CLIP_KNEE) / width).tanh()).copysign(x)
}

/// The level, its followers and the readings they give (see the module
/// documentation), for one linked stereo pair.
#[derive(Debug, Clone, Default)]
struct Detector {
    /// Samples per window, and left in the current one.
    window: usize,
    countdown: usize,
    /// The loudest sample of the current window and of the one before.
    current: f32,
    previous: f32,
    /// The level, and the followers that rise after it and fall after it.
    level: f32,
    rise: f32,
    hold: f32,
    /// The attack reading's recent peak, in dB, for the pump.
    pump: f32,
    /// One-pole coefficients for the times at the speed set.
    level_release: f32,
    rise_rate: f32,
    hold_release: f32,
    pump_release: f32,
}

impl Detector {
    fn prepare(&mut self, sample_rate: f32) {
        self.window = ((WINDOW_MS * 0.001 * sample_rate).round() as usize).max(1);
    }

    fn set_speed(&mut self, speed: f32, sample_rate: f32) {
        self.level_release = coefficient(LEVEL_RELEASE_MS / speed, sample_rate);
        self.rise_rate = coefficient(ATTACK_MS / speed, sample_rate);
        self.hold_release = coefficient(SUSTAIN_MS / speed, sample_rate);
        self.pump_release = coefficient(PUMP_MS / speed, sample_rate);
    }

    fn reset(&mut self) {
        self.countdown = self.window;
        self.current = 0.0;
        self.previous = 0.0;
        self.level = 0.0;
        self.rise = 0.0;
        self.hold = 0.0;
        self.pump = 0.0;
    }

    /// Follows one sample's magnitude.
    #[inline]
    fn track(&mut self, magnitude: f32) {
        self.current = self.current.max(magnitude);
        let peak = self.current.max(self.previous);
        self.countdown -= 1;
        if self.countdown == 0 {
            self.previous = self.current;
            self.current = 0.0;
            self.countdown = self.window;
        }
        self.level = if peak >= self.level {
            peak
        } else {
            peak + (self.level - peak) * self.level_release
        };
        self.rise = if self.level > self.rise {
            self.level + (self.rise - self.level) * self.rise_rate
        } else {
            self.level
        };
        self.hold = if self.level >= self.hold {
            self.level
        } else {
            self.level + (self.hold - self.level) * self.hold_release
        };
    }

    /// The gain change in dB the readings call for, with the amounts at
    /// `attack` and `sustain` (-1 to 1) and `pump` (0 to 1). Moves the
    /// pump's follower, so call it once per sample.
    #[inline]
    fn target_db(&mut self, attack: f32, sustain: f32, pump: f32) -> f32 {
        let attack_db = ratio_db(self.level, self.rise);
        let sustain_db = ratio_db(self.hold, self.level);
        self.pump = if attack_db >= self.pump {
            attack_db
        } else {
            attack_db + (self.pump - attack_db) * self.pump_release
        };
        let pump_db = self.pump - attack_db;
        (attack * attack_db + sustain * sustain_db - pump * PUMP_DEPTH * pump_db)
            .clamp(-RANGE_DB, RANGE_DB)
    }

    /// Ends a decay in zeros rather than denormals.
    fn flush(&mut self) {
        for value in [
            &mut self.current,
            &mut self.previous,
            &mut self.level,
            &mut self.rise,
            &mut self.hold,
            &mut self.pump,
        ] {
            *value = flush(*value);
        }
    }
}

/// The level of the hit [`hit_response`] shapes, `ms` after it starts:
/// full scale at once out of silence, then dying away by 60 dB over
/// [`HIT_DECAY_MS`], as a drum does.
pub fn hit_level(ms: f32) -> f32 {
    if ms >= 0.0 {
        dsp::db_to_gain(-60.0 * ms / HIT_DECAY_MS)
    } else {
        0.0
    }
}

/// What parameter values `values` (in [`DESCRIPTOR`]'s order; missing or
/// NaN ones count as defaults) do to [`hit_level`]'s hit: the gain change
/// in dB, the mix's blend included, at each of `out`'s points, spread
/// evenly from the onset to `span_ms` after it. What the UI draws. Runs
/// the detector itself, so it is exactly what the effect does to a hit
/// that has that level all through each cycle.
pub fn hit_response(values: &[f32], span_ms: f32, out: &mut [f32]) {
    let value = |index: usize| crate::param_value(PARAMS, values, index);
    let mut detector = Detector::default();
    detector.prepare(HIT_RATE);
    detector.set_speed(value(SPEED), HIT_RATE);
    detector.reset();
    let (attack, sustain) = (value(ATTACK) / 100.0, value(SUSTAIN) / 100.0);
    let (pump, mix) = (value(PUMP) / 100.0, value(MIX) / 100.0);
    let span = if span_ms.is_nan() {
        1.0
    } else {
        span_ms.clamp(1.0, HIT_MAX_MS)
    };
    let steps = (span * HIT_RATE / 1000.0).round() as usize;
    let last = out.len().saturating_sub(1).max(1) as f32;
    let mut point = 0;
    for step in 0..=steps {
        detector.track(hit_level(step as f32 * 1000.0 / HIT_RATE));
        let gain_db = detector.target_db(attack, sustain, pump);
        let wet = (gain_db / DB_PER_LOG2).exp2();
        let mixed = dsp::gain_to_db(1.0 + mix * (wet - 1.0));
        while point < out.len()
            && (point as f32 / last * span * HIT_RATE / 1000.0).round() as usize <= step
        {
            out[point] = mixed;
            point += 1;
        }
    }
}

pub struct Transient {
    values: Values<COUNT>,
    sample_rate: f32,
    /// On the whole sound, and on the highs above the split.
    detectors: [Detector; 2],
    /// The amounts: attack and sustain -1 to 1, pump 0 to 1.
    attack: Smoothed,
    sustain: Smoothed,
    pump: Smoothed,
    mix: Smoothed,
    /// The output gain as an amplitude factor.
    output: Smoothed,
    /// How much of the split's and of the clip's sound is heard, 0 to 1.
    split: Smoothed,
    clip: Smoothed,
    /// The split frequency as log2, gliding once per [`CHUNK`].
    split_freq: Smoothed,
    /// The speed as log2, gliding once per [`CHUNK`], so a modulated
    /// speed never lets a follower lagging behind catch up all at once.
    speed: Smoothed,
    coefs: Coefs,
    crossovers: [Crossover; 2],
    /// Samples left until the split frequency's next glide step.
    countdown: usize,
    /// The meters' gain change, boost and cut, in dB.
    change: f32,
    boost: f32,
    cut: f32,
    /// How far the boost and cut meters fall back per sample, in dB.
    fall: f32,
    input_peak: PeakMeter,
    output_peak: PeakMeter,
}

impl Transient {
    pub fn new() -> Transient {
        let smoothed = |value: f32| Smoothed::new(value, SMOOTH_MS);
        let mut transient = Transient {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            detectors: [Detector::default(), Detector::default()],
            attack: smoothed(0.0),
            sustain: smoothed(0.0),
            pump: smoothed(0.0),
            mix: smoothed(1.0),
            output: smoothed(1.0),
            split: smoothed(0.0),
            clip: smoothed(0.0),
            split_freq: smoothed(250f32.log2()),
            speed: smoothed(0.0),
            coefs: Coefs::butterworth(250.0, 48_000.0),
            crossovers: [Crossover::default(); 2],
            countdown: 0,
            change: 0.0,
            boost: 0.0,
            cut: 0.0,
            fall: 0.0,
            input_peak: PeakMeter::default(),
            output_peak: PeakMeter::default(),
        };
        transient.prepare(48_000.0, 0);
        transient
    }

    /// The smoothers on the audio path, which tick every sample.
    fn path(&mut self) -> [&mut Smoothed; 7] {
        [
            &mut self.attack,
            &mut self.sustain,
            &mut self.pump,
            &mut self.mix,
            &mut self.output,
            &mut self.split,
            &mut self.clip,
        ]
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`; the switches always fade at the knob's.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        let on = if value >= 0.5 { 1.0 } else { 0.0 };
        match index {
            ATTACK => self.attack.move_to(value / 100.0, pace),
            SUSTAIN => self.sustain.move_to(value / 100.0, pace),
            PUMP => self.pump.move_to(value / 100.0, pace),
            SPEED => self.speed.move_to(value.log2(), pace),
            SPLIT => self.split.set_target(on),
            SPLIT_FREQ => self.split_freq.move_to(value.log2(), pace),
            OUTPUT => self.output.move_to(dsp::db_to_gain(value), pace),
            CLIP => self.clip.set_target(on),
            MIX => self.mix.move_to(value / 100.0, pace),
            _ => {}
        }
    }

    /// Tunes the detectors' followers to the speed.
    fn update_speed(&mut self) {
        let speed = self.speed.current().exp2();
        for detector in &mut self.detectors {
            detector.set_speed(speed, self.sample_rate);
        }
    }

    fn update_coefs(&mut self) {
        self.coefs = Coefs::butterworth(self.split_freq.current().exp2(), self.sample_rate);
    }
}

impl Default for Transient {
    fn default() -> Self {
        Transient::new()
    }
}

impl Effect for Transient {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        for smoothed in self.path() {
            smoothed.prepare(sample_rate);
        }
        self.split_freq.prepare_every(sample_rate, CHUNK);
        self.speed.prepare_every(sample_rate, CHUNK);
        for detector in &mut self.detectors {
            detector.prepare(sample_rate);
        }
        self.fall = METER_FALL_DB_PER_S / sample_rate;
        self.input_peak.prepare(sample_rate);
        self.output_peak.prepare(sample_rate);
        for index in 0..COUNT {
            self.apply(index, Pace::Knob);
        }
        self.reset();
    }

    fn reset(&mut self) {
        for smoothed in self.path() {
            smoothed.settle();
        }
        self.split_freq.settle();
        self.update_coefs();
        self.speed.settle();
        self.update_speed();
        self.crossovers = [Crossover::default(); 2];
        self.countdown = 0;
        for detector in &mut self.detectors {
            detector.reset();
        }
        self.change = 0.0;
        self.boost = 0.0;
        self.cut = 0.0;
        self.input_peak.reset();
        self.output_peak.reset();
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.values.played();
        let len = left.len().min(right.len());
        let (left, right) = (&mut left[..len], &mut right[..len]);
        self.input_peak
            .update(block_peak(left).max(block_peak(right)), len);
        // The stages with something to do this block; the others leave
        // the sound as it is.
        let shaping = self.attack.is_away_from(0.0)
            || self.sustain.is_away_from(0.0)
            || self.pump.is_away_from(0.0);
        let split_on = self.split.is_away_from(0.0);
        let output_on = self.output.is_away_from(1.0);
        let clip_on = self.clip.is_away_from(0.0);
        // Whose gain is heard: the whole sound's unless the split is all
        // the way in, the highs' if it is in at all.
        let whole_heard = !split_on || self.split.current() < 1.0 || self.split.is_smoothing();
        // A pump's follower that isn't moved starts afresh when it's next
        // wanted.
        for (detector, heard) in self.detectors.iter_mut().zip([whole_heard, split_on]) {
            if !shaping || !heard {
                detector.pump = 0.0;
            }
        }
        // The block's least and most gain, for the meters.
        let (mut least, mut most) = (1.0f32, 1.0f32);

        for n in 0..len {
            if self.countdown == 0 {
                if self.split_freq.is_smoothing() {
                    self.split_freq.tick();
                    self.update_coefs();
                }
                if self.speed.is_smoothing() {
                    self.speed.tick();
                    self.update_speed();
                }
                self.countdown = CHUNK;
            }
            self.countdown -= 1;

            let (l, r) = (left[n], right[n]);
            // The crossover runs whether it is heard or not, so it comes
            // in settled.
            let [cross_l, cross_r] = &mut self.crossovers;
            let (low_l, high_l) = cross_l.split(&self.coefs, l);
            let (low_r, high_r) = cross_r.split(&self.coefs, r);
            let [whole, highs] = &mut self.detectors;
            whole.track(magnitude(l, r));
            highs.track(magnitude(high_l, high_r));
            let split = if split_on { self.split.tick() } else { 0.0 };

            // The gain on the whole sound, and on the highs.
            let (mut gain, mut high_gain) = (1.0, 1.0);
            if shaping {
                let (attack, sustain) = (self.attack.tick(), self.sustain.tick());
                let (pump, mix) = (self.pump.tick(), self.mix.tick());
                let blended = |detector: &mut Detector| {
                    let wet = (detector.target_db(attack, sustain, pump) / DB_PER_LOG2).exp2();
                    1.0 + mix * (wet - 1.0)
                };
                if whole_heard {
                    gain = blended(whole);
                }
                if split_on {
                    high_gain = blended(highs);
                }
                let shown = gain + split * (high_gain - gain);
                least = least.min(shown);
                most = most.max(shown);
            }
            let (mut out_l, mut out_r) = (l * gain, r * gain);
            if split_on {
                out_l += split * (low_l + high_gain * high_l - out_l);
                out_r += split * (low_r + high_gain * high_r - out_r);
            }
            if output_on {
                let output = self.output.tick();
                out_l *= output;
                out_r *= output;
            }
            if clip_on {
                let clip = self.clip.tick();
                out_l += clip * (soft_clip(out_l) - out_l);
                out_r += clip * (soft_clip(out_r) - out_r);
            }
            left[n] = out_l;
            right[n] = out_r;
        }

        for crossover in &mut self.crossovers {
            crossover.flush();
        }
        for detector in &mut self.detectors {
            detector.flush();
        }
        let (down, up) = (dsp::gain_to_db(least), dsp::gain_to_db(most));
        self.change = if up >= -down { up } else { down };
        let fallen = self.fall * len as f32;
        self.boost = (self.boost - fallen).max(up).max(0.0);
        self.cut = (self.cut + fallen).min(down).min(0.0);
        self.output_peak
            .update(block_peak(left).max(block_peak(right)), len);
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
        out[METER_GAIN] = self.change;
        out[METER_BOOST] = self.boost;
        out[METER_CUT] = self.cut;
        out[METER_INPUT_PEAK] = self.input_peak.db();
        out[METER_OUTPUT_PEAK] = self.output_peak.db();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults;
    use crate::meter::METER_FLOOR_DB;
    use crate::testing::{RATE, db, noise, peak, rms, run, set, sine};

    fn shaper(settings: &[(&str, f32)]) -> Transient {
        let mut transient = Transient::new();
        transient.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut transient, id, value);
        }
        transient.reset();
        transient
    }

    fn processed(settings: &[(&str, f32)], left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let mut transient = shaper(settings);
        let (mut l, mut r) = (left.to_vec(), right.to_vec());
        run(&mut transient, &mut l, &mut r, 64, &Context::DEFAULT);
        (l, r)
    }

    /// Samples from one hit to the next: 400 ms.
    const EVERY: usize = 19_200;

    /// `count` drum-like hits, one every [`EVERY`] samples: noise (a
    /// snare's rattle) that starts out of silence with its peaks at about
    /// `level` and dies away as [`hit_level`] does, 60 dB over 300 ms.
    fn hits(count: usize, level: f32, seed: u32) -> Vec<f32> {
        noise(EVERY * count, seed)
            .iter()
            .enumerate()
            .map(|(n, s)| level * s * hit_level((n % EVERY) as f32 / RATE * 1000.0))
            .collect()
    }

    /// The onsets' peak (their first 5 ms) and the tails' level (100 to
    /// 250 ms in), in dB, over every hit.
    fn onset_and_tail(signal: &[f32]) -> (f64, f64) {
        let (mut onsets, mut tails) = (Vec::new(), Vec::new());
        for hit in signal.chunks(EVERY) {
            onsets.extend_from_slice(&hit[..240]);
            tails.extend_from_slice(&hit[4800..12_000]);
        }
        (db(f64::from(peak(&onsets))), db(rms(&tails)))
    }

    /// How much `settings` change the onsets and the tails of [`hits`] at
    /// `level`, in dB.
    fn changes(settings: &[(&str, f32)], level: f32) -> (f64, f64) {
        let input = hits(4, level, 1);
        let other = hits(4, level, 2);
        let (dry_onset, dry_tail) = onset_and_tail(&input);
        let (l, r) = processed(settings, &input, &other);
        assert!(l.iter().chain(&r).all(|s| s.is_finite()));
        let (onset, tail) = onset_and_tail(&l);
        (onset - dry_onset, tail - dry_tail)
    }

    #[test]
    fn the_defaults_pass_the_sound_bit_for_bit() {
        let (left, right) = (hits(2, 0.7, 3), noise(2 * EVERY, 4));
        assert_eq!(processed(&[], &left, &right), (left.clone(), right.clone()));

        // Every parameter turned up while the sound plays, then back:
        // once the glides end, exact again.
        let mut transient = shaper(&[]);
        let (mut l, mut r) = (left.clone(), right.clone());
        let mut at = 0;
        let mut play = |transient: &mut Transient, until: usize| {
            run(
                transient,
                &mut l[at..until],
                &mut r[at..until],
                64,
                &Context::DEFAULT,
            );
            at = until;
        };
        play(&mut transient, 4800);
        for (index, param) in PARAMS.iter().enumerate() {
            let top = match param.kind {
                instruments::ParameterKind::Float { max, .. } => max,
                instruments::ParameterKind::Enum { labels, .. } => (labels.len() - 1) as f32,
            };
            transient.set_param(index, top);
        }
        play(&mut transient, 9600);
        for (index, param) in PARAMS.iter().enumerate() {
            transient.set_param(index, param.default_value());
        }
        play(&mut transient, 2 * EVERY);
        let exact = 9600 + 960 + 128;
        assert_eq!(&l[exact..], &left[exact..]);
        assert_eq!(&r[exact..], &right[exact..]);
    }

    #[test]
    fn attack_lifts_or_lowers_the_onsets_and_leaves_the_tails() {
        for (attack, least) in [(100.0, 9.0), (50.0, 4.5), (-50.0, -4.5), (-100.0, -9.0)] {
            let (onset, tail) = changes(&[("attack", attack)], 0.5);
            println!("attack {attack:+}%: onsets {onset:+.2} dB, tails {tail:+.2} dB");
            let lift = onset - tail;
            if least > 0.0 {
                assert!(lift > least, "{lift}");
            } else {
                assert!(lift < least, "{lift}");
            }
            assert!(tail.abs() < 0.5, "{tail}");
        }
    }

    #[test]
    fn sustain_lifts_or_lowers_the_tails_and_leaves_the_onsets() {
        for (sustain, least) in [(100.0, 9.0), (50.0, 4.5), (-50.0, -4.5), (-100.0, -9.0)] {
            let (onset, tail) = changes(&[("sustain", sustain)], 0.5);
            println!("sustain {sustain:+}%: onsets {onset:+.2} dB, tails {tail:+.2} dB");
            if least > 0.0 {
                assert!(tail > least, "{tail}");
            } else {
                assert!(tail < least, "{tail}");
            }
            assert!(onset.abs() < 0.5, "{onset}");
        }
    }

    #[test]
    fn quiet_and_loud_hits_are_shaped_alike() {
        let settings = [
            ("attack", 60.0),
            ("sustain", -40.0),
            ("pump", 30.0),
            ("speed", 1.5),
        ];
        let (quiet_onset, quiet_tail) = changes(&settings, dsp::db_to_gain(-30.0));
        let (loud_onset, loud_tail) = changes(&settings, dsp::db_to_gain(-6.0));
        println!(
            "at -30 dBFS: onsets {quiet_onset:+.4} dB, tails {quiet_tail:+.4} dB; \
             at -6 dBFS: {loud_onset:+.4} dB, {loud_tail:+.4} dB"
        );
        assert!((quiet_onset - loud_onset).abs() < 0.01);
        assert!((quiet_tail - loud_tail).abs() < 0.01);

        // Sample for sample, the gain is the same.
        let scale = dsp::db_to_gain(24.0);
        let quiet = hits(2, dsp::db_to_gain(-30.0), 5);
        let loud: Vec<f32> = quiet.iter().map(|s| s * scale).collect();
        let (quiet_out, _) = processed(&settings, &quiet, &quiet);
        let (loud_out, _) = processed(&settings, &loud, &loud);
        let worst = quiet_out
            .iter()
            .zip(&loud_out)
            .fold(0.0f32, |m, (q, l)| m.max((q * scale - l).abs()));
        println!(
            "largest difference: {worst:e} of a {} peak",
            peak(&loud_out)
        );
        assert!(worst < 1e-4 * peak(&loud_out), "{worst}");
    }

    #[test]
    fn pump_dips_after_each_onset_without_raising_the_peaks() {
        let input = hits(4, 0.5, 6);
        let (l, _) = processed(&[("pump", 100.0)], &input, &input);
        assert!(peak(&l) <= peak(&input));
        // Just after each onset (20 to 80 ms in) and the onset itself.
        let level = |signal: &[f32], from: usize, to: usize| {
            let part: Vec<f32> = signal
                .chunks(EVERY)
                .flat_map(|hit| hit[from..to].to_vec())
                .collect();
            db(rms(&part))
        };
        let after = level(&l, 960, 3840) - level(&input, 960, 3840);
        let onset = level(&l, 0, 48) - level(&input, 0, 48);
        println!("pump 100%: onsets {onset:+.2} dB, 20 to 80 ms in {after:+.2} dB");
        assert!(after < -4.0, "{after}");
        assert!(onset > -1.0, "{onset}");
    }

    #[test]
    fn speed_scales_the_times() {
        // How long a +100% attack's lift on a hit stays above 6 dB.
        let lift_ms = |speed: f32| {
            let mut values: [f32; COUNT] = defaults(PARAMS);
            values[ATTACK] = 100.0;
            values[SPEED] = speed;
            let mut curve = [0.0; 1001];
            hit_response(&values, 100.0, &mut curve);
            assert!(curve[0] > 20.0, "{}", curve[0]);
            curve.iter().position(|&g| g < 6.0).unwrap() as f32 * 0.1
        };
        let (normal, fast) = (lift_ms(1.0), lift_ms(4.0));
        println!("attack lift over 6 dB: {normal} ms at 1x, {fast} ms at 4x");
        assert!((3.5..4.5).contains(&(normal / fast)), "{normal} {fast}");

        // The sustain lifts the tail, more the further it has fallen, and
        // garbage values read as defaults.
        let mut values: [f32; COUNT] = defaults(PARAMS);
        values[SUSTAIN] = 100.0;
        let mut curve = [0.0; 31];
        hit_response(&values, 300.0, &mut curve);
        println!("sustain +100% every 10 ms: {curve:.1?}");
        assert!(curve[0].abs() < 0.01 && curve[10] > 6.0 && curve[20] > curve[10]);
        hit_response(&[f32::NAN; 3], f32::INFINITY, &mut curve);
        assert!(curve.iter().all(|&g| g == 0.0));
    }

    #[test]
    fn the_split_shapes_only_the_highs_and_sums_flat() {
        // Nothing shaped: the two halves add back up to the level that
        // went in, at every frequency.
        let len = 3 * RATE as usize / 2;
        let settings = [("split", 1.0), ("split_freq", 300.0)];
        let mut worst = 0.0f64;
        for freq in [30.0, 100.0, 250.0, 300.0, 360.0, 1000.0, 5000.0, 15_000.0] {
            let input = sine(freq, 0.5, len, RATE);
            let (l, _) = processed(&settings, &input, &input);
            let tail = len - RATE as usize;
            let change = db(rms(&l[tail..]) / rms(&input[tail..]));
            println!("{freq:6} Hz: {change:+.4} dB");
            worst = worst.max(change.abs());
        }
        assert!(worst < 0.05, "{worst}");

        // A full attack over a 1 kHz split: hits of a 100 Hz tone keep
        // their onsets, hits of a 5 kHz one get theirs lifted.
        let tone_hits = |freq: f64| -> Vec<f32> {
            sine(freq, 0.5, EVERY * 3, RATE)
                .iter()
                .enumerate()
                .map(|(n, s)| s * hit_level((n % EVERY) as f32 / RATE * 1000.0))
                .collect()
        };
        let settings = [("split", 1.0), ("split_freq", 1000.0), ("attack", 100.0)];
        for (freq, lifted) in [(100.0, false), (5000.0, true)] {
            let input = tone_hits(freq);
            let (l, _) = processed(&settings, &input, &input);
            let onset = onset_and_tail(&l).0 - onset_and_tail(&input).0;
            println!("{freq} Hz hits, split at 1 kHz: onsets {onset:+.2} dB");
            if lifted {
                assert!(onset > 6.0, "{onset}");
            } else {
                assert!(onset.abs() < 1.0, "{onset}");
            }
        }
    }

    #[test]
    fn the_clip_keeps_the_output_under_full_scale() {
        let input = hits(3, 0.9, 7);
        let settings = [("attack", 100.0), ("output", 12.0), ("clip", 1.0)];
        let (l, r) = processed(&settings, &input, &input);
        let loudest = peak(&l).max(peak(&r));
        println!("clipped peak {loudest}");
        assert!(loudest <= 1.0 && loudest > 0.9, "{loudest}");
        // Under the knee, nothing changes.
        assert_eq!(soft_clip(0.4), 0.4);
        assert_eq!(soft_clip(-CLIP_KNEE), -CLIP_KNEE);
        assert!(soft_clip(f32::INFINITY) == 1.0 && soft_clip(-50.0) >= -1.0);
    }

    #[test]
    fn mix_blends_the_gain_with_the_dry_sound() {
        let input = hits(2, 0.5, 8);
        let (l, r) = processed(&[("attack", 100.0), ("mix", 0.0)], &input, &input);
        assert_eq!((&l, &r), (&input, &input));
        let (full, _) = changes(&[("attack", 100.0)], 0.5);
        let (half, _) = changes(&[("attack", 100.0), ("mix", 50.0)], 0.5);
        println!("attack +100%: onsets {full:+.2} dB wet, {half:+.2} dB half wet");
        assert!(half > 1.0 && half < full - 3.0);
    }

    #[test]
    fn moving_parameters_never_clicks() {
        // A 200 Hz tone dying away 20 dB a second, which the sustain reads
        // all along, so the gain is at work while parameters move: one
        // every 25 ms, to random values. All but the clip: clipping a loud
        // tone steepens its flanks, which is its sound, not a click.
        let len = 4 * RATE as usize;
        let tone: Vec<f32> = sine(200.0, 0.5, len, RATE)
            .iter()
            .enumerate()
            .map(|(n, s)| s * dsp::db_to_gain(-20.0 * (n % (2 * RATE as usize)) as f32 / RATE))
            .collect();
        let mut transient = shaper(&[("sustain", 50.0)]);
        let (mut l, mut r) = (tone.clone(), tone.clone());
        let mut seed = 0x5eed_1234u32;
        let mut random = || dsp::xorshift32(&mut seed) as f32 / u32::MAX as f32;
        for (block, (l, r)) in l.chunks_mut(1200).zip(r.chunks_mut(1200)).enumerate() {
            let index = (random() * COUNT as f32) as usize % COUNT;
            if block > 0 && index != CLIP {
                let (min, max) = match PARAMS[index].kind {
                    instruments::ParameterKind::Float { min, max, .. } => (min, max),
                    instruments::ParameterKind::Enum { labels, .. } => {
                        (0.0, (labels.len() - 1) as f32)
                    }
                };
                transient.set_param(index, min + (max - min) * random());
            }
            run(&mut transient, l, r, 64, &Context::DEFAULT);
        }
        // Each step against the loudest sample around it: a 200 Hz sine
        // moves by at most 2.6% of its peak a sample.
        let mut worst = 0.0f32;
        for n in 1200..len - 1 {
            // Not across the tone's own restart, half way.
            if (2 * RATE as usize - 480..2 * RATE as usize + 480).contains(&n) {
                continue;
            }
            let around = &l[n.saturating_sub(480)..(n + 480).min(len)];
            let local = peak(around);
            if local > 1e-3 {
                worst = worst.max((l[n + 1] - l[n]).abs() / local);
            }
        }
        println!("largest step: {:.2}% of the local peak", 100.0 * worst);
        assert!(worst < 0.04, "{worst}");
    }

    #[test]
    fn reset_forgets_everything() {
        let settings = [
            ("attack", 80.0),
            ("sustain", 70.0),
            ("pump", 50.0),
            ("split", 1.0),
            ("clip", 1.0),
        ];
        let input = hits(2, 0.8, 9);
        let mut used = shaper(&settings);
        let (mut l, mut r) = (input.clone(), input.clone());
        run(&mut used, &mut l, &mut r, 64, &Context::DEFAULT);
        assert!(used.meter() != 0.0);
        used.reset();
        assert_eq!(used.meter(), 0.0);
        let mut meters = [0.0; crate::MAX_METERS];
        used.meters(&mut meters);
        assert_eq!(&meters[..3], &[0.0; 3]);
        assert_eq!(meters[METER_INPUT_PEAK], METER_FLOOR_DB);
        // As if new.
        let (mut l, mut r) = (input.clone(), input.clone());
        run(&mut used, &mut l, &mut r, 64, &Context::DEFAULT);
        assert_eq!((l, r), processed(&settings, &input, &input));
    }

    #[test]
    fn silence_stays_silent_and_garbage_passes() {
        let mut transient = shaper(&[
            ("attack", 100.0),
            ("sustain", 100.0),
            ("pump", 100.0),
            ("split", 1.0),
            ("output", 12.0),
            ("clip", 1.0),
        ]);
        let (mut l, mut r) = (vec![0.0; 4800], vec![0.0; 4800]);
        run(&mut transient, &mut l, &mut r, 64, &Context::DEFAULT);
        assert!(l.iter().chain(&r).all(|&s| s == 0.0));
        // Garbage in: once it has gone, the sound comes out as it would
        // have, and the meters stay finite.
        let mut garbage = [f32::NAN, f32::INFINITY, 0.5, f32::NEG_INFINITY];
        let mut other = garbage;
        transient.process(&mut garbage, &mut other, &Context::DEFAULT);
        let mut meters = [0.0; crate::MAX_METERS];
        transient.meters(&mut meters);
        assert!(meters.iter().all(|m| m.is_finite()), "{:?}", &meters[..5]);
        let input = hits(1, 0.5, 10);
        let (mut l, mut r) = (input.clone(), input.clone());
        run(&mut transient, &mut l, &mut r, 64, &Context::DEFAULT);
        assert!(l.iter().chain(&r).all(|s| s.is_finite()));
        assert!(peak(&l) > 0.1);
        let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
        transient.process(&mut l, &mut r, &Context::DEFAULT);
        transient.meters(&mut meters);
        assert!(meters.iter().all(|m| m.is_finite()));
    }

    #[test]
    fn meters_show_the_boost_and_the_cut() {
        let input = hits(2, 0.5, 11);
        let read = |settings: &[(&str, f32)]| {
            let mut transient = shaper(settings);
            let mut most = [0.0f32; 3];
            for chunk in input.chunks(64) {
                let (mut l, mut r) = (chunk.to_vec(), chunk.to_vec());
                transient.process(&mut l, &mut r, &Context::DEFAULT);
                let mut meters = [0.0; crate::MAX_METERS];
                transient.meters(&mut meters);
                most[0] = most[0].max(meters[METER_GAIN].abs());
                most[1] = most[1].max(meters[METER_BOOST]);
                most[2] = most[2].min(meters[METER_CUT]);
            }
            most
        };
        let boosted = read(&[("attack", 100.0)]);
        let cut = read(&[("attack", -100.0)]);
        println!("boosted: {boosted:?}, cut: {cut:?}");
        assert!(boosted[0] > 20.0 && boosted[1] > 20.0 && boosted[2] == 0.0);
        assert!(cut[0] > 20.0 && cut[1] == 0.0 && cut[2] < -20.0);
        assert_eq!(read(&[]), [0.0; 3]);
    }
}
