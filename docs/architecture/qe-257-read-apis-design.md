# QE-257 — Vintages + market-data coverage read APIs (design / evidence note)

`Phase: PreP3` · `Area: backend / api` · `Depends on: QE-253, QE-254, QE-255, QE-256`
`Spec ref: admin-ui spec §6.2`

## Goal

Add two **session-gated** read endpoints to `qe-server`:

- `GET /api/vintages` — list sealed vintages from the configured artifacts dir (`id` / `label` /
  `summary`), the selectable list QE-259's "New backtest" trigger form consumes.
- `GET /api/market-data/coverage` — the read-only `CoverageRow[]` for the configured market store,
  the QE-259 Market-data view consumes.

Both return `401` without a valid session (registered inside `protected_routes()` behind the QE-256
`require_session` `route_layer`).

## Current-state evidence

### The hard blocker: `coverage()` lives on the wrong side of the firewall

QE-253 put `coverage()` + `CoverageRow` in `crates/cli/src/jobs/ingest.rs`
(`crates/cli/src/jobs/ingest.rs:36` `CoverageRow`, `:57` `coverage`). `qe-cli` depends on
`qe-runtime`/`qe-venue` (`crates/cli/Cargo.toml`), and the QE-132/QE-254 firewall
(`crates/architecture/src/lib.rs:212` `FirewallRule { upstream: "qe-server", forbidden:
["qe-runtime","qe-venue"] }`, asserted by `crates/architecture/tests/firewall.rs`) forbids
`qe-server` from depending on `qe-cli`/`qe-runtime`/`qe-venue`. So the server cannot reach
`coverage()` where it currently lives.

The body of `coverage()` uses only `MarketStore` + `qe-domain` (`InstrumentId`, `Resolution`,
`Timestamp`) and returns via `store.scan_bars()?` (a `StorageError`). Its only coupling to `qe-cli`
is the `RunError` error type in its signature. → It belongs in `qe-storage`, a leaf/shared crate
(`crates/storage/Cargo.toml` deps: `qe-domain`, `heed`, `rust_decimal`, `serde`, `thiserror` — no
runtime). Both `qe-cli` and `qe-server` may depend on `qe-storage`.

### Vintage repository API (source for `/api/vintages`)

`qe-vintage` (`crates/vintage/src/lib.rs`) is a shared/training-side crate (deps: `qe-signal`,
`qe-risk`, `qe-determinism` — all verified clear of `qe-runtime`/`qe-venue`, so a `qe-server →
qe-vintage` edge stays firewall-legal). `VintageRepository::new(root)` reads `<root>/<id>.json`;
`Vintage::load` verifies the content hash on load. `Vintage.content` carries `vintage_id`,
`chromosomes`, `worst_case_loss`, `format_version`; `Vintage.content_hash` pins it. There is **no
list method** — QE-257 adds `VintageRepository::list()`.

### QE-256 gating surface

`crates/server/src/auth/mod.rs:360` `protected_routes(auth)` merges `/me` + `runs::api::routes()`
then applies `require_session` via `route_layer`. New read routes merge into the same subtree ⇒
inherit the session gate. Public/`health`/`auth/*` stay outside. Test harness
(`crates/server/tests/common/mod.rs`) mints sessions through the production signer
(`qe_server::mint_session_cookie`) via `session_cookie_header(email)`.

### Config style (QE-254/255)

`ServerConfig` (`crates/server/src/lib.rs:106`) holds `QE_SERVER_*`-prefixed, **relative-default**
knobs. Repo `data/` layout (`crates/config`, README): `storage.market_dir = data/lmdb/market`,
`storage.artifacts_dir = data/artifacts`.

## Decisions

1. **Relocate `coverage()` + `CoverageRow` → `qe-storage`** in a new `crates/storage/src/coverage.rs`
   module, re-exported from `qe_storage` root. Error type swaps `qe_cli::jobs::RunError` →
   `qe_storage::StorageError` (the `?` on `scan_bars` already yields `StorageError`; `RunError`
   carried it via `#[from]`, so behaviour is identical). `CoverageRow` stays `pub` + serde. The
   QE-253 serde-shape unit test moves with it. `crates/cli/src/jobs/ingest.rs` **re-exports**
   `pub use qe_storage::coverage::{coverage, CoverageRow};` so `qe_cli::jobs::ingest::{coverage,
   CoverageRow}` (used by `crates/cli/tests/ingest_job.rs`) keeps compiling unchanged.

2. **Instrument enumeration for the server coverage endpoint.** `coverage(store, instruments)` takes
   an explicit instrument slice (unchanged — QE-253 test depends on it). The server has no universe
   to pass, so add `MarketStore::bar_instruments()` (iterate the `bars` DB keys, decode the symbol =
   bytes before the first `0x00` delimiter, dedupe in key order → ascending, deterministic; skip any
   non-decodable key defensively — our writer never produces one) and a `qe_storage::coverage_all(store)`
   convenience = enumerate + `coverage`. The endpoint reports coverage for exactly what is stored.

3. **`/api/vintages` response shape** (matches QE-259's selectable list):
   `[{ "id": <vintage_id>, "label": <vintage_id>, "summary": { "chromosomes": N, "content_hash":
   <hex>, "worst_case_loss": f64|null, "format_version": u16 } }]`. `id` is the value POST `/api/runs`
   takes as `vintage` (`crates/server/tests/runs.rs` create body uses `"vintage": "sample_vintage"`).
   `label` = `vintage_id` (no distinct display field exists yet). `summary` is a structured object the
   form can render. Ordering ascending by `vintage_id` (deterministic).

4. **`VintageRepository::list()`** added to `qe-vintage`: scan `root` for `*.json`, `Vintage::load`
   each (hash-verified), skip files that don't parse as a vintage (dir may hold unrelated artefacts),
   sort by `vintage_id`. A missing dir ⇒ `Ok(vec![])` (graceful before any vintage is sealed).

5. **Config keys.** Extend `ServerConfig` with `artifacts_dir` (`QE_SERVER_ARTIFACTS_DIR`, default
   `data/artifacts`) and `market_dir` (`QE_SERVER_MARKET_DIR`, default `data/lmdb/market`) — relative
   defaults, `QE_SERVER_` prefix, consistent with QE-254/255.

6. **State wiring / the LMDB single-open contract.** `MarketStore::open` docs warn opening the same
   path more than once concurrently in a process is UB — so the store is opened **once at startup**,
   not per request. Add `ReadState { vintages: VintageRepository, market_store: Arc<MarketStore> }`
   to `AppState`, projected via `FromRef<AppState> for Arc<ReadState>`. `ServerConfig::read_state()`
   opens the store (`DEFAULT_MAP_SIZE`) + builds the repo. `main.rs` fails fast if the store can't be
   opened (mirrors the bind-failure path). Handlers run the blocking LMDB / fs work inside
   `tokio::task::spawn_blocking` (keeps async non-blocking, mirrors the QE-256 verifier pattern).

7. **Routes** live in a new `crates/server/src/read.rs` module (`routes() -> Router<AppState>`),
   merged into `protected_routes()`.

## Test plan

- **Relocation stays green:** `crates/cli/tests/ingest_job.rs` (coverage over the sample store =
  BTCUSDT/1h/120 bars, unknown-instrument empty, ingest round-trip) unchanged — via the re-export.
  The moved serde-shape unit test runs in `qe-storage`. New `qe-storage` unit test for
  `bar_instruments` + `coverage_all`. QE-251 golden + QE-253 tests must not regress.
- **New server integration tests** (`crates/server/tests/read.rs`, `#[tokio::test]` + `tower::oneshot`,
  no network):
  - **Fixture strategy = option (i), copy the QE-251 fixtures** into `crates/server/tests/fixtures/`
    (`sample_store/` = BTCUSDT/1h/120 bars, `sample_vintage.json` = one sealed vintage). Rationale:
    `qe-server` can't depend on `qe-cli` (firewall) so the fixtures can't be reached across the crate
    boundary, and constructing a *sealed* vintage in-code needs `qe-signal`/`qe-risk`/`qe-determinism`
    plus a hand-built `Genome` — copying the tiny (940 B + 224 KB) committed fixtures is the cleanest
    hermetic option and needs zero new dev-deps. The store is copied into a tempdir before opening
    (its schema-init write txn must not touch the committed fixture); the vintage json is read in place.
  - `GET /api/vintages` with a valid session ⇒ `200` + the sealed vintage's `id`/`label`/`summary`.
  - `GET /api/market-data/coverage` with a valid session ⇒ `200` + the expected `CoverageRow`.
  - Both without a session ⇒ `401`.
- **Green gate:** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -D
  warnings`, `cargo test --workspace --locked`, `cargo test -p qe-architecture --test firewall
  --locked`, `cargo deny check`.

## Risks

- **Firewall:** adding `qe-storage` + `qe-vintage` to `qe-server` must not introduce a `qe-runtime`/
  `qe-venue` edge — verified transitively above; the firewall test is the backstop.
- **LMDB double-open UB:** mitigated by opening once at startup (decision 6); tests likewise open one
  store per path.
- **Vintage layout drift:** README sketches `data/artifacts/vintages/<id>/manifest.json`, but the
  implemented `VintageRepository` uses flat `<root>/<id>.json` (matching the QE-251 `sample_vintage.json`
  fixture). This note treats `artifacts_dir` as the flat `VintageRepository` root; a future manifest
  layout is out of scope.
- **Breaking-change surface:** `CoverageRow`'s import path moves out of `qe-cli`, but the re-export
  keeps the QE-253 path (`qe_cli::jobs::ingest::CoverageRow`) valid.
</content>
