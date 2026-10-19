//! Low-frequency oscillators for the mod matrix.
//!
//! LFOs run at the control rate. A free-running LFO keeps one phase for the
//! whole instrument; a voice starting copies it, so every voice moves
//! together. Retriggered and envelope-like LFOs (one-shot, looping and
//! sustaining) restart per note. An LFO switched to mono is one instance
//! for the whole instrument, which the synth runs and every voice reads.
//!
//! Besides the fixed shapes, each LFO has two drawn ones: the custom shape,
//! [`LFO_POINTS`] values evenly spread over the cycle with a glide between
//! them (see [`custom_shape`]), and the Points shape, up to [`MAX_NODES`]
//! nodes anywhere in the cycle with a curve on every segment, as Serum and
//! Vital draw them (see [`Nodes`]). The chaos shapes follow a Lorenz or a
//! Rössler attractor, which never repeats yet moves smoothly (Serum 2's
//! chaos LFOs).

use std::f32::consts::TAU;

use super::params::{LFO_POINTS, MAX_NODES, curve_coefficients};

pub const SHAPE_SINE: usize = 0;
pub const SHAPE_TRIANGLE: usize = 1;
pub const SHAPE_SAW_UP: usize = 2;
pub const SHAPE_SAW_DOWN: usize = 3;
pub const SHAPE_SQUARE: usize = 4;
pub const SHAPE_SAMPLE_HOLD: usize = 5;
pub const SHAPE_SMOOTH_RANDOM: usize = 6;
/// The LFO's own drawn points.
pub const SHAPE_CUSTOM: usize = 7;
/// The LFO's drawn nodes (see [`Nodes`]).
pub const SHAPE_NODES: usize = 8;
/// The x of a Lorenz attractor: two lobes it swings between.
pub const SHAPE_LORENZ: usize = 9;
/// The x of a Rössler attractor: a spiral with the odd wider loop.
pub const SHAPE_ROSSLER: usize = 10;

pub const MODE_FREE: usize = 0;
pub const MODE_RETRIG: usize = 1;
/// Runs one cycle from its start phase and stays at the end.
pub const MODE_ENV: usize = 2;
/// Runs from its start phase to the end, then loops from the loop point to
/// the end while the key is held; once let go, it plays out to the end.
pub const MODE_LOOP: usize = 3;
/// Runs from its start phase to the loop point and waits there while the
/// key is held; once let go, it plays out to the end.
pub const MODE_SUSTAIN: usize = 4;

/// Longest smoothing time constant, at Smooth = 1.
const MAX_SMOOTH_S: f32 = 0.25;

/// Whether `shape` is one of the chaos shapes.
pub const fn is_chaos(shape: usize) -> bool {
    matches!(shape, SHAPE_LORENZ | SHAPE_ROSSLER)
}

/// Whether `mode` repeats its cycle rather than playing it once.
pub const fn repeats(mode: usize) -> bool {
    matches!(mode, MODE_FREE | MODE_RETRIG)
}

/// The value of LFO shape `shape` (see [`super::params::LFO_SHAPES`]) at
/// `phase`, `-1..=1`, for drawing. The periodic shapes repeat every whole
/// phase. The random shapes pick a value per whole phase from a fixed
/// sequence, so draw a few cycles (say phase 0..4) to show them; what plays
/// is fresh random values, shown live by the meters. The drawn and chaos
/// shapes need more than a shape: see `lfo_shape_at`; here they are flat.
pub fn lfo_value(shape: usize, phase: f32) -> f32 {
    if !phase.is_finite() {
        return 0.0;
    }
    let cycle = phase.floor();
    let p = phase - cycle;
    match shape {
        SHAPE_SAMPLE_HOLD => random_value(cycle as i64 as u32),
        SHAPE_SMOOTH_RANDOM => {
            let a = random_value(cycle as i64 as u32);
            let b = random_value((cycle as i64 + 1) as u32);
            a + (b - a) * smoothstep(p)
        }
        SHAPE_CUSTOM | SHAPE_NODES | SHAPE_LORENZ | SHAPE_ROSSLER => 0.0,
        _ => periodic(shape, p),
    }
}

/// The custom shape at `phase` (`0..=1` through the cycle), `0..=1`, between
/// `points` (evenly spread, point 0 at phase 0), with `glide` from 0
/// (steps) to 1 (one smooth curve through every point).
///
/// Each point holds for the first `1 - glide` of its step, then eases to
/// the next (a cubic with flat ends) over the rest. As glide nears 1 the
/// ends take the slopes of a Catmull-Rom spline instead, so at 1 the eases
/// join into one curve. With `wrap` the last point eases back to the first,
/// as a repeating cycle does; without (a one-shot) the shape ends on the
/// last point and stays there.
pub fn custom_shape(points: &[f32; LFO_POINTS], phase: f32, glide: f32, wrap: bool) -> f32 {
    let n = LFO_POINTS as isize;
    let at = |i: isize| {
        let i = if wrap {
            i.rem_euclid(n)
        } else {
            i.clamp(0, n - 1)
        };
        points[i as usize]
    };
    let phase = if !phase.is_finite() {
        0.0
    } else if wrap {
        phase.rem_euclid(1.0)
    } else {
        phase.clamp(0.0, 1.0)
    };
    let position = phase * LFO_POINTS as f32;
    let i = (position as isize).min(n - 1);
    let u = position - i as f32;
    let glide = glide.clamp(0.0, 1.0);
    let hold = 1.0 - glide;
    if u <= hold || glide <= 0.0 {
        return at(i).clamp(0.0, 1.0);
    }
    let w = ((u - hold) / glide).min(1.0);
    let (v0, v1) = (at(i), at(i + 1));
    // Catmull-Rom slopes, per step, scaled into the ease's own span and
    // brought in only as the glide nears 1.
    let slope = glide.powi(5) * 0.5;
    let m0 = slope * (v1 - at(i - 1));
    let m1 = slope * (at(i + 2) - v0);
    let (w2, w3) = (w * w, w * w * w);
    let value = (2.0 * w3 - 3.0 * w2 + 1.0) * v0
        + (w3 - 2.0 * w2 + w) * m0
        + (3.0 * w2 - 2.0 * w3) * v1
        + (w3 - w2) * m1;
    value.clamp(0.0, 1.0)
}

/// A Points shape: nodes in order through the cycle, each with a value and
/// a curve for the segment that leaves it, as Serum's and Vital's LFO
/// graphs have. The first node sits at the start of the cycle and the last
/// at its end; two nodes at the same place make a jump. The curves are
/// worked out once per control tick, so evaluating costs one `exp`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Nodes {
    pub count: usize,
    pub x: [f32; MAX_NODES],
    pub y: [f32; MAX_NODES],
    /// Each segment's curve as [`curve_coefficients`] gives it.
    k: [f32; MAX_NODES],
    norm: [f32; MAX_NODES],
}

impl Default for Nodes {
    fn default() -> Self {
        Nodes::new(&[(0.0, 0.5, 0.0), (1.0, 0.5, 0.0)])
    }
}

impl Nodes {
    /// Nodes from `(x, y, curve)` triples, at least two and at most
    /// [`MAX_NODES`]. Places are kept in order and inside the cycle, the
    /// first at 0 and the last at 1.
    pub fn new(nodes: &[(f32, f32, f32)]) -> Self {
        let count = nodes.len().clamp(2, MAX_NODES);
        let mut out = Nodes {
            count,
            x: [1.0; MAX_NODES],
            y: [0.5; MAX_NODES],
            k: [0.0; MAX_NODES],
            norm: [1.0; MAX_NODES],
        };
        let mut last = 0.0f32;
        for i in 0..count {
            let (x, y, curve) = nodes.get(i).copied().unwrap_or((1.0, 0.5, 0.0));
            let x = if i == 0 {
                0.0
            } else if i == count - 1 {
                1.0
            } else {
                finite_or(x, last).clamp(last, 1.0)
            };
            last = x;
            out.x[i] = x;
            out.y[i] = finite_or(y, 0.5).clamp(0.0, 1.0);
            (out.k[i], out.norm[i]) = curve_coefficients(finite_or(curve, 0.0));
        }
        out
    }

    /// The shape at `phase` (`0..=1`; anything else wraps), `0..=1`.
    pub fn value(&self, phase: f32) -> f32 {
        let p = if !phase.is_finite() {
            0.0
        } else if (0.0..=1.0).contains(&phase) {
            phase
        } else {
            phase.rem_euclid(1.0)
        };
        let last = self.count - 1;
        // The segment holding p: the last node at or before it, skipping
        // jumps (two nodes at one place) to the one after them.
        let mut i = 0;
        while i < last - 1 && self.x[i + 1] <= p {
            i += 1;
        }
        let (x0, x1) = (self.x[i], self.x[i + 1]);
        if x1 <= x0 {
            return self.y[i + 1];
        }
        let t = ((p - x0) / (x1 - x0)).clamp(0.0, 1.0);
        let (k, norm) = (self.k[i], self.norm[i]);
        let shape = if k == 0.0 {
            t
        } else {
            (1.0 - (-k * t).exp()) * norm
        };
        self.y[i] + (self.y[i + 1] - self.y[i]) * shape
    }
}

fn finite_or(x: f32, fallback: f32) -> f32 {
    if x.is_finite() { x } else { fallback }
}

/// One of the periodic shapes at `p` in `0..=1`. Sine and triangle start at
/// zero and rise, like the oscillators.
#[inline]
fn periodic(shape: usize, p: f32) -> f32 {
    match shape {
        SHAPE_SINE => (TAU * p).sin(),
        SHAPE_TRIANGLE => {
            if p < 0.25 {
                4.0 * p
            } else if p < 0.75 {
                2.0 - 4.0 * p
            } else {
                4.0 * p - 4.0
            }
        }
        SHAPE_SAW_UP => 2.0 * p - 1.0,
        SHAPE_SAW_DOWN => 1.0 - 2.0 * p,
        _ => {
            if p < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
    }
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// A repeatable value in `-1..=1` for index `n`.
fn random_value(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B9) ^ 0x85EB_CA6B;
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    (x >> 8) as f32 / (1 << 23) as f32 - 1.0
}

/// A chaotic system's state and how to run it: Lorenz's (σ 10, ρ 28,
/// β 8/3) or Rössler's (a 0.2, b 0.2, c 5.7) equations.
mod chaos {
    use super::SHAPE_LORENZ;

    /// Points on each attractor to start from, so a fresh LFO is already
    /// moving as it will rather than spiralling in from nowhere.
    const LORENZ_START: [f32; 3] = [-5.7, -8.5, 17.6];
    const ROSSLER_START: [f32; 3] = [6.5, -2.0, 0.03];
    /// System time per LFO cycle: one lap of a Lorenz lobe takes about
    /// 0.7, one Rössler orbit about 6.
    const LORENZ_SPEED: f32 = 0.7;
    const ROSSLER_SPEED: f32 = 6.0;
    /// Longest step the midpoint method takes, to stay on the attractor.
    const LORENZ_STEP: f32 = 0.008;
    const ROSSLER_STEP: f32 = 0.05;

    fn derivative(shape: usize, s: [f32; 3]) -> [f32; 3] {
        let [x, y, z] = s;
        if shape == SHAPE_LORENZ {
            [10.0 * (y - x), x * (28.0 - z) - y, x * y - 8.0 / 3.0 * z]
        } else {
            [-y - z, x + 0.2 * y, 0.2 + z * (x - 5.7)]
        }
    }

    pub fn start(shape: usize, jitter: f32) -> [f32; 3] {
        let base = if shape == SHAPE_LORENZ {
            LORENZ_START
        } else {
            ROSSLER_START
        };
        [base[0] + jitter, base[1] - jitter, base[2]]
    }

    /// Runs `state` on by `cycles` LFO cycles.
    pub fn advance(shape: usize, state: &mut [f32; 3], cycles: f32) {
        let (speed, longest) = if shape == SHAPE_LORENZ {
            (LORENZ_SPEED, LORENZ_STEP)
        } else {
            (ROSSLER_SPEED, ROSSLER_STEP)
        };
        let time = (cycles * speed).max(0.0);
        let steps = (time / longest).ceil().clamp(1.0, 16.0);
        let dt = time / steps;
        for _ in 0..steps as usize {
            let d = derivative(shape, *state);
            let mid: [f32; 3] = std::array::from_fn(|i| state[i] + 0.5 * dt * d[i]);
            let d = derivative(shape, mid);
            for i in 0..3 {
                state[i] += dt * d[i];
            }
        }
        // A step too long for a wild setting could leave the attractor;
        // start again rather than run off.
        if !state.iter().all(|v| v.is_finite() && v.abs() < 1000.0) {
            *state = start(shape, 0.0);
        }
    }

    /// The state as two values in `-1..=1`: the output, and a second
    /// coordinate to draw it against.
    pub fn view(shape: usize, s: [f32; 3]) -> (f32, f32) {
        let (x, y) = if shape == SHAPE_LORENZ {
            (s[0] / 20.0, (s[2] - 25.0) / 22.0)
        } else {
            (s[0] / 11.0, s[1] / 11.0)
        };
        (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0))
    }
}

/// `seconds` of LFO shape `shape` (a chaos shape) at one cycle a second,
/// as `(output, second coordinate)` pairs `step` seconds apart, from a
/// fixed start: what the attractor looks like, for drawing.
pub fn chaos_trace(shape: usize, seconds: f32, step: f32) -> Vec<(f32, f32)> {
    let mut state = chaos::start(shape, 0.0);
    let count = (seconds / step.max(1e-3)) as usize;
    (0..count)
        .map(|_| {
            chaos::advance(shape, &mut state, step);
            chaos::view(shape, state)
        })
        .collect()
}

/// An LFO's settings, gathered from the parameters once per control tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LfoSettings {
    pub shape: usize,
    pub mode: usize,
    /// Cycles per second before modulation, already tempo-synced if set to.
    pub rate_hz: f32,
    pub start_phase: f32,
    /// `0..=1`.
    pub smooth: f32,
    /// Seconds to fade in after the delay; 0 for none.
    pub rise: f32,
    /// Seconds after a note before the LFO starts.
    pub delay: f32,
    /// Where the Loop mode loops back to and the Sustain mode waits.
    pub loop_point: f32,
    /// One LFO for the whole instrument.
    pub mono: bool,
    /// The custom shape's points, `0..=1`, and how it glides between them
    /// (see [`custom_shape`]).
    pub points: [f32; LFO_POINTS],
    pub glide: f32,
    /// The Points shape.
    pub nodes: Nodes,
}

/// What the mod matrix does to an LFO at this control tick.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LfoMods {
    /// Cycles added to where the shape is read (Vital's phase modulation):
    /// the clock runs on as before.
    pub phase: f32,
    /// Added to the smoothing, `0..=1`.
    pub smooth: f32,
    /// Seconds added to the rise and the delay.
    pub rise: f32,
    pub delay: f32,
}

impl LfoSettings {
    /// How far faded in the LFO is `age` seconds into a note, `0..=1`:
    /// nothing during the delay, then up over the rise (both as `mods`
    /// move them).
    pub fn rise_gain(&self, age: f32, mods: &LfoMods) -> f32 {
        let age = age - (self.delay + mods.delay).max(0.0);
        let rise = self.rise + mods.rise;
        if age < 0.0 {
            0.0
        } else if rise <= 0.0 {
            1.0
        } else {
            (age / rise).clamp(0.0, 1.0)
        }
    }

    /// Whether the LFO's cycle runs `age` seconds into a note: a free one
    /// always does, the others wait out the delay.
    pub fn running(&self, age: f32, mods: &LfoMods) -> bool {
        self.mode == MODE_FREE || age >= self.delay + mods.delay
    }
}

impl Default for LfoSettings {
    fn default() -> Self {
        Self {
            shape: SHAPE_SINE,
            mode: MODE_FREE,
            rate_hz: 1.0,
            start_phase: 0.0,
            smooth: 0.0,
            rise: 0.0,
            delay: 0.0,
            loop_point: 0.5,
            mono: false,
            points: [0.5; LFO_POINTS],
            glide: 1.0,
            nodes: Nodes::default(),
        }
    }
}

/// One LFO's running state.
#[derive(Debug, Clone, Copy)]
pub struct Lfo {
    /// `0..1`, or exactly 1 once a one-shot has finished.
    pub phase: f32,
    /// The output after smoothing, `-1..=1`.
    pub value: f32,
    /// Xorshift state for the random shapes.
    rng: u32,
    /// Sample-and-hold's value, and where smooth random comes from.
    held: f32,
    /// Where smooth random is heading this cycle.
    next: f32,
    /// A one-shot that reached the end of its cycle.
    done: bool,
    /// The chaos shapes' system.
    chaos: [f32; 3],
    /// Which chaos system `chaos` belongs to, so a change of shape starts
    /// the new one on its own attractor.
    chaos_shape: usize,
    /// The mod matrix's say, set by the voice before each advance.
    pub mods: LfoMods,
}

impl Lfo {
    pub fn new(seed: u32) -> Self {
        let mut lfo = Self {
            phase: 0.0,
            value: 0.0,
            rng: seed.max(1),
            held: 0.0,
            next: 0.0,
            done: false,
            chaos: chaos::start(SHAPE_LORENZ, 0.0),
            chaos_shape: SHAPE_LORENZ,
            mods: LfoMods::default(),
        };
        lfo.held = lfo.random();
        lfo.next = lfo.random();
        lfo
    }

    fn random(&mut self) -> f32 {
        dsp::noise::bipolar(dsp::xorshift32(&mut self.rng))
    }

    /// Starts over from `settings.start_phase`, with the output jumping
    /// straight to the shape's value there. A chaos shape starts at a
    /// point of its own on the attractor, so voices drift apart.
    pub fn restart(&mut self, settings: &LfoSettings) {
        self.phase = settings.start_phase.rem_euclid(1.0);
        self.done = false;
        self.held = self.random();
        self.next = self.random();
        if is_chaos(settings.shape) {
            let jitter = self.random() * 2.0;
            self.chaos = chaos::start(settings.shape, jitter);
            self.chaos_shape = settings.shape;
            // Some way along, so the start phase still means something.
            chaos::advance(settings.shape, &mut self.chaos, self.phase);
        }
        self.value = self.raw(settings);
    }

    /// Restarts the random sequence from `seed`, so each note draws its own
    /// values yet a render from the top repeats exactly.
    pub fn reseed(&mut self, seed: u32) {
        let mut x = seed.wrapping_mul(0x9E37_79B9) ^ 0x2545_F491;
        x ^= x >> 15;
        x = x.wrapping_mul(0x846C_A68B);
        x ^= x >> 16;
        self.rng = x.max(1);
    }

    /// For a chaos shape, the second coordinate of where it is, `-1..=1`,
    /// to draw the attractor's dot; 0 for the others.
    pub fn chaos_y(&self, shape: usize) -> f32 {
        if is_chaos(shape) && shape == self.chaos_shape {
            chaos::view(shape, self.chaos).1
        } else {
            0.0
        }
    }

    /// Where the shape is read: the clock's phase shifted by the phase
    /// modulation, around the cycle (or, played once, held at its ends).
    /// A finished one-shot stays at the end.
    pub fn read_phase(&self, mode: usize) -> f32 {
        if self.mods.phase == 0.0 || self.done {
            self.phase
        } else if repeats(mode) {
            (self.phase + self.mods.phase).rem_euclid(1.0)
        } else {
            (self.phase + self.mods.phase).clamp(0.0, 1.0)
        }
    }

    fn raw(&self, settings: &LfoSettings) -> f32 {
        let phase = self.read_phase(settings.mode);
        match settings.shape {
            SHAPE_SAMPLE_HOLD => self.held,
            SHAPE_SMOOTH_RANDOM => self.held + (self.next - self.held) * smoothstep(self.phase),
            SHAPE_CUSTOM => {
                let wrap = repeats(settings.mode);
                2.0 * custom_shape(&settings.points, phase, settings.glide, wrap) - 1.0
            }
            SHAPE_NODES => {
                // A finished one-shot stays on the last node.
                let phase = if phase >= 1.0 { 1.0 } else { phase };
                2.0 * settings.nodes.value(phase) - 1.0
            }
            SHAPE_LORENZ | SHAPE_ROSSLER => chaos::view(settings.shape, self.chaos).0,
            shape => periodic(shape, phase),
        }
    }

    /// A new cycle begins: the random shapes draw their next value.
    fn new_cycle(&mut self, settings: &LfoSettings) {
        self.held = if settings.shape == SHAPE_SMOOTH_RANDOM {
            self.next
        } else {
            self.random()
        };
        self.next = self.random();
    }

    /// Advances by `seconds` at `rate_hz` (the settings' rate after
    /// modulation) and returns the new value. `held`: the key (any key,
    /// for a mono LFO) is down, which the Loop and Sustain modes wait on.
    pub fn advance(
        &mut self,
        settings: &LfoSettings,
        rate_hz: f32,
        seconds: f32,
        held: bool,
    ) -> f32 {
        // Faster than half the update rate would only alias.
        let rate = if rate_hz.is_finite() {
            rate_hz.clamp(0.0, 0.5 / seconds)
        } else {
            0.0
        };
        let step = rate * seconds;
        if is_chaos(settings.shape) {
            if self.chaos_shape != settings.shape {
                self.chaos = chaos::start(settings.shape, 0.0);
                self.chaos_shape = settings.shape;
            }
            chaos::advance(settings.shape, &mut self.chaos, step);
        }
        if !self.done {
            let loop_point = settings.loop_point.clamp(0.0, 0.999);
            let before = self.phase;
            self.phase += step;
            if settings.mode == MODE_SUSTAIN && held && before <= loop_point {
                // Waits at the loop point while held.
                self.phase = self.phase.min(loop_point);
            }
            if self.phase >= 1.0 {
                match settings.mode {
                    MODE_ENV | MODE_SUSTAIN => {
                        self.phase = 1.0;
                        self.done = true;
                    }
                    MODE_LOOP if !held => {
                        self.phase = 1.0;
                        self.done = true;
                    }
                    MODE_LOOP => {
                        // Back to the loop point, as far past it as past the end.
                        let span = 1.0 - loop_point;
                        self.phase = loop_point + (self.phase - 1.0).rem_euclid(span);
                        self.new_cycle(settings);
                    }
                    _ => {
                        self.phase -= self.phase.floor();
                        self.new_cycle(settings);
                    }
                }
            }
        }
        let raw = self.raw(settings);
        let smooth = (settings.smooth + self.mods.smooth).clamp(0.0, 1.0);
        let tau = smooth * smooth * MAX_SMOOTH_S;
        if tau <= seconds * 0.5 {
            self.value = raw;
        } else {
            self.value += (raw - self.value) * (1.0 - (-seconds / tau).exp());
        }
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periodic_shapes_hit_their_landmarks() {
        assert!(lfo_value(SHAPE_SINE, 0.25) > 0.999);
        assert_eq!(lfo_value(SHAPE_TRIANGLE, 0.25), 1.0);
        assert_eq!(lfo_value(SHAPE_TRIANGLE, 0.75), -1.0);
        assert_eq!(lfo_value(SHAPE_SAW_UP, 0.0), -1.0);
        assert_eq!(lfo_value(SHAPE_SAW_DOWN, 0.0), 1.0);
        assert_eq!(lfo_value(SHAPE_SQUARE, 0.7), -1.0);
        assert_eq!(lfo_value(SHAPE_SINE, 1.25), lfo_value(SHAPE_SINE, 0.25));
    }

    #[test]
    fn random_shapes_stay_in_range_and_hold_per_cycle() {
        for i in 0..400 {
            let phase = i as f32 * 0.01;
            for shape in [SHAPE_SAMPLE_HOLD, SHAPE_SMOOTH_RANDOM] {
                let v = lfo_value(shape, phase);
                assert!((-1.0..=1.0).contains(&v), "{shape} at {phase}: {v}");
            }
        }
        assert_eq!(
            lfo_value(SHAPE_SAMPLE_HOLD, 2.1),
            lfo_value(SHAPE_SAMPLE_HOLD, 2.9)
        );
    }

    #[test]
    fn a_one_shot_stops_at_the_end_of_its_cycle() {
        let settings = LfoSettings {
            shape: SHAPE_SAW_UP,
            mode: MODE_ENV,
            ..LfoSettings::default()
        };
        let mut lfo = Lfo::new(1);
        lfo.restart(&settings);
        for _ in 0..300 {
            lfo.advance(&settings, 1.0, 0.01, true);
        }
        assert_eq!(lfo.phase, 1.0);
        assert_eq!(lfo.value, 1.0);
    }

    #[test]
    fn smoothing_rounds_off_a_square() {
        let settings = LfoSettings {
            shape: SHAPE_SQUARE,
            smooth: 0.5,
            ..LfoSettings::default()
        };
        let mut lfo = Lfo::new(1);
        lfo.restart(&settings);
        let mut previous = lfo.value;
        let mut largest_step = 0.0f32;
        for _ in 0..1000 {
            let v = lfo.advance(&settings, 2.0, 0.001, true);
            largest_step = largest_step.max((v - previous).abs());
            previous = v;
        }
        assert!(largest_step < 0.1, "{largest_step}");
    }

    fn zigzag() -> [f32; LFO_POINTS] {
        std::array::from_fn(|i| if i % 2 == 0 { 0.2 } else { 0.9 })
    }

    #[test]
    fn a_custom_shape_steps_glides_and_passes_its_points() {
        let points = zigzag();
        let step = 1.0 / LFO_POINTS as f32;
        for i in 0..LFO_POINTS {
            let start = i as f32 * step;
            // Without glide each point holds for its whole step.
            for t in [0.0, 0.5, 0.99] {
                let v = custom_shape(&points, start + t * step, 0.0, true);
                assert_eq!(v, points[i], "point {i} at {t}");
            }
            // With full glide the curve still passes through every point.
            let v = custom_shape(&points, start, 1.0, true);
            assert!((v - points[i]).abs() < 1e-6, "point {i}: {v}");
        }
        // Half glide holds for the first half of a step, then eases.
        let held = custom_shape(&points, 0.4 * step, 0.5, true);
        let easing = custom_shape(&points, 0.75 * step, 0.5, true);
        assert_eq!(held, 0.2);
        assert!((easing - 0.55).abs() < 1e-5, "{easing}");
    }

    #[test]
    fn a_one_shot_custom_shape_ends_on_its_last_point() {
        let mut points = zigzag();
        points[LFO_POINTS - 1] = 0.7;
        assert!((custom_shape(&points, 1.0, 1.0, false) - 0.7).abs() < 1e-6);
        assert!((custom_shape(&points, 1.0, 1.0, true) - 0.2).abs() < 1e-6);
        let settings = LfoSettings {
            shape: SHAPE_CUSTOM,
            mode: MODE_ENV,
            points,
            glide: 0.3,
            ..LfoSettings::default()
        };
        let mut lfo = Lfo::new(1);
        lfo.restart(&settings);
        for _ in 0..300 {
            lfo.advance(&settings, 1.0, 0.01, true);
        }
        assert!((lfo.value - 0.4).abs() < 1e-6, "{}", lfo.value);
    }

    #[test]
    fn nodes_pass_through_their_points_and_bend_between() {
        let nodes = Nodes::new(&[
            (0.0, 0.0, 0.0),
            (0.25, 1.0, -0.8),
            (0.5, 0.2, 0.0),
            (0.5, 0.8, 0.6),
            (1.0, 0.0, 0.0),
        ]);
        assert_eq!(nodes.value(0.0), 0.0);
        assert!((nodes.value(0.25) - 1.0).abs() < 1e-6);
        assert!(
            (nodes.value(0.125) - 0.5).abs() < 1e-6,
            "a straight segment"
        );
        // A negative curve leaves fast: past halfway at the segment's middle.
        let bent = nodes.value(0.375);
        assert!(bent < 0.6 - 0.1, "{bent}");
        // The jump at 0.5 lands on the second node there.
        assert!((nodes.value(0.5) - 0.8).abs() < 1e-6);
        assert!((nodes.value(0.4999) - 0.2).abs() < 0.01);
        // A positive curve leaves slowly.
        assert!(nodes.value(0.75) > 0.4 + 0.1);
        assert_eq!(nodes.value(1.0), 0.0);
        assert_eq!(nodes.value(1.25), nodes.value(0.25));
        // Out of order, missing or wild places are put right.
        let messy = Nodes::new(&[(0.7, 2.0, 9.0), (0.9, -1.0, 0.0), (0.3, f32::NAN, 0.0)]);
        assert_eq!(messy.x[..3], [0.0, 0.9, 1.0]);
        for i in 0..=100 {
            let v = messy.value(i as f32 / 100.0);
            assert!((0.0..=1.0).contains(&v), "{v}");
        }
    }

    #[test]
    fn loop_mode_loops_while_held_and_plays_out_after() {
        let settings = LfoSettings {
            shape: SHAPE_SAW_UP,
            mode: MODE_LOOP,
            loop_point: 0.5,
            ..LfoSettings::default()
        };
        let mut lfo = Lfo::new(1);
        lfo.restart(&settings);
        let mut lowest_after_first = 1.0f32;
        for i in 0..400 {
            lfo.advance(&settings, 1.0, 0.01, true);
            if i > 120 {
                lowest_after_first = lowest_after_first.min(lfo.phase);
            }
        }
        assert!(
            (0.49..0.6).contains(&lowest_after_first),
            "{lowest_after_first}"
        );
        for _ in 0..100 {
            lfo.advance(&settings, 1.0, 0.01, false);
        }
        assert_eq!(lfo.phase, 1.0);
    }

    #[test]
    fn sustain_mode_waits_at_its_point_until_let_go() {
        let settings = LfoSettings {
            shape: SHAPE_SAW_UP,
            mode: MODE_SUSTAIN,
            loop_point: 0.3,
            ..LfoSettings::default()
        };
        let mut lfo = Lfo::new(1);
        lfo.restart(&settings);
        for _ in 0..300 {
            lfo.advance(&settings, 1.0, 0.01, true);
        }
        assert!((lfo.phase - 0.3).abs() < 1e-6, "{}", lfo.phase);
        for _ in 0..30 {
            lfo.advance(&settings, 1.0, 0.01, false);
        }
        assert!((lfo.phase - 0.6).abs() < 1e-3, "{}", lfo.phase);
        for _ in 0..100 {
            lfo.advance(&settings, 1.0, 0.01, false);
        }
        assert_eq!(lfo.phase, 1.0);
    }

    #[test]
    fn chaos_moves_smoothly_and_stays_in_range() {
        for shape in [SHAPE_LORENZ, SHAPE_ROSSLER] {
            let settings = LfoSettings {
                shape,
                mode: MODE_RETRIG,
                ..LfoSettings::default()
            };
            let mut lfo = Lfo::new(7);
            lfo.restart(&settings);
            let (mut low, mut high) = (1.0f32, -1.0f32);
            let mut previous = lfo.value;
            let mut largest_step = 0.0f32;
            for _ in 0..5000 {
                let v = lfo.advance(&settings, 1.0, 0.002, true);
                assert!(v.is_finite() && (-1.0..=1.0).contains(&v));
                largest_step = largest_step.max((v - previous).abs());
                previous = v;
                low = low.min(v);
                high = high.max(v);
            }
            assert!(high - low > 0.8, "shape {shape}: {low}..{high}");
            assert!(largest_step < 0.05, "shape {shape}: {largest_step}");
            // Fast rates take more steps rather than leave the attractor.
            for _ in 0..1000 {
                let v = lfo.advance(&settings, 50.0, 0.02, true);
                assert!(v.is_finite());
            }
            assert!(!chaos_trace(shape, 2.0, 0.01).is_empty());
        }
    }

    #[test]
    fn delay_and_rise_fade_in_after_the_delay() {
        let settings = LfoSettings {
            delay: 0.5,
            rise: 1.0,
            mode: MODE_RETRIG,
            ..LfoSettings::default()
        };
        let none = LfoMods::default();
        assert_eq!(settings.rise_gain(0.3, &none), 0.0);
        assert!(!settings.running(0.3, &none));
        assert!((settings.rise_gain(1.0, &none) - 0.5).abs() < 1e-6);
        assert_eq!(settings.rise_gain(2.0, &none), 1.0);
        // Modulation moves both: half a second less delay, a second more rise.
        let mods = LfoMods {
            delay: -0.5,
            rise: 1.0,
            ..LfoMods::default()
        };
        assert!(settings.running(0.3, &mods));
        assert!((settings.rise_gain(0.5, &mods) - 0.25).abs() < 1e-6);
        let free = LfoSettings {
            mode: MODE_FREE,
            ..settings
        };
        assert!(free.running(0.0, &none), "a free LFO keeps time");
    }

    #[test]
    fn phase_modulation_shifts_where_the_shape_is_read() {
        let settings = LfoSettings {
            shape: SHAPE_SINE,
            mode: MODE_RETRIG,
            ..LfoSettings::default()
        };
        let mut lfo = Lfo::new(3);
        lfo.restart(&settings);
        let plain = lfo.advance(&settings, 1.0, 0.1, true);
        assert!((plain - periodic(SHAPE_SINE, 0.1)).abs() < 1e-6);
        lfo.mods.phase = 0.95;
        let shifted = lfo.advance(&settings, 1.0, 0.0, true);
        assert!((shifted - periodic(SHAPE_SINE, 0.05)).abs() < 1e-6);
        assert!(
            (lfo.phase - 0.1).abs() < 1e-6,
            "the clock runs on as it was"
        );
        assert!((lfo.read_phase(MODE_RETRIG) - 0.05).abs() < 1e-6);
        // Played once, the shifted read stops at the end.
        assert_eq!(lfo.read_phase(MODE_ENV), 1.0);
    }
}
