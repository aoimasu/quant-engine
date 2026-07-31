# QE-491 — Ensemble objective: retire the redundant correlation penalty, carry diversification honestly

> Design note for [QE-491](../mds/tickets/QE-491.md). Source: a 3-approach research panel (portfolio-theory /
> calibrate-&-constrain / deflation-consistent) + synthesis, verified against `crates/ensemble/src/objective.rs`.
> Follows PR#182 review finding **R3.1** (verified) — the ensemble selection objective is decorrelation-dominated at
> realistic Sharpe.

## 1. Diagnosis (settled — all three approaches converged)

The differential-evolution portfolio search maximizes `objective_weighted` (`objective.rs:527-563`):

```
obj = mean/risk  +  tail_weight·(CVaR/risk)  −  corr_weight·corr          (objective.rs:563)
```

with `risk = return_volatility(combined)` and defaults `tail_weight = corr_weight = 1.0`.

**The penalty double-counts diversification.** Because `risk` is the volatility of the **combined** ensemble and
`σ²_combined = Σᵢⱼ wᵢwⱼσᵢσⱼρᵢⱼ`, the `mean/risk` (Sharpe) term already prices the Markowitz (1952) diversification
benefit — and prices it *correctly*, proportionally to the members' own Sharpe (near-zero credit for near-zero-mean
members). The explicit `− corr_weight·corr` is a **second charge for the same property**, and it is **miscalibrated**:
a flat `∈ [0,1]` axis with high between-candidate spread that pays full `corr` regardless of returns, while the
correctly-scaled Sharpe channel has small spread and contributes only `≈ 2·Sharpe ≈ 0.2` at realistic per-bar Sharpe
`≈ 0.1`. At `ρ ≈ 0.5` the penalty is `≈ 0.5` and **outvotes** the return signal — the residual the pinned test
`decorrelation_still_dominates_at_realistic_low_sharpe` verifies. So the defect is **structural, not a tuning miss**:
QE-475 fixed the magnitude but left *both* channels paying for decorrelation.

Two corollaries:
- The **`CVaR/risk` term is a near-constant shape factor** (`≈ −2..−2.5` across candidates, R3.3) — almost no
  discriminating power, and it partly double-counts the gate's DSR skew/kurtosis deflation (`dsr.rs`).
- The penalty's one **non-redundant** job was defending against **phantom in-sample decorrelation** (QE-430): pairs
  whose sample correlation dipped low by luck on a short fold slice (an overfitting hazard — López de Prado CSCV/PBO).
  **Dropping the penalty does not remove this hazard — it *relocates* it into `σ_combined`** (spuriously-low sample
  corr → spuriously-low vol → spuriously-high in-sample Sharpe). That relocation is what separates a band-aid from a
  principled fix.

## 2. Options considered

| Approach | Core change | Soundness | DSR/OOS consistency | Effort | Sealed-history risk | Verdict |
|---|---|---|---|---|---|---|
| **Q0 — drop the additive penalty** (`corr_weight → 0`), flag-gated | remove the swamping axis; let `mean/σ_combined` carry diversification | High but **incomplete** — relocates the phantom-decorrelation overfit | necessary, not sufficient | ~hours | low (flagged) | **Step 0 (this PR)** |
| **P1 — shrinkage-deflated risk denominator** (Ledoit–Wolf → Elton–Gruber constant-corr target, `δ = δ(N)` from QE-430) + excess-tail | the only mechanism that guards the *relocated* overfit, in return/risk units | best — one Sharpe object the G1 gate deflates | moderate | low (enum default = legacy) | **Step 1 — primary lever** |
| **P2 — deflated-correlation cap** (admissibility, not scoring) | Michaud (1989): constraints dominate point-estimate optimization OOS | good — guards the near-duplicate direction P1 does not | ~1 day | low | **Step 1 — complementary guardrail** |
| **P3 — score DE on Deflated Sharpe** | align selection with the gate | **rank-invariant** — DSR with constant `(V,N)` is a monotone transform of Sharpe, so it does *not* re-rank; adds an ensemble→validation firewall edge for ~nil benefit | moderate–high | — | **rejected as a selection lever** |

## 3. Recommendation

**Demote-and-replace the penalty; let a shrinkage-honest combined Sharpe carry diversification, with a correlation cap
as a cheap structural backstop.** Sequenced:

**Step 0 (this PR — interim de-risk, flagged).** `enum RiskModel { RealizedVol, ShrunkCorr }` on `ObjectiveConfig`.
Default `RealizedVol` = shipped QE-475 (byte-identical — no golden/v8 change). Arm B `ShrunkCorr` retires the explicit
penalty (realized-vol denominator, δ ≡ 0). This removes the *verified* dominance defect immediately, gated for A/B.
**Step 0 alone is not the fix** — it relocates the phantom-decorrelation overfit into `σ_combined`.

**Step 1 (follow-up — the principled fix).** Under `ShrunkCorr`, replacing `objective.rs:554-563`:

```
obj = mean(combined) / risk_shrunk  +  tail_weight · excess_tail          (over a feasible set)

risk_shrunk = sqrt(wᵀ · Σ_shrunk · w),  w = deployed member weights
Σ_shrunk    = (1−δ)·Σ_sample + δ·Σ_target        // Ledoit–Wolf (2004)
Σ_target    = constant-correlation at ρ̄          // Elton–Gruber
δ = δ(N)                                          // ↑ as fold-slice N ↓; reuse QE-430 min_significant_r curve
excess_tail = CVaR(combined, α)/risk_shrunk − ES_gauss(α),   ES_gauss(0.05) ≈ −2.063
keep the ×1e-9 relative-vol floor + 1.0 fallback (objective.rs:557-561)

// admissibility (P2 backstop), NOT a scoring term:
if deflated_max_pairwise_corr(members) > max_corr:  obj −= barrier·(corr − max_corr)
   max_corr ≈ 0.40, barrier ≈ 100.0, bound on the binding provenance value (objective.rs:266)
```

The shrinkage denominator is the only mechanism that closes the *relocated* overfit — it pulls `σ_combined` toward the
constant-correlation target exactly when N is too small to trust a low sample correlation, expressing QE-430's intent as
a risk *estimate* rather than an ad-hoc subtraction, and it credits diversification proportionally to members' Sharpe
(`S_combined = S_avg·√(k/(1+(k−1)ρ))`), so a near-zero-mean decorrelated noise cluster earns near-zero credit. The
correlation **cap** is complementary: shrinkage guards spuriously-**low** correlation; the cap guards genuinely-**high**
near-duplicates. `excess_tail` restores the dead CVaR term's discrimination.

## 4. Validation protocol (the ship gate)

Selection changes are unfalsifiable on the in-sample objective value. The arbiter is the engine's own honesty stack on
**held-out** data. Arm A = legacy; Arm B = `ShrunkCorr` (shrinkage + cap on).

- **(A) Synthetic planted ground truth** (deterministic xorshift, scale up the fixture at `objective.rs:854-879`):
  a high-Sharpe correlated cluster (ρ≈0.5, S≈0.15) vs a zero-Sharpe decorrelated cluster — assert **B selects the
  high-Sharpe cluster, A selects the noise cluster**, and B's `dsr_p05` clears `CpcvGate.dsr_floor` (`cpcv.rs:318`)
  while A fails; a **phantom-decorrelation pool** (low correlations *insignificant* at fold N) — assert `ShrunkCorr`
  (δ>0) does not over-credit it where the δ≡0 Step-0 variant does (isolates the shrinkage term's value). Sweep
  ρ×Sharpe to map A's damage zone (largest B-wins at ρ≈0.5, Sharpe≈0.1).
- **(B) Real Binance walk-forward** (`data/lmdb/market`, wiring per `train.rs:801-905`): A vs B side-by-side; compare
  CPCV `dsr_p05`, PBO, realized OOS combined Sharpe, and `g1.promoted` rate.
- **(C) Determinism:** two same-seed runs byte-identical.

**Pass criterion (hard gate):** ship B **only if** CPCV held-out median Sharpe ≥ A **AND** `dsr_p05` ≥ A **AND** PBO not
worse, on **both** synthetic and real. B must dominate on what survives deflation, not on in-sample score.

## 5. Rollout & sealed history

- **No `VINTAGE_FORMAT_VERSION` bump** — that is a *schema* version; this is a *behaviour* change and the artifact
  schema is unchanged (bumping would wrongly invalidate v8 vintages).
- `ObjectiveConfig::with_defaults` stays `RealizedVol` (byte-identical; no golden pins ensemble membership). Arm B is
  opt-in via `SearchConfig`; only flagged A/B vintages select differently. **The flag is the "we accept new
  selections" switch.**
- Flip the default to `ShrunkCorr` **only after §4 clears**, as an explicit signed-off decision; seal both arms in
  parallel; report deltas through the QE-469 CPCV OOS-evidence surface.
- Document: the double-count diagnosis; that `excess_tail` can go slightly negative for thin-tailed candidates *by
  design*; and that the QE-451 provenance floor (`objective.rs:257-300`) must be re-cast as a structural admissibility
  constraint (or provenance-aware shrinkage target) — once the penalty leaves the maximand its provenance hook stops
  binding and must not be silently dropped.

## 6. Open questions (human calls)

1. Ship Step 0 (flagged, δ≡0) and Step 1 (shrinkage + cap) as two PRs, or hold Step 0 until the shrinkage guard lands so
   the relocated overfit is never exposed even behind a flag? (Recommendation: hold — one clean PR.)
2. Keep the tail term at all, or set `tail_weight = 0` and let the CPCV/CVaR gate own tail entirely (given R3.3)?
3. `max_corr` (~0.40) and `δ(N)` — bind to the QE-430 significance curve, or expose as tunables? Under-shrink leaks
   phantom decorrelation; over-shrink kills genuine diversification.
4. Re-express the QE-451 provenance floor as a hard admissibility cap or a provenance-aware shrinkage target (policy).
5. Sign-off owner for the default flip, and the OOS-Sharpe / `dsr_p05` *delta* (not just "≥") that is the promotion bar.
