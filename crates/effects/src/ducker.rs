//! Ducker: turns the sound down when another track hits, precisely and the
//! same way every time, like Nicky Romero's Kickstart or a sidechained
//! Kilohearts Compactor. Route the kick (or anything) to its sidechain.
//!
//! - **Hits:** each time the sidechain rises through the threshold, the
//!   gain follows the duck shape: down by `depth` over `attack`, held for
//!   `hold`, back up over `release` along `curve`. Every hit ducks the same,
//!   however loud it is, which is what makes a pump sound tight.
//! - **Level:** the gain follows the sidechain's level instead, like a
//!   compressor with no ratio to set: fully ducked 12 dB over the threshold.
//! - **Tempo:** the shape fires on every beat division, locked to the song,
//!   with no sidechain needed.
//!
//! The sidechain can be low-passed first, so only a kick's thump triggers.
//! Lookahead delays this track's own sound (but not the sidechain), so the
//! duck is already down when the hit arrives; it adds that much latency.
//!
//! Depth, mix and the shape's attack, hold, release and curve glide over
//! [`SMOOTH_MS`] (a modulator's moves within a block; see "Modulation" in
//! `lib.rs`), so moving them reshapes a duck under way without a jump;
//! lookahead, which is latency, always crossfades.

use dsp::{db_to_gain, gain_to_db};
use instruments::ParamDescriptor;

use crate::line::{Crossfade, Ring};
use crate::{Context, Effect, EffectDescriptor, MAX_METERS, Values, choice, float};
use dsp::flush;
use dsp::{Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.ducker",
    name: "Ducker",
    description: "Precise sidechain ducking: by hit, by level, or to the beat",
    category: crate::Category::Dynamics,
    params: PARAMS,
    meters: &[
        "Gain reduction (dB)",
        "Sidechain level (dB)",
        "Time since the last hit (ms)",
    ],
    sidechain: true,
    latency: |values, sample_rate| {
        let ms = values
            .get(LOOKAHEAD)
            .copied()
            .filter(|v| v.is_finite())
            .unwrap_or(0.0)
            .clamp(0.0, MAX_LOOKAHEAD_MS);
        (ms / 1000.0 * sample_rate).round() as usize
    },
    create: || Box::new(Ducker::new()),
    ..EffectDescriptor::BASE
};

/// Beat divisions for Tempo mode, and their length in quarter notes.
pub const RATES: [(&str, f64); 6] = [
    ("1/16", 0.25),
    ("1/8", 0.5),
    ("1/4", 1.0),
    ("1/2", 2.0),
    ("1 bar", 4.0),
    ("2 bars", 8.0),
];

const PARAMS: &[ParamDescriptor] = &[
    choice("mode", "Trigger", &["Hits", "Level", "Tempo"], 0),
    float("threshold", "Threshold", -60.0, 0.0, -24.0, "dB", false),
    float("depth", "Depth", 0.0, 60.0, 24.0, "dB", false),
    float("attack", "Attack", 0.0, 50.0, 1.0, "ms", false),
    float("hold", "Hold", 0.0, 500.0, 20.0, "ms", false),
    float("release", "Release", 5.0, 2000.0, 180.0, "ms", true),
    float("curve", "Curve", -1.0, 1.0, 0.4, "", false),
    float("lookahead", "Lookahead", 0.0, 10.0, 0.0, "ms", false),
    float(
        "sc_lowpass",
        "Listen below",
        40.0,
        20_000.0,
        20_000.0,
        "Hz",
        true,
    ),
    choice(
        "rate",
        "Rate",
        &["1/16", "1/8", "1/4", "1/2", "1 bar", "2 bars"],
        2,
    ),
    float("mix", "Mix", 0.0, 100.0, 100.0, "%", false),
];

const COUNT: usize = 11;
const MODE: usize = 0;
const THRESHOLD: usize = 1;
const DEPTH: usize = 2;
const ATTACK: usize = 3;
const HOLD: usize = 4;
const RELEASE: usize = 5;
const CURVE: usize = 6;
const LOOKAHEAD: usize = 7;
const SC_LOWPASS: usize = 8;
const RATE: usize = 9;
const MIX: usize = 10;

const HITS: usize = 0;
const LEVEL: usize = 1;
const TEMPO: usize = 2;

const MAX_LOOKAHEAD_MS: f32 = 10.0;
/// The sidechain follower: quick to catch a hit, slow enough to ride a
/// note's body rather than its waveform.
const FOLLOW_ATTACK_MS: f32 = 0.5;
const FOLLOW_RELEASE_MS: f32 = 40.0;
/// After a hit, the sidechain must fall this far under the threshold before
/// another one counts, so one kick with a bumpy tail is one hit.
const REARM_DB: f32 = 4.0;
/// In Level mode, how far over the threshold ducks all the way.
const LEVEL_SPAN_DB: f32 = 12.0;
/// The last smoothing on the gain, so a zero attack doesn't click.
const DECLICK_MS: f32 = 0.3;
/// How long depth, mix and the shape take to reach a new value.
const SMOOTH_MS: f32 = 20.0;
/// The shape's parameters in [`Ducker::shape`]'s order.
const SHAPE: [usize; 4] = [ATTACK, HOLD, RELEASE, CURVE];

/// How far through the release (0 to 1) the gain has come back at `t`
/// (0 to 1): `curve` above 0 stays down longer and comes back late, the
/// classic pump; below 0 comes back early.
fn release_shape(t: f32, curve: f32) -> f32 {
    t.clamp(0.0, 1.0)
        .powf((curve.clamp(-1.0, 1.0) * 2.5).exp2())
}

/// The duck shape: the gain (0 to 1) `ms` after a hit that started from
/// full gain, at these settings. For drawing it, too.
pub fn duck_gain(values: &[f32], ms: f32) -> f32 {
    let get = |i: usize| {
        values
            .get(i)
            .copied()
            .filter(|v| v.is_finite())
            .unwrap_or(PARAMS[i].default_value())
            .clamp(PARAMS[i].clamp(f32::MIN), PARAMS[i].clamp(f32::MAX))
    };
    shape_gain(
        1.0,
        db_to_gain(-get(DEPTH)),
        get(ATTACK),
        get(HOLD),
        get(RELEASE),
        get(CURVE),
        ms,
    )
}

/// The duck from `from` down to `low` and back, `ms` after it started.
fn shape_gain(
    from: f32,
    low: f32,
    attack: f32,
    hold: f32,
    release: f32,
    curve: f32,
    ms: f32,
) -> f32 {
    if ms < attack {
        from + (low - from) * (ms / attack)
    } else if ms < attack + hold {
        low
    } else if ms < attack + hold + release {
        let t = (ms - attack - hold) / release;
        low + (1.0 - low) * release_shape(t, curve)
    } else {
        1.0
    }
}

/// The whole shape's length in ms, at these settings.
pub fn duck_length(values: &[f32]) -> f32 {
    let get = |i: usize| values.get(i).copied().unwrap_or(PARAMS[i].default_value());
    (get(ATTACK) + get(HOLD) + get(RELEASE)).max(1.0)
}

pub struct Ducker {
    values: Values<COUNT>,
    sample_rate: f32,
    depth: Smoothed,
    mix: Smoothed,
    /// The duck's attack, hold and release (ms) and curve, gliding.
    shape: [Smoothed; 4],
    /// This track's own sound, delayed by the lookahead.
    lines: [Ring; 2],
    lookahead: Crossfade,
    /// The sidechain's two-pole low-pass and level follower.
    lowpass: [f32; 2],
    level: f32,
    armed: bool,
    /// Samples since the last hit (Hits mode), and the gain it started from.
    since: f32,
    from: f32,
    /// Level mode's smoothed reduction, in dB.
    reduction: f32,
    /// Tempo mode's own clock while the song is stopped, in quarter notes.
    free_beat: f64,
    gain: f32,
    /// What the meters show.
    shown_level: f32,
}

impl Ducker {
    pub fn new() -> Ducker {
        Ducker {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            depth: Smoothed::new(PARAMS[DEPTH].default_value(), SMOOTH_MS),
            mix: Smoothed::new(1.0, SMOOTH_MS),
            shape: SHAPE.map(|index| Smoothed::new(PARAMS[index].default_value(), SMOOTH_MS)),
            lines: [Ring::default(), Ring::default()],
            lookahead: Crossfade::new(0),
            lowpass: [0.0; 2],
            level: 0.0,
            armed: true,
            since: f32::MAX,
            from: 1.0,
            reduction: 0.0,
            free_beat: 0.0,
            gain: 1.0,
            shown_level: 0.0,
        }
    }

    fn ms_to_samples(&self, ms: f32) -> f32 {
        ms * self.sample_rate / 1000.0
    }

    fn lookahead_samples(&self) -> usize {
        self.ms_to_samples(self.values[LOOKAHEAD]).round() as usize
    }
}

impl Default for Ducker {
    fn default() -> Self {
        Ducker::new()
    }
}

fn coefficient(ms: f32, sample_rate: f32) -> f32 {
    1.0 - (-1000.0 / (ms.max(0.01) * sample_rate)).exp()
}

impl Effect for Ducker {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate.max(1.0);
        let len = (MAX_LOOKAHEAD_MS / 1000.0 * self.sample_rate).ceil() as usize + 2;
        for line in &mut self.lines {
            line.allocate(len);
        }
        self.depth.prepare(self.sample_rate);
        self.mix.prepare(self.sample_rate);
        for smoothed in &mut self.shape {
            smoothed.prepare(self.sample_rate);
        }
        self.lookahead
            .set_fade_length((0.005 * self.sample_rate) as usize);
        self.lookahead.jump(self.lookahead_samples());
        self.reset();
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
        self.lowpass = [0.0; 2];
        self.level = 0.0;
        self.armed = true;
        self.since = f32::MAX;
        self.from = 1.0;
        self.reduction = 0.0;
        self.gain = 1.0;
        self.shown_level = 0.0;
        self.free_beat = 0.0;
        self.depth.set_immediate(self.values[DEPTH]);
        self.mix.set_immediate(self.values[MIX] / 100.0);
        for (smoothed, index) in self.shape.iter_mut().zip(SHAPE) {
            smoothed.set_immediate(self.values[index]);
        }
        self.lookahead.jump(self.lookahead_samples());
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: &Context) {
        self.values.played();
        let mode = self.values[MODE] as usize;
        let sr = self.sample_rate;
        let threshold = db_to_gain(self.values[THRESHOLD]);
        let rearm = db_to_gain(self.values[THRESHOLD] - REARM_DB);
        // Level mode's speeds, from where the shape's times are now.
        let (attack, release) = (self.shape[0].current(), self.shape[2].current());
        let lowpass = (self.values[SC_LOWPASS] < 19_000.0).then(|| {
            coefficient(
                1000.0 / (std::f32::consts::TAU * self.values[SC_LOWPASS]),
                sr,
            )
        });
        let (follow_up, follow_down) = (
            coefficient(FOLLOW_ATTACK_MS, sr),
            coefficient(FOLLOW_RELEASE_MS, sr),
        );
        let (level_down, level_up) = (coefficient(attack.max(0.1), sr), coefficient(release, sr));
        let declick = coefficient(DECLICK_MS, sr);
        let ms_per_sample = 1000.0 / sr;
        let bpm = if context.bpm.is_finite() && context.bpm > 1.0 {
            context.bpm.min(999.0)
        } else {
            120.0
        };
        let rate_beats = RATES[(self.values[RATE] as usize).min(RATES.len() - 1)].1;
        let beats_per_sample = bpm / 60.0 / f64::from(sr);
        let sidechain = context.sidechain;
        let mut peak_level = 0.0f32;

        for i in 0..left.len() {
            let depth = self.depth.tick();
            let mix = self.mix.tick();
            let [attack, hold, release, curve] = self.shape.each_mut().map(Smoothed::tick);
            let low = db_to_gain(-depth);

            // What the sidechain is doing.
            let mut level = self.level;
            if let Some((sl, sr_)) = sidechain {
                let mut x =
                    0.5 * (sl.get(i).copied().unwrap_or(0.0) + sr_.get(i).copied().unwrap_or(0.0));
                if !x.is_finite() {
                    x = 0.0;
                }
                if let Some(a) = lowpass {
                    self.lowpass[0] = flush(self.lowpass[0] + a * (x - self.lowpass[0]));
                    self.lowpass[1] =
                        flush(self.lowpass[1] + a * (self.lowpass[0] - self.lowpass[1]));
                    x = self.lowpass[1];
                }
                let rectified = x.abs();
                let a = if rectified > level {
                    follow_up
                } else {
                    follow_down
                };
                level = flush(level + a * (rectified - level));
            } else {
                level = 0.0;
            }
            self.level = level;
            peak_level = peak_level.max(level);

            let target = match mode {
                HITS => {
                    if sidechain.is_some() && self.armed && level > threshold {
                        self.armed = false;
                        self.from = self.gain;
                        self.since = 0.0;
                    } else if level < rearm {
                        self.armed = true;
                    }
                    let ms = self.since * ms_per_sample;
                    if self.since < f32::MAX {
                        self.since += 1.0;
                    }
                    shape_gain(self.from, low, attack, hold, release, curve, ms)
                }
                LEVEL => {
                    let over = gain_to_db(level.max(1e-9)) - self.values[THRESHOLD];
                    let wanted = depth * (over / LEVEL_SPAN_DB).clamp(0.0, 1.0);
                    let a = if wanted > self.reduction {
                        level_down
                    } else {
                        level_up
                    };
                    self.reduction += a * (wanted - self.reduction);
                    db_to_gain(-self.reduction)
                }
                _ => {
                    // Locked to the song while it plays; on its own clock
                    // while it doesn't.
                    let beat = if context.playing && context.beat.is_finite() {
                        context.beat + i as f64 * beats_per_sample
                    } else {
                        self.free_beat += beats_per_sample;
                        self.free_beat
                    };
                    let into = (beat / rate_beats).rem_euclid(1.0) * rate_beats;
                    let ms = (into * 60_000.0 / bpm) as f32;
                    self.since = ms / ms_per_sample;
                    shape_gain(1.0, low, attack, hold, release, curve, ms)
                }
            };
            self.gain = flush(self.gain + declick * (target - self.gain)).clamp(0.0, 1.0);
            let gain = 1.0 - mix * (1.0 - self.gain);

            let taps = self.lookahead.advance();
            for (line, sample) in self.lines.iter_mut().zip([&mut left[i], &mut right[i]]) {
                let x = if sample.is_finite() { *sample } else { 0.0 };
                line.push(x);
                *sample = taps.read(line) * gain;
            }
        }
        if mode == TEMPO && !context.playing {
            self.free_beat = self.free_beat.rem_euclid(1024.0);
        }
        self.shown_level = peak_level;
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn receive(&mut self, index: usize, value: f32, pace: Pace) {
        let Some(param) = PARAMS.get(index) else {
            return;
        };
        // Anything but a number puts the parameter back to its default.
        let value = if value.is_finite() {
            value
        } else {
            param.default_value()
        };
        self.values.set(index, value);
        let (value, fresh) = (self.values[index], self.values.is_fresh());
        match index {
            DEPTH if fresh => self.depth.set_immediate(value),
            DEPTH => self.depth.move_to(value, pace),
            MIX if fresh => self.mix.set_immediate(value / 100.0),
            MIX => self.mix.move_to(value / 100.0, pace),
            ATTACK | HOLD | RELEASE | CURVE => {
                let slot = SHAPE.iter().position(|&i| i == index).unwrap_or(0);
                if fresh {
                    self.shape[slot].set_immediate(value);
                } else {
                    self.shape[slot].move_to(value, pace);
                }
            }
            LOOKAHEAD => {
                let age = self.lookahead_samples();
                if fresh {
                    self.lookahead.jump(age);
                } else {
                    self.lookahead.set_target(age);
                }
            }
            _ => {}
        }
    }

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out[0] = (-gain_to_db(self.gain.max(1e-6))).max(0.0);
        out[1] = gain_to_db(self.shown_level.max(1e-5));
        out[2] = (self.since * 1000.0 / self.sample_rate).min(10_000.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults;

    const RATE_HZ: f32 = 48_000.0;

    fn ducker(params: &[(usize, f32)]) -> Ducker {
        let mut d = Ducker::new();
        d.prepare(RATE_HZ, 512);
        for (i, v) in params {
            d.set_param(*i, *v);
        }
        d.reset();
        d
    }

    /// A steady tone, ducked by a sidechain of short clicks at `hits`
    /// (samples); returns the left output.
    fn run(d: &mut Ducker, len: usize, hits: &[usize], context: Context) -> Vec<f32> {
        let mut out = Vec::with_capacity(len);
        let mut side = vec![0.0f32; len];
        for &h in hits {
            for s in side.iter_mut().skip(h).take(480) {
                *s = 0.8;
            }
        }
        for start in (0..len).step_by(64) {
            let end = (start + 64).min(len);
            let mut l = vec![0.5f32; end - start];
            let mut r = l.clone();
            let block = Context {
                sidechain: Some((&side[start..end], &side[start..end])),
                ..context
            };
            d.process(&mut l, &mut r, &block);
            out.extend_from_slice(&l);
        }
        out
    }

    #[test]
    fn a_hit_ducks_by_the_depth_and_comes_back() {
        let mut d = ducker(&[(DEPTH, 12.0), (ATTACK, 1.0), (HOLD, 20.0), (RELEASE, 100.0)]);
        let out = run(&mut d, 24_000, &[4800], Context::DEFAULT);
        assert!(
            (out[4000] - 0.5).abs() < 1e-3,
            "untouched before: {}",
            out[4000]
        );
        let bottom = out[4800 + 480];
        let expected = 0.5 * db_to_gain(-12.0);
        assert!((bottom - expected).abs() < 0.01, "{bottom} vs {expected}");
        assert!(
            (out[4800 + 48 * 200] - 0.5).abs() < 1e-3,
            "back after the release"
        );
    }

    #[test]
    fn every_hit_ducks_the_same_however_loud() {
        let mut d = ducker(&[(DEPTH, 18.0), (THRESHOLD, -30.0)]);
        let quiet_side: Vec<f32> = vec![0.05; 64];
        let loud_side: Vec<f32> = vec![0.9; 64];
        let mut lowest = Vec::new();
        for side in [&quiet_side, &loud_side] {
            d.reset();
            let mut low = 1.0f32;
            for _ in 0..40 {
                let (mut l, mut r) = (vec![1.0; 64], vec![1.0; 64]);
                let c = Context {
                    sidechain: Some((side, side)),
                    ..Context::DEFAULT
                };
                d.process(&mut l, &mut r, &c);
                low = l.iter().copied().fold(low, f32::min);
            }
            lowest.push(low);
        }
        assert!((lowest[0] - lowest[1]).abs() < 0.01, "{lowest:?}");
    }

    #[test]
    fn no_sidechain_no_ducking_except_to_the_tempo() {
        let mut d = ducker(&[]);
        let (mut l, mut r) = (vec![0.5; 4096], vec![0.5; 4096]);
        d.process(&mut l, &mut r, &Context::DEFAULT);
        assert!(l.iter().all(|s| (*s - 0.5).abs() < 1e-6));

        // Tempo mode at 120 BPM, quarter notes: ducked at each beat.
        let mut d = ducker(&[(MODE, TEMPO as f32), (DEPTH, 24.0), (HOLD, 30.0)]);
        let beat = 24_000;
        let mut out = Vec::new();
        for block in 0..(beat * 2 / 64) {
            let (mut l, mut r) = (vec![1.0; 64], vec![1.0; 64]);
            let c = Context {
                playing: true,
                beat: (block * 64) as f64 / beat as f64,
                ..Context::DEFAULT
            };
            d.process(&mut l, &mut r, &c);
            out.extend_from_slice(&l);
        }
        let ducked = db_to_gain(-24.0);
        assert!(
            out[beat + 480] < ducked * 1.1,
            "down on beat 2: {}",
            out[beat + 480]
        );
        assert!(out[beat - 100] > 0.95, "up before it: {}", out[beat - 100]);
    }

    #[test]
    fn level_mode_follows_how_loud_the_sidechain_is() {
        let mut d = ducker(&[(MODE, LEVEL as f32), (THRESHOLD, -30.0), (DEPTH, 24.0)]);
        let level_for = |d: &mut Ducker, side: f32| {
            let s = vec![side; 64];
            let mut last = 1.0;
            for _ in 0..200 {
                let (mut l, mut r) = (vec![1.0; 64], vec![1.0; 64]);
                let c = Context {
                    sidechain: Some((&s, &s)),
                    ..Context::DEFAULT
                };
                d.process(&mut l, &mut r, &c);
                last = l[63];
            }
            last
        };
        let half = level_for(&mut d, db_to_gain(-24.0));
        let full = level_for(&mut d, db_to_gain(-6.0));
        assert!(
            (gain_to_db(half) + 12.0).abs() < 1.0,
            "6 dB over: {}",
            gain_to_db(half)
        );
        assert!(
            (gain_to_db(full) + 24.0).abs() < 1.0,
            "fully: {}",
            gain_to_db(full)
        );
    }

    #[test]
    fn lookahead_ducks_before_the_hit_arrives() {
        let lookahead = 5.0;
        let mut d = ducker(&[(LOOKAHEAD, lookahead), (ATTACK, 0.0), (DEPTH, 40.0)]);
        // The sidechain hits where this track's own transient is.
        let out = run(&mut d, 9600, &[4800], Context::DEFAULT);
        let delay = (lookahead / 1000.0 * RATE_HZ) as usize;
        assert!(out[4800 + delay + 10] < 0.05, "already down when it plays");
    }

    #[test]
    fn the_drawn_shape_matches() {
        let values = defaults::<COUNT>(PARAMS);
        assert_eq!(duck_gain(&values, 0.0), 1.0);
        let low = db_to_gain(-values[DEPTH]);
        assert!((duck_gain(&values, values[ATTACK] + 1.0) - low).abs() < 1e-6);
        assert_eq!(duck_gain(&values, duck_length(&values) + 1.0), 1.0);
        // Positive curve: still mostly down halfway through the release.
        let halfway = values[ATTACK] + values[HOLD] + values[RELEASE] / 2.0;
        assert!(duck_gain(&values, halfway) < (1.0 + low) / 2.0);
    }
}
