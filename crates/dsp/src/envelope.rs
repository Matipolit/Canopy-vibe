/// Largest curvature. `curve = ±1` maps to this many time constants across the
/// segment, which is already a very sharp bend; beyond it the shape barely changes.
const MAX_CURVATURE: f32 = 8.0;

/// Below this curvature the exponential is indistinguishable from a line, and
/// its normalisation term would divide by almost zero.
const LINEAR_THRESHOLD: f32 = 1e-3;

/// Where a stage with this `curve` is, `0..=1`, a fraction `t` of the way
/// through it. The same shape [`Adsr`] plays, for drawing it.
pub fn stage_shape(t: f32, curve: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let k = -curve.clamp(-1.0, 1.0) * MAX_CURVATURE;
    if k.abs() < LINEAR_THRESHOLD {
        t
    } else {
        (1.0 - (-k * t).exp()) / (1.0 - (-k).exp())
    }
}

/// A stage's `curve` as `(k, norm)`: its shape a fraction `t` through is
/// `(1 - e^(-k·t))·norm`, reaching exactly 1 at `t = 1`, or `t` itself when
/// `k` is 0 (a curve too slight to tell from a line).
pub fn curve_coefficients(curve: f32) -> (f32, f32) {
    let k = -curve.clamp(-1.0, 1.0) * MAX_CURVATURE;
    if k.abs() < LINEAR_THRESHOLD {
        (0.0, 1.0)
    } else {
        (k, 1.0 / (1.0 - (-k).exp()))
    }
}

/// How long one envelope stage takes and how it bends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StageDesc {
    pub time_ms: f32,
    /// Shape of the stage, `-1..=1`. `0` is a straight line. Negative starts
    /// fast and flattens out. Positive starts slow and accelerates.
    pub curve: f32,
}

impl StageDesc {
    pub const fn new(time_ms: f32, curve: f32) -> Self {
        Self { time_ms, curve }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdsrDesc {
    pub attack: StageDesc,
    pub decay: StageDesc,
    /// Level held while the gate stays on, `0..=1`.
    pub sustain: f32,
    pub release: StageDesc,
}

impl Default for AdsrDesc {
    /// A snappy, analog-style pluck: fast-start curves on every stage.
    fn default() -> Self {
        Self {
            attack: StageDesc::new(5.0, -0.5),
            decay: StageDesc::new(200.0, -0.7),
            sustain: 0.7,
            release: StageDesc::new(300.0, -0.7),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdsrStage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// One timed stage of an envelope: from `start` to `end` over `length`
/// samples, bent by an exponential that is advanced with a single multiply
/// per sample (or one `powi` when skipped ahead), and stretched while it
/// runs if its time is modulated.
#[derive(Debug, Clone, Copy)]
pub struct Segment {
    start: f32,
    end: f32,
    position: u32,
    length: u32,
    /// `exp(-k·t)` at the current position, stepped by `mult` per sample.
    e: f32,
    mult: f32,
    /// `1 / (1 - exp(-k))`, so the shape reaches exactly 1 at the end.
    norm: f32,
    /// The curvature, 0 for a straight line.
    k: f32,
}

impl Segment {
    /// Finished, at silence.
    pub const IDLE: Segment = Segment {
        start: 0.0,
        end: 0.0,
        position: 0,
        length: 1,
        e: 1.0,
        mult: 1.0,
        norm: 1.0,
        k: 0.0,
    };

    /// A stage from `start` to `end` lasting `ms` at `sample_rate`, bent by
    /// `curve` (see [`StageDesc::curve`]).
    pub fn new(start: f32, end: f32, ms: f32, curve: f32, sample_rate: f32) -> Segment {
        let length = (ms * sample_rate / 1000.0).round().max(1.0) as u32;
        let (k, norm) = curve_coefficients(curve);
        Segment {
            start,
            end,
            position: 0,
            length,
            e: 1.0,
            mult: if k == 0.0 {
                1.0
            } else {
                (-k / length as f32).exp()
            },
            norm,
            k,
        }
    }

    /// Makes the stage `factor` times as long, keeping how far through it
    /// is (so the level carries on from where it is).
    pub fn stretch(&mut self, factor: f32) {
        if self.finished() || !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let progress = self.progress();
        let length = (self.length as f32 * factor)
            .round()
            .clamp(1.0, u32::MAX as f32 / 2.0) as u32;
        self.length = length;
        self.position = ((progress * length as f32).round() as u32).min(length - 1);
        if self.k != 0.0 {
            self.mult = (-self.k / length as f32).exp();
            self.e = (-self.k * self.position as f32 / length as f32).exp();
        }
    }

    /// Moves one sample on, which must not be past the end, and returns the
    /// level there.
    #[inline]
    pub fn tick(&mut self) -> f32 {
        self.position += 1;
        let shape = if self.k == 0.0 {
            self.position as f32 / self.length as f32
        } else {
            self.e *= self.mult;
            (1.0 - self.e) * self.norm
        };
        self.level(shape)
    }

    /// Moves `n` samples on (at most to the end) and returns the level there.
    #[inline]
    pub fn step(&mut self, n: u32) -> f32 {
        let n = n.min(self.length - self.position);
        self.position += n;
        let shape = if self.k == 0.0 {
            self.position as f32 / self.length as f32
        } else {
            self.e *= if n == 1 {
                self.mult
            } else {
                self.mult.powi(n as i32)
            };
            (1.0 - self.e) * self.norm
        };
        self.level(shape)
    }

    /// The level `shape` of the way through; exactly `end` once there, as
    /// rounding in the exponential could leave it a little off.
    #[inline]
    fn level(&self, shape: f32) -> f32 {
        if self.finished() {
            self.end
        } else {
            self.start + (self.end - self.start) * shape
        }
    }

    pub fn finished(&self) -> bool {
        self.position >= self.length
    }

    /// Samples into the stage.
    pub fn position(&self) -> u32 {
        self.position
    }

    /// The stage's length in samples.
    pub fn length(&self) -> u32 {
        self.length
    }

    /// How far through the stage, `0..=1`.
    pub fn progress(&self) -> f32 {
        self.position as f32 / self.length as f32
    }
}

/// Attack-decay-sustain-release envelope with a curve control per stage.
///
/// Every stage is a segment from the level the envelope is at to a target
/// level, so retriggering during the release restarts the attack from where
/// the level is instead of jumping to zero.
///
/// Changes to the settings take effect when the next stage starts; the
/// sustain level is read live.
#[derive(Debug, Clone)]
pub struct Adsr {
    description: AdsrDesc,
    sample_rate: f32,
    current_stage: AdsrStage,
    current_level: f32,
    current_segment: Segment,
}

impl Adsr {
    pub fn new(description: AdsrDesc, sample_rate: f32) -> Self {
        Self {
            description,
            sample_rate,
            current_stage: AdsrStage::Idle,
            current_level: 0.0,
            current_segment: Segment::IDLE,
        }
    }

    /// Go silent immediately. Not a release.
    pub fn reset(&mut self) {
        self.current_stage = AdsrStage::Idle;
        self.current_level = 0.0;
        self.current_segment = Segment::IDLE;
    }

    pub fn desc(&self) -> &AdsrDesc {
        &self.description
    }

    pub fn desc_mut(&mut self) -> &mut AdsrDesc {
        &mut self.description
    }

    pub fn set_desc(&mut self, desc: AdsrDesc) {
        self.description = desc;
    }

    pub fn start_attack(&mut self) {
        self.enter(AdsrStage::Attack, 1.0, self.description.attack);
    }

    pub fn start_release(&mut self) {
        if self.current_stage != AdsrStage::Idle {
            self.enter(AdsrStage::Release, 0.0, self.description.release);
        }
    }

    pub fn is_active(&self) -> bool {
        self.current_stage != AdsrStage::Idle
    }

    pub fn level(&self) -> f32 {
        self.current_level
    }

    /// Advance one sample and return the new level.
    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        match self.current_stage {
            AdsrStage::Idle => 0.0,
            AdsrStage::Sustain => {
                self.current_level = self.description.sustain.clamp(0.0, 1.0);
                self.current_level
            }
            AdsrStage::Attack | AdsrStage::Decay | AdsrStage::Release => {
                self.current_level = self.current_segment.tick();
                if self.current_segment.finished() {
                    self.advance();
                }
                self.current_level
            }
        }
    }

    fn advance(&mut self) {
        match self.current_stage {
            AdsrStage::Attack => {
                let sustain = self.description.sustain.clamp(0.0, 1.0);
                self.enter(AdsrStage::Decay, sustain, self.description.decay);
            }
            AdsrStage::Decay => self.current_stage = AdsrStage::Sustain,
            AdsrStage::Release => self.reset(),
            AdsrStage::Idle | AdsrStage::Sustain => {}
        }
    }

    /// Begin `stage`, travelling from the current level to `end`.
    fn enter(&mut self, stage: AdsrStage, end: f32, desc: StageDesc) {
        self.current_stage = stage;
        self.current_segment = Segment::new(
            self.current_level,
            end,
            desc.time_ms,
            desc.curve,
            self.sample_rate,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // for simplicity
    const SAMPLE_RATE: f32 = 1000.0;

    fn envelope(desc: AdsrDesc) -> Adsr {
        Adsr::new(desc, SAMPLE_RATE)
    }

    fn linear_desc() -> AdsrDesc {
        AdsrDesc {
            attack: StageDesc::new(10.0, 0.0),
            decay: StageDesc::new(20.0, 0.0),
            sustain: 0.5,
            release: StageDesc::new(40.0, 0.0),
        }
    }

    fn run(env: &mut Adsr, samples: usize) -> Vec<f32> {
        (0..samples).map(|_| env.next_sample()).collect()
    }

    #[test]
    fn idle_until_gated() {
        let mut env = envelope(linear_desc());
        assert!(!env.is_active());
        assert!(run(&mut env, 16).iter().all(|&s| s == 0.0));
        env.start_release();
        assert!(
            !env.is_active(),
            "gate off while idle must not start a release"
        );
    }

    #[test]
    fn linear_attack_is_a_straight_line_that_lands_on_full() {
        let mut env = envelope(linear_desc());
        env.start_attack();
        let out = run(&mut env, 10);
        for (i, &level) in out.iter().enumerate() {
            let expected = (i + 1) as f32 / 10.0;
            assert!((level - expected).abs() < 1e-6, "sample {i}: {level}");
        }
        assert_eq!(out[9], 1.0);
    }

    #[test]
    fn decays_to_sustain_and_holds() {
        let mut env = envelope(linear_desc());
        env.start_attack();
        run(&mut env, 10);
        let decay = run(&mut env, 20);
        assert!(decay.windows(2).all(|w| w[1] < w[0]), "decay should fall");
        assert_eq!(decay[19], 0.5);
        assert!(run(&mut env, 100).iter().all(|&s| s == 0.5));
        assert!(env.is_active());
    }

    #[test]
    fn sustain_changes_apply_while_held() {
        let mut env = envelope(linear_desc());
        env.start_attack();
        run(&mut env, 30);
        env.desc_mut().sustain = 0.25;
        assert_eq!(env.next_sample(), 0.25);
    }

    #[test]
    fn release_reaches_silence_then_idle() {
        let mut env = envelope(linear_desc());
        env.start_attack();
        run(&mut env, 30);
        env.start_release();
        let release = run(&mut env, 40);
        assert!(
            release.windows(2).all(|w| w[1] < w[0]),
            "release should fall"
        );
        assert_eq!(release[39], 0.0);
        assert!(!env.is_active());
        assert_eq!(env.next_sample(), 0.0);
    }

    #[test]
    fn release_from_mid_attack_starts_at_current_level() {
        let mut env = envelope(linear_desc());
        env.start_attack();
        run(&mut env, 5);
        let before = env.level();
        env.start_release();
        let after = env.next_sample();
        assert!(
            after < before && after > before - 0.1,
            "{before} -> {after}"
        );
    }

    #[test]
    fn retrigger_during_release_starts_from_current_level() {
        let mut env = envelope(linear_desc());
        env.start_attack();
        run(&mut env, 30);
        env.start_release();
        run(&mut env, 20);
        let before = env.level();
        assert!(before > 0.0);
        env.start_attack();
        let after = env.next_sample();
        assert!(
            after > before && after < before + 0.1,
            "{before} -> {after}"
        );
        assert_eq!(
            run(&mut env, 9)[8],
            1.0,
            "a full attack time later it is at full level"
        );
    }

    #[test]
    fn reset_silences_immediately() {
        let mut env = envelope(linear_desc());
        env.start_attack();
        run(&mut env, 30);
        env.reset();
        assert!(!env.is_active());
        assert_eq!(env.level(), 0.0);
        assert_eq!(env.next_sample(), 0.0);
    }

    fn attack_curve(curve: f32) -> Vec<f32> {
        let mut env = envelope(AdsrDesc {
            attack: StageDesc::new(100.0, curve),
            ..linear_desc()
        });
        env.start_attack();
        run(&mut env, 100)
    }

    #[test]
    fn negative_curve_starts_fast_and_positive_starts_slow() {
        let fast = attack_curve(-1.0);
        let line = attack_curve(0.0);
        let slow = attack_curve(1.0);
        assert!(
            fast[49] > 0.9,
            "fast-start is nearly there halfway: {}",
            fast[49]
        );
        assert!((line[49] - 0.5).abs() < 1e-5);
        assert!(
            slow[49] < 0.1,
            "slow-start has barely moved halfway: {}",
            slow[49]
        );
    }

    #[test]
    fn every_curve_rises_monotonically_and_lands_exactly() {
        for curve in [-1.0, -0.5, -0.01, 0.0, 0.01, 0.5, 1.0] {
            let out = attack_curve(curve);
            assert!(
                out.windows(2).all(|w| w[1] > w[0]),
                "curve {curve} should rise monotonically"
            );
            assert_eq!(out[99], 1.0, "curve {curve}");
            assert!(out[98] < 1.0, "curve {curve} reached full too early");
        }
    }

    #[test]
    fn curves_apply_to_decay_and_release_too() {
        let mut env = envelope(AdsrDesc {
            attack: StageDesc::new(1.0, 0.0),
            decay: StageDesc::new(100.0, -1.0),
            sustain: 0.0,
            release: StageDesc::new(100.0, 1.0),
        });
        env.start_attack();
        run(&mut env, 1);
        let decay = run(&mut env, 100);
        assert!(
            decay[49] < 0.1,
            "fast-start decay drops quickly: {}",
            decay[49]
        );
        assert_eq!(decay[99], 0.0);

        env.desc_mut().sustain = 1.0;
        env.start_attack();
        run(&mut env, 101);
        env.start_release();
        let release = run(&mut env, 100);
        assert!(
            release[49] > 0.9,
            "slow-start release lingers: {}",
            release[49]
        );
        assert_eq!(release[99], 0.0);
    }

    #[test]
    fn stage_shape_matches_what_plays() {
        for curve in [-1.0, -0.3, 0.0, 0.6, 1.0] {
            let played = attack_curve(curve);
            for i in [9usize, 49, 89] {
                let t = (i + 1) as f32 / 100.0;
                assert!(
                    (stage_shape(t, curve) - played[i]).abs() < 1e-3,
                    "curve {curve} at {t}"
                );
            }
        }
        assert_eq!(stage_shape(1.0, -0.7), 1.0);
        assert_eq!(stage_shape(0.0, 0.7), 0.0);
    }

    #[test]
    fn zero_length_stages_take_one_sample() {
        let mut env = envelope(AdsrDesc {
            attack: StageDesc::new(0.0, -0.5),
            decay: StageDesc::new(0.0, 0.0),
            sustain: 0.5,
            release: StageDesc::new(0.0, 0.0),
        });
        env.start_attack();
        assert_eq!(env.next_sample(), 1.0);
        assert_eq!(env.next_sample(), 0.5);
        env.start_release();
        assert_eq!(env.next_sample(), 0.0);
        assert!(!env.is_active());
    }
}
