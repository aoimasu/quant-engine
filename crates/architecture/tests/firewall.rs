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
        "qe-vintage",
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
    // QE-405: `qe-vintage → qe-signal` is the real edge by which the vintage reaches genome logic
    // (never via `qe-wfo`). Asserting it was parsed proves the new `qe-vintage ⊬ {qe-wfo, qe-ensemble}`
    // rule is non-vacuous — a deps-dropping parser bug can't make the live-side guard pass for free.
    assert!(
        reachable(&graph, "qe-vintage").contains("qe-signal"),
        "expected `qe-vintage → qe-signal` edge was not parsed — the qe-vintage firewall rule would be \
         vacuous.\n qe-vintage deps reachable: {:?}",
        reachable(&graph, "qe-vintage")
    );

    let violations = check_firewall(&graph, &firewall_rules());
    assert!(
        violations.is_empty(),
        "information-firewall breach (search ⟂ portfolio ⟂ live): {violations:#?}\n\
         A crate read an outcome it must not see — remove the forbidden dependency."
    );
}
