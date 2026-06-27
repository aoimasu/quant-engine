//! qe-architecture (QE-132) — executable architectural invariants.
//!
//! The information firewall — **search ⟂ portfolio ⟂ live** — has so far been upheld by convention and
//! reviewer vigilance. This crate makes it a test: it reads the workspace's internal crate-dependency
//! graph from the manifests and asserts that no forbidden edge exists (transitively), so introducing one
//! fails `cargo test --workspace` and therefore CI (QE-132 AC).
//!
//! The graph logic is parameterised on a plain [`Graph`] so the detector can be unit-tested on synthetic
//! graphs (proving it catches a forbidden edge) independently of reading the real manifests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// An internal crate-dependency graph: crate name → the set of `qe-*` crates it depends on.
pub type Graph = BTreeMap<String, BTreeSet<String>>;

/// The workspace root (the directory containing the top-level `Cargo.toml`). This crate lives at
/// `<root>/crates/architecture`, so the root is two directories up from `CARGO_MANIFEST_DIR`.
#[must_use]
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Parse a member crate's `Cargo.toml` text into `(package name, internal qe-* dependencies)`.
///
/// Collects `qe-*` entries under `[dependencies]` and `[build-dependencies]` only — **dev-dependencies
/// are excluded** (they compile for tests only and never enter a shipped data path, so they do not breach
/// the production firewall). Matches the repo's uniform `qe-foo.workspace = true` style; an inline
/// `qe-foo = { path = … }` is still captured (the `qe-` token is read up to `.`/space/`=`).
#[must_use]
pub fn parse_manifest(text: &str) -> (Option<String>, BTreeSet<String>) {
    let mut section = String::new();
    let mut name = None;
    let mut deps = BTreeSet::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            section = line.to_string();
            continue;
        }
        if section == "[package]" && line.starts_with("name") {
            name = quoted_value(line);
        }
        let is_dep_section = section == "[dependencies]" || section == "[build-dependencies]";
        if is_dep_section && line.starts_with("qe-") {
            let dep: String = line
                .chars()
                .take_while(|c| *c != '.' && *c != ' ' && *c != '=' && *c != '\t')
                .collect();
            deps.insert(dep);
        }
    }
    (name, deps)
}

/// Extract the first double-quoted value from a `key = "value"` line.
fn quoted_value(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

/// Build the internal dependency graph from the workspace's member manifests (`crates/*/Cargo.toml`,
/// direct children only — the nested fixture crate is excluded).
///
/// # Panics
/// If the `crates/` directory cannot be read (the workspace layout is broken).
#[must_use]
pub fn dependency_graph() -> Graph {
    let crates_dir = workspace_root().join("crates");
    let mut graph = Graph::new();
    let entries = std::fs::read_dir(&crates_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", crates_dir.display()));
    for entry in entries.flatten() {
        let manifest = entry.path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue; // not a crate directory
        };
        let (name, deps) = parse_manifest(&text);
        if let Some(name) = name {
            graph.insert(name, deps);
        }
    }
    graph
}

/// The set of crates reachable from `start` over the internal edges (transitive closure, excluding
/// `start` itself).
#[must_use]
pub fn reachable(graph: &Graph, start: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<String> = graph.get(start).into_iter().flatten().cloned().collect();
    while let Some(node) = stack.pop() {
        if seen.insert(node.clone()) {
            if let Some(next) = graph.get(&node) {
                stack.extend(next.iter().cloned());
            }
        }
    }
    seen
}

/// A firewall rule: `upstream` must not (transitively) depend on any crate in `forbidden`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirewallRule {
    /// The upstream crate that must not read the forbidden outcomes.
    pub upstream: &'static str,
    /// The crates whose outcomes it must not be able to reach.
    pub forbidden: &'static [&'static str],
}

/// The information-firewall rules: **search ⟂ portfolio ⟂ live** (QE-001/QE-132). Search (`qe-wfo`) may
/// read neither portfolio (`qe-ensemble`) nor live (`qe-runtime`/`qe-venue`); portfolio may read neither
/// search nor live. (Live reading search/portfolio *outputs* is the allowed downstream direction.)
#[must_use]
pub fn firewall_rules() -> Vec<FirewallRule> {
    vec![
        FirewallRule {
            upstream: "qe-wfo",
            forbidden: &["qe-ensemble", "qe-runtime", "qe-venue"],
        },
        FirewallRule {
            upstream: "qe-ensemble",
            forbidden: &["qe-wfo", "qe-runtime", "qe-venue"],
        },
    ]
}

/// A detected firewall breach: `upstream` can reach `forbidden`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The upstream crate.
    pub upstream: String,
    /// The forbidden crate it can reach.
    pub forbidden: String,
}

/// Every firewall breach in `graph` under `rules` (empty ⇒ the firewall holds). Reachability is
/// transitive, so an indirect `wfo → … → ensemble` path is caught like a direct edge.
#[must_use]
pub fn check_firewall(graph: &Graph, rules: &[FirewallRule]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for rule in rules {
        let reach = reachable(graph, rule.upstream);
        for &forbidden in rule.forbidden {
            if reach.contains(forbidden) {
                violations.push(Violation {
                    upstream: rule.upstream.to_string(),
                    forbidden: forbidden.to_string(),
                });
            }
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_of(edges: &[(&str, &[&str])]) -> Graph {
        edges
            .iter()
            .map(|(k, vs)| (k.to_string(), vs.iter().map(|s| s.to_string()).collect()))
            .collect()
    }

    #[test]
    fn parses_name_and_internal_deps_excluding_dev() {
        let toml = "\
[package]
name = \"qe-wfo\"

[dependencies]
qe-domain.workspace = true
serde.workspace = true
qe-signal.workspace = true

[dev-dependencies]
qe-ensemble.workspace = true
";
        let (name, deps) = parse_manifest(toml);
        assert_eq!(name.as_deref(), Some("qe-wfo"));
        // dev-dependency qe-ensemble is NOT counted; non-qe serde is ignored.
        assert_eq!(
            deps,
            ["qe-domain", "qe-signal"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
    }

    #[test]
    fn reachable_is_transitive() {
        let g = graph_of(&[("qe-wfo", &["qe-signal"]), ("qe-signal", &["qe-domain"])]);
        let r = reachable(&g, "qe-wfo");
        assert!(r.contains("qe-signal") && r.contains("qe-domain"));
        assert!(!r.contains("qe-wfo")); // excludes itself
    }

    #[test]
    fn detects_a_direct_forbidden_edge() {
        let g = graph_of(&[("qe-wfo", &["qe-ensemble"]), ("qe-ensemble", &[])]);
        let v = check_firewall(&g, &firewall_rules());
        assert!(v.contains(&Violation {
            upstream: "qe-wfo".into(),
            forbidden: "qe-ensemble".into(),
        }));
    }

    #[test]
    fn detects_a_transitive_forbidden_edge() {
        // qe-wfo → qe-mid → qe-runtime: an indirect path is still a breach.
        let g = graph_of(&[
            ("qe-wfo", &["qe-mid"]),
            ("qe-mid", &["qe-runtime"]),
            ("qe-runtime", &[]),
        ]);
        let v = check_firewall(&g, &firewall_rules());
        assert!(v.contains(&Violation {
            upstream: "qe-wfo".into(),
            forbidden: "qe-runtime".into(),
        }));
    }

    #[test]
    fn a_clean_graph_has_no_violations() {
        let g = graph_of(&[
            ("qe-wfo", &["qe-domain", "qe-signal"]),
            ("qe-ensemble", &["qe-domain", "qe-signal"]),
            ("qe-runtime", &["qe-venue"]),
        ]);
        assert!(check_firewall(&g, &firewall_rules()).is_empty());
    }
}
