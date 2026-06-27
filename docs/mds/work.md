# Work — PR review tracker

Active PRs awaiting/under review for the P0/P1 ticket run. Each entry is reviewed by the
dedicated review agent, which writes `[Reviewed]`/`[Approved]` + comments inline. On merge, the
approved block is archived to `docs/mds/reviewed/<ticket>.md` and removed from here.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

## Completed (archived in `docs/mds/reviewed/`)
- QE-001 — Cargo workspace & crate topology — PR #1 — Approved & merged.
- QE-002 — Configuration system — PR #2 — Approved & merged.
- QE-003 — Structured logging & tracing — PR #3 — Approved & merged.
- QE-004 — Error model & result conventions — PR #4 — Approved & merged.
- QE-005 — CI pipeline — PR #5 — Approved & merged.
- QE-006 — Determinism & reproducibility harness — PR #6 — Approved & merged.
- QE-007 — Shared domain types — PR #7 — Approved & merged.
- QE-008 — Clock-skew / time-sync guard — PR #8 — Approved & merged.
- QE-009 — Risk-limit & kill-switch contract — PR #9 — Approved & merged.
- QE-010 — LMDB market-data store — PR #10 — Approved & merged.
- QE-011 — LMDB synthetic-data store — PR #11 — Approved & merged.
- QE-012 — Instrument-universe config & point-in-time membership — PR #12 — Approved & merged.
- QE-013 — Local run & deployment-agnostic packaging — PR #13 — Approved & merged. **(P0 complete)**
- QE-101 — Binance public-dumps downloader — PR #14 — Approved & merged.
- QE-102 — Venue REST month-to-date backfill client — PR #15 — Approved & merged.
- QE-103 — Data-integrity & source reconciliation validation — PR #16 — Approved & merged.
- QE-104 — Fusion, normalisation & Arrow serialisation — PR #17 — Approved & merged.
- QE-105 — Persist fused market data to LMDB — PR #18 — Approved & merged.
- QE-106 — Multi-resolution bar reconstruction (batch) — PR #19 — Approved & merged.
- QE-107 — Indicator catalogue (quantised, deterministic, parity-ready) — PR #20 — Approved & merged.
- QE-108 — Feature vector assembly → synthetic store — PR #21 — Approved & merged.

---

## QE-109 — Execution-friction & funding model — PR #22 — [Approved]

- **Branch:** `qe-109/execution-friction-funding`
- **PR:** https://github.com/aoimasu/quant-engine/pull/22
- **Latest commit:** `77fbc78` (+ Cargo.lock amend)
- **Evidence/design:** `docs/architecture/qe-109-execution-friction-funding-design.md`
- **Changed surface:** `crates/wfo` — **new** `src/friction.rs`, `lib.rs` wiring, `Cargo.toml`
  (+`rust_decimal`), `Cargo.lock`. No new third-party crates. Also bundles the QE-108 archive
  (`docs/mds/reviewed/qe-108.md`) + `docs/mds/work.md` bookkeeping — branch protection blocks direct
  `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] Backtest P&L is net-of-cost and funding-adjusted; a turnover-1 strategy shows fee drag.
- [x] A held-through-funding directional strategy shows the correct funding sign in P&L.
- [x] Cost-sensitivity sweep is available to the validation report (QE-133).

### Verification (run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — **270 passed, 1 ignored** (qe-wfo friction 7)
- `cargo test -p qe-cli --test dependency_topology` — passes (QE-001 `runtime ⊥ wfo` untouched)
- `cargo deny check` — advisories/bans/licenses/sources ok (only `rust_decimal`, already a workspace dep)

Key AC-proving tests (`friction::tests`):
- **AC #1 (fee drag)** — `ac1_turnover_one_shows_fee_drag`: buy 1 @100 + sell 1 @100 (flat) → `gross
  == 0`, `fees == 0.10`, `slippage == 0.02`, `net == −0.12 < 0`.
- **AC #2 (funding sign)** — `ac2_funding_sign_is_correct_for_direction`: long through `+rate` pays
  (`funding < 0`); short receives (`> 0`); negative rate flips; flat at the stamp → `0`.
- **AC #3 (sweep)** — `ac3_cost_sweep_scales_assumed_costs_only`: at `[1×, 2×]` fees + slippage
  exactly double; `gross` and `funding` unchanged; `net` worse at 2×.
- **Supporting:** `position_realises_average_cost_pnl` (add → avg, partial reduce → realised on closed
  qty, flip → realise all + reopen remainder), `defaults_are_vip0`, `maker_is_cheaper_than_taker`.

### Design notes for the reviewer
- **Decomposed P&L is the point.** `simulate` returns `{ gross, fees, slippage, funding }` so fee drag
  and funding sign are directly assertable and feed the QE-133 report. `net = gross − fees − slippage
  + funding` (funding is a signed cashflow — negative when the trader pays).
- **Funding from the actual series.** `FundingStamp` carries the historical `rate` + `mark_price`;
  cashflow `= −signed_qty · mark · rate`. Not a constant.
- **Sweep scopes assumed costs only.** `cost_multiplier` scales fees + slippage; funding (a realised
  market cashflow) is never scaled — so the 1×/2× sweep is an honest cost-sensitivity, not a funding
  re-estimate.
- **Exact money.** All arithmetic is `rust_decimal`; no float. Next-bar-open fill convention is the
  caller's (QE-120 supplies the prices); documented on `Fill`.
- **Topology.** Lives in `qe-wfo` (already domain/signal/storage); nothing new points into
  `qe-runtime`, so the QE-001 `runtime ⊥ wfo` invariant is untouched.
- **Out of scope:** strategy logic / walk-forward windowing (QE-110+/QE-120); live execution (QE-217).
