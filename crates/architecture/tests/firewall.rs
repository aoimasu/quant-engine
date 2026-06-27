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
    // make this test vacuously pass.
    for required in ["qe-wfo", "qe-ensemble", "qe-runtime", "qe-venue"] {
        assert!(
            graph.contains_key(required),
            "dependency graph is missing `{required}` — manifest parsing is broken, not a real pass.\n\
             parsed crates: {:?}",
            graph.keys().collect::<Vec<_>>()
        );
    }
    // Stronger sanity: a known real edge was actually parsed (qe-runtime → qe-venue), so a deps-dropping
    // parser bug (crate present but with empty deps) also cannot make this test pass vacuously.
    assert!(
        reachable(&graph, "qe-runtime").contains("qe-venue"),
        "expected `qe-runtime → qe-venue` edge was not parsed — the guard would be vacuous.\n\
         qe-runtime deps reachable: {:?}",
        reachable(&graph, "qe-runtime")
    );

    let violations = check_firewall(&graph, &firewall_rules());
    assert!(
        violations.is_empty(),
        "information-firewall breach (search ⟂ portfolio ⟂ live): {violations:#?}\n\
         A crate read an outcome it must not see — remove the forbidden dependency."
    );
}
