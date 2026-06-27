//! qe-wfo — walk-forward optimisation (QD MAP-Elites) and backtest realism.
//!
//! - [`friction`] (QE-109) — the execution-friction & funding cost model: fees (taker/maker),
//!   size-dependent slippage, funding accrued from the actual historical series, and a
//!   cost-sensitivity sweep, returning a decomposed `gross / fees / slippage / funding` P&L.
//! - [`genome`] (QE-110) — the strategy genome: a fixed-structure rule-bank representation over
//!   quantised feature states that the QD/DE search mutates, recombines, and niches; emits a per-bar
//!   `Decision` stream the backtester (QE-120) drives through `friction`.
//! - [`archive`] (QE-111) — the QD/MAP-Elites behaviour descriptors: genotype-derived family /
//!   timescale / holding axes, the per-direction grid resolution + Deep-Grid sub-population size, and
//!   the descriptor-stability metric that keeps a genome's niche stable across walk-forward windows.
//! - [`operator`] (QE-112) — adaptive operator selection: the sliding-window credit bandit that
//!   allocates search budget across the variation operators from in-training novelty/improvement only
//!   (never OOS), shifting toward exploration on a sparse archive and exploitation on a dense one.
//! - [`fitness`] (QE-113) — geometric (time-average log-growth) fitness on net-of-cost returns with
//!   absorbing near-ruin, plus noise-robust multi-window evaluation and SE-aware elite replacement.
//! - [`cv`] (QE-113) — purged + embargoed cross-validation: leakage-free train/test splits whose
//!   information windows are provably disjoint including the indicator lookback.
//! - [`lifecycle`] (QE-114) — the phased-lifecycle quality gate: exploration→exploitation graduation by
//!   evaluation depth plus a robust validation-distribution threshold, so only survivors persist and
//!   early lucky candidates do not.
//! - [`walkforward`] (QE-117) — the anchored/rolling walk-forward window manager: purge+embargo-gapped
//!   train→validate windows (leakage-free including lookback) that carry the archive across transitions.
//! - [`mapelites`] (QE-118) — the QD MAP-Elites archive: per-direction Deep-Grid sub-populations over the
//!   QE-111 niche grid, niche parent sampling, and embarrassingly-parallel deterministic evaluation.
//! - [`variation`] (QE-119) — the variation operators (local-refine / explore / fresh-random) and the
//!   adaptive-selection driver that allocates operator budget by productivity (QE-112 credit).

pub mod archive;
pub mod cv;
pub mod fitness;
pub mod friction;
pub mod genome;
pub mod lifecycle;
pub mod mapelites;
pub mod operator;
pub mod variation;
pub mod walkforward;

pub use archive::{
    cell_reassignment_rate, descriptor_for, family_of, grid_cells, Cell, HoldingBand,
    IndicatorFamily, TimescaleBand, CELLS_PER_DIRECTION, STABILITY_THRESHOLD, SUBPOP_SIZE,
};
pub use cv::{Fold, PurgedKFold};
pub use fitness::{geom_return, log_growth, should_replace, NoiseRobustFitness, DEFAULT_K_SIGMA};
pub use friction::{
    cost_sweep, simulate, Event, FeeSchedule, Fill, FrictionConfig, FundingStamp, Liquidity,
    PnlBreakdown, Position, SlippageModel,
};
pub use genome::{
    Clause, Decision, ExitParams, Genome, PositionState, RiskParams, RuleSet, CLAUSES_PER_SET,
    MAX_SIZE_BPS, REP_VERSION,
};
pub use lifecycle::{
    Phase, QualityGate, QualityThreshold, ThresholdPolicy, DEFAULT_MIN_EXPLOITATION_WINDOWS,
    DEFAULT_QUANTILE,
};
pub use mapelites::{
    evaluate_and_insert, evaluate_batch, DirectionArchive, Elite, InsertOutcome, Insertion,
    MapElitesArchive, SubPopulation,
};
pub use operator::{
    ApplicationOutcome, Operator, OperatorSelector, DEFAULT_EPSILON, DEFAULT_WINDOW,
    NOVELTY_REWARD, OPERATORS,
};
pub use variation::{
    explore, fresh_random, local_refine, StepReport, VariationDriver, LOCAL_SIZE_STEP,
};
pub use walkforward::{WalkForward, Window, WindowMode};

/// Returns this crate's package name. Placeholder until later tickets add real APIs.
#[must_use]
pub fn crate_name() -> &'static str {
    "qe-wfo"
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_set() {
        assert_eq!(super::crate_name(), "qe-wfo");
    }
}
