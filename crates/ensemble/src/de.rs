//! Discrete differential-evolution operators over the strategy pool (QE-115/D1–D2).
//!
//! An ensemble is a fixed-length binary [`EnsembleMask`] over the pool (bit `i` = "strategy `i` is a
//! member"). The operators are the binary analogue of classical DE: a parameter-free [`de_mutant`]
//! (`a XOR (b XOR c)`) and a [`binomial_crossover`] driven by a seeded RNG (QE-006). The DE search
//! *loop* (selection, generations) is QE-126; this module fixes the operator surface.

use rand_core::RngCore;

/// Default crossover rate for [`binomial_crossover`].
pub const DEFAULT_CR: f64 = 0.9;

/// A binary inclusion mask over the strategy pool — one ensemble candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsembleMask(pub Vec<bool>);

impl EnsembleMask {
    /// The pool size this mask is defined over.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the mask spans an empty pool.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The number of selected members.
    #[must_use]
    pub fn count(&self) -> usize {
        self.0.iter().filter(|b| **b).count()
    }

    /// The selected member indices (ascending) — the form the [`crate::objective`] consumes.
    #[must_use]
    pub fn members(&self) -> Vec<usize> {
        self.0
            .iter()
            .enumerate()
            .filter_map(|(i, &b)| b.then_some(i))
            .collect()
    }
}

/// Binary DE mutant `v = a XOR (b XOR c)` — the analogue of `a + (b − c)`: the loci where donors `b`
/// and `c` **differ** are the "difference vector", toggled onto base `a`. Deterministic and
/// parameter-free. Truncated to the shortest donor length.
#[must_use]
pub fn de_mutant(a: &EnsembleMask, b: &EnsembleMask, c: &EnsembleMask) -> EnsembleMask {
    let n = a.len().min(b.len()).min(c.len());
    let v = (0..n).map(|i| a.0[i] ^ (b.0[i] ^ c.0[i])).collect();
    EnsembleMask(v)
}

/// Binomial (uniform) crossover of `target` and `mutant` (QE-115/D2): each locus takes the mutant bit
/// with probability `cr`, else the target bit — except a single guaranteed locus `j_rand` always taken
/// from the mutant, so the trial differs from the target. Deterministic for a given RNG state.
/// Truncated to the shorter length.
pub fn binomial_crossover<R: RngCore>(
    target: &EnsembleMask,
    mutant: &EnsembleMask,
    cr: f64,
    rng: &mut R,
) -> EnsembleMask {
    let n = target.len().min(mutant.len());
    if n == 0 {
        return EnsembleMask(Vec::new());
    }
    let cr = cr.clamp(0.0, 1.0);
    let j_rand = (rng.next_u64() % n as u64) as usize;
    let v = (0..n)
        .map(|i| {
            let u = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
            if i == j_rand || u < cr {
                mutant.0[i]
            } else {
                target.0[i]
            }
        })
        .collect();
    EnsembleMask(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_determinism::seed_rng;

    fn mask(bits: &[u8]) -> EnsembleMask {
        EnsembleMask(bits.iter().map(|b| *b != 0).collect())
    }

    #[test]
    fn mask_members_and_count() {
        let m = mask(&[1, 0, 1, 1, 0]);
        assert_eq!(m.len(), 5);
        assert_eq!(m.count(), 3);
        assert_eq!(m.members(), vec![0, 2, 3]);
        assert!(!m.is_empty());
    }

    #[test]
    fn de_mutant_toggles_the_difference_loci() {
        let a = mask(&[1, 1, 0, 0]);
        // b == c → difference is empty → mutant equals a.
        let bc = mask(&[1, 0, 1, 0]);
        assert_eq!(de_mutant(&a, &bc, &bc), a);
        // b and c differ at loci 0 and 3 → those toggle on a.
        let b = mask(&[1, 0, 1, 1]);
        let c = mask(&[0, 0, 1, 0]);
        // diff = b XOR c = [1,0,0,1]; a XOR diff = [0,1,0,1].
        assert_eq!(de_mutant(&a, &b, &c), mask(&[0, 1, 0, 1]));
    }

    #[test]
    fn crossover_cr_one_is_all_mutant_and_deterministic() {
        let target = mask(&[0, 0, 0, 0]);
        let mutant = mask(&[1, 1, 1, 1]);
        let mut rng = seed_rng(1);
        let trial = binomial_crossover(&target, &mutant, 1.0, &mut rng);
        assert_eq!(trial, mutant); // cr = 1 → every locus from the mutant
                                   // Reproducible for a fixed seed.
        let mut a = seed_rng(99);
        let mut b = seed_rng(99);
        assert_eq!(
            binomial_crossover(&target, &mutant, 0.5, &mut a),
            binomial_crossover(&target, &mutant, 0.5, &mut b)
        );
    }

    #[test]
    fn crossover_cr_zero_still_inherits_one_mutant_locus() {
        let target = mask(&[0, 0, 0, 0, 0, 0]);
        let mutant = mask(&[1, 1, 1, 1, 1, 1]);
        let mut rng = seed_rng(7);
        let trial = binomial_crossover(&target, &mutant, 0.0, &mut rng);
        // cr = 0 → only the guaranteed j_rand locus comes from the mutant.
        assert_eq!(
            trial.count(),
            1,
            "exactly the j_rand locus is taken from the mutant"
        );
    }

    #[test]
    fn empty_pool_crossover_is_empty() {
        let mut rng = seed_rng(3);
        assert_eq!(
            binomial_crossover(&EnsembleMask(vec![]), &EnsembleMask(vec![]), 0.9, &mut rng),
            EnsembleMask(vec![])
        );
    }
}
