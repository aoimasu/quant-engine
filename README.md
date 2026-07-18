# quant-engine

A deterministic, deployment-agnostic quant engine. Two decoupled pipelines (training → backtest,
and runtime), sharing one domain vocabulary. Runs locally today; the packaging is 12-factor so
moving to a platform later (e.g. Railway) is mechanical.

## Requirements

- Rust `1.96.0` (pinned in `rust-toolchain.toml` — `rustup` installs it automatically).
- Node (only to build the admin SPA in `web/`).

## Runnable jobs (`qe-cli`)

The deterministic pipeline runs as three CLI jobs, each emitting **JSON-line progress** on stdout.

**Train** — run the real search → ensemble → validation → **G1 gate**, and seal a **vintage**:

```sh
cargo run -p qe-cli -- train --config config.example.toml
```

It resolves the point-in-time instrument universe, runs the MAP-Elites/WFO search, builds the
ensemble, applies the G1 over-fit gate, and writes a content-addressed vintage to:

```
data/artifacts/vintages/<vintage-id>.json
```

The `<vintage-id>` is a SHA-256 over the run's lineage (config hash + seed + code commit), so the
**same config + seed + commit reproduces the same vintage** bit-for-bit.

**Backtest** — run a sealed vintage over a window into a deterministic `result.json`:

```sh
cargo run -p qe-cli -- backtest --vintage <vintage-id> \
    --start 2021-01-01 --end 2024-12-31 --resolution 1h --run-dir /tmp/run --json
# → /tmp/run/result.json (metrics, equity/drawdown curves, monthly heatmap, trades)
```

**Ingest** — populate the LMDB market store (real Binance decoders live behind the default-off
`http` feature; the committed sample store lets backtests run fully offline):

```sh
cargo run -p qe-cli -- ingest --config config.example.toml --start … --end … --resolution 1h
```

**Evolve** — run the offline GP indicator search and seal a **formula pool** (default-off / research-first):

```sh
cargo run -p qe-cli -- evolve --run-dir /tmp/evolve --json
# → data/artifacts/{research|…}/pools/<pool-id>  (K≤16 canonical S-expressions + deflation summary)
```

`Done` emits a `pool:` id (**never a vintage**), and a sealed pool never auto-mints a vintage. Sealing a pool
to **production** is governed server-side — RBAC + a server-authoritative `seal_allowed` predicate + a
tamper-evident audit log — and stays fail-closed until per-formula `gate_evidence` is wired into the pipeline.

Print the version:

```sh
cargo run -p qe-cli
```

## Admin UI (server + SPA)

`qe-server` is an axum backend (a second composition root — reuses the training/shared crates,
never the runtime) that triggers & supervises the CLI jobs and serves an authenticated React SPA to
trigger, monitor, and review runs in a browser.

```sh
# 1) build the SPA
cd web && npm ci && npm run build          # → web/dist

# 2) run the server (serves web/dist at / and the API at /api)
QE_SERVER_STATIC_DIR=web/dist \
QE_ADMIN_ALLOWED_EMAILS=you@example.com \
QE_GOOGLE_CLIENT_ID=… QE_GOOGLE_CLIENT_SECRET=… QE_GOOGLE_REDIRECT_URI=… QE_SESSION_SECRET=… \
    cargo run -p qe-server
```

Sign in with Google (allowlisted email) → trigger/monitor/review backtest & training runs.
All state is paper/offline; there is no live order submission.

## Configuration (12-factor)

Settings come from `config.example.toml` (copy it to `config.toml` for your own run). Every value is
overridable by a `QE_`-prefixed environment variable, nested with `__`:

```sh
QE_DETERMINISM__SEED=7 cargo run -p qe-cli -- train --config config.example.toml
```

**All persistent state lives under configurable, volume-friendly directories** — there are no
hard-coded absolute paths:

| Setting                 | Default              | Holds                              |
| ----------------------- | -------------------- | ---------------------------------- |
| `storage.market_dir`    | `data/lmdb/market`   | LMDB market-data store             |
| `storage.synthetic_dir` | `data/lmdb/synthetic`| LMDB synthetic/indicator cache     |
| `storage.artifacts_dir` | `data/artifacts`     | Vintage artefacts                  |

Point them anywhere (a mounted volume, a scratch dir) via config or `QE_STORAGE__*` env vars.

## Docker (dev/prod parity)

The optional `Dockerfile` builds the workspace and runs the **same `qe` binary** as the local run:

```sh
docker build -t quant-engine .
docker run --rm -v "$PWD/data:/app/data" quant-engine          # == train --config config.toml
docker run --rm -v "$PWD/data:/app/data" quant-engine version
```

State is written to the mounted `/app/data` volume; no platform-specific assumptions.

## Workspace

A Cargo workspace under `crates/*` (domain, config, storage, determinism, …, plus `server`,
`qe-formula-pool`, and `qe-run-protocol`) and a React SPA under `web/`. Run the gates:

```sh
# Rust
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test -p qe-architecture --test firewall --locked   # train/live decoupling guard
cargo deny check

# Frontend
cd web && npm ci && npm run lint && npm run build && npm test
```
