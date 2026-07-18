# QE-451 Phase 1a — Offline GP Expr-tree MAP-Elites pool illumination (default-off) — review record

*QE-451 epic, Phase 1a of 3.*

- **PR**: https://github.com/aoimasu/quant-engine/pull/153 (squash-merged)
- **Branch**: qe-451-p1a/gp-pool-illumination
- **Implementation commit**: 0970216 (fix pass; round-1 was 48522a1)
- **Spec of record**: `docs/architecture/qe-450-gp-indicator-evolution-design.md` §4.2/§4.3/§4.4/§4.5/§6/§9 (Phase 1a row)
- **Evidence note**: `docs/architecture/qe-451-phase1a-gp-pool-design.md`

## Acceptance criteria (Phase 1a)
- [x] Full FIR grammar + normalising root (`Rank`/`Zscore`) feeding the existing point-wise `Quantiser` unchanged (`quant.rs` +0); FIR closure preserved (`max_lookback` exact ≤200; excludes IIR/transcendentals/expanding/forward/window<5/`Delta(x,1)`-at-root; flow gated off).
- [x] `ExprTree::repair` deterministic + idempotent; caps enforced by pruning that never removes the root; snaps periods/consts.
- [x] Tree operators on `DetRng`; `Elite<ExprTree>` 45-cell archive (structural niching, uniform-non-empty-cell, behavioural dedup >0.95); trivial-head illumination; distinct-canonical trial count (rejects counted) on a separate `PoolLineage`.
- [x] Determinism (golden mutation-stream + same-seed reproduces archive); default-off / no golden moved; deflation/IC/MDL/cost/capacity/freeze/flow deferred to Phase 1b.

## Implementation
Grammar (`qe-signal/indicator/expr.rs`): `Rank`/`Zscore` FIR `WinOp`s → existing quantiser unchanged; lattice/grid/caps; `snap_period`/`snap_const`. `ExprTree::repair` (force normalising root, snap, deterministic cap-pruning, recompute lookback) + `canonicalize` + Decimal SHA-256 `canonical_hash`. Search (`qe-wfo/src/gp/`): structural descriptors (family/timescale/complexity, 5×3×3), separate `Elite<ExprTree>` archive (Deep-Grid 8, uniform-non-empty-cell, dedup >0.95), operators on `DetRng` (uniform pre-order node selection, reuse `OperatorSelector`), `illuminate()` under a trivial threshold-cross head. Distinct-canonical count in a `PoolLineage` wrapper — production `Lineage` untouched.

## Review — two rounds
**Round 1 (48522a1): [Approved], 0 blocking.** All four ACs verified — FIR-closure of the Rank/Zscore roots (strictly causal, exact `max_lookback`), quantiser unchanged (+0), repair idempotent + caps via non-root pruning, `DetRng`-by-index determinism (thread-independent, golden stream pinned, no f64 in the hash path), distinct-canonical count sound (canonicalisation collapses equivalents, counts every evaluated tree incl. rejects), default-off/no-golden, firewall clean. **Primary nit:** the slow-reference oracle's generator (`below(7)`) never emitted Rank/Zscore → the 256-tree independent cross-check didn't exercise the two new critical roots. Orchestrator sent it back to close the gap before Phase 1b builds on them.

**Round 2 (0970216): [Approved], 0 blocking.** `rand_winop` widened `below(7)→below(9)` with `Rank`/`Zscore` arms → both roots generated at every window position. **Load-bearing coverage assertion:** counters increment only when `warm && contains_winop(op)`; because `eval` has no short-circuit (a warm root ⟹ every node returned `Some`), `warm && contains(Rank)` genuinely implies the Rank aggregate ran and was cross-checked (reviewer traced the shadowing non-cases). `assert!(rank_warm>0) && assert!(zscore_warm>0)` fail if the corpus ever loses a warm Rank AND warm Zscore (reverting to `below(7)` fails) → the roots can never silently regress to dead code. The independent reference computes Rank (`count(v<current)/n`) + Zscore (`(current−mean)/std_pop` clipped, std=0⇒0) by its own naive scan (no `Roll`-fold code); `assert_eq!(streaming, reference)` exact (not loosened) → **no interpreter discrepancy surfaced**; the roots' raw-correctness is now oracle-verified, not just unit-tested. Test-only fix (default-off/`CATALOGUE_VERSION` unchanged, regenerate → empty).

## Verification (LOCAL green gate re-run by reviewer on `0970216`, CI disabled — all PASS)
fmt · clippy locked + all-features `-D warnings` · `cargo test --workspace --locked` (**933 passed, 2 ignored**; gp 16 + expr 18 + oracle) · deny · firewall.

### Non-blocking follow-ups (ride Phase 1b)
1. Add a test isolating a single dedup-reject incrementing the count (currently covered by the aggregate `total == gens×offspring`).
2. Extend `canonicalize`'s monotone-wrapper strip to Zscore-affine equivalents (currently a conservative safe over-count under Zscore).

## Phase status
- Phase 0 seam proof — delivered ([`qe-451-phase0.md`](./qe-451-phase0.md)).
- Phase 1a offline pool — **delivered** (this record).
- Phase 1b (GP-aware deflation + IC/MDL/cost/turnover/capacity gates + cross-asset pooled fitness + freeze K≤16 into `CatalogueIdentity`) — pending.
