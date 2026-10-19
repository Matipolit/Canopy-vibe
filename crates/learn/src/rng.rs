//! A small random number generator (xorshift64*) for picking exercises.
//! Seeded from the time in the app and fixed in tests.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        // Zero would stay zero forever.
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }

    pub fn from_time() -> Rng {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        Rng::new(nanos)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n`; 0 when `n` is 0.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }

    /// Uniform in `low..=high`.
    pub fn range(&mut self, low: i32, high: i32) -> i32 {
        let (low, high) = (low.min(high), low.max(high));
        low + self.below((high - low + 1) as usize) as i32
    }

    pub fn chance(&mut self, p: f64) -> bool {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        items.get(self.below(items.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_in_range_and_covers_it() {
        let mut rng = Rng::new(0);
        let mut seen = [false; 5];
        for _ in 0..1000 {
            let v = rng.range(-2, 2);
            assert!((-2..=2).contains(&v));
            seen[(v + 2) as usize] = true;
        }
        assert!(seen.iter().all(|s| *s));
        assert_eq!(rng.below(0), 0);
        assert_eq!(rng.pick::<u8>(&[]), None);
        let heads = (0..1000).filter(|_| rng.chance(0.5)).count();
        assert!((400..600).contains(&heads));
        // Same seed, same numbers.
        assert_eq!(Rng::new(7).next_u64(), Rng::new(7).next_u64());
    }
}
