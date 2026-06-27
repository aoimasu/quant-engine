//! QD / MAP-Elites archive behaviour descriptors (QE-111) — the structural niche a genome occupies.
//!
//! **All descriptor axes are genotype-derived (decision QE-111/D1).** Each axis is a pure function of
//! the [`Genome`] genes plus the *static* catalogue (ids + lookbacks) — never of the evaluation
//! window's outcomes. That is what keeps a genome's niche **stable across walk-forward windows**: see
//! [`cell_reassignment_rate`] and `STABILITY_THRESHOLD`. The three axes are:
//!
//! 1. [`IndicatorFamily`] — the dominant family of the direction-bank's enabled clauses;
//! 2. [`TimescaleBand`] — the discretised max lookback among referenced features;
//! 3. [`HoldingBand`] — the discretised `exit.max_holding_bars`.
//!
//! Their product is the MAP-Elites grid ([`grid_cells`], [`CELLS_PER_DIRECTION`]); per-direction
//! archives (D4) keep short niches first-class so the ensemble (QE-126) is not net-long by
//! construction. This module defines the descriptors, resolution, sub-population size, and the
//! stability metric — **not** insertion / elite replacement / fitness (those are QE-118 / QE-120).

use qe_domain::Direction;
use qe_signal::FeatureSchema;

use crate::genome::{Genome, RuleSet};

/// Deep-Grid sub-population size — elites held per cell (Flageat & Cully 2020). Larger than 1 so a
/// noisy single evaluation cannot evict a genome; small so the archive stays compact (QE-111/D3).
pub const SUBPOP_SIZE: usize = 8;

/// Maximum tolerated cell-reassignment rate across re-evaluation windows (QE-111/D5). Genotype-derived
/// descriptors achieve exactly `0.0`; this is the budget any future outcome-derived axis must respect.
pub const STABILITY_THRESHOLD: f64 = 0.05;

/// The family of signal an indicator belongs to — the first (categorical) archive axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IndicatorFamily {
    /// Moving-average / MACD-style trend factors.
    Trend,
    /// Oscillators / rate-of-change momentum factors.
    Momentum,
    /// Volatility / dispersion factors.
    Volatility,
    /// Volume / flow-of-volume factors.
    Volume,
    /// Funding / open-interest / premium (perp microstructure) factors.
    Flow,
}

/// All families in fixed order — also the deterministic tie-break order for the dominant family.
pub const FAMILIES: [IndicatorFamily; 5] = [
    IndicatorFamily::Trend,
    IndicatorFamily::Momentum,
    IndicatorFamily::Volatility,
    IndicatorFamily::Volume,
    IndicatorFamily::Flow,
];

impl IndicatorFamily {
    /// This family's position in [`FAMILIES`] (also its archive-axis index). Total — every variant
    /// maps to a slot, so callers need no fallback.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            IndicatorFamily::Trend => 0,
            IndicatorFamily::Momentum => 1,
            IndicatorFamily::Volatility => 2,
            IndicatorFamily::Volume => 3,
            IndicatorFamily::Flow => 4,
        }
    }
}

/// Classify a catalogue indicator id (QE-107) into its [`IndicatorFamily`]. Returns `None` for an
/// unrecognised id — test `family_classifier_covers_catalogue` fails loudly if a catalogue indicator
/// is left unclassified.
#[must_use]
pub fn family_of(id: &str) -> Option<IndicatorFamily> {
    // Trend: moving averages and MACD.
    if id.starts_with("sma_") || id.starts_with("ema_") || id.starts_with("macd_") {
        return Some(IndicatorFamily::Trend);
    }
    // Volatility: Bollinger, ATR, return dispersion.
    if id.starts_with("bb_") || id.starts_with("atr_") || id.starts_with("std_returns") {
        return Some(IndicatorFamily::Volatility);
    }
    // Volume: volume ratios and money-flow.
    if id.starts_with("volume_ratio") || id.starts_with("signed_volume") || id.starts_with("cmf") {
        return Some(IndicatorFamily::Volume);
    }
    // Flow: perp microstructure (funding / OI / premium).
    if id.starts_with("funding_") || id.starts_with("oi_") || id.starts_with("premium_") {
        return Some(IndicatorFamily::Flow);
    }
    // Momentum: oscillators and rate-of-change (rsi, stoch, williams_r, roc, return, cci, mfi, aroon).
    if id.starts_with("rsi")
        || id.starts_with("stoch")
        || id.starts_with("williams_r")
        || id.starts_with("roc")
        || id.starts_with("return_")
        || id.starts_with("cci")
        || id.starts_with("mfi")
        || id.starts_with("aroon")
    {
        return Some(IndicatorFamily::Momentum);
    }
    None
}

/// Reaction-speed band — the discretised max lookback among a genome's referenced features (in bars).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TimescaleBand {
    /// Short lookback (`≤ FAST_MAX_LOOKBACK` bars).
    Fast,
    /// Medium lookback.
    Medium,
    /// Long lookback (`> MEDIUM_MAX_LOOKBACK` bars).
    Slow,
}

/// Upper lookback (bars) for the `Fast` band.
pub const FAST_MAX_LOOKBACK: usize = 14;
/// Upper lookback (bars) for the `Medium` band.
pub const MEDIUM_MAX_LOOKBACK: usize = 28;

/// All timescale bands in order.
pub const TIMESCALES: [TimescaleBand; 3] = [
    TimescaleBand::Fast,
    TimescaleBand::Medium,
    TimescaleBand::Slow,
];

impl TimescaleBand {
    /// Band for a max lookback (bars). Cutoffs are seeded from the 5m base grid / catalogue spread and
    /// are config-ready (QE-111/D2).
    #[must_use]
    pub fn from_lookback(lookback: usize) -> Self {
        if lookback <= FAST_MAX_LOOKBACK {
            TimescaleBand::Fast
        } else if lookback <= MEDIUM_MAX_LOOKBACK {
            TimescaleBand::Medium
        } else {
            TimescaleBand::Slow
        }
    }
}

/// Holding-horizon band — the discretised `exit.max_holding_bars`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HoldingBand {
    /// Very short holds (`≤ SCALP_MAX_BARS` bars).
    Scalp,
    /// Intraday-to-swing holds.
    Swing,
    /// Long holds (`> SWING_MAX_BARS` bars).
    Position,
}

/// Upper holding (bars) for the `Scalp` band (6 × 5m ≈ 30 min).
pub const SCALP_MAX_BARS: u16 = 6;
/// Upper holding (bars) for the `Swing` band (48 × 5m ≈ 4 h).
pub const SWING_MAX_BARS: u16 = 48;

/// All holding bands in order.
pub const HOLDINGS: [HoldingBand; 3] = [
    HoldingBand::Scalp,
    HoldingBand::Swing,
    HoldingBand::Position,
];

impl HoldingBand {
    /// Band for a max-holding cap (bars).
    #[must_use]
    pub fn from_max_holding(bars: u16) -> Self {
        if bars <= SCALP_MAX_BARS {
            HoldingBand::Scalp
        } else if bars <= SWING_MAX_BARS {
            HoldingBand::Swing
        } else {
            HoldingBand::Position
        }
    }
}

/// A MAP-Elites grid coordinate: the structural niche a genome occupies in one direction's archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cell {
    /// Dominant indicator family.
    pub family: IndicatorFamily,
    /// Reaction-speed band.
    pub timescale: TimescaleBand,
    /// Holding-horizon band.
    pub holding: HoldingBand,
}

/// Number of grid cells per direction = `|families| × |timescales| × |holdings|`.
pub const CELLS_PER_DIRECTION: usize = FAMILIES.len() * TIMESCALES.len() * HOLDINGS.len();

/// Enumerate every grid cell (the archive resolution), in a fixed deterministic order.
pub fn grid_cells() -> impl Iterator<Item = Cell> {
    FAMILIES.into_iter().flat_map(|family| {
        TIMESCALES.into_iter().flat_map(move |timescale| {
            HOLDINGS.into_iter().map(move |holding| Cell {
                family,
                timescale,
                holding,
            })
        })
    })
}

/// The descriptor [`Cell`] for `genome` in `direction`, computed from **that direction's** entry bank
/// (QE-111/D4). `None` if the bank has no enabled clause referencing a classifiable catalogue feature
/// — such a genome does not occupy this direction's archive.
///
/// Dominant family = the most-referenced family among enabled clauses (ties broken by [`FAMILIES`]
/// order). Timescale = band of the max lookback among referenced features. Holding = band of
/// `exit.max_holding_bars`. All inputs are genotype + static schema, so the result is window-invariant.
#[must_use]
pub fn descriptor_for(
    genome: &Genome,
    direction: Direction,
    schema: &FeatureSchema,
) -> Option<Cell> {
    let bank: &RuleSet = match direction {
        Direction::Long => &genome.long_entry,
        Direction::Short => &genome.short_entry,
    };

    let ids = schema.ids();
    let lookbacks = schema.lookbacks();

    // Per-family reference counts and the max referenced lookback.
    let mut family_counts = [0usize; FAMILIES.len()];
    let mut max_lookback: Option<usize> = None;
    for clause in bank.clauses.iter().filter(|c| c.enabled) {
        let idx = clause.feature as usize;
        let Some(id) = ids.get(idx) else { continue };
        let Some(family) = family_of(id) else {
            continue;
        };
        family_counts[family.index()] += 1;
        let lb = lookbacks.get(idx).copied().unwrap_or(0);
        max_lookback = Some(max_lookback.map_or(lb, |m| m.max(lb)));
    }

    // Dominant family = argmax of counts; FAMILIES order is the deterministic tie-break (first wins).
    let (dominant_pos, &best) = family_counts
        .iter()
        .enumerate()
        .max_by_key(|(pos, &count)| (count, std::cmp::Reverse(*pos)))?;
    if best == 0 {
        return None; // no classifiable enabled clause
    }

    Some(Cell {
        family: FAMILIES[dominant_pos],
        timescale: TimescaleBand::from_lookback(max_lookback.unwrap_or(0)),
        holding: HoldingBand::from_max_holding(genome.exit.max_holding_bars),
    })
}

/// Cell-reassignment rate (QE-111/D5): the fraction of genomes whose assigned [`Cell`] differs between
/// two evaluations `a` and `b` (e.g. the same genomes re-assigned on a different walk-forward window).
///
/// Genomes unassigned in **both** are excluded from the denominator; an assigned↔unassigned flip
/// counts as a reassignment. Returns `0.0` for an empty / all-unassigned input. For genotype-derived
/// descriptors `a == b` always, so the rate is `0.0 ≤ STABILITY_THRESHOLD`.
#[must_use]
pub fn cell_reassignment_rate(a: &[Option<Cell>], b: &[Option<Cell>]) -> f64 {
    // The two evaluations must align one genome per slot; mismatched lengths would silently
    // `zip`-truncate and under-count. Callers (QE-118) must pass parallel assignments.
    debug_assert_eq!(
        a.len(),
        b.len(),
        "cell_reassignment_rate: assignment slices must be the same length"
    );
    let mut considered = 0usize;
    let mut reassigned = 0usize;
    for (x, y) in a.iter().zip(b.iter()) {
        if x.is_none() && y.is_none() {
            continue; // never placed in either — not part of the population
        }
        considered += 1;
        if x != y {
            reassigned += 1;
        }
    }
    if considered == 0 {
        return 0.0;
    }
    reassigned as f64 / considered as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genome::{Clause, ExitParams, RiskParams, RuleSet, REP_VERSION};
    use qe_signal::{CatalogueConfig, FeatureSchema};

    fn schema() -> FeatureSchema {
        FeatureSchema::from_catalogue(&CatalogueConfig { states: 5 })
    }

    fn clause(enabled: bool, feature: u16) -> Clause {
        Clause {
            enabled,
            feature,
            lo: 0,
            hi: 1,
        }
    }

    fn disabled() -> Clause {
        clause(false, 0)
    }

    /// Build a genome whose long bank references `long_feats` and short bank `short_feats`, with the
    /// given holding cap. Up to 4 features per bank (padded with disabled clauses).
    fn genome_with(long_feats: &[u16], short_feats: &[u16], max_holding_bars: u16) -> Genome {
        let bank = |feats: &[u16]| {
            let mut clauses = [disabled(), disabled(), disabled(), disabled()];
            for (slot, &f) in clauses.iter_mut().zip(feats.iter()) {
                *slot = clause(true, f);
            }
            RuleSet {
                clauses,
                min_satisfied: 1,
            }
        };
        Genome {
            version: REP_VERSION,
            long_entry: bank(long_feats),
            short_entry: bank(short_feats),
            exit: ExitParams {
                max_holding_bars,
                exit_on_opposite: true,
            },
            risk: RiskParams { size_bps: 5_000 },
        }
    }

    /// Index of a catalogue id (panics in-test if absent — guards the fixture against catalogue drift).
    fn idx_of(schema: &FeatureSchema, id: &str) -> u16 {
        schema
            .ids()
            .iter()
            .position(|s| s == id)
            .map(|p| p as u16)
            .unwrap_or_else(|| panic!("indicator {id} not in catalogue"))
    }

    #[test]
    fn family_classifier_covers_catalogue() {
        let s = schema();
        for id in s.ids() {
            assert!(family_of(id).is_some(), "unclassified indicator id: {id}");
        }
        // Spot-check a representative of each family.
        assert_eq!(family_of("ema_ratio_20"), Some(IndicatorFamily::Trend));
        assert_eq!(family_of("rsi_14"), Some(IndicatorFamily::Momentum));
        assert_eq!(family_of("atr_pct_14"), Some(IndicatorFamily::Volatility));
        assert_eq!(family_of("cmf_20"), Some(IndicatorFamily::Volume));
        assert_eq!(family_of("funding_state"), Some(IndicatorFamily::Flow));
        assert_eq!(family_of("not_an_indicator"), None);
    }

    #[test]
    fn descriptor_is_dominant_family_max_lookback_and_holding_band() {
        let s = schema();
        // Long bank: two Momentum (rsi_14 lb15, cci_20 lb20) + one Trend (ema_ratio_20 lb20).
        // Dominant family = Momentum (2 vs 1); max referenced lookback = 20 → Medium; holding 10 → Swing.
        let g = genome_with(
            &[
                idx_of(&s, "rsi_14"),
                idx_of(&s, "cci_20"),
                idx_of(&s, "ema_ratio_20"),
            ],
            &[idx_of(&s, "funding_state")],
            10,
        );
        assert_eq!(
            descriptor_for(&g, Direction::Long, &s),
            Some(Cell {
                family: IndicatorFamily::Momentum,
                timescale: TimescaleBand::Medium,
                holding: HoldingBand::Swing,
            })
        );
        // Short bank references only funding_state (Flow, lookback 1 → Fast).
        assert_eq!(
            descriptor_for(&g, Direction::Short, &s),
            Some(Cell {
                family: IndicatorFamily::Flow,
                timescale: TimescaleBand::Fast,
                holding: HoldingBand::Swing,
            })
        );
    }

    #[test]
    fn disabled_clauses_are_ignored_and_empty_bank_is_unassigned() {
        let s = schema();
        // Long bank has only disabled clauses → None for Long; short has one Trend → Some for Short.
        let mut g = genome_with(&[idx_of(&s, "rsi_14")], &[idx_of(&s, "sma_ratio_20")], 3);
        g.long_entry.clauses = [disabled(), disabled(), disabled(), disabled()];
        assert_eq!(descriptor_for(&g, Direction::Long, &s), None);
        assert_eq!(
            descriptor_for(&g, Direction::Short, &s).map(|c| c.family),
            Some(IndicatorFamily::Trend)
        );
    }

    #[test]
    fn timescale_and_holding_band_cutoffs() {
        assert_eq!(TimescaleBand::from_lookback(14), TimescaleBand::Fast);
        assert_eq!(TimescaleBand::from_lookback(15), TimescaleBand::Medium);
        assert_eq!(TimescaleBand::from_lookback(28), TimescaleBand::Medium);
        assert_eq!(TimescaleBand::from_lookback(29), TimescaleBand::Slow);
        assert_eq!(HoldingBand::from_max_holding(6), HoldingBand::Scalp);
        assert_eq!(HoldingBand::from_max_holding(7), HoldingBand::Swing);
        assert_eq!(HoldingBand::from_max_holding(48), HoldingBand::Swing);
        assert_eq!(HoldingBand::from_max_holding(49), HoldingBand::Position);
    }

    #[test]
    fn grid_enumerates_exactly_45_unique_cells() {
        let cells: Vec<Cell> = grid_cells().collect();
        assert_eq!(cells.len(), CELLS_PER_DIRECTION);
        assert_eq!(CELLS_PER_DIRECTION, 45);
        let mut uniq = cells.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), cells.len(), "grid cells must be unique");
    }

    // --- descriptor stability (the AC) -------------------------------------------------------

    /// A spread of genomes touching several families/timescales/holdings.
    fn population(s: &FeatureSchema) -> Vec<Genome> {
        vec![
            genome_with(&[idx_of(s, "rsi_14")], &[idx_of(s, "funding_state")], 3),
            genome_with(&[idx_of(s, "ema_ratio_20")], &[idx_of(s, "cmf_20")], 30),
            genome_with(
                &[idx_of(s, "macd_hist_12_26_9")],
                &[idx_of(s, "atr_pct_14")],
                60,
            ),
            genome_with(&[idx_of(s, "oi_roc_10")], &[idx_of(s, "stoch_k_14")], 12),
        ]
    }

    #[test]
    fn genotype_derived_descriptors_are_window_stable() {
        let s = schema();
        let pop = population(&s);
        // "Window A" and "window B" are two *independent* re-derivations (descriptor_for reads no
        // window data, so a different window only differs in the schema instance, which we rebuild).
        let schema_a = schema();
        let schema_b = schema();
        let assign_a: Vec<Option<Cell>> = pop
            .iter()
            .map(|g| descriptor_for(g, Direction::Long, &schema_a))
            .collect();
        let assign_b: Vec<Option<Cell>> = pop
            .iter()
            .map(|g| descriptor_for(g, Direction::Long, &schema_b))
            .collect();
        let rate = cell_reassignment_rate(&assign_a, &assign_b);
        assert_eq!(rate, 0.0);
        assert!(rate <= STABILITY_THRESHOLD);
        // Every genome here trades long → all assigned.
        assert!(assign_a.iter().all(Option::is_some));
    }

    #[test]
    fn reassignment_metric_detects_instability() {
        let trend_cell = Cell {
            family: IndicatorFamily::Trend,
            timescale: TimescaleBand::Fast,
            holding: HoldingBand::Scalp,
        };
        let flow_cell = Cell {
            family: IndicatorFamily::Flow,
            timescale: TimescaleBand::Slow,
            holding: HoldingBand::Position,
        };
        let a = [Some(trend_cell), Some(flow_cell), None, Some(trend_cell)];
        // genome 1 moved cell; genome 2 same; genome 3 None↔None excluded; genome 4 assigned↔None flip.
        let b = [Some(flow_cell), Some(flow_cell), None, None];
        // considered = 3 (idx 0,1,3); reassigned = 2 (idx 0 and 3) → 2/3.
        let rate = cell_reassignment_rate(&a, &b);
        assert!((rate - 2.0 / 3.0).abs() < 1e-12);
        assert!(rate > STABILITY_THRESHOLD);
        // Empty / all-None inputs → 0.0.
        assert_eq!(cell_reassignment_rate(&[], &[]), 0.0);
        assert_eq!(cell_reassignment_rate(&[None], &[None]), 0.0);
    }
}
