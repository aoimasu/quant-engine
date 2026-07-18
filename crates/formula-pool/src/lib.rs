//! qe-formula-pool (QE-452 Phase A) — the frozen **formula-pool** artefact format.
//!
//! A **formula pool** is the output of the offline GP `evolve` job (QE-450 §13.2/§13.3): the `K ≤ 16`
//! canonical S-expression formulas an illumination campaign froze, a **deflation-summary block** (the
//! GP-aware trial basis + DSR/PBO/`E[maxSR]` diagnostics), and its **review lineage** (campaign id, seed,
//! mode, code commit, pinned input snapshot). It is a **separate resource** from a
//! [`qe_vintage::Vintage`](../qe_vintage/index.html): different content shape, a different (human-paced,
//! revocable) governance lifecycle, and **runtime never loads a pool** — so this is a dedicated crate, not
//! a `qe-vintage` variant, and pool artefacts live under a **separate directory root**.
//!
//! It **reuses `Vintage`'s seal/verify/load SHA-256 discipline verbatim**: [`FormulaPool::seal`] validates
//! then pins a lowercase-hex SHA-256 over the canonical JSON; [`FormulaPool::verify`] recomputes and
//! compares; [`FormulaPool::load`] **verifies before returning**, so a tampered pool is rejected at load
//! exactly like the QE-451 Phase-1b tamper-load test. Every **hashed** numeric field is a
//! [`rust_decimal::Decimal`] serialised as a string, so seal → load is byte-stable by construction (no
//! `f64` re-parse instability, no `hash_stable` rounding dance).
//!
//! **Firewall.** A pure serde leaf — deps are `serde`/`serde_json`/`sha2`/`thiserror`/`rust_decimal`, **no
//! `qe-*` crate** — so it cannot reach `qe-runtime`/`qe-venue` (asserted by `qe-architecture`'s firewall).

use std::io::{Read, Write};
use std::path::PathBuf;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub mod governance_record;
pub mod lifecycle;

pub use governance_record::{GovernanceRecord, RevocationRecord, Revocations};
pub use lifecycle::{
    LifecycleError, PoolGovernance, PoolGovernanceStore, PoolLifecycleState, PoolTransition,
    TransitionRecord,
};

/// The pool artefact format version. Part of the hashed content, so a format change changes the hash.
pub const POOL_FORMAT_VERSION: u16 = 1;

/// The frozen-pool cap `K` (design §3/§9; mirrors `qe_wfo::gp::freeze::MAX_POOL_SIZE`): at most 16
/// evolved formulas may be sealed into a pool.
pub const MAX_POOL_SIZE: usize = 16;

/// The campaign mode a pool was produced under (design §13.6). Mirrors `qe_run_protocol::EvolveMode`
/// (kept as a plain leaf enum so this crate stays `qe-*`-dep-free). Sealed into the hashed content and
/// recorded in the lineage so a sandbox pool is content-addressably distinct from a production one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolMode {
    /// Research mode — written to a separate research root; never on the production load path.
    #[default]
    Sandbox,
    /// Production mode (only launchable once the QE-454 prerequisite gate is satisfied — not Phase A).
    Production,
}

/// One frozen formula: its exact canonical S-expression and the `formula_hash` (SHA-256 over that
/// S-expression). Mirrors `qe_wfo::gp::freeze::FrozenFormula` as pure data (this crate does not depend on
/// `qe-wfo`; the `evolve` CLI job maps the frozen formulas into these).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolFormula {
    /// The exact canonical S-expression text (human-readable, `rust_decimal`-only).
    pub sexpr: String,
    /// SHA-256 over `sexpr` — the content-addressed `formula_hash` (64 lowercase hex chars).
    pub formula_hash: String,
}

/// The deflation-summary block (design §5/§13.5): the minimum honest stat set that gates a later seal.
/// Every numeric bar is exact [`Decimal`] (serialised as a string) so it is part of a byte-stable hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeflationSummary {
    /// Whether the trial basis came from the real GP-aware trial-counter path (QE-439). `false` ⇒ the
    /// blind analytic floor was used (a later production seal must block on this — QE-454).
    pub gp_aware: bool,
    /// Distinct-canonical formulas ever scored (incl. all rejects) — QE-439's trial basis.
    pub distinct_evaluations: u64,
    /// The GP-aware trial basis `N` (= `max(distinct, analytic floor)`) the DSR deflated against.
    pub n_trials: u64,
    /// The analytic `cells·gens·windows` floor (so `N == floor` — "QE-439 not wired" — is visible).
    pub analytic_floor: u64,
    /// Size of the uncensored Sharpe-dispersion population.
    pub variance_trials: u64,
    /// Cross-trial Sharpe **variance** over the uncensored population (sets the deflation bar).
    #[serde(with = "rust_decimal::serde::str")]
    pub trial_variance: Decimal,
    /// The best-of-`N` noise Sharpe bar `E[max SR]` (finite via QE-439's log-N path).
    #[serde(with = "rust_decimal::serde::str")]
    pub expected_max_sharpe: Decimal,
    /// The champion's Deflated Sharpe Ratio (necessary-not-sufficient floor).
    #[serde(with = "rust_decimal::serde::str")]
    pub champion_dsr: Decimal,
    /// **Uncensored PBO** over the full evaluated population (the primary GP gate). `None` if the
    /// population was too small / short to estimate — an *absent* PBO is a later hard-block (QE-454).
    #[serde(with = "rust_decimal::serde::str_option", default)]
    pub uncensored_pbo: Option<Decimal>,
}

/// The realised-turnover ceiling as a fraction of `n_bars` a production formula must stay under (design
/// §13.5 hard-block 6; `max_turnover_frac = 0.25`).
pub const MAX_TURNOVER_FRAC: &str = "0.25";
/// The per-formula capacity floor (USD) a production formula must clear (design §13.5 hard-block 6,
/// `CAPACITY_FLOOR ≈ $250k`).
pub const CAPACITY_FLOOR_USD: i64 = 250_000;

/// Per-formula **tradability + parsimony evidence** for the §13.5 hard-blocks 5–8 that `seal_allowed`
/// re-derives (QE-454 Phase B). This is the *displayed = enforced = evidenced* per-formula row: the exact
/// numbers the SPA PoolReview shows, the seal predicate enforces, and the audit `evidence_hash` captures.
///
/// It is carried in an **optional** [`FormulaPoolContent::gate_evidence`] block (absent-by-default), so a
/// pool sealed without it (every pre-Phase-B / format-v1 pool) serialises **byte-identically** — and an
/// **absent** evidence block is a *hard-block* at seal time (every absent stat blocks, never a vacuous
/// pass, design §13.5). Every numeric bar is exact [`Decimal`] (string-serialised) so it is byte-stable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaGateEvidence {
    /// The `formula_hash` this evidence row belongs to (binds the row to its formula; a mismatch blocks).
    pub formula_hash: String,
    /// Hard-block 5 (QE-434): the formula's rank-IC is same-sign across the two folds **and** clears the
    /// Benjamini-Hochberg FDR screen.
    pub ic_two_fold_same_sign_fdr_pass: bool,
    /// Hard-block 6: the **minimum** over the `{1×, 2×}` cost-stress of the net log-growth (must be finite
    /// and `> 0`).
    #[serde(with = "rust_decimal::serde::str")]
    pub cost_stress_min_net_log_growth: Decimal,
    /// Hard-block 6: realised turnover as a fraction of `n_bars` (must be `≤ 0.25`).
    #[serde(with = "rust_decimal::serde::str")]
    pub realised_turnover_frac: Decimal,
    /// Hard-block 6: estimated capacity in USD (must be `≥ 250_000`).
    #[serde(with = "rust_decimal::serde::str")]
    pub capacity_usd: Decimal,
    /// Hard-block 7 (QE-436): the formula is within the MDL / node-count / depth / lookback caps **and**
    /// clears deflation against its own node-count stratum.
    pub within_caps_and_stratum_deflated: bool,
    /// Hard-block 8 (`nulls.rs`): the formula beats its turnover-matched random-entry null (does not
    /// "SCRAPE NOISE").
    pub random_entry_null_pass: bool,
}

impl FormulaGateEvidence {
    /// Whether this formula clears **all** of hard-blocks 5–8 (design §13.5). Every clause must pass;
    /// a non-finite / non-positive cost-stress growth, an over-turnover, or a sub-floor capacity blocks.
    #[must_use]
    pub fn passes(&self) -> bool {
        let max_turnover = Decimal::from_str_exact(MAX_TURNOVER_FRAC).unwrap_or(Decimal::ZERO);
        let capacity_floor = Decimal::from(CAPACITY_FLOOR_USD);
        self.ic_two_fold_same_sign_fdr_pass
            && self.cost_stress_min_net_log_growth > Decimal::ZERO
            && self.realised_turnover_frac <= max_turnover
            && self.capacity_usd >= capacity_floor
            && self.within_caps_and_stratum_deflated
            && self.random_entry_null_pass
    }
}

/// The pool's **review lineage** (design §13.10): the reproducible provenance that binds an approval to a
/// byte-reproducible pool. Plain strings + the seed keep this a leaf (no `qe-determinism` dep).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolLineage {
    /// The campaign identity (the deterministic lineage id the `evolve` job derived).
    pub campaign_id: String,
    /// The master illumination seed (**required** on an evolve run — reproducibility).
    pub seed: u64,
    /// The campaign mode.
    pub mode: PoolMode,
    /// Build code provenance (the git sha / sentinel folded into the campaign id).
    pub code_commit: String,
    /// The pinned market-snapshot id (empty until the ingest snapshot seam lands).
    pub input_snapshot_id: String,
    /// The config hash the campaign ran under.
    pub config_hash: String,
    /// A single content address over the sorted `formula_hash` list (audit/lineage join key) —
    /// `qe_wfo::gp::freeze::FrozenPool::pool_hash` computes the same value.
    pub pool_hash: String,
}

/// The hashed content of a formula pool — everything the content hash covers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaPoolContent {
    /// Artefact format version ([`POOL_FORMAT_VERSION`]).
    pub format_version: u16,
    /// The pool id (the campaign's deterministic lineage id; the on-disk filename stem).
    pub pool_id: String,
    /// The campaign mode (sandbox / production).
    pub mode: PoolMode,
    /// The `K ≤ 16` frozen formulas, **strictly ascending by `formula_hash`** (sorted + deduplicated).
    pub formulas: Vec<PoolFormula>,
    /// The deflation-summary block.
    pub deflation: DeflationSummary,
    /// **Optional** per-formula tradability + parsimony evidence (design §13.5 hard-blocks 5–8, QE-454
    /// Phase B). Absent-by-default (`skip_serializing_if`), so a pool sealed without it serialises
    /// byte-identically to a pre-Phase-B/format-v1 pool; an **absent** block is a hard-block at seal
    /// time (every absent stat blocks). When present, it carries one row per formula.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_evidence: Option<Vec<FormulaGateEvidence>>,
    /// The review lineage.
    pub lineage: PoolLineage,
}

impl FormulaPoolContent {
    /// Validate the artefact's structural invariants: `K ≤ 16`; every `formula_hash` is 64 lowercase-hex
    /// chars; and the formulas are **strictly ascending by `formula_hash`** (the freeze's sorted +
    /// deduplicated identity — a reorder or a duplicate is rejected). Called by [`FormulaPool::seal`].
    ///
    /// # Errors
    /// [`PoolError::TooLarge`], [`PoolError::InvalidFormulaHash`], or [`PoolError::FormulasNotSorted`].
    pub fn validate(&self) -> Result<(), PoolError> {
        if self.formulas.len() > MAX_POOL_SIZE {
            return Err(PoolError::TooLarge {
                offered: self.formulas.len(),
            });
        }
        for (index, f) in self.formulas.iter().enumerate() {
            if f.formula_hash.len() != 64 || !f.formula_hash.chars().all(|c| c.is_ascii_hexdigit())
            {
                return Err(PoolError::InvalidFormulaHash { index });
            }
        }
        for pair in self.formulas.windows(2) {
            if pair[0].formula_hash >= pair[1].formula_hash {
                return Err(PoolError::FormulasNotSorted);
            }
        }
        Ok(())
    }

    /// Lowercase-hex SHA-256 over the record's canonical JSON — the **content hash** (the exact discipline
    /// of [`qe_vintage::VintageContent::content_hash`]). Stable because every embedded type serialises
    /// deterministically (fixed field order; no `HashMap`/`HashSet`; every numeric bar a string-serialised
    /// `Decimal`), so there is no `f64` re-parse hazard.
    ///
    /// # Errors
    /// [`PoolError::Serialize`] if the content cannot be serialised.
    pub fn content_hash(&self) -> Result<String, PoolError> {
        let bytes = serde_json::to_vec(self).map_err(|e| PoolError::Serialize(e.to_string()))?;
        Ok(hex(&Sha256::digest(&bytes)))
    }

    /// **Structural barrier 3** (design §13.6): assert this pool is eligible to load on the **production**
    /// path, reusing the exact-match-on-a-hashed-identity-field discipline of
    /// [`qe_vintage::schema::assert_schema`]. The pool's `mode` is a **hashed** content field, so a
    /// sandbox-identity pool copied into the production directory still verifies its `content_hash` (it is
    /// a real pool) but fails here — its sealed `mode == Sandbox` cannot be flipped without breaking the
    /// hash. Fail-closed: any non-production mode is refused. The production repository load path calls
    /// this, so a sandbox pool is **structurally unloadable** in production even if the file is copied in.
    ///
    /// # Errors
    /// [`PoolError::NotProductionEligible`] when `mode != Production`.
    pub fn assert_production_eligible(&self) -> Result<(), PoolError> {
        if self.mode != PoolMode::Production {
            return Err(PoolError::NotProductionEligible { mode: self.mode });
        }
        Ok(())
    }

    /// The sorted `formula_hash` list (the identity payload; equals `lineage.pool_hash`'s preimage).
    #[must_use]
    pub fn formula_hashes(&self) -> Vec<String> {
        self.formulas
            .iter()
            .map(|f| f.formula_hash.clone())
            .collect()
    }
}

/// A sealed formula-pool artefact: its [`FormulaPoolContent`] plus the content hash that pins it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaPool {
    /// The hashed content.
    pub content: FormulaPoolContent,
    /// The content hash computed at [`seal`](FormulaPool::seal) time.
    pub content_hash: String,
}

impl FormulaPool {
    /// Seal `content` by [validating](FormulaPoolContent::validate) its invariants, then computing and
    /// pinning its content hash (mirrors [`qe_vintage::Vintage::seal`]).
    ///
    /// # Errors
    /// [`FormulaPoolContent::validate`] errors, or a serialisation failure from
    /// [`FormulaPoolContent::content_hash`].
    pub fn seal(content: FormulaPoolContent) -> Result<Self, PoolError> {
        content.validate()?;
        let content_hash = content.content_hash()?;
        Ok(FormulaPool {
            content,
            content_hash,
        })
    }

    /// Verify the stored hash matches a freshly recomputed one — detects any post-seal tampering.
    ///
    /// # Errors
    /// [`PoolError::HashMismatch`] if the stored hash does not match, or a serialisation failure.
    pub fn verify(&self) -> Result<(), PoolError> {
        let recomputed = self.content.content_hash()?;
        if recomputed != self.content_hash {
            return Err(PoolError::HashMismatch {
                stored: self.content_hash.clone(),
                recomputed,
            });
        }
        Ok(())
    }

    /// Serialise the sealed artefact as JSON to `w`.
    ///
    /// # Errors
    /// [`PoolError::Serialize`] / [`PoolError::Io`] on failure.
    pub fn write<W: Write>(&self, w: &mut W) -> Result<(), PoolError> {
        let bytes = serde_json::to_vec(self).map_err(|e| PoolError::Serialize(e.to_string()))?;
        w.write_all(&bytes)?;
        Ok(())
    }

    /// Load a sealed artefact from a JSON reader, **verifying the content hash** before returning — a load
    /// never yields an unverified pool (the exact [`qe_vintage::Vintage::load`] rule).
    ///
    /// # Errors
    /// [`PoolError::Deserialize`] / [`PoolError::Io`] on read failure, [`PoolError::HashMismatch`] if the
    /// content hash does not verify.
    pub fn load<R: Read>(r: R) -> Result<Self, PoolError> {
        let pool: FormulaPool =
            serde_json::from_reader(r).map_err(|e| PoolError::Deserialize(e.to_string()))?;
        pool.verify()?;
        Ok(pool)
    }
}

/// A directory-backed store of formula pools under a **separate root** from the vintage repository
/// (design §13.2/§13.6): one `<root>/<pool_id>.json` per pool. Read paths (the future Phase-B server) open
/// it; runtime never does.
#[derive(Debug, Clone)]
pub struct FormulaPoolRepository {
    root: PathBuf,
}

impl FormulaPoolRepository {
    /// A repository rooted at `root` (created on first [`write`](FormulaPoolRepository::write)).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FormulaPoolRepository { root: root.into() }
    }

    /// The repository root (a directory **separate** from the vintage repository root).
    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// The on-disk path for `pool_id`.
    #[must_use]
    pub fn path_for(&self, pool_id: &str) -> PathBuf {
        self.root.join(format!("{pool_id}.json"))
    }

    /// Write `pool` to `<root>/<pool_id>.json`, creating `root` if needed. Returns the path.
    ///
    /// # Errors
    /// [`PoolError::Io`] / [`PoolError::Serialize`] on failure.
    pub fn write(&self, pool: &FormulaPool) -> Result<PathBuf, PoolError> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.path_for(&pool.content.pool_id);
        let mut file = std::fs::File::create(&path)?;
        pool.write(&mut file)?;
        Ok(path)
    }

    /// Load and verify the pool `pool_id` from disk.
    ///
    /// # Errors
    /// [`PoolError::Io`] if the file is missing/unreadable, plus the [`FormulaPool::load`] errors.
    pub fn load(&self, pool_id: &str) -> Result<FormulaPool, PoolError> {
        let file = std::fs::File::open(self.path_for(pool_id))?;
        FormulaPool::load(file)
    }

    /// List every sealed pool under `root`, **ascending by `pool_id`** (deterministic order). Each
    /// `*.json` is loaded through [`FormulaPool::load`] (so the content hash is verified); files that
    /// don't parse/verify are skipped; a missing `root` yields an empty list.
    ///
    /// # Errors
    /// [`PoolError::Io`] on a filesystem error reading the directory (other than "not found").
    pub fn list(&self) -> Result<Vec<FormulaPool>, PoolError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(PoolError::Io(e)),
        };
        let mut pools = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(file) = std::fs::File::open(&path) {
                if let Ok(pool) = FormulaPool::load(file) {
                    pools.push(pool);
                }
            }
        }
        pools.sort_by(|a, b| a.content.pool_id.cmp(&b.content.pool_id));
        Ok(pools)
    }
}

/// Errors raised while sealing / writing / loading a formula pool.
#[derive(Debug, Error)]
pub enum PoolError {
    /// The artefact could not be serialised.
    #[error("failed to serialise formula pool: {0}")]
    Serialize(String),
    /// The artefact could not be deserialised.
    #[error("failed to deserialise formula pool: {0}")]
    Deserialize(String),
    /// The content hash did not verify (tampered or corrupted artefact).
    #[error("formula pool content hash mismatch: stored {stored}, recomputed {recomputed}")]
    HashMismatch {
        /// The hash stored in the artefact.
        stored: String,
        /// The hash recomputed from the content.
        recomputed: String,
    },
    /// More than [`MAX_POOL_SIZE`] formulas were offered (`K ≤ 16`).
    #[error("formula pool exceeds K ≤ {MAX_POOL_SIZE}: {offered} formulas offered")]
    TooLarge {
        /// The formula count offered.
        offered: usize,
    },
    /// A `formula_hash` is not 64 lowercase-hex chars (a valid SHA-256).
    #[error("formula #{index} has a malformed formula_hash (expected 64 hex chars)")]
    InvalidFormulaHash {
        /// The offending formula index.
        index: usize,
    },
    /// The formulas are not strictly ascending by `formula_hash` (unsorted or a duplicate).
    #[error(
        "formula pool formulas must be strictly ascending by formula_hash (sorted + deduplicated)"
    )]
    FormulasNotSorted,
    /// The pool is not eligible to load on the production path — its sealed `mode` is not `Production`
    /// (structural barrier 3, design §13.6). A sandbox pool copied into the prod dir fails here.
    #[error(
        "formula pool is not production-eligible: sealed mode is {mode:?}, expected Production"
    )]
    NotProductionEligible {
        /// The pool's sealed (hashed) mode.
        mode: PoolMode,
    },
    /// Underlying I/O error.
    #[error("formula pool I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Lowercase-hex encoding of a byte slice.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(n: i64, scale: u32) -> Decimal {
        Decimal::new(n, scale)
    }

    /// SHA-256 of `s` as lowercase hex — a stand-in for `ExprTree::canonical_hash` (this leaf crate has no
    /// `qe-wfo`; the real job hands in the frozen formulas' actual hashes).
    fn hash_of(s: &str) -> String {
        hex(&Sha256::digest(s.as_bytes()))
    }

    fn formula(sexpr: &str) -> PoolFormula {
        PoolFormula {
            sexpr: sexpr.to_owned(),
            formula_hash: hash_of(sexpr),
        }
    }

    /// A sorted+deduplicated formula list (the freeze contract) built from distinct S-expressions.
    fn sorted_formulas(sexprs: &[&str]) -> Vec<PoolFormula> {
        let mut fs: Vec<PoolFormula> = sexprs.iter().map(|s| formula(s)).collect();
        fs.sort_by(|a, b| a.formula_hash.cmp(&b.formula_hash));
        fs.dedup_by(|a, b| a.formula_hash == b.formula_hash);
        fs
    }

    fn deflation() -> DeflationSummary {
        DeflationSummary {
            gp_aware: true,
            distinct_evaluations: 192,
            n_trials: 200,
            analytic_floor: 90,
            variance_trials: 45,
            trial_variance: dec(1234, 4),     // 0.1234
            expected_max_sharpe: dec(21, 1),  // 2.1
            champion_dsr: dec(97, 2),         // 0.97
            uncensored_pbo: Some(dec(42, 2)), // 0.42
        }
    }

    fn lineage(formulas: &[PoolFormula]) -> PoolLineage {
        // A stand-in pool_hash over the sorted formula hashes (the job uses FrozenPool::pool_hash).
        let mut hasher = Sha256::new();
        for f in formulas {
            hasher.update(f.formula_hash.as_bytes());
            hasher.update(b"\n");
        }
        PoolLineage {
            campaign_id: "campaign-abc".to_owned(),
            seed: 20_260_718,
            mode: PoolMode::Sandbox,
            code_commit: "commit-deadbeef".to_owned(),
            input_snapshot_id: String::new(),
            config_hash: "cfg-hash".to_owned(),
            pool_hash: hex(&hasher.finalize()),
        }
    }

    fn content(sexprs: &[&str]) -> FormulaPoolContent {
        let formulas = sorted_formulas(sexprs);
        let lineage = lineage(&formulas);
        FormulaPoolContent {
            format_version: POOL_FORMAT_VERSION,
            pool_id: "campaign-abc".to_owned(),
            mode: PoolMode::Sandbox,
            formulas,
            deflation: deflation(),
            gate_evidence: None,
            lineage,
        }
    }

    #[test]
    fn round_trips_with_stable_verifiable_hash() {
        let sealed = FormulaPool::seal(content(&["rank(close,20)", "zscore(high,50)"])).unwrap();

        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        let loaded = FormulaPool::load(buf.as_slice()).unwrap();
        assert_eq!(loaded, sealed);
        assert_eq!(loaded.content_hash, sealed.content_hash);

        // Re-sealing the same content yields the same hash (deterministic content-address).
        let resealed = FormulaPool::seal(content(&["rank(close,20)", "zscore(high,50)"])).unwrap();
        assert_eq!(resealed.content_hash, sealed.content_hash);
        assert_eq!(sealed.content_hash.len(), 64);
        assert!(sealed.content_hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn tampering_with_content_fails_verification_and_load() {
        // Mirrors the QE-451 Phase-1b tamper-load test: a mutated pool no longer verifies, and a load of
        // the tampered bytes is rejected before any unverified pool is returned.
        let mut sealed =
            FormulaPool::seal(content(&["rank(close,20)", "zscore(high,50)"])).unwrap();
        sealed.content.deflation.champion_dsr = dec(1, 0); // 1.0 — a rosier stat than sealed
        assert!(matches!(
            sealed.verify(),
            Err(PoolError::HashMismatch { .. })
        ));

        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        assert!(matches!(
            FormulaPool::load(buf.as_slice()),
            Err(PoolError::HashMismatch { .. })
        ));
    }

    #[test]
    fn seal_enforces_the_k_le_16_cap() {
        let many: Vec<String> = (0..17).map(|i| format!("f{i}(close,20)")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let c = content(&refs);
        assert_eq!(c.formulas.len(), 17);
        assert!(matches!(
            FormulaPool::seal(c),
            Err(PoolError::TooLarge { offered: 17 })
        ));
        // Exactly 16 seals fine.
        let ok = content(&refs[..16]);
        assert_eq!(FormulaPool::seal(ok).unwrap().content.formulas.len(), 16);
    }

    #[test]
    fn seal_rejects_unsorted_or_duplicate_formulas() {
        let mut c = content(&["rank(close,20)", "zscore(high,50)"]);
        c.formulas.reverse(); // descending ⇒ not strictly ascending
        assert!(matches!(
            FormulaPool::seal(c),
            Err(PoolError::FormulasNotSorted)
        ));

        // A duplicate formula_hash is also rejected (not strictly ascending).
        let mut dup = content(&["rank(close,20)"]);
        let f = dup.formulas[0].clone();
        dup.formulas.push(f);
        // Re-sort so the two identical hashes are adjacent-equal (fails the strict-ascending check).
        dup.formulas
            .sort_by(|a, b| a.formula_hash.cmp(&b.formula_hash));
        assert!(matches!(
            FormulaPool::seal(dup),
            Err(PoolError::FormulasNotSorted)
        ));
    }

    #[test]
    fn seal_rejects_a_malformed_formula_hash() {
        let mut c = content(&["rank(close,20)"]);
        c.formulas[0].formula_hash = "not-a-real-hash".to_owned();
        assert!(matches!(
            FormulaPool::seal(c),
            Err(PoolError::InvalidFormulaHash { index: 0 })
        ));
    }

    #[test]
    fn decimal_stats_round_trip_byte_stable() {
        // A many-digit stat serialises as a string and re-loads exactly (no f64 re-parse hazard).
        let mut c = content(&["rank(close,20)"]);
        c.deflation.trial_variance = Decimal::from_str_exact("0.123456789012345").unwrap();
        let sealed = FormulaPool::seal(c).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        let loaded = FormulaPool::load(buf.as_slice()).unwrap();
        assert_eq!(
            loaded.content.deflation.trial_variance,
            sealed.content.deflation.trial_variance
        );
        assert_eq!(loaded.content_hash, sealed.content_hash);
    }

    fn good_evidence(formula_hash: &str) -> FormulaGateEvidence {
        FormulaGateEvidence {
            formula_hash: formula_hash.to_owned(),
            ic_two_fold_same_sign_fdr_pass: true,
            cost_stress_min_net_log_growth: dec(5, 3), // 0.005 > 0
            realised_turnover_frac: dec(20, 2),        // 0.20 ≤ 0.25
            capacity_usd: Decimal::from(300_000),      // ≥ 250k
            within_caps_and_stratum_deflated: true,
            random_entry_null_pass: true,
        }
    }

    #[test]
    fn assert_production_eligible_is_fail_closed_on_a_sandbox_pool() {
        // A sandbox pool (default) is refused; a production pool passes. The mode is a HASHED field, so a
        // copied sandbox pool cannot masquerade as production without breaking its content hash.
        let mut c = content(&["rank(close,20)"]);
        assert!(matches!(
            c.assert_production_eligible(),
            Err(PoolError::NotProductionEligible {
                mode: PoolMode::Sandbox
            })
        ));
        c.mode = PoolMode::Production;
        assert!(c.assert_production_eligible().is_ok());
    }

    #[test]
    fn absent_gate_evidence_serialises_byte_identically() {
        // Golden-safety: a pool with `gate_evidence: None` must serialise WITHOUT the key, so a
        // pre-Phase-B/format-v1 pool is byte-identical and its content hash is unchanged.
        let sealed = FormulaPool::seal(content(&["rank(close,20)", "zscore(high,50)"])).unwrap();
        let json = serde_json::to_string(&sealed).unwrap();
        assert!(
            !json.contains("gate_evidence"),
            "absent gate_evidence must not appear on the wire (byte-stability): {json}"
        );
        // Round-trips back with the field absent.
        let back: FormulaPool = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content.gate_evidence, None);
        assert_eq!(back.content_hash, sealed.content_hash);
    }

    #[test]
    fn formula_gate_evidence_passes_and_each_clause_blocks_alone() {
        let base = good_evidence("abc");
        assert!(base.passes(), "the all-green evidence passes");
        // Flip each clause bad → the formula blocks.
        assert!(!FormulaGateEvidence {
            ic_two_fold_same_sign_fdr_pass: false,
            ..base.clone()
        }
        .passes());
        assert!(!FormulaGateEvidence {
            cost_stress_min_net_log_growth: dec(0, 0), // not > 0
            ..base.clone()
        }
        .passes());
        assert!(!FormulaGateEvidence {
            realised_turnover_frac: dec(26, 2), // 0.26 > 0.25
            ..base.clone()
        }
        .passes());
        assert!(!FormulaGateEvidence {
            capacity_usd: Decimal::from(249_999), // < 250k
            ..base.clone()
        }
        .passes());
        assert!(!FormulaGateEvidence {
            within_caps_and_stratum_deflated: false,
            ..base.clone()
        }
        .passes());
        assert!(!FormulaGateEvidence {
            random_entry_null_pass: false,
            ..base
        }
        .passes());
    }

    #[test]
    fn repository_round_trips_under_a_separate_root() {
        let dir = tempfile::tempdir().unwrap();
        let repo = FormulaPoolRepository::new(dir.path().join("pools"));
        assert!(repo.list().unwrap().is_empty()); // missing root ⇒ empty

        let sealed = FormulaPool::seal(content(&["rank(close,20)", "zscore(high,50)"])).unwrap();
        let path = repo.write(&sealed).unwrap();
        assert!(path.exists());
        // The pool root is separate from any vintage root.
        assert!(path.starts_with(dir.path().join("pools")));

        let loaded = repo.load(&sealed.content.pool_id).unwrap();
        assert_eq!(loaded, sealed);
        assert_eq!(repo.list().unwrap().len(), 1);
    }
}
