//! Generated automation: repeating shapes and random values, for the
//! automation lane's Generate window.

use midi::Tick;

use crate::AutomationPoint;

/// What the generated values do over each period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wave {
    #[default]
    Sine,
    Triangle,
    /// Rises over each period, then drops.
    SawUp,
    SawDown,
    Square,
    /// A new random value each period, held.
    RandomSteps,
    /// A new random value each period, gliding from one to the next.
    RandomSmooth,
}

impl Wave {
    pub const ALL: [Wave; 7] = [
        Wave::Sine,
        Wave::Triangle,
        Wave::SawUp,
        Wave::SawDown,
        Wave::Square,
        Wave::RandomSteps,
        Wave::RandomSmooth,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Wave::Sine => "Sine",
            Wave::Triangle => "Triangle",
            Wave::SawUp => "Saw up",
            Wave::SawDown => "Saw down",
            Wave::Square => "Square",
            Wave::RandomSteps => "Random steps",
            Wave::RandomSmooth => "Random smooth",
        }
    }

    pub fn is_random(self) -> bool {
        matches!(self, Wave::RandomSteps | Wave::RandomSmooth)
    }
}

/// How to fill `start..end`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Generate {
    pub wave: Wave,
    pub start: Tick,
    pub end: Tick,
    /// One cycle (or, for random, one value) every `period`.
    pub period: Tick,
    /// The range swept, on the lane's `0..=1` scale. `low` may be above
    /// `high`, which turns the shape upside down.
    pub low: f32,
    pub high: f32,
    /// Where in the cycle it starts, `0..1`.
    pub phase: f32,
    pub seed: u64,
}

/// Points per cycle for a sine; enough to look and sound smooth with the
/// segments' curves.
const SINE_POINTS: usize = 8;
/// Generating never makes more points than this.
const MAX_POINTS: usize = 20_000;

impl Generate {
    /// The points, sorted, all within `start..end`.
    pub fn points(&self) -> Vec<AutomationPoint> {
        let (start, end) = (self.start.get(), self.end.get());
        let period = self.period.get().max(2);
        if end <= start {
            return Vec::new();
        }
        let level = |x: f32| self.low + (self.high - self.low) * x.clamp(0.0, 1.0);
        let point = |tick: i64, x: f32, curve: f32| AutomationPoint {
            tick: Tick::new(tick.clamp(start, end)),
            value: level(x).clamp(0.0, 1.0),
            curve,
        };
        // The cycle position at `tick`, `0..1`.
        let phase_at =
            |tick: i64| ((tick - start) as f64 / period as f64 + self.phase as f64).fract();
        let mut points = Vec::new();
        let mut rng = self.seed.max(1);
        let mut random =
            move || (dsp::noise::xorshift64(&mut rng) >> 40) as f32 / (1u64 << 24) as f32;
        // Cycle boundaries, shifted by the phase so a 25% phase starts a
        // quarter of the way in.
        let shift = (self.phase.rem_euclid(1.0) as f64 * period as f64) as i64;
        let first = start - shift;
        match self.wave {
            Wave::Sine => {
                let step = (period as f64 / SINE_POINTS as f64).max(1.0);
                let mut t = start as f64;
                while t < end as f64 && points.len() < MAX_POINTS {
                    let x = phase_at(t as i64);
                    let s = 0.5 + 0.5 * (std::f64::consts::TAU * x).sin();
                    points.push(point(t.round() as i64, s as f32, 0.0));
                    t += step;
                }
                let x = phase_at(end);
                points.push(point(
                    end,
                    (0.5 + 0.5 * (std::f64::consts::TAU * x).sin()) as f32,
                    0.0,
                ));
            }
            Wave::Triangle | Wave::SawUp | Wave::SawDown | Wave::Square => {
                let shape = |x: f64| -> f32 {
                    match self.wave {
                        Wave::Triangle => (1.0 - (2.0 * x - 1.0).abs()) as f32,
                        Wave::SawUp => x as f32,
                        Wave::SawDown => (1.0 - x) as f32,
                        _ => {
                            if x < 0.5 {
                                1.0
                            } else {
                                0.0
                            }
                        }
                    }
                };
                // The corners of each cycle: where the shape turns or jumps.
                let corners: &[f64] = match self.wave {
                    Wave::Triangle => &[0.0, 0.5],
                    Wave::Square => &[0.0, 0.5],
                    _ => &[0.0],
                };
                let jumps = matches!(self.wave, Wave::SawUp | Wave::SawDown | Wave::Square);
                points.push(point(start, shape(phase_at(start)), 0.0));
                let mut cycle = first;
                while cycle < end && points.len() < MAX_POINTS {
                    for &corner in corners {
                        let tick = cycle + (corner * period as f64) as i64;
                        if tick <= start || tick >= end {
                            continue;
                        }
                        if jumps {
                            // Arrive at the old level a tick early, then jump.
                            points.push(point(tick - 1, shape(phase_at(tick - 1)), 0.0));
                        }
                        points.push(point(tick, shape(corner), 0.0));
                    }
                    cycle += period;
                }
                points.push(point(end, shape(phase_at(end).min(0.999_999)), 0.0));
            }
            Wave::RandomSteps | Wave::RandomSmooth => {
                let mut tick = start;
                let mut value = random();
                while tick < end && points.len() < MAX_POINTS {
                    let next = (tick + period).min(end);
                    points.push(point(tick, value, 0.0));
                    let next_value = random();
                    if self.wave == Wave::RandomSteps && next < end {
                        points.push(point(next - 1, value, 0.0));
                    }
                    if next >= end {
                        let last = if self.wave == Wave::RandomSteps {
                            value
                        } else {
                            next_value
                        };
                        points.push(point(end, last, 0.0));
                    }
                    tick = next;
                    value = next_value;
                }
            }
        }
        points.sort_by_key(|p| p.tick);
        points.dedup_by(|b, a| a.tick == b.tick && a.value == b.value);
        points
    }
}

/// `points` with everything in `start..=end` replaced by `new`.
pub fn replace_range(
    points: &[AutomationPoint],
    start: Tick,
    end: Tick,
    new: Vec<AutomationPoint>,
) -> Vec<AutomationPoint> {
    let mut out: Vec<AutomationPoint> = points
        .iter()
        .filter(|p| p.tick < start || p.tick > end)
        .copied()
        .chain(new)
        .collect();
    out.sort_by_key(|p| p.tick);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AutomationLane;

    fn lane(points: Vec<AutomationPoint>) -> AutomationLane {
        AutomationLane {
            target: crate::AutomationTarget::Volume,
            points: points.into(),
        }
    }

    fn generate(wave: Wave) -> Generate {
        Generate {
            wave,
            start: Tick::new(0),
            end: Tick::new(3840 * 2),
            period: Tick::new(3840),
            low: 0.2,
            high: 0.8,
            phase: 0.0,
            seed: 7,
        }
    }

    #[test]
    fn shapes_follow_their_waves() {
        let at = |g: &Generate, tick: i64| lane(g.points()).value_at(Tick::new(tick)).unwrap();
        let close = |a: f32, b: f32| (a - b).abs() < 0.03;

        let sine = generate(Wave::Sine);
        assert!(close(at(&sine, 0), 0.5));
        assert!(close(at(&sine, 960), 0.8), "peak a quarter in");
        assert!(close(at(&sine, 2880), 0.2), "trough three quarters in");
        assert!(close(at(&sine, 3840 + 960), 0.8), "and again");

        let triangle = generate(Wave::Triangle);
        assert!(close(at(&triangle, 0), 0.2));
        assert!(close(at(&triangle, 1920), 0.8));
        assert!(close(at(&triangle, 960), 0.5));

        let saw = generate(Wave::SawUp);
        assert!(close(at(&saw, 0), 0.2));
        assert!(close(at(&saw, 3838), 0.8), "{}", at(&saw, 3838));
        assert!(close(at(&saw, 3840), 0.2), "drops at the period");

        let square = generate(Wave::Square);
        assert!(close(at(&square, 100), 0.8));
        assert!(close(at(&square, 1918), 0.8));
        assert!(close(at(&square, 2000), 0.2));

        // A quarter of a phase moves the sine's peak to the start.
        let shifted = Generate {
            phase: 0.25,
            ..sine
        };
        assert!(close(at(&shifted, 0), 0.8));
    }

    #[test]
    fn random_values_stay_in_range_and_repeat_with_the_seed() {
        for wave in [Wave::RandomSteps, Wave::RandomSmooth] {
            let g = generate(wave);
            let points = g.points();
            assert!(points.iter().all(|p| (0.2..=0.8).contains(&p.value)));
            assert!(points.iter().all(|p| p.tick >= g.start && p.tick <= g.end));
            assert_eq!(points, g.points(), "same seed, same points");
            assert_ne!(points, Generate { seed: 8, ..g }.points());
        }
        // Steps hold their value across the period.
        let steps = lane(generate(Wave::RandomSteps).points());
        let a = steps.value_at(Tick::new(10)).unwrap();
        assert_eq!(steps.value_at(Tick::new(3800)).unwrap(), a);
    }

    #[test]
    fn huge_requests_are_capped_and_empty_ranges_are_empty() {
        let g = Generate {
            end: Tick::new(i64::MAX / 8),
            period: Tick::new(2),
            ..generate(Wave::Sine)
        };
        assert!(g.points().len() <= MAX_POINTS + 1);
        assert!(
            Generate {
                end: Tick::ZERO,
                ..generate(Wave::Square)
            }
            .points()
            .is_empty()
        );
    }

    #[test]
    fn replacing_keeps_what_is_outside_the_range() {
        let p = |tick: i64, value: f32| AutomationPoint {
            tick: Tick::new(tick),
            value,
            curve: 0.0,
        };
        let old = vec![p(0, 0.1), p(100, 0.2), p(500, 0.3)];
        let new = replace_range(&old, Tick::new(50), Tick::new(200), vec![p(60, 0.9)]);
        assert_eq!(new, vec![p(0, 0.1), p(60, 0.9), p(500, 0.3)]);
    }
}
