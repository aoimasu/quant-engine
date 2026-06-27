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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyRecord {
    /// The strategy genome (QE-110).
    pub genome: Genome,
    /// Noise-robust geometric fitness over evaluation windows (QE-113/120).
    pub fitness: NoiseRobustFitness,
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

    /// Record a candidate **iff** it is an exploitation-phase survivor above `threshold` (QE-114). Returns
    /// `true` if persisted (tagged with `lineage`), `false` if the gate rejects it — early lucky
    /// candidates and below-bar draws are not persisted.
    pub fn try_record(
        &mut self,
        genome: Genome,
        fitness: NoiseRobustFitness,
        threshold: &QualityThreshold,
        lineage: Lineage,
    ) -> bool {
        if self.gate.persists(&fitness, threshold) {
            self.records.push(StrategyRecord {
                genome,
                fitness,
                lineage,
            });
            true
        } else {
            false
        }
    }

    /// Record every survivor of a cohort against `threshold`, in input order; returns how many persisted.
    pub fn record_survivors(
        &mut self,
        candidates: impl IntoIterator<Item = (Genome, NoiseRobustFitness, Lineage)>,
        threshold: &QualityThreshold,
    ) -> usize {
        let before = self.records.len();
        for (genome, fitness, lineage) in candidates {
            self.try_record(genome, fitness, threshold, lineage);
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
        let robust = backtest(
            &g,
            &bars,
            &BacktestConfig {
                windows: 6,
                ..BacktestConfig::default()
            },
        )
        .fitness;
        assert_eq!(robust.n, 6);
        assert!(robust.mean.is_finite());

        // "Early lucky": the SAME genome evaluated on a single window (Exploration, n = 1) — high mean,
        // no robustness. Must not persist regardless of how good that one draw looks.
        let lucky = backtest(
            &g,
            &bars,
            &BacktestConfig {
                windows: 1,
                ..BacktestConfig::default()
            },
        )
        .fitness;
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
        assert!(!repo.try_record(g.clone(), lucky, &threshold, lineage(1)));
        assert!(repo.is_empty());
        // … the exploitation survivor above the bar is recorded.
        assert!(repo.try_record(g.clone(), robust, &threshold, lineage(2)));
        assert_eq!(repo.len(), 1);

        // A below-threshold exploitation candidate is also rejected.
        let below = NoiseRobustFitness {
            mean: robust.mean - 100.0,
            std_error: robust.std_error,
            n: 6,
        };
        assert!(!repo.try_record(g, below, &threshold, lineage(3)));
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
        assert!(repo.try_record(long_genome(3), fitness, &threshold, lin.clone()));

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
            assert!(repo.try_record(long_genome(*hold), fitness, &threshold, lineage(i as u64)));
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
}
