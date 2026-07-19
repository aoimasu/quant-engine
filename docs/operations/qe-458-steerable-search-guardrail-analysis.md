# QE-458 — Steerable-search params + gate-monotone whitelist guardrail (evidence note)

Authoritative ticket: `docs/mds/tickets/QE-458.md`. Design ref: `docs/architecture/qe-455-research-flow-design.md`
§6 (whitelist), §6.1a (deflation-scaling / cardinality→N / archive-coverage / regime invariant), §6.2
(blocklist), §6.3 (proof obligation).

## 1. Current state (real anchors)

| Concern | Location | Notes |
|---|---|---|
| `TrainParams` wire shape | `crates/run-protocol/src/lib.rs:357` | serde-`default`-lenient; already exposes `seed/generations/population/holdout/embargo/config/profile`. Deps: **serde only** (no vintage/validation). |
| `validate_train` | `crates/server/src/runs/manager.rs:415` | today only requires `start/end/resolution`. Uniform `400` via `CreateError::Validation`. |
| Spawner flag mapping | `crates/server/src/runs/spawn.rs:113` (`train_args`) | maps `TrainParams` → `qe train` CLI flags. |
| QE-439 effective-trials basis | `crates/validation/src/dsr.rs:25` `effective_trials(cells,generations,windows)` | analytic floor `cells·gens·windows`; over-count = safe. `expected_max_sharpe` at `dsr.rs:74`. |
| GP trial basis wrapper | `crates/wfo/src/gp/deflation.rs:30` `gp_trial_basis` / `assess_gp_champion` | `N = max(distinct, analytic floor)`; `GpDeflationGate` (PBO-primary, DSR floor). |
| MAP-Elites GP archive | `crates/wfo/src/gp/archive.rs` (`ExprArchive::len`=occupied niches, `occupied_cells`) | descriptor space = `EXPR_CELLS = 45` (`crates/wfo/src/gp/descriptor.rs:142`, `5×3×3`). |
| Sealed evolved pool | `crates/formula-pool/src/lib.rs` (`FormulaPoolContent.formulas`, `PoolFormula{sexpr, formula_hash}`, `MAX_POOL_SIZE=16`, `CAPACITY_FLOOR_USD=250_000`, `MAX_TURNOVER_FRAC="0.25"`) | firewall-covered edge `qe-server → qe-formula-pool` already exists (`crates/server/Cargo.toml:45`). |
| Catalogue count | `crates/signal/src/indicator/mod.rs:166` `catalogue(cfg).len()` (≥20) | catalogue-indicator count for feature-space size. |
| Vintage lineage/provenance schema (QE-467) | `crates/vintage/src/lib.rs:171` `SteerDelta`, `:194` `ResearchProvenance.steer_delta: Option<SteerDelta>` | schema owned by QE-467; `VINTAGE_FORMAT_VERSION=8` (line 50). This ticket **populates**, does **not** bump. |
| Train seal populates provenance | `crates/cli/src/jobs/train.rs:610` | writes `ResearchProvenance{data_provenance:Real, ..default}` → `steer_delta:None` (un-steered; no golden move). |
| Blocklist source consts | cost-stress `{1×,2×}` `crates/wfo/src/gp/gates.rs:80`; turnover `0.25` / capacity `250_000` `crates/formula-pool/src/lib.rs`; DSR/PBO `GpDeflationGate::default` (`min_dsr 0.95`, `max_pbo 0.5`). | |
| Firewall test | `crates/architecture/tests/firewall.rs` + `crates/architecture/src/lib.rs:210` `firewall_rules()` | already asserts `qe-server → qe-formula-pool` parsed + no forbidden edge. |
| `evaluate_g1` / `G1Criteria` | `crates/gate` | **NOT touched** (out of scope). |
| `DEFLATION_BASIS_VERSION` | `crates/validation/src/basis.rs` | server-side, non-editable — no request field flips it. |

## 2. Implementation decisions

Placement respects the firewall (search ⟂ portfolio ⟂ live) and adds **no new cross-crate edge**:

1. **`crates/run-protocol/src/lib.rs`** — extend `TrainParams` with whitelisted steer knobs
   (`indicator_subset: Option<Vec<String>>`, `evolved_pool: Option<String>`,
   `evolved_formulas: Option<Vec<String>>`, `windows: Option<usize>`, `folds: Option<usize>`) and the
   blocklist probe fields (`cost_stress_multiplier`, `max_turnover_frac`, `capacity_floor_usd`,
   `dsr_cutoff`, `pbo_cutoff`, `ic_fdr_threshold`, `purge` as `Option<..>`; `holdout`/`embargo` already
   exist). All `#[serde(default, skip_serializing_if)]` — wire stays lenient, `#[serde(default)]`. No new dep.
2. **`crates/validation/src/steer.rs`** (new, pure; deps unchanged = qe-determinism only) — the compiled
   **floors** + guardrail math:
   - `available_feature_space(catalogue_count, evolved_count)`;
   - `effective_trials_with_features(cells, generations, windows, feature_space)` = `effective_trials(..)
     .saturating_mul(feature_space.max(1))` — **monotone non-decreasing** in feature-space size and budget,
     over-counts (safe direction). This is the QE-439 basis extended to ingest cardinality (AC a).
   - `archive_coverage(occupied, descriptor_space)`, `coverage_floor_ok(occupied)`,
     `DESCRIPTOR_SPACE_CELLS=45` (mirrors `EXPR_CELLS`, documented mirror to avoid a wfo→ dep inversion),
     `MIN_OCCUPIED_NICHES` (AC c);
   - regime-invariant floors `MIN_WFO_WINDOWS`, `MIN_WFO_FOLDS` (AC d proxy — see risks);
   - blocklist floor consts `COST_STRESS_MULTIPLIER_FLOOR=1.0`, `MAX_TURNOVER_CAP_FLOOR=0.25`,
     `CAPACITY_FLOOR_USD=250_000`, `DSR_CUTOFF_FLOOR=0.95`, `PBO_CUTOFF_FLOOR=0.5`,
     `IC_FDR_THRESHOLD_FLOOR=0.10`, `HOLDOUT_FLOOR`, `EMBARGO_FLOOR`, `PURGE_FLOOR` (AC blocklist).
3. **`crates/server/src/runs/steer.rs`** (new; server has sha2 + qe-vintage + qe-formula-pool + qe-signal +
   qe-validation) — `steer_delta_for(&TrainParams, catalogue_count) -> Option<SteerDelta>`
   (SHA-256 subset hash + budget + window/fold counts), populating QE-467's field (AC e). Only `Some` when a
   knob is set → un-steered vintages keep `steer_delta:None` (no golden move).
4. **`validate_train`** (`crates/server/src/runs/manager.rs`) — enforce: whitelist field validity,
   blocklist `400`s (reject any blocklist knob set **below** its compiled floor), regime-coverage invariant
   (`windows`/`folds` below floor → `400`).

## 3. Test plan (each merge-gate = an AC)

- **deflation-scaling monotonicity** (`crates/validation/src/steer.rs` tests) — `effective_trials_with_features`
  non-decreasing as feature-space / generations / windows rise (AC b, base case; proof §6.3).
- **noise-series false-discovery** (`crates/validation/src/steer.rs` tests) — on pure-noise returns, enlarging
  the feature subset raises `N` and `E[maxSharpe]`, so the champion DSR is **non-increasing** ⇒ seal rate does
  not rise (AC b).
- **gate-monotone sweep** (`crates/validation/src/steer.rs` tests) — fixed noise population where the deflation
  gate **rejects** un-steered; sweep every whitelisted knob up; assert `N`↑ non-decreasing, `E[maxSharpe]`↑
  non-decreasing, champion DSR non-increasing, and the gate verdict never flips reject→accept (§6.3).
- **archive-coverage floor** (`crates/validation/src/steer.rs` tests) — coverage recorded pre/post; a
  collapsed occupied-count trips `coverage_floor_ok=false` (AC c).
- **regime-coverage invariant** (`crates/server` `validate_train` tests) — `windows`/`folds` below floor →
  `400`; at/above floor → ok (AC d).
- **blocklist 400s** (`crates/server` `validate_train` tests) — each blocklist knob below floor → `400`; at/above
  floor → ok; `DEFLATION_BASIS_VERSION`/`G1Criteria` untouched (AC blocklist).
- **steer-delta population** (`crates/server/src/runs/steer.rs` tests) — `steer_delta_for` yields `Some` with a
  64-hex subset hash under steering, `None` un-steered; sealing a `VintageContent` with it changes the vintage
  id and round-trips (AC e).
- **evolved-pool firewall-green load path** (`crates/server` test + existing `firewall` test) — counting formulas
  from a sealed `FormulaPoolContent` for feature-space/hash uses only `qe-formula-pool` (already firewall-covered);
  `cargo test -p qe-architecture --test firewall` stays green (AC f). No re-deflation / un-seal path is added.

## 4. Risks / flagged product questions

- **`MIN_OCCUPIED_NICHES` (archive floor)** — **not defined anywhere** in the repo. Descriptor space = 45 cells.
  Chosen conservative default **5** (coverage ≥ ~0.11) as a genuine-collapse tripwire that surfaces collapse
  without falsely rejecting a healthy run. **Product sign-off needed** on the exact QD floor.
- **OOS-span floor / "mandated stress regime"** — no compiled OOS-span-in-bars floor and **no named
  stress-regime catalogue** exist server-side at validate time (regime composition is a downstream QE-460 field).
  `validate_train` sees only `start/end` + window/fold **counts**, not bars. The regime-coverage invariant is
  therefore enforced as a conservative **window/fold count floor** proxy (`MIN_WFO_WINDOWS=4`, `MIN_WFO_FOLDS=2`)
  — fewer windows ⇒ less OOS span ⇒ reject. **Product decision needed** to (a) name the mandated stress regime and
  (b) set the OOS-span-in-bars floor once QE-460's regime classifier lands; then the invariant can key on bars.
- **Blocklist semantics (hardened past the AC's literal wording)** — the AC says "reject … below its compiled
  floor" uniformly, but a *literal* below-floor check leaves a **real hole**: cap/ceiling-style knobs
  (`max_turnover_frac`, `pbo_cutoff`, `ic_fdr_threshold`) RELAX the gate when *raised*, so "reject below floor"
  would happily accept a gate-loosening high value — reintroducing exactly the overfitting this ticket kills.
  Decision: the six gate-decision knobs (`cost_stress_multiplier`, `max_turnover_frac`, `capacity_floor_usd`,
  `dsr_cutoff`, `pbo_cutoff`, `ic_fdr_threshold`) are **not editable in ANY direction** — a request that so
  much as names one is a `400` (`reject_if_present`). This is strictly stronger than "reject below floor" and
  fail-safe, matching design §6.2 ("These are **not** steerable"). Only `holdout`/`embargo`/`purge` — which
  PRE-EXIST as legitimate QE-261 knobs and are *floored, not tuned* — keep floor semantics (may be raised, never
  dropped below floor). The compiled `*_FLOOR` consts name the pinned server-side values.
## 5. Live seal-path wiring (post-review, PR #171 blocking items 1–4)

The reviewer ruled that AC (a)/(c)/(e) are behavioural and NOT met by "extension built but not called". The
live train job (`crates/cli/src/jobs/train.rs` `run_train_job`) now applies the steer knobs — **steered runs
only**, so un-steered runs stay byte-identical (no golden / `DEFLATION_BASIS_VERSION` / `VINTAGE_FORMAT_VERSION`
move):

- **Item 4 (apply the subset — the honesty prerequisite).** A leak-free **feature allowlist** on the MAP-Elites
  variation (`qe_wfo::variation`: `pick_feature` / `fresh_random_masked` / `explore_masked` /
  `VariationDriver::with_allowed_features`) confines a steered search to the requested `indicator_subset`.
  Golden-safe because it is **RNG-draw-neutral** (an empty allowlist draws exactly `below(rng,len)` as before,
  proven by `empty_allowlist_is_byte_identical_to_the_unmasked_search`) and **leak-free** because `Genome::repair`
  only clamps out-of-range features and never re-points an in-range one (proven by
  `allowlist_confines_every_referenced_feature_to_the_subset` and `driver_with_allowed_features_restricts_the_run`).
  Keeping the full-width schema means the sealed steered vintage's `CatalogueIdentity` is unchanged, so it stays
  backtestable through the QE-402 load boundary. `windows`/`folds` override the WFO window / CV-fold config live.
  A misnamed indicator is a hard `RunError::UnknownIndicator` (never a silent full-catalogue fallback).
- **Item 2 / AC (a) — cardinality feeds the LIVE `N`.** For a steered run, `n_trials =
  effective_trials_with_features(occupied, generations, windows, available_feature_space(catalogue_count_in_play, 0))`;
  un-steered keeps plain `effective_trials`. Monotone (`feature_space ≥ 1`) ⇒ steered `N` ≥ un-steered. Proven by
  `steered_run_records_steer_delta_raises_the_trial_basis_and_stays_golden_for_unsteered` (steered
  `seal_evidence.n_trials` strictly exceeds the un-steered run's).
- **Item 3 / AC (c) — archive-coverage floor enforced live.** `enforce_coverage_floor(is_steered, occupied)` fails
  a steered run whose occupied niches fall below `MIN_OCCUPIED_NICHES` (surfaced, never silently sealed); un-steered
  runs are never newly gated. Proven by `coverage_floor_is_enforced_only_on_steered_runs`.
- **Item 1 / AC (e) — steer delta written at seal.** `SteerDelta::from_parts(catalogue_ids_in_play, &[], gens, pop,
  windows, folds)` (single hashing source in `qe-vintage`) is set on `content.provenance.steer_delta` for steered
  runs, `None` for un-steered. Proven by the integration test above (un-steered `steer_delta` is `None`, steered
  records the subset hash + budget/window/fold counts; un-steered content hash reproduces byte-identically).
- **`evolved_pool` / `evolved_formulas` — REJECTED at `validate_train`, not silently ignored.** Consuming a sealed
  GP `ExprTree` pool as a *strategy* indicator would change the feature-space width and thus the `CatalogueIdentity`
  (breaking QE-402 backtestability), so it cannot be applied golden-safely on the live strategy search today. Per
  the reviewer's sanctioned alternative to silent-ignore (item 4), a steered request naming an evolved pool is a
  `400` ("not yet supported on the live train search"). Proven by
  `validate_train_rejects_evolved_pool_as_not_yet_supported_not_silently_ignored`. A QE-402-safe feature-space
  extension for evolved-pool-as-indicator is a genuine follow-up. `included_evolved_formulas` (the read-only,
  firewall-safe pool counter) stays in place for that follow-up; the firewall test remains green.

The end-to-end path is threaded: server `spawn.rs` `train_args` emits `--indicator`/`--windows`/`--folds` (never
`evolved_*`), the CLI parser + `Command::Train` + `TrainOptions` carry them, and `run_train_job` applies them.
