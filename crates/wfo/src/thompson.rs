//! Thompson-sampling parent selection (QE-121) — Bayesian parent-niche selection under fitness
//! uncertainty, with a strictly **in-training** reward channel (no OOS leakage).
//!
//! Each occupied [`Cell`](crate::archive::Cell) is a bandit arm with a conjugate Normal posterior over
//! its mean in-training reward ([`NichePrior`]). [`ThompsonParentSelector::select_parent`] draws one
//! posterior sample per occupied cell, picks the argmax niche, and samples an elite within it — so
//! parent budget concentrates on productive niches while the posterior variance keeps exploring uncertain
//! ones. The **only** way to update a posterior is [`record`](ThompsonParentSelector::record) with an
//! [`ApplicationOutcome`] — the QE-112/119 in-training novelty / elite-improvement credit. There is no
//! parameter or path by which a held-out / validation score can enter the bandit (QE-001/QE-132
//! firewall); the leakage test makes that observable.

use std::collections::BTreeMap;
use std::f64::consts::TAU;

use qe_determinism::DetRng;
use qe_domain::Direction;
use rand_core::RngCore;

use crate::archive::Cell;
use crate::genome::Genome;
use crate::mapelites::MapElitesArchive;
use crate::operator::ApplicationOutcome;

/// Default prior mean reward of an unseen niche.
pub const DEFAULT_PRIOR_MEAN: f64 = 0.0;
/// Default prior variance of an unseen niche (optimism / exploration of unseen niches).
pub const DEFAULT_PRIOR_VAR: f64 = 1.0;
/// Default per-observation reward variance.
pub const DEFAULT_OBS_VAR: f64 = 1.0;

/// Floor on a prior's `var` / `obs_var`. A sane positive minimum (not `f64::MIN_POSITIVE`) so that
/// `1/var` cannot overflow to a non-finite precision even if a caller passes a near-zero variance.
const MIN_VARIANCE: f64 = 1e-12;

/// The Normal–Normal prior + observation model for a niche's mean reward.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NichePrior {
    /// Prior mean reward.
    pub mean: f64,
    /// Prior variance (`> 0`).
    pub var: f64,
    /// Per-observation variance (`> 0`).
    pub obs_var: f64,
}

impl Default for NichePrior {
    fn default() -> Self {
        NichePrior {
            mean: DEFAULT_PRIOR_MEAN,
            var: DEFAULT_PRIOR_VAR,
            obs_var: DEFAULT_OBS_VAR,
        }
    }
}

/// Running sufficient statistics for one niche: observation count and reward sum.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct NichePosterior {
    n: f64,
    sum: f64,
}

/// Thompson-sampling parent selector over the archive's niches.
#[derive(Debug, Clone)]
pub struct ThompsonParentSelector {
    prior: NichePrior,
    posteriors: BTreeMap<Cell, NichePosterior>,
}

impl ThompsonParentSelector {
    /// Build a selector with an explicit prior (`var`/`obs_var` floored to a small positive).
    #[must_use]
    pub fn new(prior: NichePrior) -> Self {
        let prior = NichePrior {
            mean: prior.mean,
            var: prior.var.max(MIN_VARIANCE),
            obs_var: prior.obs_var.max(MIN_VARIANCE),
        };
        ThompsonParentSelector {
            prior,
            posteriors: BTreeMap::new(),
        }
    }

    /// A selector with the QE-121 default prior.
    #[must_use]
    pub fn with_defaults() -> Self {
        ThompsonParentSelector::new(NichePrior::default())
    }

    /// Credit a niche with one application's **in-training** reward (the only update path — no OOS input).
    pub fn record(&mut self, cell: Cell, outcome: &ApplicationOutcome) {
        let p = self.posteriors.entry(cell).or_default();
        p.n += 1.0;
        p.sum += outcome.reward();
    }

    /// Posterior `(mean, variance)` of a niche's mean reward from its sufficient statistics.
    fn posterior_params(&self, cell: &Cell) -> (f64, f64) {
        let post = self.posteriors.get(cell).copied().unwrap_or_default();
        let prior_precision = 1.0 / self.prior.var;
        let data_precision = post.n / self.prior.obs_var;
        let precision = prior_precision + data_precision;
        let mean = (self.prior.mean * prior_precision + post.sum / self.prior.obs_var) / precision;
        (mean, 1.0 / precision)
    }

    /// Posterior mean reward of a niche (the prior mean if unseen) — for inspection / tests.
    #[must_use]
    pub fn posterior_mean(&self, cell: &Cell) -> f64 {
        self.posterior_params(cell).0
    }

    /// Draw one Thompson sample of a niche's mean reward.
    fn sampled_value(&self, cell: &Cell, rng: &mut DetRng) -> f64 {
        let (mean, var) = self.posterior_params(cell);
        mean + var.sqrt() * standard_normal(rng)
    }

    /// Thompson-select a parent in `direction`: sample each occupied niche's posterior, take the argmax
    /// niche (ties by sorted `Cell` order), then sample an elite uniformly within it. `None` if that
    /// direction's archive is empty. Deterministic for a given `rng` state.
    #[must_use]
    pub fn select_parent<'a>(
        &self,
        archive: &'a MapElitesArchive,
        direction: Direction,
        rng: &mut DetRng,
    ) -> Option<(Cell, &'a Genome)> {
        let dir = archive.direction(direction);
        let mut best: Option<(f64, Cell)> = None;
        for cell in dir.occupied_cells() {
            let v = self.sampled_value(cell, rng);
            if best.is_none_or(|(bv, _)| v > bv) {
                best = Some((v, *cell));
            }
        }
        let (_, cell) = best?;
        let elites = dir.cell(&cell)?.elites();
        if elites.is_empty() {
            return None;
        }
        let idx = (rng.next_u64() % elites.len() as u64) as usize;
        Some((cell, &elites[idx].genome))
    }
}

/// A standard-normal draw via Box–Muller from one seeded RNG (two uniforms).
fn standard_normal(rng: &mut DetRng) -> f64 {
    let u1 = uniform01(rng).max(f64::MIN_POSITIVE); // avoid ln(0)
    let u2 = uniform01(rng);
    (-2.0 * u1.ln()).sqrt() * (TAU * u2).cos()
}

/// Uniform in `[0, 1)` from the 53 high bits of one `u64` draw.
fn uniform01(rng: &mut DetRng) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genome::{
        Clause, ExitParams, Genome, RiskParams, RuleSet, CLAUSES_PER_SET, REP_VERSION,
    };
    use crate::mapelites::MapElitesArchive;
    use crate::{descriptor_for, ApplicationOutcome};
    use qe_determinism::task_rng;
    use qe_signal::{CatalogueConfig, FeatureSchema};

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

    fn genome_with(long_feats: &[u16], hold: u16) -> Genome {
        let mut clauses = [Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        }; CLAUSES_PER_SET];
        for (slot, &f) in clauses.iter_mut().zip(long_feats.iter()) {
            *slot = Clause {
                enabled: true,
                feature: f,
                lo: 1,
                hi: 2,
            };
        }
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

    /// Build a two-niche archive (cells A and B) and return their cells.
    fn two_niche_archive(s: &FeatureSchema) -> (MapElitesArchive, Cell, Cell) {
        let mut arc = MapElitesArchive::new(schema());
        let g_a = genome_with(&[idx_of(s, "ema_ratio_20")], 3); // Trend niche
        let g_b = genome_with(&[idx_of(s, "rsi_14")], 3); // Momentum niche
        let cell_a = descriptor_for(&g_a, Direction::Long, s).unwrap();
        let cell_b = descriptor_for(&g_b, Direction::Long, s).unwrap();
        assert_ne!(cell_a, cell_b);
        arc.insert(g_a, 1.0);
        arc.insert(g_b, 1.0);
        (arc, cell_a, cell_b)
    }

    #[test]
    fn parent_selection_uses_no_validation_signal() {
        // THE LEAKAGE TEST (AC). In-training reward and validation are ANTI-correlated:
        //   cell A — in-training PRODUCTIVE, validation POOR.
        //   cell B — in-training UNPRODUCTIVE, validation EXCELLENT.
        // The selector is fed ONLY in-training outcomes; if any OOS/validation signal leaked into the
        // bandit, selection would track B. It tracks A → no validation signal is used.
        let s = schema();
        let (arc, cell_a, cell_b) = two_niche_archive(&s);

        // A held-out/validation score that strongly favours B. It is defined here and *never* passed to
        // the selector — there is no API that accepts it.
        let _validation_favouring_b = [(cell_a, -10.0_f64), (cell_b, 10.0_f64)];

        let mut sel = ThompsonParentSelector::with_defaults();
        for _ in 0..30 {
            // Only the in-training outcome is recorded.
            sel.record(cell_a, &ApplicationOutcome::ImprovedElite { gain: 1.0 });
            sel.record(cell_b, &ApplicationOutcome::NoImprovement);
        }

        let mut rng = task_rng(42, 0);
        let (mut a, mut b) = (0u32, 0u32);
        for _ in 0..400 {
            let (cell, _) = sel.select_parent(&arc, Direction::Long, &mut rng).unwrap();
            if cell == cell_a {
                a += 1;
            } else if cell == cell_b {
                b += 1;
            }
        }
        // Selection concentrates on the in-training winner A, despite validation favouring B.
        assert!(
            a > b * 5,
            "Thompson must follow in-training reward (A), not validation (B): a={a} b={b}"
        );
    }

    #[test]
    fn reward_currency_is_the_in_training_outcome() {
        let s = schema();
        let (_, cell_a, cell_b) = two_niche_archive(&s);
        let mut sel = ThompsonParentSelector::with_defaults();
        for _ in 0..10 {
            sel.record(cell_a, &ApplicationOutcome::NewCell); // productive
            sel.record(cell_b, &ApplicationOutcome::NoImprovement); // not
        }
        // NewCell / ImprovedElite raise a niche's posterior mean; NoImprovement leaves it at the prior.
        assert!(sel.posterior_mean(&cell_a) > sel.posterior_mean(&cell_b));
        assert!((sel.posterior_mean(&cell_b) - DEFAULT_PRIOR_MEAN).abs() < 1e-12);
        assert!(sel.posterior_mean(&cell_a) > DEFAULT_PRIOR_MEAN);
    }

    #[test]
    fn concentrates_on_high_reward_niche_and_is_deterministic() {
        let s = schema();
        let (arc, cell_a, cell_b) = two_niche_archive(&s);
        let mut sel = ThompsonParentSelector::with_defaults();
        for _ in 0..25 {
            sel.record(cell_a, &ApplicationOutcome::ImprovedElite { gain: 2.0 });
            sel.record(cell_b, &ApplicationOutcome::NoImprovement);
        }

        let count_a = |seed: u64| {
            let mut rng = task_rng(seed, 0);
            (0..300)
                .filter(|_| {
                    sel.select_parent(&arc, Direction::Long, &mut rng)
                        .unwrap()
                        .0
                        == cell_a
                })
                .count()
        };
        let a1 = count_a(7);
        assert!(a1 > 200, "high-reward niche should dominate: {a1}/300");
        // Deterministic for a fixed seed.
        assert_eq!(count_a(7), a1);
    }

    #[test]
    fn empty_archive_yields_none_and_single_cell_is_always_returned() {
        let s = schema();
        let empty = MapElitesArchive::new(schema());
        let sel = ThompsonParentSelector::with_defaults();
        let mut rng = task_rng(1, 0);
        assert!(sel
            .select_parent(&empty, Direction::Long, &mut rng)
            .is_none());

        // One occupied cell ⇒ always that cell.
        let mut arc = MapElitesArchive::new(schema());
        let g = genome_with(&[idx_of(&s, "ema_ratio_20")], 3);
        let cell = descriptor_for(&g, Direction::Long, &s).unwrap();
        arc.insert(g, 1.0);
        for _ in 0..20 {
            assert_eq!(
                sel.select_parent(&arc, Direction::Long, &mut rng)
                    .unwrap()
                    .0,
                cell
            );
        }
    }

    #[test]
    fn degenerate_prior_variance_stays_finite() {
        // A caller passing a (near-)zero variance is floored to MIN_VARIANCE, so `1/var` cannot blow up
        // to a non-finite precision and selection stays finite/deterministic — no NaN footgun.
        let s = schema();
        let (arc, cell_a, _cell_b) = two_niche_archive(&s);
        let mut sel = ThompsonParentSelector::new(NichePrior {
            mean: 0.0,
            var: 0.0,
            obs_var: 0.0,
        });
        sel.record(cell_a, &ApplicationOutcome::ImprovedElite { gain: 1.0 });
        assert!(sel.posterior_mean(&cell_a).is_finite());
        let mut rng = task_rng(3, 0);
        for _ in 0..50 {
            let picked = sel.select_parent(&arc, Direction::Long, &mut rng);
            assert!(picked.is_some());
        }
    }
}
