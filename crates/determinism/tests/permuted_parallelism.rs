//! QE-445 — permuted-parallelism (thread-count-independence) determinism test.
//!
//! The QE-006 harness [`reproduce`] re-runs a stage *under the same conditions* and never varies the
//! thread count, so it exercises only **reproducibility**, not the **scheduling-independence** property
//! `task_rng(master, index)` is designed for (see `crates/determinism/src/rng.rs`). This test turns that
//! design intent into an enforced invariant: it runs a **real stochastic parallel stage** — a MAP-Elites
//! generation from `qe-wfo` (`evaluate_and_insert`, an `rayon` `par_iter` over per-task-seeded evals) —
//! twice under **different rayon thread-pool sizes (1 vs N)** and asserts the serialised genome archive is
//! **byte-identical**.
//!
//! Non-vacuity is proved two ways (see the evidence note
//! `docs/architecture/qe-445-permuted-parallelism-determinism-design.md`):
//!   1. the stage genuinely runs in parallel — the test observes max task-concurrency `>= 2` under the
//!      N-thread pool (not a pool that quietly ran everything serially);
//!   2. it would FAIL if per-task seeding were replaced by a shared/thread-local RNG — a mutation guard
//!      shows the shared-RNG scheme is order-sensitive (hence thread-count-dependent) while the production
//!      `task_rng` scheme is order-invariant.
//!
//! TEST-ONLY: `qe-wfo` / `qe-signal` / `qe-domain` are dev-dependencies (invisible to the firewall, which
//! parses only production dependency tables); no production code or golden is touched.

use std::sync::atomic::{AtomicUsize, Ordering};

use qe_determinism::{reproduce, seed_rng, task_rng, DetRng};
use qe_domain::Direction;
use qe_signal::{
    genome::{Clause, ExitParams, Genome, RiskParams, RuleSet, REP_VERSION},
    CatalogueConfig, FeatureSchema,
};
use qe_wfo::{evaluate_and_insert, MapElitesArchive};
use rand_core::RngCore;
use rayon::ThreadPoolBuilder;

// --- fixtures: a real MAP-Elites generation input ------------------------------------------------

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

fn clause(enabled: bool, feature: u16) -> Clause {
    Clause {
        enabled,
        feature,
        lo: 0,
        hi: 1,
    }
}

fn genome_with(long_feats: &[u16], short_feats: &[u16], max_holding_bars: u16) -> Genome {
    let bank = |feats: &[u16]| {
        let mut clauses = [
            clause(false, 0),
            clause(false, 0),
            clause(false, 0),
            clause(false, 0),
        ];
        for (slot, &f) in clauses.iter_mut().zip(feats.iter()) {
            *slot = clause(true, f);
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
        risk: RiskParams { size_bps: 5_000 },
    }
}

/// A batch that populates *many* niches across both directions — a realistic generation, not a
/// single-cell degenerate case. Cycles distinct catalogue features (⇒ distinct families/timescales) and
/// holding horizons (⇒ distinct holding bands) so the archive spans multiple occupied cells.
fn generation_batch(schema: &FeatureSchema, n: usize) -> Vec<Genome> {
    let longs = [
        idx_of(schema, "ema_ratio_20"),
        idx_of(schema, "rsi_14"),
        idx_of(schema, "atr_pct_14"),
        idx_of(schema, "cmf_20"),
    ];
    let shorts = [idx_of(schema, "funding_state"), idx_of(schema, "oi_roc_10")];
    let holdings = [3u16, 12, 30, 60, 120];
    (0..n)
        .map(|i| {
            genome_with(
                &[longs[i % longs.len()]],
                &[shorts[i % shorts.len()]],
                holdings[i % holdings.len()],
            )
        })
        .collect()
}

// --- the eval: rng-consuming and genome-dependent, with a concurrency observer -------------------

/// A `[0, 1)` draw from one `u64` (same shape as the production `uniform01`).
fn uniform01(rng: &mut DetRng) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// Deterministic, genome-derived component of the fitness — added identically in every scheme, so any
/// artefact divergence is due to the RNG stream, not the genome term.
fn genome_base(g: &Genome) -> f64 {
    f64::from(g.risk.size_bps) + f64::from(g.exit.max_holding_bars)
}

/// Number of RNG draws each task consumes. Large enough that (a) the stream materially shapes the
/// fitness and (b) tasks stay live long enough to overlap under a multi-thread pool.
const DRAWS_PER_TASK: usize = 512;

/// One task's RNG work: consume `DRAWS_PER_TASK` draws from its own stream, folded into the fitness.
fn draw_work(g: &Genome, rng: &mut DetRng) -> f64 {
    let mut acc = 0.0f64;
    for _ in 0..DRAWS_PER_TASK {
        acc += uniform01(rng);
    }
    genome_base(g) + acc
}

/// Build the production-style eval closure, instrumented to observe real task concurrency.
///
/// On entry a task bumps a live-task counter and records the running maximum; a bounded cooperative wait
/// lets a sibling task overlap (reliable multi-thread overlap without deadlock — under a 1-thread pool it
/// simply times out and concurrency stays 1). The closure captures only `&AtomicUsize` (Sync), so it is
/// `Fn + Sync` as `evaluate_and_insert` requires.
fn observed_eval<'a>(
    live: &'a AtomicUsize,
    max_live: &'a AtomicUsize,
) -> impl Fn(&Genome, &mut DetRng) -> f64 + Sync + 'a {
    move |g, rng| {
        let now = live.fetch_add(1, Ordering::SeqCst) + 1;
        max_live.fetch_max(now, Ordering::SeqCst);
        for _ in 0..200_000 {
            if max_live.load(Ordering::SeqCst) >= 2 {
                break;
            }
            std::hint::spin_loop();
        }
        let out = draw_work(g, rng);
        live.fetch_sub(1, Ordering::SeqCst);
        out
    }
}

// --- canonical serialisation of the genome-archive artefact --------------------------------------

/// Serialise a MAP-Elites archive to a canonical byte artefact: both directions in fixed order, each
/// direction's occupied cells in sorted `BTreeMap` order, and per stored `Elite` the genome as canonical
/// JSON (`Genome: Serialize`, the vintage-lineage form) plus the fitness `f64` bit pattern. Byte-equality
/// of this artefact across two archives ⟺ the archives are observationally identical.
fn serialize_archive(arc: &MapElitesArchive) -> Vec<u8> {
    let mut out = Vec::new();
    for (tag, dir) in [(0u8, Direction::Long), (1u8, Direction::Short)] {
        let d = arc.direction(dir);
        out.push(tag);
        out.extend_from_slice(&(d.len() as u64).to_le_bytes());
        for cell in d.occupied_cells() {
            let desc = format!("{cell:?}"); // Cell: Debug is deterministic
            out.extend_from_slice(&(desc.len() as u64).to_le_bytes());
            out.extend_from_slice(desc.as_bytes());
            let sub = d.cell(cell).expect("occupied cell has a sub-population");
            out.extend_from_slice(&(sub.len() as u64).to_le_bytes());
            for e in sub.elites() {
                let g = serde_json::to_vec(&e.genome).expect("genome serialises to JSON");
                out.extend_from_slice(&(g.len() as u64).to_le_bytes());
                out.extend_from_slice(&g);
                out.extend_from_slice(&e.fitness.to_bits().to_le_bytes());
            }
        }
    }
    out
}

/// Run a full MAP-Elites generation (parallel eval + sequential insert) inside a pool of exactly
/// `threads` workers, and return the serialised archive plus the observed max task-concurrency.
fn generation_under(threads: usize, seed: u64, genomes: &[Genome]) -> (Vec<u8>, usize) {
    let live = AtomicUsize::new(0);
    let max_live = AtomicUsize::new(0);
    let mut archive = MapElitesArchive::new(schema());
    let pool = ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("thread pool builds");
    assert_eq!(pool.current_num_threads(), threads);
    pool.install(|| {
        evaluate_and_insert(
            &mut archive,
            seed,
            genomes.to_vec(),
            observed_eval(&live, &max_live),
        );
    });
    (serialize_archive(&archive), max_live.load(Ordering::SeqCst))
}

// --- the AC: byte-identical artefacts across thread counts ---------------------------------------

const SEED: u64 = 0x0000_0445_0000_0006;

#[test]
fn mapelites_generation_is_byte_identical_across_thread_counts() {
    let s = schema();
    let genomes = generation_batch(&s, 160);

    let (bytes_one, conc_one) = generation_under(1, SEED, &genomes);
    let (bytes_many, conc_many) = generation_under(8, SEED, &genomes);

    // The permuted-parallelism invariant: same seeded generation, different pool size, same bytes.
    assert_eq!(
        bytes_one, bytes_many,
        "MAP-Elites generation must emit byte-identical artefacts regardless of rayon pool size"
    );

    // Non-vacuity 1a: a non-trivial artefact over multiple occupied niches (a real generation).
    assert!(
        bytes_many.len() > 256,
        "artefact should be substantial, got {} bytes",
        bytes_many.len()
    );

    // Non-vacuity 1b: the N-thread run genuinely executed tasks concurrently (not degenerate-to-serial),
    // while the 1-thread run stayed serial — so the byte-identity above is earned under real parallelism.
    assert!(
        conc_many >= 2,
        "stage must genuinely run in parallel under 8 threads (observed max concurrency {conc_many})"
    );
    assert_eq!(
        conc_one, 1,
        "the 1-thread pool must run serially (observed max concurrency {conc_one})"
    );
}

#[test]
fn multi_threaded_generation_reproduces_via_the_qe006_harness() {
    // Extend re-run-twice: the QE-006 harness re-runs the *multi-threaded* stage twice and confirms
    // byte-identical artefacts — the original reproducibility guarantee, now on a real parallel stage.
    let s = schema();
    let genomes = generation_batch(&s, 96);
    let (reference, _) = generation_under(8, SEED, &genomes);

    let out = reproduce(|| generation_under(8, SEED, &genomes).0)
        .expect("multi-threaded MAP-Elites generation is reproducible");
    assert_eq!(out, reference);
}

// --- mutation guard: the test FAILS if per-task seeding becomes a shared/thread-local RNG ---------

/// The PRODUCTION scheme: each genome draws from its own `task_rng(master, index)` stream. `order` is the
/// task *execution* order (a permutation of `0..n`); the result is indexed by GENOME index. Because the
/// stream depends only on the genome's index, the result is **independent of `order`** — which is exactly
/// what makes the parallel stage thread-count-independent.
fn per_task_artifact(genomes: &[Genome], master: u64, order: &[usize]) -> Vec<u64> {
    let mut out = vec![0u64; genomes.len()];
    for &gi in order {
        let mut rng = task_rng(master, gi as u64);
        out[gi] = draw_work(&genomes[gi], &mut rng).to_bits();
    }
    out
}

/// The BROKEN scheme this ticket guards against: ONE shared generator, drawn in task *execution* order.
/// The value a genome receives depends on *where in the schedule* its task ran, so the result depends on
/// `order` — and a real thread pool permutes that order relative to a single worker.
fn shared_rng_artifact(genomes: &[Genome], master: u64, order: &[usize]) -> Vec<u64> {
    let mut rng = seed_rng(master);
    let mut out = vec![0u64; genomes.len()];
    for &gi in order {
        out[gi] = draw_work(&genomes[gi], &mut rng).to_bits();
    }
    out
}

#[test]
fn shared_rng_would_break_thread_count_independence() {
    let s = schema();
    let genomes = generation_batch(&s, 64);
    let n = genomes.len();

    // Two execution orders standing in for "one worker" vs "a permuting thread pool".
    let forward: Vec<usize> = (0..n).collect();
    let reversed: Vec<usize> = (0..n).rev().collect();

    // Production per-task seeding: the artefact is INVARIANT under execution order — this is the property
    // that yields byte-identical artefacts across pool sizes.
    assert_eq!(
        per_task_artifact(&genomes, SEED, &forward),
        per_task_artifact(&genomes, SEED, &reversed),
        "per-task task_rng seeding must be execution-order-invariant"
    );

    // Swap in a shared RNG (the regression): the artefact now DEPENDS on execution order, so it would be
    // thread-count-dependent and the byte-identity assertion in the AC test above would fail. This proves
    // that assertion is load-bearing on the per-task seed derivation, not on incidental determinism.
    assert_ne!(
        shared_rng_artifact(&genomes, SEED, &forward),
        shared_rng_artifact(&genomes, SEED, &reversed),
        "a shared/thread-local RNG must be execution-order-sensitive (the guarded-against regression)"
    );
}
