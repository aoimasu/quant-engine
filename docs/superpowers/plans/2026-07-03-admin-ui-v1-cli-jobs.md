# Admin UI v1 — Runnable CLI jobs (spec 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a backtest of a sealed vintage genuinely runnable from `qe-cli`, emitting JSON-line progress and a `result.json` matching the UI's result contract — the foundation the `qe-server` and SPA are built against.

**Architecture:** Two new deterministic `qe-cli` subcommands (`backtest`, `ingest`) wire existing libraries (`qe-vintage` load, `qe-storage` bar reads, `qe-wfo` backtester, cost model) into runnable jobs that write artifacts to a run directory. The server (spec 2) later spawns these as subprocesses. No async, no server here.

**Tech Stack:** Rust (workspace, toolchain 1.96.0), `rust_decimal`, `serde`/`serde_json`, existing crates `qe-vintage`, `qe-storage`, `qe-wfo`, `qe-signal`, `qe-domain`, `qe-config`.

## Global Constraints

- **Toolchain:** Rust `1.96.0` (pinned by `rust-toolchain.toml`). Source `. "$HOME/.cargo/env"` before cargo.
- **Determinism:** single-threaded, pull-based, no wall-clock in any output. Same inputs ⇒ byte-identical `result.json`. No `Date::now`/RNG in job output.
- **Firewall:** these jobs live in `qe-cli` (the composition root) — it already depends on both worlds. Do **not** add training-crate deps to `qe-runtime`. The QE-132 firewall + QE-001 decoupling tests must stay green.
- **Green gate (hard precondition for any commit past a task):** `cargo fmt --all --check` · `cargo clippy --workspace --all-targets --locked -- -D warnings` · `cargo test --workspace --locked` · `cargo test -p qe-architecture --test firewall --locked` · `cargo deny check`.
- **Progress protocol (stdout, one JSON object per line):**
  `{"t":"progress","pct":<0-100>,"stage":"<load|scan|simulate|report>","msg":"<human line>"}` then terminal `{"t":"done","result":"result.json"}` or `{"t":"error","msg":"…"}`.
- **Result contract:** the schema in the spec §8.1 (`strategy`, `window`, `universe`, `costs`, `metrics{cagr,sharpe,sortino,max_dd,win_rate,profit_factor}`, `equity_curve`, `drawdown`, `monthly_returns`, `trades[]`). All numbers serialised as JSON numbers; money/qty from `Decimal` via string→number is avoided (use `f64` in the result, `Decimal` internally).
- **Run directory:** a job is invoked with `--run-dir <path>`; it writes `result.json` (and nothing else in v1) there. Progress goes to stdout. The caller (server, spec 2) owns `meta.json`.

---

## Known API surface (verified against the code)

Use these exact signatures — confirmed in the tree on 2026-07-03:

```rust
// qe-vintage
qe_vintage::VintageRepository::new(root: impl Into<PathBuf>) -> VintageRepository;
repo.load(vintage_id: &str) -> Result<qe_vintage::Vintage, VintageError>;
vintage.verify() -> Result<(), VintageError>;
vintage.content: qe_vintage::VintageContent {
    vintage_id: String, chromosomes: Vec<qe_signal::Genome>, weights: Vec<f64>, /* … */ }

// qe-storage
qe_storage::store::MarketStore::open(path: impl AsRef<Path>, map_size: usize) -> Result<MarketStore, StorageError>;
store.scan_bars(instrument: &InstrumentId, resolution: Resolution, from: Timestamp, to: Timestamp)
    -> Result<Vec<qe_domain::Bar>, StorageError>;

// qe-wfo backtester (QE-120)
qe_wfo::backtest::backtest(genome: &Genome, bars: &[Bar], cfg: &BacktestConfig) -> BacktestResult;
struct BacktestConfig { friction: qe_wfo::friction::FrictionConfig, min_trades: usize, windows: usize }
struct BacktestResult { returns: Vec<f64>, trades: usize, net_pnl: Decimal, accepted: bool, fitness: NoiseRobustFitness }

// qe-domain
struct Bar { open_time: Timestamp, resolution: Resolution, open: Price, high: Price, low: Price, close: Price, volume: Qty, trades: u64 }
```

**Gap to close (Task 4):** `BacktestResult` gives per-bar `returns` and a trade **count** — not a per-trade log, win-rate, profit-factor, or Sortino. The design's Trades tab + two of the six metrics need trade-level data. Task 4 adds a trade-recording path; until then those fields are computed-where-possible and the trade list is explicitly empty (never faked).

## File structure

- Create `crates/cli/src/jobs/mod.rs` — job dispatch shared types (`ProgressLine`, `RunError`, `emit_progress`).
- Create `crates/cli/src/jobs/backtest.rs` — the backtest job: params, orchestration, `run_backtest(...) -> Result<BacktestResultDoc, RunError>`.
- Create `crates/cli/src/jobs/metrics.rs` — pure functions: equity curve, drawdown, CAGR, Sharpe, Sortino, monthly returns, win-rate, profit-factor (all `#[cfg(test)]`-covered, no IO).
- Create `crates/cli/src/jobs/result.rs` — the serialisable result contract structs (`BacktestResultDoc`, `Metrics`, `TradeRow`, …) with `serde`.
- Create `crates/cli/src/jobs/ingest.rs` — the ingest job scaffold + coverage query.
- Modify `crates/cli/src/lib.rs` — extend `Command` enum with `Backtest{…}` / `Ingest{…}`; parse flags; dispatch.
- Modify `crates/cli/src/main.rs` — route the new commands, print progress to stdout, set exit code.
- Create `crates/cli/tests/fixtures/sample_store/` + `crates/cli/tests/fixtures/sample_vintage.json` — committed deterministic fixtures.
- Create `crates/cli/tests/backtest_job.rs` — integration test: fixture vintage + sample store ⇒ golden `result.json`.

---

## Task 1: `backtest` / `ingest` command parsing

**Files:**
- Modify: `crates/cli/src/lib.rs` (the `Command` enum ~155 and `parse_args` ~173)
- Test: `crates/cli/src/lib.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `Command::Backtest { vintage: String, strategy: Option<String>, start: String, end: String, resolution: String, universe: Vec<String>, taker_fee_bps: f64, slippage_model: String, run_dir: PathBuf, json: bool }` and `Command::Ingest { config: PathBuf, start: String, end: String, resolution: String }`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn backtest_parses_required_and_optional_flags() {
    let cmd = parse_args([
        "backtest", "--vintage", "v-2026-07", "--start", "2021-01-01",
        "--end", "2024-12-31", "--resolution", "1h", "--run-dir", "/tmp/r", "--json",
    ]).unwrap();
    assert_eq!(cmd, Command::Backtest {
        vintage: "v-2026-07".into(), strategy: None,
        start: "2021-01-01".into(), end: "2024-12-31".into(),
        resolution: "1h".into(), universe: vec![],
        taker_fee_bps: 2.0, slippage_model: "square-root-impact".into(),
        run_dir: std::path::PathBuf::from("/tmp/r"), json: true,
    });
}

#[test]
fn backtest_requires_vintage() {
    assert!(matches!(parse_args(["backtest", "--start", "2021-01-01"]), Err(CliError::Usage(_))));
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p qe-cli backtest_parses --locked` → FAIL (variant missing).
- [ ] **Step 3: Add the enum variants and parsing** — extend `Command` with `Backtest{…}`/`Ingest{…}` (exact fields above; defaults `taker_fee_bps=2.0`, `slippage_model="square-root-impact"`, `strategy=None`, `universe=vec![]`, `json=false`), and a `"backtest"`/`"ingest"` arm in `parse_args` mirroring the existing `"train"` flag loop; `--vintage` missing ⇒ `CliError::Usage`.
- [ ] **Step 4: Run to verify pass** — `cargo test -p qe-cli --locked` → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat: PreP3 qe-cli backtest/ingest arg parsing"`.

## Task 2: result contract types

**Files:**
- Create: `crates/cli/src/jobs/result.rs`
- Modify: `crates/cli/src/lib.rs` (add `pub mod jobs;` and `jobs/mod.rs` with `pub mod result;`)
- Test: in `result.rs`

**Interfaces:**
- Produces: `BacktestResultDoc { strategy: Strategy, window: Window, universe: Universe, costs: Costs, metrics: Metrics, equity_curve: Vec<f64>, drawdown: Vec<f64>, monthly_returns: Vec<MonthlyRow>, trades: Vec<TradeRow> }` and the nested `serde`-derived structs, matching spec §8.1 field names exactly (`cagr, sharpe, sortino, max_dd, win_rate, profit_factor`; `TradeRow { id, symbol, side, entry, exit, hold, return_pct, result }`).

- [ ] **Step 1: Write the failing test** — round-trip a hand-built `BacktestResultDoc` through `serde_json` and assert the JSON keys are exactly the contract keys (`serde_json::to_value(...)` then check `["metrics"]["profit_factor"]` etc.).
- [ ] **Step 2: Run to verify it fails** — `cargo test -p qe-cli result::` → FAIL (types missing).
- [ ] **Step 3: Define the structs** with `#[derive(Serialize, Deserialize, PartialEq, Debug)]` and `#[serde(rename_all = "snake_case")]` where needed; field names verbatim from §8.1.
- [ ] **Step 4: Run to verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat: PreP3 backtest result contract types"`.

## Task 3: pure metrics functions

**Files:**
- Create: `crates/cli/src/jobs/metrics.rs`
- Test: in `metrics.rs`

**Interfaces:**
- Consumes: per-bar `returns: &[f64]`, `periods_per_year: f64`, bar `open_time`s for monthly bucketing.
- Produces: `equity_curve(returns) -> Vec<f64>`, `drawdown(equity) -> Vec<f64>`, `cagr(equity, years) -> f64`, `sharpe(returns, ppy) -> f64`, `sortino(returns, ppy) -> f64`, `monthly_returns(returns, times) -> Vec<MonthlyRow>`.

- [ ] **Step 1: Write the failing tests** (deterministic, hand-checked):

```rust
#[test]
fn equity_curve_compounds_from_one() {
    let eq = equity_curve(&[0.10, -0.05]);
    assert!((eq[0] - 1.0).abs() < 1e-12);
    assert!((eq[1] - 1.10).abs() < 1e-12);
    assert!((eq[2] - 1.045).abs() < 1e-12); // 1.10 * 0.95
}
#[test]
fn drawdown_is_zero_at_new_highs_and_negative_below_peak() {
    let dd = drawdown(&equity_curve(&[0.10, -0.05]));
    assert!(dd.iter().all(|d| *d <= 1e-12));
    assert!(dd.last().unwrap() < &-0.03); // below the 1.10 peak
}
#[test]
fn sharpe_zero_variance_is_zero_not_nan() {
    assert_eq!(sharpe(&[0.0, 0.0, 0.0], 8760.0), 0.0);
}
```

- [ ] **Step 2: Run to verify they fail.**
- [ ] **Step 3: Implement the pure functions:**

```rust
pub fn equity_curve(returns: &[f64]) -> Vec<f64> {
    let mut eq = Vec::with_capacity(returns.len() + 1);
    let mut v = 1.0; eq.push(v);
    for r in returns { v *= 1.0 + r; eq.push(v); }
    eq
}
pub fn drawdown(equity: &[f64]) -> Vec<f64> {
    let mut peak = f64::MIN;
    equity.iter().map(|&v| { peak = peak.max(v); (v - peak) / peak }).collect()
}
pub fn sharpe(returns: &[f64], ppy: f64) -> f64 {
    let n = returns.len() as f64;
    if n < 2.0 { return 0.0; }
    let mean = returns.iter().sum::<f64>() / n;
    let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
    if var <= 0.0 { return 0.0; }
    mean / var.sqrt() * ppy.sqrt()
}
```
(Sortino: same but downside-deviation denominator; CAGR: `equity.last()^(1/years) - 1`; `monthly_returns`: bucket `returns[i]` by the calendar month of `times[i]`, compound within a month, group by year → `MonthlyRow{year, months:[f64;12]}`.)

- [ ] **Step 4: Run to verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat: PreP3 backtest metrics (equity/dd/cagr/sharpe/sortino/monthly)"`.

## Task 4: trade-level recording (closes the design gap)

**Files:**
- Modify: `crates/wfo/src/backtest.rs` — add an opt-in trade recorder that emits `Vec<TradeFill>` alongside `BacktestResult` without changing the existing hot-path signature (add `backtest_with_trades(genome, bars, cfg) -> (BacktestResult, Vec<TradeFill>)`; keep `backtest()` delegating to it and discarding trades).
- Create: (types) `qe_wfo::backtest::TradeFill { entry_idx, exit_idx, side, entry_px, exit_px, return_frac }`.
- Modify: `crates/cli/src/jobs/metrics.rs` — `win_rate(&[TradeRow]) -> f64`, `profit_factor(&[TradeRow]) -> f64`.
- Test: `crates/wfo/src/backtest.rs` tests + `metrics.rs` tests.

**Interfaces:**
- Produces: `TradeFill` records (one per closed round-trip), mapped in the job to `TradeRow`. `win_rate` = wins/total; `profit_factor` = Σ gains / |Σ losses| (`f64::INFINITY` when no losses, documented).

- [ ] **Step 1: Write the failing test** — a synthetic genome + bar series with a known single winning round-trip ⇒ `backtest_with_trades` returns exactly one `TradeFill` with `return_frac > 0`, and the aggregate `trades == 1` matches the existing count. `win_rate([win]) == 1.0`; `profit_factor([win_of_+2, loss_of_-1]) == 2.0`.
- [ ] **Step 2: Run to verify they fail.**
- [ ] **Step 3: Implement** the recorder inside the existing simulation loop (record on each flat→position entry and position→flat exit; the loop already tracks these transitions for the `trades` counter — extend, don't rewrite) and the two pure metric fns.
- [ ] **Step 4: Run to verify pass**, and confirm the whole `qe-wfo` suite still passes (`cargo test -p qe-wfo --locked`) — the recorder must not change existing `returns`/`net_pnl`.
- [ ] **Step 5: Commit** — `git commit -m "feat: PreP3 backtester trade-level recording + win-rate/profit-factor"`.

## Task 5: the backtest job (orchestration + progress + artifact)

**Files:**
- Create: `crates/cli/src/jobs/backtest.rs`
- Create: `crates/cli/src/jobs/mod.rs` (`ProgressLine`, `emit_progress(&mut impl Write, …)`, `RunError`)
- Modify: `crates/cli/src/main.rs` (dispatch `Command::Backtest`, stream progress to stdout, exit code)
- Test: `crates/cli/tests/backtest_job.rs` (integration, golden file) + committed fixtures.

**Interfaces:**
- Consumes: everything above + the verified library APIs.
- Produces: `run_backtest(params: &BacktestParams, progress: &mut impl FnMut(u8, &str, &str)) -> Result<BacktestResultDoc, RunError>`, and a `main`-level writer of `result.json` into `--run-dir`.

- [ ] **Step 1: Write the failing integration test:**

```rust
// crates/cli/tests/backtest_job.rs
#[test]
fn backtest_over_fixture_store_matches_golden() {
    let run_dir = tempdir().unwrap();
    let params = fixture_params(run_dir.path());       // points at tests/fixtures/*
    let doc = qe_cli::jobs::backtest::run_backtest(&params, &mut |_,_,_| {}).unwrap();
    let got = serde_json::to_value(&doc).unwrap();
    let want: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/golden_result.json")).unwrap();
    assert_eq!(got, want);   // deterministic ⇒ exact match
}
```

- [ ] **Step 2: Build the fixtures** — a tiny committed `MarketStore` (a handful of bars for 1–2 instruments over a short window) and a `sample_vintage.json` (one simple deterministic genome). Generate `golden_result.json` once from the implementation, eyeball it, commit it.
- [ ] **Step 3: Run to verify it fails** — FAIL (module missing).
- [ ] **Step 4: Implement `run_backtest`:** open the store (`MarketStore::open`), load+`verify()` the vintage, for each chromosome `scan_bars(instrument, resolution, from, to)` → `backtest_with_trades` → weight-aggregate per-bar returns by `weights`, map trades → `TradeRow`, call the Task-3/4 metrics, assemble `BacktestResultDoc`. Emit progress at `load`(10) / `scan`(30) / `simulate`(70) / `report`(95) / done(100). Parse `--start`/`--end` to `Timestamp` (reuse `qe-domain`/`qe-config` date parsing; `Usage` error on bad dates).
- [ ] **Step 5: Wire `main.rs`** to call it, print each progress as a JSON line, write `result.json`, print terminal `{"t":"done",...}`, exit 0; on `RunError` print `{"t":"error",...}` and exit non-zero.
- [ ] **Step 6: Run to verify pass** — `cargo test -p qe-cli --locked`.
- [ ] **Step 7: Commit** — `git commit -m "feat: PreP3 qe-cli backtest job (progress + result.json)"`.

## Task 6: `ingest` scaffold + coverage helper

**Files:**
- Create: `crates/cli/src/jobs/ingest.rs`
- Modify: `crates/cli/src/main.rs` (dispatch `Command::Ingest`)
- Test: `crates/cli/tests/ingest_job.rs`

**Interfaces:**
- Produces: `run_ingest(params, progress) -> Result<(), RunError>` (populates a `MarketStore` from a `HistoricalSource` seam) and `coverage(store: &MarketStore, instruments) -> Vec<CoverageRow{symbol, resolution, from, to, bars}>` used later by the server's read-only Market-data view.

- [ ] **Step 1: Write the failing test** — `coverage()` over the committed sample store returns the expected symbol/range/bar-count rows (deterministic).
- [ ] **Step 2: Run to verify it fails.**
- [ ] **Step 3: Implement `coverage`** (scan each instrument/resolution, report min/max `open_time` + count) and `run_ingest` against the **injectable `HistoricalSource`** seam the bootstrap already defines (real Binance decoders stay behind the default-off `http` feature — out of scope here; `run_ingest` is exercised in tests with an in-memory source, and the sample store fixture is what backtests use).
- [ ] **Step 4: Run to verify pass.**
- [ ] **Step 5: Full green gate** (all five commands from Global Constraints) and **Commit** — `git commit -m "feat: PreP3 qe-cli ingest scaffold + coverage query"`.

---

## Follow-on specs (own plans — outlined only)

- **Spec 2 — `qe-server`** (axum+tokio): run store (`data/runs/`), run lifecycle API + subprocess supervision of these jobs, Google OAuth + `QE_ADMIN_ALLOWED_EMAILS` + signed cookie, vintages + coverage read APIs, static-SPA serving. New crate; must not depend on `qe-runtime`; firewall test extended to assert that.
- **Spec 3 — React SPA** (Vite): port the Claude Design tokens + primitives + `AppShell`; screens Login / Backtests list / New backtest / Backtest result / Market-data coverage; polling `GET /api/runs/:id`.
- **Spec 4 — training monitor** (fast-follow): wire `qe-cli train` into a real WFO search job with rich progress (generations, MAP-Elites archive coverage via `qe_wfo::regularise::coverage`, CV folds, G1 gate) + the net-new training-monitor screen.

## Self-review

- **Spec coverage (spec §5, §8):** `backtest` job ✓ (T1,T5), result contract ✓ (T2, §8.1), progress protocol ✓ (T5, Global Constraints), `ingest` + coverage ✓ (T6), metric provenance gap ✓ handled (T3 computes equity/dd/cagr/sharpe/sortino/monthly; T4 adds trades + win-rate/profit-factor — nothing invented in the UI). Spec §6/§7 (server/SPA) are separate plans, noted. ✓
- **Placeholder scan:** none — every code step shows real code or names a verified API; the one true unknown (real Binance decoders) is explicitly out of scope behind `http`, not a hidden TODO. ✓
- **Type consistency:** `BacktestResultDoc`/`Metrics`/`TradeRow` field names match §8.1 across T2/T3/T5; `TradeFill` (T4) → `TradeRow` (T2) mapping is explicit; `backtest_with_trades` reused by T5. ✓
