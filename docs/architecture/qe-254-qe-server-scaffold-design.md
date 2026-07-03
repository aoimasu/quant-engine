# QE-254 — `qe-server` crate scaffold (axum + tokio, static SPA, firewall guard)

Evidence note written **before** implementation (work-on-tickets step 1).

Ticket: `docs/backlog.md` "## QE-254". Spec: `docs/superpowers/specs/2026-07-02-admin-ui-training-backtest-design.md`
§4, §6; ADR **D4a** (`docs/architecture/admin-ui-decisions.md`). Plan Spec 2:
`docs/superpowers/plans/2026-07-03-admin-ui-v1-cli-jobs.md`.

## 1. Goal / scope (this ticket only)

Stand up a new workspace crate `crates/server` (package `qe-server`) that is a **second composition
root** (D4a): axum + tokio, async **isolated to this crate**. Deliver **only** the scaffold:

- a **health endpoint** (`GET /api/health` → `200` JSON `{"status":"ok"}`),
- serve **built SPA static assets at `/`** (a configured static dir + SPA fallback to `index.html`),
- **reserve `/api`** as the namespace future tickets extend (health lives under it),
- a **firewall/decoupling guard** asserting `qe-server` pulls in **no** `qe-runtime`/`qe-venue` edge,
  now actually covering `qe-server`,
- 12-factor `QE_`-prefixed configuration for the bind address and static dir.

**Out of scope** (later tickets, deliberately not implemented): run lifecycle/subprocess (QE-255),
auth (QE-256), vintages/coverage read APIs (QE-257), the real SPA (QE-258).

## 2. Current-state evidence (verified by reading the repo)

### 2.1 Workspace dependency conventions
- Root `Cargo.toml`: `members = ["crates/*"]`; shared third-party + internal path crates are declared
  once under `[workspace.dependencies]` and opted into per-crate as `dep.workspace = true`
  (verified lines 18–62). Internal crates are `publish = false` via `[workspace.package]`.
- Member crates inherit `version/edition/license/rust-version/repository/publish` from
  `[workspace.package]` and set `[lints] workspace = true` (verified in `crates/cli/Cargo.toml`,
  `crates/config/Cargo.toml`).
- A crate with a binary declares `[[bin]]` (e.g. `qe-cli` → binary `qe`). `qe-server` ships a binary.
- **No** `tokio`/`axum`/`tower` currently appear in `Cargo.lock` (grep = empty) — async is genuinely
  absent today, consistent with D4a's "async isolated to the server crate".

### 2.2 The two guards this ticket must extend
Both already parameterise a reachability check over a graph; I only add a rule + coverage.

1. **QE-132 firewall** — `crates/architecture/src/lib.rs` + integration test
   `crates/architecture/tests/firewall.rs` (run via `cargo test -p qe-architecture --test firewall`).
   - `dependency_graph()` parses **`crates/*/Cargo.toml`** manifests directly, collecting only
     internal `qe-*` production deps (dev/build-dev excluded; dependency-table + platform forms
     handled). A brand-new `crates/server/Cargo.toml` is therefore picked up automatically.
   - `firewall_rules()` (lines 198–208) returns `Vec<FirewallRule { upstream, forbidden }>`;
     `check_firewall()` fails if `upstream` transitively reaches any `forbidden` crate.
   - The integration test has a **sanity gate**: it asserts named crates are present in the parsed
     graph (so a parse break can't vacuously pass). I extend this list to include `qe-server`, so the
     guard genuinely covers it, plus assert the real parsed edge `qe-server → qe-telemetry` exists so
     the new rule can't be vacuous.

2. **QE-001 decoupling** — `crates/cli/tests/dependency_topology.rs`. Builds the workspace graph from
   **`cargo metadata --no-deps`** (normal deps only, workspace-internal edges) and asserts absence of
   forbidden transitive edges. I add: `qe-server ⇏ qe-runtime` and `qe-server ⇏ qe-venue`, plus a
   presence sanity for `qe-server`.

Two independent mechanisms (manifest-parse vs `cargo metadata`) now both assert the same invariant for
`qe-server` — defence in depth, matching how the repo already double-guards the search/live firewall.

## 3. Decisions

### D1 — Health route shape
Spec §6.2 lists the future `/api` routes (auth/runs/vintages/coverage) but **does not** enumerate a
health endpoint. Per the ticket ("if unspecified, pick a clean conventional one and document it") I use
**`GET /api/health` → `200`, `application/json`, body `{"status":"ok"}`** (axum `Json`). It lives under
the reserved `/api` namespace, so it composes with the later API surface.

### D2 — `/api` reservation via nested router
The router nests an `/api` sub-router (`Router::new().route("/health", get(health))`) under
`.nest("/api", …)`. Because `nest` owns the whole `/api/*` prefix, an unknown `/api/*` path returns
**404 from the API sub-router** rather than falling through to the SPA `index.html`. This keeps `/api`
a clean, reserved JSON namespace for QE-255/256/257 to extend, and prevents the SPA fallback from
masking a genuinely missing API route.

### D3 — Static serving + SPA fallback
`tower-http`'s `ServeDir` mounted as the outer `.fallback_service`, with a per-request fallback to
`ServeFile::new(<dir>/index.html)` so client-side routes (e.g. `/backtests/123`) still return the SPA
shell. If the dir/index is absent (e.g. before QE-258 builds the SPA), `ServeDir`+`ServeFile` return
`404` gracefully — **no panic, no hard-coded path**. A tiny placeholder `crates/server/static/index.html`
is committed so a fresh checkout serves *something* at `/`; the real build lands in QE-258.

### D4 — Configuration (12-factor, `QE_`-prefixed)
Server-only knobs read from the environment via a small `ServerConfig::from_env()` in `qe-server`:
- `QE_SERVER_ADDR` — bind address, default `127.0.0.1:8080`.
- `QE_SERVER_STATIC_DIR` — static-assets dir, default `crates/server/static` (relative; **never** an
  absolute path).

These stay **in `qe-server`, not `qe-config`**: `qe-config::Config` is a training-domain schema
(universe/bars/history/determinism); server transport knobs don't belong there and would couple the
composition root to the training schema. The `QE_` prefix + env-override style is deliberately
consistent with `qe-config`'s conventions (`Env::prefixed("QE_")`). Spec §6.4's richer keys
(`QE_DATA_DIR`, OAuth/session secrets) arrive with QE-255/256; only addr + static dir are in scope now.

### D5 — Dependency posture (firewall-safe)
`qe-server` depends on: `axum`, `tokio`, `tower-http`, `serde`, `serde_json`, `tracing`, and the shared
internal crate **`qe-telemetry`** (structured startup logging — a genuine use that also gives the
firewall sanity a real `qe-server → qe-telemetry` edge). It depends on **no** `qe-runtime`/`qe-venue`
(D4a). Dev-deps: `tokio` (macros/rt for the async test), `tower` (`util` → `oneshot`), `tempfile`
(temp static dir). New workspace deps are added to root `[workspace.dependencies]` per convention.

### D6 — `cargo deny`
Axum/tokio pull a large MIT/Apache-2.0 tree. `deny.toml`'s allow-list already covers
MIT/Apache-2.0/BSD-2/BSD-3/ISC/Unicode-3.0/Zlib/MPL-2.0/CC0. Plan: add deps, run `cargo deny check`,
and only add a **well-known permissive** licence to the allow-list if a new transitive dep needs one,
recording it here. **No** weakening of advisory/ban checks. Any GPL/AGPL/unknown licence ⇒ STOP + blocker.
(Actual outcome recorded in §6 after running.)

## 4. Test plan (TDD where practical)

1. **Firewall rule first** (red→green): add `qe-server` to `firewall_rules()` forbidding
   `qe-runtime`/`qe-venue`, and to the integration-test sanity list. Before the crate exists this fails
   (crate missing from graph); it passes once `crates/server/Cargo.toml` exists with the right deps.
2. **Decoupling**: extend `dependency_topology.rs` with `qe-server ⇏ {qe-runtime, qe-venue}` + presence.
3. **Async integration test** (`crates/server/tests/http.rs`, `#[tokio::test]`): build the router via
   the public factory against a temp static dir containing a known `index.html`; use
   `tower::ServiceExt::oneshot` (no network bind):
   - `GET /api/health` ⇒ `200`, body `{"status":"ok"}`.
   - `GET /` ⇒ `200`, body == the temp `index.html` contents.
   - `GET /api/does-not-exist` ⇒ `404` (proves `/api` is reserved, not SPA-swallowed).
4. **Unit**: `ServerConfig::from_env` defaults + override (serialized env access).
5. **Green gate**: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -D warnings`;
   `cargo test --workspace --locked`; `cargo test -p qe-architecture --test firewall --locked`;
   `cargo deny check`.

## 5. Risks

- **Dependency-tree size / deny**: mitigated by minimal tokio/axum features and the pre-existing
  permissive allow-list; §6 records any addition.
- **axum 0.8 API drift** (path-param syntax `{id}`): no path params in scope, so unaffected.
- **`Cargo.lock` churn**: gates run `--locked`; the regenerated lock is committed with the feature.
- **SPA fallback vs reserved `/api`**: resolved by D2 (nested `/api` returns its own 404).

## 6. Deny / verification outcome

Run on the feature commit (toolchain 1.96.0, cargo-deny 0.19.9):

- `cargo fmt --all --check` — **PASS**.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — **PASS** (no issues).
- `cargo test --workspace --locked` — **PASS** (620 passed, 2 ignored).
- `cargo test -p qe-architecture --test firewall --locked` — **PASS**.
- `cargo deny check` — **PASS** (exit 0): `advisories ok, bans ok, licenses ok, sources ok`. The only
  output is non-failing `warning[duplicate]` lines (`getrandom`, `r-efi`, `rand_core`) from the
  `multiple-versions = "warn"` policy — informational, not a gate failure.
- **`deny.toml` changes: NONE.** The axum/tokio/tower-http tree is entirely MIT / Apache-2.0 / ISC /
  BSD / Unicode-3.0 / Zlib — all already in the existing allow-list. No new licence, no ban, no
  advisory/source relaxation.
