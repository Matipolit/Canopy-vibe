//! Splitting sound into frequency bands that add back up to the whole: the
//! effect graph's band Split nodes, like Bitwig's Multiband FX.
//!
//! Each crossover point is a Linkwitz-Riley filter pair, a Butterworth
//! lowpass and highpass each applied twice: 12 dB/octave (two first-order
//! Butterworths), 24 dB/octave (two second-order) or 48 dB/octave (two
//! fourth-order). A pair's halves add up to an allpass, flat in level at
//! every frequency, with no latency (they're minimum phase, unlike a
//! linear-phase split). The 12 dB pair's highpass comes out inverted, as it
//! must for its halves to add up flat.
//!
//! With more than one crossover the sound is split at the lowest first,
//! then what's above it at the next, and so on. A band split off early
//! hasn't been through the later crossovers' phase shifts, so it goes
//! through each one's allpass (what that crossover's halves add up to)
//! too: untouched, all the bands sum to one allpass, flat in level
//! whatever the frequencies.
//!
//! The filters are Simper's trapezoidal state-variable filters (as in
//! `svf.rs`), all from the same prewarped integrator gain per crossover, so
//! the identities hold exactly after discretization, not just in the analog
//! prototype. Frequencies glide when they change, a block at a time.

use std::f32::consts::{FRAC_1_SQRT_2, PI};

/// The most bands a [`Crossover`] makes: three crossover points.
pub const MAX_BANDS: usize = 4;
const MAX_POINTS: usize = MAX_BANDS - 1;

/// Lowest and highest crossover frequency, as a fraction of the sample rate
/// at the top: past it the prewarped gain heads for infinity.
const MIN_FREQ: f32 = 10.0;
const MAX_FREQ_RATIO: f32 = 0.45;

/// How quickly a changed frequency glides to its new place: the time
/// constant, in seconds, of the glide in log frequency.
const GLIDE_SECONDS: f32 = 0.02;

/// How steep a crossover's filters are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CrossoverSlope {
    Db12,
    #[default]
    Db24,
    Db48,
}

impl CrossoverSlope {
    /// The Butterworth order applied twice.
    fn order(self) -> i32 {
        match self {
            CrossoverSlope::Db12 => 1,
            CrossoverSlope::Db24 => 2,
            CrossoverSlope::Db48 => 4,
        }
    }
}

/// The level (as a linear gain, 0 to 1) of `band` of a split at
/// `crossovers` (Hz, ascending) at `frequency`, from the analog prototype:
/// for drawing a split, not for processing.
pub fn band_gain(slope: CrossoverSlope, crossovers: &[f32], band: usize, frequency: f32) -> f32 {
    let order = slope.order();
    let crossovers = &crossovers[..crossovers.len().min(MAX_POINTS)];
    let mut gain = 1.0;
    for (stage, &fc) in crossovers.iter().enumerate() {
        let ratio = (frequency / fc.max(MIN_FREQ)).powi(2 * order);
        let low = 1.0 / (1.0 + ratio);
        match stage.cmp(&band) {
            // Split off below an earlier crossover: what's above it went on.
            std::cmp::Ordering::Less => gain *= 1.0 - low,
            std::cmp::Ordering::Equal => return gain * low,
            std::cmp::Ordering::Greater => break,
        }
    }
    gain
}

/// Coefficients of a trapezoidal state-variable filter.
#[derive(Debug, Clone, Copy, Default)]
struct Coefs {
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl Coefs {
    /// From the prewarped integrator gain `g` and damping `k` (`1 / Q`).
    fn new(g: f32, k: f32) -> Coefs {
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        Coefs {
            k,
            a1,
            a2,
            a3: g * a2,
        }
    }
}

/// One channel of a state-variable filter: lowpass, bandpass and highpass
/// at once.
#[derive(Debug, Clone, Copy, Default)]
struct Svf {
    s1: f32,
    s2: f32,
}

impl Svf {
    #[inline]
    fn tick(&mut self, c: &Coefs, x: f32) -> (f32, f32, f32) {
        let v3 = x - self.s2;
        let v1 = c.a1 * self.s1 + c.a2 * v3;
        let v2 = self.s2 + c.a2 * self.s1 + c.a3 * v3;
        self.s1 = 2.0 * v1 - self.s1;
        self.s2 = 2.0 * v2 - self.s2;
        (v2, v1, x - c.k * v1 - v2)
    }

    /// The allpass the filter's three outputs make: `x - 2k·bandpass`.
    #[inline]
    fn allpass(&mut self, c: &Coefs, x: f32) -> f32 {
        let (_, band, _) = self.tick(c, x);
        x - 2.0 * c.k * band
    }

    fn flush(&mut self) {
        self.s1 = flush(self.s1);
        self.s2 = flush(self.s2);
    }
}

/// One channel of a first-order trapezoidal lowpass, for the 12 dB
/// crossover's first-order allpass.
#[derive(Debug, Clone, Copy, Default)]
struct OnePole {
    s: f32,
}

impl OnePole {
    /// `(1 - s) / (1 + s)` at the crossover: twice the lowpass, less the
    /// input. `gain` is `g / (1 + g)`.
    #[inline]
    fn allpass(&mut self, gain: f32, x: f32) -> f32 {
        let v = (x - self.s) * gain;
        let low = v + self.s;
        self.s = low + v;
        2.0 * low - x
    }
}

/// Denormals cost a lot of time in the filters' feedback; zero them.
fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0.0 } else { x }
}

/// The coefficients of one crossover point: the filters' (one set, or the
/// fourth-order Butterworth's two sections) and the one-pole's gain.
#[derive(Debug, Clone, Copy, Default)]
struct Point {
    first: Coefs,
    second: Coefs,
    pole: f32,
}

impl Point {
    fn new(slope: CrossoverSlope, freq: f32, sample_rate: f32) -> Point {
        let g = (PI * freq / sample_rate).tan();
        match slope {
            // Q = 0.5: the square of a first-order Butterworth.
            CrossoverSlope::Db12 => Point {
                first: Coefs::new(g, 2.0),
                second: Coefs::default(),
                pole: g / (1.0 + g),
            },
            CrossoverSlope::Db24 => Point {
                first: Coefs::new(g, 2.0 * FRAC_1_SQRT_2),
                second: Coefs::default(),
                pole: 0.0,
            },
            // The fourth-order Butterworth's two sections.
            CrossoverSlope::Db48 => Point {
                first: Coefs::new(g, 2.0 * (PI / 8.0).cos()),
                second: Coefs::new(g, 2.0 * (3.0 * PI / 8.0).cos()),
                pole: 0.0,
            },
        }
    }
}

/// One channel's filters for one crossover point: the split, and the
/// allpasses that bring each band split off below it into phase.
#[derive(Debug, Clone, Copy, Default)]
struct Stage {
    /// The 48 dB split needs seven filters: one shared first section, then
    /// three more on each side. The others use fewer.
    split: [Svf; 7],
    /// One allpass per band below this point.
    allpass: [[Svf; 2]; MAX_POINTS],
    poles: [OnePole; MAX_POINTS],
}

impl Stage {
    /// (low, high) for input `x`; they add up to this point's allpass.
    #[inline]
    fn split(&mut self, slope: CrossoverSlope, p: &Point, x: f32) -> (f32, f32) {
        let f = &mut self.split;
        match slope {
            CrossoverSlope::Db12 => {
                let (low, _, high) = f[0].tick(&p.first, x);
                (low, -high)
            }
            CrossoverSlope::Db24 => {
                let (low, _, high) = f[0].tick(&p.first, x);
                (f[1].tick(&p.first, low).0, f[2].tick(&p.first, high).2)
            }
            CrossoverSlope::Db48 => {
                let (low, _, high) = f[0].tick(&p.first, x);
                let low = f[1].tick(&p.second, low).0;
                let low = f[2].tick(&p.first, low).0;
                let low = f[3].tick(&p.second, low).0;
                let high = f[4].tick(&p.second, high).2;
                let high = f[5].tick(&p.first, high).2;
                let high = f[6].tick(&p.second, high).2;
                (low, high)
            }
        }
    }

    /// Band `band`'s sound through this point's allpass.
    #[inline]
    fn allpass(&mut self, slope: CrossoverSlope, p: &Point, band: usize, x: f32) -> f32 {
        match slope {
            CrossoverSlope::Db12 => self.poles[band].allpass(p.pole, x),
            CrossoverSlope::Db24 => self.allpass[band][0].allpass(&p.first, x),
            CrossoverSlope::Db48 => {
                let [a, b] = &mut self.allpass[band];
                b.allpass(&p.second, a.allpass(&p.first, x))
            }
        }
    }

    fn flush(&mut self) {
        for svf in self.split.iter_mut().chain(self.allpass.iter_mut().flatten()) {
            svf.flush();
        }
        for pole in &mut self.poles {
            pole.s = flush(pole.s);
        }
    }
}

/// A stereo band split: 2 to [`MAX_BANDS`] bands. Real-time safe; all its
/// state is inline.
#[derive(Debug, Clone)]
pub struct Crossover {
    sample_rate: f32,
    slope: CrossoverSlope,
    /// Crossover points in use.
    count: usize,
    /// Where each point is now, and where it's gliding to.
    freqs: [f32; MAX_POINTS],
    targets: [f32; MAX_POINTS],
    points: [Point; MAX_POINTS],
    channels: [[Stage; MAX_POINTS]; 2],
}

impl Crossover {
    /// A split at `crossovers` (Hz, 1 to 3 of them; sorted, and kept
    /// apart, here). Starts at its frequencies, without gliding.
    pub fn new(sample_rate: f32, slope: CrossoverSlope, crossovers: &[f32]) -> Crossover {
        let mut crossover = Crossover {
            sample_rate: sample_rate.max(1.0),
            slope,
            count: 0,
            freqs: [0.0; MAX_POINTS],
            targets: [0.0; MAX_POINTS],
            points: [Point::default(); MAX_POINTS],
            channels: [[Stage::default(); MAX_POINTS]; 2],
        };
        crossover.configure(slope, crossovers);
        crossover.freqs = crossover.targets;
        crossover.update();
        crossover
    }

    /// How many bands it makes.
    pub fn bands(&self) -> usize {
        self.count + 1
    }

    /// Changes the split. New frequencies glide from the old ones; a new
    /// slope or number of bands starts the filters afresh. Real-time safe.
    pub fn configure(&mut self, slope: CrossoverSlope, crossovers: &[f32]) {
        let count = crossovers.len().clamp(1, MAX_POINTS);
        if slope != self.slope || count != self.count {
            self.slope = slope;
            self.count = count;
            self.reset();
            self.freqs = [0.0; MAX_POINTS];
        }
        let top = self.sample_rate * MAX_FREQ_RATIO;
        let mut floor = MIN_FREQ;
        for (i, target) in self.targets.iter_mut().enumerate().take(count) {
            let f = crossovers.get(i).copied().unwrap_or(floor);
            let f = if f.is_finite() { f } else { floor };
            // Each above the last, so the bands keep their order.
            *target = f.clamp(floor, top);
            floor = (*target * 1.01).min(top);
        }
        for i in 0..count {
            if self.freqs[i] <= 0.0 {
                self.freqs[i] = self.targets[i];
            }
        }
        self.update();
    }

    /// Silences the filters, keeping the settings.
    pub fn reset(&mut self) {
        self.channels = [[Stage::default(); MAX_POINTS]; 2];
    }

    /// Before each block of `frames`: moves gliding frequencies along.
    pub fn begin_block(&mut self, frames: usize) {
        if self.freqs[..self.count] == self.targets[..self.count] {
            return;
        }
        let t = 1.0 - (-(frames as f32) / (GLIDE_SECONDS * self.sample_rate)).exp();
        for i in 0..self.count {
            let (from, to) = (self.freqs[i], self.targets[i]);
            let glided = from * (to / from).powf(t);
            self.freqs[i] = if (glided / to - 1.0).abs() < 1e-4 {
                to
            } else {
                glided
            };
        }
        self.update();
        for channel in &mut self.channels {
            for stage in channel {
                stage.flush();
            }
        }
    }

    fn update(&mut self) {
        for i in 0..self.count {
            self.points[i] = Point::new(self.slope, self.freqs[i], self.sample_rate);
        }
    }

    /// One sample of `channel` (0 left, 1 right): each band's sound; bands
    /// past [`Self::bands`] are 0.
    #[inline]
    pub fn tick(&mut self, channel: usize, x: f32) -> [f32; MAX_BANDS] {
        let stages = &mut self.channels[channel & 1];
        let mut bands = [0.0; MAX_BANDS];
        let mut rest = x;
        for point in 0..self.count {
            let p = &self.points[point];
            let (low, high) = stages[point].split(self.slope, p, rest);
            bands[point] = low;
            // The bands split off below take this point's phase shift.
            for (band, value) in bands.iter_mut().enumerate().take(point) {
                *value = stages[point].allpass(self.slope, p, band, *value);
            }
            rest = high;
        }
        bands[self.count] = rest;
        bands
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    /// The level, in dB, of `sum(bands)` (or of band `which`) at `freq`,
    /// measured with a steady sine.
    fn level_db(crossover: &mut Crossover, freq: f32, which: Option<usize>) -> f32 {
        crossover.reset();
        let len = (RATE * 0.5) as usize;
        let settle = len / 2;
        let mut peak = 0.0f32;
        for i in 0..len {
            let x = (i as f32 * freq * 2.0 * PI / RATE).sin();
            let bands = crossover.tick(0, x);
            let y = match which {
                Some(band) => bands[band],
                None => bands.iter().sum(),
            };
            if i >= settle {
                peak = peak.max(y.abs());
            }
        }
        20.0 * peak.max(1e-9).log10()
    }

    #[test]
    fn the_bands_add_up_flat_at_every_slope_and_count() {
        let freqs = [30.0, 120.0, 400.0, 1000.0, 3000.0, 8000.0, 15_000.0];
        for slope in [
            CrossoverSlope::Db12,
            CrossoverSlope::Db24,
            CrossoverSlope::Db48,
        ] {
            for points in [&[1000.0][..], &[250.0, 4000.0], &[200.0, 1500.0, 6000.0]] {
                let mut crossover = Crossover::new(RATE, slope, points);
                assert_eq!(crossover.bands(), points.len() + 1);
                for freq in freqs {
                    let db = level_db(&mut crossover, freq, None);
                    assert!(
                        db.abs() < 0.1,
                        "{slope:?} {points:?} at {freq} Hz: {db:.3} dB"
                    );
                }
            }
        }
    }

    #[test]
    fn each_band_keeps_its_own_range() {
        let mut crossover = Crossover::new(RATE, CrossoverSlope::Db24, &[300.0, 3000.0]);
        let low = |c: &mut Crossover, f| level_db(c, f, Some(0));
        let mid = |c: &mut Crossover, f| level_db(c, f, Some(1));
        let high = |c: &mut Crossover, f| level_db(c, f, Some(2));
        assert!(low(&mut crossover, 60.0) > -0.5);
        assert!(low(&mut crossover, 3000.0) < -35.0);
        assert!(mid(&mut crossover, 1000.0) > -1.5);
        assert!(mid(&mut crossover, 50.0) < -25.0);
        assert!(high(&mut crossover, 12_000.0) > -0.5);
        assert!(high(&mut crossover, 300.0) < -35.0);
        // -6 dB at a crossover, as Linkwitz-Riley filters are.
        assert!((low(&mut crossover, 300.0) + 6.0).abs() < 0.5);
        // The analog drawing agrees.
        let drawn = band_gain(CrossoverSlope::Db24, &[300.0, 3000.0], 0, 300.0);
        assert!((drawn - 0.5).abs() < 1e-3);
        let total: f32 = (0..3)
            .map(|b| band_gain(CrossoverSlope::Db24, &[300.0, 3000.0], b, 1000.0))
            .sum();
        assert!(total > 0.95 && total <= 1.0 + 1e-3);
    }

    #[test]
    fn frequencies_are_sorted_kept_apart_and_glide() {
        let mut crossover = Crossover::new(RATE, CrossoverSlope::Db24, &[5000.0, 100.0, f32::NAN]);
        assert!(crossover.targets[0] < crossover.targets[1]);
        assert!(crossover.targets[1] < crossover.targets[2]);
        assert!(crossover.targets[2] <= RATE * MAX_FREQ_RATIO);

        let mut crossover = Crossover::new(RATE, CrossoverSlope::Db24, &[200.0]);
        crossover.configure(CrossoverSlope::Db24, &[2000.0]);
        crossover.begin_block(64);
        assert!(crossover.freqs[0] > 200.0 && crossover.freqs[0] < 2000.0);
        for _ in 0..200 {
            crossover.begin_block(64);
        }
        assert_eq!(crossover.freqs[0], 2000.0);
        // A different count starts afresh, at the new frequencies.
        crossover.configure(CrossoverSlope::Db24, &[100.0, 1000.0]);
        assert_eq!(crossover.bands(), 3);
        assert_eq!(crossover.freqs[..2], [100.0, 1000.0]);
    }
}
