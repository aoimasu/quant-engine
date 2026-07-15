# QE-420 — Real code-commit provenance in vintage lineage (build-time git SHA)

Design / evidence note. Spec source of truth: `docs/reviews/2026-07-15-team-improvement-review.md` §QE-420.

## Problem (current state, with file:line)

`Lineage` binds `code_commit` as one of the four inputs that fully determine a stage's output and
its resolvable id (`crates/determinism/src/lineage.rs:14-24`, `:60-70` — the id is a SHA-256 over the
canonical JSON of the record, so `code_commit` is inside the hash).

The CLI resolves that provenance in `crates/cli/src/main.rs:17-19`:

```rust
fn code_commit() -> String {
    std::env::var("QE_CODE_COMMIT").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned())
}
```

When `QE_CODE_COMMIT` is unset this is the literal crate version `"0.1.0"`. The `Dockerfile` never
sets `QE_CODE_COMMIT`, so the default and containerised paths stamp **every** vintage from **every**
source tree with the same constant `code_commit`. Two different code states get identical
`code_commit` and can collide in lineage provenance — reproducibility is real on the config+seed axes
but a no-op on the code axis.

`code_commit()` is called from exactly one place: `run_train_command` at `main.rs:199`
(`run_train(&cfg, &cmd.opts, &code_commit(), &mut emit)`). `run_train` (`crates/cli/src/lib.rs:86-115`)
threads it into `Lineage::from_config(cfg, "", code_commit, vec![seed])`, whose id becomes the sealed
vintage id.

## Everything that depends on `code_commit` (determinism surface)

Searched `crates/` for `code_commit` / `QE_CODE_COMMIT` / `CARGO_PKG_VERSION`:

- `crates/cli/tests/train_job.rs:44-46` — the test's `lineage(seed)` passes an **explicit fixed** code
  commit `"qe-260-commit"` into `Lineage::new`; the "same seed → same vintage id/content hash"
  determinism assertions (`:196-207`) use this fixed value, not the CLI fallback.
- `crates/cli/tests/backtest_job.rs:198` — builds a lineage with explicit `"fixture-commit"`.
- `crates/cli/tests/fixtures/sample_vintage.json` and
  `crates/server/tests/fixtures/sample_vintage.json` — sealed-vintage golden fixtures with a **static**
  `"code_commit":"fixture-commit"` and a pinned `content_hash`. These are read in place
  (`crates/server/tests/read.rs`, `crates/server/tests/runs.rs:66`), never recomputed, so they do not
  depend on the runtime fallback.
- `crates/cli/tests/train.rs` — config/packaging asserts, incl. `dockerfile_runs_the_same_binary`
  (structural Dockerfile check). No vintage id is computed here.

**Conclusion: no test exercises `code_commit()`.** Every place that *computes* a vintage id at test
time passes an explicit, fixed `code_commit`, and every golden fixture stores a static one. The
`qe` binary is never invoked as a subprocess in the test suite (only `cargo metadata` /
`cargo` are, in `dependency_topology.rs` and `error/tests/hot_path_lint.rs`). Therefore changing the
`main.rs` fallback **cannot change any computed or pinned vintage id / lineage hash.** Determinism is
preserved by construction; no golden is regenerated.

## Chosen design

### 1. Build-time git SHA — zero-dependency `build.rs`

New `crates/cli/build.rs` (auto-detected; **no** `[build-dependencies]`, so `deny`, the firewall test
`qe-architecture --test firewall`, and `dependency_topology.rs` are all unaffected — build/dev deps are
filtered out of the architectural graph anyway, `dependency_topology.rs:43-49`).

It shells out to the system `git`:
- `git rev-parse --short=12 HEAD` for the SHA;
- `git status --porcelain` → append `-dirty` if the tree is non-empty;
- if `git` is absent or this is not a repo (e.g. the Docker build context ships no `.git`), fall back to
  the sentinel `"unknown"`.

It exposes the result via `cargo:rustc-env=QE_BUILD_GIT_SHA=...` and emits
`cargo:rerun-if-changed` on `.git/HEAD` + `.git/packed-refs` so a new commit re-stamps the binary.
No crate is added, so `cargo build --locked` needs no `Cargo.lock` change.

### 2. Runtime resolution precedence (`main.rs::code_commit`)

`QE_CODE_COMMIT` env override (non-empty) → build-time `QE_BUILD_GIT_SHA` (if not empty/`"unknown"`)
→ last-resort sentinel `CARGO_PKG_VERSION`. Empty `QE_CODE_COMMIT` is treated as unset so the Docker
`ENV QE_CODE_COMMIT=$QE_CODE_COMMIT` pattern degrades cleanly when the ARG is not passed. The explicit
override keeps working exactly as before.

### 3. Dockerfile stamps the build SHA

The Docker build context may not include `.git`, so `build.rs` would resolve `"unknown"` inside the
image. Add `ARG QE_CODE_COMMIT` + `ENV QE_CODE_COMMIT=$QE_CODE_COMMIT` to the runtime stage so CI/build
tooling can thread the real SHA (`docker build --build-arg QE_CODE_COMMIT=$(git rev-parse --short=12 HEAD)`).
At `docker run` the env override then wins via precedence rule (2). `train.rs::dockerfile_runs_the_same_binary`
is extended to assert the ARG is present.

### 4. `--allow-dirty` seal guard — DEFERRED

Spec marks it optional. It touches CLI arg parsing (`parse_args`) and the seal path, with real risk to
existing CLI tests, for a guard that is not required by the acceptance criteria. Deferred; the
`-dirty` suffix already makes a dirty build *visible* in provenance, which delivers the observability
without the seal-refusal behaviour change.

## Acceptance criteria mapping

- "Vintages sealed from two different commits carry different `code_commit`/lineage ids with no env
  var set" — met: the fallback is now the real short SHA, folded into `Lineage::from_config` → id.
- "The container image stamps its build SHA" — met via the ARG→ENV pattern + runtime precedence.

## Risk / rollback

- Risk: build.rs runs `git` at compile time. Handled: any failure → `"unknown"`, never a build error.
- Risk: changed default fallback altering goldens — analysed above, cannot occur (no test uses the
  fallback; all goldens pin explicit commits).
- Rollback: revert the branch; `build.rs` and the Dockerfile ARG are additive and self-contained.
