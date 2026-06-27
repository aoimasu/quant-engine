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
/// Collects `qe-*` production dependencies — entries under `[dependencies]` / `[build-dependencies]`,
/// their dependency-**table** forms (`[dependencies.qe-foo]`), and platform variants
/// (`[target.'cfg(…)'.dependencies]` and `[target.'cfg(…)'.dependencies.qe-foo]`). **Dev-dependencies are
/// excluded** in every form (they compile for tests only and never enter a shipped data path, so they do
/// not breach the production firewall). Section headers are classified structurally (a quote-aware
/// dotted-key parse), so the dependency-table / platform / inline forms are all caught — not just the
/// repo's usual `qe-foo.workspace = true` lines.
#[must_use]
pub fn parse_manifest(text: &str) -> (Option<String>, BTreeSet<String>) {
    let mut kind = SectionKind::Other;
    let mut name = None;
    let mut deps = BTreeSet::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            kind = classify_section(line);
            // A `[dependencies.qe-foo]` table names the dependency in the header itself.
            if let SectionKind::ProdDepTable(dep) = &kind {
                if dep.starts_with("qe-") {
                    deps.insert(dep.clone());
                }
            }
            continue;
        }
        match kind {
            SectionKind::Package if line.starts_with("name") => {
                name = quoted_value(line);
            }
            SectionKind::ProdDeps if line.starts_with("qe-") => {
                let dep: String = line
                    .chars()
                    .take_while(|c| *c != '.' && *c != ' ' && *c != '=' && *c != '\t')
                    .collect();
                deps.insert(dep);
            }
            _ => {}
        }
    }
    (name, deps)
}

/// The role of a `Cargo.toml` section for firewall parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SectionKind {
    /// `[package]` — read the `name`.
    Package,
    /// `[dependencies]` / `[build-dependencies]` / `[target.*.dependencies]` — inner `qe-*` lines are deps.
    ProdDeps,
    /// `[dependencies.qe-foo]` / `[target.*.build-dependencies.qe-foo]` — the header names the dep.
    ProdDepTable(String),
    /// Anything else (incl. every `dev-dependencies` form — excluded from the production firewall).
    Other,
}

/// Classify a `[section.header]` line. A section is a production-dependency section iff it contains a
/// `dependencies` or `build-dependencies` path segment and **no** `dev-dependencies` segment; a segment
/// after that names a dependency-table entry.
fn classify_section(header: &str) -> SectionKind {
    let inner = header.trim().trim_start_matches('[').trim_end_matches(']');
    let segs = split_dotted_key(inner);
    // Exactly `[package]` — not `[package.metadata.*]`, whose `name` keys must not be read as the crate.
    if segs.len() == 1 && segs[0] == "package" {
        return SectionKind::Package;
    }
    if segs.iter().any(|s| s == "dev-dependencies") {
        return SectionKind::Other; // every dev-dependency form is excluded
    }
    if let Some(pos) = segs
        .iter()
        .position(|s| s == "dependencies" || s == "build-dependencies")
    {
        return match segs.get(pos + 1) {
            Some(dep_name) => SectionKind::ProdDepTable(dep_name.clone()),
            None => SectionKind::ProdDeps,
        };
    }
    SectionKind::Other
}

/// Split a dotted TOML key into its segments, respecting single/double-quoted segments (so the dots
/// inside a `target.'cfg(...)'` predicate do not split it). Quotes are stripped; segments trimmed.
fn split_dotted_key(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in inner.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '.' => out.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            },
        }
    }
    out.push(cur);
    out.iter().map(|s| s.trim().to_string()).collect()
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
    fn catches_dependency_table_and_platform_forms() {
        // Regression for the pass-1 blocker: the dependency-TABLE form and platform deps are valid
        // production dependencies cargo recognises, and must be caught — while their dev variants are not.
        let toml = "\
[package]
name = \"qe-wfo\"

[dependencies.qe-ensemble]
workspace = true

[build-dependencies.qe-vintage]
workspace = true

[target.'cfg(unix)'.dependencies]
qe-runtime.workspace = true

[target.'cfg(windows)'.dependencies.qe-venue]
workspace = true

[dev-dependencies.qe-validation]
workspace = true

[target.'cfg(unix)'.dev-dependencies]
qe-storage.workspace = true
";
        let (_, deps) = parse_manifest(toml);
        // All four production forms detected…
        for expected in ["qe-ensemble", "qe-vintage", "qe-runtime", "qe-venue"] {
            assert!(
                deps.contains(expected),
                "missed production dep {expected}: {deps:?}"
            );
        }
        // …and both dev forms excluded.
        assert!(
            !deps.contains("qe-validation") && !deps.contains("qe-storage"),
            "{deps:?}"
        );
    }

    #[test]
    fn package_metadata_name_is_not_read_as_the_crate_name() {
        // `[package.metadata.*]` is not `[package]`; its `name` key must not become the crate name.
        let toml = "\
[package]
name = \"qe-wfo\"

[package.metadata.deb]
name = \"some-debian-package\"
";
        let (name, _) = parse_manifest(toml);
        assert_eq!(name.as_deref(), Some("qe-wfo"));
    }

    #[test]
    fn split_dotted_key_respects_quotes() {
        assert_eq!(
            split_dotted_key("target.'cfg(unix)'.dependencies"),
            vec!["target", "cfg(unix)", "dependencies"]
        );
        assert_eq!(
            split_dotted_key("dependencies.qe-ensemble"),
            vec!["dependencies", "qe-ensemble"]
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
