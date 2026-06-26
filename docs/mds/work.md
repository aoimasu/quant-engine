# Work — PR review tracker

Active PRs awaiting/under review for the P0/P1 ticket run. Each entry is reviewed by the
dedicated review agent, which writes `[Reviewed]`/`[Approved]` + comments inline. On merge, the
approved block is archived to `docs/mds/reviewed/<ticket>.md` and removed from here.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

## Completed (archived in `docs/mds/reviewed/`)
- QE-001 — Cargo workspace & crate topology — PR #1 — Approved & merged.
- QE-002 — Configuration system — PR #2 — Approved & merged.
- QE-003 — Structured logging & tracing — PR #3 — Approved & merged.
- QE-004 — Error model & result conventions — PR #4 — Approved & merged.
- QE-005 — CI pipeline — PR #5 — Approved & merged.
- QE-006 — Determinism & reproducibility harness — PR #6 — Approved & merged.
- QE-007 — Shared domain types — PR #7 — Approved & merged.
- QE-008 — Clock-skew / time-sync guard — PR #8 — Approved & merged.
- QE-009 — Risk-limit & kill-switch contract — PR #9 — Approved & merged.
- QE-010 — LMDB market-data store — PR #10 — Approved & merged.
- QE-011 — LMDB synthetic-data store — PR #11 — Approved & merged.
- QE-012 — Instrument-universe config & point-in-time membership — PR #12 — Approved & merged.

---

## QE-013 — Local run & deployment-agnostic packaging — PR #13 — [Ready-for-review]

- **Branch:** `qe-013/local-run-packaging`
- **PR:** https://github.com/aoimasu/quant-engine/pull/13
- **Latest commit:** `9098602`
- **Evidence/design:** `docs/architecture/qe-013-local-run-packaging-design.md`
- **Changed surface:** `crates/cli` — **new** `src/lib.rs` (`run_train`, vintage manifest, CLI
  parsing, `CliError`), `src/main.rs` (thin dispatcher, `qe` binary + `ExitCode`), **new**
  `tests/train.rs` (5 integration tests), `Cargo.toml` (+`qe-config`/`qe-determinism`/`serde`/
  `thiserror`/`tempfile`-dev, `[[bin]] name = "qe"`). Repo root — **new** `Dockerfile`,
  `config.example.toml`; rewritten `README.md`. Also bundles the QE-012 archive
  (`docs/mds/reviewed/qe-012.md`) + `docs/mds/work.md` bookkeeping — branch protection blocks direct
  `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] A clean checkout runs the training pipeline locally from the documented steps and produces a
  vintage.
- [x] Every persistent-state location is configurable; no absolute paths are hard-coded.
- [x] If a Dockerfile is provided, the image runs the same binary as the local run.

### Verification (re-run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — `qe-cli` 3 unit + 5 train integration + 1 topology; workspace green
- `cargo deny check` — advisories/bans/licenses/sources ok (no new third-party deps;
  `qe-config`/`qe-determinism` are internal path deps)
- **End-to-end run** (proof of AC #1/#2): `QE_STORAGE__ARTIFACTS_DIR=<scratch>/artifacts …
  cargo run -p qe-cli -- train --config config.example.toml` printed
  `produced vintage <64-hex> → <scratch>/artifacts/vintages/<id>/manifest.json` and wrote the
  manifest there — env overrides redirected all state, nothing absolute.

Key AC-proving tests (`crates/cli/tests/train.rs`):
- **AC #1 (one-command run produces a vintage)** — `run_train_produces_a_vintage_manifest`: a
  `run_train` against a temp-dir config writes `vintages/<id>/manifest.json`; the id is a valid
  64-hex `VintageHash`; the manifest records the full universe roster (incl. the delisted ETH — no
  survivorship drop). `example_config_loads_and_validates` covers the documented `config.example.toml`.
- **AC #1 (determinism)** — `vintage_is_deterministic_for_same_inputs`: same config + commit →
  identical vintage id and byte-identical manifest (no wall-clock); a different commit changes the id.
- **AC #2 (configurable, no absolutes)** — `all_state_is_under_configured_dirs_no_absolutes`: a run
  writes only under the configured artifacts dir; the *default* `[storage]` paths are all relative.
- **AC #3 (Docker parity, structural)** — `dockerfile_runs_the_same_binary`: the `Dockerfile` builds
  `qe-cli` and sets `ENTRYPOINT ["qe"]` — the same binary as the local run.

### Design notes for the reviewer
- **Runnable skeleton, real vintage.** The training *stages* are P1 (QE-101+); QE-013 wires the
  composition root `run_train` that loads config (QE-002) → resolves the point-in-time universe
  (QE-012) → ensures the configurable state dirs exist → writes a **content-addressed vintage
  manifest** built from a real `qe_determinism::Lineage` (QE-006): `vintage_id = SHA-256(lineage)`
  validated via `qe_domain::VintageHash`. It produces a resolvable vintage from real inputs (config
  hash + seed + universe), which is what AC #1 asks of the P0 skeleton.
- **Deterministic / reproducible.** `code_commit` is passed in (binary supplies `QE_CODE_COMMIT` →
  crate version fallback); the manifest carries **no wall-clock**, so the same config + commit
  reproduces the same vintage id and bytes. Full multi-stage vintage lineage is QE-129.
- **12-factor packaging.** All state dirs come from `[storage]` (relative defaults), env-overridable
  via `QE_STORAGE__*`; nothing absolute is baked in. The `Dockerfile` (multi-stage `rust:1.96` →
  slim) runs the same `qe` binary with `/app/data` as a mounted volume — no platform lock-in.
- **Minimal deps:** hand-rolled arg parsing (two flags) instead of pulling `clap`, matching the
  crate's existing minimal-dependency ethos.
- **Testability:** logic lives in `src/lib.rs`; `main.rs` is a thin dispatcher — so `run_train` and
  the parser are unit/integration-tested.
- **Topology:** `qe-cli` is the composition root (already depends on the pipeline crates); adding
  `qe-config`/`qe-determinism` adds no edge into `runtime`'s forbidden set → QE-001 guard green.

### Review notes

_(awaiting dedicated review agent — `start-review-ticket` against this branch/diff vs the ACs above)_
