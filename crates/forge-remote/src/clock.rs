//! Time, injectable so expiry and throttling are tested without sleeping.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

/// A monotonic clock for expiry, and wall time for what is shown and stored.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Instant;
    fn wall(&self) -> DateTime<Utc>;
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wall(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock that moves only when told to. Both readings advance together.
#[derive(Debug)]
pub struct ManualClock {
    base: Instant,
    wall_base: DateTime<Utc>,
    offset: Mutex<Duration>,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
            wall_base: Utc::now(),
            offset: Mutex::new(Duration::ZERO),
        }
    }

    pub fn advance(&self, by: Duration) {
        let mut offset = self.offset.lock().unwrap_or_else(|e| e.into_inner());
        *offset += by;
    }

    fn offset(&self) -> Duration {
        *self.offset.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.base + self.offset()
    }

    fn wall(&self) -> DateTime<Utc> {
        self.wall_base + chrono::Duration::from_std(self.offset()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manual_clock_moves_both_readings_together() {
        let clock = ManualClock::new();
        let (start, wall) = (clock.now(), clock.wall());
        clock.advance(Duration::from_secs(90));
        assert_eq!(clock.now() - start, Duration::from_secs(90));
        assert_eq!((clock.wall() - wall).num_seconds(), 90);
    }
}
