# QE-428 — Route reported-backtest impact through the selection cost model / a CLI flag so reporting PnL matches selection

- **Ticket**: QE-428 (P3, follow-up from the QE-403 review). Spec of record: `docs/backlog.md` (Review R1, §R1.b) row + `docs/mds/reviewed/qe-403.md` (review verdict, non-blocking nit 1).
- **Depends on**: QE-403 (merged), QE-128 (capacity model — present for *weights*, see below).
- **Goal**: reporting PnL matches selection — the reporting backtest must price size-impact through the **same** cost model selection optimises against.

## Current-state asymmetry (evidence)

The SELECTION path and the REPORTING path price size-impact differently today:

- **Selection** (`crates/cli/src/jobs/train.rs`): the train search, ensemble backtests, funding/net figures and G1 all run on
  `BacktestConfig { min_trades: 1, windows: 2, ..BacktestConfig::default() }` (train.rs:309-313). `BacktestConfig::default().friction = FrictionConfig::default()` (`crates/wfo/src/backtest.rs:52-56`), whose `slippage = SlippageModel::default()` carries a **non-zero** size-impact `impact = Decimal::new(1, 4) = 0.0001` (`crates/wfo/src/friction.rs:68-80`, set by QE-403). So selection prices the quadratic size term `notional · impact · qty`.
- **Reporting** (`crates/cli/src/jobs/backtest.rs`): `backtest_config()` (backtest.rs:188-214) deliberately pins
  `slippage: SlippageModel { impact: Decimal::ZERO, ..SlippageModel::default() }`. The inline comment (backtest.rs:201-205) states this was done to keep `golden_result.json` byte-identical at QE-403 time, with routing reported impact through a flag / QE-128 explicitly deferred.

**Consequence**: reported net PnL is computed with zero size-impact while selection used `0.0001`. `apply_fill` (`crates/wfo/src/backtest.rs:291-309`) subtracts `slippage.cost(notional_abs, qty_abs)` from cash, so a non-zero impact strictly lowers net-of-cost returns. Reported PnL therefore overstates the strategy versus the cost model selection actually optimised against — the gap QE-428 closes.

### How selection prices impact — is QE-128 involved?

The literal selection impact is the **flat `FrictionConfig::default().slippage.impact = 0.0001`** — NOT a capacity/size-derived per-run value. QE-128's `CapacityModel`/`capacity()` *is* wired into selection (`train.rs:651-674`, `capacity_capped_weights`), but only to cap **ensemble weights** at a target AUM; it does not feed the `impact` coefficient in the friction/PnL model. So "matches selection" means literally `impact = 0.0001` (the shared `SlippageModel::default()` value). Deriving reporting impact from QE-128's capacity model would NOT match selection (selection doesn't price impact that way) and is therefore **out of scope** here (future work if selection ever prices capacity-derived impact).

## Chosen approach — B (CLI flag defaulting to the selection value)

**Approach B**: add a `--reporting-impact <val>` CLI flag controlling the reporting size-impact coefficient, **defaulting to the selection value** so the default converges reporting with selection (and regenerates the golden); the flag allows override.

- Represented as `BacktestParams.reporting_impact: Option<Decimal>`: `None` = "match selection" → resolved to `SlippageModel::default().impact` (== `FrictionConfig::default().slippage.impact`, the exact selection value, so it stays in sync if the selection default ever moves); `Some(v)` = explicit override.
- `backtest_config()` stops pinning `impact = ZERO` and instead uses `FrictionConfig::default()`'s slippage (impact `0.0001`) by default, overriding only `impact` when the flag is set.

**Why B over A** (make reporting unconditionally use the selection FrictionConfig): the ticket title literally names "the selection cost model **/ a CLI flag (QE-128)**", and the QE-403 review nit asks to "route reported impact through a CLI flag / the QE-128 capacity model". B satisfies both phrasings — the **default** delivers "reporting PnL matches selection" out of the box (identical to A's effect on the golden), and the flag adds an explicit, testable override without a new capacity model. Impact is parsed as `Decimal` (exact, consistent with the friction model), so the default/golden path is exact.

The `Costs` contract (`taker_fee_bps`, `slippage_model` label) is unchanged: both are verbatim CLI inputs, not computed costs, so no result-schema change. The pre-existing `slippage_model` label (`"square-root-impact"` in the fixture) is now *honoured* rather than cosmetic.

## Golden-fixture change (intended, regenerated via real code)

Aligning reporting impact with selection changes `crates/cli/tests/fixtures/golden_result.json`. Fields that change (all cost-impacted, because non-zero impact lowers net-of-cost returns):

- `metrics` (cagr, sharpe, sortino, max_dd, profit_factor; win_rate may shift if a marginal trade flips WIN/LOSS),
- `equity_curve`, `drawdown`, `monthly_returns`,
- per-trade `return_pct` and `result` (WIN/LOSS).

Fields that stay byte-identical: `strategy`, `window`, `universe`, `costs`, and every trade's `id/symbol/side/entry/exit/hold` (impact does not move entry/exit *decisions*, only PnL — trade count stays 16).

**Regeneration**: via the real production code path only — the `#[ignore]`d `regenerate_fixtures` test (`crates/cli/tests/backtest_job.rs`) which calls the actual `run_backtest`. No hand-edited numbers/hashes. Reproducibility: re-run `regenerate_fixtures` twice → byte-identical output (backtest is deterministic, single-threaded, no wall-clock/RNG). Verified the diff touches only the cost-impacted fields above and NO other golden (train/vintage goldens) moves.

## Test plan

1. Unit (`backtest.rs` tests): `backtest_config(fee, None).friction.slippage.impact == BacktestConfig::default().friction.slippage.impact` and `!= Decimal::ZERO` — reporting default literally equals the selection impact. Non-vacuous: under the old `impact = ZERO` pin this fails.
2. Unit: `backtest_config(fee, Some(x)).friction.slippage.impact == x` — the flag overrides.
3. Integration (`backtest_job.rs`): run the fixture with the default (`reporting_impact: None`) vs an explicit `Some(ZERO)` baseline; assert same trade count but strictly lower final equity / cagr under the priced impact. Non-vacuous: identical to the old behaviour only if impact were zero — it isn't.
4. Golden: `backtest_over_fixture_store_matches_golden` passes against the regenerated golden.
5. CLI parse tests: `--reporting-impact` parses to `Some(Decimal)`; absent ⇒ `None`.
6. Green gate: fmt --check, clippy -D warnings, test --all, deny check, firewall test.

## Risks / blast radius

- **Scope**: `crates/cli` (backtest job + arg parsing + main wiring) and the committed golden fixture only. No change to `crates/wfo` friction/selection logic. Selection is untouched, so no other golden (train/vintage) can move.
- **Behavioural**: reported metrics for real runs shift lower (more honest — net of the impact selection already priced). This is the intended correction, documented here and in the PR.
- **Rollback**: revert the PR; `--reporting-impact 0` reproduces the old zero-impact reporting numbers without a revert if needed.
- **QE-128 capacity-derived impact**: out of scope (selection prices flat impact, not capacity-derived); noted as future work.
