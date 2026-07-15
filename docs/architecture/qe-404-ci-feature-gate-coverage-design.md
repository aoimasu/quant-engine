# QE-404 — CI must build, lint and test the feature-gated code (`http` / `arrow`)

- Ticket source of truth: `docs/reviews/2026-07-15-team-improvement-review.md` § `### QE-404`
  (lines 143–167).
- Branch: `qe-404/ci-feature-gate-coverage`
- Change surface: `.github/workflows/ci.yml` only (plus this evidence note). No Rust source
  changes were required — see "Lint fixes" below.

## Problem (current-state evidence)

The workspace parks all real I/O behind default-off cargo features, and CI never enables them,
so the most safety-relevant code has never been seen by `fmt`, `clippy -D warnings`,
`clippy::unwrap_used = "deny"`, or `cargo test`.

CI, before this change (`.github/workflows/ci.yml`):

- `clippy` job: `cargo clippy --workspace --all-targets --locked -- -D warnings`
  (`ci.yml:40`) — no `--features` / `--all-features`.
- `test` job: `cargo test --workspace --locked` (`ci.yml:54`) — no features.
- `fmt` (`ci.yml:28`) and `deny` (`ci.yml:56-63`) likewise never enable a feature.

Workspace lint policy that must therefore also bind the feature-gated code
(`Cargo.toml:91-99`):

- `[workspace.lints.rust] unsafe_code = "deny"` (`Cargo.toml:92`)
- `[workspace.lints.clippy] all = warn (priority -1)` (`Cargo.toml:95`)
- `[workspace.lints.clippy] unwrap_used = "deny"` (`Cargo.toml:99`)
  (`clippy.toml` exempts `#[cfg(test)]` via `allow-unwrap-in-tests = true`).

Feature-gated code that was excluded from every gate:

| Crate       | Feature | What it gates                                                                    | Cargo.toml evidence |
|-------------|---------|----------------------------------------------------------------------------------|---------------------|
| `qe-venue`  | `http`  | real `ureq`+native-tls REST transport (`crates/venue/src/rest.rs:253-309`)        | `crates/venue/Cargo.toml:14-16` |
| `qe-ingest` | `http`  | real `ureq` REST fetcher (`crates/ingest/src/rest.rs:159-207`, `src/fetcher.rs`)  | `crates/ingest/Cargo.toml:14-19` |
| `qe-ingest` | `arrow` | QE-104 Arrow record-batch + IPC artefact serialisation                           | `crates/ingest/Cargo.toml:17-19` |
| `qe-server` | `http`  | the real Google ID-token verifier (`crates/server/src/auth/google.rs`)           | `crates/server/Cargo.toml:15-19` |
| `qe-cli`    | `http`  | live Binance decoders / `HistoricalSource` scaffolding (`src/main.rs:290`)        | `crates/cli/Cargo.toml:13-15` |

Feature names were confirmed by reading each crate's `[features]` table (above); `qe-cli`'s
`http` feature has no extra `dep:` (pure `cfg` gate), while `venue`/`ingest`/`server` pull
`dep:ureq` (native-tls). `qe-ingest`'s `arrow` pulls `arrow-array`/`arrow-schema`/`arrow-ipc`.

## CI-matrix decision

Add two new jobs to `ci.yml`, mirroring the existing job style (pinned
`dtolnay/rust-toolchain@67ef31d…` at `1.96.0`, `Swatinem/rust-cache@v2`, `--locked`):

1. `feature-gate` — a `fail-fast: false` matrix over each `(crate, feature)` pair:
   - `qe-venue http`, `qe-ingest http`, `qe-ingest arrow`, `qe-server http`, `qe-cli http`.
   - Each leg runs **clippy** (`--all-targets -- -D warnings`, so `unwrap_used`/`unsafe_code`
     denies apply) then **test** (`--locked`). Both are scoped `-p <crate> --features <feat>`.
2. `all-features-build` — `cargo build --workspace --all-features --locked`, satisfying the
   acceptance criterion that the all-features build is green in CI.

The existing offline `clippy`/`test` jobs are left untouched as the fast default path (they do
not gain features, so they do not slow down). The new jobs run in parallel with them.

### Why matrix clippy+test rather than build-only

The spec permits "network-free unit tests only". I verified (see below) that **no** feature-gated
test performs a live network call: the real transports (`HttpRestTransport`, `HttpRestSource`,
`GoogleVerifier`) are production impls; their `#[cfg(test)]` modules exercise in-memory fakes
(`FakeTransport`) and URL construction, and `google.rs` has no test module at all. So `cargo test
--features http -p …` is offline and safe in CI, and running it maximises coverage (the acceptance
criterion "a bare `unwrap()` inside a `cfg(feature)` block fails CI" is already met by clippy,
tests are additional assurance). No live-network integration tests were added (out of scope).

## Test plan / local verification (toolchain 1.96.0)

All run with `export PATH="$HOME/.cargo/bin:$PATH"`. Results are from this branch's commit.

Default green gate:

- `cargo fmt --all --check` — PASS
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — PASS ("No issues found")
- `cargo test --workspace --locked` — PASS (671 passed, 2 ignored)
- `cargo test -p qe-architecture --test firewall --locked` — PASS (1 passed)

Feature-on clippy (`--all-targets --locked -- -D warnings`):

- `-p qe-venue --features http` — PASS
- `-p qe-ingest --features http` — PASS
- `-p qe-ingest --features arrow` — PASS
- `-p qe-server --features http` — PASS
- `-p qe-cli   --features http` — PASS

Feature-on test (`--locked`):

- `-p qe-venue --features http` — PASS (offline)
- `-p qe-ingest --features http` — PASS (offline)
- `-p qe-ingest --features arrow` — PASS (offline)
- `-p qe-server --features http` — PASS (7 tests, offline)
- `-p qe-cli   --features http` — PASS (3 tests, offline)

All-features build:

- `cargo build --workspace --all-features --locked` — PASS

Not runnable locally (CI will run): `cargo deny` (`cargo-deny` not installed here) and the
GitHub Actions workflow itself.

## Lint fixes

**None.** Enabling `http` (venue/ingest/server/cli) and `arrow` (ingest) produced **zero**
`-D warnings` / `unwrap_used` / `unsafe_code` violations — every feature-gated crate compiled and
clippied clean on the first feature-on run. No production Rust source was modified by this ticket.

## Risks

- **System OpenSSL for `native-tls`.** `venue`/`ingest`/`server` `http` pulls `ureq` with
  `native-tls`, which links system OpenSSL (`openssl-sys`). GitHub's `ubuntu-latest` runners ship
  `libssl-dev`, so the CI build resolves it with no extra step. (Locally this sandbox lacked the
  dev headers; installing `libssl-dev`/`pkg-config` was required to reproduce — a local-only
  concern, not a CI change.)
- **Build cost.** The feature jobs compile ureq/native-tls/arrow trees not in the default build;
  `Swatinem/rust-cache@v2` amortises this and the jobs run in parallel with the fast path, so the
  default `clippy`/`test` latency is unchanged.
- **Network flake risk: none.** The feature test legs are offline (verified above); no test issues
  a live request, so CI cannot flake on Binance/Google availability.

## Rollback

Revert the single commit touching `.github/workflows/ci.yml` (and delete this note). No runtime,
crate, or `Cargo.lock` changes are involved, so rollback is inert for the shipped binaries.
