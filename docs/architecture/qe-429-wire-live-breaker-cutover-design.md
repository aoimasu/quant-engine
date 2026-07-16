# QE-429 — Wire the live BreakerLayer at cutover + promote runtime-risk constants — design note

- **Ticket**: QE-429 (backlog `docs/backlog.md` → Review R1, §R1.b). Follow-up spawned during the R1
  delivery run from the QE-416 / QE-401 / QE-417 review records. No `docs/mds/tickets/qe-429.md` exists;
  the spec of record is the backlog row + those three review records.
- **Depends on**: QE-416, QE-401, QE-417 (all merged), QE-211, QE-212.
- **Author**: implementation session for QE-429.

## 1. The latent gap (evidence)

QE-416/401/417 built a complete runtime-risk loop but left it **latent** — the constructed pieces have no
production caller:

- `crates/hedger/src/live_breakers.rs`: `BreakerLayer::from_calibration(profile, ids, window)` and
  `seed_committed_peaks(&ReconstructedState)` are invoked **only in `#[cfg(test)]` code** (this module's
  unit tests + `crates/runtime/tests/breaker_seed.rs` + `crates/cli/tests/train_job.rs`). No non-test site
  constructs a calibrated, seeded `BreakerLayer`.
- `crates/hedger/src/cutover.rs`: `Cutover` is the in-process bootstrap→live handoff driver. It owns the
  `EvaluatorSession`, drives live bars through `feed_live_bar`, and returns the **raw** per-chromosome
  `EvalOutput` decisions. It never constructs a `BreakerLayer`, never seeds committed peaks, and never
  routes the live decision stream through `BreakerLayer::clamp` — so the sealed thresholds + no-pre-gate
  guarantee (QE-416) and the committed-peak seed (QE-401) are inert on the live path.
- The three review records each flag exactly this and fold the fix into QE-429:
  - QE-416 finding (1): "`from_calibration` has no production caller yet … LATENT until the runtime wires
    `from_calibration` using `content.strategy_ids()`."
  - QE-401 note (1): "no runtime site constructs a live `BreakerLayer` at cutover yet … QE-429 must
    construct the layer at cutover and call `seed_committed_peaks` before the first live tick."
  - QE-417 notes: `stale` has no consumer; `staleness_bound_secs`/`tick_secs`/`half_life` are constructor
    params, not a config knob.

grep confirmation (non-test callers of the wiring): **none** outside `live_breakers.rs` tests and the two
integration tests.

## 2. The convergence point (the exact cutover site)

`Cutover` is where the sealed vintage and the reconstructed state converge on the live path:

- The `EvaluatorSession` inside the `Cutover` owns the sealed `Vintage`, which exposes
  `calibration()` (the per-vintage `CalibrationProfile`) and — via `VintageContent::strategy_ids()` —
  the canonical positional strategy ids (`"0".."k-1"`) the seal keyed the calibration under.
- The `ReconstructedState` (built by `ReconstructedState::from_replay` from the same bootstrap replay,
  QE-210/401) carries each strategy's true all-time `committed_peak_equity`.

Both are available at `Cutover` construction, **before the first live bar is fed** — the correct place to
build + seed the breaker.

`EvaluatorSession` does not yet expose the strategy ids; it exposes `calibration()` but not
`strategy_ids()`. Add a read-only `EvaluatorSession::strategy_ids()` delegating to
`self.vintage.content.strategy_ids()` (keeps the SSOT — the same method the seal wrote under and
`from_calibration` looks up).

## 3. Primary wiring (implementation)

In `crates/hedger/src/cutover.rs`:

1. `Cutover` gains a field `breaker: Option<BreakerLayer>`.
   - Existing constructors `Cutover::new(...)` and `Cutover::from_reconstructed(...)` keep `breaker: None`
     — **behaviour-preserving**: no clamp, existing cutover tests (decision parity vs a continuous
     reference) stay byte-identical.
2. New constructor
   `Cutover::from_reconstructed_calibrated(reconstructed, state: &ReconstructedState, base, fast_window)`:
   - `let mut breaker = BreakerLayer::from_calibration(session.calibration(), &session.strategy_ids(), fast_window);`
   - `breaker.seed_committed_peaks(state);`  ← seeded **before** any live tick.
   - then anchor `last_open_ms` on `decisions.last()` (same rule as `from_reconstructed`; `EmptyReplay`
     if none) and store `Some(breaker)`.
3. `feed_live_bar`: after `self.session.on_bar(bar)`, when a breaker is present rewrite the output's
   decisions through `breaker.clamp(&out.decisions)` — gated strategies flattened to `Exit` **before
   netting** (QE-213). No breaker ⇒ unchanged.
4. Equity-tick routing (the QE-217 live-equity feed's plug point, same honest forward pattern as
   `observe_funding`): `observe_strategy_equity(index, equity) -> Option<BreakerTier>` and
   `observe_ensemble_equity(equity) -> Option<BreakerTier>` forward to the layer (no-op `None` if absent).
5. `breaker(&self) -> Option<&BreakerLayer>` read-only accessor (observability + tests).

Panic-freedom: add the QE-268 module lint
`#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` to `cutover.rs` (matching
`evaluator.rs`/`live_breakers.rs`/`live_netter.rs`). Existing production code has no unwrap/expect/panic
(`debug_assert!` is not `panic!`); the new code delegates to `from_calibration`/`seed_committed_peaks`/
`clamp`/`observe_*`, all already proven panic-free (QE-401/416 reviews).

## 4. Integration test (cutover call site, non-vacuous)

`crates/runtime/tests/cutover_breaker_wiring.rs` (mirrors `breaker_seed.rs`, but at the real `Cutover`
site via `Cutover::from_reconstructed_calibrated`):

- Seal a 2-strategy `Vintage` whose `CalibrationProfile` keys strategy `"0"` (calibrated) and **omits**
  `"1"` (uncalibrated → fail-safe). Build a warmed `EvaluatorSession` → `Reconstructed`, plus a
  `ReconstructedState` with strategy 0 `committed_peak_equity = 200`, strategy 1 `None`.
- Construct the cutover via `from_reconstructed_calibrated(..)`. Assert on `cutover.breaker()`:
  - (a) `strategy_count == 2`, strategy 0 **not** pre-gated (calibration keyed by `strategy_ids()`);
  - (c) strategy 1 **pre-gated** (uncalibrated fail-safe);
  - (b) `strategy_peak(0) == 200`; `observe_strategy_equity(0, 170)` ⇒ `Some(Med)` and `is_gated(0)` — the
    first post-cutover tick reports the true ~15% drawdown, not ~0.
- Route a live bar through `feed_live_bar` and assert both gated strategies' decisions are clamped to
  `Exit` (the wired clamp).
- **Non-vacuous control**: same cutover built + seeded from an **empty** `ReconstructedState` (no peaks) —
  strategy 0's first tick at 170 re-anchors and stays silent (`None`, not gated). Proves the seed is
  load-bearing at the cutover site (the exact QE-401 bug).

## 5. Secondary — constants: promote vs defer (vintage-hash safety analysis)

### PROMOTE — QE-417 mark-EMA runtime constants (golden-safe)

`half_life_secs` (60.0), `tick_secs` (`DEFAULT_TICK_SECS` = 1.0), `staleness_bound_secs`
(`DEFAULT_STALENESS_BOUND_SECS` = 5.0) live in `crates/hedger/src/live_mark.rs` as consts/constructor
params. They are **runtime/live-only**: consumed by `MarkEmaLoop` to smooth the live mark feed. They are
**never serialized into a `Vintage`**, never touch the seal, the lineage, or `Config::content_hash` — so
they cannot move the vintage content hash or any golden.

Promotion: add a per-run `MarkEmaConfig { half_life_secs, tick_secs, staleness_bound_secs }` block with a
`Default` reproducing the spec baseline **byte-for-byte**, and `MarkEmaLoop::from_config(&MarkEmaConfig)`;
refactor `spec_baseline()` to delegate. A test proves `from_config(default)` equals `spec_baseline()`
tick-for-tick (behaviour-preserving).

It is deliberately a **local runtime-risk config block in `qe-hedger`**, **not** a field on
`qe_config::Config`: `qe_config::Config::content_hash` is documented as feeding **vintage lineage**, and
`Lineage` is part of the hashed `VintageContent` — so adding a field to `qe_config::Config` would move the
config hash → the lineage → the vintage `content_hash` and break golden byte-identity. The local block
avoids that entirely.

### DEFER — QE-416 seal-time capacity/calibration constants (hash-sensitive)

`TARGET_AUM_USD` ($1M), `DEFAULT_FAST_QUANTILE` (0.95), `default_calibration_margin()` (1.5),
`CapacityModel::with_defaults()`, and the gross/turnover proxies live in `crates/cli/src/jobs/train.rs`
and are consumed **at seal time** by `capacity_capped_weights` / `worst_case_loss` / `calibrate_profile`.
Their outputs — `weights`, `worst_case_loss`, `calibration` — are fields of `VintageContent` and are
**hashed** into `content_hash`.

Deferred because promoting them risks the vintage hash for two independent reasons:

1. The natural config home is `qe_config::Config`, whose `content_hash` feeds the vintage **lineage**
   (part of the hashed content). Adding fields there moves the hash even if values are unchanged
   (serialization of a new field into the hashed config).
2. Even a value-preserving const→config refactor keeps the *outputs* identical only if the numeric
   pipeline is untouched; any incidental reserialization/rounding change would shift the sealed
   `weights`/`worst_case_loss`/`calibration` and the goldens `golden_result.json` / `sample_vintage.json`
   (baseline `content_hash = f59b27cf70aa402c270d711a0b0f6c348454e425455903a8eb3773b62a800f10`).

Per the ticket's critical constraint ("if promoting a seal-time constant to config would move the vintage
hash for ANY reason … DO NOT promote that constant … wire ONLY the runtime-only mark-EMA constants"),
these stay hardcoded in `train.rs`. Follow-up (if operators need to tune them): a seal-time modelling
config threaded through the lineage on purpose, with a deliberate golden regen — out of scope for QE-429,
which must keep goldens byte-identical.

## 6. Verification plan (green gate)

- `cargo fmt --all --check`; `cargo clippy --all-targets --all-features -- -D warnings` (panic-free lint
  on hedger/edge); `cargo test --all`; `cargo deny check`; firewall test.
- **Goldens byte-identical**: `golden_result.json`, `sample_vintage.json`, train/vintage/determinism
  goldens unchanged; `content_hash` unmoved. The diff touches only `qe-hedger` runtime code + one
  integration test + this note — no seal-path, config-schema, or lineage change — so no golden input
  moves. Verified by the existing `crates/cli/tests/train_job.rs` + determinism tests passing unchanged.

## 7. Risks / blast radius

- `Cutover` gains a field + methods; existing constructors keep `breaker: None` (behaviour-preserving).
  `EvalOutput`/`ChromosomeDecision` structs unchanged. `feed_live_bar` only rewrites decisions when a
  breaker is present.
- `EvaluatorSession::strategy_ids()` is a pure read-only delegate — no state change.
- `MarkEmaConfig` is additive; `spec_baseline()` output is unchanged (proven by test).
- No new dependencies; no seal/lineage/config-schema change; goldens untouched.
