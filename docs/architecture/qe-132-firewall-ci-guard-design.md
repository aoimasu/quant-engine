# QE-132 — Information-firewall CI guard — design note

`Phase: P1` · `Area: cross-cutting` · `Depends on: QE-123, QE-126`
`Branch: qe-132/firewall-ci-guard`

## Goal (from backlog)

The firewall (search ⟂ portfolio ⟂ live) is an architectural invariant; make it a **test, not a
convention**.

- A CI/architectural test asserting WFO cannot read portfolio/live outcomes and ensemble cannot read live
  outcomes (no forbidden dependencies / data paths).

**Acceptance criteria.**
- [ ] Introducing a forbidden dependency fails CI.

**Out of scope.** Runtime firewall direction (covered by crate topology, QE-001).

## Current-state evidence

- The firewall has so far been upheld by convention + reviewer vigilance (every QE-126/127/128 review
  hand-checked that `crates/ensemble/Cargo.toml` has no `qe-wfo`; QE-129/131 that the output crates add no
  `qe-ensemble`). This ticket turns that into an executable invariant.
- Current internal topology (parsed `[dependencies]`): `qe-wfo → {domain, signal, storage, determinism}`,
  `qe-ensemble → {domain, signal, storage, determinism}` (no `qe-wfo`), `qe-runtime → {domain, risk,
  signal, storage, venue}`, `qe-venue → {domain}`. The firewall holds today.
- Precedent for architectural tests: `crates/error/tests/hot_path_lint.rs` runs `cargo clippy` against a
  fixture and asserts failure. QE-132 is the same spirit for the dependency graph.

## Design

### D1 — Where: a dedicated `qe-architecture` crate

The firewall is genuinely cross-cutting (it constrains `wfo`/`ensemble`/`runtime`), so it does not belong
to any one domain crate. A small **`qe-architecture`** crate hosts architectural-invariant logic + tests.
It picks up automatically via `members = ["crates/*"]`, so its tests run under `cargo test --workspace`
(what CI runs) — a real forbidden dependency makes that test fail, i.e. **fails CI** (the AC). It depends
on no internal crate (pure `std` filesystem + parsing), so it cannot itself perturb the graph.

### D2 — Build the internal dependency graph from the manifests

`dependency_graph()` globs `crates/*/Cargo.toml` (direct children only — the nested `hotpath_violation`
fixture is excluded), and for each parses the package `name` and the `qe-*` entries under
`[dependencies]` and `[build-dependencies]`. **Dev-dependencies are deliberately excluded**: they compile
only for tests and never enter a shipped data path, so they do not breach the production firewall (and
forbidding them would block a legitimate cross-crate test fixture). The parse is line-based against the
repo's uniform `qe-foo.workspace = true` style — no `toml` dependency.

### D3 — The firewall rules (transitive)

`reachable(graph, crate)` is the transitive closure over internal edges. `check_firewall(graph, rules)`
returns every violation where a forbidden crate is reachable from an upstream crate, with the offending
path. The rules encode **search ⟂ portfolio ⟂ live**:

| Upstream (may not read) | Forbidden (the outcome it must not see) | Why |
|---|---|---|
| `qe-wfo` (search) | `qe-ensemble` | search ⊥ portfolio |
| `qe-wfo` (search) | `qe-runtime`, `qe-venue` | search ⊥ live |
| `qe-ensemble` (portfolio) | `qe-wfo` | portfolio ⊥ search (the firewall is symmetric ⟂) |
| `qe-ensemble` (portfolio) | `qe-runtime`, `qe-venue` | portfolio ⊥ live |

Transitivity matters: `wfo → X → ensemble` is as forbidden as a direct edge, so the closure (not just
direct deps) is checked. Live (`runtime`/`venue`) reading search/portfolio *outputs* is the allowed
downstream direction and is not constrained here (QE-001 owns runtime direction).

### D4 — Tests prove the guard bites

- `firewall_holds_in_the_workspace` — builds the **real** graph from disk; asserts zero violations. This
  is the live guard: add `qe-ensemble` to `qe-wfo` and it fails.
- `detects_a_direct_forbidden_edge` / `detects_a_transitive_forbidden_edge` — run `check_firewall` on
  **synthetic** graphs containing `wfo→ensemble` and `wfo→mid→runtime`; assert each is reported. This
  proves the detector is non-vacuous (the AC is about *catching* a forbidden dependency, so the catching
  is tested directly without committing a real violation).
- `reachable_is_transitive` — closure sanity (`wfo` reaches `domain`).

## Module / API plan

New crate `crates/architecture` (`qe-architecture`), no internal deps:
- `workspace_root()`, `dependency_graph() -> Graph` (`BTreeMap<String, BTreeSet<String>>`),
  `reachable(&Graph, &str) -> BTreeSet<String>`, `FirewallRule { upstream, forbidden }`,
  `firewall_rules() -> Vec<FirewallRule>`, `Violation { upstream, forbidden, via }`,
  `check_firewall(&Graph, &[FirewallRule]) -> Vec<Violation>`.

## Test plan (TDD)

Covered by D4 — real-graph guard (passes now, fails on a forbidden edge), synthetic direct + transitive
detection, closure sanity. `cargo test -p qe-architecture`, `cargo test --workspace`.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-architecture`,
`cargo test --workspace`, `cargo deny check`.

## Risks

- **Parser scope.** Line-based parsing assumes the repo's `qe-foo.workspace = true` convention; a future
  inline `qe-foo = { path = … }` in a member is still caught (the `qe-` token is read up to `.`/` `/`=`).
  Documented; a `toml`-crate parse is a drop-in upgrade behind `dependency_graph()` if the style diverges.
- **Dev-dependency exclusion** is deliberate (D2) — noted so a future reviewer doesn't read it as a gap.
- **New-crate placement.** The firewall must not depend on any internal crate or it could mask a path;
  `qe-architecture` has zero internal deps by construction (and the real-graph test would show it).
