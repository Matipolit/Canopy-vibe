//! The sounds of the hands rather than the strings: a finger coming off a
//! string, a fingertip sliding along a wound string as the hand moves (the
//! squeak), and a string slapping onto a fret.
//!
//! A squeak follows Pakarinen, Penttinen & Bank 2007: a fingertip sliding
//! along a wound string bumps over its windings, a train of little impulses
//! at the sliding speed over the winding pitch (one to a few kilohertz),
//! which the string's own longitudinal resonances colour. The hand speeds
//! up and slows down, so the pitch of the squeak rises and falls. The thump
//! of a finger lifting off is a short, dull burst of noise, and a fret
//! click a shorter, brighter one.
//!
//! A few at a time are enough: each new one takes a free slot, or the
//! quietest.

use std::f32::consts::{PI, TAU};

/// Noises sounding at once.
const SLOTS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Idle,
    /// Filtered noise: a finger's thump or a fret's click.
    Burst,
    /// A fingertip over the windings.
    Squeak,
}

/// Two-pole band-pass resonator with a constant peak gain of 1.
#[derive(Debug, Clone, Copy, Default)]
struct Resonator {
    b0: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Resonator {
    fn new(w: f32, q: f32) -> Self {
        let alpha = w.sin() / (2.0 * q);
        let norm = 1.0 / (1.0 + alpha);
        Self {
            b0: alpha * norm,
            a1: -2.0 * w.cos() * norm,
            a2: (1.0 - alpha) * norm,
            ..Self::default()
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * (x - self.x2) - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    kind: Kind,
    /// Samples since it started, and how long it lasts.
    t: f32,
    length: f32,
    level: f32,
    /// Bursts: the noise's lowpass and its decay per sample.
    lowpass: f32,
    lowpass_state: f32,
    decay: f32,
    envelope: f32,
    /// Share of the 1 ms rise covered per sample.
    rise: f32,
    /// Squeaks: impulses per sample at the fastest, and where the train is.
    peak_rate: f32,
    phase: f32,
    resonators: [Resonator; 2],
    rng: u32,
}

impl Slot {
    const IDLE: Slot = Slot {
        kind: Kind::Idle,
        t: 0.0,
        length: 0.0,
        level: 0.0,
        lowpass: 0.0,
        lowpass_state: 0.0,
        decay: 0.0,
        envelope: 0.0,
        rise: 1.0,
        peak_rate: 0.0,
        phase: 0.0,
        resonators: [Resonator {
            b0: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }; 2],
        rng: 1,
    };

    fn white(&mut self) -> f32 {
        dsp::noise::bipolar(dsp::xorshift32(&mut self.rng))
    }

    #[inline]
    fn next(&mut self) -> f32 {
        let out = match self.kind {
            Kind::Idle => 0.0,
            Kind::Burst => {
                let white = self.white();
                self.lowpass_state = white + (self.lowpass_state - white) * self.lowpass;
                // A 1 ms rise, so even a click isn't a step.
                let rise = (self.t * self.rise).min(1.0);
                let out = self.lowpass_state * self.envelope * rise * self.level;
                self.envelope *= self.decay;
                out
            }
            Kind::Squeak => {
                // The hand speeds up, then slows: the rate follows half a sine.
                let progress = self.t / self.length;
                let shape = (PI * progress).sin();
                self.phase += self.peak_rate * shape.max(0.05);
                let mut impulse = 0.0;
                if self.phase >= 1.0 {
                    self.phase -= 1.0;
                    impulse = 1.0;
                }
                let excitation = impulse + 0.15 * self.white();
                let ring = self.resonators[0].process(excitation)
                    + 0.6 * self.resonators[1].process(excitation);
                ring * shape * self.level
            }
        };
        self.t += 1.0;
        if self.t >= self.length {
            self.kind = Kind::Idle;
        }
        out
    }

    /// Roughly how loud it still is.
    fn loudness(&self) -> f32 {
        match self.kind {
            Kind::Idle => 0.0,
            Kind::Burst => self.level * self.envelope,
            Kind::Squeak => self.level * (1.0 - self.t / self.length),
        }
    }
}

pub(super) struct Noises {
    sample_rate: f32,
    slots: [Slot; SLOTS],
}

impl Noises {
    pub fn new() -> Self {
        Self {
            sample_rate: 48_000.0,
            slots: [Slot::IDLE; SLOTS],
        }
    }

    pub fn prepare(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.reset();
    }

    pub fn reset(&mut self) {
        self.slots = [Slot::IDLE; SLOTS];
    }

    pub fn is_active(&self) -> bool {
        self.slots.iter().any(|s| s.kind != Kind::Idle)
    }

    fn free_slot(&mut self) -> &mut Slot {
        let index = self
            .slots
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.loudness().total_cmp(&b.1.loudness()))
            .map_or(0, |(i, _)| i);
        &mut self.slots[index]
    }

    /// A burst of noise lowpassed at `hz`, falling by 60 dB in `seconds`.
    pub fn burst(&mut self, level: f32, hz: f32, seconds: f32, seed: u32) {
        if level <= 0.0 {
            return;
        }
        let rate = self.sample_rate;
        let length = seconds * rate;
        *self.free_slot() = Slot {
            kind: Kind::Burst,
            length,
            level,
            lowpass: (-TAU * hz.min(0.45 * rate) / rate).exp(),
            // Lowpassing white noise loses level; give some back.
            envelope: (rate / (TAU * hz)).sqrt().min(30.0),
            decay: (-6.9 / length).exp(),
            rise: 1000.0 / rate,
            rng: seed | 1,
            ..Slot::IDLE
        };
    }

    /// A finger sliding along a wound string for `seconds`, bumping over
    /// the windings up to `peak_hz` times a second, coloured by the
    /// string's longitudinal resonance near `ring_hz`.
    pub fn squeak(&mut self, level: f32, peak_hz: f32, ring_hz: f32, seconds: f32, seed: u32) {
        if level <= 0.0 {
            return;
        }
        let rate = self.sample_rate;
        let w = |hz: f32| (TAU * hz / rate).min(3.0);
        *self.free_slot() = Slot {
            kind: Kind::Squeak,
            length: seconds * rate,
            level,
            peak_rate: (peak_hz / rate).min(0.45),
            resonators: [
                Resonator::new(w(ring_hz), 6.0),
                Resonator::new(w(2.3 * ring_hz), 5.0),
            ],
            rng: seed | 1,
            ..Slot::IDLE
        };
    }

    /// Adds whatever is sounding to `out`.
    pub fn render(&mut self, out: &mut [f32]) {
        for slot in self.slots.iter_mut().filter(|s| s.kind != Kind::Idle) {
            for sample in out.iter_mut() {
                *sample += slot.next();
                if slot.kind == Kind::Idle {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(noises: &mut Noises, samples: usize) -> Vec<f32> {
        let mut out = vec![0.0; samples];
        for chunk in out.chunks_mut(64) {
            noises.render(chunk);
        }
        out
    }

    #[test]
    fn noises_end_and_leave_silence() {
        let mut noises = Noises::new();
        noises.prepare(48_000.0);
        noises.burst(0.1, 800.0, 0.03, 3);
        noises.squeak(0.1, 2000.0, 1400.0, 0.1, 5);
        let out = render(&mut noises, 9600);
        assert!(out[..4800].iter().any(|s| s.abs() > 0.01));
        assert!(out[4900..].iter().all(|&s| s == 0.0));
        assert!(!noises.is_active());
    }

    #[test]
    fn a_squeak_rings_near_its_resonance() {
        let mut noises = Noises::new();
        noises.prepare(48_000.0);
        noises.squeak(0.1, 2500.0, 1400.0, 0.2, 9);
        let out = render(&mut noises, 9600);
        let level = |hz: f32| {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, &x) in out.iter().enumerate() {
                let arg = std::f64::consts::TAU * f64::from(hz) * n as f64 / 48_000.0;
                re += f64::from(x) * arg.cos();
                im += f64::from(x) * arg.sin();
            }
            (re * re + im * im).sqrt()
        };
        assert!(level(1400.0) > 3.0 * level(300.0));
    }
}
