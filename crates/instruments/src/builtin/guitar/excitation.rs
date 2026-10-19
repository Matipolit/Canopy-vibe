//! How a string is set in motion: by a pick, a finger, a fretting finger
//! hammering on or pulling off, or a scratch on a muted string.
//!
//! The force a plucked string puts on the bridge is, for one period, a
//! rectangular pulse as long as the stretch of string between the bridge and
//! the pluck: its spectrum, `sin(n π β) / n` for pluck position `β`, has the
//! characteristic nulls of a pluck (none of the harmonics that have a node
//! under the pick) and the -6 dB/octave slope of a real one (Fletcher &
//! Rossing 1998, §9.2; the excitation of Karjalainen, Välimäki & Tolonen
//! 1998). What plucks it shapes the rest:
//!
//! - The pulse's edges take as long as the string takes to slip off: a
//!   fraction of a millisecond off a hard pick, a millisecond or more off
//!   the flesh of a finger (Evangelista & Eckerholm 2010; Cuzzucoli &
//!   Lombardo 1999). Harder playing lets go faster, so loud notes are
//!   brighter, as on a real guitar.
//! - A fingertip's flesh smooths it further (a gentle lowpass).
//! - A pick scrapes the string as it goes over it, loudest on the wound
//!   strings: a short burst of band-passed noise, most of it into the
//!   string (where it rings at the string's pitches) and some heard as is.
//! - A pick or a nail leaving the string ticks: a millisecond-long damped
//!   tone heard directly.
//!
//! Everything is deterministic for a given [`Pluck`], whose `seed` the
//! instrument derives from the note, so renders repeat exactly.

use std::f32::consts::TAU;

/// A pluck, as numbers. Times are in samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Pluck {
    /// Height of the pulse. Full velocity is 1; negative for an up-stroke.
    pub amplitude: f32,
    /// Pulse length: the pluck position times the period.
    pub length: f32,
    /// Rise and fall time of the pulse's edges. Wider is softer.
    pub width: f32,
    /// One-pole smoothing coefficient applied after the pulse, `0..1`: a
    /// fingertip's flesh. 0 for none.
    pub flesh: f32,
    /// Peak of the pick's scrape, relative to `amplitude`.
    pub scrape: f32,
    /// Samples for the scrape to fall by a factor of e.
    pub scrape_time: f32,
    /// Centre of the scrape's band, radians per sample.
    pub scrape_w: f32,
    /// Level of the tick of the pick or nail leaving the string, relative
    /// to `amplitude`, heard directly.
    pub click: f32,
    /// Its frequency, radians per sample.
    pub click_w: f32,
    /// Samples for the tick to fall by a factor of e.
    pub click_time: f32,
    /// Seed for the noise, so every pluck sounds a little different.
    pub seed: u32,
}

impl Pluck {
    /// Nothing at all: a string set going only by the bridge.
    pub const NONE: Pluck = Pluck {
        amplitude: 0.0,
        length: 1.0,
        width: 1.0,
        flesh: 0.0,
        scrape: 0.0,
        scrape_time: 1.0,
        scrape_w: 0.5,
        click: 0.0,
        click_w: 0.5,
        click_time: 1.0,
        seed: 1,
    };
}

/// Share of the scrape heard directly rather than through the string.
const SCRAPE_DIRECT: f32 = 0.3;

/// Q of the scrape's band.
const SCRAPE_Q: f32 = 1.2;

/// Plays a [`Pluck`] sample by sample: what goes into the string, and what
/// is heard directly.
#[derive(Debug, Clone, Default)]
pub(super) struct Excitation {
    /// Samples since the pluck.
    t: f32,
    /// When everything is over.
    end: f32,
    amplitude: f32,
    length: f32,
    inv_width: f32,
    flesh: f32,
    flesh_state: f32,
    scrape: f32,
    scrape_decay: f32,
    /// Band-pass resonator for the scrape (direct form, two poles).
    band_b0: f32,
    band_a1: f32,
    band_a2: f32,
    band_y1: f32,
    band_y2: f32,
    band_x2: f32,
    band_x1: f32,
    /// The tick, as a decaying rotating phasor.
    click: f32,
    click_cos: f32,
    click_sin: f32,
    click_re: f32,
    click_im: f32,
    rng: u32,
}

impl Excitation {
    pub fn new(pluck: &Pluck) -> Self {
        let width = pluck.width.max(1.0);
        let scrape_time = pluck.scrape_time.max(1.0);
        let click_time = pluck.click_time.max(1.0);
        let flesh = pluck.flesh.clamp(0.0, 0.995);
        let flesh_tail = if flesh > 0.0 { -8.0 / flesh.ln() } else { 0.0 };
        // Constant-peak band-pass (RBJ) for the scrape.
        let w = pluck.scrape_w.clamp(0.01, 3.0);
        let alpha = w.sin() / (2.0 * SCRAPE_Q);
        let norm = 1.0 / (1.0 + alpha);
        let decay = (-1.0 / click_time).exp();
        let mut end = pluck.length + width + flesh_tail;
        if pluck.scrape > 0.0 {
            end = end.max(7.0 * scrape_time);
        }
        if pluck.click > 0.0 {
            end = end.max(7.0 * click_time);
        }
        Self {
            t: 0.0,
            end,
            amplitude: pluck.amplitude,
            length: pluck.length,
            inv_width: 1.0 / width,
            flesh,
            flesh_state: 0.0,
            scrape: pluck.scrape,
            scrape_decay: (-1.0 / scrape_time).exp(),
            band_b0: alpha * norm,
            band_a1: -2.0 * w.cos() * norm,
            band_a2: (1.0 - alpha) * norm,
            band_y1: 0.0,
            band_y2: 0.0,
            band_x1: 0.0,
            band_x2: 0.0,
            click: pluck.click,
            click_cos: decay * pluck.click_w.cos(),
            click_sin: decay * pluck.click_w.sin(),
            click_re: 1.0,
            click_im: 0.0,
            rng: pluck.seed | 1,
        }
    }

    /// The next sample: (into the string, heard directly).
    #[inline]
    pub fn next(&mut self) -> (f32, f32) {
        let t = self.t;
        let pulse = smoothstep(t * self.inv_width) - smoothstep((t - self.length) * self.inv_width);
        let shaped = if self.flesh > 0.0 {
            self.flesh_state = pulse + (self.flesh_state - pulse) * self.flesh;
            self.flesh_state
        } else {
            pulse
        };
        let mut scrape = 0.0;
        if self.scrape > 1e-6 {
            let white = dsp::noise::bipolar(dsp::xorshift32(&mut self.rng));
            let drive = white - self.band_x2;
            self.band_x2 = self.band_x1;
            self.band_x1 = white;
            let y =
                self.band_b0 * drive - self.band_a1 * self.band_y1 - self.band_a2 * self.band_y2;
            self.band_y2 = self.band_y1;
            self.band_y1 = y;
            // The scrape builds over the first moments of contact.
            let onset = (t * 0.25).min(1.0);
            scrape = y * self.scrape * onset;
            self.scrape *= self.scrape_decay;
        }
        let mut click = 0.0;
        if self.click > 1e-6 {
            let re = self.click_re * self.click_cos - self.click_im * self.click_sin;
            let im = self.click_re * self.click_sin + self.click_im * self.click_cos;
            self.click_re = re;
            self.click_im = im;
            click = self.click * im;
        }
        self.t += 1.0;
        (
            (shaped + scrape * (1.0 - SCRAPE_DIRECT)) * self.amplitude,
            (scrape * SCRAPE_DIRECT + click) * self.amplitude,
        )
    }

    pub fn finished(&self) -> bool {
        self.t >= self.end
    }
}

/// `0` below 0, `1` above 1, an S-curve between.
#[inline]
fn smoothstep(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

/// Radians per sample of `hz` at `rate`.
pub(super) fn omega(hz: f32, rate: f32) -> f32 {
    (TAU * hz / rate).min(3.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pluck(width: f32) -> Pluck {
        Pluck {
            amplitude: 1.0,
            length: 100.0,
            width,
            ..Pluck::NONE
        }
    }

    fn run(pluck: &Pluck) -> (Vec<f32>, Vec<f32>) {
        let mut excitation = Excitation::new(pluck);
        let mut string = Vec::new();
        let mut direct = Vec::new();
        while !excitation.finished() {
            let (s, d) = excitation.next();
            string.push(s);
            direct.push(d);
        }
        (string, direct)
    }

    /// Energy of the first difference over the energy: how bright.
    fn brightness(x: &[f32]) -> f32 {
        let slope: f32 = x.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
        slope / x.iter().map(|v| v * v).sum::<f32>()
    }

    #[test]
    fn a_pulse_as_long_as_asked_with_softer_edges_when_wider() {
        let (hard, _) = run(&pluck(2.0));
        let (soft, _) = run(&pluck(30.0));
        let area = |x: &[f32]| x.iter().sum::<f32>();
        // Same area: the same displacement, let go of differently.
        assert!((area(&hard) - 100.0).abs() < 1.0);
        assert!((area(&soft) - 100.0).abs() < 1.0);
        assert!(brightness(&hard) > 5.0 * brightness(&soft));
    }

    #[test]
    fn flesh_rounds_the_pulse_and_scrape_and_click_add_noise() {
        let (plain, direct) = run(&pluck(4.0));
        assert!(direct.iter().all(|&d| d == 0.0));
        let (fleshy, _) = run(&Pluck {
            flesh: 0.8,
            ..pluck(4.0)
        });
        assert!(brightness(&fleshy) < 0.5 * brightness(&plain));
        let picked = Pluck {
            scrape: 0.2,
            scrape_time: 100.0,
            scrape_w: omega(3000.0, 48_000.0),
            click: 0.1,
            click_w: omega(4000.0, 48_000.0),
            click_time: 40.0,
            seed: 7,
            ..pluck(4.0)
        };
        let (string, direct) = run(&picked);
        let (scraped, smooth) = (brightness(&string), brightness(&plain));
        assert!(scraped > 1.05 * smooth, "{scraped} vs {smooth}");
        let heard: f32 = direct.iter().map(|d| d.abs()).fold(0.0, f32::max);
        assert!(heard > 0.02, "{heard}");
        // The same pluck again is the same, sample for sample.
        assert_eq!(run(&picked), (string, direct));
    }
}
