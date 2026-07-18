//! Phased recording → strategy repository (QE-123).
//!
//! The recording stage of the search: only **exploitation-phase survivors** above the cohort quality
//! threshold are persisted, each carrying a resolvable [`Lineage`] (QE-006). Admission is the QE-114
//! [`QualityGate`] — an *early lucky* candidate (evaluated on too few windows ⇒ still Exploration) is
//! rejected no matter how high its mean, and an Exploitation candidate whose lower confidence bound is
//! below the bar is rejected as a lucky single draw. Records are `serde`-serialisable and round-trip
//! through [`write_jsonl`](StrategyRepository::write_jsonl) / [`read_records`](StrategyRepository::read_records)
//! one JSON object per line — the portable, auditable vintage form. Ensemble construction is QE-126.

use std::io::{self, BufRead, Write};

use qe_determinism::{HasLineage, Lineage};
use serde::{Deserialize, Serialize};

use crate::fitness::NoiseRobustFitness;
use crate::genome::Genome;
use crate::lifecycle::{QualityGate, QualityThreshold};

/// A persisted strategy: the genome, its noise-robust fitness summary, and the lineage that produced it.
///
/// This record is a **non-hashed** vintage artefact (it is not part of the content-hashed
/// [`VintageContent`](../../vintage) — it only round-trips through the JSONL form), so reporting fields
/// added here never move a `content_hash` or golden.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyRecord {
    /// The strategy genome (QE-110).
    pub genome: Genome,
    /// Noise-robust geometric fitness over evaluation windows (QE-113/120).
    pub fitness: NoiseRobustFitness,
    /// Worst peak-to-trough drawdown of the strategy's realised equity path (QE-446), a non-negative
    /// magnitude in `[0, 1]` (see [`max_drawdown`](crate::fitness::max_drawdown)). **Reporting only** —
    /// `log_growth` fitness is blind to intermediate drawdown at a fixed size, so this surfaces it on
    /// the per-strategy record. It rides the non-hashed record, so recording it moves no golden; the
    /// behaviour-changing use of drawdown is the *optional* gate ceiling
    /// ([`QualityGate::max_drawdown_ceiling`](crate::lifecycle::QualityGate)), OFF by default.
    pub max_drawdown: f64,
    /// Provenance — resolvable to a vintage (QE-006).
    pub lineage: Lineage,
}

impl HasLineage for StrategyRecord {
    fn lineage(&self) -> &Lineage {
        &self.lineage
    }
}

/// The strategy repository: a phased-gate-admitted, lineage-tagged store of survivors.
#[derive(Debug, Clone)]
pub struct StrategyRepository {
    gate: QualityGate,
    records: Vec<StrategyRecord>,
}

impl StrategyRepository {
    /// An empty repository admitting via `gate`.
    #[must_use]
    pub fn new(gate: QualityGate) -> Self {
        StrategyRepository {
            gate,
            records: Vec::new(),
        }
    }

    /// An empty repository with the QE-114 default quality gate.
    #[must_use]
    pub fn with_defaults() -> Self {
        StrategyRepository::new(QualityGate::with_defaults())
    }

    /// The admission gate.
    #[must_use]
    pub fn gate(&self) -> &QualityGate {
        &self.gate
    }

    /// The persisted records.
    #[must_use]
    pub fn records(&self) -> &[StrategyRecord] {
        &self.records
    }

    /// Number of persisted strategies.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the repository holds no strategies.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Record a candidate **iff** it is an exploitation-phase survivor above `threshold` (QE-114) **and**
    /// within the gate's optional drawdown ceiling (QE-446). `max_drawdown` is the candidate's realised
    /// peak-to-trough drawdown magnitude ([`max_drawdown`](crate::fitness::max_drawdown)); it is stored
    /// on the record for reporting and, when the gate configures a ceiling, gates admission. Returns
    /// `true` if persisted (tagged with `lineage`), `false` if the gate rejects it — early lucky
    /// candidates, below-bar draws, and (when a ceiling is set) deep-drawdown genomes are not persisted.
    ///
    /// The gate's drawdown ceiling defaults OFF, so with a default gate this admits **exactly** the same
    /// candidates as before — the drawdown is pure reporting.
    pub fn try_record(
        &mut self,
        genome: Genome,
        fitness: NoiseRobustFitness,
        max_drawdown: f64,
        threshold: &QualityThreshold,
        lineage: Lineage,
    ) -> bool {
        if self
            .gate
            .persists_with_drawdown(&fitness, max_drawdown, threshold)
        {
            self.records.push(StrategyRecord {
                genome,
                fitness,
                max_drawdown,
                lineage,
            });
            true
        } else {
            false
        }
    }

    /// Record every survivor of a cohort against `threshold`, in input order; returns how many persisted.
    /// Each cohort item carries its realised `max_drawdown` (QE-446).
    pub fn record_survivors(
        &mut self,
        candidates: impl IntoIterator<Item = (Genome, NoiseRobustFitness, f64, Lineage)>,
        threshold: &QualityThreshold,
    ) -> usize {
        let before = self.records.len();
        for (genome, fitness, max_drawdown, lineage) in candidates {
            self.try_record(genome, fitness, max_drawdown, threshold, lineage);
        }
        self.records.len() - before
    }

    /// Write the records as JSON Lines (one record per line) to `w` — durable, auditable persistence.
    ///
    /// # Errors
    /// Returns any underlying I/O error, or a serialisation error mapped to [`io::Error`].
    pub fn write_jsonl<W: Write>(&self, w: &mut W) -> io::Result<()> {
        for record in &self.records {
            let line = serde_json::to_string(record).map_err(io::Error::other)?;
            writeln!(w, "{line}")?;
        }
        Ok(())
    }

    /// Read strategy records from a JSON Lines source (blank lines skipped).
    ///
    /// # Errors
    /// Returns any underlying I/O error, or a deserialisation error mapped to [`io::Error`].
    pub fn read_records<R: BufRead>(reader: R) -> io::Result<Vec<StrategyRecord>> {
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            out.push(serde_json::from_str(&line).map_err(io::Error::other)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::{backtest, BacktestConfig, Bar};
    use crate::genome::{Clause, ExitParams, RiskParams, RuleSet, CLAUSES_PER_SET, REP_VERSION};
    use crate::lifecycle::ThresholdPolicy;
    use qe_signal::{CatalogueConfig, FeatureSchema, QState};
    use rust_decimal::Decimal;

    fn schema() -> FeatureSchema {
        FeatureSchema::from_catalogue(&CatalogueConfig { states: 5 })
    }

    fn long_genome(hold: u16) -> Genome {
        let mut clauses = [Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        }; CLAUSES_PER_SET];
        clauses[0] = Clause {
            enabled: true,
            feature: 0,
            lo: 3,
            hi: 4,
        };
        Genome {
            version: REP_VERSION,
            long_entry: RuleSet {
                clauses,
                min_satisfied: 1,
            },
            short_entry: RuleSet {
                clauses: [Clause {
                    enabled: false,
                    feature: 0,
                    lo: 0,
                    hi: 0,
                }; CLAUSES_PER_SET],
                min_satisfied: 1,
            },
            exit: ExitParams {
                max_holding_bars: hold,
                exit_on_opposite: false,
            },
            risk: RiskParams { size_bps: 5_000 },
        }
    }

    fn uptrend_bars(schema: &FeatureSchema, n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let state0 = if (i / 2) % 2 == 0 { 4 } else { 0 };
                let mut states = vec![None; schema.len()];
                states[0] = Some(QState::from_index(state0));
                Bar {
                    features: qe_signal::FeatureVector {
                        time_ms: i as i64 * 60_000,
                        states,
                    },
                    price: Decimal::from(100 + i as i64),
                    volume: Decimal::from(1000),
                    funding_rate: None,
                }
            })
            .collect()
    }

    fn lineage(seed: u64) -> Lineage {
        Lineage::new(
            "cfg-hash-abc",
            "snapshot-2024-01",
            "commit-deadbeef",
            vec![seed],
        )
    }

    #[test]
    fn early_lucky_candidate_is_not_persisted_but_survivor_is() {
        let s = schema();
        let bars = uptrend_bars(&s, 160);
        let g = long_genome(2);

        // Exploitation evaluation (n = 6 windows ≥ min_exploitation_windows = 5).
        let robust_bt = backtest(
            &g,
            &bars,
            &BacktestConfig {
                windows: 6,
                ..BacktestConfig::default()
            },
        );
        let robust = robust_bt.fitness;
        let robust_dd = crate::fitness::max_drawdown(&robust_bt.returns);
        assert_eq!(robust.n, 6);
        assert!(robust.mean.is_finite());

        // "Early lucky": the SAME genome evaluated on a single window (Exploration, n = 1) — high mean,
        // no robustness. Must not persist regardless of how good that one draw looks.
        let lucky_bt = backtest(
            &g,
            &bars,
            &BacktestConfig {
                windows: 1,
                ..BacktestConfig::default()
            },
        );
        let lucky = lucky_bt.fitness;
        let lucky_dd = crate::fitness::max_drawdown(&lucky_bt.returns);
        assert_eq!(lucky.n, 1);

        // A bar below the robust candidate so it clears, derived from a cohort distribution.
        let mut repo = StrategyRepository::new(QualityGate::new(
            ThresholdPolicy::Quantile(0.5),
            5,
            crate::fitness::DEFAULT_K_SIGMA,
        ));
        let distribution = [robust.mean - 0.01, robust.mean - 0.02, robust.mean - 0.03];
        let threshold = repo.gate().threshold(&distribution);

        // The early lucky (Exploration) candidate is rejected …
        assert!(!repo.try_record(g.clone(), lucky, lucky_dd, &threshold, lineage(1)));
        assert!(repo.is_empty());
        // … the exploitation survivor above the bar is recorded, carrying its drawdown stat.
        assert!(repo.try_record(g.clone(), robust, robust_dd, &threshold, lineage(2)));
        assert_eq!(repo.len(), 1);
        // The reporting statistic rides the record (QE-446).
        assert_eq!(repo.records()[0].max_drawdown, robust_dd);

        // A below-threshold exploitation candidate is also rejected.
        let below = NoiseRobustFitness {
            mean: robust.mean - 100.0,
            std_error: robust.std_error,
            n: 6,
        };
        assert!(!repo.try_record(g, below, robust_dd, &threshold, lineage(3)));
        assert_eq!(repo.len(), 1);
    }

    #[test]
    fn persisted_strategy_carries_resolvable_lineage() {
        let mut repo = StrategyRepository::with_defaults();
        let fitness = NoiseRobustFitness {
            mean: 0.10,
            std_error: 0.001,
            n: 6,
        };
        let threshold = QualityThreshold::at(0.0);
        let lin = lineage(42);
        assert!(repo.try_record(long_genome(3), fitness, 0.1, &threshold, lin.clone()));

        let rec = &repo.records()[0];
        // The record carries the supplied lineage, and it resolves to a stable id.
        assert_eq!(rec.lineage(), &lin);
        assert_eq!(rec.lineage().id().unwrap(), lin.id().unwrap());
    }

    #[test]
    fn records_round_trip_through_jsonl_with_lineage() {
        let mut repo = StrategyRepository::with_defaults();
        let threshold = QualityThreshold::at(0.0);
        for (i, hold) in [2u16, 5, 9].iter().enumerate() {
            let fitness = NoiseRobustFitness {
                mean: 0.05 + i as f64 * 0.01,
                std_error: 0.001,
                n: 6,
            };
            let dd = 0.05 + i as f64 * 0.02;
            assert!(repo.try_record(
                long_genome(*hold),
                fitness,
                dd,
                &threshold,
                lineage(i as u64)
            ));
        }

        let mut buf: Vec<u8> = Vec::new();
        repo.write_jsonl(&mut buf).unwrap();
        let back = StrategyRepository::read_records(buf.as_slice()).unwrap();
        assert_eq!(back, repo.records());
        // Lineage survives the round-trip.
        assert_eq!(
            back[0].lineage().id().unwrap(),
            repo.records()[0].lineage().id().unwrap()
        );
    }

    #[test]
    fn record_survivors_persists_only_the_gate_survivors_in_order() {
        let mut repo = StrategyRepository::with_defaults();
        let threshold = QualityThreshold::at(0.05);
        let cohort = vec![
            // exploitation, above bar → persists
            (
                long_genome(2),
                NoiseRobustFitness {
                    mean: 0.10,
                    std_error: 0.0,
                    n: 6,
                },
                0.10,
                lineage(1),
            ),
            // exploration (n=1) → rejected
            (
                long_genome(3),
                NoiseRobustFitness {
                    mean: 0.99,
                    std_error: 0.0,
                    n: 1,
                },
                0.10,
                lineage(2),
            ),
            // exploitation, below bar → rejected
            (
                long_genome(4),
                NoiseRobustFitness {
                    mean: 0.01,
                    std_error: 0.0,
                    n: 6,
                },
                0.10,
                lineage(3),
            ),
            // exploitation, above bar → persists
            (
                long_genome(7),
                NoiseRobustFitness {
                    mean: 0.20,
                    std_error: 0.0,
                    n: 6,
                },
                0.10,
                lineage(4),
            ),
        ];
        let persisted = repo.record_survivors(cohort, &threshold);
        assert_eq!(persisted, 2);
        assert_eq!(repo.len(), 2);
        // Order preserved: the two survivors are holds 2 and 7.
        assert_eq!(repo.records()[0].genome.exit.max_holding_bars, 2);
        assert_eq!(repo.records()[1].genome.exit.max_holding_bars, 7);
    }

    #[test]
    fn drawdown_ceiling_blocks_deep_drawdown_survivor_and_stat_rides_record() {
        use crate::lifecycle::ThresholdPolicy;
        // A gate that graduates on growth but enforces a 30% drawdown ceiling (QE-446 behaviour-on).
        let gate = QualityGate::new(
            ThresholdPolicy::Quantile(0.75),
            5,
            crate::fitness::DEFAULT_K_SIGMA,
        )
        .with_drawdown_ceiling(0.30);
        let mut repo = StrategyRepository::new(gate);
        let threshold = QualityThreshold::at(0.10);
        let strong = NoiseRobustFitness {
            mean: 0.20,
            std_error: 0.0,
            n: 6,
        }; // clears the growth bar

        // Deep-drawdown (45%) high-growth genome is BLOCKED by the ceiling …
        assert!(!repo.try_record(long_genome(2), strong, 0.45, &threshold, lineage(1)));
        assert!(repo.is_empty());
        // … the SAME growth with a shallow (20%) drawdown graduates, and the stat rides the record.
        assert!(repo.try_record(long_genome(3), strong, 0.20, &threshold, lineage(2)));
        assert_eq!(repo.len(), 1);
        assert_eq!(repo.records()[0].max_drawdown, 0.20);

        // Default gate (ceiling OFF) admits the same deep-drawdown genome — the stat is pure reporting.
        let mut repo_off = StrategyRepository::new(QualityGate::new(
            ThresholdPolicy::Quantile(0.75),
            5,
            crate::fitness::DEFAULT_K_SIGMA,
        ));
        assert!(repo_off.try_record(long_genome(2), strong, 0.45, &threshold, lineage(3)));
        assert_eq!(repo_off.records()[0].max_drawdown, 0.45);
    }
}
