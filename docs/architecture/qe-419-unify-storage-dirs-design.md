# QE-419 — Unify config: single source of truth for storage dirs across `qe-server` and the spawned CLI

`Phase: PreP3` · `Area: architecture / config` · `Effort: M`

## Problem — current-state evidence of the double configuration

`qe-config` (`crates/config`) is the 12-factor configuration system: figment-layered TOML, a `QE_`-prefixed
env convention split on `__`, validated at load, and the source of the vintage `content_hash`. The
storage directories live in one place there:

```toml
# config.example.toml
[storage]
market_dir = "data/lmdb/market"
artifacts_dir = "data/artifacts"
```

The spawned `qe-cli` reads exactly these. `crates/cli/src/main.rs:169-176` (backtest):

```rust
let config_path = std::env::var("QE_CONFIG").unwrap_or_else(|_| "config.toml".to_owned());
let cfg = Config::load(Profile::RuntimeSim, &PathBuf::from(config_path))?;
// store_path   = cfg.storage.market_dir
// vintage_root = cfg.storage.artifacts_dir / "vintages"
```

But `qe-server` bypasses `qe-config` entirely and re-declares the **same two physical dirs** through a
parallel `QE_SERVER_*` namespace with bespoke parsing (`crates/server/src/lib.rs`):

```rust
pub const DEFAULT_ARTIFACTS_DIR: &str = "data/artifacts";      // == storage.artifacts_dir
pub const DEFAULT_MARKET_DIR: &str    = "data/lmdb/market";    // == storage.market_dir
pub const ENV_ARTIFACTS_DIR: &str     = "QE_SERVER_ARTIFACTS_DIR";
pub const ENV_MARKET_DIR: &str        = "QE_SERVER_MARKET_DIR";
// ServerConfig::from_env() reads those envs into cfg.artifacts_dir / cfg.market_dir,
// and ServerConfig::read_state() opens the MarketStore + VintageRepository from them.
```

So the server's read APIs — `/api/vintages` (`VintageRepository` rooted at `artifacts_dir`) and
`/api/market-data/coverage` (`MarketStore` opened at `market_dir`) — resolve their dirs from
`QE_SERVER_ARTIFACTS_DIR`/`QE_SERVER_MARKET_DIR`, while the `qe-cli` the server **spawns** to run
backtests/coverage resolves them from `config.toml [storage]`. **The same dirs are configured twice
with no cross-check.** A mismatch (e.g. `QE_SERVER_MARKET_DIR=/vol/a` while `config.toml` has
`market_dir = /vol/b`) is silent: training writes one store, the server's coverage endpoint scans the
other, and nothing detects it until a query returns confusingly empty/stale data. The ticket's AC is to
collapse this to **one source of truth** and to detect any residual mismatch **at boot, not at query
time**.

## Chosen approach — (a) load `qe-config` in `qe-server`, plus an explicit config pin for the child

Two options were on the table:

- **(a)** Load `qe-config` for the shared storage dirs in `qe-server` (server-only transport/auth knobs
  stay separate). This makes `config.toml [storage]` the *literal* single source both the server and the
  spawned CLI read.
- **(b)** Pass the server's resolved dirs to the spawned CLI explicitly (new CLI flags). The backtest
  subcommand takes no store/artifacts flags today, so (b) would widen the CLI's flag surface and still
  leave the server resolving dirs from *somewhere*.

**Decision: (a).** It minimises drift and gives a *real* single source — `config.toml [storage]` — that
server and CLI already agree on by construction, because both now call `qe-config`. `qe-config` is a
foundational crate (deps: `qe-domain`, `serde`, `figment`, `sha2`, `thiserror`), so the new
`qe-server → qe-config` edge cannot reach `qe-runtime`/`qe-venue` and is firewall-legal (verified below).

We also take the useful half of (b) to make the unification airtight against CWD/env drift: the server
resolves the config path once (`QE_CONFIG` or `config.toml`, exactly as the CLI does) and **pins that
path onto every spawned child** via `QE_CONFIG` in the child's environment (`CliJobSpawner::with_config_path`).
The child therefore provably reads the same file the server loaded and guarded.

### Server-only knobs stay separate

`addr`, `static_dir`, `data_dir`, `cli_bin`, `max_concurrency` remain in `ServerConfig` with their
`QE_SERVER_*` env convention — they are genuinely server-only transport/lifecycle knobs, not shared
storage. Only the two *shared* dirs move to `qe-config`.

### The `QE_SERVER_ARTIFACTS_DIR` / `QE_SERVER_MARKET_DIR` namespace — deprecated + guarded

Rather than a hard removal (which would silently ignore existing deployments' env), the two storage
envs are **deprecated overrides**: if set, they override the server's read-state dir *and emit a
deprecation `warn!*, and are then **cross-checked at boot against the qe-config value the CLI reads**.
Because the boot guard refuses to proceed on divergence, an override can no longer make the server and
CLI disagree — it is neutered and on its way out. The recommended path is to drop them and use
`config.toml [storage]` (or the unified `QE_STORAGE__MARKET_DIR` / `QE_STORAGE__ARTIFACTS_DIR` figment
env, which overrides *both* the server load and the child load identically). Migration implication:
any deployment currently relying on a `QE_SERVER_*` storage override that differs from `config.toml`
will now **fail to boot** until reconciled — which is exactly the previously-silent bug surfacing.

## Boot-guard design

A pure, unit-testable function (mirroring QE-409's `check_session_secret_policy` /
`should_warn_insecure_cookies`):

```rust
// crates/server/src/config.rs
pub fn check_storage_dirs_match(server: &StorageDirs, cli: &StorageDirs)
    -> Result<(), StorageDirMismatch>;
```

- `StorageDirs { artifacts_dir, market_dir }` is the pair that must agree.
- `cli` = `StorageDirs::from_config(&app_config)` — what the spawned CLI reads from `[storage]`.
- `server` = `server_storage_dirs(&cli)` — `cli` with the deprecated `QE_SERVER_*` overrides applied.
- Equal → `Ok(())`; divergent → `Err(StorageDirMismatch { .. })`.

Wired in `crates/server/src/main.rs` **at boot**, alongside the QE-409 policy checks and *before* the
market store is opened / the listener binds:

1. `resolve_config_path()` → `QE_CONFIG` or `config.toml`.
2. `load_app_config(&path)` (`Profile::RuntimeSim`, matching the CLI backtest). A load/validate error is
   **fatal** (the CLI would fail identically).
3. `cli_dirs = StorageDirs::from_config(&cfg)`; `server_dirs = server_storage_dirs(&cli_dirs)`.
4. `check_storage_dirs_match(&server_dirs, &cli_dirs)` → on `Err`, `tracing::error!` and
   `ExitCode::FAILURE` (**fail-closed refuse to boot**, like the missing-session-secret case — a
   divergent store is a silent-data-integrity risk, so we do not merely warn).
5. Build `ReadState` from `server_dirs`; pin the child via `run_manager(&config_path)`.

**Fail vs warn:** the mismatch guard **fails boot** (refuses to proceed). The *deprecation* of a
`QE_SERVER_*` override is a **warn** (non-fatal) emitted when the override is present.

## Test plan

- **Unit (pure guard):** `check_storage_dirs_match` — identical dirs ⇒ `Ok`; differing `market_dir` ⇒
  `Err`; differing `artifacts_dir` ⇒ `Err`.
- **Unit (unification):** `StorageDirs::from_config` maps `[storage].market_dir`/`artifacts_dir`
  verbatim, so server + CLI resolve the SAME dirs from one source.
- **Unit (override + guard interplay):** with a `QE_SERVER_MARKET_DIR` override diverging from config,
  `server_storage_dirs` yields a `server` that the guard rejects; with no override, `server == cli`.
- **`ServerConfig::from_env`:** updated — the `artifacts_dir`/`market_dir` fields and their
  `QE_SERVER_*` reads are gone from `ServerConfig`; the test now covers `config_path` resolution.
- **Spawn pin:** `CliJobSpawner::with_config_path` sets `QE_CONFIG` on the child (covered by the
  existing spawn arg tests' construction path; behaviour with `new` unchanged — no pin, env inherited).
- Green gate: `fmt`, `clippy -D warnings`, `test --all`, `cargo deny check`, firewall test.

## Risks & blast radius

- **New crate edge `qe-server → qe-config`.** Firewall forbids only `qe-server → qe-runtime|qe-venue`;
  `qe-config` reaches neither. `crates/architecture/tests/firewall.rs` will re-parse the graph and must
  stay green.
- **Deprecated-override behaviour change.** A deployment with a *divergent* `QE_SERVER_*` storage
  override now fails to boot. This is intended (it was the silent bug). Documented in ARCHITECTURE.md
  env table.
- **`ServerConfig` field removal** (`artifacts_dir`/`market_dir`). Internal; the library tests build
  `ReadState` directly (`tests/common::empty_read_state_under`) and are unaffected. The `from_env` unit
  test is updated.
- Scope contained to `crates/server` (+ the new `qe-config` dep) and ARCHITECTURE.md's env table.

## Vintage / goldens — explicit confirmation of no movement

This ticket **does not change** the config schema, any field, or any storage-dir *value*. It only
changes *where the server reads* the dirs from (now `qe-config` instead of a parallel namespace) and
adds a boot guard. `Config::content_hash` (the vintage-lineage input) is untouched; no golden/vintage
fixture is modified. The green gate verifies goldens are **byte-identical**; if any golden moves, that
signals the change leaked into vintage content and the work stops and returns (out of scope).
