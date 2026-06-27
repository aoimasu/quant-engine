//! Venue weight rate limiter — a deterministic rolling-window weight budget.
//!
//! Binance-style: a venue grants a fixed weight budget per rolling window (e.g. 1200 weight / 60_000 ms).
//! Every REST call has a weight; the limiter charges it and, when the window is full, yields the instant
//! at which the request *will* fit rather than ever rejecting it. It is a pure accountant: `now_ms` in, a
//! decision out — no clock, no I/O — so the retry loop above it stays deterministic and testable.

/// Default weight budget per window (Binance USDⓈ-M REST default: 1200 / minute).
pub const DEFAULT_WEIGHT_BUDGET: u32 = 1_200;
/// Default rolling-window length in milliseconds (one minute).
pub const DEFAULT_WINDOW_MS: i64 = 60_000;

/// The limiter's verdict for an `acquire`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acquire {
    /// The weight fit in the current window and was charged — proceed now.
    Ready,
    /// The window (or a venue `Retry-After`) is blocking; wait until this epoch-ms, then re-acquire.
    /// Never a rejection — the request is delayed, not dropped.
    WaitUntil(i64),
}

/// A rolling-window weight budget honouring a venue's rate limits.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    budget: u32,
    window_ms: i64,
    window_start_ms: i64,
    used: u32,
    /// A venue-supplied floor on the next-allowed instant (from a `429`/`418` `Retry-After`).
    next_allowed_ms: i64,
}

impl RateLimiter {
    /// A limiter with an explicit `budget` per `window_ms`.
    #[must_use]
    pub fn new(budget: u32, window_ms: i64) -> Self {
        Self {
            budget,
            window_ms,
            window_start_ms: i64::MIN,
            used: 0,
            next_allowed_ms: i64::MIN,
        }
    }

    /// The Binance USDⓈ-M defaults ([`DEFAULT_WEIGHT_BUDGET`] / [`DEFAULT_WINDOW_MS`]).
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_WEIGHT_BUDGET, DEFAULT_WINDOW_MS)
    }

    /// Try to charge `weight` at `now_ms`. Rolls the window if it has elapsed; charges and returns
    /// [`Acquire::Ready`] if it fits and no venue back-off is pending; otherwise returns the earliest
    /// instant to retry ([`Acquire::WaitUntil`]). Charging only happens on `Ready`.
    pub fn acquire(&mut self, weight: u32, now_ms: i64) -> Acquire {
        // Honour a venue-imposed back-off first — it overrides our own estimate.
        if now_ms < self.next_allowed_ms {
            return Acquire::WaitUntil(self.next_allowed_ms);
        }
        // Roll the window when it (or the very first call) has elapsed.
        if now_ms >= self.window_start_ms.saturating_add(self.window_ms) {
            self.window_start_ms = now_ms;
            self.used = 0;
        }
        if self.used.saturating_add(weight) <= self.budget {
            self.used += weight;
            Acquire::Ready
        } else {
            // Wait for the window to roll, when `used` resets and the weight will fit.
            Acquire::WaitUntil(self.window_start_ms.saturating_add(self.window_ms))
        }
    }

    /// Record a venue `Retry-After`: no `acquire` may proceed before `until_ms`.
    pub fn note_retry_after(&mut self, until_ms: i64) {
        self.next_allowed_ms = self.next_allowed_ms.max(until_ms);
    }

    /// The per-window weight budget. A request whose weight exceeds this can never fit and must be
    /// rejected by the caller rather than fed to `acquire` (which would otherwise wait forever).
    #[must_use]
    pub fn budget(&self) -> u32 {
        self.budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_until_budget_then_waits_for_the_window_roll() {
        let mut rl = RateLimiter::new(10, 1_000);
        // First call seeds the window at t=0.
        assert_eq!(rl.acquire(6, 0), Acquire::Ready); // used=6
        assert_eq!(rl.acquire(4, 100), Acquire::Ready); // used=10 (== budget)
                                                        // No room left this window → wait until it rolls at window_start(0)+1000.
        assert_eq!(rl.acquire(1, 200), Acquire::WaitUntil(1_000));
        // After the roll the budget is fresh again.
        assert_eq!(rl.acquire(7, 1_000), Acquire::Ready);
    }

    #[test]
    fn window_rolls_only_after_full_window_elapses() {
        let mut rl = RateLimiter::new(5, 1_000);
        assert_eq!(rl.acquire(5, 0), Acquire::Ready);
        // 999ms later still the same window — full.
        assert_eq!(rl.acquire(1, 999), Acquire::WaitUntil(1_000));
        // Exactly at the boundary it rolls.
        assert_eq!(rl.acquire(1, 1_000), Acquire::Ready);
    }

    #[test]
    fn retry_after_floors_the_next_allowed_instant() {
        let mut rl = RateLimiter::new(100, 1_000);
        rl.note_retry_after(5_000);
        // Even with ample budget, nothing proceeds before the venue's Retry-After.
        assert_eq!(rl.acquire(1, 200), Acquire::WaitUntil(5_000));
        assert_eq!(rl.acquire(1, 5_000), Acquire::Ready);
        // A later, smaller Retry-After cannot pull the floor back in.
        rl.note_retry_after(4_000);
        assert_eq!(rl.acquire(1, 5_500), Acquire::Ready);
    }
}
