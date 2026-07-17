# Review R2 — Max Dama panel: ticket specs (QE-430..QE-433)

> **Provenance.** A six-discipline panel — two senior quant researchers, two mathematicians, a senior
> software engineer, and a trading expert — compared *Max Dama on Automated Trading* against the engine
> on 2026-07-16 (independent analysis → debate → chair synthesis). The full report (area-by-area
> verdicts, the sharpest debates, and all 20 ranked recommendations) is
> [`docs/reviews/maxdama-vs-quant-engine.html`](./maxdama-vs-quant-engine.html).
>
> This document is the **spec of record** for **all 20** panel recommendations, ticketed into
> [Review R2](../backlog.md#review-r2): the four high-priority items (rank #1–#4) below, then the medium/low
> items (rank #5–#20) under the deferred-triage heading further down. Same QE-4xx review/hardening band conventions as R1: these are
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

---

# Review R2 — medium / low priority (deferred triage)
> These sixteen tickets carry the remaining panel recommendations (rank #5–#20). They are lower-severity
> than QE-430..433 — a mix of *correct-but-improvable* refinements, cheap safety rails that only bite at
> larger scale, and clarity/labelling fixes. Specs are the panel's own reconciled wording; each will get a
> full evidence note at implementation time. Priority tags map the panel priority (high→P1, medium→P2,
> low→P3).

<a id="qe-434"></a>
## QE-434 — Add per-indicator IC / information-horizon screening as a catalogue-admission pre-filter

`Phase: Review R2 (P1 — panel #5, split)` · `Area: signal / validation` · `Depends on: QE-131, QE-107` · `Effort: M`

**Why.** grep-confirmed there is no regression/IC/forward-return diagnostic anywhere; every indicator enters unconditionally and the genome discovers sign/threshold with no prior evidence the factor predicts forward return. Screening gives Dama's table-stakes 'does the signal even work, at what horizon' AND shrinks effective search dimensionality — fewer dead factors to select over directly lowers the cells*gens*windows count DSR must deflate against.

_Debate / dissent:_ Defenders argue evolutionary band-selection is a legitimate non-parametric substitute for explicit IC (handles sign-flip and thresholding), making this a nice-to-have diagnostic rather than a correctness defect. SQR#1 holds it a real gap with the highest single-repo payoff. Panel reports the split honestly — adopt as high-value, acknowledge severity is contested.

**Scope / requirements.** Compute rank-IC and IC-by-horizon for each catalogue indicator against forward net returns, out-of-fold, on the training/CV span; drop or flag zero-IC factors before they enter the search.

**Repo change.** `crates/signal (catalogue admission / reporting layer) or crates/validation.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §4.8 'Regression' (IC, sign-flip, unit conversion) and §4.10 checklist 'information horizon'.`

<a id="qe-435"></a>
## QE-435 — Close the train-backtest <-> live execution/money-model parity gap

`Phase: Review R2 (P2 — panel #6, unanimous)` · `Area: wfo / hedger / edge` · `Depends on: QE-120, QE-219` · `Effort: L`

**Why.** Archive selection rides on net-of-friction log_growth, so the MONEY model — not just the decision — must match live. Today only Genome::decide/PositionState::advance are shared; fills/costs/sizing are two independent implementations. A genome tuned to the wfo linear-slippage ledger can be selected and sized on fills it never gets live. Same defect class as the equal-weight-scored / capacity-weight-deployed inconsistency.

_Debate / dissent:_ 'Immaterial at 1h frequency' rejected as the same assume-it-away move the deployed-weight gap makes; fix by proving the optimized object equals the deployed one, not by betting the divergence is small.

**Scope / requirements.** Route the wfo backtest's fills through the same VenueSimulator/plan_delta the live edge uses, OR add an oracle test asserting wfo friction cost for (side, qty, mark, spread) equals the VenueSimulator fill for identical inputs. Fold in the §7.7 calibration vintage so both price the same money.

**Repo change.** `crates/wfo/src/backtest.rs vs crates/hedger + crates/edge (VenueSimulator).`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §5.5 'backtesting on recorded data should produce the same results as the live run.'`

<a id="qe-436"></a>
## QE-436 — Add an in-search parsimony (MDL) penalty and decouple size discovery from rule discovery

`Phase: Review R2 (P2 — panel #7, majority)` · `Area: wfo / signal` · `Depends on: QE-113, QE-114, QE-110, QE-107` · `Effort: S`

**Why.** Nothing in fitness.rs, lifecycle.rs, or regularise.rs (which is behavioral novelty, not complexity) rewards fewer clauses/features, so a 1-clause and a 4-clause genome compete on fitness alone and search drifts to maximal complexity over correlated indicators. Because size_bps co-evolves under the same log_growth scalar, the illumination cannot distinguish real edge from tolerable edge levered up — operationalizing Dama's 'fewer parameters' INSIDE the search closes both.

_Debate / dissent:_ Minority holds downstream DSR/PBO already punish over-complex genomes; majority wants parsimony operationalized in-search rather than only caught post-hoc.

**Scope / requirements.** Subtract a small cost per enabled clause / distinct referenced feature in the selection fitness or the lifecycle lower-bound (tie-break toward parsimony at equal robust fitness); and either two-stage the search (discover rules under a size-normalized/unit-risk fitness, THEN solve f*) or add an alpha-quality term so leverage cannot substitute for edge inside a niche.

**Repo change.** `crates/wfo/src/fitness.rs, crates/wfo lifecycle.rs, crates/signal genome.rs.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §5.4 'better to have fewer parameters than many' + §4.9 Voodoo Spectrum (variable selection at the dangerous end).`

<a id="qe-437"></a>
## QE-437 — Gate G1 on the already-computed PBO

`Phase: Review R2 (P2 — panel #8, unanimous)` · `Area: gate` · `Depends on: QE-134` · `Effort: S`

**Why.** pbo.rs is a faithful CSCV and RobustnessReport.pbo is recorded, but evaluate_g1 gates only on holdout Sharpe, DSR, SPA p-value and OOS-tolerance — never reads pbo. The most direct backtest-overfitting probability the book's §5.4 concern maps to is decorative. Zero new math; the value already exists.

_Debate / dissent:_ Noted mild redundancy with DSR/holdout, not a reason to leave a computed overfit probability unused.

**Scope / requirements.** Add a pre-registered criterion pbo < 0.5 to G1Criteria and evaluate_g1.

**Repo change.** `crates/gate/src/lib.rs (G1Criteria, evaluate_g1).`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §5.4 (overfitting is the central risk; PBO is its most direct probability).`

<a id="qe-438"></a>
## QE-438 — Score the DE membership objective on the actual deployed (capacity-capped) weight vector, not equal-weight

`Phase: Review R2 (P2 — panel #9, majority)` · `Area: ensemble` · `Depends on: QE-115, QE-130` · `Effort: S`

**Why.** Membership is optimized on equal-weight combined_returns but deployed under capacity-capped non-1/N weights; when caps bind (a turnover-2, 0.1%-edge member caps ~$100k, a 5x haircut at $1M) the selected set is no longer provably optimal for the book that runs. Whether it bites is itself a function of the guessed impact coefficient — fixing rec #2 makes this answerable. Same 'optimize-X-deploy-Y' class as the execution-parity gap.

_Debate / dissent:_ 'Moot if caps rarely bind at target AUM' — but the cheap fix removes the need to argue how often caps bind, and Trading confirmed binding is real for high-turnover members.

**Scope / requirements.** Thread the deployed weight vector (capacity-capped, and inverse-vol if adopted) into combined_returns during scoring, reusing the weighted_combined already present in stress.rs.

**Repo change.** `crates/ensemble/src/objective.rs (combined_returns), reuse stress.rs weighted_combined.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §6.3 (select and deploy the same weighted object).`

<a id="qe-439"></a>
## QE-439 — Make the DSR deflation basis coherent on the gens/windows axis

`Phase: Review R2 (P2 — panel #10, majority)` · `Area: validation` · `Depends on: QE-131, QE-134` · `Effort: M`

**Why.** QE-414 fixed cell-axis coherence (V from full occupied-cell champions), but N still multiplies by generations*windows — serial mutations of one persistent elite and re-evaluations of one strategy are not independent trials. Currently errs conservative (over-deflates, false-reject), so low urgency, but the count is not a defensible independent-trials number.

_Debate / dissent:_ Math#2 explicitly downgraded round-1 'incoherent basis' to 'conservative on gens/windows' after verifying QE-414 — this is a tightening, not a live risk. SQR#1 co-signs the niche-count basis.

**Scope / requirements.** Derive an effective independent-trials count from the trial autocorrelation structure (bounded by the number of occupied/behaviorally-distinct niches, since Deep-Grid deliberately correlates within a cell) rather than the raw cells*gens*windows product; or use the BdP analytic per-trial Sharpe variance so N and V share one population on every axis.

**Repo change.** `crates/validation/src/dsr.rs (effective_trials), cli/src/jobs/train.rs:429.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §5.4 / §6.2 (few observations => optimistic selection); Bailey-Lopez de Prado DSR.`

<a id="qe-440"></a>
## QE-440 — Move the impact model to concave square-root-in-participation and add a rolling ADV input

`Phase: Review R2 (P2 — panel #11, majority)` · `Area: wfo / ensemble` · `Depends on: QE-128, QE-109` · `Effort: M`

**Why.** §7.7's single-curve collapse shows impact is concave/square-root; the convex (quadratic-total) form overstates large-order cost (conservative on friction) but UNDER-states capacity, cannot be reconciled with any measured impact, and forces asset-specific per-contract coefficients. The CLI already advertises a 'square-root-impact' contract tag while the engine runs linear — this closes that gap. Pairs naturally with the rec #2 calibration.

_Debate / dissent:_ Math#1 notes for the SIZE coordinate at near-zero participation the mis-shape is second-order and safe-direction, so ranks it below fractional/portfolio-Kelly for sizing payoff; it bites mainly in capacity.

**Scope / requirements.** Replace cost = notional*(half_spread + impact*qty) with impact proportional to (qty/ADV)^beta, beta ~ 0.2-0.5; add a rolling hourly ADV (currently absent — no %ADV anywhere) so the coefficient is dimensionless, asset-portable, and shared between friction and capacity.

**Repo change.** `crates/wfo/src/friction.rs, crates/ensemble/src/capacity.rs; ingest ADV.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §7.1 (depth/impact) and §7.7 (impact = volume^beta, beta<1).`

<a id="qe-441"></a>
## QE-441 — Inject bar-level scenario shocks into the single-strategy sizing fitness (tail-aware Kelly), with seeded/content-addressed shocks

`Phase: Review R2 (P2 — panel #12, majority)` · `Area: wfo / ensemble / determinism` · `Depends on: QE-130, QE-120, QE-006` · `Effort: S`

**Why.** The single-strategy log_growth that sets size_bps sees only the raw historical net path, fitting leverage to crypto's empirically thin crash sample; the ensemble stress overlay exists but never reaches the size-setting fitness. Bar-level injection lets a larger size produce a larger drawdown, so log_growth self-selects a lower, tail-aware leverage — Dama's imaginary-PnL-before-optimizing-f done correctly.

_Debate / dissent:_ Math#2 warns the shock severity/frequency become un-deflated researcher DOF — mandatory mitigation: freeze/pre-register the shock set per vintage. SSE adds it MUST be seeded or it voids byte-reproducibility.

**Scope / requirements.** Inject bounded synthetic gap/funding-spike/ADL shocks at the PRICE/bar level in backtest.rs BEFORE size_frac scales the notional — NOT appended to the post-size return series (which only trips the -inf ruin absorber and rejects every genome uniformly). Draw shocks from the seeded portable RNG and pin the shock set as content-addressed.

**Repo change.** `crates/wfo/src/backtest.rs (price path), reuse crates/ensemble stress.rs shocks; crates/determinism/rng.rs seeding.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §6.1 (add imaginary/Black-Swan PnLs in proportion to frequency before optimizing f) + §6.4 (heavy left tail pulls Kelly down).`

<a id="qe-442"></a>
## QE-442 — Make signal combination graded (probability surface) rather than hard-boolean k-of-n

`Phase: Review R2 (P2 — panel #13, majority)` · `Area: signal` · `Depends on: QE-110, QE-107` · `Effort: L`

**Why.** The ordinal strength the quantiser already computes is discarded at the decision boundary — two genomes fire identically whether a feature barely clears its band or sits deep in it. Dubno's dictum is 'signals should be probability surfaces in price and time.' Graded conviction reduces band-edge sensitivity (an overfitting surface of its own) and carries information into sizing.

_Debate / dissent:_ Real but medium payoff at 1h cadence; ranked below the IC and correlation/DSR fixes.

**Scope / requirements.** Use the quantiser's ordinal QState (distance into band, or count of satisfied clauses) as a graded conviction feeding entry strength and/or size_bps, instead of collapsing it to a bool in RuleSet::fires.

**Repo change.** `crates/signal genome.rs (RuleSet::fires -> graded), entry/size path.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §5.5 Dubno ('probability surfaces') + §4.8 (signals as continuous strengths).`

<a id="qe-443"></a>
## QE-443 — Seed member weights with inverse-vol (EWMA) risk parity, then apply the existing capacity water-fill

`Phase: Review R2 (P2 — panel #14, split)` · `Area: hedger / ensemble` · `Depends on: QE-219` · `Effort: M`

**Why.** Pure 1/N ignores §6.2's own argument that unequal weights cancel risk better when volatilities differ; capacity caps for turnover, not volatility, so a high-vol member dominates the combined tail at 1/N. A strong shared BTC-beta also concentrates members' participation into the same bars, so per-strategy capacity under-counts aggregate impact — a one-factor view helps correct aggregate impact accounting.

_Debate / dissent:_ Genuine trade-off, not a clear win: §6.2 method 9 endorses 1/N as OOS-robust and inverse-vol reintroduces an estimated variance. Panel: seed only, medium priority, do NOT present as strictly superior; full RMT/Barra correctly omitted as dimensionality-mismatched.

**Scope / requirements.** Seed weights with inverse-vol using a single EWMA variance decay constant (low-parameter, deterministic), then let capacity cap_weights layer on top; optionally add a single-factor BTC-beta neutralization.

**Repo change.** `crates/hedger/src/bootstrap.rs, evaluator.rs; crates/ensemble weighting.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §6.2 two-strategy MV (unequal risk => unequal weights) + method 10 (variance is predictable via short EWMA).`

<a id="qe-444"></a>
## QE-444 — Add a decision-to-fill alpha-loss (implementation-shortfall) term to friction

`Phase: Review R2 (P2 — panel #15, majority)` · `Area: wfo` · `Depends on: QE-109` · `Effort: M`

**Why.** The backtest fills the whole delta at next-bar open with only a static half_spread; if the signal's own edge leaks into the close->open gap, net returns are optimistically biased. This is the one piece of §7.3 that applies at 1h without any TWAP/VWAP machinery, and it is the natural home for the execution-parity fix.

_Debate / dissent:_ Trading expert notes generally conservative if omitted; medium not high.

**Scope / requirements.** Measure realized bar-close (decision) vs next-bar-open (fill) directional drift on the live/shadow path and fold it into friction as an explicit directional slippage term (not just symmetric half-spread), routed through the shared calibration vintage.

**Repo change.** `crates/wfo/src/friction.rs, live/shadow measurement path.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §7.3 Alpha Loss (profit absorbed between signal and fill).`

<a id="qe-445"></a>
## QE-445 — Extend re-run-twice into a permuted-parallelism determinism test

`Phase: Review R2 (P2 — panel #16, unanimous)` · `Area: determinism` · `Depends on: QE-006` · `Effort: S`

**Why.** reproduce() re-runs under the same conditions and never varies thread count, so it does not actively exercise the scheduling-independence property task_rng is designed for. This turns a design intent into an enforced invariant and guards the exact property that lets the powerful search be trusted.

_Debate / dissent:_ None; SSE-originated, uncontested.

**Scope / requirements.** Run a stochastic stage (MAP-Elites generation or the DE ensemble search) twice with DIFFERENT rayon thread-pool sizes and assert byte-identical artifacts.

**Repo change.** `crates/determinism harness.rs.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §5.5 (control 100% of state for reproducibility).`

<a id="qe-446"></a>
## QE-446 — Report strategy-level max/avg drawdown at the lifecycle graduation gate

`Phase: Review R2 (P3 — panel #17, unanimous)` · `Area: wfo` · `Depends on: QE-134, QE-114` · `Effort: S`

**Why.** log_growth penalizes terminal ruin but is indifferent to intermediate max-drawdown at a fixed size, and lifecycle.rs graduates on the log-growth lower bound with no drawdown term — a high-growth/deep-drawdown genome can graduate on growth alone before the ensemble's CVaR/CDaR ever sees it.

_Debate / dissent:_ Largely mitigated by the ensemble tail objective; residual risk only for a single graduated strategy — low priority by agreement.

**Scope / requirements.** Attach a max-drawdown (or CDaR) statistic to the per-strategy record and add an optional drawdown ceiling to the graduation gate.

**Repo change.** `crates/wfo lifecycle.rs (QualityGate).`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §5.5 Dubno ('maximum and average drawdown are better objectives than Sharpe').`

<a id="qe-447"></a>
## QE-447 — Add a pre-trade %ADV participation guard

`Phase: Review R2 (P3 — panel #18, unanimous)` · `Area: hedger` · `Depends on: QE-215, QE-219` · `Effort: S`

**Why.** There is no ADV concept anywhere and no participation guardrail; at current AUM participation is ~0 so this is latent, but it only bites when AUM grows or liquidity thins in a stress regime — exactly when it matters. Cheap safety rail that also sanity-checks capacity.

_Debate / dissent:_ Latent, not a current defect.

**Scope / requirements.** Compute rolling hourly ADV and reject/flag any delta-close order exceeding a configured %ADV in pretrade.

**Repo change.** `crates/hedger pretrade.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §7 intro (participation = %ADV; 1% is already high).`

<a id="qe-448"></a>
## QE-448 — Correct the SPA label and document the biases deflation cannot see

`Phase: Review R2 (P3 — panel #19, unanimous)` · `Area: validation` · `Depends on: QE-131` · `Effort: S`

**Why.** The module claims 'Hansen's SPA' while omitting Hansen's defining contribution, so it is conservative but under-powered and mislabeled. DSR is absolute (vs noise ceiling), so any systematic cost/adverse-selection/survivorship bias flows through undeflated — worth documenting as a known boundary.

_Debate / dissent:_ Safe direction; purely a power/clarity fix by agreement.

**Scope / requirements.** Relabel spa.rs as White's Reality Check (it recenters all k models = SPA-lower), or implement Hansen's model-omission recentring (sqrt(2 log log T)) to recover power; separately assert point-in-time / survivorship-safe universe provenance in the vintage lineage SHA and document that DSR/PBO/SPA correct for SELECTION, not per-trade optimistic bias.

**Repo change.** `crates/validation/src/spa.rs; vintage lineage docs.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: Hansen 2005 SPA; §5.3 Survivorship/Adverse Selection/Transaction Costs.`

<a id="qe-449"></a>
## QE-449 — Guard the unused maker rate against future adverse-selection blindness

`Phase: Review R2 (P3 — panel #20, unanimous)` · `Area: cross-cutting` · `Depends on: QE-109` · `Effort: S`

**Why.** The engine is a confirmed pure taker (no post_only/OrderType in edge or hedger), so adverse selection is a non-issue today — but the 2bp maker rate sitting in FeeSchedule reads as a free rebate. A future maker-rebate optimization would look profitable in backtest while losing to adverse selection live (a classic §7.6 trap), and the inflated Sharpe would pass DSR undeflated.

_Debate / dissent:_ Latent trap to guard, not a current defect — Math#2 and Trading agree.

**Scope / requirements.** Document that the FeeSchedule maker rate must not be used without a paired adverse-selection markout; if post-only orders are ever added to harvest the maker/taker gap, model an expected fill-conditional adverse drift alongside.

**Repo change.** `FeeSchedule docs; friction.rs if post-only added.`

**Acceptance criteria.**
- The behaviour described in *Scope* is implemented and covered by focused unit/property tests.
- If any golden/vintage output moves, it is regenerated **via real code** (never hand-edited) and the `content_hash` change is tracked.
- Full green gate (fmt / clippy `-D warnings` / test / deny) + determinism harness pass on the exact commit.

`Spec ref: §7.6 (maker fills carry adverse selection that outweighs the collected spread).`
