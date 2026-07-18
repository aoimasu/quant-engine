//! QE-451 **Phase 1b** — IC pre-screen + tradability gates at archive insertion (QE-450 §4.6, §5 item 2/5).
//! **Default-off machinery** exercised by tests; nothing here is wired into the default pipeline.
//!
//! - **IC pre-screen** (item 2, QE-434): purged rank-IC two-fold sign-consistency + Benjamini–Hochberg FDR
//!   across all trees screened this generation, reusing `qe_validation::screen_catalogue` verbatim. The
//!   screen filters **compute**, never the hypothesis count — every screened tree (pass AND fail) still
//!   counts toward the deflation basis `N` upstream.
//! - **Tradability gates** (item 5, §4.6): (a) cost-stressed net `min` over friction multiplier
//!   `m ∈ {1×,2×}` via the merged QE-431 `cost_sweep` — require finite & `> 0`; (b) a max-turnover REJECT
//!   gate (avg hold ≥ 4h ⇒ `turnover ≤ 0.25·n_bars`); (c) a capacity floor `≥ $250k` via an **inlined**
//!   capacity model whose impact coefficients are **duplicated from the shared `qe_risk::SlippageCalibration`**
//!   (firewall-safe — `capacity.rs` lives in `qe-ensemble`, a forbidden edge — guarded by a coefficient-parity
//!   test). Rejected candidates STILL count toward `N` (design §4.6, §5).

use qe_domain::Side;
use qe_risk::SlippageCalibration;
use qe_signal::indicator::expr::ExprTree;
use qe_signal::indicator::Sample;
use qe_validation::{screen_catalogue, IcScreenConfig, IcScreenReport, IndicatorSignals};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::friction::{cost_sweep, Event, Fill, FrictionConfig, Liquidity};
use crate::gp::deflation::{formula_returns, signal_series};
use crate::gp::eval_tree;

/// Default capacity floor (design §4.6c): a formula must support at least this AUM (in $) to be worth a
/// catalogue slot.
pub const CAPACITY_FLOOR: f64 = 250_000.0;

/// Default max per-period turnover fraction (design §4.6b): `0.25` ⇒ avg hold ≥ 4 bars (≥ 4h at 1h bars).
pub const MAX_TURNOVER_FRAC: f64 = 0.25;

/// Build one tree's ordinal-signal column for the IC screen: the quantised `QState::index()` per bar
/// (`f64`), `NaN` while the tree is warming — the shape `qe_validation::screen_catalogue` consumes.
#[must_use]
pub fn tree_ic_signals(
    id: &str,
    tree: &ExprTree,
    samples: &[Sample],
    states: u16,
) -> IndicatorSignals {
    let (_f, series) = eval_tree(tree, samples, states);
    let values = series
        .iter()
        .map(|s| s.map_or(f64::NAN, |v| v as f64))
        .collect();
    IndicatorSignals {
        id: id.to_string(),
        values,
    }
}

/// **IC pre-screen** for a generation of trees (item 2, QE-434). Reuses `screen_catalogue` verbatim:
/// purged rank-IC on two out-of-fold index sets, admitting only a tree whose second fold is same-sign +
/// comparable-magnitude AND clears the Benjamini–Hochberg FDR bar across **all** trees screened. `fold_a`
/// / `fold_b` are the disjoint out-of-fold bar-index sets (from the purged/embargoed CV). The returned
/// report classifies every tree Admit / Flag / Drop; the caller admits only `Verdict::Admit` into the
/// pool but still counts every screened tree toward the deflation basis `N` (the screen filters compute).
#[must_use]
pub fn ic_screen_trees(
    labelled: &[(String, ExprTree)],
    samples: &[Sample],
    states: u16,
    net_returns: &[f64],
    fold_a: &[usize],
    fold_b: &[usize],
    cfg: &IcScreenConfig,
) -> IcScreenReport {
    let signals: Vec<IndicatorSignals> = labelled
        .iter()
        .map(|(id, tree)| tree_ic_signals(id, tree, samples, states))
        .collect();
    screen_catalogue(&signals, net_returns, fold_a, fold_b, cfg)
}

/// Configuration for the tradability gates (design §4.6).
#[derive(Debug, Clone)]
pub struct TradabilityConfig {
    /// Friction multipliers for the cost-stress sweep (design §4.6a: `{1×, 2×}`).
    pub cost_multipliers: Vec<Decimal>,
    /// Max per-period turnover fraction (design §4.6b).
    pub max_turnover_frac: f64,
    /// Capacity floor in $ (design §4.6c).
    pub capacity_floor: f64,
    /// Notional per fill ($) used to price the cost-stress backtest.
    pub fill_notional: Decimal,
    /// Rolling ADV ($) of the traded instrument, keying the participation impact (QE-440).
    pub adv_notional: f64,
    /// The shared slippage/impact calibration (QE-431) both the cost sweep and the inlined capacity derive
    /// from — the single source of truth (no magic literal on the selection path).
    pub calibration: SlippageCalibration,
}

impl Default for TradabilityConfig {
    fn default() -> Self {
        TradabilityConfig {
            cost_multipliers: vec![Decimal::ONE, Decimal::from(2)],
            max_turnover_frac: MAX_TURNOVER_FRAC,
            capacity_floor: CAPACITY_FLOOR,
            fill_notional: Decimal::from(10_000),
            adv_notional: 50_000_000.0,
            calibration: SlippageCalibration::default(),
        }
    }
}

/// The tradability verdict for one candidate. A reject STILL counts toward the deflation basis `N`
/// (design §4.6, §5) — this only tightens the survivor set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TradabilityVerdict {
    /// Passes all gates — admissible to the pool.
    Accept,
    /// Rejected: cost-stressed net (min over `m ∈ {1×,2×}`) not finite & `> 0` (design §4.6a).
    RejectCostStress {
        /// The worst-case (min over multipliers) net money.
        min_net: f64,
    },
    /// Rejected: realised turnover exceeds the max (design §4.6b) — the noise-scraper backstop.
    RejectTurnover {
        /// The realised per-period turnover fraction.
        turnover_frac: f64,
    },
    /// Rejected: modelled capacity below the floor (design §4.6c).
    RejectCapacity {
        /// The modelled capacity in $.
        capacity: f64,
    },
}

impl TradabilityVerdict {
    /// Whether the candidate is admissible.
    #[must_use]
    pub fn is_accept(&self) -> bool {
        matches!(self, TradabilityVerdict::Accept)
    }
}

/// The per-period target position of a tree under the trivial head: `+1` long / `−1` short / `0` flat
/// (from the directional signal). Length `= samples.len() − 1`.
fn positions(tree: &ExprTree, samples: &[Sample], states: u16) -> Vec<i8> {
    signal_series(tree, samples, states)
        .iter()
        .map(|s| {
            if *s > 0.0 {
                1
            } else if *s < 0.0 {
                -1
            } else {
                0
            }
        })
        .collect()
}

/// The realised per-period turnover fraction: fraction of periods whose target position changed.
#[must_use]
pub fn turnover_frac(tree: &ExprTree, samples: &[Sample], states: u16) -> f64 {
    let pos = positions(tree, samples, states);
    if pos.len() < 2 {
        return 0.0;
    }
    let changes = pos.windows(2).filter(|w| w[0] != w[1]).count();
    changes as f64 / (pos.len() - 1) as f64
}

/// Build the fill event stream for a tree's target-position path, for the cost-stress backtest. A fill is
/// emitted at every position change (taker, at the bar close), sized so the traded notional is
/// `cfg.fill_notional`. Reuses the immutable friction harness (QE-431); `cost_sweep` then re-prices it at
/// each multiplier.
fn build_fills(
    tree: &ExprTree,
    samples: &[Sample],
    states: u16,
    cfg: &TradabilityConfig,
) -> Vec<Event> {
    let pos = positions(tree, samples, states);
    let adv = Decimal::from_f64_retain(cfg.adv_notional).unwrap_or(Decimal::ZERO);
    let mut events = Vec::new();
    let mut current: i8 = 0;
    for (t, &target) in pos.iter().enumerate() {
        if target == current {
            continue;
        }
        let price = samples[t].bar.close().get();
        if price.is_zero() {
            continue;
        }
        let delta = i32::from(target) - i32::from(current);
        let qty = (cfg.fill_notional / price) * Decimal::from(delta.abs());
        let side = if delta > 0 { Side::Buy } else { Side::Sell };
        events.push(Event::Fill(Fill {
            side,
            qty,
            price,
            adv,
            liquidity: Liquidity::Taker,
        }));
        current = target;
    }
    // Flatten any open position at the last bar so its P&L is realised (the harness marks realised P&L on
    // closes only; without a final flatten a held winner would show 0 gross and net = −entry-cost).
    if current != 0 {
        if let Some(last) = pos.len().checked_sub(1) {
            let price = samples[last].bar.close().get();
            if !price.is_zero() {
                let qty = (cfg.fill_notional / price) * Decimal::from(current.unsigned_abs());
                let side = if current > 0 { Side::Sell } else { Side::Buy };
                events.push(Event::Fill(Fill {
                    side,
                    qty,
                    price,
                    adv,
                    liquidity: Liquidity::Taker,
                }));
            }
        }
    }
    events
}

/// The **cost-stressed net** (design §4.6a): the `min` over friction multipliers `m ∈ {1×,2×}` of the
/// re-costed net P&L of the tree's backtest, reusing the merged QE-431 `cost_sweep`. Because `decide()` is
/// cost-blind the event stream is identical across multipliers — only fees + slippage scale.
#[must_use]
pub fn cost_stressed_net(
    tree: &ExprTree,
    samples: &[Sample],
    states: u16,
    cfg: &TradabilityConfig,
) -> f64 {
    let events = build_fills(tree, samples, states, cfg);
    let base = FrictionConfig {
        slippage: crate::friction::SlippageModel::from_calibration(&cfg.calibration),
        ..FrictionConfig::default()
    };
    cost_sweep(&events, &base, &cfg.cost_multipliers)
        .into_iter()
        .map(|(_m, pnl)| pnl.net().to_f64().unwrap_or(f64::NEG_INFINITY))
        .fold(f64::INFINITY, f64::min)
}

/// The **inlined capacity** model (design §4.6c) — QE-440's concave √-in-participation law, with impact
/// coefficients **duplicated from the shared `SlippageCalibration`** (the single source of truth; the
/// coefficient-parity test guards drift). Mirrors `qe_ensemble::capacity::capacity` verbatim; kept here so
/// `qe-wfo` needs no `qe-ensemble` edge (firewall).
///
/// ```text
/// net(W) = gross_edge − turnover·half_spread − turnover·impact_coeff·(turnover·W / ADV$)^β
/// W* = (ADV$ / turnover)·[ (gross_edge − turnover·half_spread) / (turnover·impact_coeff) ]^(1/β)
/// ```
///
/// (`edge_retention = 0`, matching `capacity.rs`'s `DEFAULT_EDGE_RETENTION`.) `0.0` if the spread-cross
/// alone eats the edge; `+∞` if there is no modellable size cap.
#[must_use]
pub fn inlined_capacity(
    gross_edge: f64,
    turnover: f64,
    adv_notional: f64,
    cal: &SlippageCalibration,
) -> f64 {
    let turnover = turnover.max(0.0);
    let half_spread = cal.half_spread_f64();
    let impact_coeff = cal.impact_coeff_f64();
    let beta = cal.impact_exponent_f64();
    let usable_edge = gross_edge - turnover * half_spread;
    if usable_edge <= 0.0 {
        return 0.0;
    }
    let impact_slope = turnover * impact_coeff;
    if impact_slope <= 0.0 || !adv_notional.is_finite() || adv_notional <= 0.0 {
        return f64::INFINITY;
    }
    let participation = (usable_edge / impact_slope).powf(1.0 / beta);
    (adv_notional / turnover) * participation
}

/// Evaluate all tradability gates (design §4.6) for one candidate tree. Returns the **first** failing gate
/// (turnover → cost-stress → capacity), or `Accept`. The caller counts a reject toward `N` regardless.
#[must_use]
pub fn evaluate_tradability(
    tree: &ExprTree,
    samples: &[Sample],
    states: u16,
    cfg: &TradabilityConfig,
) -> TradabilityVerdict {
    // (b) Max-turnover REJECT — the cheapest gate + the noise-scraper backstop.
    let turnover = turnover_frac(tree, samples, states);
    if turnover > cfg.max_turnover_frac {
        return TradabilityVerdict::RejectTurnover {
            turnover_frac: turnover,
        };
    }
    // (a) Cost-stressed net finite & > 0 over m ∈ {1×,2×}.
    let min_net = cost_stressed_net(tree, samples, states, cfg);
    if !min_net.is_finite() || min_net <= 0.0 {
        return TradabilityVerdict::RejectCostStress { min_net };
    }
    // (c) Capacity floor. gross_edge = mean per-period gross return of the formula.
    let gross = formula_returns(tree, samples, states);
    let gross_edge = if gross.is_empty() {
        0.0
    } else {
        gross.iter().sum::<f64>() / gross.len() as f64
    };
    let capacity = inlined_capacity(gross_edge, turnover, cfg.adv_notional, &cfg.calibration);
    if capacity < cfg.capacity_floor {
        return TradabilityVerdict::RejectCapacity { capacity };
    }
    TradabilityVerdict::Accept
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Bar, Price, Qty, Resolution, Timestamp};
    use qe_signal::indicator::expr::{Expr, Field, WinOp};

    const MIN: i64 = 60_000;

    fn dec(n: i64) -> Decimal {
        Decimal::from(n)
    }
    fn boxed(e: Expr) -> Box<Expr> {
        Box::new(e)
    }

    // A trending series (a slow-signal formula earns a positive net; a flip-flop scraper churns cost).
    fn trending(n: usize) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let i64i = i as i64;
                let base = 100 + i64i * 2 + (i64i % 9) * 3;
                let bar = Bar::new(
                    Timestamp::from_millis(i64i * 60 * MIN),
                    Resolution::H1,
                    Price::new(dec(base)).unwrap(),
                    Price::new(dec(base + 5)).unwrap(),
                    Price::new(dec((base - 5).max(1))).unwrap(),
                    Price::new(dec(base + (i64i % 3))).unwrap(),
                    Qty::new(dec(100 + (i64i % 6))).unwrap(),
                    1 + (i % 4) as u64,
                )
                .unwrap();
                Sample::from_bar(bar)
            })
            .collect()
    }

    // A choppy, oscillating series: close alternates sharply so sign(delta(close,1)) flips almost every
    // bar (the scraper churns), while a 50-window mean stays smooth (the slow formula does not).
    fn choppy(n: usize) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let i64i = i as i64;
                let base = 100 + i64i / 4;
                let close = base + if i % 2 == 0 { 8 } else { -8 };
                let bar = Bar::new(
                    Timestamp::from_millis(i64i * 60 * MIN),
                    Resolution::H1,
                    Price::new(dec(base)).unwrap(),
                    Price::new(dec(base + 12)).unwrap(),
                    Price::new(dec((base - 12).max(1))).unwrap(),
                    Price::new(dec(close.max(1))).unwrap(),
                    Qty::new(dec(100 + (i64i % 6))).unwrap(),
                    1 + (i % 4) as u64,
                )
                .unwrap();
                Sample::from_bar(bar)
            })
            .collect()
    }

    fn slow_tree() -> ExprTree {
        // rank(mean(close,50), 100) — a slow, low-turnover trend formula.
        ExprTree::repaired(Expr::Window(
            WinOp::Rank,
            boxed(Expr::Window(
                WinOp::Mean,
                boxed(Expr::Input(Field::Close)),
                50,
            )),
            100,
        ))
    }

    fn scraper_tree() -> ExprTree {
        // rank(close, 5) — a *fast* rank on the raw oscillating close: on the choppy series the current
        // close's 5-window rank alternates high/low every bar, so the quantised position flips almost every
        // bar ⇒ turnover ≈ 1. The maximally-flip-flopping noise-scraper (design §7 risk 7). (A raw
        // `sign(delta(close,1))` is defeated by `repair` snapping the period 1 → 5, so we exercise the
        // churn directly through a valid fast root period.)
        ExprTree::repaired(Expr::Window(
            WinOp::Rank,
            boxed(Expr::Input(Field::Close)),
            5,
        ))
    }

    #[test]
    fn coefficient_parity_inlined_capacity_matches_the_shared_calibration() {
        // The inlined capacity's impact form MUST equal the shared SlippageCalibration's `cost_fraction`
        // for identical inputs — the firewall-safe duplication guard (design §6 / §4.6c). Compare the
        // participation cost fraction `half_spread + impact_coeff·u^β` at several participations.
        let cal = SlippageCalibration::default();
        for &u in &[0.0_f64, 0.01, 0.1, 0.5, 1.0] {
            let inlined =
                cal.half_spread_f64() + cal.impact_coeff_f64() * u.powf(cal.impact_exponent_f64());
            let shared = cal
                .cost_fraction(Decimal::from_f64_retain(u).unwrap())
                .to_f64()
                .unwrap();
            assert!(
                (inlined - shared).abs() < 1e-9,
                "capacity coefficient parity broke at u={u}: inlined {inlined} vs shared {shared}"
            );
        }
    }

    #[test]
    fn turnover_gate_rejects_the_flip_flop_scraper() {
        let s = choppy(400);
        let scraper = scraper_tree();
        let tf = turnover_frac(&scraper, &s, 5);
        assert!(
            tf > MAX_TURNOVER_FRAC,
            "the sign(delta(close,1)) scraper must exceed the turnover cap, got {tf}"
        );
        assert!(matches!(
            evaluate_tradability(&scraper, &s, 5, &TradabilityConfig::default()),
            TradabilityVerdict::RejectTurnover { .. }
        ));
        // The slow (50-window mean) formula stays well under the cap even on the choppy series.
        assert!(turnover_frac(&slow_tree(), &s, 5) <= MAX_TURNOVER_FRAC);
    }

    #[test]
    fn cost_stress_is_monotone_and_rejects_a_churning_zero_edge_formula() {
        // The churning scraper on the choppy series realises ≈ zero gross edge but pays cost on every flip.
        // (a) The cost-stressed net is MONOTONE in the multiplier band: min over {1×,K×} ≤ the 1× net.
        // (b) Under a crushing multiplier the net goes negative ⇒ the cost-stress gate would reject.
        let s = choppy(400);
        let scraper = scraper_tree();
        let cfg_1x = TradabilityConfig {
            cost_multipliers: vec![Decimal::ONE],
            ..TradabilityConfig::default()
        };
        let net_1x = cost_stressed_net(&scraper, &s, 5, &cfg_1x);
        let cfg_stressed = TradabilityConfig {
            cost_multipliers: vec![Decimal::ONE, Decimal::from(1000)],
            ..TradabilityConfig::default()
        };
        let net_min = cost_stressed_net(&scraper, &s, 5, &cfg_stressed);
        assert!(net_min.is_finite());
        assert!(
            net_min <= net_1x,
            "min over the multiplier band must not exceed the 1× net: {net_min} !<= {net_1x}"
        );
        assert!(
            net_min < 0.0,
            "a churning zero-edge formula under crushing cost must go net-negative, got {net_min}"
        );
        // Directly through the gate (isolating cost-stress by lifting the turnover cap to 1.0).
        let gate_cfg = TradabilityConfig {
            cost_multipliers: vec![Decimal::ONE, Decimal::from(1000)],
            max_turnover_frac: 1.0,
            ..TradabilityConfig::default()
        };
        assert!(matches!(
            evaluate_tradability(&scraper, &s, 5, &gate_cfg),
            TradabilityVerdict::RejectCostStress { .. }
        ));
    }

    #[test]
    fn capacity_gate_rejects_a_thin_capacity_formula() {
        // With a tiny ADV the modelled capacity falls below the $250k floor even for a positive-net,
        // low-turnover formula. Capacity scales linearly with ADV, so a small ADV ⇒ small capacity.
        let s = trending(400);
        // Turnover (slow tree) and cost-stress pass (small notional vs the large default ADV ⇒ tiny cost,
        // positive net), but an unreachably-high floor forces the capacity reject deterministically —
        // isolating the capacity gate from the other two.
        let cfg = TradabilityConfig {
            fill_notional: Decimal::from(100),
            capacity_floor: 1e30,
            ..TradabilityConfig::default()
        };
        let verdict = evaluate_tradability(&slow_tree(), &s, 5, &cfg);
        assert!(
            matches!(verdict, TradabilityVerdict::RejectCapacity { .. }),
            "sub-floor capacity must reject, got {verdict:?}"
        );
    }

    #[test]
    fn inlined_capacity_matches_the_documented_law() {
        // Spot-check the closed form against a hand computation (design §4.6c). gross_edge 0.001/period,
        // turnover 0.1, ADV $1e7, half_spread 1e-4, impact_coeff 1e-2, β 0.5:
        //   usable = 0.001 − 0.1·1e-4 = 0.00099; slope = 0.1·1e-2 = 1e-3;
        //   u = (0.00099/1e-3)^(1/0.5) = 0.99^2 = 0.9801; W* = (1e7/0.1)·0.9801 = 9.801e7.
        let cal = SlippageCalibration::default();
        let w = inlined_capacity(0.001, 0.1, 1e7, &cal);
        assert!(
            (w - 9.801e7).abs() / 9.801e7 < 1e-6,
            "capacity law mismatch: {w}"
        );
        // Spread-cross alone eats the edge ⇒ 0 capacity.
        assert_eq!(inlined_capacity(0.0, 0.1, 1e7, &cal), 0.0);
        // Zero turnover ⇒ no modellable size cap ⇒ +∞.
        assert!(inlined_capacity(0.001, 0.0, 1e7, &cal).is_infinite());
    }

    #[test]
    fn accept_when_all_gates_pass() {
        let s = trending(400);
        let cfg = TradabilityConfig::default();
        // The slow trend formula on a strongly-trending series: low turnover, positive cost-stressed net,
        // ample capacity (large default ADV).
        let verdict = evaluate_tradability(&slow_tree(), &s, 5, &cfg);
        assert_eq!(verdict, TradabilityVerdict::Accept, "got {verdict:?}");
    }

    #[test]
    fn ic_screen_reuses_the_merged_screen_and_classifies() {
        // Two trees screened together; the screen runs and returns a verdict per tree (the point is the
        // reuse + shape, not a specific admit — every screened tree counts toward N regardless).
        let s = trending(300);
        let net_returns = formula_returns(&slow_tree(), &s, 5);
        let labelled = vec![
            ("slow".to_string(), slow_tree()),
            ("scraper".to_string(), scraper_tree()),
        ];
        // Two disjoint out-of-fold index sets over the warm span.
        let fold_a: Vec<usize> = (120..200).collect();
        let fold_b: Vec<usize> = (200..280).collect();
        let report = ic_screen_trees(
            &labelled,
            &s,
            5,
            &net_returns,
            &fold_a,
            &fold_b,
            &IcScreenConfig::default(),
        );
        assert_eq!(report.indicators.len(), 2, "one screen per tree");
        // The screen produced a defined verdict for each (Admit/Flag/Drop) — filters compute, not N.
        assert!(report.indicators.iter().all(|i| matches!(
            i.verdict,
            qe_validation::Verdict::Admit
                | qe_validation::Verdict::Flag
                | qe_validation::Verdict::Drop
        )));
    }
}
