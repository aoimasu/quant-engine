//! QE-458 — the server-side steer-delta builder + evolved-pool feature-space load path.
//!
//! `validate_train` (design §6.2) enforces the whitelist/blocklist; this module turns an accepted, steered
//! [`TrainParams`] into the [`SteerDelta`] recorded in `VintageContent.lineage` (QE-467's schema, design
//! §6-e) and computes the **available-feature-space size** (catalogue count + included sealed evolved-pool
//! formulas) that feeds the QE-439 distinct-trial basis `N` (design §6.1a, AC a).
//!
//! **Firewall (AC f):** this reads a sealed [`FormulaPoolContent`] via `qe-formula-pool` — an edge
//! `qe-server → qe-formula-pool` that already exists and is proven green by
//! `crates/architecture/tests/firewall.rs`. No `qe-wfo`/`qe-ensemble`/`qe-runtime`/`qe-venue` edge is
//! introduced, and nothing here re-deflates or un-seals the pool (it is read-only).

use qe_formula_pool::PoolFormula;
use qe_run_protocol::TrainParams;
use qe_vintage::SteerDelta;

/// Whether any whitelisted steer knob is set — an un-steered request records **no** steer delta
/// (`steer_delta: None`), so the default seal path keeps its golden vintage hash (design §6-e).
#[must_use]
pub fn is_steered(p: &TrainParams) -> bool {
    p.indicator_subset.is_some()
        || p.evolved_pool.is_some()
        || p.evolved_formulas.is_some()
        || p.windows.is_some()
        || p.folds.is_some()
        // budget knobs are steerable too (design §6.1) — steering purely on budget still records a delta.
        || p.generations.is_some()
        || p.population.is_some()
}

/// The order-independent steered-subset hash — delegates to [`SteerDelta::subset_hash`] (the single
/// hashing source in `qe-vintage`) so the server's recorded hash and the CLI seal path's hash agree.
#[must_use]
pub fn indicator_subset_hash(
    catalogue_ids: &[String],
    evolved_formula_hashes: &[String],
) -> String {
    SteerDelta::subset_hash(catalogue_ids, evolved_formula_hashes)
}

/// The count of included, already-sealed evolved-pool formulas (design §6.1a): the subset the client named
/// (`evolved_formulas`) intersected with what the pool actually sealed, or the whole sealed pool when the
/// client named none. `sealed` is the sealed pool's frozen `formulas` (read-only — reading it never
/// re-deflates or un-seals the pool, AC f). A named hash that is **not** in the sealed pool is ignored (it
/// cannot conjure a formula that was never sealed). Returns `(count, included_hashes)`.
#[must_use]
pub fn included_evolved_formulas(
    sealed_formulas: &[PoolFormula],
    requested: Option<&[String]>,
) -> (usize, Vec<String>) {
    let sealed: Vec<String> = sealed_formulas
        .iter()
        .map(|f| f.formula_hash.clone())
        .collect();
    match requested {
        None => (sealed.len(), sealed),
        Some(req) => {
            let included: Vec<String> = sealed
                .into_iter()
                .filter(|h| req.iter().any(|r| r == h))
                .collect();
            (included.len(), included)
        }
    }
}

/// Build the [`SteerDelta`] recorded into `VintageContent.lineage` for an accepted, steered request (design
/// §6-e). `catalogue_count` is the catalogue-indicator count actually in play (full catalogue, or the
/// steered `indicator_subset` length); `evolved_formula_hashes` is the included sealed-pool subset. Returns
/// `None` for an un-steered request so the golden un-steered seal path is unchanged.
#[must_use]
pub fn steer_delta_for(
    p: &TrainParams,
    catalogue_ids_in_play: &[String],
    evolved_formula_hashes: &[String],
) -> Option<SteerDelta> {
    if !is_steered(p) {
        return None;
    }
    Some(SteerDelta {
        indicator_subset_hash: indicator_subset_hash(catalogue_ids_in_play, evolved_formula_hashes),
        generations: p.generations.unwrap_or(0) as u64,
        population: p.population.unwrap_or(0) as u64,
        windows: p.windows.unwrap_or(0) as u64,
        folds: p.folds.unwrap_or(0) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> TrainParams {
        TrainParams {
            start: "2020-01-01".into(),
            end: "2021-01-01".into(),
            resolution: "1h".into(),
            ..TrainParams::default()
        }
    }

    #[test]
    fn unsteered_request_records_no_delta() {
        assert!(!is_steered(&params()));
        assert!(steer_delta_for(&params(), &[], &[]).is_none());
    }

    #[test]
    fn steered_request_records_a_delta_with_a_64_hex_subset_hash() {
        let mut p = params();
        p.indicator_subset = Some(vec!["rsi_14".into(), "atr_pct".into()]);
        p.generations = Some(40);
        p.population = Some(12);
        p.windows = Some(6);
        p.folds = Some(4);
        assert!(is_steered(&p));
        let d = steer_delta_for(&p, &["rsi_14".into(), "atr_pct".into()], &[]).unwrap();
        assert_eq!(d.generations, 40);
        assert_eq!(d.population, 12);
        assert_eq!(d.windows, 6);
        assert_eq!(d.folds, 4);
        assert_eq!(d.indicator_subset_hash.len(), 64);
        assert!(d
            .indicator_subset_hash
            .chars()
            .all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn subset_hash_is_order_independent_and_set_based() {
        let a = indicator_subset_hash(&["rsi_14".into(), "atr_pct".into()], &[]);
        let b = indicator_subset_hash(&["atr_pct".into(), "rsi_14".into()], &[]);
        let c = indicator_subset_hash(&["atr_pct".into(), "rsi_14".into(), "rsi_14".into()], &[]);
        assert_eq!(a, b, "hash must be order-independent");
        assert_eq!(a, c, "hash must be dedup/set-based");
        let d = indicator_subset_hash(&["rsi_14".into()], &[]);
        assert_ne!(a, d, "a different subset must hash differently");
    }

    fn sealed(hashes: &[&str]) -> Vec<PoolFormula> {
        hashes
            .iter()
            .map(|h| PoolFormula {
                sexpr: format!("(rank (input close) 20) ; {h}"),
                formula_hash: (*h).to_string(),
            })
            .collect()
    }

    #[test]
    fn evolved_pool_load_path_counts_sealed_formulas_read_only() {
        // AC (f)/§6.1a: counting included evolved formulas reads the sealed pool only — no re-deflation.
        let pool = sealed(&["aa", "bb", "cc"]);
        // No subset named ⇒ whole sealed pool.
        let (count, included) = included_evolved_formulas(&pool, None);
        assert_eq!(count, 3);
        assert_eq!(included, vec!["aa", "bb", "cc"]);
        // A named subset intersects with what was actually sealed.
        let (count, included) = included_evolved_formulas(&pool, Some(&["bb".into(), "cc".into()]));
        assert_eq!(count, 2);
        assert_eq!(included, vec!["bb", "cc"]);
        // A hash that was never sealed cannot be conjured into the feature space.
        let (count, included) = included_evolved_formulas(&pool, Some(&["zz".into()]));
        assert_eq!(count, 0);
        assert!(included.is_empty());
    }
}
