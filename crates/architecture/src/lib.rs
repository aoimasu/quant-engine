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
///
/// **Edges resolve by the dependency's TARGET, not the manifest key (QE-489).** A `package = "qe-…"`
/// rename (`foo = { package = "qe-live" }`) is recorded as an edge to its real package, and a workspace
/// `path = "../<crate>"` dep is recorded as an internal edge to that crate — *regardless of the key's
/// prefix* — so an internal crate pulled under a non-`qe-` key cannot bypass the firewall. Both the
/// dependency-table (`[dependencies.foo]` with a `package`/`path` body) and inline-table
/// (`foo = { package = … }`) forms are handled, not only the bare `qe-foo.workspace = true` shape.
#[must_use]
pub fn parse_manifest(text: &str) -> (Option<String>, BTreeSet<String>) {
    parse_manifest_with_aliases(text, &BTreeMap::new())
}

/// Like [`parse_manifest`], but resolves a member's `key.workspace = true` line whose key is **not**
/// `qe-`-prefixed through the root `[workspace.dependencies]` `aliases` map (QE-489). This closes the last
/// bypass: an internal crate aliased under a non-`qe-` root key (`sneaky = { package = "qe-runtime" }`)
/// and pulled by a member as `sneaky.workspace = true` carries no `package`/`path` on the member line, so
/// only the root alias reveals its real target. Build `aliases` with [`workspace_alias_targets`] from the
/// root manifest; [`dependency_graph`] does this. An empty map reduces to the bare-key behaviour.
#[must_use]
fn parse_manifest_with_aliases(
    text: &str,
    aliases: &BTreeMap<String, String>,
) -> (Option<String>, BTreeSet<String>) {
    let mut kind = SectionKind::Other;
    let mut name = None;
    let mut deps = BTreeSet::new();
    // Pending dependency-table (`[dependencies.<key>]`): the key comes from the header, but its
    // `package`/`path` target may appear on later body lines, so it is finalised at the next header/EOF.
    let mut table: Option<PendingTable> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            finalize_table(&mut deps, table.take(), aliases);
            kind = classify_section(line);
            // A `[dependencies.<key>]` table names the dependency KEY in the header; its target
            // (`package`/`path`) is resolved from the body lines that follow.
            if let SectionKind::ProdDepTable(key) = &kind {
                table = Some(PendingTable {
                    key: key.clone(),
                    package: None,
                    path: None,
                });
            }
            continue;
        }
        match kind {
            SectionKind::Package if line.starts_with("name") => {
                name = quoted_value(line);
            }
            // Inline dependency line (`qe-foo.workspace = true`, `foo = { package = "qe-live" }`,
            // `foo = { path = "../live" }`): resolve the edge by its target.
            SectionKind::ProdDeps => {
                let key: String = line
                    .chars()
                    .take_while(|c| *c != '.' && *c != ' ' && *c != '=' && *c != '\t')
                    .collect();
                if !key.is_empty() {
                    let package = assigned_value(line, "package");
                    let path = assigned_value(line, "path");
                    if let Some(target) =
                        resolve_internal_target(&key, package.as_deref(), path.as_deref(), aliases)
                    {
                        deps.insert(target);
                    }
                }
            }
            // Body of a `[dependencies.<key>]` table: capture its `package`/`path` target.
            SectionKind::ProdDepTable(_) => {
                if let Some(t) = table.as_mut() {
                    if let Some(v) = assigned_value(line, "package") {
                        t.package = Some(v);
                    }
                    if let Some(v) = assigned_value(line, "path") {
                        t.path = Some(v);
                    }
                }
            }
            _ => {}
        }
    }
    finalize_table(&mut deps, table, aliases);
    (name, deps)
}

/// Parse the **root** `Cargo.toml`'s `[workspace.dependencies]` table into `alias key → internal target
/// crate`, for every entry that resolves to a workspace-member crate — via a `package = "qe-…"` rename or a
/// `path` into the workspace (QE-489). A member's `key.workspace = true` line inherits this target, so an
/// internal crate aliased under a **non-`qe-` root key** (e.g. `sneaky = { package = "qe-runtime" }`) is
/// still resolved to its real crate and cannot slip past the firewall. External aliases (`serde`, `tokio`,
/// …) resolve to nothing and are omitted. Both the inline (`key = { package = … }`) and dependency-table
/// (`[workspace.dependencies.key]`) forms are handled.
#[must_use]
fn workspace_alias_targets(root_text: &str) -> BTreeMap<String, String> {
    /// Record `t`'s alias→internal-target edge into `out`, if it resolves internal. Root aliases resolve
    /// on their own `package`/`path` (never through another alias), so an empty alias map is passed.
    fn record(t: PendingTable, out: &mut BTreeMap<String, String>) {
        if let Some(target) = resolve_internal_target(
            &t.key,
            t.package.as_deref(),
            t.path.as_deref(),
            &BTreeMap::new(),
        ) {
            out.insert(t.key, target);
        }
    }

    let mut aliases = BTreeMap::new();
    // Whether we are inside `[workspace.dependencies]` (inline entries) …
    let mut in_ws_deps = false;
    // … or inside a `[workspace.dependencies.<key>]` table (its `package`/`path` come from body lines).
    let mut table: Option<PendingTable> = None;
    for raw in root_text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            if let Some(t) = table.take() {
                record(t, &mut aliases);
            }
            let inner = line.trim_start_matches('[').trim_end_matches(']');
            let segs = split_dotted_key(inner);
            in_ws_deps = false;
            if segs.len() >= 2 && segs[0] == "workspace" && segs[1] == "dependencies" {
                match segs.get(2) {
                    None => in_ws_deps = true,
                    Some(key) => {
                        table = Some(PendingTable {
                            key: key.clone(),
                            package: None,
                            path: None,
                        });
                    }
                }
            }
            continue;
        }
        if in_ws_deps {
            let key: String = line
                .chars()
                .take_while(|c| *c != '.' && *c != ' ' && *c != '=' && *c != '\t')
                .collect();
            if !key.is_empty() {
                let package = assigned_value(line, "package");
                let path = assigned_value(line, "path");
                record(PendingTable { key, package, path }, &mut aliases);
            }
        } else if let Some(t) = table.as_mut() {
            if let Some(v) = assigned_value(line, "package") {
                t.package = Some(v);
            }
            if let Some(v) = assigned_value(line, "path") {
                t.path = Some(v);
            }
        }
    }
    if let Some(t) = table {
        record(t, &mut aliases);
    }
    aliases
}

/// A `[dependencies.<key>]` table awaiting its target: the header key plus any `package`/`path` read from
/// the body lines.
struct PendingTable {
    key: String,
    package: Option<String>,
    path: Option<String>,
}

/// Finalise a pending dependency-table into `deps`, resolving its edge by target (QE-489). `aliases`
/// resolves a non-`qe-` key that inherits its target from the root `[workspace.dependencies]`.
fn finalize_table(
    deps: &mut BTreeSet<String>,
    table: Option<PendingTable>,
    aliases: &BTreeMap<String, String>,
) {
    if let Some(t) = table {
        if let Some(target) =
            resolve_internal_target(&t.key, t.package.as_deref(), t.path.as_deref(), aliases)
        {
            deps.insert(target);
        }
    }
}

/// Resolve a dependency to its internal firewall edge target, or `None` when it is external (QE-489).
///
/// Precedence: a `package = "qe-…"` rename resolves the edge by its real package (whatever the key); a
/// `path = "../<crate>"` into the workspace is an internal edge mapped to that crate's package name via
/// the `crates/<dir>` ⇒ `qe-<dir>` convention (again, whatever the key); a bare key is internal iff it is
/// itself `qe-`-named; finally a non-`qe-` bare key (a `key.workspace = true` with no local `package`/
/// `path`) is resolved through the root `[workspace.dependencies]` `aliases` map, so an internal crate
/// aliased under a non-`qe-` root key is still caught (QE-489). A `package`-renamed or `path`-referenced
/// *external* crate stays external.
fn resolve_internal_target(
    key: &str,
    package: Option<&str>,
    path: Option<&str>,
    aliases: &BTreeMap<String, String>,
) -> Option<String> {
    if let Some(pkg) = package {
        return pkg.starts_with("qe-").then(|| pkg.to_string());
    }
    if let Some(p) = path {
        if let Some(target) = workspace_target_from_path(p) {
            return Some(target);
        }
    }
    if key.starts_with("qe-") {
        return Some(key.to_string());
    }
    aliases.get(key).cloned()
}

/// Map a workspace `path = "../<crate>"` dependency to the target crate's package name, or `None` when the
/// path is not a crate directory (e.g. a `src/main.rs` bin path). The final path segment is the crate
/// **directory**; the workspace convention `crates/<dir>` ⇒ package `qe-<dir>` gives its package name (a
/// segment already `qe-`-prefixed is taken verbatim). Any relative crate path is treated as internal, so
/// a non-`qe-` key over a `path` dep cannot slip past the firewall (fail-closed).
fn workspace_target_from_path(path: &str) -> Option<String> {
    let seg = path
        .trim_end_matches('/')
        .rsplit('/')
        .find(|s| !s.is_empty() && *s != "..")?;
    if seg.contains('.') {
        return None; // a file path (has an extension), not a crate directory
    }
    if seg.starts_with("qe-") {
        Some(seg.to_string())
    } else {
        Some(format!("qe-{seg}"))
    }
}

/// Find the double-quoted value assigned to `key` (`key = "value"`) anywhere in `text`, matching `key`
/// only where it is immediately followed (modulo whitespace) by `=` — so `path` inside a *value*
/// (`"../path"`) or as a substring of another key never matches. Deterministic, quote-delimited.
fn assigned_value(text: &str, key: &str) -> Option<String> {
    let mut rest = text;
    while let Some(pos) = rest.find(key) {
        let after = rest[pos + key.len()..].trim_start();
        if let Some(value_part) = after.strip_prefix('=') {
            let value_part = value_part.trim_start();
            if let Some(inner) = value_part.strip_prefix('"') {
                let end = inner.find('"')?;
                return Some(inner[..end].to_string());
            }
        }
        rest = &rest[pos + key.len()..];
    }
    None
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
    let root = workspace_root();
    // QE-489: the root `[workspace.dependencies]` alias map, so a member's `key.workspace = true` under a
    // non-`qe-` key resolves to its real internal target (an unreadable root manifest fails closed to the
    // bare-key behaviour — the same as before this backstop existed).
    let aliases = std::fs::read_to_string(root.join("Cargo.toml"))
        .map(|text| workspace_alias_targets(&text))
        .unwrap_or_default();
    let crates_dir = root.join("crates");
    let mut graph = Graph::new();
    let entries = std::fs::read_dir(&crates_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", crates_dir.display()));
    for entry in entries.flatten() {
        let manifest = entry.path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue; // not a crate directory
        };
        let (name, deps) = parse_manifest_with_aliases(&text, &aliases);
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
///
/// QE-254 adds the **second composition root** `qe-server` (admin-UI backend, ADR D4a): it reuses the
/// training-side + shared crates but must stay clear of the live side — no `qe-runtime`/`qe-venue`
/// edge — so async stays isolated to that crate and the server never links the live trading path.
///
/// QE-426 split the `qe-runtime` god-crate along the process seams into `qe-runtime-core` (the shared
/// planner⑤ ↔ edge⑥ contract), `qe-hedger` (Bootstrap③+Live④+Hedge⑤), and `qe-edge` (the order-submitting
/// Edge gateway⑥). The "live" side is therefore now five crates, so the existing search/portfolio/server
/// rules gain the three split crates on their forbidden lists. Three new rules assert the split's own
/// boundaries: the order-submitting `qe-edge` stays tight (no search/portfolio/vintage/planner deps — it is
/// the security boundary), the planner `qe-hedger` never links the adapter (the gRPC seam is a compile
/// boundary), and the shared `qe-runtime-core` contract stays a pure leaf.
#[must_use]
pub fn firewall_rules() -> Vec<FirewallRule> {
    vec![
        FirewallRule {
            upstream: "qe-wfo",
            forbidden: &[
                "qe-ensemble",
                "qe-runtime",
                "qe-venue",
                "qe-runtime-core",
                "qe-hedger",
                "qe-edge",
            ],
        },
        FirewallRule {
            upstream: "qe-ensemble",
            forbidden: &[
                "qe-wfo",
                "qe-runtime",
                "qe-venue",
                "qe-runtime-core",
                "qe-hedger",
                "qe-edge",
            ],
        },
        FirewallRule {
            upstream: "qe-server",
            forbidden: &[
                "qe-runtime",
                "qe-venue",
                "qe-runtime-core",
                "qe-hedger",
                "qe-edge",
            ],
        },
        // QE-426: the order-submitting crate is the deployment/security boundary — its dependency surface
        // stays minimal. It must link neither search/portfolio (`qe-wfo`/`qe-ensemble`), nor the genome /
        // vintage eval (`qe-vintage`/`qe-signal`), nor the planner (`qe-hedger`). It reaches only the shared
        // contract + domain/venue/risk/error.
        FirewallRule {
            upstream: "qe-edge",
            forbidden: &[
                "qe-wfo",
                "qe-ensemble",
                "qe-vintage",
                "qe-signal",
                "qe-hedger",
            ],
        },
        // QE-426: the planner must not link the order-submission adapter — the QE-218 gRPC seam is a compile
        // boundary (two colocated processes). It also stays clear of the training side.
        FirewallRule {
            upstream: "qe-hedger",
            forbidden: &["qe-edge", "qe-wfo", "qe-ensemble"],
        },
        // QE-426: the shared planner⑤ ↔ edge⑥ contract is a pure leaf — it must reach neither side, nor the
        // venue/risk/signal beyond its `qe-domain` money primitives.
        FirewallRule {
            upstream: "qe-runtime-core",
            forbidden: &["qe-edge", "qe-hedger", "qe-venue", "qe-risk", "qe-signal"],
        },
        // QE-452 Phase A: the frozen formula-pool artefact crate must stay clear of the **live** side —
        // runtime never loads a pool (§13.2/§13.3), so the pool artefact must not reach `qe-runtime`/
        // `qe-venue` (nor the split live crates). It is a pure serde leaf today (no `qe-*` dep), so the
        // rule holds; this guards a future dep that would breach the pool ⟂ live separation.
        FirewallRule {
            upstream: "qe-formula-pool",
            forbidden: &[
                "qe-runtime",
                "qe-venue",
                "qe-runtime-core",
                "qe-hedger",
                "qe-edge",
            ],
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

    /// QE-489: an internal crate pulled under a **non-`qe-` key** is resolved by its `package =` target,
    /// not the key — so a `package` rename cannot make the edge invisible. Both inline-table and
    /// dependency-table forms resolve to the real target crate.
    #[test]
    fn package_rename_under_non_qe_key_resolves_to_target() {
        // Inline-table form: `foo = { package = "qe-runtime" }`.
        let inline = "\
[package]
name = \"qe-wfo\"

[dependencies]
foo = { package = \"qe-runtime\", version = \"0.1\" }
serde = { workspace = true }
";
        let (name, deps) = parse_manifest(inline);
        assert_eq!(name.as_deref(), Some("qe-wfo"));
        assert!(
            deps.contains("qe-runtime"),
            "renamed internal dep must resolve to its package target: {deps:?}"
        );
        assert!(
            !deps.contains("foo"),
            "the key must not be the edge: {deps:?}"
        );
        assert!(!deps.contains("serde"), "external dep stays out: {deps:?}");

        // Dependency-table form: `[dependencies.foo]` with a `package` body line.
        let table = "\
[package]
name = \"qe-wfo\"

[dependencies.foo]
package = \"qe-runtime\"
version = \"0.1\"
";
        let (_, deps) = parse_manifest(table);
        assert!(
            deps.contains("qe-runtime") && !deps.contains("foo"),
            "table-form package rename must resolve to target: {deps:?}"
        );
    }

    /// QE-489: a bare `path = "../<crate>"` dep is an internal edge regardless of the key's prefix,
    /// mapped to the target crate's package name. Both inline and table forms are covered.
    #[test]
    fn path_dep_under_non_qe_key_resolves_to_internal_target() {
        let inline = "\
[package]
name = \"qe-wfo\"

[dependencies]
search = { path = \"../search\" }
";
        let (_, deps) = parse_manifest(inline);
        assert!(
            deps.contains("qe-search"),
            "a `path` dep under a non-qe key must produce an internal edge: {deps:?}"
        );
        assert!(
            !deps.contains("search"),
            "the key must not be the edge: {deps:?}"
        );

        let table = "\
[package]
name = \"qe-wfo\"

[dependencies.search]
path = \"../search\"
";
        let (_, deps) = parse_manifest(table);
        assert!(
            deps.contains("qe-search"),
            "table-form path dep must resolve internal: {deps:?}"
        );
    }

    /// QE-489 regression: the idiomatic, key-named forms still resolve exactly as before — including the
    /// live `qe-domain = { path = "../domain" }` shape (the sole real `path` dep), whose key *and* target
    /// agree, so it is caught either way.
    #[test]
    fn idiomatic_and_live_path_forms_still_caught() {
        let toml = "\
[package]
name = \"qe-config\"

[dependencies]
qe-domain = { path = \"../domain\" }
qe-signal.workspace = true
serde.workspace = true
";
        let (name, deps) = parse_manifest(toml);
        assert_eq!(name.as_deref(), Some("qe-config"));
        assert_eq!(
            deps,
            ["qe-domain", "qe-signal"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
    }

    /// QE-489 end-to-end: a forbidden edge introduced by RENAMING the key (so the manifest never mentions
    /// `qe-runtime` as a key) is now caught by the firewall — the bypass is closed fail-closed.
    #[test]
    fn firewall_catches_a_forbidden_edge_hidden_behind_a_key_rename() {
        // `qe-wfo` must not reach `qe-runtime`; here it does, disguised under a non-qe key.
        let wfo_manifest = "\
[package]
name = \"qe-wfo\"

[dependencies]
qe-signal.workspace = true
sneaky = { package = \"qe-runtime\" }
";
        let (name, deps) = parse_manifest(wfo_manifest);
        let mut graph = Graph::new();
        graph.insert(name.unwrap(), deps);
        graph.insert("qe-runtime".to_string(), BTreeSet::new());

        let violations = check_firewall(&graph, &firewall_rules());
        assert!(
            violations.contains(&Violation {
                upstream: "qe-wfo".into(),
                forbidden: "qe-runtime".into(),
            }),
            "renamed-key edge must breach the firewall: {violations:?}"
        );
    }

    /// QE-489: dev-dependencies remain excluded even when they use a `package`/`path` target form.
    #[test]
    fn dev_dependencies_with_target_forms_stay_excluded() {
        let toml = "\
[package]
name = \"qe-wfo\"

[dev-dependencies]
foo = { package = \"qe-runtime\" }
bar = { path = \"../ensemble\" }

[dev-dependencies.baz]
package = \"qe-venue\"
";
        let (_, deps) = parse_manifest(toml);
        assert!(
            deps.is_empty(),
            "no dev-dependency (in any target form) enters the production firewall: {deps:?}"
        );
    }

    /// QE-489 backstop: an internal crate aliased in the **root** `[workspace.dependencies]` under a
    /// non-`qe-` key, then pulled by a member as `key.workspace = true`, was invisible to the firewall —
    /// the member line carries no `package`/`path`, so only the root alias reveals its real target. The
    /// firewall now parses the root manifest and resolves the alias, closing the bypass fail-closed.
    #[test]
    fn root_workspace_alias_under_non_qe_key_is_resolved_and_caught() {
        // The root manifest: internal crates aliased under non-`qe-` keys (both inline `package`/`path`
        // and the dependency-table form), plus external aliases that must stay external.
        let root = "\
[workspace]
members = [\"crates/*\"]

[workspace.dependencies]
serde = { version = \"1\", features = [\"derive\"] }
qe-signal = { path = \"crates/signal\" }
sneaky = { package = \"qe-runtime\" }
by_path = { path = \"crates/venue\" }

[workspace.dependencies.tabled]
package = \"qe-ensemble\"
version = \"0.1\"
";
        let aliases = workspace_alias_targets(root);
        assert_eq!(
            aliases.get("sneaky").map(String::as_str),
            Some("qe-runtime")
        );
        assert_eq!(aliases.get("by_path").map(String::as_str), Some("qe-venue"));
        assert_eq!(
            aliases.get("tabled").map(String::as_str),
            Some("qe-ensemble")
        );
        assert!(
            !aliases.contains_key("serde"),
            "external alias must be omitted: {aliases:?}"
        );

        // A member that pulls the internal crate under the non-`qe-` key via `key.workspace = true` — no
        // `package`/`path` on the member line, so only the root alias can reveal the edge.
        let member = "\
[package]
name = \"qe-wfo\"

[dependencies]
qe-signal.workspace = true
sneaky.workspace = true
";
        let (name, deps) = parse_manifest_with_aliases(member, &aliases);
        assert_eq!(name.as_deref(), Some("qe-wfo"));
        assert!(
            deps.contains("qe-runtime"),
            "root-aliased internal dep must resolve to its real target: {deps:?}"
        );
        assert!(
            !deps.contains("sneaky"),
            "the key is not the edge: {deps:?}"
        );

        // Sanity — WITHOUT the root aliases the edge is invisible (the bug this closes): proves the fix is
        // load-bearing, not vacuous.
        let (_, blind) = parse_manifest(member);
        assert!(
            !blind.contains("qe-runtime"),
            "bare parse cannot see the root alias (the bypass): {blind:?}"
        );

        // End-to-end: the firewall now breaches on the disguised `qe-wfo → qe-runtime` edge.
        let mut graph = Graph::new();
        graph.insert(name.unwrap(), deps);
        graph.insert("qe-runtime".to_string(), BTreeSet::new());
        let violations = check_firewall(&graph, &firewall_rules());
        assert!(
            violations.contains(&Violation {
                upstream: "qe-wfo".into(),
                forbidden: "qe-runtime".into(),
            }),
            "root-aliased renamed edge must breach the firewall: {violations:?}"
        );
    }

    /// QE-489 determinism: `parse_manifest` output is order-stable (a `BTreeSet`), independent of the
    /// order deps appear in the manifest text.
    #[test]
    fn parse_manifest_output_is_order_stable() {
        let a = "\
[package]
name = \"qe-wfo\"

[dependencies]
qe-signal.workspace = true
qe-domain.workspace = true
foo = { package = \"qe-runtime\" }
";
        let b = "\
[package]
name = \"qe-wfo\"

[dependencies]
foo = { package = \"qe-runtime\" }
qe-domain.workspace = true
qe-signal.workspace = true
";
        let (_, da) = parse_manifest(a);
        let (_, db) = parse_manifest(b);
        assert_eq!(da, db);
        assert_eq!(
            da.into_iter().collect::<Vec<_>>(),
            vec!["qe-domain", "qe-runtime", "qe-signal"]
        );
    }
}
