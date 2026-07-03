//! Architectural guard for the QE-001 decoupling invariant.
//!
//! `runtime` must never depend (transitively) on `wfo`/`ensemble`, and `wfo`/`ensemble` must
//! never depend on `runtime`. The only code shared between the training and runtime sides
//! crosses through `signal`/`domain`. This test walks the workspace-local dependency graph
//! reported by `cargo metadata` and fails the build the moment a forbidden edge is added.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

/// Build the workspace-local dependency graph: crate name -> set of workspace crates it
/// directly depends on. `--no-deps` restricts `packages` to workspace members, so every
/// dependency we keep is itself a member.
fn workspace_dep_graph() -> BTreeMap<String, BTreeSet<String>> {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .expect("failed to run `cargo metadata`");
    assert!(
        output.status.success(),
        "`cargo metadata` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("failed to parse cargo metadata JSON");
    let packages = meta["packages"]
        .as_array()
        .expect("metadata.packages should be an array");

    let members: BTreeSet<String> = packages
        .iter()
        .map(|p| p["name"].as_str().expect("package name").to_owned())
        .collect();

    let mut graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for pkg in packages {
        let name = pkg["name"].as_str().expect("package name").to_owned();
        let deps: BTreeSet<String> = pkg["dependencies"]
            .as_array()
            .expect("package dependencies array")
            .iter()
            // Normal deps only: `kind` is null for normal, "dev"/"build" otherwise. Dev/build
            // deps don't ship in the binary, so they create no pipeline coupling and must not
            // count as architectural edges (e.g. a test-only fixture dep is fine).
            .filter(|d| d["kind"].is_null())
            .filter_map(|d| d["name"].as_str())
            .map(str::to_owned)
            .filter(|d| members.contains(d)) // keep only workspace-internal edges
            .collect();
        graph.insert(name, deps);
    }
    graph
}

/// Transitive closure of `start` over the workspace dependency graph (excluding `start`).
fn reachable(graph: &BTreeMap<String, BTreeSet<String>>, start: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![start.to_owned()];
    while let Some(node) = stack.pop() {
        if let Some(deps) = graph.get(&node) {
            for dep in deps {
                if seen.insert(dep.clone()) {
                    stack.push(dep.clone());
                }
            }
        }
    }
    seen.remove(start);
    seen
}

fn assert_no_dependency(graph: &BTreeMap<String, BTreeSet<String>>, from: &str, forbidden: &str) {
    let reach = reachable(graph, from);
    assert!(
        !reach.contains(forbidden),
        "decoupling invariant violated: `{from}` transitively depends on `{forbidden}` \
         (reachable: {reach:?}). Training (wfo/ensemble) and runtime must only share code via \
         signal/domain."
    );
}

#[test]
fn runtime_is_decoupled_from_training() {
    let graph = workspace_dep_graph();

    // sanity: the crates we reason about exist
    for required in [
        "qe-runtime",
        "qe-wfo",
        "qe-ensemble",
        "qe-signal",
        "qe-domain",
        "qe-server",
    ] {
        assert!(graph.contains_key(required), "missing crate {required}");
    }

    // runtime must not reach the training crates
    assert_no_dependency(&graph, "qe-runtime", "qe-wfo");
    assert_no_dependency(&graph, "qe-runtime", "qe-ensemble");

    // training crates must not reach runtime
    assert_no_dependency(&graph, "qe-wfo", "qe-runtime");
    assert_no_dependency(&graph, "qe-ensemble", "qe-runtime");

    // QE-254 / ADR D4a: `qe-server` is a second composition root (admin-UI backend). It reuses the
    // training-side + shared crates but must stay off the live trading path — no transitive edge to
    // `qe-runtime`/`qe-venue` — so its async runtime never links the live venue side.
    assert_no_dependency(&graph, "qe-server", "qe-runtime");
    assert_no_dependency(&graph, "qe-server", "qe-venue");
}
