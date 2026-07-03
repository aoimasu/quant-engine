# QE-267 — Enforce the no-`unwrap` convention with `clippy::unwrap_used = "deny"`

`Phase: Hardening` · `Area: tooling / lint` · `Priority: P3` · `Depends on: —`

## Goal

Convert the workspace's "no bare `.unwrap()` on the production panic path" property from a
review-enforced *convention* into a compiler-enforced *invariant*, by turning on
`clippy::unwrap_used = "deny"` workspace-wide. The property already holds today, so this is a
zero-behaviour-change hardening change: it only prevents future regressions.

## Current-state evidence (before the change)

- **`unwrap_used` is a clippy `restriction` lint, not in the `all` group.** The root `Cargo.toml`
  `[workspace.lints.clippy]` currently contains only `all = { level = "warn", priority = -1 }`.
  The default `all` group deliberately excludes `restriction` lints such as `unwrap_used`, so a
  new `.unwrap()` slipping through review today produces **no** clippy diagnostic — it becomes a
  latent panic with nothing to catch it.
- **All 21 member crates already opt into the workspace lint table** via `[lints] workspace = true`
  in each crate `Cargo.toml`, so a single change to `[workspace.lints.clippy]` propagates to the
  whole workspace with no per-crate edits.
- **Exactly one bare production `.unwrap()` exists, and it is provably safe.**
  `crates/cli/src/jobs/metrics.rs:58` — `let last = *equity.last().unwrap();` inside `cagr`. The
  early-return guard on line 55 (`if years <= 0.0 || equity.len() < 2 { return 0.0; }`) guarantees
  `equity` has ≥ 2 elements, so `.last()` is always `Some`. This is the sole exception the
  workspace review found.
- **The remaining `.unwrap()` calls are in colocated `#[cfg(test)]` modules.** A workspace grep of
  `crates/*/src/**.rs` finds 529 `.unwrap()` occurrences in this repo's crate sources (the review
  cited ~577 across all targets incl. integration tests); apart from `metrics.rs:58` they are all
  inside test modules — e.g. `metrics.rs:195/196` are in `mod tests`. These must keep compiling,
  which the `clippy.toml` `allow-unwrap-in-tests = true` knob guarantees (its default is already
  `true`, but we set it explicitly so the intent is documented and pinned).
- **Precedent: the hot-path lint convention already exists.** `crates/error/src/lib.rs` documents a
  per-module `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` block for
  order-emission modules, and `crates/error/tests/hot_path_lint.rs` already proves clippy rejects an
  `unwrap()` there. QE-267 generalises the `unwrap_used` half of that convention from opt-in modules
  to the whole workspace. The `error` crate is **not** modified by this ticket.

## Implementation decisions

1. **`[workspace.lints.clippy]`**: add `unwrap_used = "deny"`, keeping the existing
   `all = { level = "warn", priority = -1 }`. A plain string value (`"deny"`) needs no `priority`
   because it is not a lint *group*; the `-1` priority on `all` still lets a specific lint override
   group members. `deny` (not `warn`) is chosen so the property is a hard compile-time invariant,
   consistent with `unsafe_code = "deny"` already in `[workspace.lints.rust]`.
2. **`expect_used` is deliberately left OFF.** `expect(...)`-with-reason is the house style for
   newtype-invariant reconstruction and is used intentionally across the codebase (and in this very
   ticket's fix). Tightening `expect_used` is QE-268's separate concern (the order-path exception).
3. **New `clippy.toml` at workspace root** with `allow-unwrap-in-tests = true`. This is the
   clippy-configuration file (distinct from `Cargo.toml` lint levels); it exempts `#[cfg(test)]`
   modules and `#[test]` functions so the ~500+ colocated unit-test unwraps keep compiling under the
   new `deny`.

   **Deviation from the ticket's stated scope (documented gap in the plan).** The ticket assumed
   `allow-unwrap-in-tests = true` alone would keep *all* test unwraps compiling. It does not:
   `allow-unwrap-in-tests` only exempts code lexically inside a `#[test]` function or a
   `#[cfg(test)]` module. **Separate integration-test files under `crates/*/tests/*.rs` also contain
   module-level helper functions** (e.g. `fn inst() { InstrumentId::new("BTCUSDT").unwrap() }` in
   `crates/ingest/tests/recon.rs:14`) that are *not* `#[test]`-annotated and *not* in a
   `#[cfg(test)]` module, so the knob does not exempt them — and the required gate
   `cargo clippy --workspace --all-targets` compiles those integration targets, so they fail under
   the new `deny`. Cargo's `[lints]` table has no per-target granularity, and clippy has no config
   to treat an entire integration-test crate as "tests", so there is **no single-point global fix**.
   The idiomatic resolution is a per-file inner attribute at the top of each affected integration-test
   file:

   ```rust
   #![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)
   ```

   Applied to the **17** integration-test files under `crates/*/tests/` that use `.unwrap()`
   (`cli/tests/{backtest_job,ingest_job,train,train_job}.rs`, `clock/tests/logging.rs`,
   `config/tests/universe.rs`, `ingest/tests/{downloader,features,persist,recon}.rs`,
   `runtime/tests/restart_parity.rs`, `server/tests/{auth,http,read,runs}.rs`,
   `storage/tests/{store,synthetic}.rs`). These are entirely test code, so the allow is correct and
   changes no production behaviour. This expands the change beyond the 4 files the ticket enumerated;
   it is the minimum required to satisfy the mandatory green gate while keeping the production panic
   path denied. Production `src/` code remains fully governed by the workspace `deny` (proven below).
4. **`metrics.rs:58`**: rewrite the lone guarded `.unwrap()` to the house expect-with-reason style:
   `*equity.last().expect("equity.len() >= 2 verified above")`. This both satisfies the new lint and
   documents *why* the access is infallible, matching the codebase convention (e.g.
   `error/src/lib.rs` tests use `.expect("source present")`).

## Test / prove-it plan

- **Positive (lint fires):** temporarily insert a bare `.unwrap()` into a **non-test** production
  function, run `cargo clippy --workspace --all-targets --locked -- -D warnings`, and confirm the
  build fails with `clippy::unwrap_used`. Then revert the scratch line. (Result pasted below.)
- **Negative (test unwraps unaffected):** the full green gate — in particular
  `cargo clippy --workspace --all-targets --locked -- -D warnings` (compiles all test targets) and
  `cargo test --workspace --locked` — passing green proves the ~500+ test-module unwraps are exempt
  under `allow-unwrap-in-tests = true`.
- **No behaviour change:** the `metrics.rs` edit is a semantics-preserving `unwrap → expect` swap;
  the existing `cagr` unit tests (`cagr_doubling_over_two_years`) continue to pass unchanged.

## Prove-it result (positive case)

Scratch line temporarily added to `crates/cli/src/jobs/metrics.rs` inside the production `cagr`
function:

```rust
let _scratch: i64 = "1".parse::<i64>().unwrap(); // QE-267 prove-it scratch — REMOVE
```

`cargo clippy --workspace --all-targets --locked -- -D warnings` then failed with:

```
error: used `unwrap()` on a `Result` value
  --> crates/cli/src/jobs/metrics.rs:58:25
   |
58 |     let _scratch: i64 = "1".parse::<i64>().unwrap(); // QE-267 prove-it scratch — REMOVE
   |                         ^^^^^^^^^^^^^^^^^^^^^^^^^^^
   |
   = note: if this value is an `Err`, it will panic
   = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.96.0/index.html#unwrap_used
   = note: requested on the command line with `-D clippy::unwrap-used`

error: could not compile `qe-cli` (lib) due to 1 previous error
```

The scratch line was reverted immediately after; the committed tree does not contain it.

## Risks & rollback

- **Risk:** a legitimate future infallible access is forced to use `.expect(reason)` instead of
  `.unwrap()`. This is intended — it costs one string and documents the invariant. Truly hot-path
  modules already use the stricter per-module deny block.
- **Risk:** a test helper outside a `#[cfg(test)]` module (e.g. a `pub` fixture in `src`) uses
  `.unwrap()` and is not exempted. None exist today (green gate proves it); if one is added later it
  must use `.expect(...)` or move under `#[cfg(test)]`.
- **Rollback:** revert the one-line `Cargo.toml` addition (and optionally delete `clippy.toml` and
  restore the `metrics.rs` `.unwrap()`). No runtime artefact, schema, or API changes; nothing to
  migrate.

## Files changed

- `docs/backlog.md` — insert the QE-267 backlog section after QE-266.
- `Cargo.toml` — add `unwrap_used = "deny"` to `[workspace.lints.clippy]`.
- `clippy.toml` (new) — `allow-unwrap-in-tests = true`.
- `crates/cli/src/jobs/metrics.rs` — `unwrap()` → `expect("equity.len() >= 2 verified above")`.
- `docs/architecture/qe-267-unwrap-lint-enforcement-design.md` (this note).
