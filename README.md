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

# 2) configure — copy the template and fill in your values (Google OAuth client, allowlisted email)
cp .env.example .env                       # then edit .env

# 3) run the server (serves web/dist at / and the API at /api); loads .env on startup
cargo run -p qe-server --features http
```

`qe-server` loads a **`.env`** file at startup (via `dotenvy`) before resolving any config, so the
copied-and-filled `.env.example` is picked up automatically — no `export`/inline env needed. Real
process-env vars still win over `.env`, so you can override one value inline
(`QE_SERVER_ADDR=127.0.0.1:9000 cargo run -p qe-server --features http`).

Notes for local dev:

- **`--features http` is required** for Google sign-in to complete — the default build wires a
  disabled verifier (the server still boots and serves health/static, but login cannot finish).
- Default bind is **`127.0.0.1:8080`** (`QE_SERVER_ADDR` to change). On loopback `QE_SESSION_SECRET`
  is optional (an ephemeral secret is generated); it is only required off-loopback (fail-closed).
- Set your Google OAuth redirect URI to `http://127.0.0.1:8080/auth/callback` and add your email to
  `QE_ADMIN_ALLOWED_EMAILS` (empty ⇒ nobody can sign in).
- **Storage:** leave it at the defaults in `.env` — the server derives its store from qe-config and
  pins the same config onto the CLI jobs it spawns, so both agree and it boots (read APIs are empty
  until data is ingested). To read the committed **sample store** offline, use a config file (both
  server and CLI read it): `cp config.example.toml config.toml` then set
  `[storage].market_dir = "crates/cli/tests/fixtures/sample_store"`. Do **not** set a lone
  `QE_STORAGE__MARKET_DIR` (with no config file, qe-config then requires the whole `[storage]` table),
  and avoid the deprecated `QE_SERVER_MARKET_DIR` (it refuses boot when it diverges from the CLI's
  store — QE-419).
- To exercise the evolve/governance seal flow, also set `QE_ROLE_{OPERATORS,APPROVERS,ADMINS}` and a
  persistent `QE_AUDIT_SIGNING_KEY` (production sealing stays fail-closed by design either way).

Sign in with Google (allowlisted email) → trigger/monitor/review backtest, training & **evolve** runs.
All state is paper/offline; there is no live order submission.

> **Two env-file conventions — pick by run mode.** **`.env`** (from `.env.example`) is for the local
> `cargo run -p qe-server` above — loaded in-process by `dotenvy`, loopback defaults. **`.env.server`**
> (from `.env.server.example`) is for the **Docker Compose** admin image below — loaded by Compose's
> `--env-file`, binds `0.0.0.0` so `QE_SESSION_SECRET` + an https redirect URI are required. Both feed
> the same `QE_*` config; the `QE_OAUTH_GOOGLE_*` and `QE_GOOGLE_*` names are interchangeable aliases.
> Both `.env` and `.env.server` are gitignored — never commit the filled-in files.

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

`qe-server` additionally loads a **`.env`** file from the repo root on startup (see the Admin UI
section); process-env vars take precedence over `.env`. The CLI does not auto-load `.env` — pass
`--config` or `QE_`-prefixed vars directly.

## Docker (dev/prod parity)

The optional `Dockerfile` builds the workspace and runs the **same `qe` binary** as the local run:

```sh
docker build -t quant-engine .
docker run --rm -v "$PWD/data:/app/data" quant-engine          # == train --config config.toml
docker run --rm -v "$PWD/data:/app/data" quant-engine version
```

State is written to the mounted `/app/data` volume; no platform-specific assumptions.

For the **admin server + SPA image**, `docker-compose.server.yml` runs the same `qe-server` behind a
mounted `./data` volume; supply its environment via `.env.server` (copy `.env.server.example`):

```sh
cp .env.server.example .env.server         # fill in the OAuth + session values (see docs/deploy)
docker compose --env-file .env.server -f docker-compose.server.yml up --build
```

The container binds `0.0.0.0`, so `QE_SESSION_SECRET` and an https redirect URI are required
(fail-closed) — see `docs/deploy/README.md`.

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
