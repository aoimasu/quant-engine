# QE-013 — Local run & deployment-agnostic packaging

`Phase: P0` · `Area: cross-cutting / ops` · `Depends on: QE-002`

## Goal

A documented **one-command local run** that produces a **vintage**, with **12-factor**,
deployment-agnostic packaging: every persistent-state location configurable, **no hard-coded
absolute paths**, and an optional Dockerfile that runs the *same* binary as the local run. No
Railway/AWS lock-in.

## Current state (evidence)

- `crates/cli` is a scaffold: `main.rs` just `println!`s the version (QE-001). It depends on the
  pipeline crates but wires none of them, and does **not** depend on `qe-config` yet.
- `qe-config` (QE-002) already gives 12-factor config: layered TOML + `QE_`-prefixed env overrides,
  and a `[storage]` section with `market_dir` / `synthetic_dir` / `artifacts_dir` defaulting to
  **relative** paths (`data/lmdb/market`, …) — volume-friendly, no absolutes.
- `qe-determinism` (QE-006) provides `Lineage` (config_hash + input_snapshot_id + code_commit +
  seeds) with a stable `Lineage::id()` → 64-hex SHA-256 — a content-addressed **vintage id**.
- `qe-domain::VintageHash` (QE-007) validates that 64-hex shape. `qe-config::Config::universe()`
  (QE-012) resolves the point-in-time universe.
- The actual training **stages** are P1 (QE-101+). So QE-013 delivers the *runnable skeleton + the
  vintage artefact + the packaging contract* the stages slot into — not the stage logic.

## Design

Give `qe-cli` a small **library** (`src/lib.rs`) so the run logic is unit/integration-testable, with
`main.rs` a thin dispatcher.

### CLI surface (hand-rolled parser — no `clap`, matching the crate's minimal-dep ethos)

```
qe                                  # prints version (unchanged from QE-001)
qe train [--config <path>] [--profile <train|runtime-sim|runtime-live>]
```

`--config` defaults to `config.toml`; `--profile` defaults to `train`. Unknown flags → a usage
error (non-zero exit). Argument parsing is a dozen lines over `std::env::args` — a `clap` dependency
isn't justified for two flags, and the codebase already favours hand-rolled over heavy deps (e.g.
the QE-012 date math).

### `train` run (`run_train`)

`pub fn run_train(cfg: &Config, code_commit: &str) -> Result<Vintage, CliError>`:

1. **Resolve the universe** — `cfg.universe()?` (validates listing/delisting windows).
2. **Ensure configurable state dirs exist** — `create_dir_all` for `storage.market_dir`,
   `synthetic_dir`, `artifacts_dir`. **All paths come from config**; nothing absolute is baked in.
3. **Build the vintage lineage** — `Lineage::from_config(cfg, input_snapshot_id = "", code_commit,
   seeds = [cfg.determinism.seed])`; `vintage_id = VintageHash::new(lineage.id())?` (re-validates the
   digest shape).
4. **Write the vintage manifest** — JSON to
   `<artifacts_dir>/vintages/<vintage_id>/manifest.json`, recording the vintage id, the full
   `Lineage`, the profile, and the resolved universe roster (instrument + listed/delisted millis,
   incl. delisted symbols — no survivorship drop). Returns the `Vintage { id, manifest_path }`.

`code_commit` is passed in (the binary supplies `option_env!("QE_CODE_COMMIT")` falling back to the
crate version; tests pass a fixed value) so the vintage is **deterministic** — re-running the same
config + commit yields the **same vintage id and bytes**. The manifest deliberately carries **no
wall-clock timestamp**, keeping vintages content-addressed and reproducible (QE-006 ethos); full
vintage lineage across stages is formalised later (QE-129).

> The training *stages* are not yet implemented; `run_train` is the composition root they will hang
> off. It already produces a real, resolvable vintage from real inputs (config hash + seed +
> universe), which is exactly what AC #1 asks of the P0 skeleton.

### Packaging

- **`Dockerfile`** (multi-stage): a `rust:1.96` builder runs `cargo build --release -p qe-cli`; a
  slim runtime stage copies the `qe` binary and sets `ENTRYPOINT ["qe"]`. The image runs the **same
  binary** as the local `cargo run -p qe-cli`, so `docker run … train` is identical to the local
  run. State dirs are mounted volumes (paths from config/env), so no platform assumptions.
- **`config.example.toml`** — a committed example so a clean checkout has a one-command run:
  `cargo run -p qe-cli -- train --config config.example.toml`.
- **`README.md`** — documents the one-command local run, the env-override model, the configurable
  state dirs, and the Docker parity build.

### Why this shape

- **One-command run + vintage (AC #1):** `cargo run -p qe-cli -- train --config config.example.toml`
  on a clean checkout writes a vintage manifest under the configured artefacts dir.
- **Everything configurable, no absolutes (AC #2):** state dirs are read from `[storage]` (relative
  defaults); `run_train` writes only under `artifacts_dir`. A test asserts the defaults are relative
  and that a custom `artifacts_dir` fully redirects output.
- **Docker parity (AC #3):** the image builds the workspace and runs the same `qe` binary/entrypoint.

## Test plan (TDD)

`crates/cli/tests/train.rs` (integration) + `src/lib.rs` unit tests:

- **AC #1** — `run_train` against a temp-dir config produces a manifest file at the expected
  `vintages/<id>/manifest.json`; the id is a valid 64-hex `VintageHash`; the manifest round-trips and
  records the universe roster.
- **AC #1 determinism** — two `run_train` calls with the same config + commit yield the **same**
  vintage id and byte-identical manifest (no wall-clock).
- **AC #2** — `StorageConfig::default` paths are all **relative** (no leading `/`); a run with a
  custom `artifacts_dir` writes the manifest **only** under that dir (nothing escapes to an absolute
  path).
- **CLI parsing** — `train`/`--config`/`--profile` parse; unknown flag → usage error; bare invocation
  → version.
- **Docker/example** — assert `config.example.toml` loads + validates, and the `Dockerfile` exists
  and invokes the `qe` binary (light structural check).

## Risks / out of scope

- **Out of scope:** Railway provisioning, CD, platform volumes/secrets (QE-311); the training stage
  logic (QE-101+). The vintage here is the skeleton manifest, not the full multi-stage lineage
  (QE-129).
- **Risk:** Docker can't be exercised in CI here, so AC #3 is covered by the committed Dockerfile +
  doc + a structural test, not a live image build. Noted explicitly.
- **Topology:** `qe-cli` is the composition root (already depends on the pipeline crates); adding
  `qe-config`/`qe-determinism` adds no edge into `runtime`'s forbidden set, so the QE-001 guard stays
  green.
