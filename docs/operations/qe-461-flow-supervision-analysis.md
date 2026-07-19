# QE-461 — Flow supervision: evidence note (concurrency lane + resume/checkpoint/halt)

*Written before coding. Real file:line anchors on `main` @ `c05218b` (QE-460 merged).*

## Goal

Give the multi-hour composite flow (`RunSpec::Flow`, QE-460) three supervision affordances, each **mirroring an
existing evolve/QE-407 mechanism** — never a new one:

1. a **dedicated concurrency lane** (`QE_SERVER_MAX_FLOW_CONCURRENCY` semaphore, default 1) + a per-flow
   wall-clock deadline;
2. **resume from the sealed-vintage checkpoint** — an orphaned flow that sealed its vintage but did not finish the
   backtest re-spawns **only** the backtest phase on restart (no re-search);
3. **authorised halt** — `POST /api/runs/{id}/halt` → `Failed` + a halt reason (no 5th `RunStatus` variant).

All riding the terminal 4-state `RunStatus`; the seal predicate / `evaluate_g1` are untouched.

## Current-state map (real anchors)

| Concern | Location |
|---|---|
| `RunStatus` (4-state, `queued→running→succeeded\|failed`) | `crates/server/src/runs/model.rs:16` |
| `FlowProgress` (`train_run`/`backtest_run`/`vintage`/`holdout_*`) recorded on `meta.flow` | `crates/server/src/runs/model.rs:214` |
| `TrainProgress.vintage` (the recorded `train.vintage`, set on train `done`) | `crates/server/src/runs/model.rs:200`; written in `drain_stdout` at `manager.rs:1166` |
| `supervise_flow` (train→handoff→cost-parity→backtest sequencing) | `crates/server/src/runs/manager.rs:830` |
| `supervise` (acquires pool permit; evolve-permit branch; deadline-wrapped drain) | `manager.rs:582`; evolve-permit block `manager.rs:604`; flow delegate `manager.rs:628` |
| **Evolve semaphore** (`evolve_permits`, `DEFAULT_MAX_EVOLVE_CONCURRENCY=1`, `with_evolve_concurrency`) | `manager.rs:86,97,122,133`; acquired `manager.rs:604` |
| **Per-run deadline** (`run_deadline`, `DEFAULT_MAX_RUN_SECS`, `with_run_deadline`, `RUN_DEADLINE_REASON`) | `manager.rs:76,81,99,140`; `tokio::time::timeout(...) → start_kill()` in `drain_child` `manager.rs:813` and `supervise` `manager.rs:656` |
| `kill_on_drop(true)` on every child | `spawn.rs:113` |
| **QE-407 startup reconciler** (`reconcile_orphans`, `RECONCILE_REASON`) — fails non-terminal orphans | `manager.rs:253`; reason `manager.rs:66`; called `main.rs:46` |
| **Supervised-task registry** (`registry: HashMap<id, JoinHandle>`, insert-before-remove) | `manager.rs:104`; insert `manager.rs:226-240`; self-deregister `manager.rs:238` |
| **Halt** (`RunManager::halt`, `HaltOutcome`, `HALT_REASON`, `mark_halted`) — reuses registry.abort + kill_on_drop | `manager.rs:307,40,36,328` |
| **Halt HTTP arm** `POST /api/runs/{id}/halt` (operator-role gated; run-type-agnostic → already covers flows) | `pools.rs:240` (route), `pools.rs:912` (handler), `require_operator` `pools.rs:243` |
| Env resolution (`ENV_MAX_EVOLVE_CONCURRENCY`, `ENV_MAX_RUN_SECS`; `run_manager()` wiring) | `lib.rs:221,225,328-342` |
| Spawn seam (`JobSpawner::spawn` + `spawn_flow_train`; `--flow` marker) | `spawn.rs:24,34,92` |
| Train→backtest handoff parse (`read_train_handoff`, `flow_backtest_params`, `flow_cost_parity_ok`) | `manager.rs:783,734,760` |

Key finding: **the halt HTTP arm already exists and is run-type-agnostic** (`pools.rs:912` calls
`manager.halt(&id)` on any id). No new halt route/kill-path is needed for flows — only tests proving it behaves
correctly on a flow. This is the intended "mirror the evolve halt" reuse.

## Implementation decisions

### 1. Concurrency lane — mirror the evolve semaphore verbatim
- New `flow_permits: Arc<Semaphore>` field + `DEFAULT_MAX_FLOW_CONCURRENCY: usize = 1` + builder
  `with_flow_concurrency` — a byte-for-byte mirror of `evolve_permits`/`with_evolve_concurrency`.
- `supervise` acquires a flow permit (held for the whole sequence) for `RunSpec::Flow(_)`, in addition to the
  shared pool permit — exactly the evolve pattern at `manager.rs:604`.
- New `ENV_MAX_FLOW_CONCURRENCY = "QE_SERVER_MAX_FLOW_CONCURRENCY"`; `run_manager()` resolves it
  (invalid → default, fail-safe) and calls `.with_flow_concurrency(..)` alongside the evolve/deadline wiring.

### 2. Per-flow wall-clock deadline
`supervise_flow` currently applies `run_deadline` per sub-child. Make it a true **per-flow** ceiling: capture
`flow_start = Instant::now()` at the top and pass each `drain_child` the *remaining* budget
`run_deadline.saturating_sub(flow_start.elapsed())` (zero ⇒ immediate timeout). Same abort→`start_kill`→
`RUN_DEADLINE_REASON` pattern (`manager.rs:813`), now bounding the whole flow rather than each phase.

### 3. Resume predicate (specified) + backtest-only re-spawn
Pure classifier `classify_orphan(store, meta) -> OrphanAction { Fail | ResumeBacktest }`:

> **ResumeBacktest** iff **all** hold:
> - `meta.run_type == "flow"`;
> - **vintage sealed**: `meta.train.vintage.is_some()` **and** `<run>/train/result.json` exists (the durable,
>   content-addressed checkpoint + its readable handoff);
> - **backtest incomplete**: `<run>/backtest/result.json` does **not** exist.
>
> Everything else (non-flow orphan; flow with no sealed vintage / no readable train handoff; flow whose backtest
> already produced `result.json`) ⇒ **Fail** (dead run, terminally marked, **never** re-searched).

- `reconcile_orphans` (unchanged for non-flow) now **skips** `ResumeBacktest` flows (does not fail them); every
  other non-terminal orphan is failed with `RECONCILE_REASON` as before.
- New async `resume_orphaned_flows(&self) -> io::Result<usize>`: scans the index, and for each `ResumeBacktest`
  flow spawns a registry-tracked supervisor (`spawn_flow_backtest_resume`) that runs **only** the backtest phase.
- The backtest phase of `supervise_flow` is factored into a shared `flow_backtest_phase(...)` used by both the
  initial flow and the resume — the resumed backtest rebuilds params from the **recorded** vintage + `train`
  handoff (frozen holdout window + gate taker fee), so it rides the **same** sealed checkpoint deterministically.
  It re-runs the cost-parity guard. It **never** calls `spawn_flow_train` — no search re-runs.
- `main.rs` calls `reconcile_orphans()` (dead orphans) then `resume_orphaned_flows().await` (resumable flows),
  before serving. A resumed flow rides `Running`; it never introduces a new status.

### 4. Halt — reuse `Failed` + `HALT_REASON`
No code change to the halt path or route (already generic). A flow halted mid-run: `manager.halt` pulls its
supervisor handle, `abort()`s it (drops the in-flight `Child` → `kill_on_drop`), then `mark_halted` records
terminal `Failed` + `HALT_REASON`. Any vintage the train phase already sealed is content-addressed + durable, so
it is retained and auditable; the flow run dir is never purged. A halted (terminal) flow is **not** eligible for
resume (reconciler only touches `Queued`/`Running`), so halt is authoritative.

### 5. No status leak
`RunStatus` stays 4-state. No change to the seal predicate, `evaluate_g1`, `PROTOCOL_VERSION` (=3),
`VINTAGE_FORMAT_VERSION`. Resume/halt only ever write statuses in `{queued,running,succeeded,failed}`.

## Test plan (per AC)

- **Lane**: `the_flow_semaphore_serialises_flows` — 2 flows, `max_flow=1`, shared pool ≥2 ⇒ second stays `queued`.
- **Deadline**: `a_flow_past_the_wall_clock_ceiling_is_aborted_and_failed` — train hangs ⇒ `RUN_DEADLINE_REASON`.
- **Resume (positive)**: `flow_resumes_backtest_from_sealed_vintage_checkpoint_without_research` — seed an
  orphaned flow (train/result.json + `train.vintage` present, no backtest/result.json); a spawner script that
  touches a `train_ran` sentinel if invoked with `train`; `resume_orphaned_flows()` ⇒ flow `succeeded`, backtest
  ran, **sentinel absent** (no re-search), backtest pinned to the recorded vintage (determinism).
- **Resume predicate (unit)**: `classify_orphan_*` — sealed∧incomplete ⇒ ResumeBacktest; unsealed ⇒ Fail;
  backtest-complete ⇒ Fail; non-flow ⇒ Fail.
- **Dead run**: `reconcile_fails_unsealed_orphan_flow_never_researched` — orphan flow, no sealed vintage ⇒
  `reconcile_orphans` fails it (`RECONCILE_REASON`), `resume_orphaned_flows` resumes 0, train never invoked.
- **Halt**: `halt_flow_yields_failed_with_halt_reason_and_retains_partial_vintage` (pools.rs) — halt a running
  flow after it sealed its vintage ⇒ `Failed` + halt reason, `train/result.json` retained, re-halt ⇒ 409.
- **No status leak**: `flow_supervision_adds_no_run_status_variant` — exhaustive match asserts `RunStatus` has
  exactly the 4 variants; resumed/halted metas only ever carry those.

## Risks

- **Determinism of resume** (design risk #6): mitigated — resume reads the recorded vintage + the `train`
  handoff (frozen holdout carved once, recorded); it rebuilds identical backtest params and re-runs the
  cost-parity guard. No seed/holdout re-derivation.
- **Double-supervision race**: `reconcile_orphans` (sync, fails dead) runs before `resume_orphaned_flows`
  (async, spawns backtests) — both before `axum::serve`, so no HTTP create can interleave; the registry
  insert-before-remove invariant is preserved by mirroring `create`.
- **Default flow deadline**: reuses the existing `run_deadline` (`DEFAULT_MAX_RUN_SECS` ~24h). A flow is
  train+backtest, plausibly longer than a single evolve; 24h is a conservative shared default and is overridable
  via `QE_SERVER_MAX_RUN_SECS`. **Flagged**: no separate flow-specific deadline knob is introduced (kept scope
  tight; reuse over new mechanism per the ticket). If ops want a distinct flow ceiling, that is a follow-up.
- **Firewall**: all changes stay within `qe-server` (no new cross-crate edge); handoff still parsed as opaque
  `serde_json::Value`. Dependency-topology test stays green.
