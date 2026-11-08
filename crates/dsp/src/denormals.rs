//! Subnormal floats treated as zero on the threads that render audio.
//!
//! A decaying filter or a long impulse's quiet end sinks through the
//! subnormal range (below about 1e-38) before it reaches zero, and every
//! multiplication that meets a subnormal there takes the processor a
//! microcode assist of a hundred cycles or more. [`dsp::flush`](crate::flush)
//! keeps the states that would linger there at zero, but not everything:
//! a 153-second project took 31 million assists, 3% of its render, before
//! the engine set the processor to flush them itself.
//!
//! [`FlushToZero`] sets that mode for as long as it lives and puts back
//! what was there before, so a thread borrowed from elsewhere (PipeWire's,
//! a plugin host's) gets its own mode back. Only values that small change:
//! inputs below the smallest normal float count as zero, as do results that
//! would fall there.

/// Flush-to-zero and denormals-are-zero, while it lives.
#[must_use = "the mode lasts only as long as the guard"]
pub struct FlushToZero {
    #[cfg_attr(
        not(any(target_arch = "x86_64", target_arch = "aarch64")),
        allow(dead_code)
    )]
    saved: u64,
}

#[cfg(target_arch = "x86_64")]
mod arch {
    /// MXCSR's flush-to-zero and denormals-are-zero bits.
    const FTZ_DAZ: u32 = 1 << 15 | 1 << 6;

    pub fn enable() -> u64 {
        let mut saved = 0u32;
        // SAFETY: stores MXCSR to a local, then loads it with two mode bits
        // set; neither touches anything else.
        unsafe {
            std::arch::asm!("stmxcsr [{}]", in(reg) &mut saved, options(nostack, preserves_flags));
            let set = saved | FTZ_DAZ;
            std::arch::asm!("ldmxcsr [{}]", in(reg) &set, options(nostack, preserves_flags, readonly));
        }
        u64::from(saved)
    }

    pub fn restore(saved: u64) {
        let saved = saved as u32;
        // SAFETY: loads back the MXCSR value `enable` found.
        unsafe {
            std::arch::asm!("ldmxcsr [{}]", in(reg) &saved, options(nostack, preserves_flags, readonly));
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    /// FPCR's flush-to-zero bit (inputs and results alike).
    const FZ: u64 = 1 << 24;

    pub fn enable() -> u64 {
        let saved: u64;
        // SAFETY: reads FPCR, then writes it back with one mode bit set.
        unsafe {
            std::arch::asm!("mrs {}, fpcr", out(reg) saved, options(nomem, nostack, preserves_flags));
            std::arch::asm!("msr fpcr, {}", in(reg) saved | FZ, options(nomem, nostack, preserves_flags));
        }
        saved
    }

    pub fn restore(saved: u64) {
        // SAFETY: writes back the FPCR value `enable` found.
        unsafe {
            std::arch::asm!("msr fpcr, {}", in(reg) saved, options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod arch {
    pub fn enable() -> u64 {
        0
    }

    pub fn restore(_saved: u64) {}
}

impl FlushToZero {
    /// Sets the mode on this thread until the guard is dropped.
    #[inline]
    pub fn new() -> FlushToZero {
        FlushToZero {
            saved: arch::enable(),
        }
    }

    /// Sets the mode on this thread for good: for threads that only ever
    /// render, such as the engine's workers.
    pub fn forever() {
        std::mem::forget(FlushToZero::new());
    }
}

impl Default for FlushToZero {
    fn default() -> Self {
        FlushToZero::new()
    }
}

impl Drop for FlushToZero {
    #[inline]
    fn drop(&mut self) {
        arch::restore(self.saved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Something the optimizer can't work out ahead.
    fn opaque(x: f32) -> f32 {
        std::hint::black_box(x)
    }

    #[test]
    fn subnormals_count_as_zero_only_while_the_guard_lives() {
        let tiny = opaque(1e-40);
        assert!(tiny * opaque(1.0) != 0.0, "subnormals work as usual");
        {
            let _guard = FlushToZero::new();
            if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
                assert_eq!(opaque(tiny) * opaque(1.0), 0.0, "an input flushed");
                assert_eq!(opaque(1e-30) * opaque(1e-10), 0.0, "a result flushed");
            }
            assert_eq!(opaque(0.5) * opaque(3.0), 1.5, "normal numbers untouched");
        }
        assert!(opaque(tiny) * opaque(1.0) != 0.0, "the mode was put back");
    }
}
