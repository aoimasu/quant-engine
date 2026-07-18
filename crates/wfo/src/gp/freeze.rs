//! QE-451 **Phase 1b** — freeze `K ≤ 16` evolved trees into a content-addressed formula pool
//! (QE-450 §3, §6, §9 item 8). **Default-off machinery** exercised by tests; nothing here is wired into
//! the default `train` vintage, so no golden moves and the default vintage stays byte-identical.
//!
//! The freeze is the last Phase-1b stage: after the deflation gate + tradability gates admit survivors,
//! `K ≤ 16` trees are sealed into a [`FrozenPool`]. Each formula carries its exact **canonical
//! S-expression** and its `formula_hash` = SHA-256 over that S-expression (reusing Phase 1a
//! [`ExprTree::canonical_hash`] — `rust_decimal`-only, no `f64`). The pool's `formula_hash` list becomes a
//! [`CatalogueIdentity`] field (default-empty, byte-identical when absent), and the vintage load boundary
//! asserts an **exact** identity match. The freeze crosses into the vintage as sealed **DATA** (hash
//! strings), never a `qe-wfo → qe-vintage/qe-ensemble` code edge (firewall §6).

use qe_signal::indicator::expr::ExprTree;
use qe_signal::CatalogueIdentity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The frozen-pool cap (design §3, §9): at most 16 evolved trees enter the catalogue.
pub const MAX_POOL_SIZE: usize = 16;

/// Errors from freezing a pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreezeError {
    /// More than [`MAX_POOL_SIZE`] distinct formulas were offered.
    TooLarge {
        /// The distinct-formula count offered.
        offered: usize,
    },
}

impl std::fmt::Display for FreezeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FreezeError::TooLarge { offered } => write!(
                f,
                "frozen pool exceeds K ≤ {MAX_POOL_SIZE}: {offered} distinct formulas offered"
            ),
        }
    }
}

impl std::error::Error for FreezeError {}

/// One frozen formula: its exact canonical S-expression and the `formula_hash` (SHA-256 over that
/// S-expression). `serde` so the pool artefact persists (the sealed pool is part of the vintage lineage).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenFormula {
    /// The exact canonical S-expression text (human-readable, `rust_decimal`-only).
    pub sexpr: String,
    /// SHA-256 over `sexpr` — the content-addressed `formula_hash` (design §6).
    pub formula_hash: String,
}

/// A frozen `K ≤ 16` formula pool (design §9 item 8): the sealed, content-addressed set of evolved trees.
/// Deterministic — formulas are **sorted + deduplicated by `formula_hash`**, so the same survivor set
/// always freezes to the same pool (and the same [`CatalogueIdentity`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenPool {
    /// The frozen formulas, sorted by `formula_hash`, `K ≤ 16`.
    pub formulas: Vec<FrozenFormula>,
}

impl FrozenPool {
    /// Freeze the given trees into a pool: canonically hash each, **deduplicate by `formula_hash`**, sort
    /// for a deterministic identity, and enforce the `K ≤ 16` cap.
    ///
    /// # Errors
    /// [`FreezeError::TooLarge`] if more than [`MAX_POOL_SIZE`] **distinct** formulas are offered.
    pub fn freeze(trees: &[ExprTree]) -> Result<FrozenPool, FreezeError> {
        let mut formulas: Vec<FrozenFormula> = trees
            .iter()
            .map(|t| FrozenFormula {
                sexpr: t.canonical_sexpr(),
                formula_hash: t.canonical_hash(),
            })
            .collect();
        formulas.sort_by(|a, b| a.formula_hash.cmp(&b.formula_hash));
        formulas.dedup_by(|a, b| a.formula_hash == b.formula_hash);
        if formulas.len() > MAX_POOL_SIZE {
            return Err(FreezeError::TooLarge {
                offered: formulas.len(),
            });
        }
        Ok(FrozenPool { formulas })
    }

    /// An empty pool (the default — no evolved formulas sealed).
    #[must_use]
    pub fn empty() -> FrozenPool {
        FrozenPool {
            formulas: Vec::new(),
        }
    }

    /// Whether the pool is empty (the default / golden-safe state).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.formulas.is_empty()
    }

    /// The number of frozen formulas.
    #[must_use]
    pub fn len(&self) -> usize {
        self.formulas.len()
    }

    /// The sorted `formula_hash` list — the identity payload that rides [`CatalogueIdentity`].
    #[must_use]
    pub fn formula_hashes(&self) -> Vec<String> {
        self.formulas
            .iter()
            .map(|f| f.formula_hash.clone())
            .collect()
    }

    /// A single content-addressed `pool_hash` = SHA-256 over the sorted `formula_hash` list (one per line).
    /// Stable for a given survivor set; a convenience for lineage / audit joins.
    #[must_use]
    pub fn pool_hash(&self) -> String {
        let mut hasher = Sha256::new();
        for h in self.formula_hashes() {
            hasher.update(h.as_bytes());
            hasher.update(b"\n");
        }
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// The [`CatalogueIdentity`] of the **current build's** catalogue **with this pool sealed in** — the
    /// identity a vintage carrying this pool must pin. With an empty pool this equals
    /// [`CatalogueIdentity::current`] byte-for-byte (default-off / no golden move).
    #[must_use]
    pub fn catalogue_identity(&self) -> CatalogueIdentity {
        CatalogueIdentity::current().with_formula_pool(self.formula_hashes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_signal::indicator::expr::{Expr, Field, WinOp};

    fn boxed(e: Expr) -> Box<Expr> {
        Box::new(e)
    }

    fn win(op: WinOp, f: Field, n: usize) -> ExprTree {
        ExprTree::repaired(Expr::Window(op, boxed(Expr::Input(f)), n))
    }

    #[test]
    fn freeze_produces_stable_sorted_deduplicated_hashes() {
        let trees = vec![
            win(WinOp::Rank, Field::Close, 20),
            win(WinOp::Zscore, Field::High, 50),
            win(WinOp::Rank, Field::Close, 20), // exact duplicate ⇒ deduped
        ];
        let pool = FrozenPool::freeze(&trees).unwrap();
        assert_eq!(pool.len(), 2, "the duplicate formula is deduped by hash");
        // Hashes are the canonical SHA-256 of each surviving tree, in sorted order.
        let hashes = pool.formula_hashes();
        let mut sorted = hashes.clone();
        sorted.sort();
        assert_eq!(
            hashes, sorted,
            "formulas are sorted by hash for determinism"
        );
        for f in &pool.formulas {
            assert_eq!(f.formula_hash.len(), 64, "SHA-256 hex");
            // The formula_hash is exactly the canonical S-expression SHA-256.
            assert_eq!(
                f.formula_hash,
                {
                    use sha2::Digest;
                    sha2::Sha256::digest(f.sexpr.as_bytes())
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                },
                "formula_hash must be SHA-256 over the canonical S-expression"
            );
        }
        // Re-freezing the same survivor set (any input order) yields the SAME pool + pool_hash.
        let reordered = vec![
            win(WinOp::Zscore, Field::High, 50),
            win(WinOp::Rank, Field::Close, 20),
        ];
        let pool2 = FrozenPool::freeze(&reordered).unwrap();
        assert_eq!(pool, pool2);
        assert_eq!(pool.pool_hash(), pool2.pool_hash());
    }

    #[test]
    fn freeze_enforces_the_k_le_16_cap() {
        // 17 guaranteed-distinct trees (distinct op × field × lattice period ⇒ distinct canonical hashes).
        let fields = [
            Field::Close,
            Field::High,
            Field::Low,
            Field::Volume,
            Field::Typical,
        ];
        let periods = [5usize, 10, 20, 50, 100];
        let mut many: Vec<ExprTree> = Vec::new();
        'outer: for &op in &[WinOp::Rank, WinOp::Zscore] {
            for &field in &fields {
                for &p in &periods {
                    many.push(win(op, field, p));
                    if many.len() == 17 {
                        break 'outer;
                    }
                }
            }
        }
        assert_eq!(many.len(), 17);
        assert_eq!(
            FrozenPool::freeze(&many),
            Err(FreezeError::TooLarge { offered: 17 })
        );
        // Exactly 16 distinct is allowed.
        assert_eq!(FrozenPool::freeze(&many[..16]).unwrap().len(), 16);
    }

    #[test]
    fn empty_pool_identity_is_byte_identical_to_current() {
        // GOLDEN-SAFETY: an empty pool's catalogue identity equals CatalogueIdentity::current() exactly,
        // and serialises without the pool field — the default vintage never moves.
        let empty = FrozenPool::empty();
        assert!(empty.is_empty());
        assert_eq!(empty.catalogue_identity(), CatalogueIdentity::current());
        let json = serde_json::to_string(&empty.catalogue_identity()).unwrap();
        assert!(!json.contains("formula_pool"));
    }

    #[test]
    fn a_non_empty_pool_changes_the_catalogue_identity() {
        let pool = FrozenPool::freeze(&[win(WinOp::Rank, Field::Close, 20)]).unwrap();
        assert_ne!(pool.catalogue_identity(), CatalogueIdentity::current());
        // The identity's pool equals the (sorted) frozen hashes exactly — the load boundary asserts this.
        assert_eq!(
            pool.catalogue_identity().formula_pool,
            pool.formula_hashes()
        );
    }
}
