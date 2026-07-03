# QE-255 — Run store + run lifecycle API + subprocess supervision (design / evidence)

`Phase: PreP3` · `Area: backend / orchestration` · `Depends on: QE-251, QE-254`

Spec refs: admin-ui spec §5.2/§5.3 (CLI contract), §6.1 (run store), §6.2 (API), ADR **D4b** (file
store), **D4c** (subprocess supervision). Builds on the QE-254 `qe-server` scaffold.

## 1. Current-state evidence

### QE-254 router / config surface (build ON this)
- `crates/server/src/lib.rs`: `build_router(static_dir: &Path) -> Router` nests a `/api` sub-router
  (`/api/health`) with its own `api_not_found` fallback, and serves the SPA via `ServeDir`+`ServeFile`
  under `fallback_service`. `ServerConfig { addr, static_dir }` + `from_env()` reads `QE_SERVER_ADDR`
  / `QE_SERVER_STATIC_DIR` (QE_-prefixed, 12-factor, mirroring `qe-config`). `ConfigError::BadAddr`.
- `crates/server/src/main.rs`: `#[tokio::main]`; resolves config, `build_router`, binds, `axum::serve`.
- Firewall guards that MUST stay green after this ticket:
  - `crates/architecture/tests/firewall.rs` — manifest-graph reachability; `qe-server` is enumerated
    and asserted to reach neither `qe-runtime` nor `qe-venue`.
  - `crates/cli/tests/dependency_topology.rs` — `cargo metadata` transitive closure;
    `assert_no_dependency(qe-server, qe-runtime)` + `(qe-server, qe-venue)`.
  - **Consequence:** `qe-server` must NOT gain any edge to `qe-runtime`/`qe-venue`. We add only `uuid`
    (third-party) + tokio features + `qe-config` (already a training-side/shared crate, allowed). We do
    **not** add `qe-cli` as a dependency (it would risk pulling forbidden edges and is unnecessary — we
    spawn its built binary as a subprocess, we don't link it).

### QE-251 `qe-cli backtest` contract (the subprocess we supervise)
From `crates/cli/src/lib.rs` (arg parse), `crates/cli/src/main.rs` (dispatch), `crates/cli/src/jobs/`:
- Invocation: `qe backtest --vintage <id> [--strategy <sel>] --start <YYYY-MM-DD> --end <YYYY-MM-DD>
  --resolution <r> [--universe <csv>] [--taker-fee-bps <f>] [--slippage-model <s>] --run-dir <dir>
  [--json]`. `--vintage` required; `--taker-fee-bps` default 2.0; `--slippage-model` default
  `square-root-impact`; `--universe` accepts comma- or repeat-separated symbols.
- With `--json`: emits one JSON object per line on **stdout**:
  `{"t":"progress","pct":<0..=100>,"stage":"load|scan|features|simulate|report","msg":"…"}` then a
  terminal `{"t":"done","result":"result.json"}` (exit 0) or `{"t":"error","msg":"…"}` (exit non-zero).
- On success writes `result.json` (pretty JSON, §8.1 contract) into `--run-dir`; exit code = success/fail.
- The store path + vintage repo root come from the CLI's own config (`QE_CONFIG`/`config.toml`,
  `runtime-sim` profile) — the server does not pass them; it only passes the backtest params above.

## 2. Decisions

### D-1 Run id scheme — **UUID v4 (opaque, random)**
Run ids are identifiers, not deterministic outputs, so randomness is acceptable (the ticket explicitly
blesses uuid-like ids). UUID v4 is collision-free across restarts (no counter to reset), carries **no
wall-clock** (pure random, satisfying spec §6.1 "no wall-clock in the id itself"), and needs only the
already-in-lock `uuid` crate with the `v4` feature (pulls `getrandom`, already present; MIT/Apache —
`cargo deny` stays green). Ids render as the canonical hyphenated 36-char form.

### D-2 File store layout (ADR D4b) — under the configurable state dir
```
<data_dir>/runs/
  index.json          # discovery/order index: [{ id, type, created_ms, label }], newest appended last
  <run_id>/
    meta.json         # authoritative status + progress + params + timestamps + exit + error + artifacts
    result.json       # the §8.1 result contract, present once succeeded (written by the CLI job)
    stdout.log        # captured child stdout (all progress lines)
```
- `data_dir` is configurable via **`QE_SERVER_DATA_DIR`** (default `data`, CWD-relative — never a
  hard-coded absolute path; consistent with the crate's existing `QE_SERVER_*` env prefix and the repo's
  `data/` layout used by `qe-config` `storage.market_dir=data/lmdb/market`, `artifacts_dir=data/artifacts`).
  Spec §6.4 names this `QE_DATA_DIR`; we keep the crate-local `QE_SERVER_` prefix for consistency with
  QE-254 and note the alias for a future reconciliation.
- **Single source of truth = `meta.json`.** `index.json` stores only immutable per-run fields (id, type,
  created_ms, label) for ordering/discovery; status/progress are never duplicated there, so index and
  meta can never diverge. `GET /api/runs` reads `index.json` for the id set + order, then loads each
  `meta.json` for authoritative status/progress.
- **Atomic writes:** every `meta.json` / `index.json` write goes to a sibling temp file + `rename` (same
  dir), so a concurrent reader never sees a partial file.

### D-3 `meta.json` schema
```jsonc
{
  "id": "…uuid…",
  "type": "backtest",
  "status": "queued|running|succeeded|failed",
  "params": { "vintage","strategy?","start","end","resolution",
              "universe":[…],"taker_fee_bps","slippage_model" },
  "progress": { "pct": 0, "stage": "…", "msg": "…" },   // latest tailed progress line
  "created_ms": 0, "started_ms": null, "finished_ms": null,  // meta is operational, wall-clock OK
  "exit": null,                                          // child exit code once finished
  "error": null,                                         // stderr tail on failure
  "artifacts": ["result.json"]                           // present once written
}
```
Timestamps use wall-clock (`SystemTime`) — meta is operational state, **not** the deterministic
`result.json` (which the CLI produces wall-clock-free). This respects the determinism boundary.

### D-4 Worker pool + supervision (ADR D4c)
- Bounded concurrency via a `tokio::sync::Semaphore` with `QE_SERVER_MAX_CONCURRENCY` permits (default 2).
- `POST /api/runs`: validate params → mint id → create run dir + write `meta.json` (`queued`) → append to
  `index.json` → spawn a detached tokio task → return `{ id }` (202-ish; we return 201 with the id).
- The task: `acquire` a permit (runs beyond the cap **block here, observably `queued`**) → transition
  `queued→running` (set `started_ms`) → spawn the child with stdout+stderr piped → **tail stdout line by
  line** (`tokio::io::BufReader::lines`): append raw line to `stdout.log`, parse JSON, and on a
  `progress` line update `meta.progress`; a `done` line is noted → capture a bounded **stderr tail** →
  `await` child exit. Outcome: `done` seen **and** exit 0 ⇒ `succeeded` (+ `artifacts:["result.json"]`);
  otherwise ⇒ `failed` with the stderr tail as `error`. `finished_ms` + `exit` recorded. Permit released
  on task end (drop).
- Crash isolation: a heavy/looping job cannot destabilise the server (separate process); a non-zero exit
  becomes `failed`, never a server error.

### D-5 Binary location + spawn-injection seam (the test crux)
- **Production:** a `JobSpawner` trait; `CliJobSpawner { bin: PathBuf }` builds the `qe backtest … --json`
  command (arg-building lives here) and returns a spawned `tokio::process::Child` (stdout/stderr piped).
  `bin` resolves from **`QE_SERVER_CLI_BIN`**, else defaults to a `qe`/`qe-cli` binary **as a sibling of
  the running `qe-server` executable** (`current_exe().parent()/qe`) — robust for a co-located deploy, no
  global install assumed.
- **Tests:** the seam is the injectable `bin` path. Tests point `CliJobSpawner.bin` at a **generated
  `/bin/sh` script** (written into a `tempfile::TempDir`) that receives the *real* `backtest … --run-dir
  <dir> --json` argv the production spawner builds, parses `--run-dir`, emits known JSON-line progress,
  optionally writes a `result.json`, and exits 0 / non-zero on command. This exercises the real
  arg-building + real subprocess supervision while staying **hermetic** (no global `qe` install, no
  building `qe-cli`, deterministic and fast). For the queueing test the script blocks until a sentinel
  "release" file appears, so slots are held deterministically. (A full trait object is retained so QE-256
  can wrap and future tests can substitute a mock without a subprocess.)
- **QE-256 seam:** routes are added under `/api` on a state-carrying sub-router; they stay open now but
  are structured so QE-256 can layer session middleware over the whole `/api` nest without touching them.

## 3. Test plan (`crates/server/tests/runs.rs`, `#[tokio::test]`)
All tests build the router with a `RunManager` over a `TempDir` data dir + a `CliJobSpawner` pointed at a
generated fake-job script, drive it with `tower::ServiceExt::oneshot`, and **poll with a bounded timeout**
(no fixed sleeps) for status transitions.
1. **Success path:** `POST /api/runs` → poll `GET /api/runs/:id` until `succeeded` (observing `running`
   en route) → `GET /api/runs/:id/result` returns the fake `result.json`. `GET /api/runs` lists it.
2. **Failure path:** fake job writes a stderr line + exits non-zero ⇒ status `failed` and the stderr tail
   appears in `meta.error`; `GET /api/runs/:id/result` ⇒ 409 (succeeded-only).
3. **Bounded pool:** `max_concurrency = N`; submit `N+1` jobs whose script blocks on a release sentinel;
   poll until exactly `N` are `running` and `1` is `queued`; touch the sentinel; poll until all
   `succeeded`.
4. **Validation:** `POST /api/runs` with a missing required field ⇒ 400 (no run created).
5. **404s:** `GET /api/runs/:unknown` ⇒ 404; `GET /api/runs/:unknown/result` ⇒ 404.
Plus unit tests for the store (round-trip meta/index, atomic replace) and `ServerConfig::from_env` of the
new env vars.

## 4. Risks
- **Orphaned `running` runs after a server restart** (the child died with the parent): v1 leaves them
  `running` on disk. Acceptable for a single-node admin tool; a startup reconciler (mark stale `running`
  → `failed`) is a follow-up. Documented, not implemented.
- **`/bin/sh` fake job** assumes a POSIX shell (dev/CI are macOS/Linux). If CI ever runs on Windows this
  needs a portable helper; the injectable seam makes that a localized change.
- **Default `cli_bin` = sibling of `current_exe()`** assumes a co-located deploy; overridable via
  `QE_SERVER_CLI_BIN`. Documented in the code + here.
- **deny:** only `uuid` (v4) added; MIT/Apache, already-present `getrandom`. `cargo deny check` verified
  green. No advisory/ban weakening.
