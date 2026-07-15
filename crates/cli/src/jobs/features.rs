//! The OHLCV → decision-bar bridge (QE-251 Task 5a).
//!
//! `MarketStore::scan_bars` yields raw `qe_domain::Bar` (OHLCV); the backtester consumes
//! `qe_wfo::backtest::Bar` (a quantised `FeatureVector` + reference price + funding). Between them sits
//! the mandatory feature-engineering step: OHLCV (+ funding / premium scalar factors) → `Sample`s →
//! `qe_signal::feature::assemble_batch(catalogue_cfg, samples)` → `FeatureVector`s → zipped with the
//! bar `close` price and funding into decision bars.
//!
//! **Schema sourcing.** The schema is built from [`CatalogueConfig::default`] (the canonical catalogue at
//! the current `CATALOGUE_VERSION`) — the same schema training evolves against. As of QE-402 the vintage
//! *also* **persists** the identity of that catalogue (`CATALOGUE_VERSION`, `num_states`, and an ordered
//! indicator-id hash) inside `VintageContent.catalogue`.
//!
//! **Two complementary guards.**
//! - **Exact identity match (QE-402)** — `VintageRepository::load` asserts, via
//!   `qe_vintage::schema::assert_schema`, that the vintage's pinned [`qe_signal::CatalogueIdentity`]
//!   equals this build's exactly. This catches *identity* drift that keeps the same width and
//!   `num_states` — a catalogue **reorder** (clause indices silently mean a different indicator) or a
//!   same-width `CATALOGUE_VERSION` bump — which the bounds check alone cannot. Both the CLI backtest and
//!   the live runtime load through that boundary, so they fail closed.
//! - **Bounds check** — [`check_schema`] below still runs [`Genome::is_valid`] (feature index
//!   `< schema.len()`, state `< num_states`), yielding [`RunError::SchemaMismatch`] on **out-of-range**
//!   drift. It is retained as a belt-and-braces structural check after the exact identity match.

use std::collections::BTreeMap;

use qe_domain::{Bar as OhlcvBar, FundingRateSample};
use qe_signal::{assemble_batch, CatalogueConfig, FeatureSchema, Genome, Sample};
use qe_storage::PremiumSample;
use qe_wfo::backtest::Bar as DecisionBar;
use rust_decimal::Decimal;

use super::RunError;

/// The canonical catalogue config the schema and feature assembly are built against. The vintage does
/// not persist an alternative, so this is the single source of truth (see the module docs).
#[must_use]
pub fn catalogue_config() -> CatalogueConfig {
    CatalogueConfig::default()
}

/// The feature schema the genomes are addressed against.
#[must_use]
pub fn catalogue_schema() -> FeatureSchema {
    FeatureSchema::from_catalogue(&catalogue_config())
}

/// Assert every chromosome is valid against `schema` — the strongest catalogue-compatibility check the
/// persisted vintage allows (feature indices in range, state bounds in range).
///
/// # Errors
/// [`RunError::SchemaMismatch`] on the first invalid chromosome.
pub fn check_schema(chromosomes: &[Genome], schema: &FeatureSchema) -> Result<(), RunError> {
    for (index, g) in chromosomes.iter().enumerate() {
        if !g.is_valid(schema) {
            return Err(RunError::SchemaMismatch {
                index,
                schema_len: schema.len(),
                num_states: schema.num_states(),
            });
        }
    }
    Ok(())
}

/// Build the decision-bar series for one instrument: assemble feature vectors over the OHLCV bars
/// (with funding / premium scalar context aligned by exact bar time) and zip each with its bar `close`
/// price and funding rate.
///
/// Funding and premium samples are matched to a bar by an **exact** open-time equality (funding stamps
/// land on a sparse grid; a bar with no stamp carries `funding_rate = None`). The returned vector is
/// aligned one-to-one with `bars`.
#[must_use]
pub fn to_decision_bars(
    bars: &[OhlcvBar],
    funding: &[FundingRateSample],
    premium: &[PremiumSample],
) -> Vec<DecisionBar> {
    let funding_by_ms: BTreeMap<i64, Decimal> = funding
        .iter()
        .map(|f| (f.time.millis(), f.rate.get()))
        .collect();
    let premium_by_ms: BTreeMap<i64, Decimal> = premium
        .iter()
        .map(|p| (p.time.millis(), p.premium))
        .collect();

    let samples: Vec<Sample> = bars
        .iter()
        .map(|b| {
            let ms = b.open_time().millis();
            Sample {
                bar: b.clone(),
                funding: funding_by_ms.get(&ms).copied(),
                open_interest: None,
                premium: premium_by_ms.get(&ms).copied(),
            }
        })
        .collect();

    let features = assemble_batch(&catalogue_config(), &samples);

    features
        .into_iter()
        .zip(bars.iter())
        .map(|(fv, b)| {
            let ms = b.open_time().millis();
            DecisionBar {
                features: fv,
                price: b.close().get(),
                funding_rate: funding_by_ms.get(&ms).copied(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_signal::genome::{Clause, ExitParams, RiskParams, RuleSet, CLAUSES_PER_SET};

    fn clause(feature: u16, lo: u16, hi: u16) -> Clause {
        Clause {
            enabled: true,
            feature,
            lo,
            hi,
        }
    }

    fn disabled() -> Clause {
        Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        }
    }

    fn ruleset(c0: Clause) -> RuleSet {
        RuleSet {
            clauses: [c0, disabled(), disabled(), disabled()],
            min_satisfied: 1,
        }
    }

    fn genome(feature: u16, hi: u16) -> Genome {
        Genome {
            version: qe_signal::genome::REP_VERSION,
            long_entry: ruleset(clause(feature, 0, hi)),
            short_entry: ruleset(clause(feature, 0, hi)),
            exit: ExitParams {
                max_holding_bars: 3,
                exit_on_opposite: true,
            },
            risk: RiskParams { size_bps: 5_000 },
        }
    }

    #[test]
    fn valid_genome_passes_schema_check() {
        let schema = catalogue_schema();
        assert!(!schema.is_empty(), "catalogue must be non-empty");
        // feature 0, states within range.
        let g = genome(0, schema.num_states() - 1);
        check_schema(&[g], &schema).unwrap();
    }

    #[test]
    fn out_of_range_feature_is_schema_mismatch() {
        let schema = catalogue_schema();
        let bad_feature = schema.len() as u16; // one past the end
        let g = genome(bad_feature, 0);
        let err = check_schema(&[g], &schema).unwrap_err();
        assert!(matches!(err, RunError::SchemaMismatch { index: 0, .. }));
    }

    #[test]
    fn out_of_range_state_is_schema_mismatch() {
        let schema = catalogue_schema();
        let bad_state = schema.num_states(); // one past the max valid state
        let g = genome(0, bad_state);
        let err = check_schema(&[g], &schema).unwrap_err();
        assert!(matches!(err, RunError::SchemaMismatch { .. }));
        let _ = CLAUSES_PER_SET; // silence unused import in some configs
    }
}
