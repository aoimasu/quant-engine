//! QE-454 Phase B — the **server-authoritative production-seal predicate** (design §13.5/§13.7).
//!
//! [`seal_allowed`] is the single source of truth for whether a frozen formula pool may be sealed to
//! production. It reads **only three inputs** — the hash-verified [`FormulaPoolContent`], the audit-log
//! replay, and the compiled `DEFLATION_BASIS_VERSION` (plus two facts *derived* from those server-side: the
//! resolved launcher and the revocation status). **No request field feeds it.** It requires **all** of
//! §13.5's eight hard-blocks **plus** `mode == production`, the const satisfied, the pool not revoked, and
//! **two distinct valid approver signatures (neither == launcher)** re-derived from `pool_hash`-bound
//! `approve` events (never the stored `review.json`).
//!
//! **Every absent stat is a BLOCK** (design §13.5) — an absent uncensored-PBO, an absent per-formula
//! evidence block, or an unresolved launcher all fail closed. Any failure yields a **named blocker list**;
//! the caller turns that into a `409` + an appended rejected-attempt audit entry.
//!
//! The predicate is a **pure function** over its inputs, so every hard-block is independently unit-tested
//! (flip one stat bad ⇒ its named blocker appears) without an HTTP or filesystem harness.

use qe_formula_pool::FormulaPoolContent;
use qe_validation::basis_satisfied;
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};

use crate::audit::{AuditEntry, AuditLog, SignoffState};

/// The uncensored-PBO ceiling (design §13.13 open-q3 proposes 0.5 pending a shuffle-null calibration study;
/// mirrors `qe_wfo::gp::deflation::GpDeflationGate::default().max_pbo`).
const MAX_UNCENSORED_PBO: (i64, u32) = (5, 1); // 0.5
/// The DSR floor (necessary-not-sufficient; mirrors `qe_gate::DEFAULT_DSR_THRESHOLD` / the deflation gate).
const MIN_DSR: (i64, u32) = (95, 2); // 0.95

/// The outcome of [`seal_allowed`]: whether the pool may be sealed, the **named** blockers (empty iff
/// allowed), and the `evidence_hash` over the exact enforced stat set (design §13.5 "displayed = enforced =
/// evidenced" — recorded in the audit entry whether the attempt succeeds or is rejected).
#[derive(Debug, Clone, PartialEq)]
pub struct SealDecision {
    /// Whether the pool cleared **every** gate (all 8 hard-blocks + mode + const + dual-sig + not revoked).
    pub allowed: bool,
    /// The named blockers, in evaluation order (empty iff `allowed`).
    pub blockers: Vec<String>,
    /// SHA-256 over the canonical enforced stat set — the audit `evidence_hash`.
    pub evidence_hash: String,
}

/// The server-side facts the seal predicate consumes **alongside** the pool + audit replay + const, each
/// **derived server-side** (never from a request field): the resolved launcher and the revocation status.
#[derive(Debug, Clone, Copy)]
pub struct SealContext<'a> {
    /// The compiled `DEFLATION_BASIS_VERSION` (barrier 1).
    pub basis_version: u32,
    /// The launcher of the campaign that produced this pool, resolved via `pool_id → run → launch entry`
    /// (design carry-forward #1). **`None` is a BLOCK** — an unresolved launcher must never be passed to
    /// `derive_signoff` as `launcher = None` (which excludes NOBODY and would let the launcher self-approve).
    pub launcher: Option<&'a str>,
    /// Whether the pool is revoked on the live path (`revocations.json`) — a revoked pool cannot seal.
    pub revoked: bool,
}

/// The server-authoritative production-seal predicate (design §13.7). Pure over its inputs; the caller
/// resolves `ctx.launcher`/`ctx.revoked` server-side and passes the hash-verified `pool` + the audit replay.
#[must_use]
pub fn seal_allowed(
    pool: &FormulaPoolContent,
    audit_entries: &[AuditEntry],
    ctx: SealContext,
) -> SealDecision {
    let mut blockers = Vec::new();
    let evidence_hash = evidence_hash(pool);

    // ---- gate: production mode + const + not revoked -------------------------------------------------
    if pool.mode != qe_formula_pool::PoolMode::Production {
        blockers.push("not_production_mode".to_owned());
    }
    if !basis_satisfied(ctx.basis_version) {
        blockers.push("deflation_basis_prerequisites_unsatisfied".to_owned());
    }
    if ctx.revoked {
        blockers.push("pool_revoked".to_owned());
    }

    // ---- carry-forward #1: launcher must be RESOLVED (fail-closed) + dual sign-off -------------------
    let pool_hash = &pool.lineage.pool_hash;
    match ctx.launcher {
        None => blockers.push("launcher_unresolved".to_owned()),
        Some(launcher) => {
            // Approval is re-derived from `pool_hash`-bound `approve` events, EXCLUDING the launcher — a
            // mismatched `pool_hash` invalidates every signature (design §13.7/§13.8).
            match AuditLog::derive_signoff(audit_entries, pool_hash, Some(launcher)) {
                SignoffState::TwoDistinctSignoffs => {}
                _ => blockers.push("insufficient_distinct_approver_signoffs".to_owned()),
            }
        }
    }

    // ---- the eight §13.5 hard-blocks (every ABSENT stat is a block) ----------------------------------
    hard_blocks(pool, &mut blockers);

    SealDecision {
        allowed: blockers.is_empty(),
        blockers,
        evidence_hash,
    }
}

/// The eight §13.5 hard-blocks. Blocks 1–4 read the pool's `DeflationSummary`; blocks 5–8 read the optional
/// per-formula `gate_evidence` block (**absent ⇒ block**).
fn hard_blocks(pool: &FormulaPoolContent, blockers: &mut Vec<String>) {
    let d = &pool.deflation;

    // 1. gp_aware AND distinct_evaluations present AND > cells·gens·windows floor. N == floor ⇒ "blind floor".
    if !d.gp_aware {
        blockers.push("hb1_trial_basis_not_gp_aware".to_owned());
    }
    if d.distinct_evaluations <= d.analytic_floor {
        // `==` is the "QE-439 not wired" tell (the raw count never rose above the analytic floor).
        blockers.push("hb1_distinct_evaluations_at_or_below_analytic_floor".to_owned());
    }

    // 2. Finite E[maxSharpe] via the log-N path (a degenerate +∞ bug collapses the bar to 0 / non-positive).
    if d.expected_max_sharpe <= Decimal::ZERO {
        blockers.push("hb2_expected_max_sharpe_not_finite_positive".to_owned());
    }

    // 3. Uncensored PBO ≤ threshold (PRIMARY), over variance_trials ≥ distinct_evaluations (uncensored).
    match d.uncensored_pbo {
        None => blockers.push("hb3_uncensored_pbo_absent".to_owned()),
        Some(pbo) => {
            if pbo > dec(MAX_UNCENSORED_PBO) {
                blockers.push("hb3_uncensored_pbo_above_threshold".to_owned());
            }
        }
    }
    if d.variance_trials < d.distinct_evaluations {
        // A censored (top-N) dispersion population under-states overfitting (design §7 risk 2).
        blockers.push("hb3_pbo_population_censored".to_owned());
    }

    // 4. DSR ≥ 0.95 (necessary-not-sufficient floor).
    if d.champion_dsr < dec(MIN_DSR) {
        blockers.push("hb4_dsr_below_floor".to_owned());
    }

    // 5–8. Per-formula tradability/parsimony evidence — absent ⇒ block; each formula must have a matching
    // row that passes IC (5), cost-stress/turnover/capacity (6), MDL/caps/stratum (7), random-entry null (8).
    match &pool.gate_evidence {
        None => blockers.push("hb5_8_per_formula_evidence_absent".to_owned()),
        Some(evidence) => {
            for formula in &pool.formulas {
                match evidence
                    .iter()
                    .find(|e| e.formula_hash == formula.formula_hash)
                {
                    None => blockers.push(format!(
                        "hb5_8_per_formula_evidence_missing_for_{}",
                        formula.formula_hash
                    )),
                    Some(row) if !row.passes() => blockers.push(format!(
                        "hb5_8_per_formula_gate_failed_for_{}",
                        formula.formula_hash
                    )),
                    Some(_) => {}
                }
            }
        }
    }
}

/// SHA-256 over the canonical enforced stat set (design §13.5 "displayed = enforced = evidenced"): the
/// deflation summary + the per-formula evidence flags. Independent of wall-clock/order so the same pool
/// always evidences the same digest.
fn evidence_hash(pool: &FormulaPoolContent) -> String {
    let d = &pool.deflation;
    let mut s = format!(
        "gp_aware={};distinct={};floor={};variance_trials={};emax={};dsr={};pbo={};",
        d.gp_aware,
        d.distinct_evaluations,
        d.analytic_floor,
        d.variance_trials,
        d.expected_max_sharpe,
        d.champion_dsr,
        d.uncensored_pbo
            .map(|p| p.to_string())
            .unwrap_or_else(|| "absent".to_owned()),
    );
    match &pool.gate_evidence {
        None => s.push_str("evidence=absent;"),
        Some(rows) => {
            for r in rows {
                s.push_str(&format!("{}={};", r.formula_hash, r.passes()));
            }
        }
    }
    hex(&Sha256::digest(s.as_bytes()))
}

/// Build a `Decimal` from a `(mantissa, scale)` const pair.
fn dec((m, s): (i64, u32)) -> Decimal {
    Decimal::new(m, s)
}

/// Lowercase-hex encode.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_formula_pool::{
        DeflationSummary, FormulaGateEvidence, FormulaPool, FormulaPoolContent, PoolFormula,
        PoolLineage, PoolMode, POOL_FORMAT_VERSION,
    };

    const POOL_HASH: &str = "poolhash-1";
    const LAUNCHER: &str = "launcher@x.io";

    fn dec2(n: i64, scale: u32) -> Decimal {
        Decimal::new(n, scale)
    }

    /// SHA-256 hex of a string (a stand-in formula_hash of the right shape).
    fn h(s: &str) -> String {
        hex(&Sha256::digest(s.as_bytes()))
    }

    fn passing_deflation() -> DeflationSummary {
        DeflationSummary {
            gp_aware: true,
            distinct_evaluations: 500_000, // > floor
            n_trials: 500_000,
            analytic_floor: 7_200,
            variance_trials: 500_000, // ≥ distinct ⇒ uncensored
            trial_variance: dec2(2, 2),
            expected_max_sharpe: dec2(9, 0),  // finite > 0
            champion_dsr: dec2(98, 2),        // 0.98 ≥ 0.95
            uncensored_pbo: Some(dec2(2, 1)), // 0.20 ≤ 0.50
        }
    }

    fn good_evidence(formula_hash: &str) -> FormulaGateEvidence {
        FormulaGateEvidence {
            formula_hash: formula_hash.to_owned(),
            ic_two_fold_same_sign_fdr_pass: true,
            cost_stress_min_net_log_growth: dec2(5, 3),
            realised_turnover_frac: dec2(20, 2),
            capacity_usd: Decimal::from(300_000),
            within_caps_and_stratum_deflated: true,
            random_entry_null_pass: true,
        }
    }

    /// A fully-passing production pool: two sorted formulas, passing deflation + per-formula evidence.
    fn passing_pool() -> FormulaPoolContent {
        let mut formulas = vec![
            PoolFormula {
                sexpr: "rank(close,20)".to_owned(),
                formula_hash: h("rank(close,20)"),
            },
            PoolFormula {
                sexpr: "zscore(high,50)".to_owned(),
                formula_hash: h("zscore(high,50)"),
            },
        ];
        formulas.sort_by(|a, b| a.formula_hash.cmp(&b.formula_hash));
        let evidence = formulas
            .iter()
            .map(|f| good_evidence(&f.formula_hash))
            .collect();
        FormulaPoolContent {
            format_version: POOL_FORMAT_VERSION,
            pool_id: "campaign-1".to_owned(),
            mode: PoolMode::Production,
            formulas,
            deflation: passing_deflation(),
            gate_evidence: Some(evidence),
            lineage: PoolLineage {
                campaign_id: "campaign-1".to_owned(),
                seed: 7,
                mode: PoolMode::Production,
                code_commit: "commit".to_owned(),
                input_snapshot_id: "snap".to_owned(),
                config_hash: "cfg".to_owned(),
                pool_hash: POOL_HASH.to_owned(),
            },
        }
    }

    /// Two distinct approver signatures bound to `POOL_HASH`, neither the launcher — the happy-path audit.
    fn two_signoffs() -> Vec<AuditEntry> {
        // Build the entries through a real AuditLog so the chain/HMAC are consistent (derive_signoff only
        // reads action/subject_hash/actor, but constructing valid entries keeps this honest).
        let l = AuditLog::new(std::env::temp_dir().join("unused"), b"k".to_vec(), false);
        // We cannot append (async) here synchronously; build entries by hand — derive_signoff reads only
        // action/subject_hash/actor_email, so the hash fields are irrelevant to the predicate.
        let _ = l;
        vec![
            entry(0, LAUNCHER, crate::audit::AuditAction::Launch, "campaign-1"),
            entry(1, "a@x.io", crate::audit::AuditAction::Approve, POOL_HASH),
            entry(2, "b@x.io", crate::audit::AuditAction::Approve, POOL_HASH),
        ]
    }

    fn entry(
        seq: u64,
        actor: &str,
        action: crate::audit::AuditAction,
        subject: &str,
    ) -> AuditEntry {
        AuditEntry {
            seq,
            ts_ms: 1,
            actor_email: actor.to_owned(),
            action,
            subject_hash: subject.to_owned(),
            run_id: String::new(),
            vintage_id: String::new(),
            evidence_hash: String::new(),
            prev_hash: String::new(),
            entry_hash: String::new(),
            hmac: String::new(),
        }
    }

    fn ctx<'a>(launcher: Option<&'a str>, revoked: bool) -> SealContext<'a> {
        SealContext {
            basis_version: qe_validation::REQUIRED_DEFLATION_BASIS,
            launcher,
            revoked,
        }
    }

    #[test]
    fn a_genuinely_passing_production_pool_with_two_distinct_signoffs_seals() {
        let pool = passing_pool();
        let audit = two_signoffs();
        let decision = seal_allowed(&pool, &audit, ctx(Some(LAUNCHER), false));
        assert!(
            decision.allowed,
            "the happy path must seal; blockers: {:?}",
            decision.blockers
        );
        assert!(decision.blockers.is_empty());
        assert_eq!(decision.evidence_hash.len(), 64);
        // The pool must actually seal (verifies the content-hash discipline round-trips with evidence).
        assert!(FormulaPool::seal(pool).is_ok());
    }

    #[test]
    fn each_of_the_eight_hard_blocks_blocks_individually() {
        let audit = two_signoffs();
        let launcher = Some(LAUNCHER);

        // HB1a: not gp_aware.
        let mut p = passing_pool();
        p.deflation.gp_aware = false;
        assert_blocks(&p, &audit, launcher, "hb1_trial_basis_not_gp_aware");

        // HB1b: N == floor exactly ("QE-439 not wired" blind floor).
        let mut p = passing_pool();
        p.deflation.distinct_evaluations = p.deflation.analytic_floor;
        assert_blocks(
            &p,
            &audit,
            launcher,
            "hb1_distinct_evaluations_at_or_below_analytic_floor",
        );

        // HB2: degenerate E[maxSharpe] (the +∞ bug collapses the bar).
        let mut p = passing_pool();
        p.deflation.expected_max_sharpe = Decimal::ZERO;
        assert_blocks(
            &p,
            &audit,
            launcher,
            "hb2_expected_max_sharpe_not_finite_positive",
        );

        // HB3a: absent PBO.
        let mut p = passing_pool();
        p.deflation.uncensored_pbo = None;
        assert_blocks(&p, &audit, launcher, "hb3_uncensored_pbo_absent");

        // HB3b: PBO above threshold.
        let mut p = passing_pool();
        p.deflation.uncensored_pbo = Some(dec2(8, 1)); // 0.8 > 0.5
        assert_blocks(&p, &audit, launcher, "hb3_uncensored_pbo_above_threshold");

        // HB3c: censored population (variance_trials < distinct_evaluations).
        let mut p = passing_pool();
        p.deflation.variance_trials = p.deflation.distinct_evaluations - 1;
        assert_blocks(&p, &audit, launcher, "hb3_pbo_population_censored");

        // HB4: DSR below floor.
        let mut p = passing_pool();
        p.deflation.champion_dsr = dec2(5, 1); // 0.5 < 0.95
        assert_blocks(&p, &audit, launcher, "hb4_dsr_below_floor");

        // HB5-8a: per-formula evidence absent.
        let mut p = passing_pool();
        p.gate_evidence = None;
        assert_blocks(&p, &audit, launcher, "hb5_8_per_formula_evidence_absent");

        // HB5-8b: one formula's evidence fails a clause (e.g. scrapes noise / null fail).
        let mut p = passing_pool();
        if let Some(ev) = p.gate_evidence.as_mut() {
            ev[0].random_entry_null_pass = false;
        }
        let bad_hash = p.formulas[0].formula_hash.clone();
        assert_blocks(
            &p,
            &audit,
            launcher,
            &format!("hb5_8_per_formula_gate_failed_for_{bad_hash}"),
        );
    }

    #[test]
    fn launcher_as_approver_single_sig_and_pool_hash_mismatch_all_block() {
        let pool = passing_pool();

        // Launcher-as-approver: the launcher's own signature is excluded, leaving only one distinct approver.
        let audit_launcher_signs = vec![
            entry(0, LAUNCHER, crate::audit::AuditAction::Launch, "campaign-1"),
            entry(1, LAUNCHER, crate::audit::AuditAction::Approve, POOL_HASH),
            entry(2, "a@x.io", crate::audit::AuditAction::Approve, POOL_HASH),
        ];
        assert_blocks(
            &pool,
            &audit_launcher_signs,
            Some(LAUNCHER),
            "insufficient_distinct_approver_signoffs",
        );

        // Single signature.
        let single = vec![
            entry(0, LAUNCHER, crate::audit::AuditAction::Launch, "campaign-1"),
            entry(1, "a@x.io", crate::audit::AuditAction::Approve, POOL_HASH),
        ];
        assert_blocks(
            &pool,
            &single,
            Some(LAUNCHER),
            "insufficient_distinct_approver_signoffs",
        );

        // pool_hash mismatch: two signatures bound to a DIFFERENT hash don't count against this pool.
        let wrong_hash = vec![
            entry(0, LAUNCHER, crate::audit::AuditAction::Launch, "campaign-1"),
            entry(
                1,
                "a@x.io",
                crate::audit::AuditAction::Approve,
                "other-hash",
            ),
            entry(
                2,
                "b@x.io",
                crate::audit::AuditAction::Approve,
                "other-hash",
            ),
        ];
        assert_blocks(
            &pool,
            &wrong_hash,
            Some(LAUNCHER),
            "insufficient_distinct_approver_signoffs",
        );
    }

    #[test]
    fn an_unresolved_launcher_is_fail_closed() {
        // carry-forward #1: launcher=None must be a BLOCK, never passed to derive_signoff (which would
        // exclude NOBODY and let the launcher self-approve).
        let pool = passing_pool();
        let audit = two_signoffs();
        assert_blocks(&pool, &audit, None, "launcher_unresolved");
    }

    #[test]
    fn a_sandbox_pool_and_a_revoked_pool_and_an_unsatisfied_basis_each_block() {
        let audit = two_signoffs();

        let mut sandbox = passing_pool();
        sandbox.mode = PoolMode::Sandbox;
        assert_blocks(&sandbox, &audit, Some(LAUNCHER), "not_production_mode");

        let pool = passing_pool();
        let d = seal_allowed(&pool, &audit, ctx(Some(LAUNCHER), true));
        assert!(!d.allowed && d.blockers.contains(&"pool_revoked".to_owned()));

        let unsat = SealContext {
            basis_version: 0,
            launcher: Some(LAUNCHER),
            revoked: false,
        };
        let d = seal_allowed(&pool, &audit, unsat);
        assert!(d
            .blockers
            .contains(&"deflation_basis_prerequisites_unsatisfied".to_owned()));
    }

    #[test]
    fn evidence_hash_is_stable_and_changes_with_the_enforced_stats() {
        let pool = passing_pool();
        let h1 = evidence_hash(&pool);
        let h2 = evidence_hash(&pool);
        assert_eq!(h1, h2, "deterministic evidence digest");
        let mut mutated = passing_pool();
        mutated.deflation.champion_dsr = dec2(97, 2);
        assert_ne!(
            evidence_hash(&mutated),
            h1,
            "a different enforced stat ⇒ a different digest"
        );
    }

    /// Assert the pool is blocked and the named blocker appears (non-vacuous: it must not be allowed).
    fn assert_blocks(
        pool: &FormulaPoolContent,
        audit: &[AuditEntry],
        launcher: Option<&str>,
        needle: &str,
    ) {
        let d = seal_allowed(pool, audit, ctx(launcher, false));
        assert!(
            !d.allowed,
            "expected a block for `{needle}` but the pool sealed"
        );
        assert!(
            d.blockers.iter().any(|b| b == needle),
            "expected blocker `{needle}`, got {:?}",
            d.blockers
        );
    }
}
