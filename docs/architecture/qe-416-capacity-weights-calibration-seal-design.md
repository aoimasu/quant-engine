# QE-416 — Seal capacity-weighted allocation + worst-case-loss + real breaker calibration

**Ticket source of truth:** section `### QE-416` in
`docs/reviews/2026-07-15-team-improvement-review.md`.
**Depends on:** QE-128 (capacity model), QE-130 (stress set), QE-116 (calibration model).
**Area:** P2 portfolio construction + runtime calibration, at the train-job seal boundary.

## 1. Current state (the placeholders being replaced)

The train job (`crates/cli/src/jobs/train.rs`) selects ensemble members via the discrete-DE portfolio
search, then throws away the allocation and writes placeholders into the sealed vintage:

- **Equal weights overwrite** — `crates/cli/src/jobs/train.rs:358`
  `let weights = vec![1.0 / k as f64; k];`
  The capacity-aware allocation is discarded; every member gets `1/k` regardless of its capacity.
- **`worst_case_loss: None`** — `crates/cli/src/jobs/train.rs:433`
  The QE-130 stress set is never run at seal.
- **Constant calibration** — `crates/cli/src/jobs/train.rs:430-432`
  `calibration: CalibrationProfile::new(Fraction::new(Decimal::new(1, 1))…)` — a `0.1` ensemble
  fast-drop and an **empty** `per_strategy` map.
- **Runtime consequence** — `crates/runtime/src/live_breakers.rs:99-123`
  `BreakerLayer::from_calibration` looks each strategy up in `profile.per_strategy`; a **missing**
  entry is **explicitly pre-gated** (fail-safe: an uncalibrated strategy is flattened before any
  observe, `live_breakers.rs:106-121`). With an empty `per_strategy` map, **every** strategy of a
  vintage sealed this way is pre-gated — the vintage trades nothing live.

The models already exist and are unit-tested, but are **never invoked at seal**:

- **Capacity (QE-128)** — `crates/ensemble/src/capacity.rs`
  - `capacity(&StrategyProfile{gross_edge, turnover}, &CapacityModel) -> f64` — the AUM `W*` at which
    size-impact erodes edge to the retained floor (`capacity.rs:75-87`).
  - `cap_weights(weights, capacities, target_aum) -> Vec<f64>` — water-fills the unit budget so no
    member exceeds `capacity_i / target_aum`; freed budget redistributes to uncapped members; a
    binding cap set may leave the sum `< 1` (uninvested cash) (`capacity.rs:96-149`).
- **Stress / worst-case-loss (QE-130)** — `crates/ensemble/src/stress.rs`
  - `worst_case_loss(series, weights, scenarios) -> StressReport` — the largest peak-to-trough loss any
    single scenario produces, over the capacity-weighted combined path (`stress.rs:182-210`).
  - `default_synthetic_shocks()` — gap + funding-spike + ADL at documented magnitudes (`stress.rs:97`).
    Historical windows are caller-supplied (the engine has no calendar knowledge) → we pass only the
    synthetic set.
- **Calibration (QE-116)** — `crates/risk/src/calibration.rs`
  - `calibrate_threshold(observed_drawdowns, quantile, margin) -> Fraction` — the `quantile` of observed
    |drawdown| magnitudes × `margin`, clamped `[0,1]` (`calibration.rs:66-80`). Distribution-agnostic;
    an empty distribution → `0` (fail-safe).
  - `CalibrationProfile { per_strategy: BTreeMap<String, BreakerThresholds>, per_cohort, ensemble_fast_drop }`
    (`calibration.rs:32-39`). `BreakerThresholds { slow_dd, med_dd, fast_drop }` (`breaker.rs:86-93`);
    the breaker fires `Med`/`Slow` when total drawdown `>=` the threshold and `Fast` when the
    fast-window drop `>=` `fast_drop` **and** `fast_drop > 0` (`breaker.rs:155-164`).

### Inputs available at seal time

Everything the three models need is already computed on the train side:

- `chromosomes: Vec<Genome>` — the selected members; `genome.risk.size_bps` gives per-position sizing.
- `backtest(g, train_bars, &train_cfg) -> BacktestResult` — deterministic; yields `returns`
  (per-bar net-of-cost) and `trades` (entry-fill count) (`crates/wfo/src/backtest.rs:64-78`).
  Already invoked at seal for the ensemble pool; we additionally backtest the **selected** members.

## 2. New seal path

`capacity_capped_weights(chromosomes, selected_bt)` (new, in `train.rs`):

1. Start from equal nominal weights `1/k`.
2. Per member, estimate `StrategyProfile`:
   - `gross_edge` = mean per-period **net** return over the train window (a conservative proxy for
     gross edge — using net understates capacity slightly, never overstates it).
   - `turnover` = `trades · 2 · size_frac / n_periods` (round-trip notional per period;
     `size_frac = size_bps / 10_000`).
3. `capacities = capacity(profile, CapacityModel::with_defaults())`.
4. `capped = cap_weights(equal, capacities, TARGET_AUM_USD)` where `TARGET_AUM_USD = 1_000_000.0`
   (a documented default book size — a reviewer sanity-check / future-config item, not a per-run input).
5. **Fallback:** if the capped budget sums to `~0` (every member modelled uneconomic at the target
   AUM), fall back to equal weights so the seal still yields a *tradeable* vintage — the capacity signal
   then rides in `worst_case_loss` / calibration rather than a zero book.

The capacity-capped `weights` are used **consistently**: persisted in the vintage **and** used for the
G1 in-sample/holdout combine and the funding-net figures (they are what actually trades). Uniform
scaling leaves the Sharpe gate unchanged; only genuine per-member capping changes the combined shape.

`worst_case_loss` (QE-130): `worst_case_loss(&selected_returns, &weights, &default_synthetic_shocks())`
→ persist `Some(report.worst_case_loss)`.

`calibration` (QE-116): new `calibrate_profile(&selected_returns, &weights)` builds a
`CalibrationProfile`:
- `ensemble_fast_drop` = `calibrate_threshold` over the **capacity-weighted ensemble** equity curve's
  fast-window drop distribution.
- `per_strategy["{i}"]` = `calibrate_thresholds(equity_i, DEFAULT_FAST_WINDOW, margin)` for each member
  `i`, keyed by the member's **positional id** (see §3).

New helper `qe_risk::calibration::calibrate_thresholds(equity, fast_window, margin) -> BreakerThresholds`
(QE-116 model, added there — its natural home): slow/med from the running-peak drawdown distribution at
increasing quantiles (`0.75` / `0.95`), fast from the fast-window drop distribution (`0.95`), each scaled
by `margin` (`1.5`) via `calibrate_threshold`; `med_dd` forced `>= slow_dd` (tier invariant). Supporting
`drawdown_distribution` / `fast_drop_distribution` mirror the `CircuitBreaker` measures so the calibrated
thresholds sit just beyond replayed behaviour.

## 3. Strategy-id mapping (why `from_calibration` finds every strategy)

`BreakerLayer::from_calibration(profile, strategy_ids, fast_window)` maps member `i` to
`profile.per_strategy[strategy_ids[i]]`. There is no live caller yet, so the id convention is defined
here as the **single source of truth**: new `VintageContent::strategy_ids()` returns the positional
index of each chromosome as a string (`["0","1",…]`). The seal writes `per_strategy` under exactly these
keys; any runtime caller derives ids the same way → every sealed strategy is found, so nothing is
pre-gated. (A method, not a field — it does not enter the content hash.)

## 4. Golden regeneration

The sealed `VintageContent` **struct** is unchanged (`weights`, `worst_case_loss`, `calibration` all
already exist — vintage format stays v3). Only the train-job **seal path** changes what it writes.

- `crates/cli/tests/train_job.rs` — the train golden is **self-relative**: it asserts determinism
  (same seed → same `content_hash`) and structural invariants, **not** a pinned hash. No committed hash
  to regenerate; the determinism test recomputes its own expectation each run.
- `crates/cli/tests/fixtures/sample_vintage.json`, `crates/server/tests/fixtures/sample_vintage.json`,
  `crates/cli/tests/fixtures/golden_result.json` — the sample vintage is **hand-built** by
  `write_sample_vintage` (`backtest_job.rs:183`) and sealed through real `Vintage::seal`, **independent
  of the train seal path**. The backtest golden reads that hand-built vintage. None are affected by this
  change, so none require regeneration. (Verified empirically: the full suite stays green — see §6.)

If a future change alters the `VintageContent` struct, regenerate the sample vintage + backtest golden
via the sanctioned `#[ignore]`d regenerator:
`cargo test -p qe-cli --test backtest_job regenerate_fixtures -- --ignored --exact`.

## 5. Determinism

All three models are pure functions of the deterministic `backtest` outputs (`returns`, `trades`) over
the seeded search's selected members. No RNG, no wall-clock. `calibrate_threshold` sorts a `Vec`
deterministically; `f64 → Decimal` via `from_f64_retain` is deterministic; the `per_strategy` map is a
`BTreeMap` (stable iteration → stable hash). `train_is_deterministic_for_a_fixed_seed` proves same seed
→ same new sealed hash.

### Hash-round-trip stability (two idempotency fixes discovered during implementation)

The vintage content hash is `sha256(serde_json::to_vec(content))`, verified on reload
(`Vintage::load`). Feeding `f64`-derived values into the hashed content exposed two serialize→parse→
serialize **non-idempotencies** that would fail the QE-402 verify on load:

1. **Calibration `Decimal` excess precision.** A `Decimal` from division of `from_f64_retain` equity
   carries ~28 significant digits and can serialise (via `rust_decimal`'s string form) to a byte string
   that a parse reconstructs to a different scale. Fix: `qe_risk::quantize_calibration` rounds every
   calibrated threshold to `CALIBRATION_SCALE = 12` dp and `.normalize()`s it to canonical minimal
   scale (sub-basis-point resolution — far finer than any breaker threshold needs).
2. **`f64` weights / worst-case-loss.** `serde_json`'s **default** float parser is not correctly-rounded:
   a 17-significant-digit `f64` (a raw capacity weight or stress loss, e.g. `0.11418292380128303`) can
   re-parse to a neighbouring `f64` that serialises one ULP differently (`…305`). Fix: `hash_stable`
   rounds every sealed `f64` (weights + worst-case-loss) to `10^12` before hashing, keeping it inside the
   parser's exact range. The rounded weights are used consistently (persist + G1 combine), so nothing
   diverges.

Both fixes are deterministic and covered by `train_is_deterministic_for_a_fixed_seed` (same seed → same
hash) and `train_over_fixture_store_seals_verifiable_vintage` (`Vintage::load` verify passes).

## 6. Acceptance-criteria tests

- **(a) weights differ from equal when capacity binds** — unit test in `train.rs` on the seal weight
  helper: a binding capacity vector yields non-equal weights; a non-binding one leaves them equal; an
  all-uneconomic vector falls back to equal.
- **(b) `worst_case_loss` is `Some`** — `train_job.rs`: after `run_train_job`, the loaded vintage's
  `worst_case_loss` is `Some(_)` (finite, `>= 0`).
- **(c) `from_calibration` finds every sealed strategy (no pre-gating)** — `train_job.rs`: derive
  `loaded.content.strategy_ids()`, build `BreakerLayer::from_calibration`, assert no member `is_gated`
  before any observe and the profile has a `per_strategy` entry for every member.

## 7. Risks / rollback

- **Modelling choices to sanity-check:** `gross_edge` = mean net return (conservative proxy);
  `turnover` = round-trip notional per period; `TARGET_AUM_USD = 1_000_000`; calibration quantiles
  `0.75/0.95/0.95` and margin `1.5`. All documented constants; none are per-run config yet.
- **Degenerate capacity** (all members uneconomic at the target AUM) → equal-weight fallback keeps the
  vintage tradeable; the honest capacity signal still rides in `worst_case_loss` / calibration.
- **Flat-equity calibration** → a member with no observed drawdown calibrates to `0` thresholds (QE-116
  fail-safe). It is still present in `per_strategy` (so not pre-gated by `from_calibration`); the
  fixture members do draw down, so this does not occur in tests.
- **Rollback:** revert this commit; the vintage struct is untouched, so older/newer vintages remain
  loadable either way.
