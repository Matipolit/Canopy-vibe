//! An electric guitar's pickups: where along the string they listen, and
//! what their coils do to the sound on its way to the amp.
//!
//! A magnetic pickup senses the string's velocity over a short stretch of
//! it. Where it sits makes a comb: it hears nothing of the partials that
//! have a node there, so a bridge pickup (a few centimetres from the
//! saddle) is thin and bright and a neck pickup round and hollow. The
//! waveguide carries velocity waves, and the velocity at a point is a wave
//! minus its reflection a fixed time later, so each pickup is read straight
//! from the string's delay line ([`super::waveguide::Loop::comb`]). Its
//! width averages the string over a centimetre or two, a gentle lowpass
//! that matters most on the slow waves of the low strings. A humbucker is
//! two coils side by side, two combs added.
//!
//! The coil's inductance with the cable's capacitance makes a resonant
//! lowpass, near 4 kHz with a pronounced peak for a single coil and lower
//! and broader for a humbucker; it is most of what makes a pickup sound
//! like a pickup.

use std::f32::consts::TAU;

use super::models::{PickupSpec, pickup_mix};

/// Most combs a note is heard through: two pickups of two coils each.
pub(super) const MAX_TAPS: usize = 4;

/// Where a note is heard from, as fractions of its vibrating length.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Taps {
    pub count: usize,
    /// Distance of each coil from the bridge over the vibrating length.
    pub position: [f32; MAX_TAPS],
    pub gain: [f32; MAX_TAPS],
    /// Cutoff of the aperture's lowpass, Hz.
    pub aperture_hz: f32,
}

/// The coils that switch position `switch` (see
/// [`super::models::PICKUP_LABELS`]) mixes, for a string vibrating over
/// `length` metres at `hz`.
pub(super) fn taps(spec: &PickupSpec, switch: usize, length: f32, hz: f32) -> Taps {
    let mut taps = Taps::default();
    let coils: &[f32] = if spec.coil_spacing > 0.0 {
        &[-0.5, 0.5]
    } else {
        &[0.0]
    };
    for (pickup, &mix) in pickup_mix(switch).iter().enumerate() {
        if mix == 0.0 {
            continue;
        }
        for &offset in coils {
            let at = spec.positions[pickup] + offset * spec.coil_spacing;
            taps.position[taps.count] = (at / length).clamp(0.005, 0.95);
            taps.gain[taps.count] = mix / coils.len() as f32;
            taps.count += 1;
        }
    }
    // Averaging over the aperture: its first null falls where a partial's
    // half wavelength fits in it; a one-pole a little under that stands in.
    let wave_speed = 2.0 * length * hz;
    taps.aperture_hz = 0.45 * wave_speed / spec.aperture;
    taps
}

/// The coil's resonance with its load: a resonant two-pole lowpass.
#[derive(Debug, Clone, Default)]
pub(super) struct Electronics {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Electronics {
    /// Sets the resonance (RBJ lowpass).
    pub fn set(&mut self, hz: f32, q: f32, sample_rate: f32) {
        let w = TAU * hz.min(0.45 * sample_rate) / sample_rate;
        let alpha = w.sin() / (2.0 * q);
        let cos = w.cos();
        let norm = 1.0 / (1.0 + alpha);
        self.b0 = 0.5 * (1.0 - cos) * norm;
        self.b1 = (1.0 - cos) * norm;
        self.b2 = self.b0;
        self.a1 = -2.0 * cos * norm;
        self.a2 = (1.0 - alpha) * norm;
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    pub fn process(&mut self, signal: &mut [f32]) {
        for x in signal {
            let y = self.b0 * *x + self.b1 * self.x1 + self.b2 * self.x2
                - self.a1 * self.y1
                - self.a2 * self.y2;
            self.x2 = self.x1;
            self.x1 = *x;
            self.y2 = self.y1;
            self.y1 = if y.abs() < 1e-20 { 0.0 } else { y };
            *x = y;
        }
    }

    /// Gain at `hz`, for tests.
    #[cfg(test)]
    pub fn gain(&self, hz: f32, sample_rate: f32) -> f32 {
        let w = TAU * hz / sample_rate;
        let (c1, s1, c2, s2) = (w.cos(), -w.sin(), (2.0 * w).cos(), -(2.0 * w).sin());
        let (nr, ni) = (
            self.b0 + self.b1 * c1 + self.b2 * c2,
            self.b1 * s1 + self.b2 * s2,
        );
        let (dr, di) = (
            1.0 + self.a1 * c1 + self.a2 * c2,
            self.a1 * s1 + self.a2 * s2,
        );
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

/// A one-pole lowpass coefficient for `hz` at `sample_rate`.
pub(super) fn one_pole(hz: f32, sample_rate: f32) -> f32 {
    (-TAU * hz.min(0.49 * sample_rate) / sample_rate).exp()
}

/// The comb a single coil at `position` makes, at partial `n`.
#[cfg(test)]
pub(super) fn comb_gain(position: f32, n: f32) -> f32 {
    (std::f32::consts::PI * n * position).sin().abs() * 2.0
}

#[cfg(test)]
mod tests {
    use super::super::models::pickups;
    use super::*;

    #[test]
    fn bridge_pickups_sit_nearer_the_end_of_the_string_than_neck_ones() {
        let spec = pickups(false);
        let neck = taps(spec, 0, 0.648, 110.0);
        let bridge = taps(spec, 4, 0.648, 110.0);
        assert_eq!((neck.count, bridge.count), (1, 1));
        assert!(neck.position[0] > 3.0 * bridge.position[0]);
        // Fretting at the 12th halves the string: the pickup is relatively
        // twice as far along it.
        let fretted = taps(spec, 4, 0.324, 220.0);
        assert!((fretted.position[0] / bridge.position[0] - 2.0).abs() < 1e-3);
        // A humbucker has two coils; two pickups make four combs.
        assert_eq!(taps(pickups(true), 1, 0.648, 110.0).count, 4);
    }

    #[test]
    fn the_coil_resonates_where_it_should() {
        let mut coil = Electronics::default();
        coil.set(3800.0, 2.4, 48_000.0);
        let peak = coil.gain(3800.0, 48_000.0);
        assert!((peak - 2.4).abs() < 0.1, "{peak}");
        assert!((coil.gain(200.0, 48_000.0) - 1.0).abs() < 0.02);
        assert!(coil.gain(12_000.0, 48_000.0) < 0.15);
    }

    #[test]
    fn a_neck_pickup_hears_less_treble_than_a_bridge_one() {
        let spec = pickups(false);
        let neck = taps(spec, 0, 0.648, 110.0).position[0];
        let bridge = taps(spec, 4, 0.648, 110.0).position[0];
        // Sum of the combs over partials 15 to 30 against 1 to 4.
        let tilt = |p: f32| {
            let high: f32 = (15..30).map(|n| comb_gain(p, n as f32)).sum::<f32>() / 15.0;
            let low: f32 = (1..5).map(|n| comb_gain(p, n as f32)).sum::<f32>() / 4.0;
            high / low
        };
        assert!(
            tilt(bridge) > 1.25 * tilt(neck),
            "{} {}",
            tilt(bridge),
            tilt(neck)
        );
    }
}
