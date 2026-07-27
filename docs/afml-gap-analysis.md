# Advances in Financial Machine Learning (López de Prado) vs. quant-engine — Gap Analysis

*Four-expert panel review — senior software engineer, senior quant developer, expert trader, mathematician. Book: M. López de Prado, "Advances in Financial Machine Learning" (2018). Repo: `quant-engine` (Rust workspace, Binance USDT-M perpetual futures).*

---

## Bottom line

The repo is **not** a López de Prado ML-classifier pipeline, and it should not try to become one. It is a **genetic-programming / MAP-Elites + differential-evolution search over rule-based strategies**, wrapped in an overfitting-control harness that implements the book's *statistical core* — purged+embargoed CV, CSCV/PBO, Deflated Sharpe — **more rigorously than the book's own reference code**, and adds machinery the book never covers (Hansen SPA, BH-FDR IC screens, thread-count-deterministic parallelism, a numerically-stable large-N deflation path that fixes an actual bug in the textbook formula).

So the interesting gaps are **not** "you're missing chapters." They are three concrete things, in priority order the panel converged on:

1. **The *reported* headline Sharpe is inflated and un-deflated** — a nearly-free fix, and the one with the clearest immediate payoff.
2. **The out-of-sample verdict rides on a single path** — the exact failure mode Ch. 12 was written to kill; the combinatorial machinery to fix it already exists in the codebase.
3. **The engine samples on a wall-clock (time-bar) clock** — the book's single most-repeated "don't", and the case is *stronger* for crypto than for the equities the book targets — but it is the largest, most invasive change and its payoff is the most contested.

Everything else (triple-barrier ML labeling, meta-labeling, fractional differentiation, HRP, the HPC chapters, the ETF trick) is either premature for this architecture or actively wrong for crypto perps, and is documented below under **Skip/Differ**.

---

## Where all four experts agree

- **The repo already out-implements the book on overfitting control.** DSR handles the large-N degenerate regime via a log-space path (`crates/validation/src/dsr.rs:104-139`) that the textbook `E[max SR]` formula (book p. 204) does not — the book's formula silently degenerates to `+∞` (DSR≡0 for everyone) at genetic-programming-scale trial counts. PBO is gated on the **uncensored** population including rejects (`crates/wfo/src/gp/deflation.rs:26-36`), and Hansen's SPA test (`crates/validation/src/spa.rs`) is layered on top. None of this is in the book's snippets.
- **The vintage leaderboard is not a multiple-testing trap.** The obvious suspicion — "comparing many variants and picking winners is p-hacking" — is explicitly defused: selection happens once at the GP/ensemble gate under DSR/PBO/SPA/FDR; the leaderboard only ranks *already-sealed* vintages and is documented as "informational, NOT a selector" (`crates/server/tests/read.rs:491`).
- **The book's ML labeling/feature-importance stack (Ch. 3–4, 8) is largely N/A here.** Triple-barrier and meta-labeling presuppose a supervised classifier emitting a label stream; this engine evolves rule genomes and sizes with graded k-of-n conviction × fractional Kelly. Bolting on an sklearn-style meta-classifier would import a whole training pipeline for marginal gain.
- **Annualization is already correct for 24/7 crypto** — `ppy = 365·24·60 / resolution.minutes()` (`crates/cli/src/jobs/backtest.rs:366-368`), i.e. 365 continuous days, not the equity-market 252.
- **Skip HRP, fractional differentiation, the ETF trick, and the HPC chapters (20/22)** — see Skip/Differ.

---

## The debate — and how it resolves

Every panelist nominated a *different* "single biggest gap." That disagreement is real and worth settling, because it's a disagreement about **what to do first**, not about facts.

### Contest 1 — What is priority #1?

| Panelist | Nominated #1 | Effort | Argument |
|---|---|---|---|
| Mathematician | Fix the reported Sharpe (surface PSR, aggregate to daily, Lo autocorrelation haircut) | **S** | DSR works in *per-period* units on the *selection* path and never touches the *reported* annualized headline. At 5-min bars `√ppy ≈ 324`; on autocorrelated non-IID crypto returns that over-scales the number a human actually reads. |
| Quant dev | CPCV as the OOS evidence (not just PBO measurement) | **M** | The promotion evidence is one walk-forward + one terminal holdout. Ch. 12's central argument is that a single path, even purged, is not enough. The combinatorial splitting + purging primitives already exist (`pbo.rs:117`, `cv.rs`). |
| SWE | CPCV (ranks it *above* the dollar-bar rebuild on risk-adjusted ROI) | **M** | Time bars bias inference, but shipping strategies selected by single-path walk-forward without a distribution of OOS Sharpe is the precise overfitting trap Part 3 exists to prevent. |
| Trader | Dollar/volume bars + fix the cost calibration | **M/L** | Time bars are the biggest *realism* gap and BTC's ~100× price range makes the dollar-bar case compound; separately, the elegant √-law slippage model is running on *prior* constants, not fitted data. |

**Moderator verdict.** These are not really in conflict — they are three tiers of a single roadmap, and the panel's own effort estimates rank them cleanly:

1. **Fix the reported Sharpe first (S).** The mathematician wins the "do this immediately" slot on value-per-effort. The math already exists (`probabilistic_sharpe_ratio`, `dsr.rs:39`); the fix is to aggregate per-bar net returns to daily before annualizing (or annualize daily-Sharpe by `√365`), surface PSR next to every headline SR, and apply a Lo (2002) autocorrelation haircut `√ppy / √(1 + 2·Σρ_k)`. This corrects the single most misleading number in the whole system and costs almost nothing. **Two of four panelists (math, and the "everyone √t-scales" objection the quant raised) sparred over whether DSR downstream already saves it — it does not, because DSR never touches the reported headline.** Verdict: uncontested once that distinction is made.

2. **CPCV-distributed OOS evidence second (M).** SWE and quant *independently* nominated this, which is the strongest signal in the whole review. Reuse the existing combinatorial block splitting and purging to emit a *distribution* of held-out Sharpe/DSR for a promoted genome, turning the single terminal-holdout point estimate (`crates/wfo/src/cv_fitness.rs:34-36`) into a confidence interval. The hard part — leak-free purging — is already done and tested.

3. **Dollar/volume bars third (M/L).** Highest realism payoff but most invasive and most contested. Do it *after* the measurement layer is trustworthy, so you can actually tell whether it helped.

### Contest 2 — Are dollar bars worth the pipeline rebuild?

The trader and SWE defend "yes"; they anticipated the quant/SWE-integration pushback that dollar bars break (a) the clean 8h funding alignment, (b) the order-preserving LMDB key scheme (`crates/storage/src/key.rs:47` hardcodes a resolution byte), and (c) the purged-CV horizon arithmetic, which is expressed in bar counts.

**Moderator verdict — adopt, but scoped and sequenced.** The integration costs are real but each has a known answer: **accrue funding on the wall-clock stamp regardless of bar clock** (funding is a real 8h cashflow, not a bar event), **widen the storage key's resolution tag to encode `{time|dollar|volume}` + size** so activity bars coexist with klines without collision, and **re-express `label_horizon` in the new bar units**. One dissent worth recording: the case is genuinely *weaker* than the book's equities example in one respect — crypto trades ~24/7, so it lacks the session-open/close activity clustering that motivates dollar bars for the E-mini. The counter that carried the panel: the engine's own `(qty/ADV)^β` slippage term and ADV-based friction *prove* activity varies wildly, which means time bars are silently mis-weighting the very impact term the backtest depends on. Net: adopt, but it's tier 3, and validate it against the now-trustworthy Sharpe rather than assuming the book is right.

### Contest 3 — Is the IID assumption quietly violated?

The mathematician and quant both flag that CSCV/PBO and PSR/DSR assume roughly-IID Sharpe slices (book p. 182), while overlapping label outcomes and the persistence of MAP-Elites elites across generations make the return slices autocorrelated and `effective_trials` an over-count. The anticipated pushback — `label_horizon = 1` (next-bar fill) makes overlap negligible — is partly right.

**Moderator verdict.** Cheap hardening, not a crisis. Compute the **average uniqueness** of the aggregated return series and either assert the low-overlap regime explicitly or deflate the effective sample size (`T → T·ū`) in PSR/DSR. This converts an *implicit, undocumented* assumption into a *checked* one. The mathematician's sharper point stands and is worth recording: the over-counted `N` is "safe" only in the Type-I direction — multiplying serially-correlated `generations·windows` factors also **false-rejects genuine edge**, which in a low-base-rate search is real money left on the table. The honest basis is an effective-independent `N` derived from the trial-Sharpe correlation matrix (the repo already built the analogous pooled `T_eff` at `deflation.rs:300-307`), not a conservative product.

### Contest 4 — Funding and cost calibration (trader, uncontested by the others)

The trader raised two crypto-native points nobody else was positioned to see:

- **Funding is the dominant PnL driver on perps, yet the search treats it as a coarse 4-state feature and a passive accrual** (`crates/signal/src/indicator/flow.rs`, `backtest.rs:278-283`). The engine will systematically under-explore funding-carry / basis strategies. This is a *strategy-space* gap, not a bug — worth a deliberate decision about whether funding-carry is in scope.
- **The slippage model is architecturally beautiful but calibration-starved:** `impact_coeff = 0.01`, `β = 0.5`, `half_spread = 1 bp`, `alpha_loss = 0` are seed priors, and the fitter exists but needs live/shadow trade+quote history (`crates/risk/src/slippage.rs:265-333`). A guessed-cheap cost curve is exactly how a backtest inflates a Sharpe that DSR cannot deflate. 1 bp half-spread is optimistic for the thin-alt tail.

**Moderator verdict.** Both accepted. The cost-calibration point reinforces tier 1 (don't trust the reported number until costs are fitted), and it pairs naturally with the liquidity screen already in place (`crates/ingest/src/liquidity.rs`, $2M ADV floor).

---

## Recommendations — ADOPT (unified, ranked across all four panels)

| # | Action | Effort | Book anchor | Why |
|---|---|---|---|---|
| 1 | **Fix the reported Sharpe**: aggregate to daily before annualizing, surface **PSR** beside every headline SR, apply the **Lo (2002)** autocorrelation haircut, and emit the tracked trial-count **N** + DSR/PBO into the human report. | **S** | Ch. 14 (pp. 203–205) | Corrects the most misleading number in the system; the math already exists and only the *reporting surface* needs it. Satisfies the "Third Law" (report N) for the reader. |
| 2 | **CPCV-distributed OOS evidence**: reuse existing combinatorial splitting + purging to replace the single terminal holdout with a *distribution* of held-out Sharpe/DSR. | **M** | Ch. 12.4 (p. 163) | Kills the single-path failure mode; two panelists nominated it independently; the hard part (leak-free purge) is done. |
| 3 | **Strategy-risk / implied-precision failure gate**: from the recorded `TradeFill` round-trips compute `p`, `π±`, `n` and reject vintages with `P[p < p_θ*] > 5%` before deploy. | **S** | Ch. 15 (pp. 214–219) | Cheap, complements DSR/PBO with a *bet-level* failure probability; data is already persisted. |
| 4 | **Uniqueness / concurrency diagnostic** at the DSR/PBO boundary; deflate `T → T·ū`. | **S** | Ch. 4 (p. 61) | Hardens the IID precondition CSCV/PSR silently assume. |
| 5 | **Fit the cost model per-instrument** (impact coeff, β, half-spread) and widen `half_spread` for thin names; make the inert `alpha_loss` term a tracked deliverable. | **M** | Ch. 19 (λ intuition) | A guessed-cheap cost curve inflates Sharpe in exactly the way DSR can't catch. |
| 6 | **Dollar/volume-clock resampling** for the feature+decision series; accrue funding on wall-clock, widen the storage key tag, re-express horizons in bar units. | **M/L** | Ch. 2 (pp. 27–29) | Biggest realism win; case is *stronger* for crypto's price range — but validate against the (now-trustworthy) Sharpe rather than on faith. |
| 7 | **VPIN / signed-volume order-flow-imbalance decision feature** — aggressor side is already in-feed for slippage fitting. | **M** | Ch. 19 (p. 292) | Crypto-native analogue of the imbalance bars being skipped; slots into the existing indicator catalogue. |
| 8 | **Path-dependent exit evaluation** (first-touch vol-scaled PT/SL) *inside the fitness* — **without** turning exits into ML labels and **without** putting stop placement in the search genome. | **M** | Ch. 3.4 (pp. 45–47) | Borrows the one triple-barrier idea that maps onto a rule engine while preserving the repo's deliberate anti-overfit choice to keep hard stops in the risk layer (`genome.rs:157-158`). |
| 9 | **SADF explosiveness detector** as a regime/kill-gate feature. | **M** | Ch. 17 (pp. 249–260) | Of the missing book methods, clearest crypto payoff (bubble/blowoff detection); composes with the existing kill gate (`crates/edge/src/kill_gate.rs`). |
| 10 | **HHI return-concentration** `h+/h−/h[t]` metrics. | **S** | Ch. 14 (p. 200) | Catches "one lucky bet" strategies Sharpe and DSR can miss; fits the existing `metrics.rs` pattern. |

---

## Recommendations — SKIP / DIFFER (and why)

- **Full meta-labeling as a secondary ML classifier (Ch. 3.6)** — presupposes a supervised primary model this architecture doesn't have; size is already handled by graded conviction × Kelly. Revisit only if an ML side-classifier is ever introduced.
- **Sequential bootstrap for bagging (Ch. 4.5)** — its purpose is to make *bagging classifiers* draw near-IID samples; there is no bagged-tree learner here. Adopt the uniqueness *weight* (rec #4), not the resampler.
- **Fractional differentiation (Ch. 5)** — features are quantile states, stationary by construction; its practical OOS value is thin in independent replications and the memory it preserves is often un-tradeable after costs.
- **MDI feature importance (Ch. 8.3.1)** — tree-impurity-specific and in-sample-biased; meaningless for a rule-genome search. If feature importance is wanted, use permutation **MDA / single-feature SFI** over the indicator catalogue (a defensible optional add, synergistic with the existing `ic.rs`, that would also shrink the search space and lower the trial count).
- **HRP (Ch. 16)** — its OOS superiority is disputed post-2018; the book *itself* cites DeMiguel et al. (2009) that naïve 1/N beats optimizers OOS (p. 223). The repo's correlation-penalized, capacity-capped, near-equal-weight ensemble (`crates/ensemble/src/regime.rs`, `objective.rs:342-353`) captures HRP's real benefit (avoid concentrated correlated bets) without HRP's disputed clustering machinery or its silence on expected returns. Adding HRP would be sophistication for its own sake.
- **Entropy price-features (Ch. 18)** — plug-in / Lempel–Ziv estimators are sample-hungry and noisy at available bar counts; marginal signal for the complexity.
- **ETF trick / single-future roll (Ch. 2)** — perps don't expire: no roll gaps, no basket convergence. Dead weight for cash-like perps.
- **Tick rule + Roll effective-spread (Ch. 19, pp. 282–283)** — aggressor side and live top-of-book quotes are in the feed; the repo reads `half_spread` off observed quotes rather than inferring it from return serial-covariance. Correct call already made.
- **Corwin–Schultz high-low spread (Ch. 19, p. 284)** — built for order-book-less illiquid bonds; crypto has continuous quotes. Marginal at best.
- **HPC chapters (Ch. 20 mechanics, Ch. 22: HDF5, MPI, Slurm, "partner with a National Laboratory")** — these are Python-GIL workarounds and 2018 supercomputer-lab artifacts. Rust `rayon` on one box already realizes the book's atom/molecule *intent* and improves on it with byte-identical output across thread counts (`crates/determinism/tests/permuted_parallelism.rs`) — a reproducibility property the book never addresses. LMDB + Arrow IPC beats HDF5 for an embedded single-node reproducible engine.
- **Open/close auction & TWAP-footprint features (Ch. 2 p. 27, Ch. 19 p. 294)** — 24/7 markets have no session open/close. Replace with **funding-time seasonality** (00/08/16 UTC volume/OI/basis patterns) — the perp-native structural clock the book has no concept of.

---

## One-line characterization

The book is a checklist for building a supervised-ML equities/futures shop from scratch. This repo made a different, coherent bet — evolutionary search over rule strategies on net-of-cost returns — and independently built the parts of the book that *transfer* (CV purging, CSCV/PBO, DSR) more carefully than the book's own code. The real work is not adding chapters; it's making the **reported** statistics as honest as the **selection** statistics already are, giving the OOS verdict a distribution instead of a point, and — only then — testing whether a dollar-bar clock actually earns its integration cost.
