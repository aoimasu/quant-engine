# QE-001 — Cargo workspace & crate topology — design / evidence

## Ticket

`Phase: P0` · `Area: cross-cutting` · `Depends on: —`

**Goal.** A single Rust workspace with clear crate boundaries that keeps the training and
runtime pipelines decoupled while sharing the indicator catalogue and domain types.

**Acceptance criteria.**
- `cargo build --workspace` and `cargo test --workspace` succeed on a clean checkout.
- A dependency check proves `runtime` does not depend on `wfo`/`ensemble` (and vice-versa);
  shared code only via `signal`/`domain`.

## Current-state evidence

- Greenfield repo: only `README.md`, `docs/specs.md`, `docs/backlog.md` tracked. No Rust code,
  no `Cargo.toml`. (`git ls-files` → docs + README only.)
- Toolchain just installed: rustc/cargo/clippy/rustfmt 1.96.0 (stable, aarch64-apple-darwin).

## Design decisions

### Crate topology (`crates/<name>`)

| Crate | Kind | Purpose | May depend on |
|-------|------|---------|---------------|
| `domain` | lib | Shared types (instruments, bars, time, money, side, vintage hash) | — |
| `signal` | lib | Indicator catalogue + bar reconstruction (shared offline/online) | `domain` |
| `storage` | lib | LMDB/Parquet/DuckDB abstractions | `domain` |
| `ingest` | lib | Data ingestion + fusion | `domain`, `storage` |
| `wfo` | lib | Walk-forward optimisation (training only) | `domain`, `signal`, `storage` |
| `ensemble` | lib | Ensemble construction (training only) | `domain`, `signal`, `storage` |
| `venue` | lib | Venue adapters / connectivity (runtime only) | `domain` |
| `runtime` | lib | Runtime pipeline (bootstrap, live, hedger) | `domain`, `signal`, `storage`, `venue` |
| `cli` | bin | Entry points / binaries | any lib crate |

**Decoupling invariant (the core of this ticket):** `runtime` must NOT depend on `wfo` or
`ensemble`, and `wfo`/`ensemble` must NOT depend on `runtime`. The only shared code crosses
through `signal`/`domain`. This mirrors `docs/specs.md` ("intentionally decoupled… aside from
shared indicators and strategy logic").

### Workspace mechanics

- **Virtual manifest** at repo root: `[workspace]` with `resolver = "2"`, `members = ["crates/*"]`.
- **Shared dependency versions** via `[workspace.dependencies]`; member crates use
  `dep.workspace = true`.
- **Shared lints** via `[workspace.lints]` (rust + clippy), inherited with `[lints] workspace = true`.
- **Profiles**: keep default dev; `release` with `lto = "thin"`, `codegen-units = 1` for the
  compute-heavy training/runtime binaries. (Conservative — revisit if build times bite.)
- **`rust-toolchain.toml`** pinning `channel = "1.96.0"` + components `rustfmt`, `clippy`.

### Dependency-check enforcement (AC #2)

Implement as a workspace integration test in `cli/tests/dependency_topology.rs` that shells out
to `cargo metadata --format-version 1 --no-deps`. With `--no-deps`, `packages` contains only
workspace members, each carrying its own declared `dependencies` (with `name` and `kind`). The
test keeps **normal** edges (`kind == null`; dev/build deps don't ship, so they create no
pipeline coupling), filters to members, computes the transitive closure per crate, and asserts:
- no path from `runtime` → `wfo`/`ensemble`;
- no path from `wfo`/`ensemble` → `runtime`.

This member-local closure is simpler than walking the full `resolve.nodes` graph and needs no
package-id matching.

Rationale for a test (not just CI grep): it travels with the workspace, runs under
`cargo test --workspace`, and fails the build the moment someone adds a forbidden edge. The
test parses metadata JSON with `serde_json` (already a common dep) — kept dependency-light.

## Test plan

1. **TDD red:** write `dependency_topology.rs` first; assert forbidden edges absent. It should
   compile/pass once crates exist with correct deps (and would fail if I wired a bad edge).
2. Add a deliberate temporary bad edge locally to confirm the test goes red, then remove it
   (documented here, not committed).
3. `cargo build --workspace` and `cargo test --workspace` green.
4. `cargo fmt --check` and `cargo clippy --workspace -- -D warnings` green (sets up QE-005).

## Risks

- **Over-scoping crate internals:** this ticket only establishes topology + empty crates with
  a placeholder item each so they compile. No business logic. Keep diff to scaffolding.
- **`cargo metadata` shape drift:** pin parsing to the documented `resolve.nodes[].deps` form;
  tolerate absent optional fields.
- **Profile tuning** (`lto`/`codegen-units`) may slow builds; acceptable for now, revisit if painful.
