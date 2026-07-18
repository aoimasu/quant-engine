# QE-449 — Guard the unused maker rate against future adverse-selection blindness

`Phase: Review R2 (P3 — panel #20, unanimous)` · `Area: cross-cutting` · `Depends on: QE-109`
· Spec of record: [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-449`](../reviews/2026-07-16-maxdama-panel-review.md)
· Backlog: [`docs/backlog.md`](../backlog.md) → Review R2.b

## 1. Problem (what the panel found)

`FeeSchedule` in `crates/wfo/src/friction.rs` carries **both** a taker rate (VIP0 `0.05%`) and a
maker rate (VIP0 `0.02%`). The maker rate is **not used to fill any order today** — the engine is a
**pure taker**. But the number sits in the config reading as a *free rebate*: a future
"maker-rebate optimization" (add `post_only` fills to collect the maker/taker gap) would look
**profitable in backtest** while **losing to adverse selection live** — the classic maxdama §7.6
trap. Worse, the resulting Sharpe inflation is a **systematic per-fill bias**, so it flows through
the DSR/PBO/SPA apparatus **undeflated** (DSR is absolute vs a noise ceiling; it cannot remove a
cost bias). Severity is **latent** (low, panel #20) — a trap to guard, not a current defect
(Math#2 and Trading concur).

## 2. Evidence: the engine is a **pure taker** today

Grepped `crates/edge`, `crates/hedger`, `crates/wfo` for any maker/limit-order machinery:

```
$ grep -rni "post_only|postonly|post-only" crates/edge crates/hedger crates/wfo   → (none)
$ grep -rni "ordertype|order_type"          crates/edge crates/hedger             → (none)
$ grep -rni "maker|limit_order|LimitOrder"  crates/edge crates/hedger             → (none)
$ grep -rn  "Liquidity::Maker"              crates/ --include=*.rs
    crates/wfo/src/friction.rs:48   Liquidity::Maker => self.maker,   # the enum arm (definition)
    crates/wfo/src/friction.rs:461  ...fee(.., Liquidity::Maker) ...   # a unit test
```

So `Liquidity::Maker` is referenced by **exactly two** sites: the `rate()` match arm that *defines*
the maker branch, and one unit test (`maker_is_cheaper_than_taker`). **No production code path
selects it.** The one place a fill is priced on the backtest/selection path hardcodes the taker
role:

```
crates/wfo/src/backtest.rs:380
    let fee = cfg.friction.fees.fee(notional_abs, Liquidity::Taker) * cfg.friction.cost_multiplier;
```

There is no `post_only`, no `OrderType`, no limit-order machinery anywhere in `edge` or `hedger`.
Conclusion: **pure taker** — adverse selection is a non-issue *today*, exactly as the ticket states.

## 3. The trap, stated precisely (§7.6)

A resting (maker) fill is **systematically selected against**: it executes only when the market is
about to move through the resting price, i.e. conditional on a fill, the subsequent short-horizon
mark drift is **adverse** in expectation. Collecting the spread / the maker rebate **without**
charging that fill-conditional adverse drift (an "adverse-selection markout") **overstates PnL**:
the modelled saving (taker − maker ≈ `0.03%`/fill) is real, but the unmodelled adverse markout that
accompanies a maker fill typically **outweighs** it. A backtest that flips fills to `Liquidity::Maker`
to harvest the gap would therefore book a phantom edge and an inflated Sharpe that DSR **cannot**
deflate (systematic bias, not selection noise).

## 4. The guard (this ticket — doc + a cheap test, behaviour-preserving)

This is a **latent-trap guard, primarily DOCUMENTATION**. No behaviour change; no golden moves.

1. **Doc-comment invariant on `FeeSchedule`** (and on the `Liquidity::Maker` variant): the maker
   rate MUST NOT be used to fill orders without a **paired adverse-selection markout**. If
   `post_only`/maker fills are ever added, they MUST be accompanied by a modelled expected
   fill-conditional adverse drift; otherwise PnL and Sharpe are overstated and DSR cannot deflate
   the bias. Point forward at the QE-444 `alpha_loss` machinery as the natural home for such a
   directional markout term.

2. **A cheap guard test** that makes the trap hard to fall into silently: assert that the current
   backtest/selection fill path charges the **taker** rate only — i.e. that no current code path
   selects `Liquidity::Maker`. Concretely: `apply_fill`/`simulate` on the default friction config
   charge `fees.taker` (never `fees.maker`) for representative fills, and the two rates are distinct
   so the assertion is non-vacuous. A future change that starts routing maker fills will trip this
   test and is thereby **forced** to also address the markout (update the guard consciously, add the
   §7.6 drift term).

Kept a **guard, not a behaviour change**: the default path stays taker-only, panic-free, and the
diff is doc + a test. **No golden output can move.**

## 5. Golden-safe / green-gate

- Doc-comment + one test in `crates/wfo/src/friction.rs`. No production logic touched → **no golden
  moves** (verified: no `content_hash` / vintage output depends on a doc-comment or a new test).
- Behaviour-preserving: taker-only today; the guard is an assertion over the existing default path.
- Green gate (local, CI disabled): `cargo fmt --all -- --check`; `cargo clippy --workspace
  --all-targets --locked -- -D warnings` and `--all-features --locked -- -D warnings`; `cargo test
  --workspace --locked`; `cargo deny check`; `cargo test -p qe-architecture --test firewall`.

`Spec ref: maxdama §7.6 (maker fills carry adverse selection that outweighs the collected spread).`
