# QE-005 — CI pipeline — design / evidence

## Ticket

`Phase: P0` · `Area: cross-cutting` · `Depends on: QE-001`

**Goal.** Every change is formatted, linted, tested, and dependency-audited; the merge fails on
any gate.

**Acceptance criteria.**
- A PR with a clippy warning or failing test cannot be merged.
- CI completes deterministically (no flaky-by-design steps).

**Out of scope.** Deployment/release automation (local packaging QE-013; Railway/CD QE-311).

## Current-state evidence

- Post-QE-004 workspace: 12 member crates + the excluded `hotpath_violation` fixture. No CI yet.
  The four gates already pass locally (run repeatedly across QE-001..004).
- Toolchain pinned to `1.96.0` (+ rustfmt, clippy) via `rust-toolchain.toml`.

## Design decisions

### `.github/workflows/ci.yml` — four required jobs

Triggers on `pull_request` and `push: [main]`. Each job is a required status check:
- **fmt** — `cargo fmt --all --check`.
- **clippy** — `cargo clippy --workspace --all-targets -- -D warnings`.
- **test** — `cargo test --workspace --locked` (`--locked` also catches a stale `Cargo.lock`, the
  exact class of bug found in QE-002 review).
- **deny** — `cargo-deny check` (advisories, licences, bans, sources) via the
  `EmbarkStudios/cargo-deny-action`.

Mechanics for determinism + speed (AC #2):
- Pin the toolchain to `1.96.0` with components (matches `rust-toolchain.toml`) via
  `dtolnay/rust-toolchain` — no floating `stable`.
- `Swatinem/rust-cache` for build caching.
- `concurrency` cancels superseded runs on the same ref.
- No network-dependent or time-dependent *test* steps → the build/test/lint gates are
  deterministic. **Caveat:** the `deny` job's advisories check pulls the live RustSec database at
  run time, so the same commit can flip green→red when a new advisory lands upstream. That is the
  deliberate point of a security gate, not flakiness — but it means "deterministic" applies to the
  code gates, not to advisory freshness.

### `deny.toml`

- `[advisories] yanked = "deny"`; `[sources]` deny unknown registries/git.
- `[licenses]` allow a standard permissive set; **our own crates are `publish = false`** (correct
  metadata for internal, never-published crates) so `[licenses.private] ignore = true` skips them —
  otherwise their non-SPDX `license = "proprietary"` would fail the licence check.
- `[bans] multiple-versions = "warn"` (informational, not a hard fail yet).

To make the crates private, add `publish = false` to `[workspace.package]` and
`publish.workspace = true` to each member manifest (inheritable field). This is metadata-only and
also prevents accidental `cargo publish`.

### Enforcing "cannot be merged" (AC #1)

The workflow reports pass/fail per PR; the *enforcement* is GitHub **branch protection** on `main`
requiring the four checks. Plan:
1. Land the workflow; confirm CI actually runs green on this PR.
2. Enable branch protection requiring `fmt`/`clippy`/`test`/`deny` via `gh api` once the checks are
   green (so a workflow bug can't lock the repo).
This makes a clippy-warning or failing-test PR un-mergeable, satisfying AC #1.

## Verification plan

- Local: the four gate commands pass on the current tree (already verified continuously).
- `cargo deny check` locally (install `cargo-deny`) to validate `deny.toml` + the dependency graph.
- Workflow YAML parses; CI run on the PR goes green (observed via `gh run`).
- Branch protection set and verified via `gh api`.

## Risks

- **cargo-deny licence check on internal crates:** mitigated by `publish = false` + `private.ignore`.
  Verified locally with `cargo deny check`.
- **Branch protection lock-out:** enable only after the checks are observed green; use exact check
  names. Keep `strict` (up-to-date) off to avoid forced rebases.
- **CI/runtime availability:** if GitHub Actions can't run in this environment, the workflow + local
  gate parity still hold; branch protection enablement is recorded as the remaining manual step.
