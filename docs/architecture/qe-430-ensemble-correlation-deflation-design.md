# QE-430 — Deflate the ensemble correlation penalty by sample size (design / evidence note)

**Ticket:** QE-430 (Review R2.a, panel rec #1, unanimous).
**Spec of record:** `docs/reviews/2026-07-16-maxdama-panel-review.md#qe-430`.
**Branch:** `qe-430/ensemble-correlation-deflation`.

## 1. Current-state evidence (read before touching code)

### The correlation term and where it is minimised
- `crates/ensemble/src/objective.rs`
  - `pearson(a, b)` (line 26): a **raw sample Pearson** with no sample-size awareness. Returns `0.0`
    on length mismatch / empty / zero-variance.
  - `positive_mean_pairwise_corr(series)` (line 51): mean over all member pairs of `max(pearson, 0)`.
    Negative correlation floors to `0` (a diversification benefit).
  - `objective(pool, members, cfg)` (line 176): `mean + tail_weight·CVaR − corr_weight·corr`. The
    correlation penalty is **subtracted**, so the DE search **minimises** it → prefers low sample corr.
  - `ObjectiveConfig` (line 144): `alpha`, `tail_weight`, `corr_weight`. No deflation knob today.
  - `TailRisk { value, tail_n }` (line 15): the pattern we mirror — a statistic plus the sample count
    it rested on (`cvar` returns `tail_n = ⌈alpha·n⌉`).
- `crates/ensemble/src/search.rs`
  - `cross_val_score(pool, members, cfg)` (line 82): partitions the common time axis into `cfg.folds`
    contiguous folds, **slices the pool per fold** (`s[lo..hi]`, line 99-103), and scores
    `leave_one_out_min` **within each fold**. So the correlation penalty is evaluated on fold slices of
    length ≈ `t/folds` — this is the "≈t/4 points" the spec flags.
  - `run_de(...)` (line 146): DE/rand/1/bin over a closure `score: impl Fn(&[usize]) -> f64`. Builds
    `SearchResult`. Generic — no `pool`/`folds` access.
  - `SearchResult` (line 65): `best`, `score`, `generations_run`, `history`.
- `crates/ensemble/src/regime.rs`: `objective(...)` (line 86) rides the same base correlation penalty,
  so a change in `objective` flows through the regime-aware path automatically.

### Key observation — N is *already* the fold-slice length
`cross_val_score` slices the pool **before** calling `leave_one_out_min → objective →
positive_mean_pairwise_corr → pearson`. The series handed to `pearson` therefore already have
length = fold-slice length. So making the penalty a function of `N = series.len()` **threads the actual
fold-slice length by construction** — no extra plumbing through the DE closure is needed. Direct
(non-fold) `objective` calls naturally use the full series length, which is the correct `N` for them.

### Consumers / score record
- `crates/cli/src/jobs/train.rs`
  - line 384: `qe_ensemble::search_portfolio(&pool, &ens_cfg, params.seed)` with `ENSEMBLE_FOLDS = 4`,
    over the elite `pool` of per-member net-of-cost return series.
  - `TrainResultDoc` (line ~185-219): the **ensemble score record** — carries `ensemble_score`,
    `selected`, `weights`. This is where the spec's "effective N recorded alongside the correlation
    penalty" lands. `TrainResultDoc` is the `result.json` sidecar, **not** part of `VintageContent`, so
    adding a field does not by itself move the vintage `content_hash`.
  - The vintage `content_hash` derives from `VintageContent` (chromosomes/weights/calibration/wcl). It
    moves only if the **selected ensemble changes** — which enabling deflation may cause.

### Golden / determinism surface
- `crates/cli/tests/fixtures/golden_result.json` is a **backtest** golden over a committed
  `sample_vintage` fixture (content_hash `f59b27…`). QE-430 touches only the ensemble
  **selection/scoring** in the *train* path; it does not regenerate `sample_vintage` nor touch the
  backtest. Expectation: this golden does **not** move. (Verified by running the suite.)
- `crates/cli/tests/train_job.rs` asserts **determinism** (same seed ⇒ same id+hash), not a literal
  hash. No pinned train `content_hash` literal exists anywhere (grep-confirmed).
- `crates/determinism/` harness does not reference train/ensemble (grep-confirmed).

## 2. Decisions

1. **Two selectable modes on a config enum** `CorrDeflation`:
   - `None` — raw sample Pearson (the reproducible A/B + golden toggle).
   - `SignificanceFloor { z }` — zero any pair with `|r| < R(N)`, `R(N) = tanh(z/√(N−3))`
     (Dama §6.2 minimum-significant-r; `z = 1.96` default).
   - `FisherShrinkage { lambda }` — `z=arctanh(r)`, `z'=sign(z)·max(0,|z|−λ/√(N−3))`, `r'=tanh(z')`.
   - `N ≤ 3` ⇒ the `1/√(N−3)` scale is undefined/degenerate ⇒ the pair is treated as insignificant
     (contribution `0`). This is the conservative, NaN-safe branch.
2. **Default ON** = `SignificanceFloor { z: 1.96 }` (Dama's canonical curve, listed first in the spec).
   `ObjectiveConfig::with_defaults()` sets it; `CorrDeflation::None` reproduces the raw pre-QE-430 path.
3. **Effective-N exposure mirrors `TailRisk`:** new `CorrPenalty { value, effective_n }` returned by
   `pairwise_corr_penalty(series, mode)`. `effective_n` = the smallest sample size any admitted pair
   rested on (`0` when `< 2` series). `objective` routes through it.
4. **`positive_mean_pairwise_corr` stays as the raw (undeflated) reference** (== `CorrDeflation::None`),
   keeping its existing semantics/tests and serving as the raw toggle's ground truth. The deflated,
   config-aware entry point is the new `pairwise_corr_penalty`.
5. **Score record:** `SearchResult` gains `corr_effective_n` (min fold-slice N the winning mask's
   penalty rested on), computed by a new `corr_penalty_effective_n` helper that **mirrors
   `cross_val_score`'s fold slicing** (so it uses the actual fold-slice length). `TrainResultDoc` gains
   `ensemble_corr_effective_n`, populated from it.
6. **Firewall:** all math stays self-contained `f64` in `qe-ensemble`; no new crate deps. The
   search⟂portfolio firewall (`crates/architecture/tests/firewall.rs`) is untouched.

## 3. Test plan (ACs → tests)

- **AC1 (property — no gaming sub-threshold noise):** `objective.rs` test generates many randomised
  **independent** series at small `N` (deterministic xorshift). For every pair with `|r| < R(N)` assert
  the deflated contribution is **exactly `0`** (identical to the independence baseline), and assert
  `deflated ≤ raw` for all pairs. Encodes "a mask chosen on noise scores no better than independence."
- **AC2 (case — floor vs still-penalised):** a genuinely correlated pair whose sample `r` lands **below**
  `R(N)` is floored to `0`; a **supra-threshold** pair keeps a positive penalty (== raw `r` under the
  significance floor). Plus a Fisher-shrinkage case (supra-threshold shrinks toward but stays `> 0`).
- **AC3 (effective N recorded):** unit test that `pairwise_corr_penalty(&[a,b], mode).effective_n ==
  a.len()`; `search.rs` test that `search_portfolio(...).corr_effective_n` equals the fold-slice length
  for a ≥2-member winner; `train_job.rs` assertion that `ensemble_corr_effective_n` is recorded and
  consistent (`>0` iff ≥2 members selected).
- **Raw toggle:** `CorrDeflation::None` reproduces `positive_mean_pairwise_corr` exactly.
- **Regression:** existing `objective.rs` / `regime.rs` / `search.rs` tests stay green (their pairs are
  near `±1` at their `N`, i.e. supra-threshold, or negative → floored either way).

## 4. Risks

- **Ensemble selection may shift** on the train fixture (deflation removes phantom-diversification
  gradient) ⇒ vintage `content_hash` moves. Mitigation: regenerate goldens **via real code** only, and
  report before→after. `golden_result.json` (backtest path) is expected unaffected.
- **N ≤ 3 folds / tiny windows:** handled by the degenerate-`N` guard (contribution `0`).
- **`atanh(±1)` = ±∞** in Fisher mode: `tanh(∞)=1` recovers the perfect correlation; negatives floor to
  `0` anyway. NaN-safe.
- **Determinism:** deflation is a pure deterministic transform of `f64`s already in the path — no new
  RNG, no ordering change. Determinism tests must stay green.
