//! QE-499 Phase B — the **`FormulaPool` → `CompiledFormula` train-intake bridge**.
//!
//! This mapping lives in **qe-cli** (which already depends on both `qe-formula-pool` and `qe-signal`), so
//! `qe-signal` never gains a `qe-formula-pool` edge (design §5). Given a sealed [`FormulaPool`], it:
//!
//! 1. **Governs intake (B5 / §4 majors), fail-closed:** the pool must be **Sandbox** mode, its deflation
//!    summary must be **GP-aware** (`gp_aware == true`) and carry a present, readable `uncensored_pbo` and a
//!    non-degenerate trial basis (else hard errors). `gate_evidence` (production hard-blocks 5–8, QE-454) is
//!    **enforced if present** but **not required** here — a pool vintage is unconditionally non-production
//!    (write-but-mark + load-boundary reject + research-root isolation), so requiring it protected nothing at
//!    the research stage; it becomes mandatory again when a pool vintage can reach production (QE-499 B5,
//!    relaxed after review sign-off). A pool failing any required check is rejected before a formula enters
//!    the catalogue.
//! 2. **Reconstructs each formula (B6):** parses the sealed canonical `sexpr` back to an [`Expr`], and
//!    **verifies the round-trip hash** (`canonical_hash(parse(sexpr)) == formula_hash`) — a sexpr/hash
//!    disagreement is rejected. Each becomes a [`CompiledFormula`] `{ id = formula_hash, expr,
//!    quantiser_for_root(root_op, states) }`, compiled at the **catalogue's own `states`** so `num_states`
//!    stays uniform (design §5, the `from_specs` last-writer-wins hazard).
//! 3. **Extracts the §4 deflation basis:** the pool's additive trial count and the `(trial_variance,
//!    expected_max_sharpe)` floor the train composes into the DSR bar.

use rust_decimal::prelude::ToPrimitive;

use qe_formula_pool::{FormulaPool, PoolMode};
use qe_signal::indicator::expr::{parse_sexpr, Expr, ExprTree, WinOp};
use qe_signal::CompiledFormula;
use qe_validation::DeflationFloor;
use qe_wfo::gp::quantiser_for_root;

use super::RunError;

/// The verified, compiled result of admitting a sealed pool into a train (QE-499 Phase B).
#[derive(Debug, Clone)]
pub struct PoolIntake {
    /// The sealed pool's id (folded into the run-params fingerprint, QE-496).
    pub pool_id: String,
    /// The injected catalogue formulas (compiled inside `catalogue()` via `CatalogueConfig.formula_pool`).
    pub compiled: Vec<CompiledFormula>,
    /// The sorted, sanctioned `formula_hash`es — the widened-identity payload (`with_formula_pool`) and the
    /// preimage the B4 load boundary rebuilds `current_with_pool` from.
    pub formula_hashes: Vec<String>,
    /// The §4 deflation floor: `max(genome variance, pool.trial_variance)` and the sealed `E[max SR]` bar.
    pub deflation_floor: DeflationFloor,
    /// The §4 **additive** pool trial basis summed onto the genome basis — the WITHIN-stage conservative
    /// `max(n_trials, distinct_evaluations, analytic_floor)` (never the sealed `n_trials` verbatim), so a
    /// pool sealed with a degenerate `n_trials` cannot widen the catalogue yet add nothing to the count.
    pub pool_n_trials: usize,
    /// The pool's sealed **uncensored PBO** (the primary GP gate) — the vintage's genome-stage PBO is floored
    /// by this at intake (`pbo = pbo.max(pool_uncensored_pbo)`) so a sealed PBO penalty is never silently
    /// dropped (MF-4). Present-and-readable is a hard requirement at intake (`RunError::PoolUncensoredPboAbsent`
    /// / `PoolUnreadableDeflationFloor`).
    pub pool_uncensored_pbo: f64,
    /// The pool's campaign id (for the data-window provenance audit trail).
    pub campaign_id: String,
    /// The pool's pinned input-snapshot id (design §6 **data-window provenance** major). **Empty** until the
    /// evolve ingest-snapshot seam lands — an empty value means the train **cannot verify** that the G1
    /// train/holdout window is disjoint from the formulas' evolve window (an overlap is in-sample
    /// contamination regardless of trial counting). The train surfaces a warning in that case; a real
    /// disjoint-window/embargo assertion is deferred to when the pool artefact records its evolve window.
    pub input_snapshot_id: String,
}

/// Admit a sealed [`FormulaPool`] into a train, compiling its formulas and extracting the §4 deflation
/// basis, after enforcing the B5 / §4 intake governance.
///
/// `states` is the **train catalogue's** state count (`catalogue_config().states`), NOT the pool's evolve
/// states — every formula is quantised at it so the widened schema's `num_states` stays uniform.
///
/// # Errors
/// [`RunError::PoolNotSandbox`], [`RunError::PoolNotGpAware`], [`RunError::PoolGateEvidenceMissing`],
/// [`RunError::PoolGateEvidenceFailed`], [`RunError::PoolFormulaParse`], or
/// [`RunError::PoolFormulaHashMismatch`] on a governance/reconstruction failure.
pub fn admit_pool(pool: &FormulaPool, states: u16) -> Result<PoolIntake, RunError> {
    let content = &pool.content;
    let pool_id = content.pool_id.clone();

    // ---- B5 (a): Sandbox-only. A production pool must go through the QE-454 seal path, not this bridge. ----
    if content.mode != PoolMode::Sandbox {
        return Err(RunError::PoolNotSandbox {
            pool_id,
            mode: format!("{:?}", content.mode),
        });
    }
    // ---- §4 major: the trial basis must be GP-aware, else the composed deflation would be dishonest. ----
    if !content.deflation.gp_aware {
        return Err(RunError::PoolNotGpAware { pool_id });
    }
    // ---- B5 — gate_evidence is enforced-IF-PRESENT, not required at the research intake (QE-499, relaxed
    // after review sign-off 2026-08-02). Rationale: `gate_evidence` (per-formula hard-blocks 5–8: IC/FDR,
    // cost-stress, turnover, capacity, random-entry null) is a PRODUCTION-seal control (QE-454). A pool
    // vintage produced here is *unconditionally non-production* — force-marked `promoted=false`
    // (write-but-mark, QE-476), rejected by the generic `assert_schema` load boundary, and its pool lives in
    // the research-only artefacts root the production repository never scans. Requiring gate_evidence
    // therefore protected nothing at this stage while blocking the legitimate research re-hunt (and `qe
    // evolve` does not emit it). We STILL enforce it when a pool carries it (defense-in-depth), and it
    // becomes MANDATORY again the moment a pool vintage can reach production — a future phase governed by
    // QE-454. The honesty-preserving controls that DO stay required: Sandbox mode, `gp_aware`,
    // `uncensored_pbo` present, and the additive two-stage deflation composition (§4).
    if let Some(evidence) = content.gate_evidence.as_ref() {
        for f in &content.formulas {
            let row = evidence
                .iter()
                .find(|e| e.formula_hash == f.formula_hash)
                .ok_or_else(|| RunError::PoolGateEvidenceMissing {
                    pool_id: pool_id.clone(),
                    formula_hash: f.formula_hash.clone(),
                })?;
            if !row.passes() {
                return Err(RunError::PoolGateEvidenceFailed {
                    formula_hash: f.formula_hash.clone(),
                });
            }
        }
    }

    // ---- B6: reconstruct + hash-verify each formula, compile at the catalogue's own `states`. ----
    let mut compiled = Vec::with_capacity(content.formulas.len());
    for f in &content.formulas {
        let expr = parse_sexpr(&f.sexpr).map_err(|e| RunError::PoolFormulaParse {
            formula_hash: f.formula_hash.clone(),
            reason: e.to_string(),
        })?;
        // The stored sexpr is canonical, so `canonical_hash` reproduces the sealed `formula_hash` exactly;
        // any disagreement means the sexpr and hash were not sealed together — fail closed.
        let tree = ExprTree::new(expr.clone());
        let recomputed = tree.canonical_hash();
        if recomputed != f.formula_hash {
            return Err(RunError::PoolFormulaHashMismatch {
                stored: f.formula_hash.clone(),
                recomputed,
            });
        }
        let root_op = match &expr {
            Expr::Window(op, _, _) => *op,
            _ => WinOp::Rank, // a canonical pool formula always has a normalising window root; safe default
        };
        compiled.push(CompiledFormula {
            id: f.formula_hash.clone(),
            expr,
            quantiser: quantiser_for_root(root_op, states),
        });
    }

    let mut formula_hashes: Vec<String> = content
        .formulas
        .iter()
        .map(|f| f.formula_hash.clone())
        .collect();
    formula_hashes.sort();
    formula_hashes.dedup();

    // ---- MF-3: the deflation floor must FAIL CLOSED. A `to_f64()` read failure on either bar previously
    // defaulted to `0.0` — which is "no penalty" under the composing `max`, silently SOFTENING the DSR bar.
    // A floor that cannot be read must BLOCK admission, never default to no-penalty. ----
    let trial_variance = content.deflation.trial_variance.to_f64().ok_or_else(|| {
        RunError::PoolUnreadableDeflationFloor {
            pool_id: pool_id.clone(),
            field: "trial_variance",
        }
    })?;
    let expected_max_sharpe = content
        .deflation
        .expected_max_sharpe
        .to_f64()
        .ok_or_else(|| RunError::PoolUnreadableDeflationFloor {
            pool_id: pool_id.clone(),
            field: "expected_max_sharpe",
        })?;

    // ---- MF-2: the pool trial basis is NOT trusted verbatim. §4 requires the WITHIN-stage conservative
    // `max(n_trials, distinct_evaluations, analytic_floor)` — a pool sealed with a degenerate `n_trials`
    // (e.g. 0) would otherwise widen the catalogue yet add nothing to the composed count. A zero/degenerate
    // basis (all three fields 0) is a hard reject: it cannot honestly count the pool's multiple testing. ----
    let pool_n_trials = content
        .deflation
        .n_trials
        .max(content.deflation.distinct_evaluations)
        .max(content.deflation.analytic_floor) as usize;
    if pool_n_trials == 0 {
        return Err(RunError::PoolDegenerateTrialBasis {
            pool_id: pool_id.clone(),
        });
    }

    // ---- MF-4: the pool's sealed uncensored PBO (the primary GP gate) is carried through so the vintage's
    // genome-stage PBO can be floored by it (never silently dropped). Present-and-readable is a hard
    // requirement at intake — an absent PBO is a QE-454 hard-block, and an unreadable one must fail closed. ----
    let pool_uncensored_pbo = content
        .deflation
        .uncensored_pbo
        .ok_or_else(|| RunError::PoolUncensoredPboAbsent {
            pool_id: pool_id.clone(),
        })?
        .to_f64()
        .ok_or_else(|| RunError::PoolUnreadableDeflationFloor {
            pool_id: pool_id.clone(),
            field: "uncensored_pbo",
        })?;

    Ok(PoolIntake {
        pool_id,
        compiled,
        formula_hashes,
        deflation_floor: DeflationFloor {
            trial_variance,
            expected_max_sharpe,
        },
        pool_n_trials,
        pool_uncensored_pbo,
        campaign_id: content.lineage.campaign_id.clone(),
        input_snapshot_id: content.lineage.input_snapshot_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_formula_pool::{
        DeflationSummary, FormulaGateEvidence, FormulaPoolContent, PoolFormula, PoolLineage,
        POOL_FORMAT_VERSION,
    };
    use qe_signal::indicator::expr::{Field, WinOp};
    use rust_decimal::Decimal;

    /// A real frozen formula: its canonical sexpr + the SHA-256 `formula_hash` over it.
    fn frozen(op: WinOp, field: Field, period: usize) -> PoolFormula {
        let tree = ExprTree::repaired(Expr::Window(op, Box::new(Expr::Input(field)), period));
        PoolFormula {
            sexpr: tree.canonical_sexpr(),
            formula_hash: tree.canonical_hash(),
        }
    }

    fn passing_evidence(formula_hash: &str) -> FormulaGateEvidence {
        FormulaGateEvidence {
            formula_hash: formula_hash.to_owned(),
            ic_two_fold_same_sign_fdr_pass: true,
            cost_stress_min_net_log_growth: Decimal::new(5, 3),
            realised_turnover_frac: Decimal::new(20, 2),
            capacity_usd: Decimal::from(300_000),
            within_caps_and_stratum_deflated: true,
            random_entry_null_pass: true,
        }
    }

    fn deflation(gp_aware: bool) -> DeflationSummary {
        DeflationSummary {
            gp_aware,
            distinct_evaluations: 192,
            n_trials: 200,
            analytic_floor: 90,
            variance_trials: 45,
            trial_variance: Decimal::new(1234, 4),
            expected_max_sharpe: Decimal::new(21, 1),
            champion_dsr: Decimal::new(97, 2),
            uncensored_pbo: Some(Decimal::new(42, 2)),
        }
    }

    fn content(mode: PoolMode, gp_aware: bool, with_evidence: bool) -> FormulaPoolContent {
        let mut formulas = vec![
            frozen(WinOp::Rank, Field::Close, 20),
            frozen(WinOp::Zscore, Field::High, 50),
        ];
        formulas.sort_by(|a, b| a.formula_hash.cmp(&b.formula_hash));
        let gate_evidence = with_evidence.then(|| {
            formulas
                .iter()
                .map(|f| passing_evidence(&f.formula_hash))
                .collect()
        });
        FormulaPoolContent {
            format_version: POOL_FORMAT_VERSION,
            pool_id: "campaign-xyz".to_owned(),
            mode,
            formulas,
            deflation: deflation(gp_aware),
            gate_evidence,
            lineage: PoolLineage {
                campaign_id: "campaign-xyz".to_owned(),
                seed: 7,
                mode,
                code_commit: "commit".to_owned(),
                input_snapshot_id: String::new(),
                config_hash: "cfg".to_owned(),
                pool_hash: "poolhash".to_owned(),
            },
        }
    }

    fn seal(c: FormulaPoolContent) -> FormulaPool {
        FormulaPool::seal(c).unwrap()
    }

    #[test]
    fn admits_a_sandbox_gp_aware_evidenced_pool_and_extracts_the_basis() {
        let pool = seal(content(PoolMode::Sandbox, true, true));
        let intake = admit_pool(&pool, 5).unwrap();
        assert_eq!(intake.compiled.len(), 2);
        // Each injected id is a 64-hex formula_hash, and the compiled expr re-hashes to it.
        for cf in &intake.compiled {
            assert_eq!(cf.id.len(), 64);
            assert_eq!(ExprTree::new(cf.expr.clone()).canonical_hash(), cf.id);
        }
        // §4 basis carried through from the sealed deflation block. MF-2: pool_n_trials is the WITHIN-stage
        // max(n_trials=200, distinct=192, analytic=90) = 200 (not the verbatim field).
        assert_eq!(intake.pool_n_trials, 200);
        assert!((intake.deflation_floor.trial_variance - 0.1234).abs() < 1e-9);
        assert!((intake.deflation_floor.expected_max_sharpe - 2.1).abs() < 1e-9);
        // MF-4: the pool's sealed uncensored PBO is carried through for the vintage PBO floor.
        assert!((intake.pool_uncensored_pbo - 0.42).abs() < 1e-9);
        // Sanctioned hashes are sorted + match the formulas.
        assert_eq!(intake.formula_hashes.len(), 2);
        assert!(intake.formula_hashes.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn rejects_a_production_pool() {
        let pool = seal(content(PoolMode::Production, true, true));
        assert!(matches!(
            admit_pool(&pool, 5),
            Err(RunError::PoolNotSandbox { .. })
        ));
    }

    #[test]
    fn rejects_a_non_gp_aware_pool_hard() {
        let pool = seal(content(PoolMode::Sandbox, false, true));
        assert!(matches!(
            admit_pool(&pool, 5),
            Err(RunError::PoolNotGpAware { .. })
        ));
    }

    #[test]
    fn admits_absent_gate_evidence_for_a_research_pool() {
        // QE-499 B5 (relaxed): gate_evidence is a PRODUCTION control (QE-454); a Sandbox research pool
        // vintage is non-production regardless, so an ABSENT gate_evidence block no longer blocks intake —
        // the other honesty controls (gp_aware, uncensored_pbo, the §4 deflation composition) still apply.
        let pool = seal(content(PoolMode::Sandbox, true, false));
        let intake = admit_pool(&pool, 5)
            .expect("a sandbox, gp-aware, uncensored-pbo pool admits w/o gate_evidence");
        assert_eq!(intake.compiled.len(), 2);
        assert_eq!(intake.pool_n_trials, 200);
    }

    #[test]
    fn rejects_failing_gate_evidence() {
        let mut c = content(PoolMode::Sandbox, true, true);
        // Flip one formula's evidence to a failing capacity (< floor).
        if let Some(ev) = c.gate_evidence.as_mut() {
            ev[0].capacity_usd = Decimal::from(1000);
        }
        let pool = seal(c);
        assert!(matches!(
            admit_pool(&pool, 5),
            Err(RunError::PoolGateEvidenceFailed { .. })
        ));
    }

    #[test]
    fn rejects_a_zero_or_degenerate_trial_basis() {
        // MF-2: a pool whose entire trial basis is zero — `max(n_trials, distinct_evaluations,
        // analytic_floor) == 0` — must be rejected. Admitting it would widen the catalogue yet add nothing
        // to the composed multiple-testing count (the forbidden uncounted case). All other governance passes.
        let mut c = content(PoolMode::Sandbox, true, true);
        c.deflation.n_trials = 0;
        c.deflation.distinct_evaluations = 0;
        c.deflation.analytic_floor = 0;
        let pool = seal(c);
        assert!(matches!(
            admit_pool(&pool, 5),
            Err(RunError::PoolDegenerateTrialBasis { .. })
        ));
    }

    #[test]
    fn max_composes_the_within_stage_pool_basis() {
        // MF-2: the pool basis is the WITHIN-stage max, so a small sealed `n_trials` cannot understate the
        // count when a larger distinct/analytic floor exists.
        let mut c = content(PoolMode::Sandbox, true, true);
        c.deflation.n_trials = 5; // understated
        c.deflation.distinct_evaluations = 512;
        c.deflation.analytic_floor = 90;
        let intake = admit_pool(&seal(c), 5).unwrap();
        assert_eq!(intake.pool_n_trials, 512, "max(5, 512, 90)");
    }

    #[test]
    fn rejects_an_absent_uncensored_pbo() {
        // MF-4: the pool's sealed uncensored PBO (the primary GP gate) is a present-and-readable hard
        // requirement — an absent PBO cannot be dropped silently; the vintage PBO cannot be floored by a
        // missing penalty. Fail closed.
        let mut c = content(PoolMode::Sandbox, true, true);
        c.deflation.uncensored_pbo = None;
        let pool = seal(c);
        assert!(matches!(
            admit_pool(&pool, 5),
            Err(RunError::PoolUncensoredPboAbsent { .. })
        ));
    }

    #[test]
    fn rejects_a_sexpr_hash_mismatch() {
        // A pool whose stored formula_hash disagrees with its sexpr cannot even seal (validate checks 64-hex,
        // but a wrong-but-well-formed hash passes seal) — build one and confirm intake fails closed.
        let mut c = content(PoolMode::Sandbox, true, true);
        // Replace formula[0]'s hash with a different real formula's hash (well-formed, wrong).
        let wrong = frozen(WinOp::Rank, Field::Low, 100).formula_hash;
        c.formulas[0].formula_hash = wrong.clone();
        // Re-sort + re-point evidence so seal's strict-ascending + evidence-binding pass.
        c.formulas
            .sort_by(|a, b| a.formula_hash.cmp(&b.formula_hash));
        c.gate_evidence = Some(
            c.formulas
                .iter()
                .map(|f| passing_evidence(&f.formula_hash))
                .collect(),
        );
        let pool = seal(c);
        assert!(matches!(
            admit_pool(&pool, 5),
            Err(RunError::PoolFormulaHashMismatch { .. })
        ));
    }
}
