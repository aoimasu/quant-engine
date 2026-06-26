# quant-engine

A deterministic, deployment-agnostic quant engine. Two decoupled pipelines (training → backtest,
and runtime), sharing one domain vocabulary. Runs locally today; the packaging is 12-factor so
moving to a platform later (e.g. Railway) is mechanical.

## Requirements

- Rust `1.96.0` (pinned in `rust-toolchain.toml` — `rustup` installs it automatically).

## One-command local run

From a clean checkout, run the training pipeline and produce a **vintage**:

```sh
cargo run -p qe-cli -- train --config config.example.toml
```

This loads the config, resolves the point-in-time instrument universe, creates the configurable
state directories, and writes a content-addressed vintage manifest:

```
data/artifacts/vintages/<vintage-id>/manifest.json
```

The `<vintage-id>` is a SHA-256 over the run's lineage (config hash + seed + code commit), so the
**same config + commit reproduces the same vintage** bit-for-bit.

Print the version:

```sh
cargo run -p qe-cli
```

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

A Cargo workspace under `crates/*` (domain, config, storage, determinism, …). Run the gates:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
```
