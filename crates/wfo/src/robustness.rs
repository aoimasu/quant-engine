//! Elite robustness gates (QE-124).
//!
//! Evolution overfits efficiently, so an elite is not trusted until it survives **re-evaluation under
//! perturbation**. [`assess_robustness`] runs three gates over a candidate elite and returns a
//! [`RobustnessReport`] that *flags* (does not delete — that policy lives in QE-123/QE-125):
//!
//! 1. **Minimum-trade-count** — too few entries ⇒ too little evidence.
//! 2. **Parameter-perturbation robustness** — a cloud of ±ε [`jitter`]ed genomes is re-evaluated through
//!    the QE-120 backtester; an over-fit elite perched on a fitness spike *collapses*, a robust one
//!    degrades gracefully.
//! 3. **Descriptor stability** — the QE-111 `cell_reassignment_rate` over the same jitter cloud: an elite
//!    sitting on a behavioural-band boundary (e.g. `max_holding_bars = SCALP_MAX_BARS`) flips its niche
//!    under a 1-bar nudge — an *unstable descriptor*.
//!
//! Every jitter sample is seeded by `task_rng(seed, i)` (QE-006), so the whole assessment is
//! byte-deterministic and evaluation-order independent.

use qe_determinism::{task_rng, DetRng};
use qe_domain::Direction;
use rand_core::RngCore;

use crate::archive::{cell_reassignment_rate, descriptor_for, Cell, STABILITY_THRESHOLD};
use crate::backtest::{backtest, BacktestConfig, Bar};
use crate::genome::Genome;
use qe_signal::FeatureSchema;

/// Default number of ±ε jitter samples per assessment.
pub const DEFAULT_ROBUSTNESS_SAMPLES: usize = 16;
/// Default ± nudge (in quantised states) applied to each enabled clause's `lo`/`hi` bounds.
pub const DEFAULT_EPS_STATE: u16 = 1;
/// Default ± nudge (in bars) applied to `exit.max_holding_bars`.
pub const DEFAULT_EPS_HOLDING: u16 = 2;
/// Default ± nudge (in basis points) applied to `risk.size_bps`.
pub const DEFAULT_EPS_SIZE_BPS: u16 = 500;
/// Default minimum entry count below which an elite is flagged.
pub const DEFAULT_MIN_TRADES: usize = 10;
/// Default fraction of the elite's fitness a jitter sample must retain to not count as a collapse.
pub const DEFAULT_RETAIN_FRACTION: f64 = 0.5;
/// Default fraction of jitter samples allowed to collapse before the perturbation gate fails.
pub const DEFAULT_MAX_COLLAPSE_FRACTION: f64 = 0.25;
/// Default ceiling on the descriptor reassignment rate (QE-111 `STABILITY_THRESHOLD`).
pub const DEFAULT_MAX_DESCRIPTOR_REASSIGNMENT: f64 = STABILITY_THRESHOLD;

/// Configuration for [`assess_robustness`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RobustnessConfig {
    /// Number of ±ε jitter samples.
    pub samples: usize,
    /// ± state nudge for clause bounds.
    pub eps_state: u16,
    /// ± bar nudge for `max_holding_bars`.
    pub eps_holding: u16,
    /// ± bps nudge for `size_bps`.
    pub eps_size_bps: u16,
    /// Minimum entry count gate.
    pub min_trades: usize,
    /// Fraction of the elite's fitness a sample must retain to survive.
    pub retain_fraction: f64,
    /// Maximum fraction of samples allowed to collapse.
    pub max_collapse_fraction: f64,
    /// Maximum tolerated descriptor reassignment rate.
    pub max_descriptor_reassignment: f64,
}

impl Default for RobustnessConfig {
    fn default() -> Self {
        RobustnessConfig {
            samples: DEFAULT_ROBUSTNESS_SAMPLES,
            eps_state: DEFAULT_EPS_STATE,
            eps_holding: DEFAULT_EPS_HOLDING,
            eps_size_bps: DEFAULT_EPS_SIZE_BPS,
            min_trades: DEFAULT_MIN_TRADES,
            retain_fraction: DEFAULT_RETAIN_FRACTION,
            max_collapse_fraction: DEFAULT_MAX_COLLAPSE_FRACTION,
            max_descriptor_reassignment: DEFAULT_MAX_DESCRIPTOR_REASSIGNMENT,
        }
    }
}

impl RobustnessConfig {
    /// The QE-124 default gate configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        RobustnessConfig::default()
    }
}

/// Which robustness gate an elite failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Fewer than `min_trades` entries — too little evidence.
    MinTrades,
    /// Too many ±ε jitter samples collapsed in fitness — an over-fit spike.
    PerturbationCollapse,
    /// The behavioural descriptor flips under ±ε jitter — a boundary-sitting niche.
    UnstableDescriptor,
}

/// The outcome of [`assess_robustness`] — the per-gate verdicts and the evidence behind them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RobustnessReport {
    /// The elite's own (unperturbed) fitness.
    pub base_fitness: f64,
    /// The elite's own entry count.
    pub base_trades: usize,
    /// Number of jitter samples evaluated.
    pub samples: usize,
    /// How many jitter samples collapsed.
    pub collapsed: usize,
    /// `collapsed / samples` (`0.0` when `samples == 0`).
    pub collapsed_fraction: f64,
    /// Descriptor reassignment rate of the jitter cloud (QE-111 metric).
    pub descriptor_reassignment: f64,
    /// Whether the minimum-trade gate passed.
    pub min_trades_ok: bool,
    /// Whether the perturbation gate passed.
    pub perturbation_ok: bool,
    /// Whether the descriptor-stability gate passed.
    pub descriptor_ok: bool,
}

impl RobustnessReport {
    /// Whether the elite cleared all three gates.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.min_trades_ok && self.perturbation_ok && self.descriptor_ok
    }

    /// Whether the elite is flagged (failed at least one gate).
    #[must_use]
    pub fn flagged(&self) -> bool {
        !self.passed()
    }

    /// The reasons the elite was flagged (empty if it [`passed`](Self::passed)).
    #[must_use]
    pub fn reasons(&self) -> Vec<RejectReason> {
        let mut out = Vec::new();
        if !self.min_trades_ok {
            out.push(RejectReason::MinTrades);
        }
        if !self.perturbation_ok {
            out.push(RejectReason::PerturbationCollapse);
        }
        if !self.descriptor_ok {
            out.push(RejectReason::UnstableDescriptor);
        }
        out
    }
}

/// `v + δ` with `δ` uniform in `[-eps, eps]`, saturating at the `u16` range.
fn nudge_u16(v: u16, eps: u16, rng: &mut DetRng) -> u16 {
    if eps == 0 {
        return v;
    }
    let span = 2 * i64::from(eps) + 1;
    let delta = (rng.next_u64() % span as u64) as i64 - i64::from(eps);
    (i64::from(v) + delta).clamp(0, i64::from(u16::MAX)) as u16
}

/// A ±ε perturbation of `base`: each enabled clause's `lo`/`hi`, `max_holding_bars`, and `size_bps` are
/// nudged within the configured epsilons, then [`repair`](Genome::repair)ed back onto the validity
/// manifold. **Structural** genes (`enabled`, `feature`) are deliberately left untouched — the gate
/// probes sensitivity to small *parameter* moves, not to a different family/timescale niche.
#[must_use]
pub fn jitter(
    base: &Genome,
    cfg: &RobustnessConfig,
    schema: &FeatureSchema,
    rng: &mut DetRng,
) -> Genome {
    let mut g = base.clone();
    for set in [&mut g.long_entry, &mut g.short_entry] {
        for clause in set.clauses.iter_mut().filter(|c| c.enabled) {
            clause.lo = nudge_u16(clause.lo, cfg.eps_state, rng);
            clause.hi = nudge_u16(clause.hi, cfg.eps_state, rng);
        }
    }
    g.exit.max_holding_bars = nudge_u16(g.exit.max_holding_bars, cfg.eps_holding, rng);
    g.risk.size_bps = nudge_u16(g.risk.size_bps, cfg.eps_size_bps, rng);
    g.repair(schema);
    g
}

/// Assess an `elite` against the three QE-124 robustness gates over `bars`, returning a flagging
/// [`RobustnessReport`]. `backtest_cfg` drives the re-evaluation (use a low `min_trades` there so the
/// fitness reflects reality — the trade gate is `cfg.min_trades`). Deterministic in `seed`.
#[must_use]
pub fn assess_robustness(
    elite: &Genome,
    bars: &[Bar],
    schema: &FeatureSchema,
    backtest_cfg: &BacktestConfig,
    cfg: &RobustnessConfig,
    seed: u64,
) -> RobustnessReport {
    let base = backtest(elite, bars, backtest_cfg);
    let base_fitness = base.fitness.mean;
    let base_trades = base.trades;
    let min_trades_ok = base_trades >= cfg.min_trades;

    // Draw the jitter cloud (each sample independently seeded → order-independent + deterministic).
    let jitters: Vec<Genome> = (0..cfg.samples)
        .map(|i| {
            let mut rng = task_rng(seed, i as u64);
            jitter(elite, cfg, schema, &mut rng)
        })
        .collect();

    // Gate 2 — fitness collapse under perturbation.
    let mut collapsed = 0usize;
    for j in &jitters {
        let r = backtest(j, bars, backtest_cfg);
        let fit = r.fitness.mean;
        let collapse = r.trades < cfg.min_trades
            || !fit.is_finite()
            || fit < cfg.retain_fraction * base_fitness;
        if collapse {
            collapsed += 1;
        }
    }
    let collapsed_fraction = if jitters.is_empty() {
        0.0
    } else {
        collapsed as f64 / jitters.len() as f64
    };
    let perturbation_ok = collapsed_fraction <= cfg.max_collapse_fraction;

    // Gate 3 — descriptor stability over the same cloud, per occupied direction.
    let mut base_cells: Vec<Option<Cell>> = Vec::new();
    let mut jit_cells: Vec<Option<Cell>> = Vec::new();
    for dir in [Direction::Long, Direction::Short] {
        let Some(base_cell) = descriptor_for(elite, dir, schema) else {
            continue; // the elite does not occupy this direction — nothing to be unstable
        };
        for j in &jitters {
            base_cells.push(Some(base_cell));
            jit_cells.push(descriptor_for(j, dir, schema));
        }
    }
    let descriptor_reassignment = cell_reassignment_rate(&base_cells, &jit_cells);
    let descriptor_ok = descriptor_reassignment <= cfg.max_descriptor_reassignment;

    RobustnessReport {
        base_fitness,
        base_trades,
        samples: jitters.len(),
        collapsed,
        collapsed_fraction,
        descriptor_reassignment,
        min_trades_ok,
        perturbation_ok,
        descriptor_ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::SCALP_MAX_BARS;
    use crate::genome::{Clause, ExitParams, RiskParams, RuleSet, CLAUSES_PER_SET, REP_VERSION};
    use qe_signal::{CatalogueConfig, FeatureVector, QState};
    use rust_decimal::Decimal;

    fn schema() -> FeatureSchema {
        FeatureSchema::from_catalogue(&CatalogueConfig { states: 5 })
    }

    /// A long-only genome reading feature 0 with the inclusive band `[lo, hi]`, holding `hold` bars.
    fn long_genome(lo: u16, hi: u16, hold: u16) -> Genome {
        let off = Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        };
        let mut clauses = [off; CLAUSES_PER_SET];
        clauses[0] = Clause {
            enabled: true,
            feature: 0,
            lo,
            hi,
        };
        Genome {
            version: REP_VERSION,
            long_entry: RuleSet {
                clauses,
                min_satisfied: 1,
            },
            short_entry: RuleSet {
                clauses: [off; CLAUSES_PER_SET],
                min_satisfied: 1,
            },
            exit: ExitParams {
                max_holding_bars: hold,
                exit_on_opposite: false,
            },
            risk: RiskParams { size_bps: 5_000 },
        }
    }

    fn bar(i: usize, schema: &FeatureSchema, state: u16, price: i64) -> Bar {
        let mut states = vec![None; schema.len()];
        states[0] = Some(QState::from_index(state));
        Bar {
            features: FeatureVector {
                time_ms: i as i64 * 60_000,
                states,
            },
            price: Decimal::from(price),
            volume: Decimal::from(1000),
            funding_rate: None,
        }
    }

    /// Zig-zag series (price alternates 100/110) where **odd** signal bars (whose fill→exit window rises)
    /// carry the GOOD state 3 and **even** signal bars (whose window falls) carry the TRAP states 2 / 4.
    /// A band of exactly `{3}` only ever takes the rising leg; widening it to 2 or 4 adds falling legs.
    fn zigzag_bars(schema: &FeatureSchema, n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let price = if i % 2 == 0 { 100 } else { 110 };
                let state = if i % 2 == 1 {
                    3 // odd bar: fill at i+1 (even, price 100) → exit i+2 (110): rising → GOOD
                } else if (i / 2) % 2 == 0 {
                    2 // even bar: fill at i+1 (odd, 110) → exit i+2 (100): falling → TRAP-low
                } else {
                    4 // even bar: falling → TRAP-high
                };
                bar(i, schema, state, price)
            })
            .collect()
    }

    /// Strict uptrend where every bar carries the same warm state in a wide band — friendly to a robust,
    /// wide-band elite.
    fn uptrend_bars(schema: &FeatureSchema, n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let state = (i % 3) as u16 + 1; // states 1..3, all inside a wide band
                bar(i, schema, state, 100 + i as i64)
            })
            .collect()
    }

    fn bt_cfg() -> BacktestConfig {
        BacktestConfig {
            min_trades: 1, // let fitness reflect reality; the trade gate is RobustnessConfig.min_trades
            windows: 4,
            ..BacktestConfig::default()
        }
    }

    #[test]
    fn fragile_overfit_elite_collapses_under_jitter() {
        let s = schema();
        let bars = zigzag_bars(&s, 400);
        // Over-fit: razor-thin band {3} that only ever takes the rising leg.
        let elite = long_genome(3, 3, 20);
        let cfg = RobustnessConfig {
            min_trades: 1, // isolate the perturbation gate (plenty of trades)
            ..RobustnessConfig::default()
        };
        let report = assess_robustness(&elite, &bars, &s, &bt_cfg(), &cfg, 7);
        assert!(
            report.base_fitness > 0.0,
            "elite itself should be profitable"
        );
        assert!(
            report.flagged() && !report.perturbation_ok,
            "fragile elite must be flagged for collapse: {report:?}"
        );
        assert!(report
            .reasons()
            .contains(&RejectReason::PerturbationCollapse));
    }

    #[test]
    fn boundary_elite_has_unstable_descriptor() {
        let s = schema();
        let bars = uptrend_bars(&s, 400);
        // Fitness-robust wide band, but holding sits exactly on the Scalp/Swing boundary (6) so a +ε
        // holding nudge flips the HoldingBand.
        let elite = long_genome(1, 3, SCALP_MAX_BARS);
        let cfg = RobustnessConfig {
            min_trades: 1,
            ..RobustnessConfig::default()
        };
        let report = assess_robustness(&elite, &bars, &s, &bt_cfg(), &cfg, 11);
        assert!(
            report.descriptor_reassignment > DEFAULT_MAX_DESCRIPTOR_REASSIGNMENT,
            "holding-boundary elite should flip niche under jitter: {report:?}"
        );
        assert!(!report.descriptor_ok && report.flagged());
        assert!(report.reasons().contains(&RejectReason::UnstableDescriptor));
    }

    #[test]
    fn robust_elite_passes_all_gates() {
        let s = schema();
        let bars = uptrend_bars(&s, 400);
        // Wide band (every state is profitable in an uptrend) deep inside the Swing band.
        let elite = long_genome(1, 3, 25);
        let cfg = RobustnessConfig {
            min_trades: 1,
            ..RobustnessConfig::default()
        };
        let report = assess_robustness(&elite, &bars, &s, &bt_cfg(), &cfg, 3);
        assert!(
            report.passed(),
            "robust elite should clear all gates: {report:?}"
        );
        assert!(report.reasons().is_empty());
    }

    #[test]
    fn rarely_trading_elite_is_flagged_for_min_trades() {
        let s = schema();
        // Only a handful of bars carry the entry state, so the elite trades very little.
        let mut bars = uptrend_bars(&s, 50);
        for (i, b) in bars.iter_mut().enumerate() {
            // Make state 3 (the only entry state) appear just twice.
            b.features.states[0] = Some(QState::from_index(if i == 5 || i == 25 { 3 } else { 0 }));
        }
        let elite = long_genome(3, 3, 10);
        let cfg = RobustnessConfig {
            min_trades: 10,
            ..RobustnessConfig::default()
        };
        let report = assess_robustness(&elite, &bars, &s, &bt_cfg(), &cfg, 5);
        assert!(report.base_trades < 10);
        assert!(!report.min_trades_ok && report.flagged());
        assert!(report.reasons().contains(&RejectReason::MinTrades));
    }

    #[test]
    fn assessment_is_deterministic() {
        let s = schema();
        let bars = zigzag_bars(&s, 300);
        let elite = long_genome(3, 3, 20);
        let cfg = RobustnessConfig::with_defaults();
        let a = assess_robustness(&elite, &bars, &s, &bt_cfg(), &cfg, 99);
        let b = assess_robustness(&elite, &bars, &s, &bt_cfg(), &cfg, 99);
        assert_eq!(a, b);
    }
}
