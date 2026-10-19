//! Parameter values as a CLAP host sees them, shared between the plugin's
//! main thread and its audio thread.
//!
//! A host shows a CLAP parameter between its minimum and maximum on a
//! straight scale, so float parameters are offered normalized (`0..=1`,
//! through [`ParamDescriptor::to_normalized`]): a frequency's knob in a
//! host's generic editor then turns on the same log scale as in canopy.
//! Enum parameters are stepped label indices. Text both ways is the
//! descriptor's own ([`ParamDescriptor::format`] and `parse`).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use instruments::{ParamDescriptor, ParameterKind};

/// The host's range for `param`: `(min, max)`.
pub fn range(param: &ParamDescriptor) -> (f64, f64) {
    match param.kind {
        ParameterKind::Float { .. } => (0.0, 1.0),
        ParameterKind::Enum { labels, .. } => (0.0, labels.len().saturating_sub(1) as f64),
    }
}

/// `plain` (the instrument's own units) in the host's units.
pub fn to_host(param: &ParamDescriptor, plain: f32) -> f64 {
    match param.kind {
        ParameterKind::Float { .. } => f64::from(param.to_normalized(plain)),
        ParameterKind::Enum { .. } => f64::from(param.clamp(plain)),
    }
}

/// The inverse of [`to_host`], clamped to the range. NaN reads as the
/// default.
pub fn to_plain(param: &ParamDescriptor, host: f64) -> f32 {
    if host.is_nan() {
        return param.default_value();
    }
    match param.kind {
        ParameterKind::Float { .. } => param.from_normalized(host as f32),
        ParameterKind::Enum { .. } => param.clamp(host as f32),
    }
}

/// Every parameter's current value, in the instrument's units (so
/// defaults and saved states are exact): what `params.get_value` answers,
/// converted, and what the state saves. The audio thread writes the values
/// automation brings; the main thread writes the ones it's given (a state,
/// a flush while inactive) and marks them pending for the audio thread to
/// apply.
pub struct Values {
    params: &'static [ParamDescriptor],
    bits: Box<[AtomicU32]>,
    pending: Box<[AtomicBool]>,
    /// Some parameter is pending: spares the audio thread a look at each.
    any_pending: AtomicBool,
}

impl Values {
    /// Every parameter at its default.
    pub fn new(params: &'static [ParamDescriptor]) -> Values {
        Values {
            params,
            bits: params
                .iter()
                .map(|p| AtomicU32::new(p.default_value().to_bits()))
                .collect(),
            pending: params.iter().map(|_| AtomicBool::new(false)).collect(),
            any_pending: AtomicBool::new(false),
        }
    }

    pub fn params(&self) -> &'static [ParamDescriptor] {
        self.params
    }

    pub fn len(&self) -> usize {
        self.params.len()
    }

    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }

    /// Parameter `index` in the instrument's units; `None` past the end.
    pub fn plain(&self, index: usize) -> Option<f32> {
        Some(f32::from_bits(
            self.bits.get(index)?.load(Ordering::Relaxed),
        ))
    }

    /// Parameter `index` in the host's units.
    pub fn get(&self, index: usize) -> Option<f64> {
        Some(to_host(self.params.get(index)?, self.plain(index)?))
    }

    /// Stores a value (host units) the audio thread is applying itself.
    /// Returns whether `index` is a parameter.
    pub fn set(&self, index: usize, host: f64) -> bool {
        match self.params.get(index) {
            Some(param) => self.store(index, to_plain(param, host)),
            None => false,
        }
    }

    /// Stores a value (host units) from the main thread, for the audio
    /// thread to apply at its next block ([`Self::take_pending`]).
    pub fn send(&self, index: usize, host: f64) {
        if self.set(index, host) {
            self.mark_pending(index);
        }
    }

    /// [`Self::send`], in the instrument's units.
    pub fn send_plain(&self, index: usize, plain: f32) {
        if self.store(index, plain) {
            self.mark_pending(index);
        }
    }

    /// Stores `plain`, clamped (NaN as the default).
    fn store(&self, index: usize, plain: f32) -> bool {
        let (Some(param), Some(bits)) = (self.params.get(index), self.bits.get(index)) else {
            return false;
        };
        let plain = if plain.is_nan() {
            param.default_value()
        } else {
            param.clamp(plain)
        };
        bits.store(plain.to_bits(), Ordering::Relaxed);
        true
    }

    fn mark_pending(&self, index: usize) {
        self.pending[index].store(true, Ordering::Release);
        self.any_pending.store(true, Ordering::Release);
    }

    /// Calls `f` with each parameter sent since the last call. Real-time
    /// safe.
    pub fn take_pending(&self, mut f: impl FnMut(usize)) {
        if !self.any_pending.swap(false, Ordering::Acquire) {
            return;
        }
        for (index, pending) in self.pending.iter().enumerate() {
            if pending.swap(false, Ordering::Acquire) {
                f(index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use instruments::{choice, float};

    static PARAMS: &[ParamDescriptor] = &[
        float("cutoff", "Cutoff", 20.0, 20_000.0, 1000.0, "Hz", true),
        choice("shape", "Shape", &["Sine", "Saw", "Square"], 1),
    ];

    #[test]
    fn floats_are_normalized_and_enums_are_indices() {
        let cutoff = &PARAMS[0];
        assert_eq!(range(cutoff), (0.0, 1.0));
        assert!((to_host(cutoff, 632.456) - 0.5).abs() < 1e-4);
        assert!((to_plain(cutoff, 0.5) - 632.456).abs() < 0.1);
        let shape = &PARAMS[1];
        assert_eq!(range(shape), (0.0, 2.0));
        assert_eq!(to_host(shape, 2.0), 2.0);
        assert_eq!(to_plain(shape, 1.4), 1.0);
    }

    #[test]
    fn values_start_at_defaults_and_clamp() {
        let values = Values::new(PARAMS);
        assert_eq!(values.plain(1), Some(1.0));
        assert!((values.plain(0).unwrap() - 1000.0).abs() < 0.1);
        values.set(0, 7.0);
        assert_eq!(values.get(0), Some(1.0));
        assert_eq!(values.plain(0), Some(20_000.0));
        values.set(1, 1.6);
        assert_eq!(values.get(1), Some(2.0));
        values.send_plain(1, f32::NAN);
        assert_eq!(values.plain(1), Some(1.0));
        assert!(!values.set(2, 0.0));
        assert_eq!(values.get(2), None);
    }

    #[test]
    fn values_sent_from_the_main_thread_are_taken_once() {
        let values = Values::new(PARAMS);
        values.send(1, 0.0);
        let mut taken = Vec::new();
        values.take_pending(|i| taken.push(i));
        values.take_pending(|i| taken.push(i));
        assert_eq!(taken, [1]);
    }
}
