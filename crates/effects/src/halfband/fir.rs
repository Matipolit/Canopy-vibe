//! Linear-phase halfband filters for oversampling by two, for the clipper:
//! Kaiser-windowed sincs whose every other tap is zero, run as two polyphase
//! branches.
//!
//! A halfband lowpass `h` of `2c + 1` taps (`c` odd) has `h[c] = ½` and
//! zeros at every even distance from the centre, so upsampling (zeros
//! stuffed between the samples, then `2h`) costs one branch of `c + 1` taps
//! per input sample, the other output being the input itself `(c - 1) / 2`
//! samples back; downsampling costs the same branch per output sample plus
//! one multiply. Unlike the IIR halfbands in `creative_halfband.rs`, these
//! delay every frequency alike, so up and back down is a plain delay of `c`
//! samples at the lower rate (to within the pass band's ripple, far below
//! a millionth): the clipper can report its latency exactly, and what it
//! doesn't clip comes out as it went in, only later.

/// The longest half-length a design may have.
const MAX_HALF: usize = 127;
/// Branches are padded with leading zeros to a multiple of this, so their
/// dot products run in whole vector lanes.
const LANES: usize = 8;

/// A linear-phase lowpass `h` of `2c + 1` taps (`c` odd), split into its
/// two polyphase branches.
#[derive(Debug, Clone)]
pub(crate) struct Design {
    /// `h[0], h[2], …, h[2c]`, zero-padded at the front to a multiple of
    /// [`LANES`]. Symmetric, so it is also the branch in oldest-first order,
    /// lined up with a [`Line`]'s window.
    branch: Box<[f32]>,
    /// `h[1], h[3], …, h[2c - 1]`, the same way; `None` for a halfband,
    /// whose only odd tap is its centre, `h[c] = ½`.
    odd: Option<Box<[f32]>>,
    /// The half-length `c`: the delay in samples of the higher rate.
    half: usize,
}

/// The modified Bessel function I₀, by its series.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..60 {
        term *= x / (2.0 * k as f64);
        sum += term * term;
        if term * term < sum * 1e-18 {
            break;
        }
    }
    sum
}

/// The Kaiser window's value `offset` taps from the centre of a window
/// reaching `width` either way, for shape `beta`.
fn kaiser(offset: f64, width: f64, beta: f64) -> f64 {
    let r = offset / width;
    bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(beta)
}

/// `taps` as a branch: zero-padded at the front to whole lanes.
fn branch(taps: &[f64]) -> Box<[f32]> {
    let padded = taps.len().div_ceil(LANES) * LANES;
    let mut branch = vec![0.0f32; padded];
    for (slot, tap) in branch[padded - taps.len()..].iter_mut().zip(taps) {
        *slot = *tap as f32;
    }
    branch.into_boxed_slice()
}

/// The Kaiser window's shape for a stop band `attenuation` dB down.
fn beta(attenuation: f64) -> f64 {
    0.1102 * (attenuation - 8.7).max(0.0)
}

impl Design {
    /// A halfband of half-length `half` (made odd), under a Kaiser window
    /// shaped for a stop band `attenuation` dB down: how wide its transition
    /// band comes out depends on the length. Allocates: not for the audio
    /// thread.
    pub fn halfband(half: usize, attenuation: f64) -> Design {
        let half = half.clamp(1, MAX_HALF) | 1;
        let beta = beta(attenuation);
        // h[c + m] for odd m from -c to c: ½·sinc(m/2) under the window.
        let mut taps: Vec<f64> = (0..=half)
            .map(|i| {
                let m = 2.0 * i as f64 - half as f64;
                let sinc = (std::f64::consts::FRAC_PI_2 * m).sin() / (std::f64::consts::PI * m);
                sinc * kaiser(m, half as f64 + 1.0, beta)
            })
            .collect();
        // Each branch passes DC at exactly half, so a constant goes up and
        // down unchanged.
        let sum: f64 = taps.iter().sum();
        for tap in &mut taps {
            *tap *= 0.5 / sum;
        }
        Design {
            branch: branch(&taps),
            odd: None,
            half,
        }
    }

    /// A lowpass of half-length `half` (made odd) cutting off at `cutoff`
    /// (a fraction of the higher rate), under a Kaiser window shaped for a
    /// stop band `attenuation` dB down. Only for downsampling. Allocates:
    /// not for the audio thread.
    pub fn lowpass(half: usize, cutoff: f64, attenuation: f64) -> Design {
        let half = half.clamp(1, 2 * MAX_HALF) | 1;
        let beta = beta(attenuation);
        let taps: Vec<f64> = (0..=2 * half)
            .map(|j| {
                let t = j as f64 - half as f64;
                let x = std::f64::consts::TAU * cutoff * t;
                let sinc = if t == 0.0 { 1.0 } else { x.sin() / x };
                2.0 * cutoff * sinc * kaiser(t, half as f64 + 1.0, beta)
            })
            .collect();
        // Unity gain at DC.
        let sum: f64 = taps.iter().sum();
        let even: Vec<f64> = taps.iter().step_by(2).map(|t| t / sum).collect();
        let odd: Vec<f64> = taps.iter().skip(1).step_by(2).map(|t| t / sum).collect();
        Design {
            branch: branch(&even),
            odd: Some(branch(&odd)),
            half,
        }
    }

    /// The filter's gain at `freq` (a fraction of the higher rate), from
    /// the taps as they are used: real, since the filter is symmetric.
    #[cfg(test)]
    pub fn response(&self, freq: f64) -> f64 {
        let half = self.half as f64;
        let at = |j: usize| (std::f64::consts::TAU * freq * (j as f64 - half)).cos();
        // Branch tap `i` from the end is filter tap `2i` (or `2i + 1`).
        let sum_of = |branch: &[f32], first: usize, count: usize| -> f64 {
            branch[branch.len() - count..]
                .iter()
                .enumerate()
                .map(|(i, &tap)| f64::from(tap) * at(first + 2 * i))
                .sum()
        };
        let even = sum_of(&self.branch, 0, self.half + 1);
        match &self.odd {
            None => even + 0.5,
            Some(odd) => even + sum_of(odd, 1, self.half),
        }
    }

    /// The length of the history a branch reads.
    fn taps(&self) -> usize {
        self.branch.len()
    }
}

/// The last few samples of a stream, written twice over so that they are
/// always one contiguous slice, oldest first.
#[derive(Debug, Clone, Default)]
pub(crate) struct Line {
    data: Box<[f32]>,
    position: usize,
    len: usize,
}

impl Line {
    /// Allocates: not for the audio thread.
    pub fn new(len: usize) -> Line {
        let len = len.max(1);
        Line {
            data: vec![0.0; 2 * len].into_boxed_slice(),
            position: 0,
            len,
        }
    }

    #[inline]
    pub fn push(&mut self, x: f32) {
        self.data[self.position] = x;
        self.data[self.position + self.len] = x;
        self.position += 1;
        if self.position == self.len {
            self.position = 0;
        }
    }

    /// The samples kept, oldest first; the newest is last.
    #[inline]
    pub fn window(&self) -> &[f32] {
        &self.data[self.position..self.position + self.len]
    }

    /// The sample pushed `age` pushes before the newest (0 is the newest).
    #[inline]
    pub fn get(&self, age: usize) -> f32 {
        self.data[self.position + self.len - 1 - age]
    }

    pub fn clear(&mut self) {
        self.data.fill(0.0);
        self.position = 0;
    }
}

/// `Σ a·b` over equal lengths that are a multiple of [`LANES`]: in twice
/// that many running sums, so the additions vectorize and enough of them
/// are in flight at once that the long branches don't wait on each other.
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut sums = [[0.0f32; LANES]; 2];
    let (pairs, rest) = a.as_chunks::<{ 2 * LANES }>();
    let (other_pairs, other_rest) = b.as_chunks::<{ 2 * LANES }>();
    for (a, b) in pairs.iter().zip(other_pairs) {
        for lane in 0..2 * LANES {
            sums[lane / LANES][lane % LANES] += a[lane] * b[lane];
        }
    }
    for (a, b) in rest
        .as_chunks::<LANES>()
        .0
        .iter()
        .zip(other_rest.as_chunks::<LANES>().0)
    {
        for lane in 0..LANES {
            sums[0][lane] += a[lane] * b[lane];
        }
    }
    let mut total = [0.0f32; LANES];
    for lane in 0..LANES {
        total[lane] = sums[0][lane] + sums[1][lane];
    }
    let [a, b, c, d, e, f, g, h] = total;
    ((a + b) + (c + d)) + ((e + f) + (g + h))
}

/// Upsampling by two, one channel.
#[derive(Debug, Clone)]
pub(crate) struct Up {
    line: Line,
}

impl Up {
    pub fn new(design: &Design) -> Up {
        debug_assert!(design.odd.is_none(), "upsampling takes a halfband");
        Up {
            line: Line::new(design.taps()),
        }
    }

    /// One sample in, the two that replace it at twice the rate out, by a
    /// halfband `design`.
    #[inline]
    pub fn push(&mut self, design: &Design, x: f32) -> [f32; 2] {
        self.line.push(x);
        let even = 2.0 * dot(self.line.window(), &design.branch);
        let odd = self.line.get((design.half - 1) / 2);
        [even, odd]
    }

    pub fn clear(&mut self) {
        self.line.clear();
    }
}

/// Downsampling by two, one channel.
#[derive(Debug, Clone)]
pub(crate) struct Down {
    /// The first sample of each pair, through the even branch.
    early: Line,
    /// The second, through the odd branch (or a halfband's centre tap).
    late: Line,
}

impl Down {
    pub fn new(design: &Design) -> Down {
        let late = match &design.odd {
            // Long enough for the even branch too, for `push_both`.
            Some(_) => design.taps(),
            None => design.half.div_ceil(2),
        };
        Down {
            early: Line::new(design.taps()),
            late: Line::new(late),
        }
    }

    /// The newest `len` samples of `line`, oldest first.
    #[inline]
    fn tail(line: &Line, len: usize) -> &[f32] {
        let window = line.window();
        &window[window.len() - len..]
    }

    /// Two samples in (earlier first), one out at half the rate. The output
    /// lines up with the first: `h[j]` weighs the sample `j` before it.
    #[inline]
    pub fn push(&mut self, design: &Design, early: f32, late: f32) -> f32 {
        self.early.push(early);
        let even = dot(self.early.window(), &design.branch);
        // The second samples of earlier pairs only.
        let odd = match &design.odd {
            Some(odd) => dot(Down::tail(&self.late, odd.len()), odd),
            None => 0.5 * self.late.get((design.half - 1) / 2),
        };
        self.late.push(late);
        even + odd
    }

    /// Like [`Down::push`], and also the filtered signal at the second
    /// sample: the output's value half a sample on, as a band-limited
    /// reconstruction would have it (exactly, where the filter stops
    /// everything above the lower rate's Nyquist). Only for a lowpass
    /// design; a halfband gives the first output twice.
    #[inline]
    pub fn push_both(&mut self, design: &Design, early: f32, late: f32) -> [f32; 2] {
        let Some(odd) = &design.odd else {
            let out = self.push(design, early, late);
            return [out, out];
        };
        self.early.push(early);
        let first =
            dot(self.early.window(), &design.branch) + dot(Down::tail(&self.late, odd.len()), odd);
        self.late.push(late);
        let second =
            dot(self.late.window(), &design.branch) + dot(Down::tail(&self.early, odd.len()), odd);
        [first, second]
    }

    pub fn clear(&mut self) {
        self.early.clear();
        self.late.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gain of downsampling a sine at `cycles / 8000` of the higher
    /// rate (a whole number of cycles in the 4000 samples measured), from
    /// its RMS once the filter has filled.
    fn through_downsampler(design: &Design, cycles: usize) -> f64 {
        let mut filter = Down::new(design);
        let freq = cycles as f64 / 8000.0;
        let len = 8000;
        let mut power = 0.0f64;
        for n in 0..len {
            let s = |t: usize| (std::f64::consts::TAU * freq * t as f64).sin() as f32;
            let out = filter.push(design, s(2 * n), s(2 * n + 1));
            if n >= len / 2 {
                power += f64::from(out).powi(2);
            }
        }
        (2.0 * power / (len / 2) as f64).sqrt()
    }

    #[test]
    fn the_filters_do_what_the_response_says() {
        let designs = [
            Design::halfband(9, 100.0),
            Design::halfband(65, 100.0),
            Design::lowpass(41, 0.2, 100.0),
            Design::lowpass(129, 0.2375, 100.0),
        ];
        for design in &designs {
            for cycles in [40, 700, 1500, 1990, 2010, 2600, 3500, 3990] {
                let measured = through_downsampler(design, cycles);
                // Below a quarter it's the pass band; above, what's left
                // is aliasing, folded below it.
                let expected = design.response(cycles as f64 / 8000.0).abs();
                assert!(
                    (measured - expected).abs() < 1e-5 + 1e-3 * expected,
                    "half-length {}, {cycles}: {measured} against {expected}",
                    design.half
                );
            }
        }
    }

    /// The largest difference between `signal` after up and down and
    /// `signal` itself, `delay` samples late.
    fn round_trip_error(up_design: &Design, down_design: &Design, delay: usize) -> f32 {
        let (mut up, mut down) = (Up::new(up_design), Down::new(down_design));
        // A band-limited signal: a few sines below 0.45 of the low rate.
        let signal: Vec<f32> = (0..4000)
            .map(|n| {
                let t = n as f64;
                ((0.013 * t).sin() + 0.5 * (1.7 * t).sin() + 0.25 * (2.75 * t + 1.0).sin()) as f32
                    / 1.75
            })
            .collect();
        let mut worst = 0.0f32;
        for (n, &x) in signal.iter().enumerate() {
            let [a, b] = up.push(up_design, x);
            let y = down.push(down_design, a, b);
            if n >= delay + 300 {
                worst = worst.max((y - signal[n - delay]).abs());
            }
        }
        worst
    }

    #[test]
    fn up_and_down_is_a_plain_delay() {
        let halfband = Design::halfband(65, 100.0);
        let worst = round_trip_error(&halfband, &halfband, 65);
        println!("halfbands up and down, 65 samples late: off by at most {worst:e}");
        assert!(worst < 1e-4, "{worst}");
        // A steeper lowpass down: (65 + 129) / 2 samples.
        let steep = Design::lowpass(129, 0.2375, 100.0);
        let worst = round_trip_error(&halfband, &steep, 97);
        println!("halfband up, lowpass down, 97 samples late: off by at most {worst:e}");
        assert!(worst < 1e-4, "{worst}");
        // The steep filter's second output is the signal half a sample on.
        let wave = |t: f64| {
            ((0.013 * t).sin() + 0.5 * (1.7 * t).sin() + 0.25 * (2.75 * t + 1.0).sin()) / 1.75
        };
        let (mut up, mut down) = (Up::new(&halfband), Down::new(&steep));
        let mut worst = 0.0f64;
        for n in 0..4000 {
            let [a, b] = up.push(&halfband, wave(n as f64) as f32);
            let [now, half] = down.push_both(&steep, a, b);
            if n >= 400 {
                let t = (n - 97) as f64;
                worst = worst
                    .max((f64::from(now) - wave(t)).abs())
                    .max((f64::from(half) - wave(t + 0.5)).abs());
            }
        }
        println!("the steep filter between samples: off by at most {worst:e}");
        assert!(worst < 1e-4, "{worst}");
        // A constant goes through exactly.
        for down_design in [&halfband, &steep] {
            let (mut up, mut down) = (Up::new(&halfband), Down::new(down_design));
            let mut y = 0.0;
            for _ in 0..500 {
                let [a, b] = up.push(&halfband, 0.5);
                y = down.push(down_design, a, b);
            }
            assert!((y - 0.5).abs() < 1e-6, "{y}");
        }
    }
}
