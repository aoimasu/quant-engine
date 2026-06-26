//! AC #1 — a stage produces byte-identical output independent of core/thread count, via fixed task
//! ordering and deterministic (fixed-order) reductions. These tests run representative parallel
//! stages at 1 vs N rayon threads and assert the bytes match exactly.

use qe_determinism::{reproduce, task_rng};
use rand_core::RngCore;
use rayon::prelude::*;

/// Run `f` inside a rayon pool of exactly `threads` worker threads.
fn with_threads<T, F>(threads: usize, f: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("thread pool builds")
        .install(f)
}

/// A toy parallel stage: each task draws one `u64` from its own per-index RNG; results are
/// collected in fixed index order and serialised little-endian. The output depends only on
/// `(master, n)` — not on how the `n` tasks were spread across threads.
fn parallel_draw(master: u64, n: usize, threads: usize) -> Vec<u8> {
    with_threads(threads, || {
        (0..n)
            .into_par_iter()
            .map(|i| task_rng(master, i as u64).next_u64())
            .collect::<Vec<u64>>() // IndexedParallelIterator::collect preserves index order
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect()
    })
}

#[test]
fn parallel_stage_is_byte_identical_across_thread_counts() {
    let one = parallel_draw(0x00C0_FFEE, 4096, 1);
    let many = parallel_draw(0x00C0_FFEE, 4096, 8);
    assert_eq!(one, many, "per-task seeding must not depend on core count");
    assert_eq!(one.len(), 4096 * 8);
}

/// A deterministic reduction: `f64`s are computed in parallel but folded **sequentially in fixed
/// index order**, so the (non-associative) floating-point sum is bit-stable regardless of how the
/// parallel map was scheduled.
fn parallel_float_sum(master: u64, n: usize, threads: usize) -> u64 {
    const SCALE: f64 = 1.0 / (1u64 << 53) as f64;
    with_threads(threads, || {
        let vals: Vec<f64> = (0..n)
            .into_par_iter()
            .map(|i| (task_rng(master, i as u64).next_u64() >> 11) as f64 * SCALE)
            .collect();
        // Sequential fold over the fixed-order Vec: the reduction order is data, not schedule.
        vals.iter().sum::<f64>().to_bits()
    })
}

#[test]
fn deterministic_reduction_is_bit_stable_across_thread_counts() {
    let a = parallel_float_sum(7, 10_000, 1);
    let b = parallel_float_sum(7, 10_000, 16);
    assert_eq!(
        a, b,
        "fixed-order reduction must be bit-identical regardless of core count"
    );
}

#[test]
fn parallel_stage_reproduces_via_harness() {
    // The harness re-runs the (multi-threaded) stage twice and confirms byte-identical artefacts.
    let out = reproduce(|| parallel_draw(1, 256, 4)).expect("parallel stage is reproducible");
    assert_eq!(out.len(), 256 * 8);
}
