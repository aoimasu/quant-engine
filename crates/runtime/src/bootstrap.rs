//! Bootstrap pipeline — cold-start state reconstruction (QE-209).
//!
//! On startup the planner has no in-memory state. The bootstrap **replays the lookback window through the
//! same evaluator** the live loop runs, so the per-strategy state ends up exactly where a
//! continuously-running planner would hold it. It composes the existing, individually-proven pieces:
//! paginated+retried+cached REST fetch ([`qe_venue::VenueRestClient`], QE-201) → stitch/dedup +
//! multi-resolution replay ([`LiveKlineSource`], QE-205) → as-of factor merge + the evaluator in replay
//! mode ([`EvaluatorSession`], QE-206/207). The replay is a **pure function of its inputs** (no clock, no
//! RNG), so a cold start is deterministic (the AC).
//!
//! markPrice is fetched and replayed at its 1-min cadence but is **not** a catalogue feature input (the
//! feature context is funding/open-interest/premium); it is surfaced as `last_mark_price` for the
//! risk/cutover layer (QE-210/211).

use rust_decimal::Decimal;
use thiserror::Error;

use qe_domain::{Bar, Resolution};
use qe_signal::CatalogueConfig;
use qe_venue::{Clock, RestError, RestResponse, RestTransport, VenueRequest, VenueRestClient};
use qe_vintage::Vintage;

use crate::evaluator::{EvalOutput, EvaluatorSession};
use crate::live_kline::LiveKlineSource;

/// A cold-start failure.
#[derive(Debug, Error)]
pub enum BootstrapError {
    /// A multi-resolution reconstruction error (invalid tier, or a bar with the wrong resolution).
    #[error("reconstruction: {0}")]
    Recon(#[from] qe_signal::reconstruct::ReconError),
    /// A REST fetch error surfaced by the paginated fetch helpers.
    #[error("rest: {0}")]
    Rest(#[from] RestError),
    /// A page failed to decode into the expected typed rows.
    #[error("decode: {0}")]
    Decode(String),
}

/// The typed historical inputs for one cold-start lookback window — what the REST fetch yields, decoded.
///
/// `bars` may arrive in any order and may overlap at page boundaries; [`BootstrapPipeline::replay`]
/// stitches/dedups them. Each context series is `(ts_ms, value)` observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricalWindow {
    /// The base kline resolution the bars are at.
    pub base: Resolution,
    /// Base-resolution klines (any order; possibly page-overlapping).
    pub bars: Vec<Bar>,
    /// Funding-rate observations.
    pub funding: Vec<(i64, Decimal)>,
    /// Open-interest observations.
    pub open_interest: Vec<(i64, Decimal)>,
    /// Premium (perp − underlier) observations.
    pub premium: Vec<(i64, Decimal)>,
    /// Mark-price observations (1-min cadence).
    pub mark_price: Vec<(i64, Decimal)>,
}

/// The REST seam: fetch one cold-start lookback window. The real implementation paginates each series
/// through [`VenueRestClient`] (retried + cached) and decodes the pages; tests use an in-memory window.
pub trait HistoricalSource {
    /// Fetch the lookback window.
    ///
    /// # Errors
    /// [`BootstrapError`] on a REST or decode failure.
    fn fetch(&mut self) -> Result<HistoricalWindow, BootstrapError>;
}

/// The outcome of a cold-start replay: the **warmed** evaluator session plus the replay trace.
pub struct Reconstructed {
    /// The evaluator session with per-chromosome state reconstructed — ready for `go_live()` (QE-211).
    pub session: EvaluatorSession,
    /// The per-bar decisions produced during replay (the full trace).
    pub decisions: Vec<EvalOutput>,
    /// The coarser bars reconstructed from the base series during multi-resolution replay (QE-205),
    /// surfaced for persistence / reconstructed-state (QE-210).
    pub coarse_bars: Vec<Bar>,
    /// Number of (deduped) base bars replayed.
    pub bars_replayed: usize,
    /// The last mark price seen (1-min cadence), for the risk/cutover layer; `None` if none observed.
    pub last_mark_price: Option<Decimal>,
}

/// Orchestrates a deterministic cold start over a sealed vintage.
pub struct BootstrapPipeline {
    cfg: CatalogueConfig,
    tiers: Vec<Resolution>,
}

/// One merged replay event, tagged for stable timeline ordering.
enum Event {
    Bar(Bar),
    Funding(Decimal),
    OpenInterest(Decimal),
    Premium(Decimal),
    Mark(Decimal),
}

impl BootstrapPipeline {
    /// A pipeline over the catalogue `cfg` the vintage's genomes were evolved against, reconstructing the
    /// coarser `tiers` during replay.
    #[must_use]
    pub fn new(cfg: CatalogueConfig, tiers: Vec<Resolution>) -> Self {
        Self { cfg, tiers }
    }

    /// Replay `window` through the evaluator to reconstruct per-strategy state. Pure function of its
    /// inputs — no clock, no RNG — so the result is deterministic (the AC).
    ///
    /// # Errors
    /// [`BootstrapError::Recon`] if a tier is invalid or a base bar has the wrong resolution.
    pub fn replay(
        &self,
        window: &HistoricalWindow,
        vintage: Vintage,
    ) -> Result<Reconstructed, BootstrapError> {
        // 1. Stitch/dedup the base bars: sort by open-time, keep only strictly-increasing open-times
        //    (the QE-205 marker rule), so overlapping REST pages cannot double-count.
        let mut bars = window.bars.clone();
        bars.sort_by_key(|b| b.open_time().millis());
        let mut deduped: Vec<Bar> = Vec::with_capacity(bars.len());
        for bar in bars {
            if deduped
                .last()
                .is_none_or(|last| bar.open_time().millis() > last.open_time().millis())
            {
                deduped.push(bar);
            }
        }

        // 2. Multi-resolution replay: prime a LiveKlineSource with the deduped base bars and flush it, so
        //    the coarse tiers match batch reconstruction by construction (QE-205/106).
        let mut klines = LiveKlineSource::new(window.base, &self.tiers)?;
        let mut coarse_bars = klines.prime(&deduped)?;
        coarse_bars.extend(klines.finish()?);

        // 3. Factor-merge timeline: context observations + base bars, stably ordered by (ts, ordinal) with
        //    context BEFORE a bar at equal ts — exactly QE-206's `value.ts <= bar.open_time` as-of rule.
        let mut timeline: Vec<(i64, u8, Event)> = Vec::new();
        for &(ts, v) in &window.funding {
            timeline.push((ts, 0, Event::Funding(v)));
        }
        for &(ts, v) in &window.open_interest {
            timeline.push((ts, 0, Event::OpenInterest(v)));
        }
        for &(ts, v) in &window.premium {
            timeline.push((ts, 0, Event::Premium(v)));
        }
        for &(ts, v) in &window.mark_price {
            timeline.push((ts, 0, Event::Mark(v)));
        }
        for bar in &deduped {
            timeline.push((bar.open_time().millis(), 1, Event::Bar(bar.clone())));
        }
        // Stable sort: equal (ts, ordinal) keeps insertion order (funding→oi→premium→mark), deterministic.
        timeline.sort_by_key(|(ts, ord, _)| (*ts, *ord));

        // 4. Evaluate in replay mode.
        let mut session = EvaluatorSession::new(vintage, &self.cfg);
        let mut decisions = Vec::new();
        let mut last_mark_price = None;
        for (_ts, _ord, event) in timeline {
            match event {
                Event::Bar(bar) => decisions.push(session.on_bar(&bar)),
                Event::Funding(v) => session.observe_funding(v),
                Event::OpenInterest(v) => session.observe_open_interest(v),
                Event::Premium(v) => session.observe_premium(v),
                Event::Mark(v) => last_mark_price = Some(v),
            }
        }

        Ok(Reconstructed {
            session,
            decisions,
            coarse_bars,
            bars_replayed: deduped.len(),
            last_mark_price,
        })
    }

    /// Fetch the lookback window from `source`, then [`replay`](Self::replay) it.
    ///
    /// # Errors
    /// [`BootstrapError`] from the fetch or the replay.
    pub fn cold_start<S: HistoricalSource>(
        &self,
        source: &mut S,
        vintage: Vintage,
    ) -> Result<Reconstructed, BootstrapError> {
        let window = source.fetch()?;
        self.replay(&window, vintage)
    }
}

/// Drive a paginated kline fetch over [`VenueRestClient`] (rate-limited, retried, cached by QE-201) and
/// decode every page into [`Bar`]s, concatenated in page order.
///
/// `next(&prev_page, &prev_req)` produces the following request (or `None` to stop); `decode` parses one
/// page's bytes into bars (the venue JSON schema is the caller's concern, kept out of the runtime crate).
///
/// # Errors
/// [`BootstrapError::Rest`] on a fetch failure, [`BootstrapError::Decode`] if a page fails to decode.
pub fn paginate_klines<T, C, F, D>(
    client: &mut VenueRestClient<T, C>,
    first: VenueRequest,
    next: F,
    decode: D,
) -> Result<Vec<Bar>, BootstrapError>
where
    T: RestTransport,
    C: Clock,
    F: FnMut(&RestResponse, &VenueRequest) -> Option<VenueRequest>,
    D: Fn(&[u8]) -> Result<Vec<Bar>, BootstrapError>,
{
    let pages = client.paginate(first, next)?;
    let mut bars = Vec::new();
    for page in &pages {
        bars.extend(decode(&page.bytes)?);
    }
    Ok(bars)
}

/// Drive a paginated scalar-series fetch over [`VenueRestClient`] (rate-limited, retried, cached) and
/// decode every page into `(ts_ms, value)` observations, concatenated in page order.
///
/// # Errors
/// [`BootstrapError::Rest`] on a fetch failure, [`BootstrapError::Decode`] if a page fails to decode.
pub fn paginate_series<T, C, F, D>(
    client: &mut VenueRestClient<T, C>,
    first: VenueRequest,
    next: F,
    decode: D,
) -> Result<Vec<(i64, Decimal)>, BootstrapError>
where
    T: RestTransport,
    C: Clock,
    F: FnMut(&RestResponse, &VenueRequest) -> Option<VenueRequest>,
    D: Fn(&[u8]) -> Result<Vec<(i64, Decimal)>, BootstrapError>,
{
    let pages = client.paginate(first, next)?;
    let mut rows = Vec::new();
    for page in &pages {
        rows.extend(decode(&page.bytes)?);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    use qe_determinism::Lineage;
    use qe_domain::{InstrumentId, Price, Qty, TimeInterval, Timestamp};
    use qe_risk::{CalibrationProfile, Fraction};
    use qe_signal::{
        Clause, ExitParams, FeatureSchema, Genome, RiskParams, RuleSet, CLAUSES_PER_SET,
        REP_VERSION,
    };
    use qe_vintage::{VintageContent, VINTAGE_FORMAT_VERSION};

    const MIN: i64 = 60_000;

    fn cfg() -> CatalogueConfig {
        CatalogueConfig::default()
    }

    fn p(n: i64) -> Price {
        Price::new(Decimal::from(n)).unwrap()
    }
    fn q(n: i64) -> Qty {
        Qty::new(Decimal::from(n)).unwrap()
    }
    fn dec(n: i64) -> Decimal {
        Decimal::from(n)
    }

    /// A base 5m bar at minute `i` with a deterministic, valid OHLC that varies enough to warm indicators.
    fn bar(i: i64) -> Bar {
        let base = 100 + (i % 13);
        Bar::new(
            Timestamp::from_millis(i * 5 * MIN),
            Resolution::M5,
            p(base),
            p(base + 3),
            p(base - 2),
            p(base + 1),
            q(10 + (i % 7)),
            5,
        )
        .unwrap()
    }

    fn bars(n: i64) -> Vec<Bar> {
        (0..n).map(bar).collect()
    }

    fn off_clause() -> Clause {
        Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        }
    }

    /// A genome that goes long whenever feature 0 is warm and exits after `max_holding` bars — once warm
    /// it cycles Enter → Hold… → Exit → Enter, giving non-trivial deterministic decisions.
    fn cycling_genome(max_holding: u16) -> Genome {
        let num_states = FeatureSchema::from_catalogue(&cfg()).num_states();
        let mut clauses = [off_clause(); CLAUSES_PER_SET];
        clauses[0] = Clause {
            enabled: true,
            feature: 0,
            lo: 0,
            hi: num_states - 1,
        };
        Genome {
            version: REP_VERSION,
            long_entry: RuleSet {
                clauses,
                min_satisfied: 1,
            },
            short_entry: RuleSet {
                clauses: [off_clause(); CLAUSES_PER_SET],
                min_satisfied: 1,
            },
            exit: ExitParams {
                max_holding_bars: max_holding,
                exit_on_opposite: false,
            },
            risk: RiskParams { size_bps: 5_000 },
        }
    }

    fn vintage_of(genomes: Vec<Genome>) -> Vintage {
        let weights = vec![1.0 / genomes.len() as f64; genomes.len()];
        let content = VintageContent {
            format_version: VINTAGE_FORMAT_VERSION,
            vintage_id: "qe-209-test".to_owned(),
            chromosomes: genomes,
            weights,
            calibration: CalibrationProfile::new(Fraction::new(Decimal::new(5, 1)).unwrap()),
            worst_case_loss: None,
            catalogue: qe_signal::CatalogueIdentity::current(),
            lineage: Lineage::new("cfg", "snap", "commit", vec![]),
        };
        Vintage::seal(content).unwrap()
    }

    fn window(bars: Vec<Bar>) -> HistoricalWindow {
        HistoricalWindow {
            base: Resolution::M5,
            bars,
            funding: vec![(2 * MIN, dec(1)), (90 * MIN, dec(2))],
            open_interest: vec![(MIN, dec(1000)), (120 * MIN, dec(1100))],
            premium: vec![(3 * MIN, dec(1))],
            mark_price: vec![(MIN, dec(100)), (60 * MIN, dec(105)), (120 * MIN, dec(110))],
        }
    }

    fn pipeline() -> BootstrapPipeline {
        BootstrapPipeline::new(cfg(), vec![Resolution::M30, Resolution::H4])
    }

    fn decisions_eq(a: &[EvalOutput], b: &[EvalOutput]) -> bool {
        a == b
    }

    fn has_enter_and_exit(trace: &[EvalOutput]) -> bool {
        use crate::evaluator::ChromosomeDecision;
        use qe_signal::Decision;
        let any = |pred: fn(&ChromosomeDecision) -> bool| {
            trace.iter().any(|o| o.decisions.iter().any(pred))
        };
        any(|d| matches!(d.decision, Decision::Enter(_))) && any(|d| d.decision == Decision::Exit)
    }

    #[test]
    fn cold_start_is_deterministic() {
        let pipe = pipeline();
        let w = window(bars(60));
        let a = pipe
            .replay(&w, vintage_of(vec![cycling_genome(3)]))
            .unwrap();
        let b = pipe
            .replay(&w, vintage_of(vec![cycling_genome(3)]))
            .unwrap();
        assert!(
            decisions_eq(&a.decisions, &b.decisions),
            "same window must reconstruct identical decisions"
        );
        assert_eq!(a.bars_replayed, b.bars_replayed);
        assert_eq!(a.last_mark_price, b.last_mark_price);
        assert_eq!(a.session.vintage_id(), b.session.vintage_id());
        // Non-vacuous: the fixture genuinely trades.
        assert!(
            has_enter_and_exit(&a.decisions),
            "fixture must produce at least one Enter and one Exit"
        );
    }

    /// An independent as-of pick: the latest observation with `ts <= bar_ts` (QE-206's `<=` rule), found by
    /// a linear max-by-ts scan — a per-bar *pull*, structurally unlike `replay`'s event-stream *push* merge.
    fn as_of(obs: &[(i64, Decimal)], bar_ts: i64) -> Option<Decimal> {
        obs.iter()
            .filter(|(t, _)| *t <= bar_ts)
            .max_by_key(|(t, _)| *t)
            .map(|(_, v)| *v)
    }

    #[test]
    fn replay_matches_an_independent_as_of_oracle() {
        // The continuous-run test re-uses replay's own merge, so it proves equivalence but not the as-of
        // *correctness* independently. This oracle builds the expected decisions by a different construction
        // — for each bar, PULL the latest context with ts <= bar.open_time and observe it just before the
        // bar — so a shared ordinal/sort bug in replay's push-merge cannot hide here.
        let pipe = pipeline();
        let w = window(bars(60));
        let got = pipe
            .replay(&w, vintage_of(vec![cycling_genome(3)]))
            .unwrap();

        let mut sorted = w.bars.clone();
        sorted.sort_by_key(|b| b.open_time().millis());
        sorted.dedup_by_key(|b| b.open_time().millis());

        let mut oracle = EvaluatorSession::new(vintage_of(vec![cycling_genome(3)]), &cfg());
        let mut expected = Vec::new();
        for b in &sorted {
            let t = b.open_time().millis();
            if let Some(v) = as_of(&w.funding, t) {
                oracle.observe_funding(v);
            }
            if let Some(v) = as_of(&w.open_interest, t) {
                oracle.observe_open_interest(v);
            }
            if let Some(v) = as_of(&w.premium, t) {
                oracle.observe_premium(v);
            }
            expected.push(oracle.on_bar(b));
        }
        assert_eq!(
            got.decisions, expected,
            "replay must match an independent per-bar as-of oracle"
        );
    }

    #[test]
    fn reconstructed_state_equals_a_continuous_run() {
        // Bootstrap over [0..n), go live, feed bar n; it must decide identically to one continuous session
        // fed [0..n] — i.e. cold-start state == continuously-running state (the ticket's "Why").
        let pipe = pipeline();
        let n = 50i64;
        let all = bars(n + 1);
        let w = window(all[..n as usize].to_vec());

        let mut boot = pipe
            .replay(&w, vintage_of(vec![cycling_genome(3)]))
            .unwrap();
        boot.session.go_live();
        let after_boot = boot.session.on_bar(&all[n as usize]);

        // One continuous session fed the same bars + context in the same order, then bar n.
        let mut cont = EvaluatorSession::new(vintage_of(vec![cycling_genome(3)]), &cfg());
        let mut timeline: Vec<(i64, u8, Event)> = Vec::new();
        for &(ts, v) in &w.funding {
            timeline.push((ts, 0, Event::Funding(v)));
        }
        for &(ts, v) in &w.open_interest {
            timeline.push((ts, 0, Event::OpenInterest(v)));
        }
        for &(ts, v) in &w.premium {
            timeline.push((ts, 0, Event::Premium(v)));
        }
        for &(ts, v) in &w.mark_price {
            timeline.push((ts, 0, Event::Mark(v)));
        }
        for b in &w.bars {
            timeline.push((b.open_time().millis(), 1, Event::Bar(b.clone())));
        }
        timeline.sort_by_key(|(ts, ord, _)| (*ts, *ord));
        for (_t, _o, e) in timeline {
            match e {
                Event::Bar(b) => {
                    cont.on_bar(&b);
                }
                Event::Funding(v) => cont.observe_funding(v),
                Event::OpenInterest(v) => cont.observe_open_interest(v),
                Event::Premium(v) => cont.observe_premium(v),
                Event::Mark(_) => {}
            }
        }
        let after_cont = cont.on_bar(&all[n as usize]);

        assert_eq!(
            after_boot.decisions, after_cont.decisions,
            "bootstrapped state must equal the continuously-running state"
        );
    }

    #[test]
    fn dedup_stitches_overlapping_pages() {
        let pipe = pipeline();
        let clean = bars(40);
        // Simulate overlapping REST pages: duplicate the boundary bars and shuffle.
        let mut overlapped = clean.clone();
        overlapped.extend(clean[18..22].iter().cloned());
        overlapped.reverse();

        let r_clean = pipe
            .replay(&window(clean), vintage_of(vec![cycling_genome(3)]))
            .unwrap();
        let r_over = pipe
            .replay(&window(overlapped), vintage_of(vec![cycling_genome(3)]))
            .unwrap();

        assert_eq!(r_over.bars_replayed, 40, "duplicates must be stitched out");
        assert!(
            decisions_eq(&r_clean.decisions, &r_over.decisions),
            "overlapping pages must reconstruct identically to the deduped window"
        );
    }

    #[test]
    fn coarse_bars_match_batch_reconstruction() {
        use qe_signal::reconstruct::reconstruct_batch;
        let pipe = pipeline();
        let base = bars(120);
        let r = pipe
            .replay(&window(base.clone()), vintage_of(vec![cycling_genome(3)]))
            .unwrap();
        for tier in [Resolution::M30, Resolution::H4] {
            let expected = reconstruct_batch(&base, Resolution::M5, tier).unwrap();
            let got: Vec<Bar> = r
                .coarse_bars
                .iter()
                .filter(|b| b.resolution() == tier)
                .cloned()
                .collect();
            assert_eq!(
                got, expected,
                "multi-resolution replay must match batch for {tier:?}"
            );
        }
    }

    #[test]
    fn thin_history_holds_without_spurious_entries() {
        // Too few bars to warm any indicator → all Hold, and replay must not panic.
        let pipe = pipeline();
        let r = pipe
            .replay(&window(bars(3)), vintage_of(vec![cycling_genome(3)]))
            .unwrap();
        use qe_signal::Decision;
        assert!(
            r.decisions
                .iter()
                .all(|o| o.decisions.iter().all(|d| d.decision == Decision::Hold)),
            "cold start with thin history must not enter"
        );
        assert_eq!(r.bars_replayed, 3);
    }

    // --- paginated REST fetch (QE-201 path) -------------------------------------------------------

    /// A logical clock for the paginate tests (the venue ManualClock is crate-private).
    struct TestClock(Cell<i64>);
    impl Clock for TestClock {
        fn now_ms(&self) -> i64 {
            self.0.get()
        }
        fn sleep_until(&self, deadline_ms: i64) {
            if deadline_ms > self.0.get() {
                self.0.set(deadline_ms);
            }
        }
    }

    /// A scripted transport: pops the next outcome per send; an empty (drained) script serves empty pages
    /// (the pagination terminator), so no `Clone` on the non-`Clone` `RestError` is needed.
    struct ScriptTransport {
        script: RefCell<Vec<Result<Vec<u8>, RestError>>>,
        hits: Cell<usize>,
    }
    impl ScriptTransport {
        fn new(script: Vec<Result<Vec<u8>, RestError>>) -> Self {
            Self {
                script: RefCell::new(script),
                hits: Cell::new(0),
            }
        }
    }
    impl RestTransport for ScriptTransport {
        fn send(&self, _req: &VenueRequest) -> Result<RestResponse, RestError> {
            self.hits.set(self.hits.get() + 1);
            let mut s = self.script.borrow_mut();
            if s.is_empty() {
                return Ok(RestResponse { bytes: Vec::new() });
            }
            s.remove(0).map(|bytes| RestResponse { bytes })
        }
    }

    fn req(start: i64) -> VenueRequest {
        VenueRequest {
            endpoint: "/fapi/v1/klines".to_owned(),
            instrument: InstrumentId::new("BTCUSDT").unwrap(),
            params: vec![("interval".to_owned(), "5m".to_owned())],
            window: TimeInterval::new(
                Timestamp::from_millis(start),
                Timestamp::from_millis(start + 5 * MIN),
            )
            .unwrap(),
            weight: 1,
        }
    }

    /// Decode a page of comma-separated open-times (ms) into base bars; empty bytes → no bars.
    fn decode_times(bytes: &[u8]) -> Result<Vec<Bar>, BootstrapError> {
        let s = std::str::from_utf8(bytes).map_err(|e| BootstrapError::Decode(e.to_string()))?;
        if s.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for tok in s.split(',') {
            let t: i64 = tok
                .parse()
                .map_err(|_| BootstrapError::Decode(format!("bad ts {tok}")))?;
            out.push(bar(t / (5 * MIN)));
        }
        Ok(out)
    }

    #[test]
    fn pagination_is_invariant_to_page_splits_and_retries() {
        // The same 6 bars served as ONE page vs THREE pages (with a transient error to exercise the
        // retry/back-off + cache path) must decode to the identical bar sequence.
        let times: Vec<i64> = (0..6).map(|i| i * 5 * MIN).collect();
        let one = times
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");

        // Single page then an empty terminator.
        let mut c1 = VenueRestClient::new(
            ScriptTransport::new(vec![Ok(one.into_bytes()), Ok(Vec::new())]),
            TestClock(Cell::new(10_000_000_000)),
        );
        let single = paginate_klines(
            &mut c1,
            req(0),
            |resp, prev| {
                if resp.bytes.is_empty() {
                    None
                } else {
                    Some(req(prev.window.end().millis()))
                }
            },
            decode_times,
        )
        .unwrap();

        // Three pages of two times each, with a transient failure before page 2, then an empty terminator.
        let page = |a: i64, b: i64| format!("{a},{b}").into_bytes();
        let mut c3 = VenueRestClient::new(
            ScriptTransport::new(vec![
                Ok(page(0, 5 * MIN)),
                Err(RestError::Transient("flaky".to_owned())),
                Ok(page(10 * MIN, 15 * MIN)),
                Ok(page(20 * MIN, 25 * MIN)),
                Ok(Vec::new()),
            ]),
            TestClock(Cell::new(10_000_000_000)),
        );
        let split = paginate_klines(
            &mut c3,
            req(0),
            |resp, prev| {
                if resp.bytes.is_empty() {
                    None
                } else {
                    Some(req(prev.window.end().millis()))
                }
            },
            decode_times,
        )
        .unwrap();

        assert_eq!(
            single, split,
            "page splits + retries must not change the decoded bars"
        );
        assert_eq!(single.len(), 6);

        // And the two feed identical reconstructions.
        let pipe = pipeline();
        let ra = pipe
            .replay(&window(single), vintage_of(vec![cycling_genome(2)]))
            .unwrap();
        let rb = pipe
            .replay(&window(split), vintage_of(vec![cycling_genome(2)]))
            .unwrap();
        assert!(decisions_eq(&ra.decisions, &rb.decisions));
    }

    #[test]
    fn cold_start_fetches_then_replays() {
        struct InMemory(HistoricalWindow);
        impl HistoricalSource for InMemory {
            fn fetch(&mut self) -> Result<HistoricalWindow, BootstrapError> {
                Ok(self.0.clone())
            }
        }
        let pipe = pipeline();
        let mut src = InMemory(window(bars(60)));
        let r = pipe
            .cold_start(&mut src, vintage_of(vec![cycling_genome(3)]))
            .unwrap();
        assert_eq!(r.bars_replayed, 60);
        assert!(has_enter_and_exit(&r.decisions));
    }
}
