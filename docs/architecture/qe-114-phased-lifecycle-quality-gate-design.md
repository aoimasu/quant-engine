# QE-114 — SPIKE: Phased-lifecycle quality gate — design / decision record

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-113` · **Blocks: QE-123**
`Branch: qe-114/phased-lifecycle-quality-gate`

## Goal (from backlog)

The lifecycle must distinguish exploration vs exploitation and persist only survivors above a quality
threshold.

**Scope / requirements.**
- Define exploration→exploitation transition and survivor persistence rules (exceed threshold +
  survive exploitation phase).
- **Baseline (spec, A1):** the quality threshold is **derived from the full validation distribution**
  per `docs/specs.md` Robustness.
- **Documented alternative (reviewer):** a stricter **train/CV-only** threshold that avoids selection
  leaking the validation distribution into the criterion — recorded as an option to revisit if leakage
  is evidenced; not the baseline.

**Acceptance criteria.**
- [ ] Decision record implements the spec's full-validation-distribution threshold as baseline and
  documents the train/CV-only alternative with its rationale.
- [ ] A test shows early "lucky" candidates are not persisted.

**Out of scope.** Holdout gate G1 (QE-134); writing survivors to the strategy repository (QE-123); the
archive itself (QE-118).

## Current-state evidence

- **QE-113** (`qe_wfo::fitness`) gives the candidate's fitness as a **distribution**:
  `NoiseRobustFitness { mean, std_error, n }` (per-window log-growth mean ± SE over `n` windows), plus
  the SE-aware `should_replace`. The lifecycle reads this directly — a candidate's `n` is how many
  windows it has been evaluated on, and its `mean ± k·se` is its robust quality.
- **QE-111/112** established the archive/operator side; this ticket is the **persistence filter** that
  decides which evaluated genomes graduate from the search into the (eventual QE-123) repository.

## Decision

### D1 — Two lifecycle phases, gated by evaluation depth

A candidate is in one of two phases, determined by how many windows it has survived:

- **Exploration** — `n < min_exploitation_windows`. Freshly-proposed / shallowly-evaluated genomes.
  **Never persisted**, however high a single-window fitness looks — this is precisely what stops an
  early *lucky* one-shot candidate (huge `mean`, `n = 1`) from being recorded.
- **Exploitation** — `n ≥ min_exploitation_windows`. The candidate has been re-evaluated across enough
  windows for its `mean ± se` to be trustworthy and is *eligible* for persistence.

`min_exploitation_windows` (default **5**) is the exploration→exploitation transition; configurable.

### D2 — Survivor persistence rule: graduate **and** clear the threshold robustly

A candidate **persists** iff *all* hold:

1. **Phase:** it is in **Exploitation** (`n ≥ min_exploitation_windows`) — graduated, not lucky-early.
2. **Finite:** `mean` is finite — a candidate ruined in any window (QE-113 ⇒ `−∞`) never persists.
3. **Robustly above threshold ("exceed threshold + survive exploitation"):** its **lower confidence
   bound** clears the quality threshold:
   ```
   mean − k_sigma · std_error  ≥  threshold
   ```
   Using the lower bound (not the raw `mean`) is what "survive the exploitation phase" means: a
   high-but-noisy candidate (lucky variance) whose band dips below the bar is rejected; only a genome
   whose quality is *robustly* above the bar survives. `k_sigma` reuses the QE-113 default (1.0).

### D3 — Quality threshold: **baseline = full validation distribution** (spec A1)

The threshold is **derived from the full validation distribution** — the spec's A1 wording
(`docs/specs.md` Robustness). Concretely, `QualityThreshold::from_distribution(samples, policy)` over the
population's validation fitnesses, with a configurable policy:

- `ThresholdPolicy::Quantile(q)` — persist candidates at/above the `q`-quantile of the distribution
  (**baseline default `q = 0.75`**: only the top quartile of the validation distribution graduates).
- `ThresholdPolicy::MeanPlusSigma(k)` — at/above `mean + k·sd` of the distribution.

Non-finite (ruined) samples are excluded from the distribution so they cannot drag the bar; an empty
finite distribution yields `+∞` (nothing persists — fail-safe).

### D4 — Documented alternative: train/CV-only threshold (not the baseline)

**Concern (reviewer).** Deriving the bar from the **validation** distribution lets the selection
criterion *see* the validation outcomes — a soft form of selection leakage: the threshold co-moves with
the very distribution it judges, so "above-threshold on validation" is partly self-fulfilling.

**Alternative.** Compute the identical `QualityThreshold` from the **train/CV** fitness distribution
only, and apply it to validation fitness — the bar is then set without touching validation, removing
that leakage channel. The mechanism is unchanged (`from_distribution` is source-agnostic — the caller
chooses which distribution to pass); only the *input distribution* differs.

**Decision.** Implement the **spec A1 baseline** (full validation distribution) as the default and
record the train/CV-only variant as a documented, ready-to-enable option to revisit **if leakage is
evidenced** (e.g. by QE-131/QE-134 holdout degradation). Not enabled now — consistent with the
backlog's spec-fidelity stance.

## Module / API plan

New module `crates/wfo/src/lifecycle.rs`, re-exported:

- `Phase { Exploration, Exploitation }`.
- `ThresholdPolicy { Quantile(f64), MeanPlusSigma(f64) }`; `QualityThreshold` (`from_distribution`, `value`).
- `QualityGate { policy, min_exploitation_windows, k_sigma }` with `with_defaults()`:
  - `phase(&NoiseRobustFitness) -> Phase`;
  - `threshold(distribution: &[f64]) -> QualityThreshold` (baseline: pass the **validation** means);
  - `persists(&NoiseRobustFitness, &QualityThreshold) -> bool` (D2);
  - `survivors<'a>(&[(&'a T, NoiseRobustFitness)], &QualityThreshold) -> Vec<&'a T>` convenience.
- Consumes QE-113 `NoiseRobustFitness` / `DEFAULT_K_SIGMA`; no new dependencies.

## Test plan (TDD)

1. **Threshold from distribution.** `Quantile(0.75)` and `MeanPlusSigma(k)` match hand-computed values;
   ruined samples excluded; empty finite distribution ⇒ `+∞`.
2. **Phase transition.** `n < min` ⇒ Exploration, `n ≥ min` ⇒ Exploitation.
3. **Early lucky candidate not persisted (AC).** A candidate with `n = 1` and a huge `mean` (top of the
   distribution) is in Exploration ⇒ **not persisted**; the same fitness once `n ≥ min` with a tight band
   *is* persisted — isolating depth as the gate.
4. **Survive-exploitation robustness.** A graduated candidate with high `mean` but large `se` whose lower
   bound dips below the bar is **not persisted**; a robust one above the bar is.
5. **Ruin never persists; below-bar never persists.**
6. **`survivors` filter** returns exactly the persisted candidates.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-wfo`,
`cargo test --workspace`.

## Risks

- **Validation-distribution leakage (the A1 concern).** Carried deliberately per spec fidelity; the
  train/CV-only alternative (D4) is implemented-by-parameter and ready to switch on if QE-131/QE-134
  evidence leakage. Flagged, not silently accepted.
- **Quantile policy is judgement.** `q = 0.75` is a pre-data default; tune against real archive-fill /
  survivor counts (config via QE-002 when QE-123 wires the repository).
- **`min_exploitation_windows` vs noise.** Too low re-admits lucky candidates; too high starves the
  repository. Default 5 mirrors the QE-113 multi-window default; revisit with QE-124 robustness evidence.
- **Quantile estimator.** Nearest-rank on the sorted finite sample (no interpolation) — simple and
  deterministic; adequate for a gate, not a reporting statistic.
