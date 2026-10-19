/// Decoded audio at a known sample rate. Always stereo: mono files are copied
/// to both sides, and files with more channels keep only the first two.
///
/// Immutable once built. The audio thread reads it through an `Arc`, and only
/// the UI thread ever drops the last reference.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioBuffer {
    sample_rate: u32,
    left: Box<[f32]>,
    right: Box<[f32]>,
}

impl AudioBuffer {
    /// Panics if the channels differ in length.
    pub fn new(sample_rate: u32, left: Vec<f32>, right: Vec<f32>) -> Self {
        assert_eq!(left.len(), right.len(), "channels must be the same length");
        Self {
            sample_rate,
            left: left.into_boxed_slice(),
            right: right.into_boxed_slice(),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn frames(&self) -> usize {
        self.left.len()
    }

    pub fn left(&self) -> &[f32] {
        &self.left
    }

    pub fn right(&self) -> &[f32] {
        &self.right
    }

    pub fn duration_seconds(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }
}
