//! Variation operators + adaptive selection driver (QE-119) — the genome-level mechanics behind the
//! QE-112 operator vocabulary, wired to the QE-118 archive and the QE-112 credit-proportional bandit.
//!
//! Three operators generate offspring (all mutate-freely-then-repair, QE-110):
//! - [`local_refine`] — exploitation: nudges only numeric genes, **preserving the descriptor cell** (a
//!   local hill-climb of an elite);
//! - [`explore`] — exploration: aggressive multi-locus mutation that re-points features and resets the
//!   holding cap, typically **changing the cell** (a jump to a new niche);
//! - [`fresh_random`] — maximal exploration: a brand-new random genome.
//!
//! [`VariationDriver`] ties selection → parent sampling → operator → evaluation → archive insertion →
//! credit back to the [`OperatorSelector`]. Because `local_refine` stays in its cell while `explore`/
//! `fresh_random` jump, the productive outcome differs by archive density: on a **sparse** archive the
//! jumpers fill empty cells (`NewCell`) while `local_refine` only `Added`s; on a **dense** archive the
//! jumpers are `Rejected` while `local_refine` improves an elite. The credit-proportional selector then
//! shifts budget to exploration when sparse and exploitation when dense — the QE-112 design intent,
//! emergent rather than hard-coded. Backtest evaluation is QE-120 (the driver takes a scalar `eval`).

use qe_determinism::DetRng;
use qe_domain::Direction;
use qe_signal::FeatureSchema;
use rand_core::RngCore;

use crate::archive::descriptor_for;
use crate::genome::{
    Clause, ExitParams, Genome, RiskParams, RuleSet, CLAUSES_PER_SET, MAX_SIZE_BPS,
};
use crate::mapelites::{InsertOutcome, MapElitesArchive};
use crate::operator::{ApplicationOutcome, Operator, OperatorSelector};

/// `size_bps` step a single `local_refine` nudge applies (basis points).
pub const LOCAL_SIZE_STEP: u16 = 500;

/// Default upper bound (bars) for a randomly-drawn `max_holding_bars`.
const MAX_RANDOM_HOLDING: u64 = 240;

/// Uniform integer in `0..n` from one rng draw (`0` if `n == 0`). Modulo bias is negligible for the
/// small ranges used here (feature/state/holding counts).
fn below(rng: &mut impl RngCore, n: u64) -> u64 {
    if n == 0 {
        0
    } else {
        rng.next_u64() % n
    }
}

/// A coin flip from one rng draw.
fn flip(rng: &mut impl RngCore) -> bool {
    rng.next_u64() & 1 == 0
}

/// Pick a clause `feature` index. With an **empty** `allowed` allowlist this is the historical
/// `below(rng, len)` — byte-identical to the un-steered search. With a **non-empty** allowlist (QE-458
/// steered search over a restricted catalogue subset) it draws uniformly from the allowed feature indices.
/// It consumes exactly **one** rng draw either way, so gating on the allowlist never shifts an un-steered
/// run's deterministic draw sequence (the sealed golden vintage stays byte-identical). The allowed indices
/// are valid indices into the full-width schema, so `Genome::repair` never re-points them and the search
/// stays leak-free — a disallowed feature can never appear in any clause.
fn pick_feature(rng: &mut impl RngCore, len: usize, allowed: &[u16]) -> u16 {
    if allowed.is_empty() {
        below(rng, len.max(1) as u64) as u16
    } else {
        allowed[below(rng, allowed.len() as u64) as usize]
    }
}

/// A random `[lo, hi]` band within `0..num_states` (inclusive, `lo ≤ hi`).
fn random_band(rng: &mut impl RngCore, num_states: u16) -> (u16, u16) {
    let n = num_states.max(1) as u64;
    let a = below(rng, n) as u16;
    let b = below(rng, n) as u16;
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// A random rule bank with at least clause 0 enabled (so the bank has a descriptor). `allowed` restricts
/// which feature indices clauses may reference (empty ⇒ the full catalogue — see [`pick_feature`]).
fn random_bank<R: RngCore>(rng: &mut R, len: usize, num_states: u16, allowed: &[u16]) -> RuleSet {
    let mut clauses = [Clause {
        enabled: false,
        feature: 0,
        lo: 0,
        hi: 0,
    }; CLAUSES_PER_SET];
    for (i, slot) in clauses.iter_mut().enumerate() {
        let (lo, hi) = random_band(rng, num_states);
        *slot = Clause {
            enabled: i == 0 || flip(rng),
            feature: pick_feature(rng, len, allowed),
            lo,
            hi,
        };
    }
    RuleSet {
        clauses,
        min_satisfied: (below(rng, CLAUSES_PER_SET as u64) as u8) + 1,
    }
}

/// **Maximal exploration:** a brand-new random genome (no parent), repaired onto the validity manifold.
pub fn fresh_random<R: RngCore>(rng: &mut R, schema: &FeatureSchema) -> Genome {
    fresh_random_masked(rng, schema, &[])
}

/// [`fresh_random`] restricted to the `allowed` catalogue-feature allowlist (QE-458 steered search).
/// `allowed` empty ⇒ identical to [`fresh_random`].
pub fn fresh_random_masked<R: RngCore>(
    rng: &mut R,
    schema: &FeatureSchema,
    allowed: &[u16],
) -> Genome {
    let len = schema.len();
    let num_states = schema.num_states();
    let mut g = Genome {
        version: crate::genome::REP_VERSION,
        long_entry: random_bank(rng, len, num_states, allowed),
        short_entry: random_bank(rng, len, num_states, allowed),
        exit: ExitParams {
            max_holding_bars: (below(rng, MAX_RANDOM_HOLDING) as u16) + 1,
            exit_on_opposite: flip(rng),
        },
        risk: RiskParams {
            size_bps: (below(rng, MAX_SIZE_BPS as u64) as u16) + 1,
        },
    };
    g.repair(schema);
    g
}

/// **Exploitation, cell-preserving:** nudge only numeric genes — each bank's enabled-clause bounds by ±1
/// and `size_bps` by ±[`LOCAL_SIZE_STEP`]. Features, the enabled set, and `max_holding_bars` are left
/// untouched, so the descriptor [`Cell`](crate::archive::Cell) is unchanged: a local hill-climb.
pub fn local_refine<R: RngCore>(parent: &Genome, rng: &mut R, schema: &FeatureSchema) -> Genome {
    let mut g = parent.clone();
    let max_state = schema.num_states().saturating_sub(1);

    // Nudge size_bps up or down by one step.
    g.risk.size_bps = if flip(rng) {
        g.risk.size_bps.saturating_add(LOCAL_SIZE_STEP)
    } else {
        g.risk.size_bps.saturating_sub(LOCAL_SIZE_STEP)
    };

    // Nudge one enabled clause's band in each bank (bounds only — feature/enabled untouched).
    for set in [&mut g.long_entry, &mut g.short_entry] {
        let enabled: Vec<usize> = set
            .clauses
            .iter()
            .enumerate()
            .filter(|(_, c)| c.enabled)
            .map(|(i, _)| i)
            .collect();
        if enabled.is_empty() {
            continue;
        }
        let i = enabled[below(rng, enabled.len() as u64) as usize];
        let c = &mut set.clauses[i];
        if flip(rng) {
            // nudge lo
            c.lo = if flip(rng) {
                (c.lo + 1).min(c.hi)
            } else {
                c.lo.saturating_sub(1)
            };
        } else {
            // nudge hi
            c.hi = if flip(rng) {
                (c.hi + 1).min(max_state)
            } else {
                c.hi.saturating_sub(1).max(c.lo)
            };
        }
    }
    g.repair(schema);
    g
}

/// **Exploration, cell-changing:** aggressive multi-locus mutation — re-point a clause's `feature`
/// (changes family/timescale), force it enabled with a fresh band, and reset `max_holding_bars` (changes
/// the holding band). The offspring typically lands in a different niche.
pub fn explore<R: RngCore>(parent: &Genome, rng: &mut R, schema: &FeatureSchema) -> Genome {
    explore_masked(parent, rng, schema, &[])
}

/// [`explore`] restricted to the `allowed` catalogue-feature allowlist (QE-458 steered search). `allowed`
/// empty ⇒ identical to [`explore`].
pub fn explore_masked<R: RngCore>(
    parent: &Genome,
    rng: &mut R,
    schema: &FeatureSchema,
    allowed: &[u16],
) -> Genome {
    let mut g = parent.clone();
    let len = schema.len();
    let num_states = schema.num_states();

    for set in [&mut g.long_entry, &mut g.short_entry] {
        let i = below(rng, CLAUSES_PER_SET as u64) as usize;
        let (lo, hi) = random_band(rng, num_states);
        set.clauses[i] = Clause {
            enabled: true,
            feature: pick_feature(rng, len, allowed),
            lo,
            hi,
        };
        // Optionally toggle another clause to vary the active count.
        let j = below(rng, CLAUSES_PER_SET as u64) as usize;
        if j != i {
            set.clauses[j].enabled = flip(rng);
        }
    }
    // Reset the holding cap (changes the holding band).
    g.exit.max_holding_bars = (below(rng, MAX_RANDOM_HOLDING) as u16) + 1;
    g.repair(schema);
    g
}

/// What one [`VariationDriver::step`] did, for inspection / logging (not required by the loop).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepReport {
    /// The operator selected this step.
    pub operator: Operator,
    /// The archive insert outcome in the driven direction (None ⇒ no descriptor there).
    pub insert_outcome: Option<InsertOutcome>,
    /// The credit-bearing outcome recorded against the operator.
    pub application: ApplicationOutcome,
}

/// Drives adaptive operator selection over a [`MapElitesArchive`] in one [`Direction`]: select →
/// sample parent → vary → evaluate → insert → credit.
#[derive(Debug, Clone)]
pub struct VariationDriver {
    selector: OperatorSelector,
    direction: Direction,
    /// QE-458 steered-search feature allowlist: the catalogue-feature indices the search may reference.
    /// **Empty ⇒ the full catalogue** (un-steered; byte-identical draw sequence). A non-empty allowlist
    /// restricts every offspring's clause features to these indices (leak-free — see [`pick_feature`]).
    allowed_features: Vec<u16>,
}

impl VariationDriver {
    /// Build a driver evolving `direction` with the given credit-proportional `selector`. The feature
    /// allowlist is empty (un-steered, full catalogue) — use [`with_allowed_features`](Self::with_allowed_features)
    /// to restrict a steered search.
    #[must_use]
    pub fn new(selector: OperatorSelector, direction: Direction) -> Self {
        VariationDriver {
            selector,
            direction,
            allowed_features: Vec::new(),
        }
    }

    /// Restrict this driver's search to the given catalogue-feature indices (QE-458 steered search). An
    /// empty slice leaves it un-steered (full catalogue). Indices must be valid into the search schema.
    #[must_use]
    pub fn with_allowed_features(mut self, allowed: Vec<u16>) -> Self {
        self.allowed_features = allowed;
        self
    }

    /// The feature allowlist this driver restricts its search to (empty ⇒ full catalogue).
    #[must_use]
    pub fn allowed_features(&self) -> &[u16] {
        &self.allowed_features
    }

    /// Read-only access to the underlying bandit (selection probabilities / per-operator credit).
    #[must_use]
    pub fn selector(&self) -> &OperatorSelector {
        &self.selector
    }

    /// The direction this driver evolves.
    #[must_use]
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Run one adaptive step: select an operator, produce an offspring, evaluate it with `eval`, insert
    /// it, and credit the operator with the outcome. Deterministic for a given `rng` state (QE-006).
    pub fn step<F>(
        &mut self,
        archive: &mut MapElitesArchive,
        schema: &FeatureSchema,
        rng: &mut DetRng,
        eval: F,
    ) -> StepReport
    where
        F: Fn(&Genome) -> f64,
    {
        let operator = self.selector.select(rng);

        // Sample a parent elite (none for FreshRandom; cold/empty archive ⇒ fall back to fresh_random).
        let parent = match operator {
            Operator::FreshRandom => None,
            _ => archive.sample_parent_elite(self.direction, rng).cloned(),
        };
        let allowed = self.allowed_features.as_slice();
        let offspring = match (operator, &parent) {
            // Local refine never re-points a feature, so the allowlist is irrelevant there.
            (Operator::LocalRefine, Some(p)) => local_refine(&p.genome, rng, schema),
            (Operator::Explore, Some(p)) => explore_masked(&p.genome, rng, schema, allowed),
            // FreshRandom, or any operator with no parent available (cold start).
            _ => fresh_random_masked(rng, schema, allowed),
        };

        let fitness = eval(&offspring);

        // The elite this insertion would displace, if the offspring's cell is already full — for `gain`.
        let cell = descriptor_for(&offspring, self.direction, schema);
        let displaced = cell
            .as_ref()
            .and_then(|c| archive.direction(self.direction).cell(c))
            .filter(|sub| sub.is_full())
            .and_then(|sub| sub.worst())
            .map(|e| e.fitness);

        let insertion = archive.insert(offspring, fitness);
        let insert_outcome = match self.direction {
            Direction::Long => insertion.long,
            Direction::Short => insertion.short,
        };

        let application = match insert_outcome {
            Some(InsertOutcome::NewCell) => ApplicationOutcome::NewCell,
            Some(InsertOutcome::ImprovedElite) => {
                // Normalised displaced improvement, on a novelty-comparable scale (QE-112 contract).
                let gain = match displaced {
                    Some(worst) if worst != 0.0 => (fitness - worst) / worst.abs(),
                    Some(worst) => fitness - worst,
                    None => fitness,
                };
                ApplicationOutcome::ImprovedElite {
                    gain: gain.max(0.0),
                }
            }
            // Added (joined a non-full Deep-Grid cell) and Rejected earn no credit; None ⇒ no descriptor.
            _ => ApplicationOutcome::NoImprovement,
        };
        self.selector.record(operator, &application);

        StepReport {
            operator,
            insert_outcome,
            application,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genome::REP_VERSION;
    use crate::operator::{OperatorSelector, OPERATORS};
    use qe_determinism::task_rng;
    use qe_signal::CatalogueConfig;

    fn schema() -> FeatureSchema {
        FeatureSchema::from_catalogue(&CatalogueConfig { states: 5 })
    }

    fn idx_of(schema: &FeatureSchema, id: &str) -> u16 {
        schema
            .ids()
            .iter()
            .position(|s| s == id)
            .map(|p| p as u16)
            .unwrap_or_else(|| panic!("indicator {id} not in catalogue"))
    }

    fn clause(enabled: bool, feature: u16, lo: u16, hi: u16) -> Clause {
        Clause {
            enabled,
            feature,
            lo,
            hi,
        }
    }

    fn genome_with(
        long_feats: &[u16],
        short_feats: &[u16],
        max_holding_bars: u16,
        size_bps: u16,
    ) -> Genome {
        let bank = |feats: &[u16]| {
            let mut clauses = [clause(false, 0, 0, 0); CLAUSES_PER_SET];
            for (slot, &f) in clauses.iter_mut().zip(feats.iter()) {
                *slot = clause(true, f, 1, 2);
            }
            RuleSet {
                clauses,
                min_satisfied: 1,
            }
        };
        Genome {
            version: REP_VERSION,
            long_entry: bank(long_feats),
            short_entry: bank(short_feats),
            exit: ExitParams {
                max_holding_bars,
                exit_on_opposite: true,
            },
            risk: RiskParams { size_bps },
        }
    }

    #[test]
    fn operators_repair_to_validity_and_local_refine_preserves_cell() {
        let s = schema();
        let parent = genome_with(
            &[idx_of(&s, "rsi_14")],
            &[idx_of(&s, "funding_state")],
            10,
            5_000,
        );
        let parent_cell = descriptor_for(&parent, Direction::Long, &s);

        let mut rng = task_rng(1, 0);
        for _ in 0..200 {
            let refined = local_refine(&parent, &mut rng, &s);
            assert!(refined.is_valid(&s));
            // Cell-preserving: same descriptor as the parent.
            assert_eq!(descriptor_for(&refined, Direction::Long, &s), parent_cell);

            let explored = explore(&parent, &mut rng, &s);
            assert!(explored.is_valid(&s));

            let fresh = fresh_random(&mut rng, &s);
            assert!(fresh.is_valid(&s));
            // FreshRandom enables clause 0 in each bank → has a descriptor in at least one direction.
            assert!(
                descriptor_for(&fresh, Direction::Long, &s).is_some()
                    || descriptor_for(&fresh, Direction::Short, &s).is_some()
            );
        }
    }

    #[test]
    fn step_is_deterministic_for_a_fixed_seed() {
        let s = schema();
        let eval = |g: &Genome| g.risk.size_bps as f64;

        let run = || {
            let mut arc = MapElitesArchive::new(schema());
            let mut driver =
                VariationDriver::new(OperatorSelector::with_defaults(), Direction::Long);
            let mut rng = task_rng(123, 0);
            let mut ops = Vec::new();
            for _ in 0..100 {
                ops.push(driver.step(&mut arc, &s, &mut rng, eval).operator);
            }
            (ops, driver.selector().probabilities())
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn budget_shifts_toward_exploration_on_a_sparse_archive() {
        let s = schema();
        let eval = |g: &Genome| g.risk.size_bps as f64;
        // Sparse: a handful of parents in distinct cells, most of the 45 cells empty.
        let mut arc = MapElitesArchive::new(schema());
        for (feat, hold) in [
            ("rsi_14", 3u16),
            ("ema_ratio_20", 30),
            ("atr_pct_14", 60),
            ("cmf_20", 12),
        ] {
            arc.insert(genome_with(&[idx_of(&s, feat)], &[], hold, 5_000), 5_000.0);
        }

        let mut driver = VariationDriver::new(OperatorSelector::with_defaults(), Direction::Long);
        let mut rng = task_rng(7, 0);
        for _ in 0..50 {
            driver.step(&mut arc, &s, &mut rng, eval);
        }

        let p = driver.selector().probabilities();
        // OPERATORS order: [LocalRefine, Explore, FreshRandom].
        let (local, explore_p, fresh) = (p[0], p[1], p[2]);
        assert!(
            explore_p + fresh > local,
            "exploration budget should exceed exploitation on a sparse archive: {p:?}"
        );
        assert!(
            driver.selector().credit(Operator::FreshRandom)
                > driver.selector().credit(Operator::LocalRefine),
            "FreshRandom must out-earn LocalRefine when cells are empty"
        );
    }

    #[test]
    fn budget_shifts_toward_exploitation_on_a_dense_archive() {
        let s = schema();
        // Smooth single-optimum landscape peaking at size_bps = 9000: local refinement reliably climbs,
        // random jumps usually cannot beat a near-optimal elite.
        let peak = 9_000.0_f64;
        let eval = move |g: &Genome| -((g.risk.size_bps as f64) - peak).powi(2);

        // Dense: saturate the reachable cells with near-optimal elites (size 8500, just below the peak)
        // so exploration finds essentially no empty niche to claim. fresh_random produces multi-clause
        // genomes, so it reaches the multi-clause-descriptor cells too.
        let mut arc = MapElitesArchive::new(schema());
        let mut fill_rng = task_rng(100, 0);
        for _ in 0..30_000 {
            let mut g = fresh_random(&mut fill_rng, &s);
            g.risk.size_bps = 8_500;
            let f = eval(&g);
            arc.insert(g, f);
        }
        // Sanity: the long archive is saturated (reachable cells full) — record the coverage achieved.
        let occupied = arc.direction(Direction::Long).len();
        assert!(
            occupied >= 30,
            "expected a saturated long archive, got {occupied} cells"
        );

        let mut driver = VariationDriver::new(OperatorSelector::with_defaults(), Direction::Long);
        let mut rng = task_rng(11, 0);
        for _ in 0..400 {
            driver.step(&mut arc, &s, &mut rng, eval);
        }

        let p = driver.selector().probabilities();
        let (local, explore_p, fresh) = (p[0], p[1], p[2]);
        assert!(
            local > explore_p && local > fresh,
            "exploitation budget should dominate on a dense archive: {p:?}"
        );
        let sel = driver.selector();
        assert!(
            sel.credit(Operator::LocalRefine) > sel.credit(Operator::Explore)
                && sel.credit(Operator::LocalRefine) > sel.credit(Operator::FreshRandom),
            "LocalRefine must out-earn the jumpers on a dense archive"
        );
    }

    #[test]
    fn insert_outcome_maps_to_credit_consistently_over_a_run() {
        // Over a real run, every step's recorded ApplicationOutcome must match the InsertOutcome→credit
        // rule: NewCell→1.0, Added/Rejected/None→0.0, ImprovedElite→≥0. This pins the `Added`→0 decision.
        let s = schema();
        let eval = |g: &Genome| g.risk.size_bps as f64;
        let mut arc = MapElitesArchive::new(schema());
        let mut driver = VariationDriver::new(OperatorSelector::with_defaults(), Direction::Long);
        let mut rng = task_rng(5, 0);

        let mut saw_added = false;
        let mut saw_new_cell = false;
        for _ in 0..300 {
            let r = driver.step(&mut arc, &s, &mut rng, eval);
            match r.insert_outcome {
                Some(InsertOutcome::NewCell) => {
                    saw_new_cell = true;
                    assert_eq!(r.application, ApplicationOutcome::NewCell);
                    assert_eq!(r.application.reward(), 1.0);
                }
                Some(InsertOutcome::Added) => {
                    saw_added = true;
                    assert_eq!(r.application, ApplicationOutcome::NoImprovement);
                    assert_eq!(r.application.reward(), 0.0);
                }
                Some(InsertOutcome::Rejected) | None => {
                    assert_eq!(r.application, ApplicationOutcome::NoImprovement);
                }
                Some(InsertOutcome::ImprovedElite) => {
                    assert!(matches!(
                        r.application,
                        ApplicationOutcome::ImprovedElite { .. }
                    ));
                    assert!(r.application.reward() >= 0.0);
                }
            }
        }
        // The run genuinely exercised both the novelty and the no-credit `Added` paths.
        assert!(saw_new_cell, "expected at least one NewCell over the run");
        assert!(
            saw_added,
            "expected at least one Added (no-credit) over the run"
        );
        let _ = OPERATORS;
    }

    // QE-458: an EMPTY allowlist reproduces the un-steered draw sequence exactly — the golden invariant.
    #[test]
    fn empty_allowlist_is_byte_identical_to_the_unmasked_search() {
        let s = schema();
        // fresh_random vs fresh_random_masked(&[]) from the same seed ⇒ identical genome.
        let mut a = task_rng(42, 0);
        let mut b = task_rng(42, 0);
        for _ in 0..200 {
            assert_eq!(
                fresh_random(&mut a, &s),
                fresh_random_masked(&mut b, &s, &[]),
                "empty allowlist must not shift the deterministic draw sequence"
            );
        }
        // explore vs explore_masked(&[]) from the same seed ⇒ identical genome.
        let mut pa = task_rng(9, 0);
        let parent = fresh_random(&mut pa, &s);
        let mut c = task_rng(7, 0);
        let mut d = task_rng(7, 0);
        for _ in 0..200 {
            assert_eq!(
                explore(&parent, &mut c, &s),
                explore_masked(&parent, &mut d, &s, &[]),
            );
        }
    }

    // QE-458: a NON-EMPTY allowlist confines every referenced feature to the allowed set — the steered
    // search genuinely explores a restricted hypothesis space, and (leak-free) `repair` never re-introduces
    // a disallowed feature. This is what makes indicator-subset steering real (AC 4) and honest (AC a/e).
    #[test]
    fn allowlist_confines_every_referenced_feature_to_the_subset() {
        let s = schema();
        // A small subset of catalogue indices (well within the full width).
        let allowed: Vec<u16> = vec![0, 3, 7];
        let allowed_set: std::collections::BTreeSet<u16> = allowed.iter().copied().collect();
        let mut rng = task_rng(2024, 0);
        // fresh_random_masked: every clause feature (enabled or not) is drawn from the allowlist.
        for _ in 0..500 {
            let g = fresh_random_masked(&mut rng, &s, &allowed);
            for set in [&g.long_entry, &g.short_entry] {
                for c in &set.clauses {
                    assert!(
                        allowed_set.contains(&c.feature),
                        "fresh_random_masked leaked feature {} not in {allowed:?}",
                        c.feature
                    );
                }
            }
        }
        // explore_masked: re-pointed clauses also stay within the allowlist; referenced (enabled) features
        // over a long chain remain a subset of the allowed set.
        let mut parent = fresh_random_masked(&mut rng, &s, &allowed);
        for _ in 0..500 {
            parent = explore_masked(&parent, &mut rng, &s, &allowed);
            assert!(
                parent.referenced_features().is_subset(&allowed_set),
                "explore_masked leaked a referenced feature outside {allowed:?}: {:?}",
                parent.referenced_features()
            );
        }
    }

    // QE-458: the driver-level allowlist restricts a full MAP-Elites run's occupied niches to the families
    // the subset can express — a driver with an empty allowlist is byte-identical to today.
    #[test]
    fn driver_with_allowed_features_restricts_the_run() {
        let s = schema();
        let allowed: Vec<u16> = vec![0, 1, 2];
        let mut steered = VariationDriver::new(OperatorSelector::with_defaults(), Direction::Long)
            .with_allowed_features(allowed.clone());
        assert_eq!(steered.allowed_features(), allowed.as_slice());
        let allowed_set: std::collections::BTreeSet<u16> = allowed.iter().copied().collect();
        let mut archive = MapElitesArchive::new(s.clone());
        let mut rng = task_rng(55, 0);
        let eval = |_: &Genome| 0.5_f64;
        for _ in 0..300 {
            let _ = steered.step(&mut archive, &s, &mut rng, eval);
        }
        // Every elite that landed in the archive references only allowed features.
        let dir_archive = archive.direction(Direction::Long);
        let cells: Vec<_> = dir_archive.occupied_cells().cloned().collect();
        assert!(
            !cells.is_empty(),
            "the steered run should occupy some niche"
        );
        for cell in &cells {
            for e in dir_archive.cell(cell).unwrap().elites() {
                assert!(
                    e.genome.referenced_features().is_subset(&allowed_set),
                    "a steered elite referenced a feature outside the allowlist"
                );
            }
        }
    }
}
