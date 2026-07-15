# QE-407 — Server run-lifecycle robustness: graceful shutdown, supervised-task registry, honest success

**Ticket spec of record:** `docs/reviews/2026-07-15-team-improvement-review.md` → `### QE-407`.
**Phase:** PreP3 · **Area:** backend / orchestration · **Extends:** QE-263 (orphan reconciliation, not
yet landed — this ticket delivers the reconciler the AC requires).

## 1. Current-state evidence

- **No graceful shutdown / no signal handling.** `crates/server/src/main.rs:81` calls
  `axum::serve(listener, router).await` with **no** `.with_graceful_shutdown(...)` and there is no
  `tokio::signal` handler anywhere in the workspace (`grep -rn 'ctrl_c\|SignalKind\|with_graceful_shutdown'
  crates` → empty). On SIGTERM/SIGINT the process dies immediately.
- **`kill_on_drop(true)` on the child.** `crates/server/src/runs/spawn.rs:59` sets `kill_on_drop(true)`,
  so when the process dies every detached supervisor task is dropped and its child is SIGKILLed
  mid-run — while `meta.json` still says `running`.
- **No registry of in-flight runs.** `crates/server/src/runs/manager.rs:112` fires the supervisor via a
  bare `tokio::spawn(...)` whose `JoinHandle` is **discarded**. `RunManager` (manager.rs:40) holds
  `store` / `spawner` / `permits` / `index_lock` but no handle map — it cannot count, drain, or cancel
  in-flight runs at shutdown. This is the root enabler of the QE-263 orphan (`running` forever).
- **Dishonest success.** `manager.rs:235-253` marks a run `Succeeded` whenever `done_seen && exit==0`
  even when `result.json` was never written (`artifacts` left empty). The in-code
  `TODO(QE-follow-up)` at manager.rs:236 flags exactly this: `GET /runs/{id}/result` then returns `409`
  (`api.rs:93-99`) on a run the UI shows green.
- **No startup reconciler.** No code reads `index.json` at boot to fail orphaned `running`/`queued`
  runs; a hard-killed run's `meta.json` stays `running` across restarts forever.

Callers of `RunManager::new` (signature must stay stable): `lib.rs:272 run_manager()` and the test
helper `crates/server/tests/runs.rs:47 app_with_script`.

## 2. Implementation decisions

All changes are confined to `crates/server` (+ the workspace `tokio` `signal` feature). No new crate
dependency ⇒ no firewall edge, no `cargo deny` surface change (`signal-hook-registry` is already in
`Cargo.lock`).

### 2a. Supervised-task registry (`manager.rs`)
- Add two fields to `RunManager`: `registry: Arc<Mutex<HashMap<String, JoinHandle<()>>>>` and
  `accepting: Arc<AtomicBool>` (initialised `true`). `RunManager::new` signature is **unchanged**.
- `create` registers the supervisor's `JoinHandle` under the run id. The insert is done **while holding
  the registry lock across the `tokio::spawn`**, and the task self-deregisters (`registry.lock().remove(id)`)
  only at the very end of `supervise`; because self-removal also needs that lock, insert is guaranteed to
  happen before remove even in the pathological instant-finish case — so the map never leaks a live entry
  and never double-removes.
- `create` first checks `accepting`; if `false` it returns a new `CreateError::ShuttingDown` (→ HTTP 503).

### 2b. Graceful shutdown drain (`manager.rs` + `main.rs`)
- New `RunManager::shutdown(drain: Duration)`:
  1. `accepting = false` (stop taking new runs);
  2. `permits.close()` — wakes any **queued** supervisor blocked on `acquire()`; the existing
     `Err` arm (manager.rs:202) already fails those cleanly ("worker pool closed");
  3. drain the registry: `await` each handle up to a shared deadline. Handles that finish within the
     window terminate their run normally (terminal `meta.json` written by `supervise`). Any handle still
     live at the deadline is `abort()`ed — dropping its `Child` fires `kill_on_drop` — then the run is
     terminally marked `failed` ("run did not finish before server shutdown").
- `main.rs`: build a `shutdown_signal()` future (`tokio::signal::ctrl_c()` ⨁ unix `SIGTERM`), pass it to
  `axum::serve(...).with_graceful_shutdown(...)`; after `serve` returns (listener stopped, in-flight HTTP
  requests drained) call `manager.shutdown(DEFAULT_SHUTDOWN_DRAIN)` and return `ExitCode::SUCCESS`.
  Signal-handler install failures degrade gracefully (fall back to the other signal / `pending`) — no
  `unwrap`/`expect` (workspace denies `unwrap_used`).

### 2c. Startup reconciler (`manager.rs` + `main.rs`) — the QE-263 pairing
- New `RunManager::reconcile_orphans() -> io::Result<usize>`: read `index.json`, and for every run whose
  `meta.json` is still `Queued`/`Running` (no live supervisor can exist in a fresh process) mark it
  `failed` ("run was interrupted by a server restart (no live supervisor)"). Returns the count.
- `main.rs` calls it once, right after building the manager, before binding the listener.

### 2d. Honest success (`manager.rs`)
- Replace the `TODO(QE-follow-up)` block: after `done_seen && exit==0`, if `store.result_path(&id)`
  exists → `Succeeded` (+ `artifacts=["result.json"]`, + the train 100% progress as today); otherwise
  `finish_failed(... "job reported done but wrote no result.json")`. The happy path is unchanged.

## 3. Test plan (each AC → a non-vacuous test, in `crates/server/tests/runs.rs`)

- **AC1 hard-kill reconcile:** pre-seed a run store with a `running` `meta.json` + matching `index.json`
  (a crashed-server snapshot), build a `RunManager` over it, call `reconcile_orphans()`, assert the run
  is now `failed` with the restart reason and the returned count is 1. A control `succeeded` run is left
  untouched (proves the reconciler is not vacuous / does not clobber terminal runs).
- **AC1 clean shutdown drain:** create a run whose fake job blocks in `running`; poll to `running`; call
  `manager.shutdown(short)`; assert no `running` meta remains — the run is `failed` with the shutdown
  reason — and that a subsequent `create` is refused (`ShuttingDown`). Proves drain terminally-marks and
  the listener/manager stops accepting.
- **AC2 dishonest success:** fake job prints a progress line + `done`, exits 0, writes **no**
  `result.json` ⇒ assert `failed` with "job reported done but wrote no result.json", and `GET /result`
  is no longer a green-run 409. The existing
  `run_transitions_running_then_succeeds_and_serves_result` covers the happy path stays `succeeded`.

Test seam: add a `app_and_manager_with_script` helper returning `(Router, Arc<RunManager>)` so tests can
drive HTTP **and** call `shutdown`/`reconcile_orphans` on the same manager.

## 4. Risks / blast radius

- **Scope:** `crates/server` only + one additive `tokio` feature. Firewall unaffected (no new crate
  edge). No golden/vintage content touched ⇒ goldens must remain byte-identical.
- **Behaviour change:** a job that emits `done`/exits 0 but writes no `result.json` now reads `failed`
  instead of `succeeded`. This is the intended correction; the real `qe-cli` writes `result.json` before
  `done`, and all three existing hermetic job scripts do too, so no existing test regresses.
- **Abort vs. write race:** on drain timeout we `abort()` then `await` the handle to completion before
  writing the terminal `meta.json`, so the cancelled task cannot interleave a later write; atomic writes
  make any residual overlap last-writer-wins and never partial.
- **Cross-domain (frontend copy):** the new failure reason surfaces through the existing `meta.error`
  field the SPA already renders, so no frontend change is required for correctness; a nicer copy string
  is a follow-up (noted in the PR), out of scope here.

## 5. Green gate

`cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all`,
`cargo deny check`, and the `crates/architecture` firewall test must all be green on the committed SHA.
