# QE-451 Phase 0 — Expr/Kernel tree interpreter seam proof (default-off) — review record

*QE-451 is an epic delivered in 3 phases (0 → 1a → 1b). This record covers **Phase 0** only.*

- **PR**: https://github.com/aoimasu/quant-engine/pull/152 (squash-merged)
- **Branch**: qe-451-p0/expr-seam-proof
- **Implementation commit**: b53180d
- **Spec of record**: `docs/architecture/qe-450-gp-indicator-evolution-design.md` §4.1/§4.2/§6/§9 (Phase 0 row)
- **Evidence note**: `docs/architecture/qe-451-phase0-expr-seam-design.md`

## Acceptance criteria (Phase 0)
- [x] `Expr` enum + `rust_decimal`-only tree interpreter compiled to a `Kernel` (rides `impl<K:Kernel> Indicator` → batch=streaming by construction); `max_lookback` recursion gives the exact FIR span.
- [x] FIR-only grammar; seeds reproduce a subset of the 22 catalogue indicators **byte-identically**; ac1/ac2 generalised; QE-432-style slow-reference oracle over the tree.
- [x] Default-off / no golden moved; `CATALOGUE_VERSION` stays 1; no f64 in the eval path. Phase 1a/1b deferred.

## Implementation
`crates/signal/src/indicator/expr.rs` (new): `Expr { Input | Const | Unary | Binary | Window }`, Decimal-only interpreter → private `Kernel`, `max_lookback` recursion, seed constructors. Reproduces `sma_ratio_20`, `volume_ratio_20`, `return_1`, `roc_10`, `stoch_k_14`. Default-off (opt-in `expr::seed_*`); catalogue/schema/identity untouched.

## Verification (LOCAL green gate re-run by reviewer on `b53180d`, CI disabled — all PASS)
fmt · clippy locked + all-features `-D warnings` · `cargo test --workspace --locked` (**907 passed, 2 ignored**) · deny · firewall. No golden moved (regenerate → empty).

## Review verdict — [Approved] (0 blocking, 3 non-blocking)
1. **Byte-identity confirmed.** `seed_reproduces_catalogue_byte_for_byte`: each twin's `compute_batch` == the hand indicator element-wise over 120 varied bars (warmup `None` + `QState`). Spot-verified `sma_ratio_20` (same formula, quantiser `lin(−10,10)`, lookback 20) — reproduces the feature, not an approximation.
2. **batch==streaming by construction.** `ExprIndicator` implements only `Kernel`, rides the blanket impl → `update()` and `compute_batch` share the one `observe→eval` path (no separate batch code). ac1 non-vacuous; ac2 generalised (perturb bar older than the FIR window → latest `QState` byte-identical).
3. **FIR/`max_lookback` exact + cross-validated.** Recursion precisely `leaf→1, const→0, unary→child, binary→max, Window(op,child,cap)→(cap−1)+child`; FIR-only grammar. The oracle gates warmth by `max_lookback` while the interpreter gates by `roll.is_full()`, so the passing byte-identity cross-validates `max_lookback` == the true FIR span (the invariant that makes purge/embargo valid for evolved trees).
4. **Decimal-only** (no f64; `Roll` aggregations incl. `std_pop = var.sqrt()` via `MathematicalOps` pure-integer Decimal). **Oracle genuinely independent** (fresh O(span) scans, own `aggregate`, no `Roll`-fold code; exact match over 256 seeded trees; window-off-by-one mutation guard caught).
5. **Default-off / no golden moved** (catalogue/schema/`CatalogueIdentity` untouched, `CATALOGUE_VERSION==1`, `from_config==current`, width 22; regenerate → empty); **scope self-contained** (Phase 1a/1b deferred); **firewall clean** (qe-signal only).

### Non-blocking follow-ups (accepted; strengthen an already-rigorous foundation)
1. Byte-identity proves quantised-`QState` equivalence (the AC wording); a direct raw-`Decimal` `eval_stream(twin) == catalogue raw` assertion would close the last gap (mitigated: the oracle proves interpreter raw-correctness).
2. Non-vacuity guards are `any(is_some)`; a `≥2 distinct states` assertion would make "not trivially constant" explicit.
3. ac2 checks out-of-window independence but not in-window dependence (would prove `max_lookback` isn't over-declared) — mitigated by the oracle's warmup cross-check.

## Phase status
- Phase 0 (seam proof) — **delivered** (this record).
- Phase 1a (offline GP pool + `Elite<ExprTree>` archive + tree operators + FIR grammar) — pending.
- Phase 1b (GP-aware deflation + IC/cost/turnover/capacity gates + cross-asset pooling + freeze K≤16 into `CatalogueIdentity`) — pending.
