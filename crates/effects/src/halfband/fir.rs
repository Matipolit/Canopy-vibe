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

use dsp::simd::F32x4;

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
/// always one contiguous slice, oldest first: for the sample-at-a-time
/// filters the block ones are tested against.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct Line {
    data: Box<[f32]>,
    position: usize,
    len: usize,
}

#[cfg(test)]
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
}

/// `Σ a·b` over equal lengths that are a multiple of [`LANES`]: in twice
/// that many running sums, so the additions vectorize and enough of them
/// are in flight at once that the long branches don't wait on each other.
#[inline]
pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
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

/// Outputs [`convolve`] works out together: four vectors of four.
const GROUP: usize = 16;

/// `out[i] = dot(&x[i..i + taps.len()], taps)` for every `i`, to the bit:
/// each output keeps [`dot`]'s sixteen running sums, filled in the same
/// order and added up in the same tree. But sixteen neighbouring outputs go
/// together, a tap's coefficient multiplying sixteen inputs in a row, so
/// the vectors run along the outputs and nothing is summed across a vector
/// until the end. A block of a stream costs about what its multiplications
/// do, where one output at a time paid for a call, a horizontal sum and a
/// short loop each.
pub(crate) fn convolve(x: &[f32], taps: &[f32], out: &mut [f32]) {
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx") {
        // SAFETY: the processor has AVX.
        unsafe { convolve_avx(x, taps, out) };
        return;
    }
    convolve_sse(x, taps, out);
}

/// [`convolve`] four lanes at a time, for any processor.
fn convolve_sse(x: &[f32], taps: &[f32], out: &mut [f32]) {
    let len = taps.len();
    debug_assert!(len % LANES == 0);
    assert!(x.len() + 1 >= out.len() + len);
    let pairs = len / (2 * LANES);
    let rest = len % (2 * LANES) != 0;
    let groups = out.len() / GROUP;
    for group in 0..groups {
        let start = group * GROUP;
        let x = &x[start..start + GROUP + len - 1];
        let mut partial = [[F32x4::splat(0.0); GROUP / 4]; 2 * LANES];
        for (lane, partial) in partial.iter_mut().enumerate() {
            let mut sums = [F32x4::splat(0.0); GROUP / 4];
            for pair in 0..pairs {
                let k = 2 * LANES * pair + lane;
                let tap = F32x4::splat(taps[k]);
                for (v, sum) in sums.iter_mut().enumerate() {
                    *sum = *sum + F32x4::load(x, k + 4 * v) * tap;
                }
            }
            if rest && lane < LANES {
                let k = 2 * LANES * pairs + lane;
                let tap = F32x4::splat(taps[k]);
                for (v, sum) in sums.iter_mut().enumerate() {
                    *sum = *sum + F32x4::load(x, k + 4 * v) * tap;
                }
            }
            *partial = sums;
        }
        for v in 0..GROUP / 4 {
            let t: [F32x4; LANES] = std::array::from_fn(|l| partial[l][v] + partial[l + LANES][v]);
            let total = ((t[0] + t[1]) + (t[2] + t[3])) + ((t[4] + t[5]) + (t[6] + t[7]));
            out[start + 4 * v..start + 4 * v + 4].copy_from_slice(&total.to_array());
        }
    }
    for i in groups * GROUP..out.len() {
        out[i] = dot(&x[i..i + len], taps);
    }
}

/// [`convolve`] eight lanes at a time: the same sums in the same order (no
/// fused multiply-adds), so the same results as [`convolve_sse`] to the
/// bit, in about half the instructions. Thirty-two outputs at a time, so
/// each of the sixteen sums has four vectors to add into while the last
/// addition finishes (with sixteen outputs, two, and the additions waited
/// on each other); what's left over goes to [`convolve_sse`].
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
fn convolve_avx(x: &[f32], taps: &[f32], out: &mut [f32]) {
    use std::arch::x86_64::*;
    let len = taps.len();
    debug_assert!(len % LANES == 0);
    assert!(x.len() + 1 >= out.len() + len);
    const WIDE: usize = 2 * GROUP;
    let pairs = len / (2 * LANES);
    let rest = len % (2 * LANES) != 0;
    let groups = out.len() / WIDE;
    let load = |x: &[f32], at: usize| {
        let window = &x[at..at + 8];
        // SAFETY: the window is eight floats long.
        unsafe { _mm256_loadu_ps(window.as_ptr()) }
    };
    for group in 0..groups {
        let start = group * WIDE;
        let x = &x[start..start + WIDE + len - 1];
        let mut partial = [[_mm256_setzero_ps(); WIDE / 8]; 2 * LANES];
        for (lane, partial) in partial.iter_mut().enumerate() {
            let mut sums = [_mm256_setzero_ps(); WIDE / 8];
            let mut add = |k: usize| {
                let tap = _mm256_set1_ps(taps[k]);
                for (v, sum) in sums.iter_mut().enumerate() {
                    *sum = _mm256_add_ps(*sum, _mm256_mul_ps(load(x, k + 8 * v), tap));
                }
            };
            for pair in 0..pairs {
                add(2 * LANES * pair + lane);
            }
            if rest && lane < LANES {
                add(2 * LANES * pairs + lane);
            }
            *partial = sums;
        }
        for v in 0..WIDE / 8 {
            let t: [__m256; LANES] =
                std::array::from_fn(|l| _mm256_add_ps(partial[l][v], partial[l + LANES][v]));
            let total = _mm256_add_ps(
                _mm256_add_ps(_mm256_add_ps(t[0], t[1]), _mm256_add_ps(t[2], t[3])),
                _mm256_add_ps(_mm256_add_ps(t[4], t[5]), _mm256_add_ps(t[6], t[7])),
            );
            let window = &mut out[start + 8 * v..start + 8 * v + 8];
            // SAFETY: the window is eight floats long.
            unsafe { _mm256_storeu_ps(window.as_mut_ptr(), total) };
        }
    }
    let done = groups * WIDE;
    if done < out.len() {
        convolve_sse(&x[done..], taps, &mut out[done..]);
    }
}

/// The last `keep` samples of a stream followed by room for a block of new
/// ones, all in one slice, so every window a filter reads over the block is
/// a plain subslice. After the block, its newest `keep` samples move to the
/// front: a copy of a filter's length per block, where a ring paid two
/// writes and a wrap test per sample.
#[derive(Debug, Clone, Default)]
pub(crate) struct History {
    data: Box<[f32]>,
    keep: usize,
}

impl History {
    /// Room for `keep` samples of history and blocks of up to `block`.
    /// Allocates: not for the audio thread.
    pub fn new(keep: usize, block: usize) -> History {
        History {
            data: vec![0.0; keep + block].into_boxed_slice(),
            keep,
        }
    }

    /// Where the next `len` samples go.
    #[inline]
    pub fn new_samples(&mut self, len: usize) -> &mut [f32] {
        &mut self.data[self.keep..self.keep + len]
    }

    /// The history, oldest first, then the `len` new samples.
    #[inline]
    pub fn with_new(&self, len: usize) -> &[f32] {
        &self.data[..self.keep + len]
    }

    /// Ends a block of `len` new samples.
    #[inline]
    pub fn advance(&mut self, len: usize) {
        self.data.copy_within(len..len + self.keep, 0);
    }

    pub fn clear(&mut self) {
        self.data.fill(0.0);
    }
}

/// [`Up`] a block at a time, one channel.
#[derive(Debug, Clone)]
pub(crate) struct UpBlock {
    history: History,
}

impl UpBlock {
    /// For blocks of up to `block` samples. Allocates: not for the audio
    /// thread.
    pub fn new(design: &Design, block: usize) -> UpBlock {
        debug_assert!(design.odd.is_none(), "upsampling takes a halfband");
        UpBlock {
            history: History::new(design.taps() - 1, block),
        }
    }

    /// `x` at twice the rate into `out` (twice as long), as [`Up::push`]
    /// gives it sample by sample; `scratch` is as long as `x`.
    pub fn run(&mut self, design: &Design, x: &[f32], out: &mut [f32], scratch: &mut [f32]) {
        let len = x.len();
        self.history.new_samples(len).copy_from_slice(x);
        let line = self.history.with_new(len);
        let even = &mut scratch[..len];
        convolve(line, &design.branch, even);
        // The pass-through branch: `Line::get((half - 1) / 2)` of each window.
        let through = &line[design.taps() - 1 - (design.half - 1) / 2..][..len];
        for ((pair, &even), &odd) in out
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(even.iter())
            .zip(through)
        {
            *pair = [2.0 * even, odd];
        }
        self.history.advance(len);
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }
}

/// [`Down`] a block at a time, one channel.
#[derive(Debug, Clone)]
pub(crate) struct DownBlock {
    early: History,
    late: History,
}

impl DownBlock {
    /// For blocks of up to `block` output samples. Allocates: not for the
    /// audio thread.
    pub fn new(design: &Design, block: usize) -> DownBlock {
        let late = match &design.odd {
            // What `Down`'s late line holds before each push, the tails the
            // odd branch reads and, after it, the even branch's window.
            Some(_) => design.taps(),
            None => design.half.div_ceil(2),
        };
        DownBlock {
            early: History::new(design.taps() - 1, block),
            late: History::new(late, block),
        }
    }

    /// `x` (pairs, earlier first) at half the rate into `out`, as
    /// [`Down::push`] gives it; with `halves`, also each output's value half
    /// a sample on into it, as [`Down::push_both`] does (only for a lowpass
    /// design). `scratch` is as long as `out`.
    pub fn run(
        &mut self,
        design: &Design,
        x: &[f32],
        out: &mut [f32],
        halves: Option<&mut [f32]>,
        scratch: &mut [f32],
    ) {
        let len = out.len();
        debug_assert_eq!(x.len(), 2 * len);
        let pairs = x.as_chunks::<2>().0;
        for ((early, late), &[a, b]) in self
            .early
            .new_samples(len)
            .iter_mut()
            .zip(self.late.new_samples(len).iter_mut())
            .zip(pairs)
        {
            *early = a;
            *late = b;
        }
        let taps = design.taps();
        let early = self.early.with_new(len);
        let late = self.late.with_new(len);
        convolve(early, &design.branch, out);
        match &design.odd {
            Some(odd) => {
                // The odd branch over the late samples before each pair's.
                let tail = late.len() - len - odd.len();
                let scratch = &mut scratch[..len];
                convolve(&late[tail..], odd, scratch);
                for (out, &odd) in out.iter_mut().zip(scratch.iter()) {
                    *out += odd;
                }
                if let Some(halves) = halves {
                    // The even branch over the late samples up to each
                    // pair's, the odd over the early ones.
                    let halves = &mut halves[..len];
                    convolve(&late[1..], &design.branch, halves);
                    convolve(&early[taps - odd.len()..], odd, scratch);
                    for (half, &odd) in halves.iter_mut().zip(scratch.iter()) {
                        *half += odd;
                    }
                }
            }
            None => {
                // `0.5 * late.get((half - 1) / 2)` before each push: the
                // oldest sample the late line held.
                for (out, &late) in out.iter_mut().zip(late) {
                    *out += 0.5 * late;
                }
                if let Some(halves) = halves {
                    halves[..len].copy_from_slice(out);
                }
            }
        }
        self.early.advance(len);
        self.late.advance(len);
    }

    pub fn clear(&mut self) {
        self.early.clear();
        self.late.clear();
    }
}

/// Upsampling by two, one channel, a sample at a time: what [`UpBlock`] is
/// tested against.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct Up {
    line: Line,
}

#[cfg(test)]
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
}

/// Downsampling by two, one channel, a sample at a time: what
/// [`DownBlock`] is tested against.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct Down {
    /// The first sample of each pair, through the even branch.
    early: Line,
    /// The second, through the odd branch (or a halfband's centre tap).
    late: Line,
}

#[cfg(test)]
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A signal with everything in it: noise, loud and quiet.
    fn signal(len: usize) -> Vec<f32> {
        let mut state = 0x1234_5678u32;
        (0..len)
            .map(|n| {
                let noise = dsp::xorshift32(&mut state) as f32 / u32::MAX as f32 - 0.5;
                noise * if n % 500 < 250 { 3.0 } else { 0.01 }
            })
            .collect()
    }

    #[test]
    fn a_block_at_a_time_is_a_sample_at_a_time_to_the_bit() {
        let input = signal(3000);
        let halfband = Design::halfband(67, 104.0);
        let short = Design::halfband(17, 104.0);
        let steep = Design::lowpass(133, 0.2375, 104.0);
        for block in [1, 7, 16, 64, 100] {
            // Up.
            for design in [&halfband, &short] {
                let mut one = Up::new(design);
                let expected: Vec<f32> = input.iter().flat_map(|&x| one.push(design, x)).collect();
                let mut many = UpBlock::new(design, block);
                let mut got = Vec::new();
                let (mut out, mut scratch) = (vec![0.0; 2 * block], vec![0.0; block]);
                for x in input.chunks(block) {
                    many.run(design, x, &mut out[..2 * x.len()], &mut scratch);
                    got.extend_from_slice(&out[..2 * x.len()]);
                }
                assert!(
                    expected
                        .iter()
                        .map(|x| x.to_bits())
                        .eq(got.iter().map(|x| x.to_bits())),
                    "up, half-length {}, blocks of {block}",
                    design.half
                );
            }
            // Down, with and without the values half a sample on.
            for design in [&halfband, &short, &steep] {
                for both in [false, true] {
                    let mut one = Down::new(design);
                    let expected: Vec<f32> = input
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .flat_map(|&[a, b]| {
                            if both {
                                one.push_both(design, a, b)
                            } else {
                                [one.push(design, a, b), 0.0]
                            }
                        })
                        .collect();
                    let mut many = DownBlock::new(design, block);
                    let mut got = Vec::new();
                    let mut out = vec![0.0; block];
                    let mut halves = vec![0.0; block];
                    let mut scratch = vec![0.0; block];
                    for x in input.chunks(2 * block) {
                        let len = x.len() / 2;
                        many.run(
                            design,
                            &x[..2 * len],
                            &mut out[..len],
                            both.then_some(&mut halves[..len]),
                            &mut scratch,
                        );
                        for i in 0..len {
                            got.extend([out[i], if both { halves[i] } else { 0.0 }]);
                        }
                    }
                    assert!(
                        expected
                            .iter()
                            .map(|x| x.to_bits())
                            .eq(got.iter().map(|x| x.to_bits())),
                        "down, half-length {}, halves {both}, blocks of {block}",
                        design.half
                    );
                }
            }
        }
    }

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
