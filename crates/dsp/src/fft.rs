//! An in-place radix-2 FFT of one size, on separate real and imaginary
//! arrays: the convolver's block transforms and the wavetable's morphed
//! frames.
//!
//! Each stage's twiddles are kept one after another, so every pass from the
//! third on is plain arithmetic over contiguous slices, four butterflies at
//! a time ([`F32x4`]); the first two, whose twiddles are 1 and ∓j, need no
//! multiplications. Read from one table of `n / 2` with a stride, the
//! twiddles had kept every pass scalar and the first passes were all loop
//! overhead: about 34 instructions a butterfly.

use crate::simd::F32x4;

#[derive(Debug, Clone, Default)]
pub struct Fft {
    n: usize,
    /// For each stage from `half = 4` on, its `half` cosines then its
    /// `half` sines, `cos(2πk/size)` and `sin(2πk/size)` for `k < half`.
    stages: Vec<f32>,
    /// The index swaps of the bit-reversal permutation, `i < j`.
    swaps: Vec<(u32, u32)>,
}

impl Fft {
    /// A transform of `n` points (a power of two), its twiddles worked out
    /// in double precision. Allocates: not for the audio thread.
    pub fn new(n: usize) -> Fft {
        Fft::with_twiddles(n, |k, size| {
            let angle = std::f64::consts::TAU * k as f64 / size as f64;
            (angle.cos() as f32, angle.sin() as f32)
        })
    }

    /// A transform of `n` points (a power of two), with `twiddle(k, size)`
    /// giving `cos(2πk/size)` and `sin(2πk/size)` for each stage's `size`
    /// from 8 to `n` and each `k < size / 2`, so a caller can keep the
    /// exact values a table of its own has. Allocates.
    pub fn with_twiddles(n: usize, twiddle: impl Fn(usize, usize) -> (f32, f32)) -> Fft {
        assert!(n.is_power_of_two(), "{n} points");
        let bits = n.trailing_zeros();
        let mut stages = Vec::new();
        let mut size = 8;
        while size <= n {
            let half = size / 2;
            let (cos, sin): (Vec<f32>, Vec<f32>) = (0..half).map(|k| twiddle(k, size)).unzip();
            stages.extend(cos);
            stages.extend(sin);
            size *= 2;
        }
        let swaps = (0..n as u32)
            .filter_map(|i| {
                let j = if bits == 0 {
                    0
                } else {
                    i.reverse_bits() >> (32 - bits)
                };
                (j > i).then_some((i, j))
            })
            .collect();
        Fft { n, stages, swaps }
    }

    /// Points transformed.
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// The transform of `re + j·im` (`e^{-2πikn/N}`, or with `inverse`
    /// `e^{+2πikn/N}`), in place, unscaled. Both are at least
    /// [`Fft::len`] long; only that much is transformed.
    pub fn transform(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let n = self.n;
        let (re, im) = (&mut re[..n], &mut im[..n]);
        for &(i, j) in &self.swaps {
            re.swap(i as usize, j as usize);
            im.swap(i as usize, j as usize);
        }
        // Size 2: twiddle 1.
        if n >= 2 {
            for (re, im) in re.as_chunks_mut::<2>().0.iter_mut().zip(im.as_chunks_mut::<2>().0) {
                let ([a, b], [c, d]) = (*re, *im);
                *re = [a + b, a - b];
                *im = [c + d, c - d];
            }
        }
        // Size 4: twiddles 1 and ∓j (forward and inverse).
        if n >= 4 {
            for (re, im) in re.as_chunks_mut::<4>().0.iter_mut().zip(im.as_chunks_mut::<4>().0) {
                let (tr, ti) = if inverse {
                    (-im[3], re[3])
                } else {
                    (im[3], -re[3])
                };
                let [r0, r1, r2, _] = *re;
                let [i0, i1, i2, _] = *im;
                *re = [r0 + r2, r1 + tr, r0 - r2, r1 - tr];
                *im = [i0 + i2, i1 + ti, i0 - i2, i1 - ti];
            }
        }
        let sign = F32x4::splat(if inverse { 1.0 } else { -1.0 });
        let mut size = 8;
        let mut twiddles = &self.stages[..];
        while size <= n {
            let half = size / 2;
            let (cos, rest) = twiddles.split_at(half);
            let (sin, rest) = rest.split_at(half);
            twiddles = rest;
            for (re, im) in re.chunks_exact_mut(size).zip(im.chunks_exact_mut(size)) {
                let (re_a, re_b) = re.split_at_mut(half);
                let (im_a, im_b) = im.split_at_mut(half);
                for k in (0..half).step_by(4) {
                    let (c, s) = (F32x4::load(cos, k), sign * F32x4::load(sin, k));
                    let (br, bi) = (F32x4::load(re_b, k), F32x4::load(im_b, k));
                    let (ar, ai) = (F32x4::load(re_a, k), F32x4::load(im_a, k));
                    let tr = br * c - bi * s;
                    let ti = br * s + bi * c;
                    (ar - tr).store(re_b, k);
                    (ai - ti).store(im_b, k);
                    (ar + tr).store(re_a, k);
                    (ai + ti).store(im_a, k);
                }
            }
            size *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transform of `re + j·im` straight from its definition.
    fn dft(re: &[f32], im: &[f32], inverse: bool) -> (Vec<f64>, Vec<f64>) {
        let n = re.len();
        let sign = if inverse { 1.0 } else { -1.0 };
        (0..n)
            .map(|k| {
                (0..n).fold((0.0, 0.0), |(sr, si), t| {
                    let angle = sign * std::f64::consts::TAU * (k * t % n) as f64 / n as f64;
                    let (c, s) = (angle.cos(), angle.sin());
                    let (x, y) = (f64::from(re[t]), f64::from(im[t]));
                    (sr + x * c - y * s, si + x * s + y * c)
                })
            })
            .unzip()
    }

    #[test]
    fn every_size_is_the_dft_both_ways() {
        let mut state = 1u32;
        let mut noise = || crate::xorshift32(&mut state) as f32 / u32::MAX as f32 - 0.5;
        for bits in 0..=9 {
            let n = 1 << bits;
            let fft = Fft::new(n);
            for inverse in [false, true] {
                let re: Vec<f32> = (0..n).map(|_| noise()).collect();
                let im: Vec<f32> = (0..n).map(|_| noise()).collect();
                let (expected_re, expected_im) = dft(&re, &im, inverse);
                let (mut got_re, mut got_im) = (re.clone(), im.clone());
                fft.transform(&mut got_re, &mut got_im, inverse);
                for k in 0..n {
                    let error = (f64::from(got_re[k]) - expected_re[k])
                        .abs()
                        .max((f64::from(got_im[k]) - expected_im[k]).abs());
                    assert!(error < 1e-5 * (n as f64).sqrt(), "{n} points, bin {k}: {error}");
                }
            }
        }
    }
}
