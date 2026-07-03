# QE-252 — Backtester trade-level recording — design & evidence note

`Phase: PreP3` · `Area: runnable jobs / wfo` · `Depends on: QE-120`
`Branch: qe-252/backtester-trade-recording` · `Ticket: docs/backlog.md §QE-252` · `Plan: docs/superpowers/plans/2026-07-03-admin-ui-v1-cli-jobs.md Task 4`

## 1. Problem

`BacktestResult` (`crates/wfo/src/backtest.rs`) exposes per-bar `returns` and an
aggregate `trades` **count** only. The admin-ui Trades tab and the win-rate /
profit-factor / Sortino metrics (spec §8.1) need **per-trade** records. QE-252
closes that gap in `qe-wfo` **without disturbing** the existing hot-path result
(`returns` / `net_pnl` / `accepted` / `fitness` must be byte-for-byte identical).

## 2. Current-state evidence

All line references are to `crates/wfo/src/backtest.rs` at this branch's base.

- **`pub fn backtest(genome, bars, cfg) -> BacktestResult`** (the only public entry,
  `#[must_use]`) runs a single pass over `bars`. It is the sole simulation loop; no
  other function walks the bar series.
- The simulation loop already tracks the position lifecycle with two locals:
  - `let mut entry_bar: Option<usize> = None;` — set to `Some(i)` on an entry fill,
    reset to `None` on a close fill.
  - `let mut trades = 0usize;` — incremented **only** inside the `Pending::Enter`
    arm, right after `apply_fill(...)` and `entry_bar = Some(i)`.
- **Entry** (`Pending::Enter(dir)` arm): computes `notional = size_frac * equity_prev`,
  `qty = notional / price`, and — **only when `qty > 0`** — maps `dir: Direction`
  (`Long`/`Short`) to a fill `Side` (`Buy`/`Sell`), calls `apply_fill`, sets
  `entry_bar = Some(i)`, and does `trades += 1`. The `dir: Direction` and the fill
  `price: Decimal` are both in scope here — the two facts an entry record needs.
- **Exit** (`Pending::Close` arm): closes the whole position (`qty = pos_qty.abs()`)
  when `qty > 0`, then sets `entry_bar = None`. The close `price: Decimal` is in scope.
- **Order source.** `pending` is set from `genome.decide(...)` at the *previous* bar
  (`Decision::Enter → Pending::Enter`, `Decision::Exit → Pending::Close`,
  `Decision::Hold → None`). Fills happen at bar `i+1` (no look-ahead).
- **Enter is flat-only.** `qe_signal::genome::Decision::Enter` is documented "only
  emitted when flat" (`crates/signal/src/genome.rs:178`) and `decide` enforces it.
  Therefore every `trades += 1` is a genuine **flat → position** transition, and a
  position is closed **only** by a `Pending::Close` fill. This is what makes a
  strict one-entry-then-one-close pairing sound: at most one fill per bar (a bar
  executes a single `pending` order), so entries and closes cannot overlap.
- **Types.** `Direction` and `Side` (`crates/domain/src/side.rs:7,16`) both derive
  `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize`. Prices are
  `rust_decimal::Decimal`. The *position side* is `Direction` (Long/Short); `Side`
  (Buy/Sell) is only the per-fill order side.

## 3. Design decisions

1. **`backtest_with_trades` is the real function; `backtest` delegates.**
   ```rust
   pub fn backtest_with_trades(genome, bars, cfg) -> (BacktestResult, Vec<TradeFill>)
   pub fn backtest(genome, bars, cfg) -> BacktestResult   // = .0, trades discarded
   ```
   Delegation guarantees the hot path's `returns` / `net_pnl` / `accepted` /
   `fitness` are unchanged by construction — the recorder is purely additive state.

2. **`TradeFill` field types** (matching the existing code):
   ```rust
   pub struct TradeFill {
       pub entry_idx: usize,       // bar index of the entry fill
       pub exit_idx: usize,        // bar index of the close fill
       pub side: Direction,        // position side (Long/Short) — the *position* side type
       pub entry_px: Decimal,      // fill price at entry
       pub exit_px: Decimal,       // fill price at exit
       pub return_frac: f64,       // signed gross price return of the round-trip
   }
   ```
   `side` is `Direction` because a round-trip trade *is* a long or short position;
   `Side` (Buy/Sell) is only an order-level detail. Derives: `Debug, Clone, Copy, PartialEq`.

3. **`return_frac` = signed gross price return**, documented on the field:
   - Long: `(exit_px - entry_px) / entry_px`
   - Short: `(entry_px - exit_px) / entry_px`

   This is the per-trade price move fraction adjusted for direction — a winning
   round-trip yields `> 0` (the AC). It is deliberately **gross** (price-only): the
   `TradeFill` carries exactly `{entry_px, exit_px, side}`, so this is the return
   derivable from the record itself. Net-of-cost accounting stays in the aggregate
   `returns` / `net_pnl` (unchanged). Downstream win-rate / profit-factor
   (QE-251, `crates/cli/src/jobs/metrics.rs`) consume the mapped `TradeRow`.

4. **Recorder lives inside the existing loop — extend, don't rewrite.**
   - Add one local: `let mut open: Option<(usize, Direction, Decimal)> = None;`
     (entry_idx, side, entry_px), set alongside the existing `entry_bar = Some(i)`
     / `trades += 1` in the `Pending::Enter` arm.
   - In the `Pending::Close` arm, alongside the existing `entry_bar = None`, if an
     `open` entry exists, compute `return_frac` and push one `TradeFill`, then clear
     `open`. A position still open at the last bar records **no** `TradeFill`
     (only *closed* round-trips are recorded), consistent with "one per closed
     round-trip".

5. **Scope.** Only `qe-wfo` trade recording. `win_rate` / `profit_factor` are
   explicitly **out of scope** here (they belong in `crates/cli/src/jobs/metrics.rs`
   under QE-251 / Task 3; see ticket note). Not added in this ticket.

## 4. Test plan (TDD)

New `#[cfg(test)]` cases in `crates/wfo/src/backtest.rs`:

1. **`single_winning_round_trip_records_one_trade`** — a long-only genome and a
   rising-price series engineered so feature-0 fires an entry exactly once; price
   rises between the entry fill and the time-based exit fill. Assert
   `trades == 1`, `fills.len() == 1`, `fills[0].return_frac > 0.0`,
   `fills[0].side == Direction::Long`, and `entry_idx < exit_idx`.
2. **`backtest_delegates_to_with_trades`** — for an existing fixture,
   `backtest(g, bars, cfg) == backtest_with_trades(g, bars, cfg).0` (result identity).
3. The whole existing `qe-wfo` suite must still pass unchanged (`backtest()` callers
   untouched) — verified by `cargo test -p qe-wfo --locked`.

## 5. Risks & mitigations

- **Hot-path drift.** Mitigated by delegation: `backtest` returns `.0` verbatim; the
  recorder never touches `cash` / `pos_qty` / `returns` / `fitness`.
- **Double-counting / orphan records.** Enter is flat-only and one fill per bar, so
  entry/close strictly alternate; `open` is `Some` only between a paired entry and
  close. An unclosed final position is intentionally not recorded.
- **`Decimal → f64` on `return_frac`.** Uses `.to_f64().unwrap_or(0.0)`, matching the
  existing `returns` conversion pattern in the same loop.

## 6. Green gate

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -D warnings`;
`cargo test --workspace --locked`; `cargo test -p qe-architecture --test firewall --locked`;
`cargo deny check`. All must pass on the committed SHA before push/PR.
