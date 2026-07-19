//! The Binance REST port + endpoint layout for month-to-date backfill.
//!
//! The backfill logic (see [`crate::backfill`]) runs against the [`RestSource`] trait so it is fully
//! testable offline; the real `ureq` client ([`HttpRestSource`]) is behind the `http` feature.

#[cfg(feature = "http")]
use std::io::Read;

use qe_domain::{InstrumentId, Resolution};
use thiserror::Error;

/// The default Binance USD-M futures REST base URL (no trailing slash).
pub const DEFAULT_REST_BASE: &str = "https://fapi.binance.com";

/// A REST endpoint to page over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestEndpoint {
    /// `/fapi/v1/klines` (OHLCV).
    Klines(Resolution),
    /// `/fapi/v1/markPriceKlines`.
    MarkPriceKlines(Resolution),
    /// `/fapi/v1/premiumIndexKlines`.
    PremiumIndexKlines(Resolution),
    /// `/fapi/v1/fundingRate`.
    FundingRate,
    /// `/futures/data/openInterestHist` with a sampling period (e.g. `"5m"`, `"1h"`).
    OpenInterestHist(Resolution),
}

impl RestEndpoint {
    /// The URL path (no query) for this endpoint.
    fn path(self) -> &'static str {
        match self {
            RestEndpoint::Klines(_) => "/fapi/v1/klines",
            RestEndpoint::MarkPriceKlines(_) => "/fapi/v1/markPriceKlines",
            RestEndpoint::PremiumIndexKlines(_) => "/fapi/v1/premiumIndexKlines",
            RestEndpoint::FundingRate => "/fapi/v1/fundingRate",
            RestEndpoint::OpenInterestHist(_) => "/futures/data/openInterestHist",
        }
    }

    /// The `interval`/`period` query parameter for kind that carry one.
    fn interval_param(self) -> Option<(&'static str, &'static str)> {
        match self {
            RestEndpoint::Klines(r)
            | RestEndpoint::MarkPriceKlines(r)
            | RestEndpoint::PremiumIndexKlines(r) => Some(("interval", r.as_str())),
            // `/futures/data` uses `period` rather than `interval`.
            RestEndpoint::OpenInterestHist(r) => Some(("period", r.as_str())),
            RestEndpoint::FundingRate => None,
        }
    }
}

/// A single page request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    /// The endpoint to read.
    pub endpoint: RestEndpoint,
    /// The instrument (validated; e.g. `BTCUSDT`).
    pub symbol: InstrumentId,
    /// Inclusive page start (epoch ms).
    pub start_ms: i64,
    /// Max rows to return.
    pub limit: u32,
}

impl PageRequest {
    /// The full request URL for `base` (no trailing slash, e.g. [`DEFAULT_REST_BASE`]).
    #[must_use]
    pub fn url(&self, base: &str) -> String {
        let mut url = format!(
            "{base}{}?symbol={}",
            self.endpoint.path(),
            self.symbol.as_str()
        );
        if let Some((k, v)) = self.endpoint.interval_param() {
            url.push_str(&format!("&{k}={v}"));
        }
        url.push_str(&format!(
            "&startTime={}&limit={}",
            self.start_ms, self.limit
        ));
        url
    }
}

/// A time-stamped REST row: its `open_time` (the pagination key) plus the raw JSON it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedRow {
    /// `open_time` in epoch ms (Binance element 0).
    pub open_time_ms: i64,
    /// The raw row, retained for fusion / reconciliation.
    pub raw: String,
}

/// A REST fetch failure, classified for the retry policy.
#[derive(Debug, Error)]
pub enum RestError {
    /// Rate limited (HTTP 429/418) — retry after the venue's `Retry-After`.
    #[error("rate limited; retry after {retry_after_ms}ms")]
    RateLimited {
        /// Suggested wait before retrying (ms).
        retry_after_ms: u64,
    },

    /// A transient failure (5xx, network) — safe to retry.
    #[error("transient rest error: {0}")]
    Transient(String),

    /// A non-retryable failure (4xx other than rate-limit, or a parse error).
    #[error("fatal rest error: {0}")]
    Fatal(String),
}

/// Fetches one page of time-series rows. The single network seam for backfill.
pub trait RestSource {
    /// Fetch the rows for `req`, ascending by `open_time`.
    ///
    /// # Errors
    /// [`RestError`] classified for the retry policy.
    fn fetch_page(&self, req: &PageRequest) -> Result<Vec<TimedRow>, RestError>;
}

/// Parse Binance's kline array form `[[openTime, open, high, …], …]` into [`TimedRow`]s, keeping
/// each raw row. Also handles the `/futures/data` object form `[{"timestamp": …, …}, …]`.
///
/// # Errors
/// [`RestError::Fatal`] if the bytes are not a JSON array of the expected shape.
pub fn parse_klines_json(bytes: &[u8]) -> Result<Vec<TimedRow>, RestError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| RestError::Fatal(format!("bad json: {e}")))?;
    let rows = value
        .as_array()
        .ok_or_else(|| RestError::Fatal("expected a JSON array".to_owned()))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let open_time_ms = match row {
            // Kline array form: element 0 is open time.
            serde_json::Value::Array(cols) => cols
                .first()
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| RestError::Fatal("kline row missing open_time".to_owned()))?,
            // Object form: `/futures/data` uses `timestamp`; `/fapi/v1/fundingRate` uses `fundingTime`
            // (QE-463). Accept either so the same pager drives klines, metrics, and funding.
            serde_json::Value::Object(map) => map
                .get("timestamp")
                .or_else(|| map.get("fundingTime"))
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| {
                    RestError::Fatal("data row missing timestamp/fundingTime".to_owned())
                })?,
            _ => return Err(RestError::Fatal("unexpected row shape".to_owned())),
        };
        out.push(TimedRow {
            open_time_ms,
            raw: row.to_string(),
        });
    }
    Ok(out)
}

/// The real `ureq` REST client (system TLS). Compiled only under the `http` feature.
#[cfg(feature = "http")]
pub struct HttpRestSource {
    agent: ureq::Agent,
    base: String,
}

#[cfg(feature = "http")]
impl HttpRestSource {
    /// A client against `base` (e.g. [`DEFAULT_REST_BASE`]).
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build(),
            base: base.into(),
        }
    }
}

#[cfg(feature = "http")]
impl RestSource for HttpRestSource {
    fn fetch_page(&self, req: &PageRequest) -> Result<Vec<TimedRow>, RestError> {
        match self.agent.get(&req.url(&self.base)).call() {
            Ok(resp) => {
                let mut body = Vec::new();
                resp.into_reader()
                    .read_to_end(&mut body)
                    .map_err(|e| RestError::Transient(e.to_string()))?;
                parse_klines_json(&body)
            }
            // 429/418 → rate limited; honour Retry-After (seconds) if present.
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

    fn inst(s: &str) -> InstrumentId {
        InstrumentId::new(s).unwrap()
    }

    #[test]
    fn kline_url_has_interval_and_window() {
        let req = PageRequest {
            endpoint: RestEndpoint::Klines(Resolution::M5),
            symbol: inst("BTCUSDT"),
            start_ms: 1_700_000_000_000,
            limit: 1500,
        };
        assert_eq!(
            req.url(DEFAULT_REST_BASE),
            "https://fapi.binance.com/fapi/v1/klines?symbol=BTCUSDT&interval=5m&startTime=1700000000000&limit=1500"
        );
    }

    #[test]
    fn funding_url_has_no_interval_and_oi_uses_period() {
        let funding = PageRequest {
            endpoint: RestEndpoint::FundingRate,
            symbol: inst("ETHUSDT"),
            start_ms: 100,
            limit: 1000,
        };
        assert_eq!(
            funding.url(DEFAULT_REST_BASE),
            "https://fapi.binance.com/fapi/v1/fundingRate?symbol=ETHUSDT&startTime=100&limit=1000"
        );
        let oi = PageRequest {
            endpoint: RestEndpoint::OpenInterestHist(Resolution::H1),
            symbol: inst("BTCUSDT"),
            start_ms: 0,
            limit: 500,
        };
        assert_eq!(
            oi.url(DEFAULT_REST_BASE),
            "https://fapi.binance.com/futures/data/openInterestHist?symbol=BTCUSDT&period=1h&startTime=0&limit=500"
        );
    }

    #[test]
    fn parses_kline_array_form() {
        // [openTime, open, high, low, close, volume, closeTime, ...]
        let json = br#"[[1700000000000,"42000.0","42010.0","41990.0","42005.0","12.3",1700000299999],
                        [1700000300000,"42005.0","42020.0","42000.0","42018.0","9.1",1700000599999]]"#;
        let rows = parse_klines_json(json).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].open_time_ms, 1_700_000_000_000);
        assert_eq!(rows[1].open_time_ms, 1_700_000_300_000);
        assert!(rows[0].raw.contains("42000.0"));
    }

    #[test]
    fn parses_futures_data_object_form() {
        let json = br#"[{"symbol":"BTCUSDT","sumOpenInterest":"100","timestamp":1700000000000}]"#;
        let rows = parse_klines_json(json).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].open_time_ms, 1_700_000_000_000);
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(matches!(
            parse_klines_json(b"not json"),
            Err(RestError::Fatal(_))
        ));
        assert!(matches!(
            parse_klines_json(br#"{"not":"an array"}"#),
            Err(RestError::Fatal(_))
        ));
        assert!(matches!(
            parse_klines_json(br#"[[ "no-open-time" ]]"#),
            Err(RestError::Fatal(_))
        ));
    }
}
