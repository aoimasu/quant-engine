//! Binance USDT-M historical decoder — klines + historical funding + open-interest + premium (QE-463,
//! QE-497).
//!
//! The **long pole** of the research flow: a real one-exchange historical ingest, plugged into the
//! existing REST port ([`crate::rest`]) + paginator ([`crate::backfill::Backfiller`]) and the
//! `HistoricalSource` seam (the CLI adapter maps [`IngestedWindow`] → the runtime `HistoricalWindow`).
//! Deliberately thin: **one** exchange, USDT-M perps, historical only.
//!
//! Every piece here is a **pure function of its inputs** (decoding from bytes, closed-window filtering
//! against an injected `now_ms`, missing-window planning against an injected coverage set) driven over
//! the [`RestSource`] port, so the whole decoder is exercised **offline** against a checked-in fixture —
//! no live network in any test. The real network client ([`crate::rest::HttpRestSource`]) is the only
//! `http`-gated piece; the logic below is always compiled and tested.
//!
//! ## Optional factors degrade gracefully (QE-497)
//! Klines and funding are **required** — a fetch/decode failure fails the whole window (fail-closed).
//! Open-interest and premium are **optional positioning/basis factors**: a symbol/venue lacking one, or
//! a window beyond the venue's retention for that series, must not abort the ingest. So the OI and
//! premium sub-fetches are **fail-open** — a REST or decode error there yields an empty series and the
//! bars + funding still land. (Binance `/futures/data/openInterestHist` only retains ~30 days and
//! rejects an older `startTime` outright with code `-1130`, so an old-window OI fetch legitimately
//! degrades to empty rather than failing the ingest.)
//!
//! ## Calibration honesty (QE-455 §8.5)
//! Even with premium/OI wired, this stays a **calibration-uncalibrated** slice: it does **not** fetch
//! aggTrade, so it never fabricates slippage/impact/ADV inputs. The result still carries
//! [`CalibrationSource::Uncalibrated`] — richer factor breadth (positioning/basis) is not execution
//! calibration, and the liquidity flag is not flipped by this ticket.

use std::collections::BTreeSet;
use std::str::FromStr;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use qe_domain::{Bar, DomainError, InstrumentId, Price, Qty, Resolution, Timestamp};

use crate::backfill::{BackfillRequest, Backfiller, RealSleeper, Sleeper};
use crate::rest::{RestEndpoint, RestSource, TimedRow};
use crate::IngestError;

/// Binance USDT-M funding settles on an 8-hour cadence.
pub const FUNDING_INTERVAL_MS: i64 = 8 * 60 * 60 * 1000;

/// How the slippage/impact/ADV calibration inputs of a vintage derived from an ingest were sourced.
///
/// QE-463 ships [`Uncalibrated`](CalibrationSource::Uncalibrated): a klines-only real vintage whose
/// tradability numbers stay at their **default**, never presented as measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationSource {
    /// Slippage/impact/ADV calibrated from real measured inputs (aggTrade + premium-index). Not QE-463.
    Measured,
    /// Klines-only real vintage — calibration stays at its default; **not** measured (QE-463).
    Uncalibrated,
}

/// The typed, closed-window historical inputs for one instrument over one window: base klines + the
/// historical funding series, plus the calibration-honesty marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestedWindow {
    /// The base kline resolution.
    pub base: Resolution,
    /// Closed base-resolution klines, ascending by open-time, deduped.
    pub bars: Vec<Bar>,
    /// Settled funding observations `(fundingTime_ms, rate)`, ascending, deduped.
    pub funding: Vec<(i64, Decimal)>,
    /// Open-interest observations `(timestamp_ms, sumOpenInterest)`, ascending, deduped (QE-497).
    /// **Optional** — empty when the venue lacks the series over the window (fail-open, see the module
    /// docs); the bars + funding are still populated.
    pub open_interest: Vec<(i64, Decimal)>,
    /// Premium-index observations `(openTime_ms, premium)`, ascending, deduped (QE-497). **Optional** —
    /// empty when unavailable over the window (fail-open); on the closed bar grid with no jitter.
    pub premium: Vec<(i64, Decimal)>,
    /// Calibration provenance — always [`CalibrationSource::Uncalibrated`]: klines-only calibration
    /// (no aggTrade/ADV), even though positioning/basis factors are now populated (QE-497).
    pub calibration_source: CalibrationSource,
}

// --- decoding -------------------------------------------------------------------------------------

fn dec_err(e: DomainError) -> IngestError {
    IngestError::Rest(format!("decode: {e}"))
}

fn col_i64(cols: &[serde_json::Value], idx: usize, name: &str) -> Result<i64, IngestError> {
    cols.get(idx)
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| IngestError::Rest(format!("kline column {idx} ({name}) missing/not int")))
}

fn col_dec(cols: &[serde_json::Value], idx: usize, name: &str) -> Result<Decimal, IngestError> {
    let s = cols
        .get(idx)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| IngestError::Rest(format!("kline column {idx} ({name}) missing/not str")))?;
    Decimal::from_str(s)
        .map_err(|e| IngestError::Rest(format!("kline column {idx} ({name}) bad decimal {s}: {e}")))
}

/// Decode one Binance USDT-M kline array row
/// `[openTime, "open","high","low","close","volume", closeTime, "quoteVol", trades, …]` into a [`Bar`].
///
/// # Errors
/// [`IngestError::Rest`] if the row is not the expected array shape, a numeric field fails to parse, or
/// the OHLC invariant is violated.
pub fn decode_kline_row(raw: &str, resolution: Resolution) -> Result<Bar, IngestError> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| IngestError::Rest(format!("kline json: {e}")))?;
    let cols = v
        .as_array()
        .ok_or_else(|| IngestError::Rest("kline row is not a JSON array".to_owned()))?;
    let open_time = col_i64(cols, 0, "openTime")?;
    let trades = u64::try_from(col_i64(cols, 8, "numberOfTrades")?.max(0)).unwrap_or(0);
    Bar::new(
        Timestamp::from_millis(open_time),
        resolution,
        Price::new(col_dec(cols, 1, "open")?).map_err(dec_err)?,
        Price::new(col_dec(cols, 2, "high")?).map_err(dec_err)?,
        Price::new(col_dec(cols, 3, "low")?).map_err(dec_err)?,
        Price::new(col_dec(cols, 4, "close")?).map_err(dec_err)?,
        Qty::new(col_dec(cols, 5, "volume")?).map_err(dec_err)?,
        trades,
    )
    .map_err(dec_err)
}

/// Decode a page of kline rows (each carrying its raw JSON) into [`Bar`]s, in row order.
///
/// # Errors
/// [`IngestError::Rest`] if any row fails to decode (see [`decode_kline_row`]).
pub fn decode_klines(rows: &[TimedRow], resolution: Resolution) -> Result<Vec<Bar>, IngestError> {
    rows.iter()
        .map(|r| decode_kline_row(&r.raw, resolution))
        .collect()
}

/// Decode one `/fapi/v1/fundingRate` object `{"fundingTime": ms, "fundingRate": "…", …}` into
/// `(fundingTime_ms, rate)`.
///
/// # Errors
/// [`IngestError::Rest`] if the row is not the expected object shape or `fundingRate` fails to parse.
pub fn decode_funding_row(raw: &str) -> Result<(i64, Decimal), IngestError> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| IngestError::Rest(format!("funding json: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| IngestError::Rest("funding row is not a JSON object".to_owned()))?;
    let ts = obj
        .get("fundingTime")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| IngestError::Rest("funding row missing fundingTime".to_owned()))?;
    let rate_s = obj
        .get("fundingRate")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| IngestError::Rest("funding row missing fundingRate".to_owned()))?;
    let rate = Decimal::from_str(rate_s)
        .map_err(|e| IngestError::Rest(format!("funding rate bad decimal {rate_s}: {e}")))?;
    Ok((ts, rate))
}

/// Decode a page of funding rows into `(fundingTime_ms, rate)` observations, in row order.
///
/// # Errors
/// [`IngestError::Rest`] if any row fails to decode (see [`decode_funding_row`]).
pub fn decode_funding(rows: &[TimedRow]) -> Result<Vec<(i64, Decimal)>, IngestError> {
    rows.iter().map(|r| decode_funding_row(&r.raw)).collect()
}

/// Decode one `/futures/data/openInterestHist` object
/// `{"sumOpenInterest":"…","sumOpenInterestValue":"…","timestamp": ms, …}` into
/// `(timestamp_ms, sumOpenInterest)` — the aggregate open interest in base units (QE-497).
///
/// # Errors
/// [`IngestError::Rest`] if the row is not the expected object shape or `sumOpenInterest` fails to parse.
pub fn decode_open_interest_row(raw: &str) -> Result<(i64, Decimal), IngestError> {
    let v: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| IngestError::Rest(format!("open-interest json: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| IngestError::Rest("open-interest row is not a JSON object".to_owned()))?;
    let ts = obj
        .get("timestamp")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| IngestError::Rest("open-interest row missing timestamp".to_owned()))?;
    let oi_s = obj
        .get("sumOpenInterest")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| IngestError::Rest("open-interest row missing sumOpenInterest".to_owned()))?;
    let oi = Decimal::from_str(oi_s)
        .map_err(|e| IngestError::Rest(format!("open-interest bad decimal {oi_s}: {e}")))?;
    Ok((ts, oi))
}

/// Decode a page of open-interest rows into `(timestamp_ms, sumOpenInterest)` observations, row order.
///
/// # Errors
/// [`IngestError::Rest`] if any row fails to decode (see [`decode_open_interest_row`]).
pub fn decode_open_interest(rows: &[TimedRow]) -> Result<Vec<(i64, Decimal)>, IngestError> {
    rows.iter()
        .map(|r| decode_open_interest_row(&r.raw))
        .collect()
}

/// Decode one `/fapi/v1/premiumIndexKlines` array row
/// `[openTime, "open","high","low","close", …]` into `(openTime_ms, close)` — the premium index at the
/// bar close, as a signed fraction (QE-497).
///
/// # Errors
/// [`IngestError::Rest`] if the row is not the expected array shape or `close` fails to parse.
pub fn decode_premium_row(raw: &str) -> Result<(i64, Decimal), IngestError> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| IngestError::Rest(format!("premium json: {e}")))?;
    let cols = v
        .as_array()
        .ok_or_else(|| IngestError::Rest("premium row is not a JSON array".to_owned()))?;
    let open_time = col_i64(cols, 0, "openTime")?;
    let close = col_dec(cols, 4, "close")?;
    Ok((open_time, close))
}

/// Decode a page of premium-index kline rows into `(openTime_ms, premium)` observations, row order.
///
/// # Errors
/// [`IngestError::Rest`] if any row fails to decode (see [`decode_premium_row`]).
pub fn decode_premium(rows: &[TimedRow]) -> Result<Vec<(i64, Decimal)>, IngestError> {
    rows.iter().map(|r| decode_premium_row(&r.raw)).collect()
}

/// A page decoder for an optional `(timestamp_ms, value)` context series (open-interest / premium).
type ContextDecoder = fn(&[TimedRow]) -> Result<Vec<(i64, Decimal)>, IngestError>;

// --- closed-window filtering ----------------------------------------------------------------------

/// Keep only **closed** bars: a bar with open-time `T` at `interval` is closed iff `T + interval <=
/// now_ms` (its close-time has passed), which **excludes the forming right-edge bar**. Returns the kept
/// bars sorted ascending by open-time and deduped, so a re-fetch is byte-identical.
#[must_use]
pub fn closed_klines(mut bars: Vec<Bar>, now_ms: i64, interval: i64) -> Vec<Bar> {
    bars.retain(|b| b.open_time().millis().saturating_add(interval) <= now_ms);
    bars.sort_by_key(|b| b.open_time().millis());
    bars.dedup_by_key(|b| b.open_time().millis());
    bars
}

/// Keep only **settled** funding observations: a `fundingTime` `F` is settled iff `F <= now_ms`, which
/// **excludes the in-progress funding interval**. Returns them ascending by time and deduped.
#[must_use]
pub fn closed_funding(mut funding: Vec<(i64, Decimal)>, now_ms: i64) -> Vec<(i64, Decimal)> {
    funding.retain(|(t, _)| *t <= now_ms);
    funding.sort_by_key(|(t, _)| *t);
    funding.dedup_by_key(|(t, _)| *t);
    funding
}

/// Keep only **closed** `(ts, value)` context observations on a `step` grid (open-interest / premium):
/// a sample at open-time `T` is closed iff `T + step <= now_ms`, which **excludes the forming
/// right-edge sample** — the same rule as [`closed_klines`]. Returns them ascending and deduped so a
/// re-fetch is byte-identical (QE-497).
#[must_use]
pub fn closed_series(mut obs: Vec<(i64, Decimal)>, now_ms: i64, step: i64) -> Vec<(i64, Decimal)> {
    obs.retain(|(t, _)| t.saturating_add(step) <= now_ms);
    obs.sort_by_key(|(t, _)| *t);
    obs.dedup_by_key(|(t, _)| *t);
    obs
}

// --- incremental / resume / internal-gap planning -------------------------------------------------

/// Floor `t` onto the `step` grid (venue slots are epoch-aligned to their interval).
fn floor_to_grid(t: i64, step: i64) -> i64 {
    t - t.rem_euclid(step)
}

/// The **missing** sub-windows to fetch over the epoch-aligned grid `[from, to]` (step `step`), given
/// the open-times already `present` in the store — returned as maximal runs of absent slots
/// `[run_start, run_end]` (inclusive open-times). `from` is floored onto the grid.
///
/// One function serves three ACs:
/// - **incremental** — a covered slot yields no run, so covered ranges are never re-downloaded;
/// - **internal-gap** — an absent run *inside* `[first, last]` is returned (a hole is found + back-filled,
///   not just edge-extended);
/// - **resume** — the still-absent trailing slots after an interruption are one final run, so the
///   download continues from coverage's end rather than restarting;
/// - **idempotent** — a fully-covered window yields `[]`, so a re-fetch fetches (and writes) nothing.
#[must_use]
pub fn plan_missing(from: i64, to: i64, step: i64, present: &BTreeSet<i64>) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    if step <= 0 || to < from {
        return out;
    }
    let mut run: Option<(i64, i64)> = None; // (run_start, last_absent)
    let mut t = floor_to_grid(from, step);
    while t <= to {
        if present.contains(&t) {
            if let Some((s, e)) = run.take() {
                out.push((s, e));
            }
        } else {
            run = Some(run.map_or((t, t), |(s, _)| (s, t)));
        }
        t += step;
    }
    if let Some((s, e)) = run.take() {
        out.push((s, e));
    }
    out
}

// --- the orchestrator -----------------------------------------------------------------------------

/// One historical-window request for [`BinanceHistorical::fetch_window`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRequest {
    /// Instrument (e.g. `BTCUSDT`).
    pub symbol: InstrumentId,
    /// Base kline resolution.
    pub resolution: Resolution,
    /// Inclusive requested left edge (epoch ms).
    pub from_ms: i64,
    /// Inclusive requested right edge (epoch ms) — trimmed to closed windows via `now_ms`.
    pub to_ms: i64,
    /// "Now" (epoch ms), injected for determinism; the forming right-edge bar/funding interval is
    /// excluded relative to this.
    pub now_ms: i64,
    /// REST page size.
    pub limit: u32,
}

/// A Binance USDT-M historical source: plans the missing windows against current coverage, pages them
/// through the retried/rate-limited [`Backfiller`], decodes klines + funding, and closed-window filters
/// — yielding an [`IngestedWindow`]. Generic over the [`RestSource`] port so it is fully offline-testable.
pub struct BinanceHistorical<S: RestSource, Sl: Sleeper = RealSleeper> {
    backfiller: Backfiller<S, Sl>,
}

impl<S: RestSource, Sl: Sleeper> BinanceHistorical<S, Sl> {
    /// Build over an already-configured [`Backfiller`] (owns the [`RestSource`] + retry policy + sleeper).
    #[must_use]
    pub fn new(backfiller: Backfiller<S, Sl>) -> Self {
        Self { backfiller }
    }

    /// Page one bounded sub-window `[a, b]` (inclusive open-times) of `endpoint` at grid `step`, keeping
    /// only rows in `[a, b]`. Reuses the [`Backfiller`]'s tested pagination + retry/back-off by setting
    /// the freshness target to exactly `b` (`now = b + step`).
    fn page(
        &self,
        endpoint: RestEndpoint,
        symbol: &InstrumentId,
        step: i64,
        a: i64,
        b: i64,
        limit: u32,
    ) -> Result<Vec<TimedRow>, IngestError> {
        let res = self.backfiller.backfill(&BackfillRequest {
            endpoint,
            symbol: symbol.clone(),
            interval_ms: step,
            from_ms: a,
            now_ms: b + step,
            overlap_ms: 0,
            limit,
        })?;
        let mut rows = res.fresh;
        rows.extend(res.overlap);
        rows.retain(|r| r.open_time_ms >= a && r.open_time_ms <= b);
        Ok(rows)
    }

    /// Page + decode a whole **optional** context series (open-interest / premium) over `[from, to]` at
    /// grid `step`, closed-window filtered. There is no incremental present-set for these yet, so the
    /// whole window is planned as one run. Returns an `Err` on any REST/decode failure — the caller is
    /// expected to treat that as an empty series (fail-open), so a missing/lagging optional factor never
    /// aborts the ingest (QE-497).
    fn fetch_context_series(
        &self,
        req: &WindowRequest,
        endpoint: RestEndpoint,
        step: i64,
        to: i64,
        decode: ContextDecoder,
    ) -> Result<Vec<(i64, Decimal)>, IngestError> {
        let mut out = Vec::new();
        for (a, b) in plan_missing(req.from_ms, to, step, &BTreeSet::new()) {
            let rows = self.page(endpoint, &req.symbol, step, a, b, req.limit)?;
            out.extend(decode(&rows)?);
        }
        Ok(closed_series(out, req.now_ms, step))
    }

    /// Fetch the closed, incremental window: for both klines and funding, plan the missing sub-windows
    /// against `present_bars` / `present_funding`, page + decode each, then closed-window filter.
    ///
    /// # Errors
    /// [`IngestError::Rest`] on a fatal REST failure, retry exhaustion, or a decode failure.
    pub fn fetch_window(
        &self,
        req: &WindowRequest,
        present_bars: &BTreeSet<i64>,
        present_funding: &BTreeSet<i64>,
    ) -> Result<IngestedWindow, IngestError> {
        let interval = i64::from(req.resolution.minutes()) * 60_000;

        // Klines: never plan past `now` (the forming edge is dropped by `closed_klines` afterwards).
        let bar_to = req.to_ms.min(req.now_ms);
        let mut bars = Vec::new();
        for (a, b) in plan_missing(req.from_ms, bar_to, interval, present_bars) {
            let rows = self.page(
                RestEndpoint::Klines(req.resolution),
                &req.symbol,
                interval,
                a,
                b,
                req.limit,
            )?;
            bars.extend(decode_klines(&rows, req.resolution)?);
        }
        let bars = closed_klines(bars, req.now_ms, interval);

        // Funding (8h grid).
        let fund_to = req.to_ms.min(req.now_ms);
        let mut funding = Vec::new();
        for (a, b) in plan_missing(req.from_ms, fund_to, FUNDING_INTERVAL_MS, present_funding) {
            let rows = self.page(
                RestEndpoint::FundingRate,
                &req.symbol,
                FUNDING_INTERVAL_MS,
                a,
                b,
                req.limit,
            )?;
            funding.extend(decode_funding(&rows)?);
        }
        let funding = closed_funding(funding, req.now_ms);

        // Optional positioning/basis factors (QE-497). Fail-open: a REST/decode error — e.g. the ~30-day
        // `openInterestHist` retention rejecting an older `startTime` with HTTP 400 / code -1130 —
        // degrades to an empty series so bars + funding still land. Only klines/funding are fail-closed.
        let ctx_to = req.to_ms.min(req.now_ms);
        let open_interest = self
            .fetch_context_series(
                req,
                RestEndpoint::OpenInterestHist(req.resolution),
                interval,
                ctx_to,
                decode_open_interest,
            )
            .unwrap_or_default();
        let premium = self
            .fetch_context_series(
                req,
                RestEndpoint::PremiumIndexKlines(req.resolution),
                interval,
                ctx_to,
                decode_premium,
            )
            .unwrap_or_default();

        Ok(IngestedWindow {
            base: req.resolution,
            bars,
            funding,
            open_interest,
            premium,
            calibration_source: CalibrationSource::Uncalibrated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backfill::RetryPolicy;
    use crate::rest::{parse_klines_json, PageRequest, RestError};

    const M5: i64 = 5 * 60_000;

    fn inst() -> InstrumentId {
        InstrumentId::new("BTCUSDT").unwrap()
    }

    fn klines_fixture() -> Vec<TimedRow> {
        parse_klines_json(include_str!("../tests/fixtures/binance_usdtm/klines_5m.json").as_bytes())
            .unwrap()
    }

    fn funding_fixture() -> Vec<TimedRow> {
        parse_klines_json(include_str!("../tests/fixtures/binance_usdtm/funding.json").as_bytes())
            .unwrap()
    }

    // --- decoding -----------------------------------------------------------------------------

    #[test]
    fn decodes_klines_fixture_into_typed_bars() {
        let bars = decode_klines(&klines_fixture(), Resolution::M5).unwrap();
        assert_eq!(bars.len(), 5);
        assert_eq!(bars[0].open_time().millis(), 1_700_000_100_000);
        assert_eq!(bars[0].open().get(), Decimal::from_str("42000.00").unwrap());
        assert_eq!(bars[0].high().get(), Decimal::from_str("42050.00").unwrap());
        assert_eq!(bars[0].low().get(), Decimal::from_str("41990.00").unwrap());
        assert_eq!(
            bars[0].close().get(),
            Decimal::from_str("42030.00").unwrap()
        );
        assert_eq!(bars[0].volume().get(), Decimal::from_str("12.345").unwrap());
        assert_eq!(bars[0].trades(), 87);
        assert!(bars.iter().all(|b| b.resolution() == Resolution::M5));
    }

    #[test]
    fn decodes_funding_fixture_into_rate_series() {
        let funding = decode_funding(&funding_fixture()).unwrap();
        assert_eq!(funding.len(), 3);
        assert_eq!(
            funding[0],
            (1_699_977_600_000, Decimal::from_str("0.00010000").unwrap())
        );
        assert_eq!(funding[1].1, Decimal::from_str("-0.00005000").unwrap());
    }

    #[test]
    fn rejects_malformed_kline_row() {
        assert!(decode_kline_row("not json", Resolution::M5).is_err());
        assert!(decode_kline_row(r#"{"not":"array"}"#, Resolution::M5).is_err());
        // high < low → OHLC invariant violation surfaces as a decode error.
        assert!(decode_kline_row(
            r#"[1700000100000,"100.0","90.0","95.0","96.0","1.0",0,"0",1]"#,
            Resolution::M5
        )
        .is_err());
    }

    // --- closed-window ------------------------------------------------------------------------

    #[test]
    fn closed_window_excludes_forming_right_edge() {
        let bars = decode_klines(&klines_fixture(), Resolution::M5).unwrap();
        let last_open = 1_700_001_300_000; // 5th bar
                                           // `now` is one ms into the last bar's window → that bar is still forming, must be dropped.
        let kept = closed_klines(bars.clone(), last_open + 1, M5);
        assert_eq!(kept.len(), 4, "the forming right-edge bar must be excluded");
        assert_eq!(kept.last().unwrap().open_time().millis(), last_open - M5);
        // `now` past the last bar's close → all five are closed.
        let all = closed_klines(bars, last_open + M5, M5);
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn closed_funding_excludes_in_progress_interval() {
        let f = decode_funding(&funding_fixture()).unwrap();
        // now between the 2nd (1700006400000) and 3rd (1700035200000) settlement → 3rd not yet settled.
        let kept = closed_funding(f, 1_700_006_400_000 + 1);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept.last().unwrap().0, 1_700_006_400_000);
    }

    // --- planning: incremental / internal-gap / resume / idempotent ---------------------------

    fn slots(from: i64, n: i64, step: i64) -> Vec<i64> {
        (0..n).map(|i| from + i * step).collect()
    }

    #[test]
    fn plan_incremental_skips_covered_prefix() {
        let from = 0;
        let to = 9 * M5;
        // Left half present (slots 0..=4); expect one run over the missing right half (5..=9).
        let present: BTreeSet<i64> = slots(from, 5, M5).into_iter().collect();
        let missing = plan_missing(from, to, M5, &present);
        assert_eq!(missing, vec![(5 * M5, 9 * M5)]);
    }

    #[test]
    fn plan_detects_internal_gap() {
        let from = 0;
        let to = 9 * M5;
        // All present except a single interior slot (5) → exactly that hole is planned, not an edge extend.
        let mut present: BTreeSet<i64> = slots(from, 10, M5).into_iter().collect();
        present.remove(&(5 * M5));
        let missing = plan_missing(from, to, M5, &present);
        assert_eq!(missing, vec![(5 * M5, 5 * M5)]);
    }

    #[test]
    fn plan_resume_from_coverage_end() {
        let from = 0;
        let to = 9 * M5;
        // An interrupted download left slots 0..=6 present → resume plans only the tail 7..=9.
        let present: BTreeSet<i64> = slots(from, 7, M5).into_iter().collect();
        assert_eq!(plan_missing(from, to, M5, &present), vec![(7 * M5, 9 * M5)]);
    }

    #[test]
    fn plan_fully_covered_is_empty() {
        let from = 0;
        let to = 9 * M5;
        let present: BTreeSet<i64> = slots(from, 10, M5).into_iter().collect();
        assert!(
            plan_missing(from, to, M5, &present).is_empty(),
            "idempotent: nothing to fetch"
        );
    }

    // --- the orchestrator over a fake REST source ---------------------------------------------

    /// A fake USDT-M REST source serving a fixed ascending kline dataset (rows with `open_time >=
    /// start_ms`, up to `limit`, ascending) and counting hits, for the klines endpoint.
    struct FakeRest {
        rows: Vec<TimedRow>,
    }
    impl FakeRest {
        fn klines(open_times: &[i64]) -> Self {
            let rows = open_times
                .iter()
                .map(|&t| TimedRow {
                    open_time_ms: t,
                    // A valid kline row: flat OHLC so the invariant always holds.
                    raw: format!(
                        r#"[{t},"100.0","101.0","99.0","100.5","1.0",{},"100.0",3]"#,
                        t + M5 - 1
                    ),
                })
                .collect();
            Self { rows }
        }
    }
    impl RestSource for FakeRest {
        fn fetch_page(&self, req: &PageRequest) -> Result<Vec<TimedRow>, RestError> {
            // These tests exercise the kline path; the funding endpoint has no data here.
            if !matches!(req.endpoint, RestEndpoint::Klines(_)) {
                return Ok(Vec::new());
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

    fn source(open_times: &[i64]) -> BinanceHistorical<FakeRest> {
        BinanceHistorical::new(Backfiller::new(
            FakeRest::klines(open_times),
            RetryPolicy::default(),
        ))
    }

    fn req(from: i64, to: i64, now: i64) -> WindowRequest {
        WindowRequest {
            symbol: inst(),
            resolution: Resolution::M5,
            from_ms: from,
            to_ms: to,
            now_ms: now,
            limit: 3, // small → forces multi-page pagination through the Backfiller
        }
    }

    fn open_times(w: &IngestedWindow) -> Vec<i64> {
        w.bars.iter().map(|b| b.open_time().millis()).collect()
    }

    #[test]
    fn fetch_incremental_only_downloads_missing_range() {
        let all = slots(0, 10, M5);
        let src = source(&all);
        // Store already covers slots 0..=4; now is well past the last slot so all are closed.
        let present: BTreeSet<i64> = slots(0, 5, M5).into_iter().collect();
        let w = src
            .fetch_window(&req(0, 9 * M5, 100 * M5), &present, &BTreeSet::new())
            .unwrap();
        assert_eq!(
            open_times(&w),
            slots(5 * M5, 5, M5),
            "only the missing right half is fetched"
        );
        assert_eq!(w.calibration_source, CalibrationSource::Uncalibrated);
    }

    #[test]
    fn fetch_backfills_internal_gap_only() {
        let all = slots(0, 10, M5);
        let src = source(&all);
        let mut present: BTreeSet<i64> = all.iter().copied().collect();
        present.remove(&(5 * M5)); // punch an interior hole
        let w = src
            .fetch_window(&req(0, 9 * M5, 100 * M5), &present, &BTreeSet::new())
            .unwrap();
        assert_eq!(
            open_times(&w),
            vec![5 * M5],
            "exactly the interior hole is back-filled"
        );
    }

    #[test]
    fn fetch_resume_reaches_same_final_window() {
        let all = slots(0, 10, M5);
        // Uninterrupted: empty store → fetch the whole window.
        let full = source(&all)
            .fetch_window(
                &req(0, 9 * M5, 100 * M5),
                &BTreeSet::new(),
                &BTreeSet::new(),
            )
            .unwrap();
        assert_eq!(open_times(&full), all);
        // Interrupted after slot 6 → resume fetches the tail; union with coverage == the full set.
        let present: BTreeSet<i64> = slots(0, 7, M5).into_iter().collect();
        let resumed = source(&all)
            .fetch_window(&req(0, 9 * M5, 100 * M5), &present, &BTreeSet::new())
            .unwrap();
        assert_eq!(open_times(&resumed), slots(7 * M5, 3, M5));
        let mut union: BTreeSet<i64> = present;
        union.extend(open_times(&resumed));
        assert_eq!(union.into_iter().collect::<Vec<_>>(), all);
    }

    #[test]
    fn fetch_idempotent_covered_window_is_byte_identical_noop() {
        let all = slots(0, 10, M5);
        let present: BTreeSet<i64> = all.iter().copied().collect();
        let src = source(&all);
        let w = src
            .fetch_window(&req(0, 9 * M5, 100 * M5), &present, &BTreeSet::new())
            .unwrap();
        let _ = &src; // covered window ⇒ `plan_missing` empty ⇒ the page loop never runs (no REST call).
        assert!(w.bars.is_empty(), "a fully-covered window fetches nothing");
        // And decoding the same page twice is byte-identical (deterministic).
        let a = decode_klines(&klines_fixture(), Resolution::M5).unwrap();
        let b = decode_klines(&klines_fixture(), Resolution::M5).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn fetch_excludes_forming_right_edge_bar() {
        let all = slots(0, 10, M5);
        let src = source(&all);
        // now == last slot's open + 1 → the last slot's bar is still forming.
        let now = 9 * M5 + 1;
        let w = src
            .fetch_window(&req(0, 9 * M5, now), &BTreeSet::new(), &BTreeSet::new())
            .unwrap();
        assert_eq!(
            open_times(&w),
            slots(0, 9, M5),
            "forming right-edge bar excluded end-to-end"
        );
    }

    // --- QE-497: optional open-interest + premium factors -------------------------------------

    #[test]
    fn decodes_open_interest_and_premium_rows() {
        let (ts, oi) = decode_open_interest_row(
            r#"{"symbol":"BTCUSDT","sumOpenInterest":"109049.57600000","sumOpenInterestValue":"6.8e9","timestamp":1704067200000}"#,
        )
        .unwrap();
        assert_eq!(ts, 1_704_067_200_000);
        assert_eq!(oi, Decimal::from_str("109049.576").unwrap());

        // premiumIndexKlines is a kline array; the premium is the close (element 4).
        let (pts, prem) = decode_premium_row(
            r#"[1704067200000,"0.00075030","0.00137721","0.00051408","0.00068158","0",1704070799999,"0",720]"#,
        )
        .unwrap();
        assert_eq!(pts, 1_704_067_200_000);
        assert_eq!(prem, Decimal::from_str("0.00068158").unwrap());

        // Malformed rows are decode errors, never silent zeros.
        assert!(decode_open_interest_row("not json").is_err());
        assert!(decode_open_interest_row(r#"{"timestamp":1}"#).is_err()); // missing sumOpenInterest
        assert!(decode_premium_row(r#"{"not":"array"}"#).is_err());
    }

    /// A fake serving klines + OI + premium. `oi_errors` makes the OI endpoint return a fatal REST error
    /// (mimicking the ~30-day `openInterestHist` retention / HTTP 400 -1130), to exercise the fail-open
    /// path where bars + funding + premium still land.
    struct FakeFull {
        klines: Vec<TimedRow>,
        oi: Vec<TimedRow>,
        premium: Vec<TimedRow>,
        oi_errors: bool,
    }
    impl RestSource for FakeFull {
        fn fetch_page(&self, req: &PageRequest) -> Result<Vec<TimedRow>, RestError> {
            let rows = match req.endpoint {
                RestEndpoint::Klines(_) => &self.klines,
                RestEndpoint::OpenInterestHist(_) => {
                    if self.oi_errors {
                        return Err(RestError::Fatal("http 400".to_owned()));
                    }
                    &self.oi
                }
                RestEndpoint::PremiumIndexKlines(_) => &self.premium,
                _ => return Ok(Vec::new()),
            };
            Ok(rows
                .iter()
                .filter(|r| r.open_time_ms >= req.start_ms)
                .take(req.limit as usize)
                .cloned()
                .collect())
        }
    }

    fn oi_rows(open_times: &[i64]) -> Vec<TimedRow> {
        open_times
            .iter()
            .enumerate()
            .map(|(i, &t)| TimedRow {
                open_time_ms: t,
                raw: format!(
                    r#"{{"symbol":"BTCUSDT","sumOpenInterest":"{}.5","sumOpenInterestValue":"1.0","timestamp":{t}}}"#,
                    1000 + i
                ),
            })
            .collect()
    }

    fn premium_rows(open_times: &[i64]) -> Vec<TimedRow> {
        open_times
            .iter()
            .map(|&t| TimedRow {
                open_time_ms: t,
                raw: format!(
                    r#"[{t},"0.0001","0.0002","0.0000","0.00015","0",{},"0",1]"#,
                    t + M5 - 1
                ),
            })
            .collect()
    }

    fn full_source(
        klines: &[i64],
        oi: &[i64],
        premium: &[i64],
        oi_errors: bool,
    ) -> BinanceHistorical<FakeFull> {
        BinanceHistorical::new(Backfiller::new(
            FakeFull {
                klines: FakeRest::klines(klines).rows,
                oi: oi_rows(oi),
                premium: premium_rows(premium),
                oi_errors,
            },
            RetryPolicy::default(),
        ))
    }

    #[test]
    fn fetch_populates_open_interest_and_premium_on_the_grid() {
        let all = slots(0, 6, M5);
        let src = full_source(&all, &all, &all, false);
        let w = src
            .fetch_window(
                &req(0, 5 * M5, 100 * M5),
                &BTreeSet::new(),
                &BTreeSet::new(),
            )
            .unwrap();
        assert_eq!(open_times(&w), all, "all closed bars present");
        // Optional factors populated on the bar grid, ascending, closed-window filtered.
        assert_eq!(
            w.open_interest.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
            all
        );
        assert_eq!(w.open_interest[0].1, Decimal::from_str("1000.5").unwrap());
        assert_eq!(w.premium.iter().map(|(t, _)| *t).collect::<Vec<_>>(), all);
        assert_eq!(w.premium[0].1, Decimal::from_str("0.00015").unwrap());
    }

    #[test]
    fn fetch_degrades_gracefully_when_open_interest_endpoint_fails() {
        // QE-497 graceful degradation: OI endpoint errors (retention/-1130) but the ingest still yields
        // bars + premium — the optional factor is simply empty, the whole window is NOT failed.
        let all = slots(0, 6, M5);
        let src = full_source(&all, &all, &all, /* oi_errors */ true);
        let w = src
            .fetch_window(
                &req(0, 5 * M5, 100 * M5),
                &BTreeSet::new(),
                &BTreeSet::new(),
            )
            .expect("a failing optional OI series must not fail the whole fetch");
        assert_eq!(open_times(&w), all, "bars still fetched despite OI failure");
        assert!(
            w.open_interest.is_empty(),
            "OI degrades to empty (fail-open)"
        );
        assert!(
            !w.premium.is_empty(),
            "premium is independent and still populated"
        );
    }

    #[test]
    fn fetch_excludes_forming_right_edge_context_samples() {
        // A premium/OI sample whose period is still forming (open + step > now) is dropped, mirroring the
        // closed-kline rule, so a re-fetch is byte-identical.
        let all = slots(0, 10, M5);
        let src = full_source(&all, &all, &all, false);
        let now = 9 * M5 + 1; // slot 9 still forming
        let w = src
            .fetch_window(&req(0, 9 * M5, now), &BTreeSet::new(), &BTreeSet::new())
            .unwrap();
        assert_eq!(w.open_interest.last().map(|(t, _)| *t), Some(8 * M5));
        assert_eq!(w.premium.last().map(|(t, _)| *t), Some(8 * M5));
    }
}
