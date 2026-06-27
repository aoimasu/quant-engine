//! The time + sleep seam for the REST client.
//!
//! Backoff needs to *read* the clock and to *wait*; both go through [`Clock`] so the retry loop is
//! deterministic in tests. [`SystemClock`] is the production wall-clock; [`ManualClock`] (test) records
//! every wait and advances a logical clock instead of sleeping.

/// Reads time and backs off. The single seam between the client and real time.
pub trait Clock {
    /// Current time in epoch-milliseconds.
    fn now_ms(&self) -> i64;
    /// Block until `deadline_ms` (no-op if already past). The back-off primitive.
    fn sleep_until(&self, deadline_ms: i64);
}

/// Production clock: system time + `thread::sleep`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        let dur = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        // Saturates rather than panics past year ~292 million.
        i64::try_from(dur.as_millis()).unwrap_or(i64::MAX)
    }

    fn sleep_until(&self, deadline_ms: i64) {
        let now = self.now_ms();
        if deadline_ms > now {
            let wait = u64::try_from(deadline_ms - now).unwrap_or(0);
            std::thread::sleep(std::time::Duration::from_millis(wait));
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::Clock;
    use std::cell::RefCell;

    /// A logical clock for tests: `sleep_until` records the deadline and jumps the clock forward instead
    /// of sleeping, so backoff is asserted exactly with zero wall-clock.
    pub(crate) struct ManualClock {
        now: RefCell<i64>,
        pub waits: RefCell<Vec<i64>>,
    }

    impl ManualClock {
        pub fn new(start_ms: i64) -> Self {
            Self {
                now: RefCell::new(start_ms),
                waits: RefCell::new(Vec::new()),
            }
        }

        /// Number of recorded backoffs (waits that actually advanced the clock).
        pub fn wait_count(&self) -> usize {
            self.waits.borrow().len()
        }
    }

    impl Clock for ManualClock {
        fn now_ms(&self) -> i64 {
            *self.now.borrow()
        }

        fn sleep_until(&self, deadline_ms: i64) {
            let mut now = self.now.borrow_mut();
            if deadline_ms > *now {
                self.waits.borrow_mut().push(deadline_ms);
                *now = deadline_ms;
            }
        }
    }

    #[test]
    fn manual_clock_advances_only_forward_and_records_waits() {
        let c = ManualClock::new(100);
        assert_eq!(c.now_ms(), 100);
        c.sleep_until(50); // already past → no-op
        assert_eq!(c.now_ms(), 100);
        assert_eq!(c.wait_count(), 0);
        c.sleep_until(500); // advances + records
        assert_eq!(c.now_ms(), 500);
        assert_eq!(c.wait_count(), 1);
    }
}
