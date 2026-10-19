//! The one clock everything is timed by: key presses (stamped on the MIDI
//! driver's threads), the song's position, the metronome. Real time in the
//! app; tests move it by hand.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Clone)]
pub struct Clock(Arc<Source>);

enum Source {
    Real(Instant),
    /// Microseconds, set by tests.
    Manual(AtomicU64),
}

impl Clock {
    pub fn real() -> Clock {
        Clock(Arc::new(Source::Real(Instant::now())))
    }

    /// Stands still until [`Clock::advance`] moves it.
    pub fn manual() -> Clock {
        Clock(Arc::new(Source::Manual(AtomicU64::new(0))))
    }

    /// Seconds since the clock started. Safe from any thread.
    pub fn now(&self) -> f64 {
        match &*self.0 {
            Source::Real(epoch) => epoch.elapsed().as_secs_f64(),
            Source::Manual(micros) => micros.load(Ordering::Relaxed) as f64 / 1e6,
        }
    }

    /// Moves a manual clock forward; a real one ignores this.
    pub fn advance(&self, seconds: f64) {
        if let Source::Manual(micros) = &*self.0 {
            micros.fetch_add((seconds * 1e6).round() as u64, Ordering::Relaxed);
        }
    }

    pub fn is_manual(&self) -> bool {
        matches!(&*self.0, Source::Manual(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clocks_move_only_when_told() {
        let clock = Clock::manual();
        assert_eq!(clock.now(), 0.0);
        let shared = clock.clone();
        shared.advance(0.25);
        assert_eq!(clock.now(), 0.25);
        assert!(clock.is_manual());
        let real = Clock::real();
        real.advance(100.0);
        assert!(real.now() < 1.0);
    }
}
