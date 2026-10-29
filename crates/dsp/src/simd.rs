//! [`F32x4`]: four `f32`s worked on at once, for running several
//! independent recursive filters side by side (a guitar's strings, say).
//! A recursive filter can't be vectorized along time, since each sample
//! waits on the last, but four of them can be along the other axis, at the
//! cost of one.
//!
//! On x86-64 it is an SSE register (SSE2 is part of the architecture, so
//! there is nothing to detect); elsewhere an array the compiler vectorizes
//! as it can. Every operation is the IEEE one, lane by lane, so a lane
//! computes exactly what the same arithmetic on plain `f32`s would.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// Four `f32` lanes.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct F32x4(
    #[cfg(target_arch = "x86_64")] __m128,
    #[cfg(not(target_arch = "x86_64"))] [f32; 4],
);

/// Four lane masks, all ones or all zeros, from a comparison or
/// [`Mask4::from_bools`].
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct Mask4(F32x4);

#[cfg(target_arch = "x86_64")]
impl F32x4 {
    #[inline(always)]
    pub fn splat(x: f32) -> F32x4 {
        // SAFETY: SSE2 is always available on x86-64.
        F32x4(unsafe { _mm_set1_ps(x) })
    }

    /// From four values, put together in registers (a vector load of four
    /// values just stored one by one would wait for the stores to land).
    #[inline(always)]
    pub fn from_array(a: [f32; 4]) -> F32x4 {
        // SAFETY: SSE.
        F32x4(unsafe { _mm_set_ps(a[3], a[2], a[1], a[0]) })
    }

    #[inline(always)]
    pub fn to_array(self) -> [f32; 4] {
        let mut a = [0.0; 4];
        // SAFETY: writes four floats to an array of four.
        unsafe { _mm_storeu_ps(a.as_mut_ptr(), self.0) };
        a
    }

    /// The four floats starting at `slice[start]`.
    #[inline(always)]
    pub fn load(slice: &[f32], start: usize) -> F32x4 {
        let window = &slice[start..start + 4];
        // SAFETY: the window is four floats long.
        F32x4(unsafe { _mm_loadu_ps(window.as_ptr()) })
    }

    /// The four floats at `ptr`, which must be valid for reading them.
    ///
    /// # Safety
    /// `ptr..ptr + 4` must be readable floats.
    #[inline(always)]
    pub unsafe fn load_ptr(ptr: *const f32) -> F32x4 {
        // SAFETY: the caller's promise.
        F32x4(unsafe { _mm_loadu_ps(ptr) })
    }

    /// Writes the four lanes to `ptr`, which must be valid for writing them.
    ///
    /// # Safety
    /// `ptr..ptr + 4` must be writable floats.
    #[inline(always)]
    pub unsafe fn store_ptr(self, ptr: *mut f32) {
        // SAFETY: the caller's promise.
        unsafe { _mm_storeu_ps(ptr, self.0) }
    }

    /// The lanes truncated towards zero, as integers, and those as floats
    /// again: `x.trunc()` for values within `i32`'s range.
    #[inline(always)]
    pub fn trunc_i32(self) -> ([i32; 4], F32x4) {
        // SAFETY: SSE2.
        unsafe {
            let whole = _mm_cvttps_epi32(self.0);
            let mut out = [0i32; 4];
            _mm_storeu_si128(out.as_mut_ptr().cast(), whole);
            (out, F32x4(_mm_cvtepi32_ps(whole)))
        }
    }

    /// The larger of each pair; `other` where `self` is NaN, as `f32::max`
    /// does when `other` isn't NaN.
    #[inline(always)]
    pub fn max(self, other: F32x4) -> F32x4 {
        // SAFETY: SSE2. `maxps` returns its second operand when either is
        // NaN, so `other` goes second.
        F32x4(unsafe { _mm_max_ps(self.0, other.0) })
    }

    /// The smaller of each pair; `other` where `self` is NaN.
    #[inline(always)]
    pub fn min_of(self, other: F32x4) -> F32x4 {
        // SAFETY: SSE2. `minps` returns its second operand when either is
        // NaN.
        F32x4(unsafe { _mm_min_ps(self.0, other.0) })
    }

    #[inline(always)]
    pub fn greater_than(self, other: F32x4) -> Mask4 {
        // SAFETY: SSE2.
        Mask4(F32x4(unsafe { _mm_cmpgt_ps(self.0, other.0) }))
    }

    #[inline(always)]
    pub fn greater_equal(self, other: F32x4) -> Mask4 {
        // SAFETY: SSE2.
        Mask4(F32x4(unsafe { _mm_cmpge_ps(self.0, other.0) }))
    }

    #[inline(always)]
    pub fn equal(self, other: F32x4) -> Mask4 {
        // SAFETY: SSE2.
        Mask4(F32x4(unsafe { _mm_cmpeq_ps(self.0, other.0) }))
    }

    /// `2^(n - bias)` for the whole number `n` in each lane's low bits (as
    /// the integer its bits make), for [`exp2`].
    #[inline(always)]
    fn exponent_from_low_bits(self, bias: i32) -> F32x4 {
        // SAFETY: SSE2.
        unsafe {
            let n = _mm_sub_epi32(_mm_castps_si128(self.0), _mm_set1_epi32(bias));
            F32x4(_mm_castsi128_ps(_mm_slli_epi32::<23>(n)))
        }
    }

    /// Each lane's binary exponent, as a float, and its mantissa in `1..2`,
    /// for [`log2`].
    #[inline(always)]
    fn split_exponent(self) -> (F32x4, F32x4) {
        // SAFETY: SSE2.
        unsafe {
            let bits = _mm_castps_si128(self.0);
            let biased = _mm_and_si128(_mm_srli_epi32::<23>(bits), _mm_set1_epi32(0xff));
            let exponent = _mm_cvtepi32_ps(_mm_sub_epi32(biased, _mm_set1_epi32(127)));
            let mantissa = _mm_or_si128(
                _mm_and_si128(bits, _mm_set1_epi32(0x007f_ffff)),
                _mm_set1_epi32(0x3f80_0000),
            );
            (F32x4(exponent), F32x4(_mm_castsi128_ps(mantissa)))
        }
    }

    /// `if mask { a } else { b }`, lane by lane.
    #[inline(always)]
    pub fn select(mask: Mask4, a: F32x4, b: F32x4) -> F32x4 {
        let m = mask.0.0;
        // SAFETY: SSE2.
        F32x4(unsafe { _mm_or_ps(_mm_and_ps(m, a.0), _mm_andnot_ps(m, b.0)) })
    }

    /// Transposes four rows of four: lane `j` of output `i` is lane `i` of
    /// input `j`.
    #[inline(always)]
    pub fn transpose(rows: [F32x4; 4]) -> [F32x4; 4] {
        // SAFETY: SSE2.
        unsafe {
            let t0 = _mm_unpacklo_ps(rows[0].0, rows[1].0);
            let t1 = _mm_unpacklo_ps(rows[2].0, rows[3].0);
            let t2 = _mm_unpackhi_ps(rows[0].0, rows[1].0);
            let t3 = _mm_unpackhi_ps(rows[2].0, rows[3].0);
            [
                F32x4(_mm_movelh_ps(t0, t1)),
                F32x4(_mm_movehl_ps(t1, t0)),
                F32x4(_mm_movelh_ps(t2, t3)),
                F32x4(_mm_movehl_ps(t3, t2)),
            ]
        }
    }
}

#[cfg(target_arch = "x86_64")]
impl Mask4 {
    #[inline(always)]
    pub fn from_bools(b: [bool; 4]) -> Mask4 {
        let bits = b.map(|b| if b { -1i32 } else { 0 });
        // SAFETY: SSE2; reads four ints from an array of four.
        Mask4(F32x4(unsafe {
            _mm_castsi128_ps(_mm_loadu_si128(bits.as_ptr().cast()))
        }))
    }
}

#[cfg(target_arch = "x86_64")]
macro_rules! binary {
    ($trait:ident, $method:ident, $intrinsic:ident) => {
        impl std::ops::$trait for F32x4 {
            type Output = F32x4;
            #[inline(always)]
            fn $method(self, rhs: F32x4) -> F32x4 {
                // SAFETY: SSE2.
                F32x4(unsafe { $intrinsic(self.0, rhs.0) })
            }
        }
    };
}

#[cfg(target_arch = "x86_64")]
binary!(Add, add, _mm_add_ps);
#[cfg(target_arch = "x86_64")]
binary!(Sub, sub, _mm_sub_ps);
#[cfg(target_arch = "x86_64")]
binary!(Mul, mul, _mm_mul_ps);
#[cfg(target_arch = "x86_64")]
binary!(Div, div, _mm_div_ps);

#[cfg(target_arch = "x86_64")]
impl std::ops::Neg for F32x4 {
    type Output = F32x4;
    #[inline(always)]
    fn neg(self) -> F32x4 {
        // SAFETY: SSE2. Flipping the sign bit is what `-x` does.
        F32x4(unsafe { _mm_xor_ps(self.0, _mm_set1_ps(-0.0)) })
    }
}

#[cfg(not(target_arch = "x86_64"))]
impl F32x4 {
    #[inline(always)]
    pub fn splat(x: f32) -> F32x4 {
        F32x4([x; 4])
    }

    #[inline(always)]
    pub fn from_array(a: [f32; 4]) -> F32x4 {
        F32x4(a)
    }

    #[inline(always)]
    pub fn to_array(self) -> [f32; 4] {
        self.0
    }

    #[inline(always)]
    pub fn load(slice: &[f32], start: usize) -> F32x4 {
        F32x4(slice[start..start + 4].try_into().unwrap())
    }

    /// # Safety
    /// `ptr..ptr + 4` must be readable floats.
    #[inline(always)]
    pub unsafe fn load_ptr(ptr: *const f32) -> F32x4 {
        // SAFETY: the caller's promise.
        F32x4(unsafe { std::ptr::read_unaligned(ptr.cast()) })
    }

    /// # Safety
    /// `ptr..ptr + 4` must be writable floats.
    #[inline(always)]
    pub unsafe fn store_ptr(self, ptr: *mut f32) {
        // SAFETY: the caller's promise.
        unsafe { std::ptr::write_unaligned(ptr.cast(), self.0) }
    }

    #[inline(always)]
    pub fn trunc_i32(self) -> ([i32; 4], F32x4) {
        let whole = self.0.map(|x| x as i32);
        (whole, F32x4(whole.map(|w| w as f32)))
    }

    #[inline(always)]
    pub fn max(self, other: F32x4) -> F32x4 {
        F32x4(std::array::from_fn(|i| {
            let (a, b) = (self.0[i], other.0[i]);
            if a > b { a } else { b }
        }))
    }

    #[inline(always)]
    pub fn min_of(self, other: F32x4) -> F32x4 {
        F32x4(std::array::from_fn(|i| {
            let (a, b) = (self.0[i], other.0[i]);
            if a < b { a } else { b }
        }))
    }

    #[inline(always)]
    pub fn greater_than(self, other: F32x4) -> Mask4 {
        Mask4::from_bools(std::array::from_fn(|i| self.0[i] > other.0[i]))
    }

    #[inline(always)]
    pub fn greater_equal(self, other: F32x4) -> Mask4 {
        Mask4::from_bools(std::array::from_fn(|i| self.0[i] >= other.0[i]))
    }

    #[inline(always)]
    pub fn equal(self, other: F32x4) -> Mask4 {
        Mask4::from_bools(std::array::from_fn(|i| self.0[i] == other.0[i]))
    }

    #[inline(always)]
    fn exponent_from_low_bits(self, bias: i32) -> F32x4 {
        F32x4(self.0.map(|x| {
            let n = (x.to_bits() as i32).wrapping_sub(bias);
            f32::from_bits((n << 23) as u32)
        }))
    }

    #[inline(always)]
    fn split_exponent(self) -> (F32x4, F32x4) {
        let exponent = self
            .0
            .map(|x| (((x.to_bits() >> 23) & 0xff) as i32 - 127) as f32);
        let mantissa = self
            .0
            .map(|x| f32::from_bits((x.to_bits() & 0x007f_ffff) | 0x3f80_0000));
        (F32x4(exponent), F32x4(mantissa))
    }

    #[inline(always)]
    pub fn select(mask: Mask4, a: F32x4, b: F32x4) -> F32x4 {
        F32x4(std::array::from_fn(|i| {
            if mask.0.0[i].to_bits() != 0 {
                a.0[i]
            } else {
                b.0[i]
            }
        }))
    }

    #[inline(always)]
    pub fn transpose(rows: [F32x4; 4]) -> [F32x4; 4] {
        std::array::from_fn(|i| F32x4(std::array::from_fn(|j| rows[j].0[i])))
    }
}

#[cfg(not(target_arch = "x86_64"))]
impl Mask4 {
    #[inline(always)]
    pub fn from_bools(b: [bool; 4]) -> Mask4 {
        Mask4(F32x4(
            b.map(|b| f32::from_bits(if b { u32::MAX } else { 0 })),
        ))
    }
}

#[cfg(not(target_arch = "x86_64"))]
macro_rules! binary {
    ($trait:ident, $method:ident, $op:tt) => {
        impl std::ops::$trait for F32x4 {
            type Output = F32x4;
            #[inline(always)]
            fn $method(self, rhs: F32x4) -> F32x4 {
                F32x4(std::array::from_fn(|i| self.0[i] $op rhs.0[i]))
            }
        }
    };
}

#[cfg(not(target_arch = "x86_64"))]
binary!(Add, add, +);
#[cfg(not(target_arch = "x86_64"))]
binary!(Sub, sub, -);
#[cfg(not(target_arch = "x86_64"))]
binary!(Mul, mul, *);
#[cfg(not(target_arch = "x86_64"))]
binary!(Div, div, /);

#[cfg(not(target_arch = "x86_64"))]
impl std::ops::Neg for F32x4 {
    type Output = F32x4;
    #[inline(always)]
    fn neg(self) -> F32x4 {
        F32x4(self.0.map(|x| -x))
    }
}

/// `2^x` lane by lane, within 3e-7 of it relatively (about 3e-6 dB),
/// for `x` in `-126..=126`, clamped there beyond; NaN stays NaN. Four at a
/// time for less than one call into the maths library: for gains in the
/// dB domain, computed for a block at once.
// The polynomial's coefficients are fitted, not ln 2 and its powers.
#[allow(clippy::approx_constant)]
#[inline(always)]
pub fn exp2(x: F32x4) -> F32x4 {
    // Clamped with NaN passing through: `max` and `min` give their second
    // operand when either is NaN.
    let x = F32x4::splat(126.0).min_of(F32x4::splat(-126.0).max(x));
    // Adding 1.5·2²³ rounds to a whole number, left in the low bits.
    let round = F32x4::splat(12_582_912.0);
    let rounded = x + round;
    let whole = rounded - round;
    let f = x - whole;
    let scale = rounded.exponent_from_low_bits(12_582_912.0f32.to_bits() as i32 - 127);
    // 2^f for f in -0.5..=0.5, fitted for least relative error.
    let c = |v: f32| F32x4::splat(v);
    let p = c(1.0)
        + f * (c(0.693_147)
            + f * (c(0.240_222_42)
                + f * (c(0.055_507_336) + f * (c(0.009_671_513) + f * c(0.001_326_472_7)))));
    p * scale
}

/// `log2(x)` lane by lane, within 4e-7 of it (about 2e-6 dB) for positive
/// normal `x`: infinity for infinity, minus infinity for zero, NaN for
/// NaN and negative numbers, and about -127 for subnormals.
#[inline(always)]
pub fn log2(x: F32x4) -> F32x4 {
    let (exponent, mantissa) = x.split_exponent();
    // The mantissa within √½..√2, where the series below is short.
    let high = mantissa.greater_than(F32x4::splat(std::f32::consts::SQRT_2));
    let m = F32x4::select(high, mantissa * F32x4::splat(0.5), mantissa);
    let exponent = F32x4::select(high, exponent + F32x4::splat(1.0), exponent);
    // log2(m) = 2·atanh(t)/ln 2 with t = (m - 1)/(m + 1), |t| < 0.18.
    let one = F32x4::splat(1.0);
    let t = (m - one) / (m + one);
    let t2 = t * t;
    let c = |v: f32| F32x4::splat(v);
    let log = exponent + t * (c(2.885_390_5) + t2 * (c(0.961_588_3) + t2 * c(0.595_780_7)));
    // The special values, as the library's `log2` has them.
    let zero = F32x4::splat(0.0);
    let log = F32x4::select(x.equal(zero), F32x4::splat(f32::NEG_INFINITY), log);
    let log = F32x4::select(x.equal(F32x4::splat(f32::INFINITY)), x, log);
    // `x >= 0` is false for NaN and negatives.
    F32x4::select(x.greater_equal(zero), log, F32x4::splat(f32::NAN))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_compute_what_scalars_do() {
        let a = [1.5f32, -2.25, 1e-30, 3.0e7];
        let b = [0.1f32, 7.0, -4.5, f32::NAN];
        let (x, y) = (F32x4::from_array(a), F32x4::from_array(b));
        let sum = (x + y).to_array();
        let product = (x * y - x).to_array();
        for i in 0..4 {
            assert_eq!(sum[i].to_bits(), (a[i] + b[i]).to_bits());
            assert_eq!(product[i].to_bits(), (a[i] * b[i] - a[i]).to_bits());
        }
        assert_eq!((-x).to_array(), a.map(|v| -v));
        let max = F32x4::from_array(b).max(F32x4::splat(2.0)).to_array();
        assert_eq!(max, [2.0, 7.0, 2.0, 2.0], "NaN gives way, as f32::max does");
        let (whole, back) = F32x4::from_array([2.7, 3.0, 1023.999, 0.2]).trunc_i32();
        assert_eq!(whole, [2, 3, 1023, 0]);
        assert_eq!(back.to_array(), [2.0, 3.0, 1023.0, 0.0]);
        let picked = F32x4::select(Mask4::from_bools([true, false, false, true]), x, y);
        assert_eq!(picked.to_array()[..3], [1.5, 7.0, -4.5]);
        assert_eq!(picked.to_array()[3], 3.0e7);
    }

    #[test]
    fn exp2_and_log2_are_within_a_millionth_of_a_db() {
        let mut worst_exp = 0.0f64;
        let mut worst_log = 0.0f64;
        for i in -500_000..=500_000 {
            let xs: [f32; 4] = std::array::from_fn(|k| (4 * i + k as i32) as f32 * 6e-5);
            let e = exp2(F32x4::from_array(xs)).to_array();
            let ys = xs.map(|x| (x * 0.2).exp2());
            let l = log2(F32x4::from_array(ys)).to_array();
            for k in 0..4 {
                let exact = (xs[k] as f64).exp2();
                worst_exp = worst_exp.max((e[k] as f64 / exact - 1.0).abs());
                worst_log = worst_log.max((l[k] as f64 - (ys[k] as f64).log2()).abs());
            }
        }
        assert!(worst_exp < 3e-7, "exp2 off by {worst_exp} relatively");
        assert!(worst_log < 4e-7, "log2 off by {worst_log}");
        let e = exp2(F32x4::from_array([0.0, -3.0, f32::NAN, -1000.0])).to_array();
        assert_eq!(e[..2], [1.0, 0.125]);
        assert!(e[2].is_nan() && e[3] > 0.0 && e[3] < 1e-37);
        let l = log2(F32x4::from_array([1.0, 0.0, f32::INFINITY, -1.0])).to_array();
        assert_eq!(l[..3], [0.0, f32::NEG_INFINITY, f32::INFINITY]);
        assert!(l[3].is_nan());
        assert!(log2(F32x4::splat(f32::NAN)).to_array()[0].is_nan());
    }

    #[test]
    fn transpose_swaps_rows_and_columns() {
        let rows: [F32x4; 4] =
            std::array::from_fn(|r| F32x4::from_array(std::array::from_fn(|c| (4 * r + c) as f32)));
        let columns = F32x4::transpose(rows);
        for (c, column) in columns.iter().enumerate() {
            assert_eq!(
                column.to_array(),
                std::array::from_fn(|r| (4 * r + c) as f32)
            );
        }
    }
}
