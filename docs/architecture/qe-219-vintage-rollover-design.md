# QE-219 — Vintage load (read-only) + rollover — design note

`Phase: P2` · `Area: ① Vintage inputs` · `Depends on: QE-129, QE-207` · `Branch: qe-219/vintage-rollover`

## Goal (from backlog)

Runtime loads the ensemble repo + calibration profile **read-only** at startup; periodic **rollover** replaces
the vintage in place when training emits a new one.

- **Scope.** Read-only vintage load at startup; **in-place rollover** that swaps repo + calibration when
  training emits a new vintage, **without violating the firewall**.
- **Out of scope.** Training emission (QE-129 writes the vintage; QE-219 only *reads* it).

**Acceptance criteria.**
- [ ] Startup loads a vintage **read-only**; a rollover **swaps it atomically** with **lineage recorded**.

`Spec ref: Runtime — ① Vintage inputs; "Vintage rollover" dashed edge.`

## Current-state evidence & placement

- QE-129 (`crates/vintage/src/lib.rs`): the sealed artefact + store. `Vintage { content, content_hash }` with
  `Vintage::verify()` (recomputes and checks the hash — detects tampering) and `VintageRepository` —
  `load(vintage_id)` opens `<root>/<id>.json` and returns a **hash-verified** `Vintage` (a load never yields an
  unverified vintage), `write(&vintage)`, `path_for(id)`. `VintageContent` carries `chromosomes`, `weights`,
  `calibration: CalibrationProfile`, and `lineage: qe_determinism::Lineage` (config-hash / snapshot / commit /
  seeds).
- QE-207 (`crates/runtime/src/evaluator.rs`): `EvaluatorSession::new(vintage, cfg)` consumes a **sealed
  vintage read-only** and exposes `calibration()`. QE-219 provides the *holder* the session's vintage comes
  from and the rollover that replaces it.
- **Placement: new `crates/runtime/src/vintage_rollover.rs`.** The runtime side owns the active vintage;
  rollover is a runtime concern. It uses `qe-vintage` (already a `qe-runtime` dep) + `qe-determinism`
  (`Lineage`). `qe-determinism` is a **cross-cutting** crate (QE-006), not on either side of the QE-132
  firewall (the rules forbid only the train side — `qe-wfo`/`qe-ensemble` — from reading live crates), and
  `qe-runtime` already reaches it transitively via `qe-vintage`; promoting it to a direct dependency adds **no
  forbidden edge**. The firewall CI test (`qe-architecture --test firewall`) re-proves this.

## Design

### D1 — `ActiveVintage` — the read-only holder + in-place rollover

```rust
pub struct RolloverRecord {
    pub from_vintage_id: String, pub to_vintage_id: String,
    pub from_lineage: Lineage,   pub to_lineage: Lineage,
}

pub struct ActiveVintage { current: Vintage, history: Vec<RolloverRecord> }
```

- **`load(repo, vintage_id) -> Result<Self, VintageError>`** — the startup path: `repo.load(vintage_id)`
  returns a **hash-verified** vintage (read-only — `load` opens the file for reading and never writes), held
  as `current`. Empty history.
- **`from_vintage(v) -> Result<Self, VintageError>`** — same, from an already-in-hand `Vintage` (verifies it
  first). For in-process wiring/tests.
- Read-only accessors: `current() -> &Vintage`, `vintage_id()`, `calibration() -> &CalibrationProfile`,
  `lineage() -> &Lineage`, `history() -> &[RolloverRecord]`. These are what the evaluator session / calibration
  breaker read; nothing here can mutate the vintage except `rollover`.
- **`rollover(next: Vintage) -> Result<&RolloverRecord, VintageError>`** — **verify `next` *before* swapping**;
  on failure the current vintage is untouched and the error propagates (atomic — a bad vintage never becomes
  active, no torn state). On success: record `from→to` (`vintage_id` + `lineage`), then replace `current` in
  place and push the record. Returns the recorded transition.
- **`rollover_from(repo, next_id) -> Result<&RolloverRecord, VintageError>`** — the real periodic path: load
  the new vintage the trainer emitted (`repo.load` verifies it), then `rollover` it.

### D2 — why this is atomic, read-only, and firewall-clean

- **Read-only load.** `VintageRepository::load` only *opens* the file (`File::open`) and verifies the hash; it
  never writes. Startup therefore cannot mutate the repo. A load of a missing id is a clean `Err(Io)`.
- **Atomic swap.** `verify()` runs **before** the single `self.current = next` assignment. Either the new
  vintage verifies and fully replaces the old (one move + one history push), or it does not and the old vintage
  and history are left exactly as they were. There is no intermediate state in which repo and calibration come
  from different vintages — `calibration` lives *inside* `current.content`, so swapping `current` swaps repo +
  calibration together, indivisibly. (Single-threaded runtime; the "atomicity" that matters is verify-before-
  commit, not thread safety.)
- **Lineage recorded.** Every rollover appends a `RolloverRecord` capturing both endpoints' `vintage_id` and
  `Lineage`, so the full chain of what replaced what is retained and auditable.
- **Firewall.** Only a new direct `qe-runtime → qe-determinism` edge (already present transitively); no
  train-side (`qe-wfo`/`qe-ensemble`) edge. QE-132 guard stays green.

## Test plan (deterministic, TDD)

1. `startup_loads_vintage_read_only` (**AC**) — write a sealed vintage to a temp `VintageRepository`;
   `ActiveVintage::load(repo, id)` → `current()`/`calibration()`/`lineage()` match the sealed artefact; the
   on-disk file is byte-for-byte unchanged after load (read-only), and `history()` is empty.
2. `rollover_swaps_in_place_with_lineage_recorded` (**AC**) — load v1; `rollover(v2)` → `current()` is v2,
   `calibration()` is v2's, and one `RolloverRecord` captures `v1.id→v2.id` + `v1.lineage→v2.lineage`.
3. `rollover_rejects_unverified_vintage_keeping_current` (**atomicity**) — a hand-built `Vintage` with a
   mismatched `content_hash` → `rollover` returns `Err(HashMismatch)`, `current()` is **still v1**, and
   `history()` is empty (no partial swap).
4. `rollover_from_repo_loads_and_swaps` — write v2 to the repo; `rollover_from(repo, v2_id)` swaps to v2 and
   records lineage (the real trainer-emits-then-runtime-rolls path).
5. `rollover_chain_records_full_lineage` — v1→v2→v3 records both transitions in order (`history().len() == 2`,
   endpoints chain v1→v2→v3).
6. `load_missing_vintage_errors` — `ActiveVintage::load(repo, "absent")` → `Err(Io)`, no panic.

## Gates

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -D warnings`,
`cargo test -p qe-runtime`, `cargo test --workspace --locked`,
`cargo test -p qe-architecture --test firewall`, `cargo deny check`.

## Risks

- **Atomicity is verify-before-commit, not cross-thread.** The runtime is single-threaded; the guarantee is
  that a rollover either fully applies a verified vintage or leaves the old one intact — never a repo/calibration
  mix. If a future concurrent reader is added, the holder would need `Arc`-swap; documented, out of scope now.
- **Read-only is enforced by using only `VintageRepository::load`** (open + verify), never `write`, on the
  startup/rollover path. The trainer (QE-129) is the only writer.
- **Lineage history grows unbounded** across many rollovers — a live process rolling over periodically retains
  every transition. Fine at expected cadence (rollovers are rare); a ring buffer is a later refinement if
  needed. Documented.
- **Firewall.** New direct edge is `qe-runtime → qe-determinism` only (cross-cutting, already transitive); no
  train-side edge. Re-proven by the firewall test.
