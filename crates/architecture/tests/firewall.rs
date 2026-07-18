//! QE-132 AC: the information firewall is a CI test, not a convention.
//!
//! Builds the **real** internal dependency graph from the workspace manifests and asserts no forbidden
//! edge exists. Introducing one (e.g. adding `qe-ensemble` to `qe-wfo`'s `[dependencies]`) makes this
//! test fail under `cargo test --workspace`, and therefore fails CI.

use qe_architecture::{check_firewall, dependency_graph, firewall_rules, reachable};

#[test]
fn firewall_holds_in_the_workspace() {
    let graph = dependency_graph();

    // Sanity: the graph really was parsed (the constrained crates are present), so a parse failure can't
    // make this test vacuously pass. `qe-server` (QE-254) is included so the firewall rule guarding the
    // second composition root actually covers a crate that exists in the graph.
    for required in [
        "qe-wfo",
        "qe-ensemble",
        "qe-runtime",
        "qe-venue",
        "qe-server",
        // QE-426: the split crates must be present so their firewall rules cover crates that exist.
        "qe-runtime-core",
        "qe-hedger",
        "qe-edge",
        // QE-452 Phase A: the frozen formula-pool artefact crate, guarded by its own pool ⟂ live rule.
        "qe-formula-pool",
    ] {
        assert!(
            graph.contains_key(required),
            "dependency graph is missing `{required}` — manifest parsing is broken, not a real pass.\n\
             parsed crates: {:?}",
            graph.keys().collect::<Vec<_>>()
        );
    }
    // Stronger sanity: known real edges were actually parsed, so a deps-dropping parser bug (crate
    // present but with empty deps) also cannot make this test pass vacuously — `qe-runtime → qe-venue`
    // exercises the live side, and `qe-server → qe-telemetry` proves the second composition root's
    // internal edges are seen (so its firewall rule is non-vacuous).
    assert!(
        reachable(&graph, "qe-runtime").contains("qe-venue"),
        "expected `qe-runtime → qe-venue` edge was not parsed — the guard would be vacuous.\n\
         qe-runtime deps reachable: {:?}",
        reachable(&graph, "qe-runtime")
    );
    assert!(
        reachable(&graph, "qe-server").contains("qe-telemetry"),
        "expected `qe-server → qe-telemetry` edge was not parsed — the qe-server firewall rule would be \
         vacuous.\n qe-server deps reachable: {:?}",
        reachable(&graph, "qe-server")
    );
    // QE-426 non-vacuity: known real edges of the split crates were actually parsed, so their new firewall
    // rules cannot pass vacuously. The order path reaches the venue (`qe-edge → qe-venue`), the planner
    // reaches the sealed vintage (`qe-hedger → qe-vintage`), the shared contract reaches the money
    // primitives (`qe-runtime-core → qe-domain`), and the facade reaches the order path (`qe-runtime →
    // qe-edge`).
    for (from, to) in [
        ("qe-edge", "qe-venue"),
        ("qe-hedger", "qe-vintage"),
        ("qe-runtime-core", "qe-domain"),
        ("qe-runtime", "qe-edge"),
        // QE-452 Phase A: the composition root reaches the pool artefact, so the pool ⟂ live rule (which
        // constrains `qe-formula-pool`) sits on a crate that a real edge actually reaches — non-vacuous.
        ("qe-cli", "qe-formula-pool"),
        // QE-452 Phase B: the admin-UI backend now reads formula pools + drives their governance lifecycle,
        // so `qe-server → qe-formula-pool` is a real edge. Asserting it is parsed proves the `qe-server`
        // firewall rule (no `qe-runtime`/`qe-venue` edge) covers the crate through which the pool routes
        // land — the pool code stays server+pool-side, never regressing onto the live path.
        ("qe-server", "qe-formula-pool"),
    ] {
        assert!(
            reachable(&graph, from).contains(to),
            "expected `{from} → {to}` edge was not parsed — the split-crate guard would be vacuous.\n\
             {from} deps reachable: {:?}",
            reachable(&graph, from)
        );
    }

    let violations = check_firewall(&graph, &firewall_rules());
    assert!(
        violations.is_empty(),
        "information-firewall breach (search ⟂ portfolio ⟂ live): {violations:#?}\n\
         A crate read an outcome it must not see — remove the forbidden dependency."
    );
}
