//! Artefact-schema registry (QE-402 / AR-7): the single place that enumerates **every persisted
//! artefact format version and the load boundary that must assert it**, plus the one assertion this
//! ticket makes real and enforced — the vintage↔catalogue / vintage↔genome-rep exact-match check.
//!
//! ## Persisted artefacts, their version, and where it is asserted
//!
//! | Artefact | Version | Asserted at |
//! |---|---|---|
//! | Vintage | [`VINTAGE_FORMAT_VERSION`] | [`crate::Vintage::load`] (content-hash verify) |
//! | Catalogue (vintage↔catalogue) | [`CATALOGUE_VERSION`] + ordered-id hash | [`crate::VintageRepository::load`] → [`assert_schema`] |
//! | Genome representation (vintage↔genome) | [`GENOME_REP_VERSION`] | [`crate::VintageRepository::load`] → [`assert_schema`] |
//! | Market / synthetic store | [`MARKET_STORE_SCHEMA_VERSION`] | `qe_storage::MarketStore::open` → `check_or_init_schema` (already asserts `StorageError::SchemaMismatch`) |
//!
//! **Compatibility note.** The vintage↔catalogue and vintage↔genome-rep assertions are enforced here:
//! [`assert_schema`] is wired into [`crate::VintageRepository::load`], the single by-id load shared by
//! the CLI backtest and the live runtime, so both fail closed on a mismatch. The market-store schema is
//! versioned and asserted at its own long-standing boundary (`qe_storage`); its constant is mirrored here
//! (with this note) rather than linked, because `qe-vintage` is loaded by the live runtime and must not
//! pull the LMDB/`heed` storage stack (footprint + train/live firewall). Adding a new persisted artefact
//! type means adding a row above **and** an assertion at its load seam.

use qe_signal::{CatalogueIdentity, CATALOGUE_VERSION, REP_VERSION};

use crate::{VintageContent, VintageError, VINTAGE_FORMAT_VERSION};

/// The current catalogue version pinned in every vintage (re-exported for the registry).
pub const CATALOGUE_SCHEMA_VERSION: u32 = CATALOGUE_VERSION;

/// The current genome representation version asserted against each persisted chromosome.
pub const GENOME_REP_VERSION: u16 = REP_VERSION;

/// The current vintage artefact format version.
pub const VINTAGE_SCHEMA_VERSION: u16 = VINTAGE_FORMAT_VERSION;

/// The market / synthetic store schema version, **mirrored** from `qe_storage::SCHEMA_VERSION` and
/// asserted there (`MarketStore::open`). Kept here only so the registry enumerates every persisted
/// version in one place; a bump must be made in `qe-storage` and reflected here in lockstep.
pub const MARKET_STORE_SCHEMA_VERSION: u32 = 1;

/// Assert a loaded vintage's persisted schema identity matches this build **exactly** (QE-402).
///
/// Checks two load-boundary invariants that the pre-QE-402 bounds check missed:
/// 1. **vintage↔catalogue** — the pinned [`CatalogueIdentity`] equals [`CatalogueIdentity::current`],
///    so a catalogue reorder or a same-width `CATALOGUE_VERSION` bump is rejected.
/// 2. **vintage↔genome-rep** — every chromosome's representation version equals [`GENOME_REP_VERSION`].
///
/// Called from [`crate::VintageRepository::load`] after the content-hash verify.
///
/// # Errors
/// [`VintageError::SchemaMismatch`] on a catalogue-identity mismatch, or
/// [`VintageError::GenomeRepMismatch`] on a chromosome representation mismatch.
pub fn assert_schema(content: &VintageContent) -> Result<(), VintageError> {
    let expected = CatalogueIdentity::current();
    if content.catalogue != expected {
        return Err(VintageError::SchemaMismatch {
            expected,
            found: content.catalogue.clone(),
        });
    }
    for (index, g) in content.chromosomes.iter().enumerate() {
        if g.version != GENOME_REP_VERSION {
            return Err(VintageError::GenomeRepMismatch {
                index,
                expected: GENOME_REP_VERSION,
                found: g.version,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Vintage, VintageRepository};
    use qe_determinism::Lineage;
    use qe_risk::{CalibrationProfile, Fraction, PortfolioSizer, ShockConfig, SlippageCalibration};
    use qe_signal::{
        CatalogueConfig, Clause, ExitParams, FeatureSchema, Genome, RiskParams, RuleSet,
        CLAUSES_PER_SET,
    };
    use rust_decimal::Decimal;

    /// A unique, self-cleaning scratch dir (no `tempfile` dev-dep needed — mirrors the crate's other
    /// tests). Drops remove the directory.
    struct Scratch {
        path: std::path::PathBuf,
    }
    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "qe-vintage-schema-{}-{}",
                name,
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            Scratch { path }
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn genome() -> Genome {
        let off = Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        };
        let mut clauses = [off; CLAUSES_PER_SET];
        clauses[0] = Clause {
            enabled: true,
            feature: 0,
            lo: 1,
            hi: 2,
        };
        Genome {
            version: GENOME_REP_VERSION,
            long_entry: RuleSet {
                clauses,
                min_satisfied: 1,
            },
            short_entry: RuleSet {
                clauses: [off; CLAUSES_PER_SET],
                min_satisfied: 1,
            },
            exit: ExitParams {
                max_holding_bars: 10,
                exit_on_opposite: false,
            },
            risk: RiskParams { size_bps: 5_000 },
        }
    }

    fn content_with(catalogue: CatalogueIdentity) -> VintageContent {
        VintageContent {
            format_version: VINTAGE_FORMAT_VERSION,
            vintage_id: "schema-test".to_string(),
            chromosomes: vec![genome()],
            weights: vec![1.0],
            calibration: CalibrationProfile::new(Fraction::new(Decimal::new(2, 1)).unwrap()),
            slippage: SlippageCalibration::default(),
            sizer: PortfolioSizer::default(),
            shocks: ShockConfig::default(),
            worst_case_loss: None,
            catalogue,
            lineage: Lineage::new("cfg", "snap", "commit", vec![1]),
        }
    }

    fn repo(name: &str) -> (Scratch, VintageRepository) {
        let scratch = Scratch::new(name);
        let repo = VintageRepository::new(&scratch.path);
        (scratch, repo)
    }

    #[test]
    fn current_catalogue_vintage_loads() {
        let (_d, repo) = repo("current");
        let sealed = Vintage::seal(content_with(CatalogueIdentity::current())).unwrap();
        repo.write(&sealed).unwrap();
        // Real load boundary: hash-verify + schema assert both pass for a current-catalogue vintage.
        let loaded = repo.load("schema-test").unwrap();
        assert_eq!(loaded.content.catalogue, CatalogueIdentity::current());
    }

    #[test]
    fn same_width_version_bump_is_rejected_on_load() {
        // A vintage sealed under catalogue vN loaded against a build at vN+1 (identical width and
        // num_states) is a hard error — the exact-match guard the bounds check could never provide.
        let mut bumped = CatalogueIdentity::current();
        bumped.catalogue_version += 1; // same id_hash + num_states, only the version differs
        let (_d, repo) = repo("bump");
        let sealed = Vintage::seal(content_with(bumped)).unwrap();
        repo.write(&sealed).unwrap();
        assert!(matches!(
            repo.load("schema-test"),
            Err(VintageError::SchemaMismatch { .. })
        ));
    }

    #[test]
    fn reordering_indicators_changes_the_hash_and_is_rejected() {
        let schema = FeatureSchema::from_catalogue(&CatalogueConfig::default());
        let mut ids = schema.ids().to_vec();
        assert!(ids.len() >= 2, "need at least two indicators to reorder");
        ids.swap(0, 1); // reorder two indicators

        let original = CatalogueIdentity::hash_ids(schema.ids());
        let reordered = CatalogueIdentity::hash_ids(&ids);
        assert_ne!(
            original, reordered,
            "reordering two indicators must change the ordered-id hash"
        );

        // A vintage carrying the reordered identity (same version + num_states) is rejected on load.
        let reordered_identity = CatalogueIdentity {
            catalogue_version: schema.version(),
            num_states: schema.num_states(),
            id_hash: reordered,
            formula_pool: Vec::new(),
        };
        let (_d, repo) = repo("reorder");
        let sealed = Vintage::seal(content_with(reordered_identity)).unwrap();
        repo.write(&sealed).unwrap();
        assert!(matches!(
            repo.load("schema-test"),
            Err(VintageError::SchemaMismatch { .. })
        ));
    }

    #[test]
    fn a_tampered_formula_pool_is_rejected_at_the_load_boundary() {
        // QE-451 Phase 1b: the frozen GP formula pool is part of the sealed [`CatalogueIdentity`], and the
        // load boundary asserts it EXACTLY (design §6). This makes tamper-rejection explicit (previously
        // covered only transitively by the reorder test).
        use qe_signal::indicator::expr::{Expr, ExprTree, Field, WinOp};
        let win = |op, f, n| ExprTree::repaired(Expr::Window(op, Box::new(Expr::Input(f)), n));
        // Two real frozen formulas → their canonical-S-expr SHA-256 `formula_hash`es (sorted).
        let f1 = win(WinOp::Rank, Field::Close, 20);
        let f2 = win(WinOp::Zscore, Field::High, 50);
        let good_pool = {
            let mut v = vec![f1.canonical_hash(), f2.canonical_hash()];
            v.sort();
            v
        };

        // A QE-454-style build that sanctions `good_pool` re-derives the SAME identity — so a vintage
        // carrying exactly `good_pool` would pass the boundary's exact-equality check (mirrors
        // `assert_schema`'s `content.catalogue == current()` comparison).
        let sanctioned = CatalogueIdentity::current().with_formula_pool(good_pool.clone());
        assert_eq!(
            sanctioned,
            CatalogueIdentity::current().with_formula_pool(good_pool.clone()),
            "the untampered pool re-derives to the identical sanctioned identity (loads clean)"
        );

        // TAMPER: alter ONE frozen entry's canonical S-expression (Rank period 20 → 50) so its recomputed
        // `formula_hash` no longer matches — a single-entry tamper must change the whole identity.
        let mut tampered_pool = good_pool.clone();
        let tampered_hash = win(WinOp::Rank, Field::Close, 50).canonical_hash();
        tampered_pool[0] = tampered_hash.clone();
        assert_ne!(
            tampered_pool, good_pool,
            "the tamper actually changed a hash"
        );
        let tampered = CatalogueIdentity::current().with_formula_pool(tampered_pool);
        assert_ne!(
            tampered, sanctioned,
            "a single tampered formula_hash must change the sealed identity"
        );

        // End-to-end through the REAL load boundary (repo.load → hash-verify → assert_schema): the default
        // build's `current()` sanctions an EMPTY pool, so a vintage carrying a (tampered) sealed pool is
        // rejected with the EXACT `SchemaMismatch`, whose `found` carries the sealed pool verbatim.
        let (_d, repo_t) = repo("pool-tamper");
        let sealed = Vintage::seal(content_with(tampered.clone())).unwrap();
        repo_t.write(&sealed).unwrap();
        match repo_t.load("schema-test") {
            Err(VintageError::SchemaMismatch { expected, found }) => {
                assert_eq!(expected, CatalogueIdentity::current());
                assert_eq!(found, tampered, "the boundary sees the exact tampered pool");
                assert!(!found.formula_pool.is_empty());
            }
            other => panic!("tampered pool must be rejected with SchemaMismatch, got {other:?}"),
        }

        // The untampered default (empty pool) vintage loads CLEAN at the same boundary — non-vacuous.
        let (_d2, repo_c) = repo("pool-clean");
        let clean = Vintage::seal(content_with(CatalogueIdentity::current())).unwrap();
        repo_c.write(&clean).unwrap();
        assert!(
            repo_c.load("schema-test").is_ok(),
            "the untampered (empty-pool) vintage must load clean"
        );
    }

    #[test]
    fn wrong_genome_rep_version_is_rejected_on_load() {
        let mut content = content_with(CatalogueIdentity::current());
        content.chromosomes[0].version = GENOME_REP_VERSION + 1;
        let (_d, repo) = repo("rep");
        let sealed = Vintage::seal(content).unwrap();
        repo.write(&sealed).unwrap();
        assert!(matches!(
            repo.load("schema-test"),
            Err(VintageError::GenomeRepMismatch {
                index: 0,
                found,
                ..
            }) if found == GENOME_REP_VERSION + 1
        ));
    }

    #[test]
    fn registry_versions_track_their_sources() {
        assert_eq!(CATALOGUE_SCHEMA_VERSION, CATALOGUE_VERSION);
        assert_eq!(GENOME_REP_VERSION, REP_VERSION);
        assert_eq!(VINTAGE_SCHEMA_VERSION, VINTAGE_FORMAT_VERSION);
    }
}
