//! QE-458 — the steer-knob guardrail math (design QE-455 §6 / §6.1a).
//!
//! Steering must change *what the search explores and how hard*, never *what passes the gate*. This module
//! is the pure, server-side, non-editable half of that contract: the compiled **floors** and the
//! **gate-monotone** deflation math that make every whitelisted knob provably unable to move a candidate
//! from G1-reject to G1-seal.
//!
//! Nothing here reads a portfolio/live outcome (deps stay `qe-determinism` only), so consuming it from
//! `validate_train` adds no firewall edge. The blocklist floors below are the compiled values the
//! `validate_train` guard rejects a request for going *below* (design §6.2) — they are **not** request
//! fields; the G1 thresholds themselves stay in `G1Criteria`/`DEFLATION_BASIS_VERSION` and are never edited.

use crate::dsr::effective_trials;

// ---- (a) available-feature-space cardinality → the distinct-trial basis N ------------------------------

/// The **available-feature-space size** the steered search may reference (design §6.1a): catalogue-indicator
/// count **plus** the count of included, already-sealed evolved-pool formulas. A search allowed to reference
/// more indicators has explored a larger hypothesis space and must be deflated against it.
#[must_use]
pub fn available_feature_space(catalogue_count: usize, evolved_count: usize) -> usize {
    catalogue_count.saturating_add(evolved_count)
}

/// QE-439's effective-trials basis (`cells·generations·windows`) **extended to ingest the available
/// feature-space size** (design §6.1a, AC a). Referencing more indicators multiplies the hypothesis space,
/// so the basis is scaled by the feature-space cardinality:
///
/// ```text
/// N = effective_trials(cells, generations, windows) · max(1, feature_space)
/// ```
///
/// This is **monotone non-decreasing** in every argument (cells, generations, windows, feature_space) —
/// raising any whitelisted knob can only *raise* `N` and therefore the `E[maxSharpe]` deflation bar, never
/// lower it. Over-counting is the safe direction (design §12.5: false-reject, never false-accept), so a
/// feature-space of 0 floors at 1 (no under-count). Saturating throughout.
#[must_use]
pub fn effective_trials_with_features(
    cells: usize,
    generations: usize,
    windows: usize,
    feature_space: usize,
) -> usize {
    effective_trials(cells, generations, windows).saturating_mul(feature_space.max(1))
}

// ---- (c) archive-coverage recording + floor (the QD mandate) ------------------------------------------

/// The MAP-Elites `Elite<ExprTree>` descriptor-space size — `5 families × 3 timescales × 3 complexity` = 45
/// cells. A **documented mirror** of `qe_wfo::gp::descriptor::EXPR_CELLS`: `qe-wfo` depends on
/// `qe-validation`, never the reverse, so the constant is duplicated here (with this note) rather than
/// inverting the dependency. The `archive_coverage_matches_wfo_cell_count` test in `qe-wfo` pins the two
/// together so a grid change cannot silently drift.
pub const DESCRIPTOR_SPACE_CELLS: usize = 45;

/// Minimum occupied niches a steered run must keep so steering cannot flatten the quality-diversity archive
/// (design §6.1a, AC c; `specs.md` QD mandate).
///
/// **FLAGGED DEFAULT (QE-458):** no QD floor is defined anywhere in the repo. This conservative default (5 of
/// 45 cells, coverage ≥ ~0.11) is a genuine-collapse tripwire — it surfaces a steer that flattens the archive
/// to a handful of niches without falsely rejecting a healthy, diverse run. The exact floor is a product
/// decision.
pub const MIN_OCCUPIED_NICHES: usize = 5;

/// Archive coverage — occupied-niche count / descriptor-space size (design §6.1a). Recorded **pre and post**
/// steer so a coverage-collapsing steer is surfaced, never hidden. `0.0` when the descriptor space is empty.
#[must_use]
pub fn archive_coverage(occupied_niches: usize, descriptor_space: usize) -> f64 {
    if descriptor_space == 0 {
        return 0.0;
    }
    occupied_niches as f64 / descriptor_space as f64
}

/// Whether a post-steer occupied-niche count clears the [`MIN_OCCUPIED_NICHES`] floor — steering that drops
/// below it has flattened the QD archive and must be surfaced/rejected (design §6.1a, AC c).
#[must_use]
pub fn coverage_floor_ok(occupied_niches: usize) -> bool {
    occupied_niches >= MIN_OCCUPIED_NICHES
}

// ---- (d) regime-coverage invariant floors (window/fold) -----------------------------------------------

/// Minimum WFO windows a steered run must keep. Fewer windows ⇒ a smaller total out-of-sample span ⇒ weaker
/// regime coverage, so a window knob below this floor is rejected (design §6.1a, AC d).
///
/// **FLAGGED DEFAULT (QE-458):** the true invariant is an OOS-span-in-bars floor plus the *mandated stress
/// regime*, neither of which is defined server-side (the regime classifier / composition is a downstream
/// QE-460 field, and `validate_train` sees only window/fold **counts**, not bars). This window-count floor is
/// the conservative proxy; product must name the stress regime + set the bar floor once QE-460 lands.
pub const MIN_WFO_WINDOWS: usize = 4;

/// Minimum cross-validation folds a steered run must keep (a CSCV split needs `≥ 2` even folds; regime
/// coverage cannot shrink below it). Conservative proxy — see [`MIN_WFO_WINDOWS`].
pub const MIN_WFO_FOLDS: usize = 2;

// ---- (§6.2) blocklist compiled floors -----------------------------------------------------------------
//
// Everything the G1 gate's decision rides on is OFF the whitelist. A request that sets any of these BELOW
// its compiled floor is a `400` (`validate_train`). They are not steerable; the floors are sourced from the
// existing compiled consts (cost-stress `{1×,2×}`, `formula-pool` turnover/capacity, `GpDeflationGate`
// defaults) so this module carries no independent policy.

/// Cost-stress friction multiplier floor — the `min{1×,2×}` re-cost sweep never drops below `1×` (design
/// §4.6 / `qe_wfo::gp::gates`).
pub const COST_STRESS_MULTIPLIER_FLOOR: f64 = 1.0;
/// Max-turnover cap floor (`qe_formula_pool::MAX_TURNOVER_FRAC = 0.25`).
pub const MAX_TURNOVER_CAP_FLOOR: f64 = 0.25;
/// Capacity floor, USD (`qe_formula_pool::CAPACITY_FLOOR_USD = 250_000`).
pub const CAPACITY_FLOOR_USD: i64 = 250_000;
/// DSR cutoff floor (`GpDeflationGate::default().min_dsr = 0.95`).
pub const DSR_CUTOFF_FLOOR: f64 = 0.95;
/// Uncensored-PBO cutoff floor (`GpDeflationGate::default().max_pbo = 0.5`; a request cannot lower the bar).
pub const PBO_CUTOFF_FLOOR: f64 = 0.5;
/// IC / FDR threshold floor (the IC-screen discovery bar; conservative default).
pub const IC_FDR_THRESHOLD_FLOOR: f64 = 0.10;
/// Holdout-size floor, bars — the frozen holdout is floored, never tuned down (design §4).
pub const HOLDOUT_FLOOR: usize = 250;
/// Embargo floor, bars — purge/embargo derives from indicator lookback, floored here.
pub const EMBARGO_FLOOR: usize = 1;
/// Purge floor, bars.
pub const PURGE_FLOOR: usize = 1;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsr::deflated_sharpe_ratio;
    use crate::nulls::label_shuffle_returns;
    use crate::stats::sharpe_ratio;

    // A deterministic pseudo-noise return series (no RNG dep): a bounded, mean-near-zero oscillation.
    fn noise_series(seed: u64, n: usize) -> Vec<f64> {
        let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((s >> 33) as f64 / (1u64 << 31) as f64) - 1.0
            })
            .collect()
    }

    #[test]
    fn feature_space_is_catalogue_plus_evolved_and_saturating() {
        assert_eq!(available_feature_space(24, 8), 32);
        assert_eq!(available_feature_space(usize::MAX, 8), usize::MAX); // saturating
    }

    // (b)/§6.3 base case: N and the deflation bar are NON-DECREASING as any whitelisted knob rises.
    #[test]
    fn deflation_scaling_is_monotone_non_decreasing_in_every_knob() {
        let base = effective_trials_with_features(45, 40, 4, 24);
        // Feature-space up (bigger indicator subset) — N must not fall.
        assert!(effective_trials_with_features(45, 40, 4, 48) >= base);
        // Generations up.
        assert!(effective_trials_with_features(45, 80, 4, 24) >= base);
        // Windows up.
        assert!(effective_trials_with_features(45, 40, 8, 24) >= base);
        // Cells (niches) up.
        assert!(effective_trials_with_features(90, 40, 4, 24) >= base);
        // Feature-space of 0 floors at 1 — never under-counts below the plain basis.
        assert_eq!(
            effective_trials_with_features(45, 40, 4, 0),
            effective_trials(45, 40, 4)
        );
        // Strict growth in the feature axis for a non-degenerate basis (the cardinality→N claim).
        assert!(
            effective_trials_with_features(45, 40, 4, 25)
                > effective_trials_with_features(45, 40, 4, 24)
        );
    }

    // (b) false-discovery on pure noise: enlarging the indicator subset RAISES N and E[maxSharpe], so the
    // champion's DSR is NON-INCREASING — a bigger subset cannot manufacture more seals on noise.
    #[test]
    fn noise_series_false_discovery_larger_subset_does_not_raise_seal_rate() {
        // A population of pure-noise trials; the champion is the max in-sample Sharpe (selection over noise).
        let population: Vec<Vec<f64>> = (0..200).map(|k| noise_series(1000 + k, 400)).collect();
        let champion = population
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| sharpe_ratio(a).total_cmp(&sharpe_ratio(b)))
            .map(|(i, _)| i)
            .unwrap();
        let trial_variance = crate::dsr::trial_sharpe_variance(&population);
        assert!(
            trial_variance > 0.0,
            "noise trials must have a real Sharpe dispersion"
        );

        // Sweep the indicator-subset cardinality up; DSR must be non-increasing (never MORE seals on noise).
        let mut prev_dsr = f64::INFINITY;
        let mut prev_n = 0usize;
        for feature_space in [8usize, 16, 32, 64, 128] {
            let n = effective_trials_with_features(45, 40, 4, feature_space);
            let dsr = deflated_sharpe_ratio(&population[champion], trial_variance, n);
            assert!(
                n >= prev_n,
                "N must be non-decreasing as the subset grows: {n} < {prev_n}"
            );
            assert!(
                dsr <= prev_dsr + 1e-9,
                "a larger subset must NOT raise the noise champion's DSR (seal rate): {dsr} > {prev_dsr}"
            );
            prev_dsr = dsr;
            prev_n = n;
        }
    }

    // §6.3 proof obligation (gate-monotone sweep): on a fixed noise dataset where the UN-STEERED deflation
    // gate REJECTS, sweep every whitelisted knob across its range and assert no steered configuration flips
    // the verdict reject→accept. DSR is monotone-decreasing in N and N is monotone-non-decreasing in every
    // knob, so a candidate the un-steered gate rejects can never be sealed by steering.
    #[test]
    fn gate_monotone_sweep_no_knob_moves_reject_to_seal() {
        let population: Vec<Vec<f64>> = (0..300)
            .map(|k| label_shuffle_returns(&noise_series(7, 500), 500 + k))
            .collect();
        let champion = population
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| sharpe_ratio(a).total_cmp(&sharpe_ratio(b)))
            .map(|(i, _)| i)
            .unwrap();
        let trial_variance = crate::dsr::trial_sharpe_variance(&population);

        // A strict DSR pass-bar; pick the un-steered basis so the noise champion is REJECTED.
        let dsr_pass = 0.95;
        let baseline_n = effective_trials_with_features(45, 20, 2, 8);
        let baseline_dsr = deflated_sharpe_ratio(&population[champion], trial_variance, baseline_n);
        assert!(
            baseline_dsr < dsr_pass,
            "test premise: the un-steered gate must REJECT the noise champion (DSR {baseline_dsr} < {dsr_pass})"
        );

        // Sweep every whitelisted knob upward across its allowed range.
        for &gens in &[20usize, 40, 80, 160] {
            for &windows in &[2usize, 4, 8] {
                for &feat in &[8usize, 16, 32, 64, 128, 256] {
                    let n = effective_trials_with_features(45, gens, windows, feat);
                    // Monotone: steering only raises N vs the un-steered basis.
                    assert!(
                        n >= baseline_n,
                        "steering must not lower N: {n} < {baseline_n}"
                    );
                    let dsr = deflated_sharpe_ratio(&population[champion], trial_variance, n);
                    assert!(
                        dsr < dsr_pass,
                        "GATE-MONOTONE VIOLATION: steer (gens={gens}, windows={windows}, feat={feat}) \
                         sealed a candidate the un-steered gate rejected (DSR {dsr} ≥ {dsr_pass})"
                    );
                }
            }
        }
    }

    #[test]
    fn archive_coverage_records_and_floors() {
        assert!((archive_coverage(45, 45) - 1.0).abs() < 1e-12);
        assert!((archive_coverage(0, 45)).abs() < 1e-12);
        assert_eq!(archive_coverage(9, 0), 0.0); // empty descriptor space
                                                 // A healthy diverse archive clears the floor; a collapsed one does not.
        assert!(coverage_floor_ok(MIN_OCCUPIED_NICHES));
        assert!(coverage_floor_ok(20));
        assert!(!coverage_floor_ok(MIN_OCCUPIED_NICHES - 1));
        assert!(
            !coverage_floor_ok(1),
            "collapse to a single niche must trip the floor"
        );
    }

    #[test]
    fn blocklist_floors_match_the_compiled_source_consts() {
        // Guard against accidental drift from the source of truth.
        assert_eq!(CAPACITY_FLOOR_USD, 250_000);
        assert!((MAX_TURNOVER_CAP_FLOOR - 0.25).abs() < 1e-12);
        assert!((COST_STRESS_MULTIPLIER_FLOOR - 1.0).abs() < 1e-12);
        assert!((DSR_CUTOFF_FLOOR - 0.95).abs() < 1e-12);
        assert!((PBO_CUTOFF_FLOOR - 0.5).abs() < 1e-12);
    }
}
