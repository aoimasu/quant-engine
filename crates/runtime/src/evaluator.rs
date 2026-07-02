//! Evaluator session — one stateful object that runs through bootstrap (replay) and live (QE-207).
//!
//! The same session evaluates a sealed vintage's chromosomes bar-by-bar through a **replay** phase
//! (bootstrap, fed historical bars) and a **live** phase (wss continuation) with **no new object and no
//! state copy**: [`go_live`](EvaluatorSession::go_live) flips a label and touches nothing else, so the
//! decision stream cannot change at the boundary (the AC). The session loads the vintage
//! (chromosomes/ensemble/calibration) read-only and drives the shared QE-206 factor join + `Genome::decide`.

use rust_decimal::Decimal;

use qe_domain::Bar;
use qe_risk::CalibrationProfile;
use qe_signal::{CatalogueConfig, Decision, PositionState};
use qe_vintage::Vintage;

use crate::factor_join::LiveFactorJoin;

/// Which phase the session is in. A *label* over one continuous state — not a state partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    /// Bootstrap: replaying historical bars to reconstruct state.
    Replay,
    /// Live: continuing on wss bars. Reached via [`go_live`](EvaluatorSession::go_live), one-way.
    Live,
}

/// One chromosome's decision on a bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromosomeDecision {
    /// Index into the vintage's `chromosomes` / `weights`.
    pub index: usize,
    /// The genome's signal for this bar.
    pub decision: Decision,
}

/// The session's output for one evaluated bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalOutput {
    /// The bar's open time (epoch ms).
    pub time_ms: i64,
    /// The phase this bar was evaluated in.
    pub mode: SessionMode,
    /// Per-chromosome decisions, aligned to the vintage's chromosomes.
    pub decisions: Vec<ChromosomeDecision>,
}

/// A single evaluator session over a sealed vintage, shared across replay and live.
pub struct EvaluatorSession {
    vintage: Vintage,
    join: LiveFactorJoin,
    /// One position per chromosome (aligned to `vintage.content.chromosomes`).
    positions: Vec<PositionState>,
    mode: SessionMode,
}

impl EvaluatorSession {
    /// Load `vintage` read-only and build a session over the catalogue `cfg` the genomes were evolved
    /// against. Starts in [`SessionMode::Replay`] with every chromosome flat.
    #[must_use]
    pub fn new(vintage: Vintage, cfg: &CatalogueConfig) -> Self {
        let n = vintage.content.chromosomes.len();
        Self {
            vintage,
            join: LiveFactorJoin::new(cfg),
            positions: vec![PositionState::flat(); n],
            mode: SessionMode::Replay,
        }
    }

    /// The current phase.
    #[must_use]
    pub fn mode(&self) -> SessionMode {
        self.mode
    }

    /// Transition replay → live. **One-way and state-preserving**: it only flips the mode label — the
    /// factor-join warm-up and every chromosome's position carry over untouched, so the decision stream is
    /// continuous across the boundary. A second call is a no-op.
    pub fn go_live(&mut self) {
        self.mode = SessionMode::Live;
    }

    /// Record the latest funding rate (forwarded to the factor join's as-of context).
    pub fn observe_funding(&mut self, value: Decimal) {
        self.join.observe_funding(value);
    }

    /// Record the latest open interest.
    pub fn observe_open_interest(&mut self, value: Decimal) {
        self.join.observe_open_interest(value);
    }

    /// Record the latest premium.
    pub fn observe_premium(&mut self, value: Decimal) {
        self.join.observe_premium(value);
    }

    /// Evaluate one closed base bar: assemble its factor row, decide for every chromosome against its
    /// current position, advance the positions, and return the per-chromosome decisions tagged with the
    /// current mode.
    pub fn on_bar(&mut self, bar: &Bar) -> EvalOutput {
        let fv = self.join.on_bar(bar);
        let mut decisions = Vec::with_capacity(self.vintage.content.chromosomes.len());
        for (i, genome) in self.vintage.content.chromosomes.iter().enumerate() {
            let decision = genome.decide(&fv, self.positions[i]);
            // `advance` is the shared decide/advance counterpart in qe-signal — one source of truth so
            // live and training (backtest) bookkeeping cannot drift (train/live decision parity).
            self.positions[i] = self.positions[i].advance(decision);
            decisions.push(ChromosomeDecision { index: i, decision });
        }
        EvalOutput {
            time_ms: fv.time_ms,
            mode: self.mode,
            decisions,
        }
    }

    /// Read-only: the vintage identifier.
    #[must_use]
    pub fn vintage_id(&self) -> &str {
        &self.vintage.content.vintage_id
    }

    /// Read-only: the per-chromosome ensemble weights.
    #[must_use]
    pub fn weights(&self) -> &[f64] {
        &self.vintage.content.weights
    }

    /// Read-only: the per-vintage calibration profile.
    #[must_use]
    pub fn calibration(&self) -> &CalibrationProfile {
        &self.vintage.content.calibration
    }

    /// Read-only: the number of chromosomes.
    #[must_use]
    pub fn chromosome_count(&self) -> usize {
        self.vintage.content.chromosomes.len()
    }

    /// Read-only: the current per-chromosome positions (aligned to the vintage's chromosomes). The
    /// reconstructed-state builder (QE-210) reads these as the bootstrap's per-strategy positions.
    #[must_use]
    pub fn positions(&self) -> &[PositionState] {
        &self.positions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Price, Qty, Resolution, Timestamp};
    use qe_risk::Fraction;
    use qe_signal::{
        Clause, ExitParams, FeatureSchema, Genome, RiskParams, RuleSet, CLAUSES_PER_SET,
        REP_VERSION,
    };
    use rust_decimal::Decimal;

    const MIN: i64 = 60_000;

    fn cfg() -> CatalogueConfig {
        CatalogueConfig::default()
    }

    fn off_clause() -> Clause {
        Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        }
    }

    /// A genome that goes long whenever feature 0 is warm (clause spans all states) and exits after
    /// `max_holding` bars — so once warm it cycles Enter → Hold… → Exit → Enter, giving non-trivial,
    /// deterministic decisions.
    fn cycling_genome(max_holding: u16) -> Genome {
        let num_states = FeatureSchema::from_catalogue(&cfg()).num_states();
        let mut clauses = [off_clause(); CLAUSES_PER_SET];
        clauses[0] = Clause {
            enabled: true,
            feature: 0,
            lo: 0,
            hi: num_states - 1, // any warm state of feature 0
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
        use qe_determinism::Lineage;
        use qe_vintage::{VintageContent, VINTAGE_FORMAT_VERSION};
        let n = genomes.len();
        let weights = vec![1.0 / n as f64; n];
        let content = VintageContent {
            format_version: VINTAGE_FORMAT_VERSION,
            vintage_id: "test-vintage".to_string(),
            chromosomes: genomes,
            weights,
            calibration: CalibrationProfile::new(Fraction::new(Decimal::new(2, 1)).unwrap()),
            worst_case_loss: Some(0.2),
            lineage: Lineage::new("cfg", "snap", "commit", vec![1]),
        };
        Vintage::seal(content).unwrap()
    }

    fn base_bar(slot: i64) -> Bar {
        let base = 100 + (slot % 9);
        Bar::new(
            Timestamp::from_millis(slot * 5 * MIN),
            Resolution::M5,
            Price::new(Decimal::from(base)).unwrap(),
            Price::new(Decimal::from(base + 4)).unwrap(),
            Price::new(Decimal::from(base - 3)).unwrap(),
            Price::new(Decimal::from(base + 1)).unwrap(),
            Qty::new(Decimal::from(10 + (slot % 5))).unwrap(),
            1,
        )
        .unwrap()
    }

    fn run(
        session: &mut EvaluatorSession,
        bars: &[Bar],
        go_live_at: Option<usize>,
    ) -> Vec<EvalOutput> {
        bars.iter()
            .enumerate()
            .map(|(i, bar)| {
                if Some(i) == go_live_at {
                    session.go_live();
                }
                session.on_bar(bar)
            })
            .collect()
    }

    #[test]
    fn replay_to_live_transition_does_not_change_decisions() {
        // AC: the same session run wholly in replay vs switched to live partway produces identical
        // decisions bar-for-bar — proving no state copy / reset at the boundary.
        let bars: Vec<Bar> = (0..60).map(base_bar).collect();

        let mut all_replay = EvaluatorSession::new(
            vintage_of(vec![cycling_genome(2), cycling_genome(3)]),
            &cfg(),
        );
        let replay_out = run(&mut all_replay, &bars, None);

        let mut switched = EvaluatorSession::new(
            vintage_of(vec![cycling_genome(2), cycling_genome(3)]),
            &cfg(),
        );
        let switched_out = run(&mut switched, &bars, Some(30));

        // Decisions are identical across the boundary (modes differ, decisions do not).
        let dec = |o: &EvalOutput| o.decisions.clone();
        assert_eq!(
            replay_out.iter().map(dec).collect::<Vec<_>>(),
            switched_out.iter().map(dec).collect::<Vec<_>>(),
            "decisions must be identical whether or not the session went live mid-stream"
        );
        // The mode label flips at the boundary, but nothing else.
        assert!(switched_out[..30]
            .iter()
            .all(|o| o.mode == SessionMode::Replay));
        assert!(switched_out[30..]
            .iter()
            .all(|o| o.mode == SessionMode::Live));
        assert!(replay_out.iter().all(|o| o.mode == SessionMode::Replay));
    }

    #[test]
    fn decisions_are_non_trivial() {
        // The parity above is only meaningful if the genome actually trades.
        let bars: Vec<Bar> = (0..60).map(base_bar).collect();
        let mut s = EvaluatorSession::new(vintage_of(vec![cycling_genome(2)]), &cfg());
        let out = run(&mut s, &bars, None);
        let decisions: Vec<Decision> = out
            .iter()
            .flat_map(|o| o.decisions.iter().map(|d| d.decision))
            .collect();
        assert!(
            decisions.iter().any(|d| matches!(d, Decision::Enter(_))),
            "fixture must produce at least one Enter"
        );
        assert!(
            decisions.iter().any(|d| matches!(d, Decision::Exit)),
            "fixture must produce at least one Exit"
        );
    }

    #[test]
    fn go_live_is_one_way_and_idempotent() {
        let bars: Vec<Bar> = (0..10).map(base_bar).collect();
        let mut s = EvaluatorSession::new(vintage_of(vec![cycling_genome(2)]), &cfg());
        assert_eq!(s.mode(), SessionMode::Replay);
        s.on_bar(&bars[0]);
        s.go_live();
        assert_eq!(s.mode(), SessionMode::Live);
        s.go_live(); // no-op
        assert_eq!(s.mode(), SessionMode::Live);
    }

    #[test]
    fn read_only_vintage_load() {
        let s = EvaluatorSession::new(
            vintage_of(vec![cycling_genome(2), cycling_genome(4)]),
            &cfg(),
        );
        assert_eq!(s.chromosome_count(), 2);
        assert_eq!(s.weights().len(), 2);
        assert_eq!(s.vintage_id(), "test-vintage");
        assert!((s.weights().iter().sum::<f64>() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn single_genome_exits_at_max_holding() {
        // A 1-genome session entering then holding must Exit exactly when bars_held reaches max_holding.
        let bars: Vec<Bar> = (0..60).map(base_bar).collect();
        let mut s = EvaluatorSession::new(vintage_of(vec![cycling_genome(2)]), &cfg());
        let out = run(&mut s, &bars, None);
        // Find the first Enter, then assert the cycle Enter, Hold, Hold, Exit (max_holding = 2).
        let seq: Vec<Decision> = out.iter().map(|o| o.decisions[0].decision).collect();
        let first_enter = seq
            .iter()
            .position(|d| matches!(d, Decision::Enter(_)))
            .expect("an Enter");
        assert!(matches!(seq[first_enter], Decision::Enter(_)));
        assert_eq!(seq[first_enter + 1], Decision::Hold);
        assert_eq!(seq[first_enter + 2], Decision::Hold);
        assert_eq!(seq[first_enter + 3], Decision::Exit);
    }
}
