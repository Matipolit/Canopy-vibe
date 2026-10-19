/// Note velocity at MIDI 2.0 resolution (16 bits).
///
/// MIDI 1.0 uses 7 bits; convert with [`Velocity::from_midi1`] and
/// [`Velocity::to_midi1`], which follow the MIDI 2.0 spec's min-center-max
/// scaling, so 0, 64 and 127 map exactly to 0, `0x8000` and `0xFFFF`.
///
/// Unlike MIDI 1.0, a Note On with velocity 0 is not a Note Off in MIDI 2.0.
/// When writing MIDI 1.0 Note Ons, raise a velocity of 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
#[repr(transparent)]
pub struct Velocity(u16);

impl Velocity {
    pub const MIN: Velocity = Velocity(0);
    pub const MAX: Velocity = Velocity(u16::MAX);
    /// MIDI 1.0 velocity 64, what devices send when they don't measure release velocity.
    pub const CENTER: Velocity = Velocity(0x8000);

    pub const fn new(value: u16) -> Velocity {
        Velocity(value)
    }

    pub const fn get(self) -> u16 {
        self.0
    }

    /// Upscales a 7-bit MIDI 1.0 velocity. Values above 127 are clamped.
    pub const fn from_midi1(velocity: u8) -> Velocity {
        let v = if velocity > 127 { 127 } else { velocity } as u16;
        // Min-center-max scaling: below the center, a plain shift. Above it,
        // repeat the low 6 bits into the new low bits so 127 reaches 0xFFFF.
        let shifted = v << 9;
        if v <= 64 {
            return Velocity(shifted);
        }
        let repeat = v & 0x3F;
        Velocity(shifted | (repeat << 3) | (repeat >> 3))
    }

    /// Downscales to a 7-bit MIDI 1.0 velocity by dropping the low bits.
    pub const fn to_midi1(self) -> u8 {
        (self.0 >> 9) as u8
    }

    pub fn as_f32(self) -> f32 {
        self.0 as f32 / u16::MAX as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi1_anchor_points() {
        assert_eq!(Velocity::from_midi1(0), Velocity::MIN);
        assert_eq!(Velocity::from_midi1(64), Velocity::CENTER);
        assert_eq!(Velocity::from_midi1(127), Velocity::MAX);
        assert_eq!(Velocity::from_midi1(200), Velocity::MAX);
    }

    #[test]
    fn midi1_round_trips_and_is_monotonic() {
        let mut previous = None;
        for v in 0..=127 {
            let up = Velocity::from_midi1(v);
            assert_eq!(up.to_midi1(), v);
            assert!(previous < Some(up));
            previous = Some(up);
        }
    }

    #[test]
    fn as_f32_spans_unit_range() {
        assert_eq!(Velocity::MIN.as_f32(), 0.0);
        assert_eq!(Velocity::MAX.as_f32(), 1.0);
    }
}
