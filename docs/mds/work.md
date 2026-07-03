# Work — PR review tracker

Transient scratchpad for the **PR currently under review** only. A PR entry is added here when it
reaches review, the dedicated review agent writes `[Reviewed]`/`[Approved]` + comments inline, and on
merge the approved block is archived to `docs/mds/reviewed/<ticket>.md` and this file is **cleared back
to empty**. No running "Completed" list is kept here — the traceable history lives solely in
`docs/mds/reviewed/`.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

---

## [Approved] PR #78 — QE-252 Backtester trade-level recording
- **Ticket:** QE-252 — Backtester trade-level recording (trades, win-rate, profit-factor)
- **Branch:** `qe-252/backtester-trade-recording`
- **PR:** https://github.com/aoimasu/quant-engine/pull/78
- **Latest commit:** `f002962`
- **Evidence note:** `docs/architecture/qe-252-backtester-trade-recording-design.md`
- **Changed docs:** `docs/architecture/qe-252-backtester-trade-recording-design.md` (new)

### Acceptance criteria (from backlog QE-252)
- [ ] A known single winning round-trip yields exactly one `TradeFill` with `return_frac > 0`.
- [ ] The existing `qe-wfo` suite still passes unchanged.
- [ ] `win_rate`/`profit_factor` match hand-computed values. *(NOTE: these two pure fns are scoped to
  `crates/cli/src/jobs/metrics.rs` under QE-251/Task 3 — deferred out of QE-252 per the v1 plan; QE-252
  delivers only the `qe-wfo` trade-recording substrate. Reviewer: verify this split is acceptable.)*

### Scope delivered
- `qe_wfo::backtest::backtest_with_trades(genome, bars, cfg) -> (BacktestResult, Vec<TradeFill>)`;
  `backtest()` delegates via `.0` (hot-path result unchanged by construction).
- `TradeFill { entry_idx, exit_idx, side: Direction, entry_px: Decimal, exit_px: Decimal, return_frac: f64 }`,
  one per closed round-trip; recorder extends the existing entry/close arms of the sim loop.
- Exported `backtest_with_trades` + `TradeFill` from `qe_wfo::lib`.
- 3 TDD tests (RED-verified first): single winning round-trip, delegation identity, unclosed-position-records-nothing.

### Verification (commit `f002962`)
- `cargo fmt --all --check` — PASS
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — PASS
- `cargo test --workspace --locked` — PASS (581 passed, 1 ignored)
- `cargo test -p qe-architecture --test firewall --locked` — PASS
- `cargo deny check` — PASS

### Review — [Approved]

Reviewed branch `qe-252/backtester-trade-recording` (diff vs `main`): `crates/wfo/src/backtest.rs`,
`crates/wfo/src/lib.rs`, and the new design note. Scope judged against QE-252's `qe-wfo`
trade-recording substrate only (`win_rate`/`profit_factor` correctly deferred to QE-251 —
deferral is sound: those are pure functions over the emitted records).

**Correctness — verified against source, no blocking issues:**
- **One `TradeFill` per closed round-trip:** `open = Some((i, dir, price))` is set *inside* the
  `qty > Decimal::ZERO` entry guard (backtest.rs:189), alongside the existing `entry_bar = Some(i)` /
  `trades += 1`, so recorder state and the `trades` counter stay in lockstep. The close arm consumes it
  via `open.take()` under the `qty > 0` close guard (backtest.rs:200–216), pushing exactly one fill.
  `entry_idx`/`exit_idx`/`side`(=entry `dir`)/`entry_px`/`exit_px` all captured from the correct fills.
- **`return_frac` sign:** Long `(exit−entry)/entry`, Short `(entry−exit)/entry` (backtest.rs:202–208) —
  correct signed gross price return; a winning round-trip is `> 0` (AC #1 met). `.to_f64().unwrap_or(0.0)`
  matches the existing per-bar `returns` conversion pattern.
- **`backtest()` delegation is byte-identical by construction:** returns `.0`; the recorder only mutates
  the new `open`/`fills` locals and never touches `cash`/`pos_qty`/`returns`/`equity`/`trades`/`net_pnl`/
  `accepted`/`fitness`. `BacktestResult` derives `PartialEq`, so the identity test is valid. AC #2 (suite
  unchanged) holds — no hot-path behavior change.
- **Edge cases sound:** unclosed final position leaves `open = Some` and is never pushed
  (`open_position_at_end_records_no_trade` pins it); `entry_idx < exit_idx` is guaranteed (one fill/bar,
  earliest exit is entry+1); the flat-only-entry + one-fill-per-bar invariant makes entry/close strictly
  alternate, so no orphan/double records. Deterministic (no map iteration or RNG).
- **Test quality:** `single_winning_round_trip_records_one_trade` genuinely pins the new behavior
  (asserts `trades==1`, `fills.len()==1`, side, `entry_idx<exit_idx`, `exit_px>entry_px`, `return_frac>0`
  and matches the recomputed gross return within 1e-12). Delegation and unclosed-position tests are
  meaningful. No scope creep (only backtest.rs + lib.rs export + design note touched).

**Non-blocking nits:**
- (nit) Short-side sign is untested: all fixtures are long (`single_entry_uptrend` + `long_genome`). The
  `Direction::Short => (entry_px - price)/entry_px` branch (backtest.rs:204) is correct by inspection but
  has no test. Recommend a short winning-round-trip case asserting `return_frac > 0` on a falling series.
- (nit) `TradeFill` is deliberately gross/price-only and carries no `qty`/fees, so per-trade *net* P&L is
  not reconstructable from the record. Fine and documented for QE-252, but QE-251's `win_rate`/
  `profit_factor` built on `return_frac` will be gross approximations — flag for that ticket so the
  cost-blind semantics are an explicit choice, not an accident.
- (nit) `backtest_delegates_to_with_trades` compares `BacktestResult` via derived `PartialEq` over f64
  `fitness` fields; safe here because it's the same deterministic input twice, but it would spuriously
  fail if `fitness.mean` were ever `NaN` for a chosen fixture. Current fixture (accepted, finite) is fine.

Verdict: **[Approved]** — meets all in-scope acceptance criteria; 0 blocking, 3 non-blocking nits.
