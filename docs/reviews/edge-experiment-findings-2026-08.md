# Edge experiment — consolidated findings (2026-07-31 → 2026-08-02)

> The record of a systematic search for a deployable trading edge with the quant-engine, on real Binance data, and the
> engineering it drove. **Headline: across every lever tried — data quantity, factor breadth, search budget, and
> strategy representation — the engine's honesty machinery reports no deflation-surviving, out-of-sample-persistent edge
> in the space it can express on liquid BTC/ETH perps (2020–2025). That is a real, valuable answer, and the fact that the
> G1 gate *hardens* under every lever rather than being gameable by any is the strongest evidence the engine is
> trustworthy.**

## 1. Objective & method

**Goal.** Produce the best strategies/vintages the engine can, and specifically find a vintage that clears the **G1
gate** — the engine's promotion bar. **Constraint held throughout:** the gate was never weakened. No threshold,
`funding_coverage_min`, `cv_folds`, or gate/validation constant was touched; a G1 *fail* was always treated as a
legitimate result, never something to tune away.

**The bar (G1, 7 conjoined criteria).** holdout-samples ≥ 30 · net-of-cost edge persists · **DSR > 0.95** (deflated
Sharpe, discounted by the search's own trial count) · **SPA p < 0.05** (White's Reality-Check) · **PBO < 0.5** (CSCV
overfit probability) · holdout tracks in-sample · **CPCV OOS distribution clears the DSR floor**. It requires a
genuinely strong, *robust*, *out-of-sample-persistent* edge — exactly what defeats data-snooping.

**Market & data.** BTCUSDT / ETHUSDT USDT-M perpetual futures, Binance (`fapi.binance.com`), OHLCV 1h + 4h, funding,
and (after QE-497) premium-index, 2020-01 → 2025-06; point-in-time universe (real listing dates, no survivorship).
Paper/offline, uncalibrated execution ("NOT tradable-at-size"), fixed-point money, deterministic (seeded).

## 2. Results by lever

### 2a. Baseline hunt — 81 runs, 0 passes
A 5-quant fleet swept seeds, train windows, search budgets, WFO/split geometry, and a free-rein champion lane on BTC/ETH
1h. **Zero G1 promotions.** Best DSR 0.65 (vs the 0.95 floor); every SPA p ≥ 0.87; **`holdout_sharpe = 0.0` on every
run** — the in-sample edge never survived into the held-out tail. Crucially, the gate *held under a 4× budget sweep*
(wider search → *more* deflation, not more passes). The only genuine signal found: a **short-side effect in the 2022
bear** (BTC 4h) that beats the SPA null at **p = 0.014** — real, but it still fails DSR/PBO/OOS.

### 2b. Diagnosis — the binding constraint is representation, not data or search
Grounded in the code + the run data: **data quantity is fine** (48k 1h bars/symbol); **the search is large, and that is
counter-productive** (DSR deflates against `n_trials`, which the biggest searches drove to 10⁵–10⁶ → DSR ≈ 0); the real
constraints are **factor breadth** (klines-only — the funding/premium/OI flow indicators were dead) and **strategy
representation** (a fixed *k*-of-4 threshold grammar over ~18 classic TA indicators — the most-arbitraged signal class on
liquid majors).

### 2c. Factor experiment — premium made live (QE-497), 31-run re-hunt, 0 passes
Wiring premium ingest revived the `premium_state` factor over full history. Result: premium **inflated in-sample DSR
without out-of-sample edge** — one premium-steered run hit DSR **0.996** (clearing the 0.95 bar) while PBO collapsed to
fail and OOS stayed zero; the gate correctly rejected it. Premium moved individual diagnostics (it even *improved* PBO on
the bear window, 0.93 → 0.29) but never all-together, and `holdout_sharpe` stayed 0 on 27/31 runs. (Open-interest was
wired too but Binance's `openInterestHist` retains only ~31 days, so `oi_roc_10` is dead for historical training — a
documented venue limit, not a code defect.)

### 2d. Representation experiment — QE-499 GP→vintage bridge, Phase C, 0 passes
Built the bridge letting evolved GP `Expr` formulas reach the **unmodified** G1 gate as injected catalogue features, with
an honest two-stage deflation composition. Ran `evolve → pool → train --pool` end-to-end (BTC 4h):

| Run | n_trials | DSR | G1 |
|---|---|---|---|
| price-only baseline (same train window) | 4,020 | 0.0075 | fail |
| **+ 8 evolved formulas** | **117,723** | **0.000053** | fail |
| evolved #2 (bear-evolved pool → disjoint train) | 124,899 | 0.000000 | fail |

Injecting 8 evolved formulas raised the multiple-testing basis ~29× and crushed DSR ~140×. **A richer representation
behaves exactly like richer factors and a wider search:** it widens the hypothesis space, the deflation hardens
proportionally, and no edge survives.

## 3. The "impossible triangle" (why nothing passes)

Across 112+ runs, no single vintage was strong on all axes; the closest each cleared a *different* criterion and failed
the rest — the clearest possible sign the criteria measure genuinely independent things:
- **DSR 0.951** (clears the deflated-Sharpe bar) → but **PBO 1.0** (maximally overfit).
- **PBO 0.0** (perfectly robust) → but **DSR 0.0** (robust because it barely trades).
- **SPA p 0.014** (a real signal) → but DSR 0.44 / PBO 0.93 (doesn't survive deflation).

The G1 pass region (DSR > 0.95 **and** PBO < 0.5) contained **0 of 83** runs with both recorded.

## 4. What was built (the engineering the hunt drove)

| Ticket | What | Why it was needed |
|---|---|---|
| **QE-494** | Wire native-tls into the http-feature fetch agents | The `http` feature had no TLS backend — real ingest had *never* been able to run |
| **QE-495** | Jitter-tolerant funding join (nearest-hour) | Real Binance `fundingTime` stamps jitter by ms; the exact-ms join dropped ~15% → the funding gate failed *every* real train |
| **QE-496** | Fold effective run parameters into the vintage lineage id | The id hashed only config+commit+seed; distinct windows/budgets **silently overwrote** each other's sealed vintages |
| **QE-497** | Wire open-interest + premium ingest; revive flow indicators | Klines-only store killed the premium/OI factor family (factor-breadth constraint) |
| **QE-498** | Expanded point-in-time universe (more liquid perps) | More independent markets to search (training is single-instrument) |
| **QE-499** | GP evolve → G1 vintage bridge (Phase A+B+C) | The representation lever — evolved formulas reach the unmodified gate, honestly |

Every one shipped through a **design → review → implement → verify** discipline; QE-499 (honesty-critical) additionally
went through a design-review (caught 6 blockers), an implementation honesty-review (caught 4 under-deflation defects),
and a self-verification of every deflation invariant — all fixed and re-verified. The one governance relaxation (research
`gate_evidence`) was surfaced for explicit sign-off, not self-authorized, and is provably safe (three independent
non-production locks remain).

## 5. Operational findings surfaced (worth fixing regardless of edge)

- **Backtest CLI parity** — no `--config` flag (undocumented `QE_CONFIG` env), no default `--universe` from the vintage,
  and **no `--help` anywhere** in the CLI.
- **Degenerate default holdout** — 31 bars on 1h data → vacuous OOS criteria / zero-trade holdouts.
- **`result.json` lacks a CPCV summary block** despite the docs; the gate criterion value's unit is unlabeled.
- **OI retention** — `openInterestHist` serves only ~31 days; historical OI factors are dead until a persisted source
  exists.
- **QE-493** (open) — the sealed `is_promoted()` verdict is legible but **not yet enforced** at a deploy-selection
  boundary.

## 6. Recommendations

1. **Stop hunting edge on this data.** The engine has answered clearly and repeatedly; more seeds/budgets/factors/
   representation on BTC/ETH klines will not change the verdict — each lever only raises the deflation bar.
2. **If pursuing edge:** change the *signal source*, not the search — historically-deep open-interest/basis/microstructure
   or a **cross-sectional** universe (which needs a real multi-instrument training mode, not a tweak). That is where
   crypto edge more often survives deflation.
3. **If hardening the engine:** finish the operability backlog (§5) so it is production-trustworthy for the day a
   genuinely different signal source arrives — the honesty machinery itself is validated and worth preserving exactly.
4. **Never weaken the gate to manufacture a pass.** A DSR of 0.996 that fails PBO/OOS is precisely what the gate must
   reject; lowering any threshold converts an honest "no edge" into a dishonest "strategy" — the one failure this engine
   exists to prevent.

`Sources: G1-hunt HTML report (2026-08-01), premium re-hunt, QE-499 design doc §8 (Phase C results). All gate values read
verbatim from each run's result.json → g1.criteria / robustness. 112 baseline+premium runs + 2 Phase C runs, 0 G1
passes, unmodified gate throughout.`
