use serde::{Deserialize, Serialize};

/// At or below this the fader is fully off.
pub const SILENCE_DB: f32 = -60.0;
pub const MAX_VOLUME_DB: f32 = 6.0;

/// A channel strip's fader section.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Mixer {
    pub volume_db: f32,
    /// `-1` hard left, `0` center, `1` hard right.
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
}

impl Default for Mixer {
    fn default() -> Self {
        Mixer {
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
        }
    }
}

/// Linear gain for a fader position; the bottom of the travel is silence.
pub fn volume_gain(volume_db: f32) -> f32 {
    if volume_db <= SILENCE_DB {
        0.0
    } else {
        10f32.powf(volume_db.min(MAX_VOLUME_DB) / 20.0)
    }
}

/// Fader position on the `0..=1` scale automation lanes use. Linear in dB.
pub fn volume_to_normalized(volume_db: f32) -> f32 {
    ((volume_db - SILENCE_DB) / (MAX_VOLUME_DB - SILENCE_DB)).clamp(0.0, 1.0)
}

pub fn normalized_to_volume(normalized: f32) -> f32 {
    SILENCE_DB + normalized.clamp(0.0, 1.0) * (MAX_VOLUME_DB - SILENCE_DB)
}

pub fn pan_to_normalized(pan: f32) -> f32 {
    (pan.clamp(-1.0, 1.0) + 1.0) / 2.0
}

pub fn normalized_to_pan(normalized: f32) -> f32 {
    normalized.clamp(0.0, 1.0) * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_scale() {
        assert_eq!(volume_gain(SILENCE_DB), 0.0);
        assert_eq!(volume_gain(0.0), 1.0);
        assert!((volume_gain(-6.0) - 0.501).abs() < 1e-3);
        for db in [-60.0, -12.0, 0.0, 6.0] {
            assert!((normalized_to_volume(volume_to_normalized(db)) - db).abs() < 1e-4);
        }
        assert_eq!(normalized_to_pan(pan_to_normalized(-0.5)), -0.5);
    }
}
