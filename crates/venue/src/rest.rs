//! The venue-aware REST client: rate-limit handler → ephemeral cache → transport.
//!
//! The client's logic is written against the [`RestTransport`] seam so it is fully testable offline; the
//! real network client ([`HttpRestTransport`]) lives behind the `http` feature. Every call is weighted and
//! flows through the [`RateLimiter`](crate::ratelimit::RateLimiter): under pressure it *backs off* (waits
//! via the [`Clock`]) rather than dropping, and immutable closed-window responses are memoised.

#[cfg(feature = "http")]
use std::io::Read;

use thiserror::Error;

use qe_domain::{InstrumentId, TimeInterval};

use crate::cache::{CacheKey, RestCache};
use crate::clock::Clock;
use crate::ratelimit::{Acquire, RateLimiter};

/// Default cap on attempts per request before the last error is surfaced (never a silent drop).
pub const DEFAULT_MAX_ATTEMPTS: u32 = 8;
/// Base backoff for transient errors (doubled per attempt).
pub const DEFAULT_TRANSIENT_BACKOFF_MS: i64 = 500;

/// A REST request to a venue endpoint over a specific data `window`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueRequest {
    /// The venue path, e.g. `"/fapi/v1/klines"`.
    pub endpoint: String,
    /// The instrument the call is about.
    pub instrument: InstrumentId,
    /// Canonical query parameters (sorted by the caller for a stable cache key), e.g.
    /// `[("interval","5m"),("limit","500")]`.
    pub params: Vec<(String, String)>,
    /// The half-open `[start, end)` data span this call covers.
    pub window: TimeInterval,
    /// The venue weight this call costs.
    pub weight: u32,
}

impl VenueRequest {
    /// Whether the request's window is fully in the past at `now_ms` (`window.end() <= now`), hence
    /// immutable and cacheable. An open/in-progress window returns `false` and is never memoised.
    #[must_use]
    pub fn is_closed_window(&self, now_ms: i64) -> bool {
        self.window.end().millis() <= now_ms
    }

    /// A canonical cache key: endpoint + instrument + sorted params + window bounds.
    #[must_use]
    pub fn cache_key(&self) -> CacheKey {
        let mut params = self.params.clone();
        params.sort();
        let joined: String = params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        format!(
            "{}|{}|{}|{}-{}",
            self.endpoint,
            self.instrument.as_str(),
            joined,
            self.window.start().millis(),
            self.window.end().millis(),
        )
    }
}

/// A successful REST response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestResponse {
    /// The raw response bytes.
    pub bytes: Vec<u8>,
}

/// A REST failure, classified for the retry policy (mirrors the offline path's classification).
#[derive(Debug, Error)]
pub enum RestError {
    /// Rate limited (HTTP 429/418) — retry after the venue's `Retry-After`.
    #[error("rate limited; retry after {retry_after_ms}ms")]
    RateLimited {
        /// Suggested wait before retrying (ms).
        retry_after_ms: u64,
    },
    /// A transient failure (5xx, network) — safe to retry with backoff.
    #[error("transient rest error: {0}")]
    Transient(String),
    /// A non-retryable failure (4xx other than rate-limit, or a parse error).
    #[error("fatal rest error: {0}")]
    Fatal(String),
    /// The retry budget was exhausted while backing off (the request was never dropped silently).
    #[error("retry budget ({attempts}) exhausted; last error: {last}")]
    Exhausted {
        /// Attempts made before giving up.
        attempts: u32,
        /// The final underlying error.
        last: String,
    },
}

/// Fetches the bytes for a venue request. The single seam between the client and the network.
pub trait RestTransport {
    /// Send `req`, returning the response body.
    ///
    /// # Errors
    /// [`RestError`] classified for the retry policy.
    fn send(&self, req: &VenueRequest) -> Result<RestResponse, RestError>;
}

/// The venue-aware REST client: weighted rate limiting, retry/backoff, and an ephemeral closed-window
/// cache, layered over a [`RestTransport`].
pub struct VenueRestClient<T: RestTransport, C: Clock> {
    transport: T,
    clock: C,
    limiter: RateLimiter,
    cache: RestCache,
    max_attempts: u32,
}

impl<T: RestTransport, C: Clock> VenueRestClient<T, C> {
    /// A client over `transport`/`clock` with the default rate limiter and retry budget.
    pub fn new(transport: T, clock: C) -> Self {
        Self {
            transport,
            clock,
            limiter: RateLimiter::with_defaults(),
            cache: RestCache::new(),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
        }
    }

    /// Override the rate limiter (e.g. a tighter test budget).
    #[must_use]
    pub fn with_limiter(mut self, limiter: RateLimiter) -> Self {
        self.limiter = limiter;
        self
    }

    /// Override the per-request attempt budget.
    #[must_use]
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Fetch one request: cache read-through (closed window) → rate-limited, retried transport →
    /// cache write-back (closed window).
    ///
    /// Under rate-limit pressure the call **waits** for the window to roll (via the clock) and retries;
    /// it never drops a request. The attempt budget bounds the loop, surfacing
    /// [`RestError::Exhausted`] instead of looping forever.
    ///
    /// # Errors
    /// [`RestError::Fatal`] for a non-retryable failure, or [`RestError::Exhausted`] if the retry budget
    /// is spent while backing off.
    pub fn fetch(&mut self, req: &VenueRequest) -> Result<RestResponse, RestError> {
        let cacheable = req.is_closed_window(self.clock.now_ms());
        if cacheable {
            if let Some(bytes) = self.cache.get(&req.cache_key()) {
                return Ok(RestResponse { bytes });
            }
        }

        let mut attempt = 0u32;
        let mut last = String::new();
        while attempt < self.max_attempts {
            // Rate-limit gate: wait (back off) until the weight fits, then charge it.
            loop {
                let now = self.clock.now_ms();
                match self.limiter.acquire(req.weight, now) {
                    Acquire::Ready => break,
                    Acquire::WaitUntil(deadline) => self.clock.sleep_until(deadline),
                }
            }

            attempt += 1;
            match self.transport.send(req) {
                Ok(resp) => {
                    if cacheable {
                        self.cache.put(req.cache_key(), resp.bytes.clone());
                    }
                    return Ok(resp);
                }
                Err(RestError::RateLimited { retry_after_ms }) => {
                    let now = self.clock.now_ms();
                    let deadline = now.saturating_add(retry_after_ms as i64);
                    self.limiter.note_retry_after(deadline);
                    self.clock.sleep_until(deadline);
                    last = format!("rate limited ({retry_after_ms}ms)");
                }
                Err(RestError::Transient(msg)) => {
                    let backoff =
                        DEFAULT_TRANSIENT_BACKOFF_MS.saturating_mul(1i64 << (attempt - 1).min(16));
                    self.clock
                        .sleep_until(self.clock.now_ms().saturating_add(backoff));
                    last = format!("transient: {msg}");
                }
                Err(e @ (RestError::Fatal(_) | RestError::Exhausted { .. })) => return Err(e),
            }
        }
        Err(RestError::Exhausted {
            attempts: self.max_attempts,
            last,
        })
    }

    /// Drive a paginated fetch: starting from `first`, `fetch` each page, then call `next(&prev_page,
    /// &prev_req)` to produce the following request (or `None` to stop). Each page is independently
    /// rate-limited, retried, and cached.
    ///
    /// # Errors
    /// Propagates the first page error.
    pub fn paginate<F>(
        &mut self,
        first: VenueRequest,
        mut next: F,
    ) -> Result<Vec<RestResponse>, RestError>
    where
        F: FnMut(&RestResponse, &VenueRequest) -> Option<VenueRequest>,
    {
        let mut pages = Vec::new();
        let mut req = first;
        loop {
            let resp = self.fetch(&req)?;
            let follow = next(&resp, &req);
            pages.push(resp);
            match follow {
                Some(n) => req = n,
                None => break,
            }
        }
        Ok(pages)
    }

    /// Read-only access to the ephemeral cache (e.g. for assertions / introspection).
    #[must_use]
    pub fn cache(&self) -> &RestCache {
        &self.cache
    }
}

/// The real `ureq` REST transport (system TLS). Compiled only under the `http` feature.
#[cfg(feature = "http")]
pub struct HttpRestTransport {
    agent: ureq::Agent,
    base: String,
}

#[cfg(feature = "http")]
impl HttpRestTransport {
    /// A transport against `base` (e.g. `"https://fapi.binance.com"`).
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build(),
            base: base.into(),
        }
    }

    fn url(&self, req: &VenueRequest) -> String {
        let mut url = format!("{}{}", self.base, req.endpoint);
        if !req.params.is_empty() {
            let q: Vec<String> = req.params.iter().map(|(k, v)| format!("{k}={v}")).collect();
            url.push('?');
            url.push_str(&q.join("&"));
        }
        url
    }
}

#[cfg(feature = "http")]
impl RestTransport for HttpRestTransport {
    fn send(&self, req: &VenueRequest) -> Result<RestResponse, RestError> {
        match self.agent.get(&self.url(req)).call() {
            Ok(resp) => {
                let mut bytes = Vec::new();
                resp.into_reader()
                    .read_to_end(&mut bytes)
                    .map_err(|e| RestError::Transient(e.to_string()))?;
                Ok(RestResponse { bytes })
            }
            Err(ureq::Error::Status(code, resp)) if code == 429 || code == 418 => {
                let retry_after_ms = resp
                    .header("Retry-After")
                    .and_then(|s| s.parse::<u64>().ok())
                    .map_or(1_000, |secs| secs * 1_000);
                Err(RestError::RateLimited { retry_after_ms })
            }
            Err(ureq::Error::Status(code, _)) if code >= 500 => {
                Err(RestError::Transient(format!("http {code}")))
            }
            Err(ureq::Error::Status(code, _)) => Err(RestError::Fatal(format!("http {code}"))),
            Err(e) => Err(RestError::Transient(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::test_support::ManualClock;
    use crate::ratelimit::RateLimiter;
    use qe_domain::Timestamp;
    use std::cell::RefCell;

    /// A scripted in-memory transport: per call it pops the next outcome from a queue and counts hits.
    struct FakeTransport {
        /// Outcomes to serve in order; the last is repeated once the queue drains.
        script: RefCell<Vec<Outcome>>,
        hits: RefCell<usize>,
    }

    #[derive(Clone)]
    enum Outcome {
        Ok(Vec<u8>),
        RateLimited(u64),
        Transient,
        Fatal,
    }

    impl FakeTransport {
        fn new(script: Vec<Outcome>) -> Self {
            Self {
                script: RefCell::new(script),
                hits: RefCell::new(0),
            }
        }

        fn hits(&self) -> usize {
            *self.hits.borrow()
        }
    }

    impl RestTransport for FakeTransport {
        fn send(&self, _req: &VenueRequest) -> Result<RestResponse, RestError> {
            *self.hits.borrow_mut() += 1;
            let mut script = self.script.borrow_mut();
            let outcome = if script.len() > 1 {
                script.remove(0)
            } else {
                script.first().cloned().unwrap_or(Outcome::Ok(b"".to_vec()))
            };
            match outcome {
                Outcome::Ok(b) => Ok(RestResponse { bytes: b }),
                Outcome::RateLimited(ms) => Err(RestError::RateLimited { retry_after_ms: ms }),
                Outcome::Transient => Err(RestError::Transient("boom".to_owned())),
                Outcome::Fatal => Err(RestError::Fatal("nope".to_owned())),
            }
        }
    }

    fn inst() -> InstrumentId {
        InstrumentId::new("BTCUSDT").unwrap()
    }

    /// A request whose `[start,end)` window ends at `end_ms`.
    fn req_window(weight: u32, start_ms: i64, end_ms: i64) -> VenueRequest {
        VenueRequest {
            endpoint: "/fapi/v1/klines".to_owned(),
            instrument: inst(),
            params: vec![("interval".to_owned(), "5m".to_owned())],
            window: TimeInterval::new(
                Timestamp::from_millis(start_ms),
                Timestamp::from_millis(end_ms),
            )
            .unwrap(),
            weight,
        }
    }

    #[test]
    fn rate_limit_pressure_backs_off_without_dropping() {
        // Budget = 2 weight / 1000ms window; each request costs 1. Firing 5 requests overflows the
        // window twice, so the client must WAIT (back off) — but every request still reaches the
        // transport (none dropped).
        let transport = FakeTransport::new(vec![Outcome::Ok(b"ok".to_vec())]);
        let clock = ManualClock::new(0);
        let mut client =
            VenueRestClient::new(transport, clock).with_limiter(RateLimiter::new(2, 1_000));

        for i in 0..5 {
            // Distinct open windows so nothing is served from cache — each must hit the transport.
            let r = req_window(1, i, i_64_max());
            assert!(client.fetch(&r).is_ok(), "request {i} must complete");
        }
        assert_eq!(
            client.transport.hits(),
            5,
            "all 5 requests reached the transport (none dropped)"
        );
        assert!(
            client.clock.wait_count() >= 2,
            "the limiter backed off when the window filled"
        );
    }

    #[test]
    fn rate_limited_response_is_honoured_and_retried_not_dropped() {
        // Two 429s (Retry-After 2000ms) then success → the client waits and retries, returns Ok.
        let transport = FakeTransport::new(vec![
            Outcome::RateLimited(2_000),
            Outcome::RateLimited(2_000),
            Outcome::Ok(b"done".to_vec()),
        ]);
        let clock = ManualClock::new(0);
        let mut client = VenueRestClient::new(transport, clock);

        let r = req_window(1, 0, i_64_max());
        assert_eq!(client.fetch(&r).unwrap().bytes, b"done");
        assert_eq!(
            client.transport.hits(),
            3,
            "two 429s then a success — all attempted"
        );
        assert!(
            client.clock.wait_count() >= 2,
            "each 429 forced a backoff wait"
        );
    }

    #[test]
    fn closed_window_is_served_from_cache_on_repeat() {
        // Window ends at t=1000; clock at t=5000 → window is closed (immutable) → cacheable.
        let transport = FakeTransport::new(vec![Outcome::Ok(b"hist".to_vec())]);
        let clock = ManualClock::new(5_000);
        let mut client = VenueRestClient::new(transport, clock);

        let r = req_window(1, 0, 1_000);
        assert_eq!(client.fetch(&r).unwrap().bytes, b"hist");
        assert_eq!(client.transport.hits(), 1);
        // Identical repeat → served from cache, transport not hit again.
        assert_eq!(client.fetch(&r).unwrap().bytes, b"hist");
        assert_eq!(
            client.transport.hits(),
            1,
            "closed-window repeat served from cache"
        );
        assert_eq!(client.cache().len(), 1);
    }

    #[test]
    fn open_window_is_not_cached() {
        // Window ends at t=10_000 but clock is at t=5_000 → still open → not cacheable.
        let transport = FakeTransport::new(vec![Outcome::Ok(b"live".to_vec())]);
        let clock = ManualClock::new(5_000);
        let mut client = VenueRestClient::new(transport, clock);

        let r = req_window(1, 0, 10_000);
        client.fetch(&r).unwrap();
        client.fetch(&r).unwrap();
        assert_eq!(
            client.transport.hits(),
            2,
            "open window re-fetched, never cached"
        );
        assert!(client.cache().is_empty());
    }

    #[test]
    fn transient_retries_then_succeeds_and_fatal_stops() {
        let transport = FakeTransport::new(vec![Outcome::Transient, Outcome::Ok(b"ok".to_vec())]);
        let mut client = VenueRestClient::new(transport, ManualClock::new(0));
        assert_eq!(
            client.fetch(&req_window(1, 0, i_64_max())).unwrap().bytes,
            b"ok"
        );

        let fatal = FakeTransport::new(vec![Outcome::Fatal]);
        let mut client2 = VenueRestClient::new(fatal, ManualClock::new(0));
        assert!(matches!(
            client2.fetch(&req_window(1, 0, i_64_max())),
            Err(RestError::Fatal(_))
        ));
        assert_eq!(client2.transport.hits(), 1, "a fatal error is not retried");
    }

    #[test]
    fn exhausts_retry_budget_without_silent_drop() {
        // Permanent 429 → bounded retries then a surfaced Exhausted error (never a silent drop).
        let transport = FakeTransport::new(vec![Outcome::RateLimited(100)]);
        let mut client = VenueRestClient::new(transport, ManualClock::new(0)).with_max_attempts(3);
        let err = client.fetch(&req_window(1, 0, i_64_max())).unwrap_err();
        assert!(matches!(err, RestError::Exhausted { attempts: 3, .. }));
        assert_eq!(client.transport.hits(), 3);
    }

    #[test]
    fn paginate_drives_pages_each_rate_limited() {
        // Three pages of one byte each; stop after the third.
        let transport = FakeTransport::new(vec![Outcome::Ok(b"p".to_vec())]);
        let mut client = VenueRestClient::new(transport, ManualClock::new(0));
        let first = req_window(1, 0, i_64_max());
        let pages = client
            .paginate(first, |_resp, prev| {
                // Advance the window forward; stop once we've gathered 3 pages.
                let next_start = prev.window.start().millis() + 1;
                if next_start >= 3 {
                    None
                } else {
                    Some(req_window(1, next_start, i_64_max()))
                }
            })
            .unwrap();
        assert_eq!(pages.len(), 3);
        assert_eq!(client.transport.hits(), 3);
    }

    /// A far-future window end so a request is treated as an open (uncacheable) window.
    fn i_64_max() -> i64 {
        i64::MAX
    }
}
