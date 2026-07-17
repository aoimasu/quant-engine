# QE-443 — Optional inverse-vol (EWMA) seed weighting + capacity water-fill — design

`Phase: Review R2 (P2 — panel #14, split)` · `Area: hedger / ensemble` · `Depends on: QE-219`
· Spec of record: [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-443`](../reviews/2026-07-16-maxdama-panel-review.md)
· Backlog: [Review R2.b](../backlog.md#review-r2)

## 1. The recommendation was CONSENSUS: SPLIT — read this first

The Max Dama panel did **not** agree inverse-vol risk parity is strictly better than equal weight. It is a
**genuine trade-off**, not a clear win:

- **For 1/N (equal weight):** DeMiguel et al. (2009) show `1/N` is remarkably **out-of-sample robust** —
  it estimates nothing, so it cannot be wrong. Dama §6.2 method 9 endorses it for exactly this reason.
- **For inverse-vol:** Dama's own §6.2 two-strategy mean-variance argument says that when volatilities
  differ, **unequal** weights cancel risk better; capacity caps for **turnover**, not **volatility**, so a
  high-vol member can dominate the combined tail at `1/N`. Dama §6.2 method 10 notes variance is the one
  moment that **is** predictably estimable from a short EWMA. But inverse-vol **reintroduces an estimated
  variance** — the very estimation error `1/N` avoids.

**Panel resolution (verbatim intent):** *"seed only, medium priority, do NOT present as strictly superior;
full RMT/Barra correctly omitted as dimensionality-mismatched."*

**Consequence for this ticket — the design is an OPT-IN capability, defaulted OFF:**

- Equal-weight `1/N` **remains the default**. With the option off, the sealed weights, the vintage id, and
  every committed golden are **byte-identical** to today.
- Inverse-vol (EWMA) seeding is a **config option**; when enabled it seeds the weight budget by inverse-vol
  using a **single** EWMA variance decay constant (low-parameter, deterministic), then the **existing**
  QE-128 capacity `cap_weights` water-fill layers on top — exactly
  `weights = cap_weights(inverse_vol_seed(returns, decay), capacities, target_aum)`.
- This note and the code docs present the trade-off **honestly**. We do **not** claim superiority. Enabling
  the option is a deliberate, evidence-gated choice, not a recommended default.

## 2. Where weights are seeded today (the 1/N seed point, per QE-438)

Two code paths build the same weighted object (QE-438 keeps *select* == *deploy*):

1. **Deployed seal** — `crates/cli/src/jobs/train.rs::cap_or_equal`:
   ```
   let equal = vec![1.0 / k; k];
   let capped = cap_weights(&equal, capacities, TARGET_AUM_USD);   // QE-128 water-fill
   ```
   `capped` (hash-stable rounded) is sealed into the vintage as `VintageContent.weights`.
2. **DE membership scoring** — `crates/ensemble/src/objective.rs::Weighting::member_weights`:
   ```
   let equal = vec![1.0 / k; k];
   cap_weights(&equal, &caps, target_aum)   // Weighting::CapacityCapped
   ```

`qe-hedger` (`bootstrap.rs`, `evaluator.rs`) **consumes** the sealed `weights` read-only; it does not
re-seed. So the functional seed point is the two `equal = vec![1/k; k]` lines above — this ticket makes the
**deployed** seed (path 1) pluggable.

## 3. What we build

### 3.1 The seed function (in `qe-ensemble`, firewall-safe)

`crates/ensemble/src/capacity.rs` (next to `cap_weights`, no new crate edges — pure `f64`, so **no**
`qe-wfo → qe-ensemble` edge is introduced; the firewall test stays green):

- `pub const DEFAULT_EWMA_DECAY: f64` — the **single** decay constant `λ` (RiskMetrics-standard `0.94`).
  This is the *one* free parameter, and it is the estimation knob the panel flagged; documented, not hidden.
- `pub fn ewma_variance(returns: &[f64], decay: f64) -> f64` — deterministic EWMA of squared deviations
  from the EWMA mean, walked oldest→newest in fixed order (no schedule dependence). Single `λ`.
- `pub fn inverse_vol_seed(series: &[Vec<f64>], decay: f64) -> Vec<f64>` — per member `i`,
  `vol_i = sqrt(ewma_variance_i)`; seed `w_i = (1/vol_i) / Σ_j (1/vol_j)`, summing to 1.
  - **Equal-vol members reduce to exactly `1/N`** (all `1/vol` equal ⇒ normalise to `1/N`) — an AC.
  - **A higher-vol member gets a strictly lower seed weight** — an AC.
  - **Degenerate guard:** if any member's vol is non-finite or `≤ 0` (a flat/too-short series has no
    estimable variance), or the inverse-vol sum is non-finite/zero, the function **falls back to equal
    weight** `1/N`. This is deterministic and documented — an unmodellable member vol defeats risk parity,
    so we preserve the OOS-robust `1/N` rather than invent an epsilon.
- `pub enum SeedWeighting { Equal, InverseVol { decay: f64 } }` with
  `pub fn seed(&self, series, k) -> Vec<f64>`: `Equal ⇒ vec![1/k; k]` (byte-identical to today);
  `InverseVol ⇒ inverse_vol_seed(series, decay)`. `Default = Equal`.

**Determinism / hashing.** `inverse_vol_seed` is `f64` (division, `sqrt`). Where it feeds the sealed vintage
hash the **caller already rounds** the final weights to `hash_stable` (12 dp) — the same treatment the
capacity weights already receive — so cross-platform sub-ULP drift cannot change the sealed bytes. Under
`SeedWeighting::Equal` the vector is `1/k` exactly, identical to the current literal.

### 3.2 Composition (unchanged water-fill layers on top)

`cap_or_equal(k, capacities, seed_weighting, returns)` replaces the hard-coded `equal` with
`seed_weighting.seed(returns, k)`, then calls the **existing, unchanged** `cap_weights(&seed, capacities,
TARGET_AUM_USD)`. The QE-128 water-fill is untouched — it simply distributes whatever seed budget it is
given. So `weights = cap_weights(inverse_vol_seed(returns, decay), capacities, target_aum)` when the option
is on, and `weights = cap_weights(vec![1/k;k], …)` (today's path) when off.

### 3.3 The opt-in config toggle (default OFF, golden-safe)

`crates/config/src/schema.rs::SelectionConfig` gains two `#[serde(default)]` fields:

- `inverse_vol_seed: bool` — default **`false`** (OFF).
- `ewma_decay: f64` — default `DEFAULT_EWMA_DECAY` (`0.94`); validated to `(0, 1)`.

`#[serde(default)]` keeps existing TOML back-compatible (absent ⇒ default). Threaded into `TrainParams`
(runtime param, **not** re-hashed separately) and consumed by `cap_or_equal`.

**Golden safety.** No committed golden pins the config `content_hash` (the config tests assert only
equality/length relations; the determinism golden hashes the RNG stream only; `sample_vintage.json` /
`golden_result.json` are **static** backtest fixtures, not regenerated from live config). With the option
default-OFF the seed is `1/k` and the sealed weights are unchanged, so **no fixture/golden JSON moves** —
verified by `git diff` showing no golden changes.

## 4. Scope decisions (honest boundaries)

- **DE membership-scoring seed (`Weighting::member_weights`) stays equal→cap for this ticket.** Threading
  inverse-vol into the `Copy` `Weighting` descriptor (it would need the member series + decay, and each
  leave-one-out subset would re-seed) is a larger change to the selection object. Given the panel's explicit
  **"seed only, medium priority"** and the opt-in stance, the deployed **seal** is made pluggable and the DE
  scoring is left as QE-438 built it. When the option is **OFF** (default) *select == deploy* is preserved
  exactly. When **ON**, the deployed seal uses inverse-vol while the DE scores on equal→cap — a documented,
  bounded nuance (the seed is a post-selection portfolio-construction refinement). **Follow-up:** extend the
  same `SeedWeighting` into `Weighting` for full select==deploy consistency under inverse-vol.
- **Single-factor BTC-beta neutralization: documented as a follow-up, not built.** The spec offers it
  "optionally … if cheap". In the single-instrument v1 training path (`mean_dollar_adv` is explicitly
  single-instrument; the standing dev set trades one perp per run) every member shares the same instrument
  and hence a trivially common beta, so cross-member beta neutralization is neither cheap nor meaningful
  here. It becomes worthwhile only with a genuine multi-instrument book. Recorded as a follow-up rather than
  built against a degenerate single-factor case.

## 5. Acceptance-criteria → tests

- **Higher-vol member ⇒ lower seed weight** — `inverse_vol_seed` unit test over a low-vol and a high-vol
  member asserts `w_low > w_high`.
- **Equal-vol members reduce to 1/N** — identical-vol members seed to `1/N` within tolerance.
- **Capacity water-fill still layers on top** — `cap_weights(inverse_vol_seed(...), caps, aum)` caps a
  bound member at `capacity/aum` and redistributes, same as over an equal seed.
- **Option OFF is byte-identical** — `SeedWeighting::Equal.seed(returns, k) == vec![1/k; k]` exactly, and
  `cap_or_equal` under `Equal` equals the current path; `git diff` shows no golden JSON moved.
- **Deterministic** — repeated calls on the same inputs give identical vectors; degenerate (flat/short)
  members fall back to `1/N` deterministically.
- **Config default is OFF** — `SelectionConfig::default().inverse_vol_seed == false`, `ewma_decay` validates
  in `(0,1)`.

## 6. Firewall / determinism / composition

- **Firewall (QE-132).** The seed math lives in `qe-ensemble` and uses only `f64` slices; `qe-cli` (the
  composition root) already depends on both `qe-ensemble` and `qe-config`. **No** `qe-wfo → qe-ensemble`
  edge is added; `qe-architecture`'s `firewall` test stays green.
- **Composes with QE-438** (deployed-weight scoring) — the DE still scores the capacity-capped object; the
  seed only changes the *deployed* budget the water-fill starts from (off by default).
- **Composes with QE-440** (concave capacity impact) — `cap_weights`/`capacity` are untouched; the seed
  feeds the same water-fill.
- **Determinism (QE-006).** Single decay constant, fixed-order EWMA walk, `hash_stable`-rounded at the seal
  — byte-reproducible.
