//! The voice envelopes: delay, attack, hold, decay, sustain and release,
//! each timed stage bent by its curve exactly as `dsp::Adsr` bends it.
//!
//! The synth needs more from an envelope than `dsp::Adsr` offers: a delay
//! before the attack and a hold at the top (which the shared envelope
//! parameters have no room for), which stage it is in and how far through
//! (for the UI's playheads), and a cheap way to move a modulation envelope
//! on by a whole control interval at once. So it runs the stages itself,
//! each a [`Segment`] as `Adsr` uses, skipped ahead by a whole interval
//! when it can be.
//!
//! The mod matrix can move the times and the sustain level ([`EnvMods`]):
//! a new time stretches or shrinks what is left of the stage it is in
//! (and every stage after), and the level glides to a new sustain over a
//! millisecond, so modulating either never clicks.

use dsp::{AdsrDesc, Segment};

/// How fast the level follows a modulated sustain: this time constant.
const SUSTAIN_GLIDE_MS: f32 = 1.0;
/// The time knobs' ranges, which modulation keeps the times within (a
/// tempo-synced time already outside stays where it is).
const ATTACK_RANGE_MS: (f32, f32) = (0.5, 10_000.0);
const DECAY_RANGE_MS: (f32, f32) = (1.0, 10_000.0);
const RELEASE_RANGE_MS: (f32, f32) = (1.0, 20_000.0);

/// What the mod matrix does to an envelope at this control tick.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EnvMods {
    /// Octaves added to the attack, decay and release times.
    pub attack: f32,
    pub decay: f32,
    pub release: f32,
    /// Added to the sustain level.
    pub sustain: f32,
}

/// `ms` moved by `octaves`, kept within `range` (or wherever `ms` is, if
/// outside it already).
fn modded(ms: f32, octaves: f32, range: (f32, f32)) -> f32 {
    if octaves == 0.0 {
        ms
    } else {
        (ms * octaves.exp2()).clamp(range.0.min(ms), range.1.max(ms))
    }
}

/// Where an envelope is, as [`Envelope::stage`] reports it. The numbers
/// are what [`Envelope::playhead`] counts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Idle = 0,
    Delay = 1,
    Attack = 2,
    Hold = 3,
    Decay = 4,
    Sustain = 5,
    Release = 6,
}

#[derive(Debug, Clone)]
pub struct Envelope {
    desc: AdsrDesc,
    sample_rate: f32,
    stage: Stage,
    level: f32,
    segment: Segment,
    /// Samples the delay and the hold last, and how far into either.
    delay: u32,
    hold: u32,
    waited: u32,
    mods: EnvMods,
    /// How much of the gap to a moved sustain is left after a sample.
    sustain_keep: f32,
}

impl Envelope {
    pub fn new(desc: AdsrDesc, sample_rate: f32) -> Self {
        Self {
            desc,
            sample_rate,
            stage: Stage::Idle,
            level: 0.0,
            segment: Segment::IDLE,
            delay: 0,
            hold: 0,
            waited: 0,
            mods: EnvMods::default(),
            sustain_keep: (-1000.0 / (SUSTAIN_GLIDE_MS * sample_rate.max(1.0))).exp(),
        }
    }

    /// The mod matrix's say this tick (see [`EnvMods`]): a stage that is
    /// running is stretched to its new time, later stages start with it.
    pub fn set_mods(&mut self, mods: EnvMods) {
        if mods == self.mods {
            return;
        }
        let old = std::mem::replace(&mut self.mods, mods);
        let (stage, range, before, after) = match self.stage {
            Stage::Attack => (self.desc.attack, ATTACK_RANGE_MS, old.attack, mods.attack),
            Stage::Decay => (self.desc.decay, DECAY_RANGE_MS, old.decay, mods.decay),
            Stage::Release => (
                self.desc.release,
                RELEASE_RANGE_MS,
                old.release,
                mods.release,
            ),
            _ => return,
        };
        if before != after {
            let ms = stage.time_ms;
            self.segment
                .stretch(modded(ms, after, range) / modded(ms, before, range));
        }
    }

    /// New settings, which take effect when the next stage starts; the
    /// sustain level is read live.
    pub fn set_desc(&mut self, desc: AdsrDesc) {
        self.desc = desc;
    }

    /// Silent at once, with nothing pending.
    pub fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.level = 0.0;
        self.segment = Segment::IDLE;
        self.waited = 0;
    }

    /// Starts a note: the attack after `delay` samples, then `hold`
    /// samples at full level before the decay. Retriggered, the attack
    /// starts from the level the envelope is at; a note still sounding
    /// releases during the delay rather than holding on.
    pub fn start(&mut self, delay: u32, hold: u32) {
        self.delay = delay;
        self.hold = hold;
        self.waited = 0;
        if delay == 0 {
            self.attack();
        } else {
            self.stage = Stage::Delay;
            // Fades out from where it was over the release, as a key let go.
            self.segment = self.release_segment();
        }
    }

    fn release_segment(&self) -> Segment {
        let release = self.desc.release;
        Segment::new(
            self.level,
            0.0,
            modded(release.time_ms, self.mods.release, RELEASE_RANGE_MS),
            release.curve,
            self.sample_rate,
        )
    }

    fn attack(&mut self) {
        let attack = self.desc.attack;
        self.stage = Stage::Attack;
        self.segment = Segment::new(
            self.level,
            1.0,
            modded(attack.time_ms, self.mods.attack, ATTACK_RANGE_MS),
            attack.curve,
            self.sample_rate,
        );
    }

    fn decay(&mut self) {
        let decay = self.desc.decay;
        self.stage = Stage::Decay;
        self.segment = Segment::new(
            self.level,
            self.sustain(),
            modded(decay.time_ms, self.mods.decay, DECAY_RANGE_MS),
            decay.curve,
            self.sample_rate,
        );
    }

    fn sustain(&self) -> f32 {
        (self.desc.sustain + self.mods.sustain).clamp(0.0, 1.0)
    }

    /// Lets go: the release starts from wherever the level is, and a note
    /// still waiting out its delay never starts.
    pub fn release(&mut self) {
        match self.stage {
            Stage::Idle => {}
            Stage::Delay if self.level <= 0.0 => self.reset(),
            _ => {
                self.stage = Stage::Release;
                self.segment = self.release_segment();
            }
        }
    }

    /// Sounding, or waiting to.
    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    #[cfg(test)]
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// Where the envelope is, for drawing: its [`Stage`] number plus how
    /// far through that stage, `0..1` (0 while sustaining or idle).
    pub fn playhead(&self) -> f32 {
        let progress = match self.stage {
            Stage::Idle | Stage::Sustain => 0.0,
            Stage::Delay => self.waited as f32 / self.delay.max(1) as f32,
            Stage::Hold => self.waited as f32 / self.hold.max(1) as f32,
            _ => self.segment.progress(),
        };
        self.stage as usize as f32 + progress.min(0.999)
    }

    /// Advances one sample and returns the new level.
    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        self.advance(1)
    }

    /// Advances `n` samples and returns the level then. Stepping a whole
    /// control interval at once lands where stepping sample by sample
    /// would, to rounding: modulation envelopes are moved on this way.
    pub fn advance(&mut self, mut n: u32) -> f32 {
        while n > 0 {
            match self.stage {
                Stage::Idle => return 0.0,
                Stage::Sustain => {
                    // Glides to a moved sustain rather than stepping.
                    let target = self.sustain();
                    if self.level != target {
                        let keep = if n == 1 {
                            self.sustain_keep
                        } else {
                            self.sustain_keep.powi(n as i32)
                        };
                        self.level = target + (self.level - target) * keep;
                        if (self.level - target).abs() < 1e-6 {
                            self.level = target;
                        }
                    }
                    return self.level;
                }
                Stage::Delay => {
                    let wait = (self.delay - self.waited).min(n);
                    if !self.segment.finished() {
                        self.level = self.segment.step(wait);
                    } else {
                        self.level = 0.0;
                    }
                    self.waited += wait;
                    n -= wait;
                    if self.waited >= self.delay {
                        self.attack();
                    }
                }
                Stage::Hold => {
                    let wait = (self.hold - self.waited).min(n);
                    self.waited += wait;
                    n -= wait;
                    if self.waited >= self.hold {
                        self.decay();
                    }
                }
                Stage::Attack | Stage::Decay | Stage::Release => {
                    let before = self.segment.position();
                    self.level = self.segment.step(n);
                    n -= self.segment.position() - before;
                    if self.segment.finished() {
                        match self.stage {
                            Stage::Attack if self.hold > 0 => {
                                self.stage = Stage::Hold;
                                self.waited = 0;
                            }
                            Stage::Attack => self.decay(),
                            Stage::Decay => self.stage = Stage::Sustain,
                            _ => {
                                self.reset();
                                return 0.0;
                            }
                        }
                    }
                }
            }
        }
        self.level
    }
}

#[cfg(test)]
mod tests {
    use dsp::{Adsr, StageDesc};

    use super::*;

    fn desc(curve: f32) -> AdsrDesc {
        AdsrDesc {
            attack: StageDesc::new(10.0, curve),
            decay: StageDesc::new(20.0, curve),
            sustain: 0.5,
            release: StageDesc::new(40.0, curve),
        }
    }

    fn linear() -> Envelope {
        Envelope::new(desc(0.0), 1000.0)
    }

    fn run(env: &mut Envelope, samples: usize) -> Vec<f32> {
        (0..samples).map(|_| env.next_sample()).collect()
    }

    #[test]
    fn delay_waits_then_attacks_and_hold_waits_at_the_top() {
        let mut env = linear();
        env.start(5, 7);
        assert!(env.is_active());
        assert_eq!(env.stage(), Stage::Delay);
        let out = run(&mut env, 5 + 10 + 7 + 20);
        assert!(out[..5].iter().all(|v| *v == 0.0), "{out:?}");
        assert_eq!(out[5], 0.1, "the attack's first step comes after the delay");
        assert_eq!(out[14], 1.0);
        assert!(out[15..22].iter().all(|v| *v == 1.0), "held: {out:?}");
        assert!(out[22] < 1.0, "then decays: {}", out[22]);
        assert_eq!(out[41], 0.5);
        assert_eq!(env.stage(), Stage::Sustain);
    }

    #[test]
    fn without_delay_or_hold_it_is_the_plain_adsr() {
        for curve in [0.0, -0.7, 0.4] {
            let mut env = Envelope::new(desc(curve), 1000.0);
            let mut adsr = Adsr::new(desc(curve), 1000.0);
            env.start(0, 0);
            adsr.start_attack();
            for _ in 0..50 {
                assert_eq!(env.next_sample(), adsr.next_sample());
            }
            env.release();
            adsr.start_release();
            for _ in 0..50 {
                assert_eq!(env.next_sample(), adsr.next_sample());
            }
            assert!(!env.is_active() && !adsr.is_active());
        }
    }

    #[test]
    fn skipping_ahead_lands_where_stepping_does() {
        for curve in [0.0, -0.6, 0.8] {
            let mut stepped = Envelope::new(desc(curve), 1000.0);
            let mut skipped = stepped.clone();
            stepped.start(3, 4);
            skipped.start(3, 4);
            for block in 0..12 {
                let mut level = 0.0;
                for _ in 0..7 {
                    level = stepped.next_sample();
                }
                let jumped = skipped.advance(7);
                assert!(
                    (level - jumped).abs() < 1e-4,
                    "block {block}: {level} {jumped}"
                );
                assert_eq!(stepped.stage(), skipped.stage());
                if block == 6 {
                    stepped.release();
                    skipped.release();
                }
            }
        }
    }

    #[test]
    fn the_playhead_walks_through_the_stages() {
        let mut env = linear();
        env.start(4, 2);
        assert_eq!(env.playhead(), 1.0);
        run(&mut env, 2);
        assert_eq!(env.playhead(), 1.5);
        run(&mut env, 2 + 5);
        assert!((env.playhead() - 2.5).abs() < 1e-6, "{}", env.playhead());
        run(&mut env, 5 + 1);
        assert!((env.playhead() - 3.5).abs() < 1e-6, "{}", env.playhead());
        run(&mut env, 1 + 30);
        assert_eq!(env.playhead(), 5.0);
        env.release();
        run(&mut env, 10);
        assert!((env.playhead() - 6.25).abs() < 1e-6, "{}", env.playhead());
    }

    #[test]
    fn releasing_during_the_delay_never_sounds() {
        let mut env = linear();
        env.start(50, 0);
        run(&mut env, 10);
        env.release();
        assert!(!env.is_active());
        assert!(run(&mut env, 100).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn modulated_times_stretch_the_running_stage_and_the_next() {
        // A linear 10-sample attack at half way, then made twice as long:
        // the level carries on from half way, over 10 more samples.
        let mut env = linear();
        env.start(0, 0);
        run(&mut env, 5);
        assert_eq!(env.level(), 0.5);
        env.set_mods(EnvMods {
            attack: 1.0,
            decay: -1.0,
            ..EnvMods::default()
        });
        let out = run(&mut env, 10);
        assert!((out[0] - 0.55).abs() < 1e-6, "{out:?}");
        assert_eq!(out[9], 1.0);
        assert_eq!(env.stage(), Stage::Decay);
        // The decay starts at its modulated time: 10 samples, not 20.
        let out = run(&mut env, 10);
        assert_eq!(out[9], 0.5);
        assert_eq!(env.stage(), Stage::Sustain);
    }

    #[test]
    fn a_modulated_sustain_glides_and_times_stay_in_range() {
        let mut env = Envelope::new(desc(0.0), 48_000.0);
        env.start(0, 0);
        env.advance(48_000);
        assert_eq!(env.level(), 0.5);
        env.set_mods(EnvMods {
            sustain: 0.3,
            ..EnvMods::default()
        });
        let first = env.next_sample();
        assert!(first > 0.5 && first < 0.52, "glides: {first}");
        env.advance(4_800);
        assert!((env.level() - 0.8).abs() < 1e-5, "{}", env.level());
        // Far past the knob's range, the release stays within it: 20 s.
        env.set_mods(EnvMods {
            sustain: 0.3,
            release: 40.0,
            ..EnvMods::default()
        });
        env.release();
        assert!(
            env.segment.length() == 20 * 48_000,
            "{}",
            env.segment.length()
        );
    }

    #[test]
    fn a_retrigger_with_a_delay_releases_first() {
        let mut env = linear();
        env.start(0, 0);
        run(&mut env, 40);
        env.start(20, 0);
        let out = run(&mut env, 30);
        assert!(out[..20].windows(2).all(|w| w[1] < w[0]), "{out:?}");
        assert!(out[25] > out[19], "then attacks: {out:?}");
    }
}
