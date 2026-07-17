# Review R2 — Max Dama panel: ticket specs (QE-430..QE-433)

> **Provenance.** A six-discipline panel — two senior quant researchers, two mathematicians, a senior
> software engineer, and a trading expert — compared *Max Dama on Automated Trading* against the engine
> on 2026-07-16 (independent analysis → debate → chair synthesis). The full report (area-by-area
> verdicts, the sharpest debates, and all 20 ranked recommendations) is
> [`docs/reviews/maxdama-vs-quant-engine.html`](./maxdama-vs-quant-engine.html).
>
> This document is the **spec of record** for the four **high-priority** recommendations (panel rank
> #1–#4) that were ticketed into [Review R2](../backlog.md#review-r2). The remaining medium/low items
> stay in the report for later triage. Same QE-4xx review/hardening band conventions as R1: these are
> cross-cutting correctness items, not new spec features, and do not move the P0–P2 gates.

---

<a id="qe-430"></a>
## QE-430 — Deflate the ensemble correlation penalty by sample size (R(N) floor / Fisher-z shrinkage)

`Phase: Review R2 (P1 — statistical correctness)` · `Area: ensemble / portfolio` · `Depends on: QE-115, QE-126` · `Panel: rec #1 (unanimous)`

**Why.** The DE ensemble search selects the member mask by **minimising** `positive_mean_pairwise_corr`
(`crates/ensemble/src/objective.rs:51`), built from a **raw sample Pearson** (`objective.rs:26`) with no
sample-size awareness — and it is scored **inside the fold-CV loop** (`crates/ensemble/src/search.rs:91`),
so each correlation rests on only ≈`t/4` points. That is squarely Dama §6.2's spurious-correlation regime
(at N=10, |r| must exceed ≈0.63 to be distinguishable from zero). Minimising a noisy statistic over
`K(K-1)/2` pairs and many candidate masks **preferentially admits members whose sample correlation
fluctuated low by luck** — phantom diversification that evaporates out-of-sample and fattens the combined
tail. The repo builds full multiple-testing deflation for the **Sharpe** (`crates/validation`) but applies
**none** to the correlation estimates that drive capital allocation. The CVaR/CDaR term cannot cover this:
it is evaluated **after** selection, so it cannot un-pick a member admitted on a phantom signal (debate
highlight #3; two experts converged on this fix independently).

**Scope / requirements.**
- Make `pearson` / `positive_mean_pairwise_corr` **sample-size aware** in `crates/ensemble/src/objective.rs`,
  configurable via `ObjectiveConfig`, with two selectable modes:
  - **Significance floor** — zero any pair with `|r| < R(N)`, where `R(N) = tanh(1.96/√(N−3))` (Dama's
    minimum-significant-r curve);
  - **Fisher-z shrinkage** — `z = arctanh(r)`, `z' = sign(z)·max(0, |z| − λ/√(N−3))`, `r' = tanh(z')`,
    with `λ` configurable (softer, no cliff).
- Use the **actual fold-slice length** `N` (thread it from the `search.rs` fold slicing), **not** the
  full-window length.
- **Expose the effective N** behind each penalty (mirror how `TailRisk` surfaces `tail_n`) so G1 / the
  score record can flag penalties resting on tiny samples.
- Default the new behaviour **on**; keep raw-Pearson reproducible behind a config toggle for A/B and goldens.

**Out of scope.** Full covariance-matrix / RMT / factor-model estimation (panel judged it
dimensionality-mismatched for this archive); any change to the CVaR/CDaR tail terms; the position-sizing
work (QE-433).

**Acceptance criteria.**
- Property test: over randomised **independent** member series at small N, the DE search **cannot** lower
  the penalty by selecting a sub-threshold sample correlation (a mask chosen on noise scores no better than
  independence).
- A case test: a genuinely correlated pair whose **fold** sample `r` lands below `R(N)` is floored/shrunk to
  ≈0, while a **supra-threshold** correlation is still penalised.
- The **effective N** is recorded alongside the correlation penalty in the ensemble score record.
- If the ensemble output moves, goldens are regenerated **via real code** (never hand-edited); any
  `content_hash` change is tracked; the determinism harness and full suite stay green.

`Spec ref: maxdama §6.2 "Spurious Correlation" (the R(N) curve) + method 3 (regularise toward identity). Cross-cutting: net-of-cost / honest-selection.`

---

<a id="qe-431"></a>
## QE-431 — Calibrate slippage `half_spread` + `impact` from venue data, shared by friction & capacity

`Phase: Review R2 (P1 — net-of-cost truth)` · `Area: wfo friction / ensemble capacity / vintage lineage` · `Depends on: QE-109, QE-128, QE-116, QE-129, QE-006` · `Panel: rec #2 (unanimous)`

**Why.** `SlippageModel { half_spread, impact }` in `crates/wfo/src/friction.rs` and the impact coefficient
in `crates/ensemble/src/capacity.rs` are **hardcoded guesses** (≈1 bp half-spread, friction impact
`1e-4`/contract, capacity `2e-9`/$), living in **two places** and **two unit systems** that can silently
drift. They are **load-bearing**: they price every trade in the **selection-critical net-of-cost fitness**
(a wrong number violates the net-of-cost-truth cross-cutting principle and biases which strategies are
selected), decide whether capacity caps bind (deployed-weight skew), ground any portfolio-Kelly sizing
(QE-433), and are precisely the **systematic per-trade cost bias the Deflated Sharpe cannot remove** (PSR is
absolute vs a noise ceiling, not vs a cost error).

**Scope / requirements.**
- Add an impact/spread **estimator** (maxdama §7.7): from the venue's own BTC/ETH-perp trade+quote history,
  bin trades by size and fit an `impact` coefficient and `half_spread` from the observed spread
  distribution. The perp trade feed **carries aggressor side**, so **skip the Lee-Ready classifier**.
- Emit the fit as **one content-addressed calibration input** (alongside `CalibrationProfile` in the vintage
  lineage, QE-006/QE-116/QE-129) that **both** `friction.rs` and `capacity.rs` read — a single source of
  truth so the two can never drift.
- Keep the fit **reproducible**: computed once on a **pinned input snapshot**, lineage-tracked, so byte-level
  determinism is preserved (this resolves the determinism-vs-data-dependence debate — a fit on a pinned
  input is more Dubno-faithful than a magic constant).

**Out of scope.** The concave **square-root-in-participation impact shape** change (panel rec #11 — separate
ticket); wiring the live `VenueSimulator` to the same calibration input (follow-up); the ADV-ingest work
(rec #11).

**Acceptance criteria.**
- `friction` and `capacity` both consume the calibration input; a test asserts they **agree** for identical
  `(side, qty, mark, spread)` (no unit-drift).
- The calibration is **content-addressed** and rides the vintage lineage; re-running on the same pinned input
  reproduces **byte-identical** coefficients.
- The hardcoded slippage/impact literals are removed from the selection path; a test proves **no magic
  slippage/impact constant remains** on that path.
- Goldens regenerated **via real code**; any `content_hash` move is tracked; determinism harness green.

`Spec ref: maxdama §7.7 "Measuring Impact" (bin trades by size, fit impact = f(volume)). Cross-cutting: net-of-cost truth; determinism / lineage.`

---

<a id="qe-432"></a>
## QE-432 — Independent slow-reference oracle for the reconstruct roll-up & net-of-cost fitness paths

`Phase: Review R2 (P1 — precondition for statistical validity)` · `Area: wfo reconstruct / backtest / friction` · `Depends on: QE-106, QE-120, QE-109, QE-006` · `Panel: rec #3 (unanimous)`

**Why.** Every current "parity" guarantee is **same-code-vs-same-code**: the determinism harness re-runs one
closure (reproducibility only), and batch/streaming parity drives the **same** reconstructor it is checked
against (a tautology). A shared logic bug in the roll-up or the **net-of-cost fitness** mis-ranks every
genome **identically**, reproduces **byte-for-byte**, is baked into every vintage, and silently corrupts the
return series the **entire DSR/PBO/SPA apparatus certifies** — garbage-in that no downstream statistic can
catch. This is the one Dubno §5.5 principle (a slow-but-correct reference verified against the optimised
path) the repo has **not** yet honoured, and the panel called it a **precondition** for the statistical
validity everything downstream assumes (debate: the more powerful the search, the more it needs an oracle it
cannot game).

**Scope / requirements.**
- Write a deliberately naive, **independent** reference for:
  - **(a)** the multi-resolution bar roll-up (`crates/wfo`/`qe-signal reconstruct`): recompute each coarse
    bar by a fresh `O(n)` window scan;
  - **(b)** the wfo cost-ledger + **net-of-cost geometric fitness** (`crates/wfo/src/backtest.rs`,
    `friction.rs`): recompute with a dead-simple trade-by-trade loop.
- **Property-test** `optimised == reference` over **randomised inputs and seeds** — covering the space the
  search actually roams, not just the fixed goldens.
- References live in **test/dev only** — no production or hot-path cost.

**Out of scope.** A second implementation of the live edge/venue execution path (a separate parity concern);
replacing the existing determinism harness or batch/streaming parity tests (this **complements** them).

**Acceptance criteria.**
- Property tests assert equivalence (byte-exact, or a documented f64 tolerance) of optimised vs reference for
  **both** the reconstruct roll-up and the net-of-cost fitness, over a meaningful number of **seeded**
  randomised cases per run.
- A **mutation guard**: a deliberately injected bug in the optimised path is **caught** by the reference
  (proves the oracle is non-vacuous).
- Determinism harness + full suite green.

`Spec ref: maxdama §5.5 (Dubno: "make a backtester that is slow but works, then verify the optimised version matches it exactly").`

---

<a id="qe-433"></a>
## QE-433 — Portfolio-level fractional (≤½) empirical-Kelly sizing pass on combined net PnL

`Phase: Review R2 (P2 — before scaling capital)` · `Area: ensemble weights / hedger netting / vintage artefact` · `Depends on: QE-113, QE-126, QE-215, QE-219, QE-431` · `Panel: rec #4 (majority)`

**Why.** The pipeline does §6.3 **step 1** (mask → 1/N → capacity cap) but never **step 2**: no growth-optimal
`f*` is solved on the **combined** net PnL. Per-strategy `size_bps` is Kelly-fit standalone and **summed**
under weights, but **Kelly is non-additive under correlation** — and crypto's dominant BTC-beta means
positively-correlated members **over-allocate the shared directional bet**. The only guard on the total is the
pretrade leverage **cap** (`crates/hedger/src/pretrade.rs`) — a backstop, not a chosen size. A **fractional**
(≤½) empirical Kelly on the realised combined series corrects the **relative** allocation and typically
**cuts** size on fat left tails (§6.4). Because it reads the **realised joint path**, it estimates **no
covariance**, so it is correlation-robust by construction — it also sidesteps QE-430's estimation problem.
(Debate: the risk-purist "stacked guards already deliver de-facto Kelly / adds fragility" objection was
resolved — the fix is fractional, clamped **below** the existing cap, and can **cut** as readily as raise
leverage, so it is a correctness fix in either direction, not a leverage grab.)

**Scope / requirements.**
- After the mask + capacity weights are fixed, solve `f* = argmax mean ln(1 + f·r_combined)` on the realised
  **combined net-of-cost** series (reuse `crates/wfo` `log_growth`); apply a fractional multiplier
  `κ ∈ [0.3, 0.5]`.
- Deploy as an **advisory sizer** that scales between `0` and the **existing pretrade cap** — **never above
  it** (the hard cap remains the backstop).
- Recompute **per-vintage**; ride it in the artefact like `CalibrationProfile` (QE-116/QE-129/QE-219);
  consume in `crates/hedger` `live_netter`.
- Solve against the **calibrated** cost/impact model (QE-431) so `f*` is not solved on stale-cost returns —
  hence the QE-431 dependency.

**Out of scope.** Replacing the hard leverage cap (kept as the backstop); intraday/execution-level sizing; any
per-strategy sizing change (this is a **portfolio-level** pass).

**Acceptance criteria.**
- A test shows the sizer can **cut** leverage (fractional Kelly on a fat-left-tail combined series comes out
  **below** the naive summed size) and **never exceeds** the pretrade cap.
- A test shows two **positively-correlated** members are **down-weighted** vs summing standalone Kellys
  (relative-allocation correctness under correlation).
- The pass is **per-vintage**, content-addressed, and reproducible; determinism harness green.

`Spec ref: maxdama §6.3 (portfolio-as-one-strategy Kelly) + §6.4 (empirical Kelly for fat/skewed tails) + §6.5 (half-Kelly robustness).`
