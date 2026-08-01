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

/// Round a **funding** sample timestamp to the nearest whole hour (QE-495). Real Binance `fundingTime`
/// stamps carry millisecond jitter — e.g. `…08:00:00.001`, measured on 151 of the 1098 BTCUSDT stamps
/// in 2024 — so an exact-ms equality join silently drops ~15% of genuine funding and fails the 90%
/// coverage gate on every real-data train. Bars open exactly on the hour and the funding grid is 8h,
/// so nearest-hour rounding heals the jitter with no risk of cross-stamp collision (the ±30min
/// tolerance is far below half the 8h period). Applied to **funding only** — premium klines are
/// dense, exactly on the bar grid, and carry no jitter, so rounding them would over-snap sub-hour
/// samples onto the wrong bar (QE-496 review R3.1). Pure integer arithmetic — deterministic (QE-006).
fn round_to_hour_ms(ms: i64) -> i64 {
    const HOUR_MS: i64 = 3_600_000;
    (ms + HOUR_MS / 2).div_euclid(HOUR_MS) * HOUR_MS
}

/// Build the decision-bar series for one instrument: assemble feature vectors over the OHLCV bars
/// (with funding / premium scalar context aligned by bar time) and zip each with its bar `close`
/// price and funding rate.
///
/// **Funding** samples are matched to a bar after nearest-hour rounding of the sparse, jittery venue
/// stamp (QE-495). **Premium** samples are joined by **exact** open-time equality — they are dense,
/// on-grid kline data with no jitter, so the exact join is already correct and rounding them would
/// mis-attach on sub-hour bars (review R3.1). A bar with no matching stamp carries `funding_rate =
/// None`. The returned vector is aligned one-to-one with `bars`.
#[must_use]
pub fn to_decision_bars(
    bars: &[OhlcvBar],
    funding: &[FundingRateSample],
    premium: &[PremiumSample],
) -> Vec<DecisionBar> {
    let funding_by_ms: BTreeMap<i64, Decimal> = funding
        .iter()
        .map(|f| (round_to_hour_ms(f.time.millis()), f.rate.get()))
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
                volume: b.volume().get(),
                funding_rate: funding_by_ms.get(&ms).copied(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_signal::genome::{Clause, ExitParams, RiskParams, RuleSet, CLAUSES_PER_SET};

    #[test]
    fn to_decision_bars_recovers_jittered_funding_and_leaves_a_genuine_gap_unfunded() {
        // QE-495 review R6.2/R5.1: prove the fix end-to-end at the join, not just round_to_hour_ms in
        // isolation. A funding stamp 1ms past the hour must land on its on-hour bar (recovered), and a
        // bar with NO stamp anywhere near it must stay `funding_rate = None` — so a genuine funding gap
        // still fails the coverage gate; nearest-hour rounding only heals ms jitter, it cannot bridge a
        // real gap.
        use qe_domain::{Bar, FundingRate, InstrumentId, Price, Qty, Resolution, Timestamp};
        const H: i64 = 3_600_000;
        let inst = InstrumentId::new("BTCUSDT").unwrap();
        let px = |v: i64| Price::new(Decimal::from(v)).unwrap();
        let bars: Vec<Bar> = (0..24)
            .map(|i| {
                Bar::new(
                    Timestamp::from_millis(i * H),
                    Resolution::H1,
                    px(100),
                    px(101),
                    px(99),
                    px(100),
                    Qty::new(Decimal::from(10)).unwrap(),
                    1,
                )
                .unwrap()
            })
            .collect();
        // One jittered stamp on the hour-8 bar (…08:00:00.001); NO stamp near hour 16.
        let funding = vec![FundingRateSample {
            instrument: inst,
            time: Timestamp::from_millis(8 * H + 1),
            rate: FundingRate::new(Decimal::new(1, 4)),
        }];
        let db = to_decision_bars(&bars, &funding, &[]);
        assert_eq!(db.len(), 24);
        assert!(
            db[8].funding_rate.is_some(),
            "a stamp 1ms past the hour must be recovered onto its on-hour bar"
        );
        assert!(
            db[16].funding_rate.is_none(),
            "a bar with no nearby stamp must stay unfunded — rounding heals jitter, never bridges a gap"
        );
        // exactly one bar funded ⇒ rounding cannot inflate coverage beyond the real stamp count
        assert_eq!(db.iter().filter(|b| b.funding_rate.is_some()).count(), 1);
    }

    #[test]
    fn jittered_funding_stamps_round_onto_the_bar_grid() {
        // QE-495: real Binance fundingTime stamps carry ±ms jitter (…08:00:00.001). Nearest-hour
        // rounding must heal small jitter in both directions, be identity on exact stamps, and never
        // move a stamp across the 8h grid.
        const H: i64 = 3_600_000;
        assert_eq!(round_to_hour_ms(8 * H), 8 * H); // exact ⇒ identity
        assert_eq!(round_to_hour_ms(8 * H + 1), 8 * H); // +1ms jitter ⇒ snapped back
        assert_eq!(round_to_hour_ms(8 * H - 3), 8 * H); // −3ms jitter ⇒ snapped forward
        assert_eq!(round_to_hour_ms(8 * H + 17), 8 * H); // +17ms observed-class jitter
        assert_eq!(round_to_hour_ms(16 * H - 1), 16 * H); // adjacent grid point unaffected
                                                          // A stamp a full half-hour off (not jitter) rounds to its NEAREST hour, never a different
                                                          // 8h stamp: 8h+29:59.999 → 8h; 8h+30:00 → 9h (a no-bar hour ⇒ simply unmatched, not wrong).
        assert_eq!(round_to_hour_ms(8 * H + 30 * 60_000 - 1), 8 * H);
        assert_eq!(round_to_hour_ms(8 * H + 30 * 60_000), 9 * H);
    }

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
