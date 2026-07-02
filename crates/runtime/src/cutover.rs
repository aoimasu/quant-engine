//! Bootstrap→live in-process cutover (QE-211).
//!
//! The handoff from replay to live is **in-process**: the warmed [`EvaluatorSession`] from bootstrap
//! (QE-209) is switched to live continuation **in place** — no new object, no state copy. This coordinator
//! drives already-decoded live base [`Bar`]s into that session while enforcing bar continuity at the seam:
//! it **drops** overlap bars the replay already covered (no duplicate), **detects a gap** if the stream
//! skips a bar (no silent skip), and flips the session to live via [`EvaluatorSession::go_live`] on the
//! first genuinely-new bar. Because `go_live` only flips a label (state-preserving, one-way), the resulting
//! decision stream is identical to a session that ran continuously and flipped at the same bar (the AC).
//!
//! The concrete wss bar decode/drive is runtime plumbing (this operates on decoded `Bar`s, per QE-205).

use thiserror::Error;

use qe_domain::{Bar, Resolution};

use crate::bootstrap::Reconstructed;
use crate::evaluator::{EvalOutput, EvaluatorSession, SessionMode};

/// A cutover failure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CutoverError {
    /// The replay produced no bar, so there is no boundary to continue from.
    #[error("cannot cut over: the replay evaluated no bars")]
    EmptyReplay,
    /// The next live bar skipped past the expected contiguous open time — a missed bar.
    #[error("cutover gap: expected next bar open at {expected_open_ms}ms, got {got_open_ms}ms")]
    Gap {
        /// The open time (ms) the next contiguous bar should have had.
        expected_open_ms: i64,
        /// The open time (ms) actually delivered.
        got_open_ms: i64,
    },
}

/// The result of feeding one live bar through the cutover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutoverStep {
    /// The bar was already covered by the replay window (`open <= last`) — dropped, not re-evaluated.
    Duplicate,
    /// A contiguous live bar was evaluated in place; carries the session's decision.
    Evaluated(EvalOutput),
}

/// Drives the in-process bootstrap→live handoff over a warmed [`EvaluatorSession`].
pub struct Cutover {
    session: EvaluatorSession,
    /// Open time (ms) of the last evaluated base bar; the next contiguous bar opens at `+ interval_ms`.
    last_open_ms: i64,
    /// The base bar interval in ms (contiguity step).
    interval_ms: i64,
    /// Whether the session has been flipped live yet (so `go_live` runs exactly once).
    live: bool,
}

impl Cutover {
    /// Take ownership of a bootstrap [`Reconstructed`] and continue it live. The boundary is anchored at the
    /// last replayed base bar (`decisions.last().time_ms`); `base` is the resolution of the replayed bars.
    ///
    /// # Errors
    /// [`CutoverError::EmptyReplay`] if the replay evaluated no bars (no boundary to anchor).
    pub fn from_reconstructed(
        reconstructed: Reconstructed,
        base: Resolution,
    ) -> Result<Self, CutoverError> {
        let Reconstructed {
            session, decisions, ..
        } = reconstructed;
        let last_open_ms = decisions
            .last()
            .map(|o| o.time_ms)
            .ok_or(CutoverError::EmptyReplay)?;
        Ok(Self::new(session, last_open_ms, base))
    }

    /// A cutover over an already-warmed `session` whose last evaluated base bar opened at `last_open_ms`,
    /// continuing at the `base` resolution.
    #[must_use]
    pub fn new(session: EvaluatorSession, last_open_ms: i64, base: Resolution) -> Self {
        Self {
            session,
            last_open_ms,
            interval_ms: i64::from(base.minutes()) * 60_000,
            live: false,
        }
    }

    /// Feed one live base bar. Drops it as a [`CutoverStep::Duplicate`] if the replay already covered it,
    /// evaluates it (flipping the session live in place on the first new bar) if it is contiguous, or
    /// reports a [`CutoverError::Gap`] if it skips ahead.
    ///
    /// # Errors
    /// [`CutoverError::Gap`] if the bar's open time is beyond the next contiguous bar.
    pub fn feed_live_bar(&mut self, bar: &Bar) -> Result<CutoverStep, CutoverError> {
        let open = bar.open_time().millis();
        if open <= self.last_open_ms {
            // Already covered by the replay — drop without re-evaluating (no duplicate, state untouched).
            return Ok(CutoverStep::Duplicate);
        }
        let expected = self.last_open_ms + self.interval_ms;
        if open != expected {
            return Err(CutoverError::Gap {
                expected_open_ms: expected,
                got_open_ms: open,
            });
        }
        // Contiguous new bar: flip live in place exactly once (the cutover), then evaluate.
        if !self.live {
            self.session.go_live();
            self.live = true;
        }
        let out = self.session.on_bar(bar);
        self.last_open_ms = open;
        Ok(CutoverStep::Evaluated(out))
    }

    /// Forward: record the latest funding rate on the session's as-of context.
    pub fn observe_funding(&mut self, value: rust_decimal::Decimal) {
        self.session.observe_funding(value);
    }

    /// Forward: record the latest open interest.
    pub fn observe_open_interest(&mut self, value: rust_decimal::Decimal) {
        self.session.observe_open_interest(value);
    }

    /// Forward: record the latest premium.
    pub fn observe_premium(&mut self, value: rust_decimal::Decimal) {
        self.session.observe_premium(value);
    }

    /// The session's current phase (`Replay` until the first live bar flips it to `Live`).
    #[must_use]
    pub fn mode(&self) -> SessionMode {
        self.session.mode()
    }

    /// Whether the cutover has flipped the session live.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.live
    }

    /// The open time (ms) of the last evaluated bar (the current continuity boundary).
    #[must_use]
    pub fn last_open_ms(&self) -> i64 {
        self.last_open_ms
    }

    /// Read-only access to the underlying session.
    #[must_use]
    pub fn session(&self) -> &EvaluatorSession {
        &self.session
    }

    /// Consume the cutover, returning the (now-live) session.
    #[must_use]
    pub fn into_session(self) -> EvaluatorSession {
        self.session
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Price, Qty, Timestamp};
    use qe_risk::{CalibrationProfile, Fraction};
    use qe_signal::{
        CatalogueConfig, Clause, ExitParams, FeatureSchema, Genome, RiskParams, RuleSet,
        CLAUSES_PER_SET, REP_VERSION,
    };
    use qe_vintage::Vintage;
    use rust_decimal::Decimal;

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

    /// A base 5m bar at index `i` (open = i*5m), matching the QE-209 fixture shape.
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

    fn off_clause() -> Clause {
        Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        }
    }

    /// A genome that cycles Enter → Hold… → Exit once warm — non-trivial deterministic decisions.
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

    fn vintage() -> Vintage {
        use qe_determinism::Lineage;
        use qe_vintage::{VintageContent, VINTAGE_FORMAT_VERSION};
        let genomes = vec![cycling_genome(3)];
        let weights = vec![1.0];
        let content = VintageContent {
            format_version: VINTAGE_FORMAT_VERSION,
            vintage_id: "qe-211-test".to_owned(),
            chromosomes: genomes,
            weights,
            calibration: CalibrationProfile::new(Fraction::new(Decimal::new(5, 1)).unwrap()),
            worst_case_loss: None,
            lineage: Lineage::new("cfg", "snap", "commit", vec![]),
        };
        Vintage::seal(content).unwrap()
    }

    /// Feed bars `[lo, hi)` to a session, returning the per-bar outputs.
    fn feed(session: &mut EvaluatorSession, lo: i64, hi: i64) -> Vec<EvalOutput> {
        (lo..hi).map(|i| session.on_bar(&bar(i))).collect()
    }

    /// **The AC.** The cutover drops the overlap (no duplicate), rejects no contiguous bar (no skip), and
    /// its live decisions match a continuously-running reference that flipped live at the same bar.
    #[test]
    fn cutover_matches_continuous_reference_with_no_dup_or_gap() {
        const N: i64 = 30;
        const K: i64 = 20; // cutover boundary: replay covered bars 0..K (last replayed = K-1)

        // Continuous reference: one session fed 0..N, flipping live exactly at bar K.
        let mut reference = EvaluatorSession::new(vintage(), &cfg());
        let _ = feed(&mut reference, 0, K);
        reference.go_live();
        let ref_live = feed(&mut reference, K, N); // outputs for K..N, mode == Live

        // Cutover path: a session that replayed 0..K (mode Replay), then continues live.
        let mut booted = EvaluatorSession::new(vintage(), &cfg());
        let _ = feed(&mut booted, 0, K);
        let mut cutover = Cutover::new(booted, bar(K - 1).open_time().millis(), Resolution::M5);
        assert_eq!(cutover.mode(), SessionMode::Replay);

        // wss re-delivers the overlap (K-2, K-1) — must be dropped as duplicates, not re-evaluated.
        assert_eq!(
            cutover.feed_live_bar(&bar(K - 2)).unwrap(),
            CutoverStep::Duplicate
        );
        assert_eq!(
            cutover.feed_live_bar(&bar(K - 1)).unwrap(),
            CutoverStep::Duplicate
        );

        // Then the genuinely-new bars K..N flow live.
        let mut cut_live = Vec::new();
        for i in K..N {
            match cutover.feed_live_bar(&bar(i)).unwrap() {
                CutoverStep::Evaluated(out) => cut_live.push(out),
                CutoverStep::Duplicate => panic!("bar {i} should be evaluated, not a duplicate"),
            }
        }

        // No duplicated or skipped bar, and the live decisions match the continuous reference bar-for-bar
        // (including mode == Live and time_ms).
        assert_eq!(cut_live, ref_live);
        assert!(cutover.is_live() && cutover.mode() == SessionMode::Live);
    }

    #[test]
    fn duplicate_bar_does_not_advance_state() {
        let mut booted = EvaluatorSession::new(vintage(), &cfg());
        let _ = feed(&mut booted, 0, 10);
        let mut cutover = Cutover::new(booted, bar(9).open_time().millis(), Resolution::M5);
        // A bar at or before the boundary is a duplicate and does not move the boundary or flip live.
        assert_eq!(
            cutover.feed_live_bar(&bar(9)).unwrap(),
            CutoverStep::Duplicate
        );
        assert_eq!(
            cutover.feed_live_bar(&bar(5)).unwrap(),
            CutoverStep::Duplicate
        );
        assert_eq!(cutover.last_open_ms(), bar(9).open_time().millis());
        assert!(!cutover.is_live());
        assert_eq!(cutover.mode(), SessionMode::Replay);
    }

    #[test]
    fn gap_bar_is_reported() {
        let mut booted = EvaluatorSession::new(vintage(), &cfg());
        let _ = feed(&mut booted, 0, 10);
        let mut cutover = Cutover::new(booted, bar(9).open_time().millis(), Resolution::M5);
        // Bar 11 skips bar 10 → a gap (expected bar 10's open, got bar 11's open).
        let err = cutover.feed_live_bar(&bar(11)).unwrap_err();
        assert_eq!(
            err,
            CutoverError::Gap {
                expected_open_ms: bar(10).open_time().millis(),
                got_open_ms: bar(11).open_time().millis(),
            }
        );
    }

    #[test]
    fn first_live_bar_flips_mode_in_place() {
        let mut booted = EvaluatorSession::new(vintage(), &cfg());
        let _ = feed(&mut booted, 0, 10);
        let mut cutover = Cutover::new(booted, bar(9).open_time().millis(), Resolution::M5);
        assert_eq!(cutover.mode(), SessionMode::Replay);
        let step = cutover.feed_live_bar(&bar(10)).unwrap();
        assert!(matches!(step, CutoverStep::Evaluated(_)));
        assert_eq!(cutover.mode(), SessionMode::Live);
        assert!(cutover.is_live());
    }

    #[test]
    fn empty_replay_is_rejected() {
        // A Reconstructed with no decisions has no boundary to anchor.
        let session = EvaluatorSession::new(vintage(), &cfg());
        let reconstructed = Reconstructed {
            session,
            decisions: Vec::new(),
            coarse_bars: Vec::new(),
            bars_replayed: 0,
            last_mark_price: None,
        };
        assert!(matches!(
            Cutover::from_reconstructed(reconstructed, Resolution::M5),
            Err(CutoverError::EmptyReplay)
        ));
    }

    #[test]
    fn from_reconstructed_anchors_on_last_replayed_bar() {
        let mut session = EvaluatorSession::new(vintage(), &cfg());
        let decisions = feed(&mut session, 0, 10);
        let last_time = decisions.last().unwrap().time_ms;
        let reconstructed = Reconstructed {
            session,
            decisions,
            coarse_bars: Vec::new(),
            bars_replayed: 10,
            last_mark_price: None,
        };
        let cutover = Cutover::from_reconstructed(reconstructed, Resolution::M5).unwrap();
        assert_eq!(cutover.last_open_ms(), last_time);
        assert_eq!(last_time, bar(9).open_time().millis());
    }
}
