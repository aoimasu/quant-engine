# QE-406 — Single-source the CLI ↔ server ↔ SPA run protocol

**Ticket:** QE-406 (tri-team: backend + architecture + frontend). Spec:
`docs/reviews/2026-07-15-team-improvement-review.md` § `### QE-406`.

**Goal.** Extract the JSON-line run protocol (progress lines + run-param DTOs) into a
dependency-free leaf crate `qe-run-protocol`; delete the two duplicated Rust definitions; add a
`protocol_version` the server checks; add a CI agreement test; and model the SPA `RunMeta` as a
discriminated union on `type` so a train run can no longer be statically mistyped.

---

## 1. Current state — three copies of one contract (file:line)

| Copy | Location | Shape | Role |
|------|----------|-------|------|
| **Emit** | `crates/cli/src/jobs/mod.rs:22` (`qe_cli::jobs::ProgressLine`) | `Serialize`; floats **required** `f64` | CLI writes JSON lines on stdout |
| **Parse** | `crates/server/src/runs/manager.rs:197` (`qe_server::runs::manager::ProgressLine`) | `Deserialize`; floats `Option<f64>` + `#[serde(default)]`; **no** `stage` field | server tails child stdout |
| **SPA** | `web/src/api/runs.ts:88` (`RunMeta`) | `type: string`, `params: BacktestParams` (always) | SPA renders runs |

Supporting duplication:
- **Run-param DTOs** (wire form: POST body → `meta.params`): `crates/server/src/runs/model.rs`
  — `BacktestParams` (`:57`, `Default` at `:84`, defaults `default_taker_fee_bps` `:40` /
  `default_slippage_model` `:45`) and `TrainParams` (`:108`). These are the wire DTOs.
- The CLI's own `BacktestParams` (`crates/cli/src/jobs/backtest.rs:29`) and `TrainParams`
  (`crates/cli/src/jobs/train.rs:60`) are **domain-typed** internal structs (`PathBuf`, `Lineage`,
  `usize`) — *not* wire types. They are intentionally **left in place** (different purpose); only the
  server's wire DTOs move to the shared crate.

**The problem:** an unversioned contract with no agreement test. A field rename / new tag breaks live
monitoring and the SPA with zero compile-time or CI signal. A `qe-server → qe-cli` dep would be the
obvious fix but is a firewall breach (it would pull the whole training tree onto the server).

## 2. Leaf-crate design

New crate `crates/run-protocol` (`qe-run-protocol`):
- **Deps: `serde` + `serde_json` only.** No `qe-*` deps → pure leaf, firewall-neutral.
- Holds:
  - `pub const PROTOCOL_VERSION: u32` — the wire-contract version.
  - `pub enum ProgressLine` (both `Serialize` + `Deserialize`) — the single progress-line type.
  - `emit_progress` / `emit_done` / `emit_train_done` / `emit_error` — the byte-exact writers
    (moved from `cli/jobs/mod.rs`; the CLI re-exports them so `qe_cli::jobs::emit_*` still resolves).
  - `BacktestParams` / `TrainParams` wire DTOs + their `Default`/default-fns (moved from server
    `model.rs`; the server re-exports them via `pub use`).
- `qe-cli` and `qe-server` both depend on it. `cli/jobs/mod.rs` and `server/runs/model.rs` re-export
  the shared types, so **no downstream import paths change**.

### Unifying the two `ProgressLine` copies
The one shared type must satisfy the CLI's serializer **and** the server's tolerant deserializer:
- **Float fields become `Option<f64>` + `#[serde(default)]`** (the server's tolerance). `serde_json`
  already renders a non-finite `f64` (e.g. `-inf` best-so-far) as `null` on **serialize**; a required
  `f64` would **fail** to deserialize that `null`. `Option<f64>` round-trips it as `None`. The CLI
  wraps its finite `f64`s in `Some(..)` at the emit sites — `Some(finite)` serializes to the same
  number, `Some(-inf)`/`None` both serialize to `null`, so the **emitted bytes are unchanged**.
- **`stage` is kept** on `Gen`/`Ensemble`/`Gate` (the CLI emits it; the server previously ignored it
  as an unknown field). Keeping it preserves the emitted bytes; the server destructures `stage: _`.
- Field **declaration order mirrors the CLI's** (the emitter defines the wire order) so serialization
  is byte-for-byte identical.

## 3. Wire-format-preservation proof
- **Progress/Gen/Ensemble/Gate/Error:** identical field set + order + tags (`t`, snake_case). Floats
  emit identically (`Some(x)` ⇒ `x`; non-finite ⇒ `null`). `stage` still emitted. → **byte-identical.**
- **Done:** gains `protocol_version` (see §5). New emitted shape:
  `{"t":"done","result":"result.json","protocol_version":1}` (+ `"vintage":…` for train). This is the
  **only** intentional wire change; both sides + the affected tests are updated. Old/legacy `done`
  lines **without** `protocol_version` still parse (`#[serde(default)]` ⇒ `0`), so the server's fake-job
  integration fixtures (`crates/server/tests/runs.rs`) keep working unchanged.
- **Guarded by** the new agreement test (golden wire strings) + the unchanged
  `crates/server/tests/runs.rs` (parses fixture lines) + `crates/cli/tests/*` (result artefacts).

## 4. Firewall reasoning
- `qe-run-protocol` has **no `qe-*` deps** → adds no edge to any reachability set.
- Rules (`crates/architecture/src/lib.rs:202`): `qe-wfo ⟂ {ensemble,runtime,venue}`,
  `qe-ensemble ⟂ {wfo,runtime,venue}`, `qe-server ⟂ {runtime,venue}`. None reference the leaf; the
  leaf reaches nothing → **no new violation**, **no rule change needed**.
- `qe-server → qe-run-protocol` is server → (pure serde leaf): does **not** reach `qe-runtime`/
  `qe-venue`. `qe-cli → qe-run-protocol` is unconstrained (cli is a composition root).
- Verified green: `cargo test -p qe-architecture --test firewall --locked` and the cli
  `dependency_topology` guard (`cargo metadata`-based) both stay green (leaf adds no forbidden edge).

## 5. `protocol_version` behavior (least-disruptive correct)
- `PROTOCOL_VERSION: u32 = 1` lives in the shared crate.
- Carried on the **terminal `done` line** (`Done.protocol_version`), always emitted by the CLI.
- Server (`manager::drain_stdout`, `Done` arm) checks it: on mismatch it **logs a `tracing::warn`
  and continues** (never rejects). Rationale: rejecting a terminal line would drop a completed run's
  outcome and regress live monitoring; a warning gives the operability signal without behavior loss.
  A legacy `done` (no field) deserializes to `0 != 1` → warns, still recognised as terminal.

## 6. Frontend union mapping (hand-mirrored from the crate)
`web/src/api/runs.ts` — `RunMeta` becomes a discriminated union on `type`:
```
RunMetaBase { id, status, progress, created_ms, started_ms, finished_ms, exit, error, artifacts }
BacktestRunMeta = RunMetaBase & { type: 'backtest'; params: BacktestParams }
TrainRunMeta    = RunMetaBase & { type: 'train';    params: TrainParams; train?: TrainProgress }
RunMeta = BacktestRunMeta | TrainRunMeta
```
Mirrors the Rust wire DTOs (`qe_run_protocol::{BacktestParams, TrainParams}`) + `RunMeta.type`
(`backtest`/`train`); a comment marks the crate as the source of truth. Narrowed consumers:
- `BacktestsList` — vintage column narrows `row.type === 'backtest'`; window/resolution use the common
  `params.start/end/resolution`.
- `TrainingList` — filters with a `run is TrainRunMeta` type-predicate so `row.train` / `TrainParams`
  narrow without casts.
- `BacktestResult` — narrows `meta.type === 'backtest'` before `params.vintage/universe/...` and before
  `createRun(meta.params)` (which needs `BacktestParams`).
- `TrainingMonitor` — narrows `meta.type === 'train'` before reading `meta.train`.
- Tests (`BacktestResult`/`BacktestsList`/`TrainingMonitor`) updated to the union fixtures.

AC check: accessing a backtest-only field (`params.vintage`) on a narrowed `train` run is now a TS
compile error.

## 7. Test plan
- **New agreement test** (`crates/run-protocol/tests/agreement.rs`): golden wire strings for every
  variant (emit ⇒ exact JSON; exact JSON ⇒ parse), the non-finite-float `null` tolerance, and the
  `protocol_version` default. A field rename breaks the golden assertion → **CI red**.
- `cargo test --workspace` — server `runs` tests + cli job tests unchanged-green.
- `cargo test -p qe-architecture --test firewall` — green.
- Frontend: `npm run lint && npm run build && npm test` — green (union typechecks + narrows).

## 8. Risks / rollback
- **Risk:** the `done` line gains a field. Mitigated: additive, defaulted on parse, server only warns,
  fixtures unchanged. **Rollback:** revert the branch (single crate + re-exports; no data migration —
  `meta.json` shapes unchanged).
- **Risk:** TS union churn at consumers. Mitigated: narrowing is local; tests cover each screen.
</invoke>
