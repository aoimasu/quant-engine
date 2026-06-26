# QE-002 — Configuration system — design / evidence

## Ticket

`Phase: P0` · `Area: cross-cutting` · `Depends on: QE-001`

**Goal.** A typed, layered, reproducible configuration system. Every stage is parameterised
(windows, archive resolution, thresholds, venue, instrument universe), so config must be
layered and its hash recorded into vintage lineage.

**Acceptance criteria.**
- Invalid config fails fast with a **field-level** message.
- The same config file produces the **same config hash** across runs/machines.

**Out of scope.** Secrets management beyond reading from env / secret-store references.

## Current-state evidence

- Post-QE-001 workspace: nine crates, no config crate. Interview decisions fix config format =
  **TOML**; profiles = `train` / `runtime-sim` / `runtime-live`; instrument universe default
  `BTCUSDT + ETHUSDT`; base resolution `5m` → reconstruct `30m`/`4h`; history default max;
  determinism is first-class (seeded).

## Design decisions

### New crate `qe-config` (`crates/config`)

Cross-cutting infra like `domain`/`signal`. It declares **no internal-crate dependencies yet**
(it has no domain types to reference); QE-007/QE-012 will reintroduce `qe-domain` when config
adopts shared instrument/resolution newtypes. Adding the crate under `crates/*` is picked up by
the workspace glob and does not affect the decoupling invariant (no path to
`wfo`/`ensemble`/`runtime`-only crates). The QE-001 topology guard still holds.

### Layering & loading

- Use **figment** (`toml` + `env` providers): merge a base TOML file, then an optional
  **profile overlay** file `<stem>.<profile>.<ext>` next to the base (e.g.
  `config.runtime-sim.toml`), then `QE_`-prefixed environment overrides (nested via `__`).
- `Config::load(profile, base_path) -> Result<Config, ConfigError>`. Precedence: base < overlay <
  env. The **requested profile is forced** onto the resolved config (authoritative over file
  contents), so `train` / `runtime-sim` / `runtime-live` are genuinely separate configurations —
  this is how the "separate profiles" Scope item is satisfied. A missing overlay is skipped.
- A `Config::from_toml_str` path for tests/embedding (single source, no filesystem; parses the
  `profile` field from content).

### Schema (representative, extensible)

Later tickets extend this; QE-002 ships a real-but-minimal schema exercising every mechanism:
- `profile: Profile` (`train` | `runtime-sim` | `runtime-live`, kebab serde).
- `instruments: Vec<String>` (default `["BTCUSDT","ETHUSDT"]`).
- `bars: { base: String, reconstructed: Vec<String> }` (default `5m` → `["30m","4h"]`).
- `history: { max_available: bool, start: Option<String> }`.
- `storage: { market_dir, synthetic_dir, artifacts_dir }` — configurable, **volume-friendly
  relative paths** (feeds QE-013); no hard-coded absolutes.
- `determinism: { seed: u64 }`.

### Validation (AC #1)

A `validate(&self) -> Result<(), ConfigError>` with **field-path** errors, e.g.
`ConfigError::Invalid { field: "bars.base", message: "unknown resolution '5x'" }`. Checks:
- `instruments` non-empty, every element non-blank, no duplicates (`instruments[i]`);
- `bars.base` is a known resolution; every `bars.reconstructed[i]` is a known resolution strictly
  coarser than `base`, with no duplicates;
- `history`: if `!max_available` then `start` must be set; if `start` is present it must be an
  ISO `YYYY-MM-DD` date (format + range check — full calendar validation is deferred to the shared
  time type in QE-007);
- storage dirs non-empty.
Loading calls `validate()` so invalid config fails fast at load.

### Reproducible hashing (AC #2)

`Config::content_hash() -> String`: serialise the resolved config to **canonical JSON** (serde
preserves struct field declaration order; no `HashMap` — `Vec`/scalars only → deterministic),
then `sha2::Sha256`, hex-encoded. Same file (and same env) ⇒ identical bytes ⇒ identical hash on
any machine. The hash is what QE-006/QE-129 fold into vintage lineage.

### Errors

Local `ConfigError` via `thiserror` (the shared error model is QE-004, not yet merged; QE-002
keeps a small local enum and QE-004 can later re-home/wrap it). Variants: `Load`, `Parse`,
`Invalid { field, message }`.

## New workspace dependencies

`serde` (derive), `figment` (toml+env), `sha2`, `thiserror`. Added to `[workspace.dependencies]`;
`serde_json` already present.

## Test plan (TDD)

1. **Hash determinism:** same TOML string → equal `content_hash` across two loads; a changed
   field → different hash. (AC #2)
2. **Field-level validation:** bad `bars.base`, reconstructed-not-coarser, empty instruments,
   `max_available=false` without `start`, empty storage dir → each yields `Invalid { field, .. }`
   naming the right field. (AC #1)
3. **Layering:** base TOML overridden by `QE_`-env var changes the resolved value.
4. **Profiles:** each profile parses; profile-specific layer overrides base.
5. Gates: fmt/clippy/build/test green.

## Risks

- **figment dep weight / env-merge semantics:** keep providers explicit; test the env override
  path so behaviour is pinned.
- **Hash stability:** must avoid any `HashMap`/`HashSet` in serialized config; enforce via
  schema (Vec only) and a determinism test. Float fields (none yet) would also threaten
  stability — keep config integer/string/bool; document the rule.
- **Schema churn:** later tickets will extend the schema; the framework (load/validate/hash) is
  the durable part — keep it generic.
