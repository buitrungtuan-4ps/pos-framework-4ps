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

use std::collections::{BTreeMap, BTreeSet, VecDeque};

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
    pub resolve: Resolve,
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

#[derive(Deserialize)]
pub struct Resolve {
    pub nodes: Vec<Node>,
}

#[derive(Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(default)]
    pub deps: Vec<NodeDep>,
}

#[derive(Deserialize)]
pub struct NodeDep {
    pub pkg: String,
    #[serde(default)]
    pub dep_kinds: Vec<DepKind>,
}

#[derive(Deserialize)]
pub struct DepKind {
    /// `null` means a normal dependency — the only kind that ships.
    #[serde(default)]
    pub kind: Option<String>,
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

/// Evaluates the rule. Pure: no filesystem, no subprocess, no clock.
#[must_use]
pub fn check(meta: &Metadata, allow: &Allowlist) -> Vec<Finding> {
    let mut findings = Vec::new();
    let by_id: BTreeMap<&str, &Package> =
        meta.packages.iter().map(|p| (p.id.as_str(), p)).collect();

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

        // -- Layer 2: the resolved normal-dependency closure. ---------------
        for reached in closure(meta, &pkg.id, &by_id) {
            let Some(dep_pkg) = by_id.get(reached.as_str()) else {
                continue;
            };
            if dep_pkg.name == name || dep_pkg.is_proc_macro() {
                continue;
            }
            if !allow.permits(&dep_pkg.name) {
                findings.push(
                    Finding::new(
                        &manifest,
                        "dependency-rule",
                        format!(
                            "`{name}` reaches `{}` through its resolved dependency graph, \
                             and it is not allow-listed",
                            dep_pkg.name
                        ),
                    )
                    .with_hint(
                        "this edge may come from feature unification rather than from this \
                         manifest — check `cargo tree -p {name} -e normal --invert` on the \
                         offending crate",
                    ),
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
        let reaches = closure(meta, &pkg.id, &by_id)
            .iter()
            .filter_map(|id| by_id.get(id.as_str()))
            .any(|p| p.name == to);
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

/// Every package reachable from `root` through normal dependencies only.
///
/// Traversal stops at procedural macros: they execute in the compiler, so what
/// they pull in is never linked into a running binary.
fn closure(meta: &Metadata, root: &str, by_id: &BTreeMap<&str, &Package>) -> BTreeSet<String> {
    let nodes: BTreeMap<&str, &Node> = meta
        .resolve
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([root.to_owned()]);

    while let Some(id) = queue.pop_front() {
        let Some(node) = nodes.get(id.as_str()) else {
            continue;
        };
        for dep in &node.deps {
            // An empty dep_kinds list means an older metadata format that did not
            // distinguish kinds; treat it as normal so the check errs strict.
            let is_normal =
                dep.dep_kinds.is_empty() || dep.dep_kinds.iter().any(|k| k.kind.is_none());
            if !is_normal || !seen.insert(dep.pkg.clone()) {
                continue;
            }
            let is_proc_macro = by_id
                .get(dep.pkg.as_str())
                .is_some_and(|p| p.is_proc_macro());
            if !is_proc_macro {
                queue.push_back(dep.pkg.clone());
            }
        }
    }
    seen
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
            .args([
                "metadata",
                "--format-version",
                "1",
                "--all-features",
                "--locked",
            ])
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
    Ok(check(&meta, &allow))
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
    use super::{Allowlist, Metadata, check};

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

    /// Builds metadata for one backbone crate with the given normal dependencies.
    fn metadata(core_deps: &[(&str, bool, &[&str])], extra_packages: &[(&str, bool)]) -> Metadata {
        let declared: Vec<String> = core_deps
            .iter()
            .map(|(name, defaults, feats)| {
                let feats = feats
                    .iter()
                    .map(|f| format!("\"{f}\""))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    r#"{{"name":"{name}","kind":null,"uses_default_features":{defaults},"features":[{feats}]}}"#
                )
            })
            .collect();
        let node_deps: Vec<String> = core_deps
            .iter()
            .map(|(name, _, _)| format!(r#"{{"pkg":"{name}-id","dep_kinds":[{{"kind":null}}]}}"#))
            .collect();
        let packages: Vec<String> = core_deps
            .iter()
            .map(|(name, _, _)| (*name, false))
            .chain(extra_packages.iter().copied())
            .map(|(name, is_pm)| {
                let kind = if is_pm { "proc-macro" } else { "lib" };
                format!(
                    r#"{{"id":"{name}-id","name":"{name}","manifest_path":"","dependencies":[],
                        "targets":[{{"kind":["{kind}"]}}]}}"#
                )
            })
            .collect();

        let json = format!(
            r#"{{
              "packages":[
                {{"id":"pos-core-id","name":"pos-core",
                  "manifest_path":"","dependencies":[{}],
                  "targets":[{{"kind":["lib"]}}]}}
                {}{}
              ],
              "resolve":{{"nodes":[
                {{"id":"pos-core-id","deps":[{}]}}
              ]}}
            }}"#,
            declared.join(","),
            if packages.is_empty() { "" } else { "," },
            packages.join(","),
            node_deps.join(","),
        );
        serde_json::from_str(&json).expect("fixture metadata parses")
    }

    #[test]
    fn accepts_an_allow_listed_dependency() {
        let meta = metadata(&[("serde", false, &["derive"])], &[]);
        let findings = check(&meta, &allowlist());
        assert!(findings.is_empty(), "clean fixture rejected: {findings:?}");
    }

    #[test]
    fn rejects_tokio_in_the_domain() {
        let meta = metadata(&[("tokio", false, &[])], &[]);
        let findings = check(&meta, &allowlist());
        assert!(
            findings.iter().any(|f| f.message.contains("tokio")),
            "the check did not fire on tokio in pos-core: {findings:?}"
        );
        assert_eq!(findings[0].file, "crates/pos-core/Cargo.toml");
    }

    #[test]
    fn rejects_the_pos_core_to_pos_ports_edge() {
        // ADR-0013: the domain performing no I/O is a property of the graph, so
        // this edge must fail even though pos-ports is itself a backbone crate.
        let meta = metadata(&[("pos-ports", false, &[])], &[]);
        let findings = check(&meta, &allowlist());
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("must not depend on `pos-ports`")),
            "the sibling rule did not fire: {findings:?}"
        );
    }

    #[test]
    fn rejects_a_forbidden_feature_on_an_allowed_crate() {
        // jiff is allow-listed, but its default features read $TZ and
        // /usr/share/zoneinfo, which is filesystem access inside the domain.
        let meta = metadata(&[("jiff", true, &[])], &[]);
        let findings = check(&meta, &allowlist());
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("default features")),
            "the feature rule did not fire on jiff: {findings:?}"
        );
    }

    #[test]
    fn rejects_a_named_forbidden_feature() {
        let meta = metadata(&[("jiff", false, &["tzdb-zoneinfo"])], &[]);
        let findings = check(&meta, &allowlist());
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("jiff/tzdb-zoneinfo")),
            "the named-feature rule did not fire: {findings:?}"
        );
    }

    #[test]
    fn ignores_proc_macro_dependencies() {
        // serde's `derive` feature pulls serde_derive, and through it syn and
        // quote. They expand in the compiler and are never linked, so they are
        // not what the rule is about. Written out explicitly because this case is
        // about package *kinds*, which the helper above does not model.
        let meta: Metadata = serde_json::from_str(
            r#"{
              "packages": [
                {"id":"pos-core-id","name":"pos-core","manifest_path":"",
                 "dependencies":[{"name":"serde","kind":null,"features":["derive"]}],
                 "targets":[{"kind":["lib"]}]},
                {"id":"serde-id","name":"serde","manifest_path":"","dependencies":[],
                 "targets":[{"kind":["lib"]}]},
                {"id":"serde_derive-id","name":"serde_derive","manifest_path":"",
                 "dependencies":[],"targets":[{"kind":["proc-macro"]}]},
                {"id":"syn-id","name":"syn","manifest_path":"","dependencies":[],
                 "targets":[{"kind":["lib"]}]}
              ],
              "resolve": {"nodes": [
                {"id":"pos-core-id","deps":[{"pkg":"serde-id","dep_kinds":[{"kind":null}]}]},
                {"id":"serde-id","deps":[{"pkg":"serde_derive-id","dep_kinds":[{"kind":null}]}]},
                {"id":"serde_derive-id","deps":[{"pkg":"syn-id","dep_kinds":[{"kind":null}]}]}
              ]}
            }"#,
        )
        .expect("fixture metadata parses");

        let findings = check(&meta, &allowlist());
        assert!(
            findings.is_empty(),
            "a proc macro was treated as an infrastructure dependency: {findings:?}"
        );
    }

    #[test]
    fn does_not_traverse_through_a_proc_macro() {
        // `syn` is reachable only *through* serde_derive. If the walk did not stop
        // at proc macros, syn would be flagged — and every serde user would have
        // to allow-list the whole macro toolchain, which would make the allow-list
        // meaningless.
        let meta: Metadata = serde_json::from_str(
            r#"{
              "packages": [
                {"id":"pos-core-id","name":"pos-core","manifest_path":"",
                 "dependencies":[{"name":"serde","kind":null,"features":["derive"]}],
                 "targets":[{"kind":["lib"]}]},
                {"id":"serde-id","name":"serde","manifest_path":"","dependencies":[],
                 "targets":[{"kind":["lib"]}]},
                {"id":"serde_derive-id","name":"serde_derive","manifest_path":"",
                 "dependencies":[],"targets":[{"kind":["proc-macro"]}]},
                {"id":"tokio-id","name":"tokio","manifest_path":"","dependencies":[],
                 "targets":[{"kind":["lib"]}]}
              ],
              "resolve": {"nodes": [
                {"id":"pos-core-id","deps":[{"pkg":"serde-id","dep_kinds":[{"kind":null}]}]},
                {"id":"serde-id","deps":[{"pkg":"serde_derive-id","dep_kinds":[{"kind":null}]}]},
                {"id":"serde_derive-id","deps":[{"pkg":"tokio-id","dep_kinds":[{"kind":null}]}]}
              ]}
            }"#,
        )
        .expect("fixture metadata parses");

        let findings = check(&meta, &allowlist());
        assert!(
            findings.is_empty(),
            "the walk crossed a proc-macro boundary: {findings:?}"
        );
    }
}
