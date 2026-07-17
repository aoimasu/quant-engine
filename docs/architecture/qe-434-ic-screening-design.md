# QE-434 — Per-indicator IC / information-horizon screening (catalogue-admission pre-filter)

`Ticket:` [QE-434](../reviews/2026-07-16-maxdama-panel-review.md#qe-434) ·
`Area:` signal / validation · `Depends on:` QE-131, QE-107 · `Effort:` M ·
`Spec ref:` maxdama §4.8 "Regression" (IC, sign-flip, unit conversion) + §4.10 checklist "information horizon".

## 1. Problem / why (current-state evidence)

grep across the workspace confirms **no** regression / IC / Spearman / rank-IC / forward-return
diagnostic exists today:

```
$ rg -i 'rank.?ic|spearman|information.?coef|\bIC\b' crates --type rust   # → no hits
```

Every catalogue indicator (`crates/signal/src/indicator/`, ≥20 indicators, each a quantised
`QState` in `0..num_states`) enters the MAP-Elites / DE search **unconditionally**
(`crates/signal/src/feature.rs` assembles the full `FeatureSchema`; the genome discovers
sign + threshold per clause with no prior evidence the factor predicts forward return).

Consequences the panel flagged (rec #5):
- **No table-stakes "does the signal even work, and at what horizon" check** (Dama §4.8/§4.10).
- **Inflated effective search dimensionality** — dead factors still cost `cells·gens·windows`
  trials the DSR (`crates/validation/src/dsr.rs::effective_trials`) must deflate against. The
  screen shrinks the *compute* the search roams, **never** the hypothesis/trial count DSR uses.

## 2. What the spec requires (ACs, restated)

- Compute **rank-IC** (Spearman of per-bar indicator signal vs forward **net** returns) for each
  catalogue indicator, **out-of-fold**, on the training/CV span; report **IC-by-horizon**.
- **Drop or flag** zero-IC factors before they enter the search.
- **Admit** an indicator only if a **second fold** shows **same-sign** IC of **comparable
  magnitude** AND it clears a **Benjamini–Hochberg FDR** threshold across all indicators screened.
- The screen filters **COMPUTE, never the hypothesis count**.
- Covered by focused unit/property tests; if a golden/vintage moves, regenerate via real code and
  track the `content_hash`.

## 3. Design decisions

### 3.1 Home: `crates/validation` (new module `ic.rs`)

- `qe-validation` is the statistics crate: it already carries the moment/Sharpe/normal primitives
  (`stats.rs`) and depends **only** on `qe-determinism` — so it "never touches the
  search⟂portfolio firewall" (its own lib doc). Adding IC there keeps that property: the module is
  **pure numeric** (operates on `&[f64]` signal columns + a net-return series + fold index sets),
  taking **no** dependency on `qe-signal` or `qe-wfo`.
- `qe-signal` is explicitly **storage-free / hot-path** ("depends on `qe-domain` only"); it must
  **not** gain `qe-validation` (+ its `qe-determinism`) weight. So the screen does **not** live
  inside the signal production graph.
- `qe-wfo` (purged CV) and `qe-validation` are **independent** crates (cli depends on both). The
  screen therefore takes fold **index sets** as inputs; the integration caller (cli train job)
  builds those from `qe-wfo::PurgedKFold` (`crates/wfo/src/cv.rs`) so screening runs **on the
  purged/embargoed CV span, out-of-fold**, with the existing leakage-safe geometry — no new purge
  logic, no wfo↔validation coupling.

### 3.2 Firewall / leakage safety

- No new crate edges: `qe-validation` keeps depending only on `qe-determinism` (+ serde/thiserror).
  `cargo test -p qe-architecture --test firewall` is unaffected (verified in the green gate).
- Leakage: the signal↔forward-return pairing is **out-of-fold**. Fold index sets come from the
  purged+embargoed `cv.rs` (`Fold::windows_disjoint` already proves train/test information windows
  are disjoint by `lookback + label_horizon`). The forward-return horizon `h` is the label horizon;
  the caller sizes the CV `label_horizon ≥ max screened horizon` so no forward window reaches across
  a fold boundary. The library computes forward returns **causally** (bar `t`'s label = net return
  over `t+1..=t+h`; the last `h` bars are `NaN`/undefined and dropped from the pairing).

### 3.3 Report-only + opt-in (no golden movement) — **the chosen posture**

The task allows a "non-behaviour-changing reporting/admission layer that does NOT move goldens
(screen can be report-only + opt-in first)". **I chose report-only.** QE-434 lands the full
screening **capability** in `qe-validation` — pure functions + an `IcScreenReport` — fully tested,
but **not wired into the default train/search path**. Therefore:

- **No indicator is removed from the live search by default**, so **which indicators enter the
  search is unchanged → no vintage/golden/`content_hash` moves**, and no `VINTAGE_FORMAT_VERSION`
  bump is needed (no new hashed field enters any artefact).
- Actually *enforcing* admission (dropping factors from the catalogue the search sees) would move
  goldens; that wiring is a deliberate **follow-up** (opt-in flag on the train job), out of scope
  here per the guidance. This keeps QE-434 a safe, additive, well-tested statistics deliverable.

### 3.4 Statistics

- **Rank-IC** = Pearson correlation of the **average-rank** transforms of the paired (signal,
  forward-return) samples → tie-corrected Spearman. Returns `None` if `< 2` finite pairs or either
  side is dispersionless.
- **p-value**: large-sample normal approximation under the no-correlation null,
  `z = ic·√(n−1)`, two-sided `p = 2·(1 − Φ(|z|))` (Φ = `stats::normal_cdf`). Documented as the
  large-sample approximation; adequate for a screen (`n` is the CV-span bar count, hundreds+).
- **Benjamini–Hochberg**: step-up at level `q` over the per-indicator (primary-horizon, pooled-fold)
  p-values — the largest rank `k` with `p_(k) ≤ (k/m)·q` rejects ranks `1..k`. Returns an
  admit-mask aligned to input order.
- **Per-indicator verdict** at the primary horizon (the horizon maximising mean |fold-A, fold-B IC|):
  - `Drop`  — `|mean OOF IC| < min_abs_ic` (a zero-IC / noise factor).
  - `Admit` — sign-consistent across folds (same non-zero sign **and** comparable magnitude,
    `min(|a|,|b|)/max(|a|,|b|) ≥ magnitude_ratio`) **and** passes BH-FDR.
  - `Flag`  — has non-trivial IC but fails second-fold sign consistency or the FDR bar.
- Config defaults: `fdr_q = 0.05`, `min_abs_ic = 0.02`, `magnitude_ratio = 0.5`, horizons caller-set.

## 4. Public API (in `qe-validation`)

```rust
// ic.rs
pub fn rank_ic(signal: &[f64], forward: &[f64]) -> Option<f64>;       // tie-aware Spearman
pub fn spearman_pvalue(ic: f64, n: usize) -> f64;                     // two-sided normal approx
pub fn benjamini_hochberg(pvalues: &[f64], q: f64) -> Vec<bool>;      // BH admit mask
pub fn forward_returns(net_per_bar: &[f64], horizon: usize) -> Vec<f64>; // causal forward sum

pub struct IcScreenConfig { pub horizons: Vec<usize>, pub fdr_q: f64,
                            pub min_abs_ic: f64, pub magnitude_ratio: f64 }
pub enum Verdict { Admit, Flag, Drop }
pub struct HorizonIc { pub horizon, ic_fold_a, ic_fold_b, ic_pooled }
pub struct IndicatorScreen { id, horizons, primary_horizon, ic_fold_a, ic_fold_b,
                             sign_consistent, pvalue, passes_fdr, verdict }
pub struct IcScreenReport { indicators, fdr_q, min_abs_ic, magnitude_ratio }

pub fn screen_catalogue(signals: &[IndicatorSignals], net_returns: &[f64],
                        fold_a: &[usize], fold_b: &[usize],
                        cfg: &IcScreenConfig) -> IcScreenReport;
```

`IndicatorSignals { id: String, values: Vec<f64> }` — one column per catalogue indicator, the
per-bar ordinal `QState::index() as f64` (caller maps `None` → `f64::NAN`). `IcScreenReport` is
`serde` so a later reporting artefact / opt-in admission step can persist it.

## 5. Test plan (prove-it / AC)

1. **rank-IC correctness on known-correlated synthetic series**: perfect monotone → `+1`; perfect
   anti-monotone → `−1`; a hand-computed tie case; independent noise → `≈0`; degenerate/short → `None`.
2. **forward_returns** causal alignment: value at `t` = sum of next `h` net returns; last `h` = NaN.
3. **two-fold sign-consistency**: an indicator with same-sign comparable IC in both folds →
   `sign_consistent = true`; a **sign-flip** between folds → `false` → **not** `Admit`.
4. **BH-FDR admits/rejects correctly**: textbook p-vectors → exact admit masks (all-reject and
   single-reject cases); monotonicity of the step-up.
5. **a purely-noise indicator is Dropped**: an independent (RNG, seeded via `qe-determinism`) signal
   column comes out `Drop` (zero-IC), while a genuinely predictive column is `Admit`.
6. **screen shrinks compute, not hypotheses**: assert the report classifies (admit/flag/drop) but
   the module exposes no path that mutates any trial/hypothesis count (documented invariant + no
   dependency on dsr trial counts).

## 6. Risks / mitigations

- **Normal-approx p-values** vs exact `t`: safe for a screen at large `n`; documented. Not used for
  any downstream certified statistic (DSR/PBO/SPA untouched).
- **Golden movement**: avoided by the report-only posture (§3.3). If a later ticket wires enforced
  admission, it regenerates goldens via real code and tracks the `content_hash` then.
- **Determinism**: pure `f64`, fixed ordering, no unordered iteration in the numeric path; the noise
  test draws from the seeded portable RNG (`qe-determinism`) so it is reproducible.
```
