# QE-123 — Phased recording → Strategy repository — design note

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-114, QE-120`
`Branch: qe-123/phased-strategy-repository`

## Goal (from backlog)

Persist only **exploitation-phase survivors** above the train/CV-derived quality threshold, into the
strategy repository.

- Implement the lifecycle/quality gate (QE-114); write survivors to the strategy repository **with
  lineage**.

**Acceptance criteria.**
- [ ] Early lucky candidates are not persisted; persisted strategies carry lineage.

**Out of scope.** Ensemble construction (QE-126).

## Current-state evidence

This is the **recording stage** that joins three merged pieces:
- **QE-114** (`qe_wfo::lifecycle`) is the admission policy: `QualityGate::persists(fitness, threshold)`
  is `true` only when the candidate is in `Phase::Exploitation` (`fitness.n ≥ min_exploitation_windows`),
  finite, and its **lower confidence bound** `mean − k_sigma·se` clears the `threshold` derived from the
  cohort distribution (`QualityGate::threshold`). That is exactly "exploitation survivor above the
  train/CV-derived bar" — so QE-123 *uses* the gate, it does not re-implement it.
- **QE-120** (`qe_wfo::backtest`) produces the `NoiseRobustFitness` (mean ± SE over `n` windows) the gate
  reads. A candidate evaluated on few windows (`n < min_exploitation_windows`) is still in Exploration —
  the "early lucky" case the AC forbids persisting.
- **QE-006** (`qe_determinism::{Lineage, HasLineage}`) is the provenance the persisted artefact must
  carry: config hash + input snapshot + code commit + seeds, with a resolvable `Lineage::id`.

## Design

### D1 — The persisted record

`StrategyRecord { genome, fitness, lineage }` — the genome (QE-110), its noise-robust fitness summary
(QE-113), and the `Lineage` that produced it (QE-006). It is `serde`-serialisable (so it can be written
durably) and implements `HasLineage`, so every persisted strategy carries a resolvable lineage by
construction (AC #2). `NoiseRobustFitness` gains `serde` derives (additive; persisted records are always
finite, so JSON round-trips cleanly).

### D2 — The repository & the phased gate (AC #1)

`StrategyRepository { gate, records }` wraps a `QualityGate`. The single admission path
`try_record(genome, fitness, threshold, lineage) -> bool` records **iff** `gate.persists(fitness,
threshold)`:
- an **Exploration** candidate (`n < min_exploitation_windows`) is rejected *regardless of how high its
  mean is* — the "early lucky candidate is not persisted" guarantee;
- an Exploitation candidate whose lower confidence bound is below the threshold is rejected (lucky single
  draw, not robust);
- only an Exploitation candidate clearing the bar is appended, tagged with its lineage.

`record_survivors` is the batch form over a cohort. The threshold is derived once from the cohort's
fitness distribution (`gate.threshold(distribution)`), the train/CV-derived bar.

### D3 — Durable persistence

`write_jsonl` / `read_records` serialise the records one JSON object per line (the canonical vintage
form, consistent with QE-110's JSON lineage). They take any `Write` / `BufRead`, so a caller persists to
a file while tests exercise the exact serde + I/O path through an in-memory buffer. Lineage survives the
round-trip, so a reloaded strategy is still auditable/reproducible.

## Module / API plan

New module `crates/wfo/src/strategy_repo.rs`, re-exported:

- `StrategyRecord { genome: Genome, fitness: NoiseRobustFitness, lineage: Lineage }` (`serde`, `HasLineage`).
- `StrategyRepository::{new, with_defaults, gate, records, len, is_empty, try_record, record_survivors, write_jsonl, read_records}`.
- Adds `serde` derives to `NoiseRobustFitness` (QE-113, additive); promotes `serde_json` to a normal
  `qe-wfo` dependency; reuses `qe-determinism` (already a normal dep). No other new deps.

## Test plan (TDD)

1. **Early lucky candidate is not persisted (AC #1).** A high-mean candidate evaluated on `n = 1` window
   (Exploration) is rejected by `try_record`; the same genome evaluated on `n ≥ min_exploitation_windows`
   windows and clearing the bar **is** recorded. A below-threshold exploitation candidate is also
   rejected. Driven through the real QE-120 backtester.
2. **Persisted strategy carries lineage (AC #2).** A recorded `StrategyRecord` exposes `lineage()` with a
   resolvable `Lineage::id`; the lineage matches what was supplied.
3. **Durable round-trip.** `write_jsonl` → `read_records` reproduces the records exactly, lineage
   included.
4. **Batch survivors.** `record_survivors` over a cohort persists exactly the gate's survivors, in order.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-wfo`,
`cargo test --workspace`.

## Risks

- **`NoiseRobustFitness` serde + non-finite.** `serde_json` cannot represent `−∞`/NaN; persisted records
  are always finite (the gate rejects non-finite), so this never bites in practice. Documented; a custom
  finite-or-error serialiser is unnecessary for the recording path.
- **In-process repository.** `records` is in memory; durability is via `write_jsonl` to a caller-owned
  sink (file). A keyed LMDB strategy store (alongside QE-010/011) is a later refinement; the JSONL form is
  the portable, auditable baseline the vintage artefact (QE-129) builds on.
- **Threshold source.** The baseline derives the bar from the supplied cohort distribution (train/CV per
  spec); passing a stressed/OOS distribution is the caller's choice — the gate is distribution-agnostic.
