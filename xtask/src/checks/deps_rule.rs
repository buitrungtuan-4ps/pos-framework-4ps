// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The dependency rule: `pos-core`, `pos-ports` and `pos-proto` may depend only on
//! allow-listed pure-computation crates, and `pos-core` may not reach `pos-ports`.
//!
//! This is one of the two checks that turn the architecture into law. Discovering
//! six months from now that the domain imports `tokio` means rewriting the domain,
//! so the check exists before the code it guards.
//!
//! Two layers, because either alone has a hole:
//!
//! * **Declared** — what each backbone manifest asks for. Catches the obvious case
//!   and names the file to edit.
//! * **Resolved** — the normal-dependency closure Cargo actually built. Feature
//!   unification can add an edge a manifest never declared, and only this layer
//!   sees it.
//!
//! Development dependencies are excluded on purpose: `proptest` and `insta` are
//! not shipped, and are governed by `dev-allow` in the allow-list.
//!
//! # Two data sources, because neither alone is right
//!
//! The declared layer reads `cargo metadata`, which is the only place that reports
//! *which features a manifest asks for* — needed to catch `jiff` with its default
//! features on.
//!
//! The closure layer needs **both** sources, because each is missing something the
//! other has.
//!
//! `cargo metadata`'s resolve graph has the structure — who depends on whom, and
//! which packages are procedural macros — but it lists every **optional** dependency
//! edge whether or not it is activated. Reading it alone reported `log`, `defmt` and
//! `bitflags` behind `jiff`, none of which this workspace links.
//!
//! `cargo tree -e normal --target all` reports exactly what is activated, but as a
//! flat list, so it cannot distinguish a runtime dependency from `syn` — which is
//! reached only through a derive macro and runs inside the compiler.
//!
//! So the walk uses the metadata graph, follows an edge only when the target is in
//! the `cargo tree` set, and stops at procedural macros. What survives is the set of
//! crates actually linked into a binary, which is what the rule is about.
//! `--target all` is deliberate: the store binary ships for Windows as well as
//! Linux, so a dependency present on only one of them still ships.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::{Error, repo_root};
use crate::Finding;

/// Crates the rule applies to.
const BACKBONE: [&str; 3] = ["pos-core", "pos-ports", "pos-proto"];

/// Edges that must not exist regardless of the allow-list, with the reason.
const FORBIDDEN_EDGES: [(&str, &str, &str); 1] = [(
    "pos-core",
    "pos-ports",
    "ADR-0013: they are siblings, so the domain performing no I/O is a property of \
     the dependency graph rather than a lint",
)];

// ---------------------------------------------------------------------------
// The `cargo metadata` subset we read.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct Metadata {
    pub packages: Vec<Package>,
    #[serde(default)]
    pub resolve: Resolve,
}

/// The dependency graph as Cargo resolved it.
#[derive(Deserialize, Default)]
pub struct Resolve {
    #[serde(default)]
    pub nodes: Vec<Node>,
}

/// One package's outgoing edges.
#[derive(Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(default)]
    pub deps: Vec<NodeDep>,
}

/// One edge.
#[derive(Deserialize)]
pub struct NodeDep {
    pub pkg: String,
    #[serde(default)]
    pub dep_kinds: Vec<DepKind>,
}

/// Whether an edge is normal, `dev`, or `build`.
#[derive(Deserialize)]
pub struct DepKind {
    /// `null` means a normal dependency — the only kind that ships.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Deserialize)]
pub struct Package {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub manifest_path: String,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub targets: Vec<Target>,
}

#[derive(Deserialize)]
pub struct Target {
    #[serde(default)]
    pub kind: Vec<String>,
}

impl Package {
    /// Whether this package is a procedural macro.
    ///
    /// A proc macro runs inside the compiler and cannot be called from domain
    /// code at runtime, so it is not an infrastructure dependency in the sense
    /// the rule cares about — `serde`'s `derive` feature legitimately pulls
    /// `serde_derive`, and through it `syn`, `quote` and `proc-macro2`. The
    /// closure walk therefore neither flags a proc macro nor traverses through
    /// one. What a proc macro *generates* still has to satisfy the rule, because
    /// generated code can only call crates the manifest actually declares.
    pub(super) fn is_proc_macro(&self) -> bool {
        self.targets
            .iter()
            .any(|t| t.kind.iter().any(|k| k == "proc-macro"))
    }
}

#[derive(Deserialize)]
pub struct Dependency {
    pub name: String,
    /// `null` for a normal dependency, `"dev"` or `"build"` otherwise.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub uses_default_features: Option<bool>,
    #[serde(default)]
    pub features: Vec<String>,
}

// ---------------------------------------------------------------------------
// The allow-list.
// ---------------------------------------------------------------------------

/// `tools/backbone-allowlist.toml`.
#[derive(Deserialize, Default)]
pub struct Allowlist {
    #[serde(default)]
    pub allow: BTreeMap<String, toml::Value>,
    /// Features that make an otherwise-allowed crate unacceptable.
    #[serde(default, rename = "forbidden-features")]
    pub forbidden_features: BTreeMap<String, Vec<String>>,
}

impl Allowlist {
    fn permits(&self, crate_name: &str) -> bool {
        // A backbone crate depending on another backbone crate is always fine,
        // except where FORBIDDEN_EDGES says otherwise.
        BACKBONE.contains(&crate_name) || self.allow.contains_key(crate_name)
    }
}

// ---------------------------------------------------------------------------
// The check, as a pure function so it can be tested against fixtures.
// ---------------------------------------------------------------------------

/// Crate names `cargo tree` reports as activated, keyed by backbone crate name.
pub type Closures = BTreeMap<String, BTreeSet<String>>;

/// Evaluates the rule. Pure: no filesystem, no subprocess, no clock.
#[must_use]
pub fn check(meta: &Metadata, allow: &Allowlist, closures: &Closures) -> Vec<Finding> {
    let mut findings = Vec::new();

    for name in BACKBONE {
        let Some(pkg) = meta.packages.iter().find(|p| p.name == name) else {
            continue; // Crate not created yet; earlier phases are allowed to lack it.
        };
        let manifest = if pkg.manifest_path.is_empty() {
            format!("crates/{name}/Cargo.toml")
        } else {
            relative(&pkg.manifest_path)
        };

        // -- Layer 1: declared normal dependencies. -------------------------
        for dep in pkg.dependencies.iter().filter(|d| d.kind.is_none()) {
            // Same reasoning as the closure walk: a proc macro expands in the
            // compiler and is never linked, so it is not an infrastructure
            // dependency whether it is declared directly or reached transitively.
            let declared_is_proc_macro = meta
                .packages
                .iter()
                .find(|p| p.name == dep.name)
                .is_some_and(Package::is_proc_macro);
            if !declared_is_proc_macro && !allow.permits(&dep.name) {
                findings.push(
                    Finding::new(
                        &manifest,
                        "dependency-rule",
                        format!(
                            "`{name}` declares `{}`, which is not allow-listed",
                            dep.name
                        ),
                    )
                    .with_hint(
                        "either drop the dependency, or add it to \
                         tools/backbone-allowlist.toml with an ADR — that file needs an \
                         architecture reviewer",
                    ),
                );
            }

            // A crate can be allow-listed and still be wrong: jiff's default
            // features read $TZ and /usr/share/zoneinfo (ADR-0014).
            if let Some(forbidden) = allow.forbidden_features.get(&dep.name) {
                if dep.uses_default_features == Some(true)
                    && forbidden.iter().any(|f| f == "default")
                {
                    findings.push(
                        Finding::new(
                            &manifest,
                            "dependency-rule",
                            format!(
                                "`{name}` takes `{}` with default features, which are \
                                 forbidden for this crate",
                                dep.name
                            ),
                        )
                        .with_hint("set `default-features = false` and list features explicitly"),
                    );
                }
                for enabled in &dep.features {
                    if forbidden.contains(enabled) {
                        findings.push(Finding::new(
                            &manifest,
                            "dependency-rule",
                            format!(
                                "`{name}` enables `{}/{enabled}`, which is forbidden \
                                 for this crate",
                                dep.name
                            ),
                        ));
                    }
                }
            }
        }

        // -- Layer 2: what is actually linked. ------------------------------
        let empty = BTreeSet::new();
        let activated = closures.get(name).unwrap_or(&empty);
        for reached in linked_closure(meta, &pkg.id, activated) {
            if reached == name {
                continue;
            }
            if !allow.permits(&reached) {
                findings.push(
                    Finding::new(
                        &manifest,
                        "dependency-rule",
                        format!("`{name}` links `{reached}`, which is not allow-listed"),
                    )
                    .with_hint(format!(
                        "find the path with `cargo tree -p {name} -e normal --target all \
                         --invert {reached}`"
                    )),
                );
            }
        }
    }

    // -- Edges that are banned outright. ------------------------------------
    for (from, to, why) in FORBIDDEN_EDGES {
        let Some(pkg) = meta.packages.iter().find(|p| p.name == from) else {
            continue;
        };
        let manifest = if pkg.manifest_path.is_empty() {
            format!("crates/{from}/Cargo.toml")
        } else {
            relative(&pkg.manifest_path)
        };
        let empty = BTreeSet::new();
        let activated = closures.get(from).unwrap_or(&empty);
        let reaches = linked_closure(meta, &pkg.id, activated).contains(to);
        if reaches {
            findings.push(Finding::new(
                &manifest,
                "dependency-rule",
                format!("`{from}` must not depend on `{to}` — {why}"),
            ));
        }
    }

    findings.sort_by(|a, b| (&a.file, &a.message).cmp(&(&b.file, &b.message)));
    findings.dedup();
    findings
}

/// The activated normal-dependency closure of one crate, via `cargo tree`.
///
/// `cargo tree` reports what is actually built, which `cargo metadata`'s resolve
/// graph does not — see the module documentation.
fn activated_closure(root: &std::path::Path, crate_name: &str) -> Result<BTreeSet<String>, Error> {
    let output = std::process::Command::new(cargo_binary())
        .args([
            "tree",
            "--package",
            crate_name,
            "--edges",
            "normal",
            "--target",
            "all",
            "--prefix",
            "none",
            "--format",
            "{p}",
            "--locked",
        ])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "cargo tree failed for {crate_name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !name.is_empty() && *name != "(*)")
        .map(str::to_owned)
        .collect())
}

/// Crate names linked into a binary built from `root`.
///
/// Walks the resolve graph, following a normal edge only when `activated` contains
/// the target, and never traversing into a procedural macro. See the module
/// documentation for why both filters are needed.
fn linked_closure(
    meta: &Metadata,
    root_id: &str,
    activated: &BTreeSet<String>,
) -> BTreeSet<String> {
    let by_id: BTreeMap<&str, &Package> =
        meta.packages.iter().map(|p| (p.id.as_str(), p)).collect();
    let nodes: BTreeMap<&str, &Node> = meta
        .resolve
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();

    let mut linked = BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([root_id.to_owned()]);
    let mut visited = BTreeSet::from([root_id.to_owned()]);

    while let Some(id) = queue.pop_front() {
        let Some(node) = nodes.get(id.as_str()) else {
            continue;
        };
        for edge in &node.deps {
            // An empty dep_kinds list means an older metadata format that did not
            // distinguish kinds; treat it as normal so the check errs strict.
            let is_normal =
                edge.dep_kinds.is_empty() || edge.dep_kinds.iter().any(|kind| kind.kind.is_none());
            if !is_normal {
                continue;
            }
            let Some(package) = by_id.get(edge.pkg.as_str()) else {
                continue;
            };
            // Not activated for any target we ship: the edge exists in the manifest
            // but nothing turns it on.
            if !activated.contains(&package.name) {
                continue;
            }
            linked.insert(package.name.clone());
            // A procedural macro runs in the compiler, so nothing beneath it is
            // linked. Record it and stop.
            if package.is_proc_macro() {
                continue;
            }
            if visited.insert(edge.pkg.clone()) {
                queue.push_back(edge.pkg.clone());
            }
        }
    }
    // Proc macros themselves are recorded above so the walk terminates cleanly, but
    // they are build-time and must not be reported.
    linked
        .into_iter()
        .filter(|name| {
            !meta
                .packages
                .iter()
                .any(|p| p.name == *name && p.is_proc_macro())
        })
        .collect()
}

fn cargo_binary() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

fn relative(path: &str) -> String {
    let root = repo_root();
    std::path::Path::new(path)
        .strip_prefix(&root)
        .map_or_else(|_| path.to_owned(), |p| p.display().to_string())
}

// ---------------------------------------------------------------------------
// Wiring.
// ---------------------------------------------------------------------------

/// Runs `cargo metadata` and evaluates the rule against it.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let output =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["metadata", "--format-version", "1", "--locked"])
            .current_dir(&root)
            .output()?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }

    let meta: Metadata = serde_json::from_slice(&output.stdout)?;
    let allow_text = std::fs::read_to_string(root.join("tools/backbone-allowlist.toml"))?;
    let allow: Allowlist = toml::from_str(&allow_text)?;

    let mut closures = Closures::new();
    for name in BACKBONE {
        if meta.packages.iter().any(|p| p.name == name) {
            closures.insert(name.to_owned(), activated_closure(&root, name)?);
        }
    }
    Ok(check(&meta, &allow, &closures))
}

// ---------------------------------------------------------------------------
// Tests.
//
// A check nobody has watched fail is not a check. These fixtures are hand-written
// rather than generated so the shape being asserted stays readable, and they run
// offline with no subprocess.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{Allowlist, Closures, Metadata, check};

    fn allowlist() -> Allowlist {
        toml::from_str(
            r#"
            [allow]
            serde = { reason = "wire serialization" }
            jiff  = { reason = "zoned datetime arithmetic" }

            [forbidden-features]
            jiff = ["tz-system", "tzdb-zoneinfo", "default"]
            "#,
        )
        .expect("fixture allow-list parses")
    }

    /// A hand-built `cargo metadata` document rooted at `pos-core`.
    struct Fixture {
        /// `pos-core`'s own manifest entries: name, uses-default-features, features.
        declared: Vec<(String, bool, Vec<String>)>,
        /// Every non-root package: name and whether it is a procedural macro.
        packages: Vec<(String, bool)>,
        /// Resolve-graph edges, by package name.
        edges: Vec<(String, String)>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                declared: Vec::new(),
                packages: Vec::new(),
                edges: Vec::new(),
            }
        }

        /// Adds a normal dependency to `pos-core`'s manifest, and the matching edge.
        fn declares(mut self, name: &str, default_features: bool, features: &[&str]) -> Self {
            self.declared.push((
                name.to_owned(),
                default_features,
                features.iter().map(|f| (*f).to_owned()).collect(),
            ));
            self.edges.push(("pos-core".to_owned(), name.to_owned()));
            self = self.package(name, false);
            self
        }

        fn package(mut self, name: &str, is_proc_macro: bool) -> Self {
            if !self.packages.iter().any(|(existing, _)| existing == name) {
                self.packages.push((name.to_owned(), is_proc_macro));
            }
            self
        }

        fn proc_macro(self, name: &str) -> Self {
            self.package(name, true)
        }

        fn edge(mut self, from: &str, to: &str) -> Self {
            self.edges.push((from.to_owned(), to.to_owned()));
            self = self.package(to, false);
            self
        }

        fn metadata(&self) -> Metadata {
            let deps: Vec<String> = self
                .declared
                .iter()
                .map(|(name, defaults, features)| {
                    let features = features
                        .iter()
                        .map(|f| format!("\"{f}\""))
                        .collect::<Vec<_>>()
                        .join(",");
                    format!(
                        r#"{{"name":"{name}","kind":null,"uses_default_features":{defaults},"features":[{features}]}}"#
                    )
                })
                .collect();

            let mut packages = vec![format!(
                r#"{{"id":"pos-core-id","name":"pos-core","manifest_path":"",
                     "dependencies":[{}],"targets":[{{"kind":["lib"]}}]}}"#,
                deps.join(",")
            )];
            for (name, is_proc_macro) in &self.packages {
                let kind = if *is_proc_macro { "proc-macro" } else { "lib" };
                packages.push(format!(
                    r#"{{"id":"{name}-id","name":"{name}","manifest_path":"",
                         "dependencies":[],"targets":[{{"kind":["{kind}"]}}]}}"#
                ));
            }

            let mut by_source: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for (from, to) in &self.edges {
                by_source.entry(from).or_default().push(to);
            }
            let mut nodes = Vec::new();
            for (name, _) in
                core::iter::once(&("pos-core".to_owned(), false)).chain(self.packages.iter())
            {
                let empty = Vec::new();
                let targets = by_source.get(name.as_str()).unwrap_or(&empty);
                let deps: Vec<String> = targets
                    .iter()
                    .map(|to| format!(r#"{{"pkg":"{to}-id","dep_kinds":[{{"kind":null}}]}}"#))
                    .collect();
                nodes.push(format!(
                    r#"{{"id":"{name}-id","deps":[{}]}}"#,
                    deps.join(",")
                ));
            }

            let json = format!(
                r#"{{"packages":[{}],"resolve":{{"nodes":[{}]}}}}"#,
                packages.join(","),
                nodes.join(",")
            );
            serde_json::from_str(&json).expect("fixture metadata parses")
        }

        /// Everything `cargo tree` would report as activated: by default, all of it.
        fn all_activated(&self) -> Closures {
            let mut activated: BTreeSet<String> =
                self.packages.iter().map(|(name, _)| name.clone()).collect();
            activated.insert("pos-core".to_owned());
            BTreeMap::from([("pos-core".to_owned(), activated)])
        }

        /// As `all_activated`, minus the named packages — the shape of an optional
        /// dependency that nothing turns on.
        fn activated_without(&self, excluded: &[&str]) -> Closures {
            let mut closures = self.all_activated();
            if let Some(set) = closures.get_mut("pos-core") {
                for name in excluded {
                    set.remove(*name);
                }
            }
            closures
        }
    }

    #[test]
    fn accepts_an_allow_listed_dependency() {
        let fixture = Fixture::new().declares("serde", false, &["derive"]);
        let findings = check(&fixture.metadata(), &allowlist(), &fixture.all_activated());
        assert!(findings.is_empty(), "clean fixture rejected: {findings:?}");
    }

    #[test]
    fn rejects_tokio_declared_in_the_manifest() {
        let fixture = Fixture::new().declares("tokio", false, &[]);
        let findings = check(&fixture.metadata(), &allowlist(), &fixture.all_activated());
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("declares `tokio`")),
            "the declared layer did not fire: {findings:?}"
        );
        assert_eq!(findings[0].file, "crates/pos-core/Cargo.toml");
    }

    #[test]
    fn rejects_an_infrastructure_crate_reached_only_transitively() {
        // The case the declared layer cannot see: nothing in pos-core's manifest
        // mentions tokio, but something it depends on pulls it in.
        let fixture = Fixture::new()
            .declares("serde", false, &[])
            .edge("serde", "tokio");
        let findings = check(&fixture.metadata(), &allowlist(), &fixture.all_activated());
        assert!(
            findings.iter().any(|f| f.message.contains("links `tokio`")),
            "the closure layer did not fire: {findings:?}"
        );
    }

    #[test]
    fn ignores_an_optional_edge_that_nothing_activates() {
        // `cargo metadata` lists every optional dependency edge whether or not it is
        // turned on. jiff carries edges to `log` and `defmt` this way, and neither is
        // ever linked, so flagging them would be a false positive.
        let fixture = Fixture::new()
            .declares("jiff", false, &[])
            .edge("jiff", "log");
        let findings = check(
            &fixture.metadata(),
            &allowlist(),
            &fixture.activated_without(&["log"]),
        );
        assert!(
            findings.is_empty(),
            "an inactive optional edge was reported: {findings:?}"
        );
    }

    #[test]
    fn reports_an_optional_edge_once_something_activates_it() {
        let fixture = Fixture::new()
            .declares("jiff", false, &[])
            .edge("jiff", "log");
        let findings = check(&fixture.metadata(), &allowlist(), &fixture.all_activated());
        assert!(
            findings.iter().any(|f| f.message.contains("links `log`")),
            "an activated optional edge was missed: {findings:?}"
        );
    }

    #[test]
    fn rejects_the_pos_core_to_pos_ports_edge() {
        // ADR-0013: the domain performing no I/O is a property of the graph, so this
        // edge must fail even though pos-ports is itself a backbone crate.
        let fixture = Fixture::new().declares("pos-ports", false, &[]);
        let findings = check(&fixture.metadata(), &allowlist(), &fixture.all_activated());
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("must not depend on `pos-ports`")),
            "the sibling rule did not fire: {findings:?}"
        );
    }

    #[test]
    fn rejects_default_features_on_an_allow_listed_crate() {
        // jiff is allow-listed, but its defaults read $TZ and /usr/share/zoneinfo,
        // which is filesystem access inside the domain (ADR-0014).
        let fixture = Fixture::new().declares("jiff", true, &[]);
        let findings = check(&fixture.metadata(), &allowlist(), &fixture.all_activated());
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("default features")),
            "the feature rule did not fire on jiff: {findings:?}"
        );
    }

    #[test]
    fn rejects_a_named_forbidden_feature() {
        let fixture = Fixture::new().declares("jiff", false, &["tzdb-zoneinfo"]);
        let findings = check(&fixture.metadata(), &allowlist(), &fixture.all_activated());
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("jiff/tzdb-zoneinfo")),
            "the named-feature rule did not fire: {findings:?}"
        );
    }

    #[test]
    fn does_not_traverse_into_a_procedural_macro() {
        // serde's `derive` feature pulls serde_derive, and through it syn. Both run
        // inside the compiler and neither is linked, so requiring the allow-list to
        // name the whole macro toolchain would make it meaningless — and it would
        // grow every time a macro gained a dependency.
        let fixture = Fixture::new()
            .declares("serde", false, &["derive"])
            .proc_macro("serde_derive")
            .edge("serde", "serde_derive")
            .edge("serde_derive", "syn")
            .edge("syn", "tokio");
        let findings = check(&fixture.metadata(), &allowlist(), &fixture.all_activated());
        assert!(
            findings.is_empty(),
            "the walk crossed a procedural-macro boundary: {findings:?}"
        );
    }
}
