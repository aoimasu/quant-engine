//! The paginated, retried month-to-date backfiller.
//!
//! Pages a [`RestSource`] forward from the vendor's right edge to `now`, retaining the vendor↔REST
//! overlap region for reconciliation (QE-103). Pagination, the retry policy, and the **rate-limit
//! backoff** all run against ports ([`RestSource`] + [`Sleeper`]), so the whole flow — including the
//! `Retry-After` wait — is tested offline without real sleeping.

use qe_domain::InstrumentId;

use crate::rest::{PageRequest, RestEndpoint, RestError, RestSource, TimedRow};
use crate::IngestError;

/// Waits between retries. Injected so the backoff is honoured in production yet instant + assertable
/// in tests (a fake records the requested durations).
pub trait Sleeper {
    /// Block for `ms` milliseconds (a no-op for `ms == 0`).
    fn sleep_ms(&self, ms: u64);
}

/// The production sleeper — `std::thread::sleep`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealSleeper;

impl Sleeper for RealSleeper {
    fn sleep_ms(&self, ms: u64) {
        if ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
}

/// Bounded-retry policy with backoff. `Fatal` errors are never retried; `RateLimited`/`Transient`
/// are retried up to `max_retries` times, **waiting first** (the venue's `Retry-After` for a rate
/// limit, else a linear `base_delay_ms` backoff) so a 429 is not hammered. (QE-201 will formalise
/// this into the shared venue handler.)
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Max retry attempts after the first try.
    pub max_retries: u32,
    /// Base backoff for a `Transient` error (multiplied by the attempt number); also the floor for a
    /// rate-limit wait when the venue supplied no `Retry-After`.
    pub base_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_delay_ms: 500,
        }
    }
}

/// A month-to-date backfill request: fill `[from_ms, now_ms]` plus an `overlap_ms` look-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillRequest {
    /// Endpoint to page.
    pub endpoint: RestEndpoint,
    /// Instrument.
    pub symbol: InstrumentId,
    /// Bar interval in ms (the pagination step and the AC #1 freshness tolerance).
    pub interval_ms: i64,
    /// The vendor's right edge — `fresh` rows start here.
    pub from_ms: i64,
    /// "Now" (epoch ms), passed in for determinism.
    pub now_ms: i64,
    /// How far before `from_ms` to re-fetch, retained as the reconciliation overlap.
    pub overlap_ms: i64,
    /// Page size.
    pub limit: u32,
}

/// The result of a backfill.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillResult {
    /// Rows at/after `from_ms` — these extend the corpus toward now.
    pub fresh: Vec<TimedRow>,
    /// Rows before `from_ms` (the vendor↔REST overlap) — retained for QE-103 diffing.
    pub overlap: Vec<TimedRow>,
    /// The newest `open_time` fetched, if any.
    pub latest_open_time_ms: Option<i64>,
}

/// Pages a [`RestSource`] forward to now under a [`RetryPolicy`], waiting via a [`Sleeper`].
pub struct Backfiller<S: RestSource, Sl: Sleeper = RealSleeper> {
    source: S,
    retry: RetryPolicy,
    sleeper: Sl,
}

impl<S: RestSource> Backfiller<S, RealSleeper> {
    /// A backfiller over `source` with `retry`, using the real (thread-sleeping) backoff.
    pub fn new(source: S, retry: RetryPolicy) -> Self {
        Self {
            source,
            retry,
            sleeper: RealSleeper,
        }
    }
}

impl<S: RestSource, Sl: Sleeper> Backfiller<S, Sl> {
    /// A backfiller with an injected `sleeper` (tests use a recording/no-op one).
    pub fn with_sleeper(source: S, retry: RetryPolicy, sleeper: Sl) -> Self {
        Self {
            source,
            retry,
            sleeper,
        }
    }

    /// Fetch one page, retrying retryable errors up to the policy's `max_retries` and **waiting**
    /// before each retry: the rate limit's `Retry-After` (floored at `base_delay_ms`), or a linear
    /// `base_delay_ms × attempt` backoff for a transient error. `Fatal` is not retried.
    fn fetch_with_retry(&self, req: &PageRequest) -> Result<Vec<TimedRow>, IngestError> {
        let mut attempts = 0;
        loop {
            let wait_ms = match self.source.fetch_page(req) {
                Ok(rows) => return Ok(rows),
                Err(RestError::Fatal(m)) => return Err(IngestError::Rest(format!("fatal: {m}"))),
                Err(e @ (RestError::RateLimited { .. } | RestError::Transient(_))) => {
                    if attempts >= self.retry.max_retries {
                        return Err(IngestError::Rest(format!(
                            "giving up after {attempts} retries: {e}"
                        )));
                    }
                    attempts += 1;
                    match e {
                        RestError::RateLimited { retry_after_ms } => {
                            retry_after_ms.max(self.retry.base_delay_ms)
                        }
                        // Linear backoff for a transient blip.
                        _ => self.retry.base_delay_ms * u64::from(attempts),
                    }
                }
            };
            self.sleeper.sleep_ms(wait_ms);
        }
    }

    /// Backfill `req`, returning the fresh rows, the retained overlap, and the right edge reached.
    ///
    /// # Errors
    /// [`IngestError::Rest`] if a page fails fatally or exhausts the retry policy.
    pub fn backfill(&self, req: &BackfillRequest) -> Result<BackfillResult, IngestError> {
        let mut all: Vec<TimedRow> = Vec::new();
        let mut cursor = req.from_ms.saturating_sub(req.overlap_ms);
        // The freshness target: the newest row should be within one interval of now.
        let target = req.now_ms - req.interval_ms;

        loop {
            let page = self.fetch_with_retry(&PageRequest {
                endpoint: req.endpoint,
                symbol: req.symbol.clone(),
                start_ms: cursor,
                limit: req.limit,
            })?;
            if page.is_empty() {
                break;
            }
            // Keep only rows that advance past what we already have (guards against page overlap).
            let last_have = all.last().map_or(i64::MIN, |r| r.open_time_ms);
            let mut progressed = false;
            for row in page {
                if row.open_time_ms > last_have {
                    progressed = true;
                    all.push(row);
                }
            }
            let newest = all.last().map_or(cursor, |r| r.open_time_ms);
            // Stop when we've reached the freshness target or the page made no forward progress.
            if !progressed || newest >= target {
                break;
            }
            cursor = newest + req.interval_ms;
        }

        let latest_open_time_ms = all.last().map(|r| r.open_time_ms);
        let (overlap, fresh): (Vec<_>, Vec<_>) =
            all.into_iter().partition(|r| r.open_time_ms < req.from_ms);
        Ok(BackfillResult {
            fresh,
            overlap,
            latest_open_time_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::Resolution;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    const MIN: i64 = 60_000;

    fn inst() -> InstrumentId {
        InstrumentId::new("BTCUSDT").unwrap()
    }

    /// A fake REST source that paginates a fixed ascending dataset (rows with `open_time >=
    /// start_ms`, up to `limit`), optionally erroring on the first scripted calls.
    struct FakeRest {
        rows: Vec<TimedRow>,
        errors: RefCell<VecDeque<RestError>>,
        calls: RefCell<usize>,
    }

    impl FakeRest {
        fn new(open_times: &[i64]) -> Self {
            Self {
                rows: open_times
                    .iter()
                    .map(|&t| TimedRow {
                        open_time_ms: t,
                        raw: format!("[{t}]"),
                    })
                    .collect(),
                errors: RefCell::new(VecDeque::new()),
                calls: RefCell::new(0),
            }
        }
        fn with_errors(mut self, errs: Vec<RestError>) -> Self {
            self.errors = RefCell::new(errs.into());
            self
        }
    }

    impl RestSource for FakeRest {
        fn fetch_page(&self, req: &PageRequest) -> Result<Vec<TimedRow>, RestError> {
            *self.calls.borrow_mut() += 1;
            if let Some(err) = self.errors.borrow_mut().pop_front() {
                return Err(err);
            }
            Ok(self
                .rows
                .iter()
                .filter(|r| r.open_time_ms >= req.start_ms)
                .take(req.limit as usize)
                .cloned()
                .collect())
        }
    }

    /// A sleeper that records requested waits instead of blocking (proves backoff without slowness).
    #[derive(Default)]
    struct RecordingSleeper {
        waits: RefCell<Vec<u64>>,
    }
    impl Sleeper for RecordingSleeper {
        fn sleep_ms(&self, ms: u64) {
            self.waits.borrow_mut().push(ms);
        }
    }

    fn req(from: i64, now: i64, overlap: i64) -> BackfillRequest {
        BackfillRequest {
            endpoint: RestEndpoint::Klines(Resolution::M1),
            symbol: inst(),
            interval_ms: MIN,
            from_ms: from,
            now_ms: now,
            overlap_ms: overlap,
            limit: 2, // small → forces multiple pages
        }
    }

    #[test]
    fn paginates_to_within_one_interval_of_now() {
        // Rows every minute from t=0..=10min; now = 10min. Target = now - interval = 9min.
        let times: Vec<i64> = (0..=10).map(|i| i * MIN).collect();
        let bf = Backfiller::new(FakeRest::new(&times), RetryPolicy::default());
        let res = bf.backfill(&req(0, 10 * MIN, 0)).unwrap();

        // AC #1: newest bar within one interval of now.
        let latest = res.latest_open_time_ms.unwrap();
        assert!(
            latest >= 10 * MIN - MIN,
            "latest {latest} must be within one interval of now"
        );
        // Multiple pages were needed (limit = 2 over 11 rows).
        assert!(*bf.source.calls.borrow() >= 5);
    }

    #[test]
    fn retains_overlap_region_before_from() {
        // Dataset spans 0..=10min; vendor right edge from = 5min; overlap = 2 intervals (back to 3min).
        let times: Vec<i64> = (0..=10).map(|i| i * MIN).collect();
        let bf = Backfiller::new(FakeRest::new(&times), RetryPolicy::default());
        let res = bf.backfill(&req(5 * MIN, 10 * MIN, 2 * MIN)).unwrap();

        // AC #2: overlap holds the rows in [from-overlap, from) = {3min, 4min}; fresh holds >= 5min.
        let overlap_times: Vec<i64> = res.overlap.iter().map(|r| r.open_time_ms).collect();
        assert_eq!(overlap_times, vec![3 * MIN, 4 * MIN]);
        assert!(res.fresh.iter().all(|r| r.open_time_ms >= 5 * MIN));
        assert!(res.fresh.iter().any(|r| r.open_time_ms == 10 * MIN));
    }

    #[test]
    fn retries_rate_limit_then_succeeds_and_waits_retry_after() {
        let times: Vec<i64> = (0..=3).map(|i| i * MIN).collect();
        let source = FakeRest::new(&times).with_errors(vec![
            RestError::RateLimited {
                retry_after_ms: 2_000,
            },
            RestError::Transient("blip".to_owned()),
        ]);
        let policy = RetryPolicy {
            max_retries: 3,
            base_delay_ms: 100,
        };
        let bf = Backfiller::with_sleeper(source, policy, RecordingSleeper::default());
        let res = bf.backfill(&req(0, 3 * MIN, 0)).unwrap();
        assert_eq!(res.latest_open_time_ms, Some(3 * MIN));

        // The rate limit's Retry-After (2000ms, ≥ base) is honoured, then the transient linear backoff
        // (base 100 × cumulative attempt 2 = 200).
        let waits = bf.sleeper.waits.borrow();
        assert_eq!(waits[0], 2_000, "must wait the venue's Retry-After");
        assert_eq!(waits[1], 200, "transient backoff = base × attempt(2)");
    }

    #[test]
    fn gives_up_after_exhausting_retries() {
        let source = FakeRest::new(&[0, MIN]).with_errors(vec![
            RestError::Transient("a".to_owned()),
            RestError::Transient("b".to_owned()),
            RestError::Transient("c".to_owned()),
        ]);
        let bf = Backfiller::with_sleeper(
            source,
            RetryPolicy {
                max_retries: 1,
                base_delay_ms: 0,
            },
            RecordingSleeper::default(),
        );
        let err = bf.backfill(&req(0, MIN, 0)).unwrap_err();
        assert!(matches!(err, IngestError::Rest(_)));
    }

    #[test]
    fn fatal_error_is_not_retried() {
        let source = FakeRest::new(&[0, MIN]).with_errors(vec![RestError::Fatal("bad".to_owned())]);
        let bf = Backfiller::with_sleeper(
            source,
            RetryPolicy {
                max_retries: 5,
                base_delay_ms: 0,
            },
            RecordingSleeper::default(),
        );
        let err = bf.backfill(&req(0, MIN, 0)).unwrap_err();
        assert!(matches!(err, IngestError::Rest(_)));
        assert_eq!(*bf.source.calls.borrow(), 1, "fatal must not retry");
        assert!(bf.sleeper.waits.borrow().is_empty(), "fatal must not wait");
    }
}
