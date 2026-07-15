# QE-405 — Extend the firewall guard: `qe-runtime` / `qe-vintage` ⊬ training crates

- **Ticket:** QE-405 (P1, cross-cutting / architecture / firewall). Authoritative spec:
  `docs/reviews/2026-07-15-team-improvement-review.md` → section `### QE-405` (there is no
  `docs/mds/tickets/QE-405.md`).
- **Depends on:** QE-132 (the executable-firewall crate).
- **Effort:** S. **Goal:** make the train/live decoupling machine-enforced, not prose-only.

## Why

`firewall_rules()` (`crates/architecture/src/lib.rs:202-217`) constrains only `qe-wfo`,
`qe-ensemble`, and `qe-server`. It has **no rule** stopping `qe-runtime` / `qe-vintage` from
depending on `qe-wfo` / `qe-ensemble`. The train/live decoupling (QE-001) is asserted for those two
crates only in prose and Cargo comments (`crates/runtime/Cargo.toml:20-22`,
`crates/vintage/Cargo.toml:13-17`). An engineer adding `qe-wfo.workspace = true` to `qe-runtime` —
pulling the whole search tree (and `rayon`) into the live binary — passes `cargo test --workspace`
today. The invariant most load-bearing for live determinism/footprint is the one the executable
guard omits.

## Current-state evidence (file:line)

### The existing rules (only three upstreams constrained)

`crates/architecture/src/lib.rs:202-217`:

```
qe-wfo      ⊬ { qe-ensemble, qe-runtime, qe-venue }
qe-ensemble ⊬ { qe-wfo, qe-runtime, qe-venue }
qe-server   ⊬ { qe-runtime, qe-venue }
```

No `qe-runtime` or `qe-vintage` upstream rule exists.

### The graph really is clean today (runtime/vintage do NOT reach wfo/ensemble)

Direct deps (from the manifests):

- `qe-runtime` (`crates/runtime/Cargo.toml:14-26`): `qe-domain`, `qe-risk`, `qe-signal`,
  `qe-storage`, `qe-venue`, `qe-vintage`, `qe-determinism`. **No `qe-wfo`, no `qe-ensemble`.**
- `qe-vintage` (`crates/vintage/Cargo.toml:14-21`): `qe-signal`, `qe-risk`, `qe-determinism`.
  **No `qe-wfo`, no `qe-ensemble`.**

Transitive check — none of the crates reachable from runtime/vintage pull in `qe-wfo`/`qe-ensemble`:
`qe-signal`, `qe-risk`, `qe-storage`, `qe-venue`, `qe-domain`, `qe-determinism` each contain no
`qe-wfo`/`qe-ensemble` production dependency (grep of each `Cargo.toml`). So the two new rules pass
on today's tree with no code moves.

### Known-good edge for the non-vacuity assertion

`crates/vintage/Cargo.toml:17` — `qe-vintage` depends on `qe-signal` (the genome is embedded via the
shared `qe-signal` crate, not `qe-wfo`). This is the real edge the new sanity assertion will require
the parser to have seen, mirroring the existing `qe-runtime → qe-venue` /
`qe-server → qe-telemetry` checks in `crates/architecture/tests/firewall.rs:34-45`.

### Data shapes to match (do not guess)

- `FirewallRule { upstream: &'static str, forbidden: &'static [&'static str] }`
  (`crates/architecture/src/lib.rs:186-192`).
- Rules constructed as a `vec![ FirewallRule { .. }, .. ]` in `firewall_rules()`.
- Test uses `dependency_graph()`, `reachable(&graph, start)`, `check_firewall(&graph, &rules)`,
  and a required-crates presence loop + explicit `reachable(...).contains(...)` non-vacuity asserts.

## Decision

1. Append two `FirewallRule`s to `firewall_rules()`:
   - `qe-runtime  ⊬ { qe-wfo, qe-ensemble }`
   - `qe-vintage  ⊬ { qe-wfo, qe-ensemble }`
   Existing rules are left unchanged (out of scope to touch them).
2. Add `qe-vintage` to the required-crates presence loop in the firewall test and add a non-vacuity
   assertion `reachable(&graph, "qe-vintage").contains("qe-signal")`, mirroring the
   `qe-runtime → qe-venue` style so the new rules cannot pass vacuously if dependency parsing breaks.
3. Update the crate-level doc / the `firewall_rules()` doc-comment to state:
   - `qe-cli` is the only crate that legitimately links both the training and live sides
     (the composition root);
   - the firewall is a **library-level** (compile/link-graph) guarantee, not a process-level one.

## Test plan

- `cargo test -p qe-architecture --test firewall --locked` — the core proof; must pass with the new
  rules on the clean tree (the two new upstreams have zero forbidden reach today).
- Existing unit tests in `crates/architecture/src/lib.rs` (`detects_a_direct_forbidden_edge`, etc.)
  still exercise the generic detector against `firewall_rules()`; the added rules keep them green
  because their synthetic graphs contain no runtime/vintage→wfo/ensemble edge.
- Full green gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -D
  warnings`, `cargo test --workspace --locked`.
- Acceptance-criteria manual sanity (not committed): adding `qe-wfo` to `qe-runtime`'s deps makes
  the firewall test fail — verified by reasoning about `check_firewall` (the new rule's `forbidden`
  contains `qe-wfo`, which would then be reachable). Left unverified-by-mutation to avoid dirtying
  the tree; the rule/relationship is identical in shape to the proven `qe-wfo ⊬ qe-ensemble` rule.

## Risks

- **Low.** Pure additive guard rules + test/doc text. No production crate manifests change, no code
  moves. Worst case is a false-positive firewall failure, which cannot happen on today's clean tree
  (evidence above).
- `cargo-deny` and GitHub Actions are not runnable locally; `deny` is a required check and runs in
  CI. This change does not alter dependencies, so `deny` is unaffected.

## Rollback

Revert the single commit; the guard returns to its prior three-rule form. No data/format migration.
