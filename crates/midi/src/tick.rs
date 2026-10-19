use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

/// Ticks per quarter note: the "division" field of a Standard MIDI File header.
/// 960 divides evenly into every common note length, including triplets.
pub const PPQ: i64 = 960;

/// A position or duration in musical time. Signed, so the difference of two
/// positions is also a `Tick`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
#[repr(transparent)]
pub struct Tick(i64);

impl Tick {
    pub const ZERO: Tick = Tick(0);
    pub const QUARTER: Tick = Tick(PPQ);

    pub const fn new(ticks: i64) -> Tick {
        Tick(ticks)
    }

    pub const fn get(self) -> i64 {
        self.0
    }

    /// Converts a length in quarter notes, rounding to the nearest tick.
    pub fn from_quarters(quarters: f64) -> Tick {
        Tick((quarters * PPQ as f64).round() as i64)
    }

    pub fn to_quarters(self) -> f64 {
        self.0 as f64 / PPQ as f64
    }
}

impl Add for Tick {
    type Output = Tick;
    fn add(self, rhs: Tick) -> Tick {
        Tick(self.0 + rhs.0)
    }
}

impl Sub for Tick {
    type Output = Tick;
    fn sub(self, rhs: Tick) -> Tick {
        Tick(self.0 - rhs.0)
    }
}

impl Neg for Tick {
    type Output = Tick;
    fn neg(self) -> Tick {
        Tick(-self.0)
    }
}

impl AddAssign for Tick {
    fn add_assign(&mut self, rhs: Tick) {
        self.0 += rhs.0;
    }
}

impl SubAssign for Tick {
    fn sub_assign(&mut self, rhs: Tick) {
        self.0 -= rhs.0;
    }
}

impl Mul<i64> for Tick {
    type Output = Tick;
    fn mul(self, rhs: i64) -> Tick {
        Tick(self.0 * rhs)
    }
}

/// Rounds toward zero, like integer division.
impl Div<i64> for Tick {
    type Output = Tick;
    fn div(self, rhs: i64) -> Tick {
        Tick(self.0 / rhs)
    }
}

impl Tick {
    /// The grid line at or before `self`. `grid` must be positive.
    pub fn floor_to(self, grid: Tick) -> Tick {
        Tick(self.0.div_euclid(grid.0) * grid.0)
    }

    /// The grid line nearest to `self`. `grid` must be positive.
    pub fn round_to(self, grid: Tick) -> Tick {
        Tick((self.0 + grid.0 / 2).div_euclid(grid.0) * grid.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_conversions() {
        assert_eq!(Tick::from_quarters(1.0), Tick::QUARTER);
        assert_eq!(Tick::from_quarters(1.0 / 3.0), Tick::new(320)); // eighth-note triplet
        assert_eq!(Tick::new(1440).to_quarters(), 1.5);
    }

    #[test]
    fn grid_rounding_handles_negative_positions() {
        let grid = Tick::QUARTER;
        assert_eq!(Tick::new(1000).floor_to(grid), Tick::new(960));
        assert_eq!(Tick::new(-10).floor_to(grid), Tick::new(-960));
        assert_eq!(Tick::new(1400).round_to(grid), Tick::new(960));
        assert_eq!(Tick::new(1500).round_to(grid), Tick::new(1920));
        assert_eq!(Tick::new(-400).round_to(grid), Tick::ZERO);
        assert_eq!(Tick::QUARTER * 4 / 2, Tick::new(1920));
    }

    #[test]
    fn arithmetic() {
        let start = Tick::new(960);
        let end = start + Tick::new(480);
        assert_eq!(end - start, Tick::new(480));
        assert_eq!(start - end, -Tick::new(480));
    }
}
