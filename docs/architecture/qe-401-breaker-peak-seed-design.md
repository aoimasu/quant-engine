# QE-401 — Seed the live drawdown breaker with the reconstructed committed-peak equity

P1 capital-safety (runtime-risk). Authoritative spec: `docs/reviews/2026-07-15-team-improvement-review.md` § `### QE-401`.

## 1. Current state (file:line evidence)

### The reconstructed peak is computed but unused
- `crates/runtime/src/boot_state.rs:9-12` — module doc: the committed peak is *load-bearing for drawdown breakers*; a windowed peak "would under-anchor the drawdown and mis-fire the breaker".
- `crates/runtime/src/boot_state.rs:127-128` — `StrategyState.committed_peak_equity: Option<Decimal>`.
- `crates/runtime/src/boot_state.rs:195` — `from_replay` folds each strategy's equity path with `CommittedPeak::from_series(...).peak()`.
- **Consumer grep:** `committed_peak_equity` is read only inside `boot_state.rs` and tests (`restart_parity.rs`). Nothing on the live path consumes it.

### The breaker has no seed API
- `crates/risk/src/breaker.rs:100` — `CircuitBreaker.peak: Option<Decimal>`.
- `crates/risk/src/breaker.rs:107-114` — `new(...)` always sets `peak: None`.
- `crates/risk/src/breaker.rs:123-126` — `reset()` sets `peak = None` (re-anchors from scratch).
- `crates/risk/src/breaker.rs:144-153` — `observe()` sets `peak = peak.max(equity)` (first live tick becomes the anchor when peak was `None`) and drives the **slow/med** drawdown tiers from it. The **fast** tier uses the `recent` window (`:131-141`), not the peak.

### BreakerLayer builds `peak:None` and has no live caller
- `crates/runtime/src/live_breakers.rs:64-89` — `new(...)` builds each `CircuitBreaker::new(...)` (peak `None`), plus a **fast-drop-only** ensemble breaker (slow/med disabled via `never_fires()` = 1.0).
- `crates/runtime/src/live_breakers.rs:99-123` — `from_calibration(...)` (per-strategy thresholds + uncalibrated fail-safe pre-gate). **grep:** `from_calibration`/`BreakerLayer::new` have no runtime caller — only tests (`live_netter.rs`, `cli/tests/train_job.rs`) construct a layer. QE-429 will wire `from_calibration` into the live evaluator.
- `crates/runtime/src/live_breakers.rs:183-192` — `reset()` calls `b.reset()` on every breaker (clearing peak to `None`) then re-applies the uncalibrated pre-gate.

### Cutover wires only the session
- `crates/runtime/src/cutover.rs:48-58,99-128` — `Cutover` owns an `EvaluatorSession`, enforces bar continuity, flips `go_live` in place. It never constructs or holds a `BreakerLayer`, and never touches `ReconstructedState`. The equity feed the breaker consumes is QE-217 (out of scope here).

**Consequence.** After every bootstrap/restart the DD breaker re-anchors its peak on the *first live equity tick*, so a book already 20% below its historical peak reports ≈0 drawdown and the slow/med DD breaker stays silent — defeating the exact mis-anchoring the reconstructed peak exists to prevent, on the capital-loss path.

## 2. Seed API design

### `qe-risk` — `CircuitBreaker`
Add a private `seed_peak: Option<Decimal>` field (the persistent anchor floor) and:
- `pub fn with_seed_peak(mut self, peak: Decimal) -> Self` — builder; sets `seed_peak = Some(peak)` **and** `peak = Some(peak)`.
- `pub fn seed_peak(&mut self, peak: Decimal)` — in-place equivalent (used to seed breakers already inside a `Vec`).

Seeding only pre-loads `peak` (the slow/med drawdown anchor). The fast window (`recent`) is untouched — fast-drop seeding is explicitly out of scope (speed tier is inherently windowed).

`reset()` becomes seed-aware:
- If `seed_peak.is_none()` → legacy behaviour: `peak = None` (un-seeded breakers, e.g. `replay`, unchanged).
- If `seed_peak.is_some()` → carry the anchor across the rollover: `seed_peak = self.peak` (which is `max(seed, highest observed)` by construction) and leave `peak` at that value. This preserves the seed **unless a genuinely higher peak was observed**, in which case the higher observed peak becomes the new (monotone non-decreasing) anchor.

`new()` sets `seed_peak: None`, so `replay` and every existing un-seeded construction is bit-for-bit unchanged.

### `qe-runtime` — `ReconstructedState` (boot_state.rs)
- `pub fn aggregate_committed_peak(&self) -> Option<Decimal>` — sum of the `Some` per-strategy `committed_peak_equity` values; `None` if none are present. A pure, deterministic function of the reconstructed peaks (the aggregate-equity notion; the reconstructed state carries no aggregate equity path).

### `qe-runtime` — `BreakerLayer` (live_breakers.rs)
- `pub fn seed_committed_peaks(&mut self, state: &ReconstructedState)` — for each `StrategyState`, seed `self.strategy[index]` from its `committed_peak_equity` (skip `None`; out-of-range indices ignored); seed the ensemble breaker from `state.aggregate_committed_peak()`.

## 3. Where it is wired at cutover

There is **no** runtime construction site that today builds a `BreakerLayer` alongside the cutover / reconstructed state — the live equity feed and the breaker-layer wiring are QE-217/QE-429. Per the ticket's guidance ("if `from_calibration` still has no live caller, seed at whatever construction site the cutover DOES use / add the seeding the runtime will use"), QE-401 delivers the **seed API + the seeding step**, not a new live loop:

`from_calibration(profile, ids, fast_window)` → `layer.seed_committed_peaks(&reconstructed)`.

QE-429 (wire `from_calibration` into the live evaluator) calls `seed_committed_peaks` immediately after constructing the layer, at the same point the reconstructed state is available (post-bootstrap, pre-first-live-tick). The seed step is independent of and composable with that wiring — it does not block QE-429, and it does not require touching `Cutover`'s session-continuity responsibility. An integration test exercises the full path end-to-end.

## 4. reset()/rollover preservation rule

The committed-peak anchor is preserved across `CircuitBreaker::reset()` (hence `BreakerLayer::reset()` on a session rollover) **unless a genuinely higher peak was observed live**, in which case that higher peak becomes the anchor. Implemented in `CircuitBreaker::reset` via `seed_peak = self.peak` for a seeded breaker (monotone non-decreasing anchor); un-seeded breakers keep the legacy `peak = None`. `BreakerLayer::reset` continues to re-apply the uncalibrated fail-safe pre-gate — unchanged.

## 5. Test plan

- `qe-risk` unit: `with_seed_peak` pre-anchors drawdown; first tick 15% below the seed trips med at threshold; `reset()` preserves the seed; `reset()` after a higher observed peak preserves the *higher* peak; un-seeded `new()`/`replay` unchanged.
- `qe-runtime` unit (`live_breakers`): `seed_committed_peaks` seeds per-strategy + ensemble; a seeded strategy's first live tick reports the true drawdown and gates; `reset` preserves seeds.
- `qe-runtime` integration (`tests/breaker_seed.rs`, **the AC**): replay an equity path that peaks then declines 15%; cold-start via `ReconstructedState::from_replay`; build the layer `from_calibration`, `seed_committed_peaks`; the **first** live tick at the declined level reports drawdown ≈ 15% (not ≈ 0) and trips the med tier at threshold; assert the seeded peak equals `CommittedPeak::from_series` of the replayed path **bit-for-bit**.
- Regression: `cargo test --workspace` (risk breaker, runtime cutover/restart-parity/live_breakers) + firewall.

## 6. Risks / rollback

- **No new firewall edges** — all changes are within `qe-risk` and within `qe-runtime` (which already depends on `qe-risk`). No new external crate (cargo-deny-safe).
- **Order-emission path** — `live_breakers.rs` carries `#![deny(clippy::unwrap_used, expect_used, panic)]`; the seeding code is panic-free (no unwrap/expect, bounds-checked `get_mut`, `Decimal` arithmetic only).
- **Determinism** — seeding is a pure function of the reconstructed committed peak (`CommittedPeak::from_series`), which is itself deterministic; no clock/RNG.
- **Behaviour-preserving default** — `CircuitBreaker::new` keeps `seed_peak: None`, so every existing construction (replay, un-seeded layers, tests) is bit-for-bit unchanged; seeding is strictly additive and opt-in.
- **Rollback** — revert the branch; no persisted format or wire change.
- **Known caveat** — the ensemble breaker is fast-drop-only (slow/med disabled), and the fast tier is windowed, so seeding the ensemble peak anchors `peak()` for observability/defence-in-depth but does not change ensemble firing today. The aggregate is the sum of per-strategy committed peaks (the reconstructed state carries no aggregate equity path); documented as such.
