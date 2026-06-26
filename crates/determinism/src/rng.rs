//! Deterministic, seedable RNG plumbing.
//!
//! Every stochastic stage (QD operators, DE, Thompson sampling) draws from a [`DetRng`] seeded
//! from the run's master seed (`config.determinism.seed`). To stay reproducible *independent of
//! core/thread count*, parallel work must never share one generator — draw order would then depend
//! on scheduling. Instead each task derives its own generator from `(master, index)` via
//! [`task_rng`], so a task's stream depends only on its index, never on which thread runs it.

use rand_chacha::ChaCha8Rng;
use rand_core::SeedableRng;

/// The project's deterministic RNG.
///
/// ChaCha8 is fast, has no machine-dependent state, and yields identical streams on every platform
/// — a precondition for the byte-identical-artefact guarantee. Avoid `std`'s `ThreadRng` and any
/// hash-randomised iteration in stages that must be reproducible.
pub type DetRng = ChaCha8Rng;

/// Seed a [`DetRng`] from a single 64-bit seed.
#[must_use]
pub fn seed_rng(seed: u64) -> DetRng {
    DetRng::seed_from_u64(seed)
}

/// Derive a child seed from a master seed and a task index.
///
/// SplitMix64 mixing decorrelates neighbouring indices, so per-task streams do not overlap or
/// align. Pure and portable: the same `(master, index)` always maps to the same seed on every
/// machine.
#[must_use]
pub fn derive_seed(master: u64, index: u64) -> u64 {
    splitmix64(master ^ splitmix64(index))
}

/// Seed a private [`DetRng`] for the task at `index` under `master`.
///
/// The returned stream depends only on `(master, index)` — never on scheduling — which is what
/// makes parallel stages byte-reproducible regardless of core count. Give each unit of parallel
/// work a stable index and seed it with this.
#[must_use]
pub fn task_rng(master: u64, index: u64) -> DetRng {
    seed_rng(derive_seed(master, index))
}

/// SplitMix64 finaliser (Vigna). A fast, well-distributed bijective mix of a 64-bit value.
fn splitmix64(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::RngCore;

    #[test]
    fn same_seed_same_stream() {
        let mut a = seed_rng(123);
        let mut b = seed_rng(123);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = seed_rng(1);
        let mut b = seed_rng(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn derive_seed_is_pure_and_index_sensitive() {
        assert_eq!(derive_seed(42, 7), derive_seed(42, 7));
        assert_ne!(derive_seed(42, 7), derive_seed(42, 8));
        assert_ne!(derive_seed(42, 7), derive_seed(43, 7));
        // Adjacent indices must not collapse to the same stream.
        assert_ne!(task_rng(0, 0).next_u64(), task_rng(0, 1).next_u64());
    }

    /// Golden values: pin the exact stream/derivation so a `rand_chacha` bump or an accidental
    /// change to `splitmix64`/`task_rng` is caught here rather than silently re-baselining every
    /// vintage. These constants are platform-independent (ChaCha8 + SplitMix64 are pure integer).
    #[test]
    fn golden_stream_and_derivation_are_pinned() {
        assert_eq!(seed_rng(0).next_u64(), 0xb585_f767_a79a_3b6c);
        assert_eq!(derive_seed(0, 0), 0xa706_dd2f_4d19_7e6f);
        assert_eq!(task_rng(0, 0).next_u64(), 0xddfd_9f22_480f_1436);
    }
}
