// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Every workspace crate has a README and a contract card, and the two agree with the workspace
//! ([ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decisions 1
//! and 5).
//!
//! A crate's **contract card** is `[package.metadata.pos]` in its `Cargo.toml`. Cargo ignores it,
//! so it changes no build:
//!
//! ```toml
//! [package.metadata.pos]
//! layer = "adapter"      # core, port, adapter, app, testing, tool or country
//! purpose = "One line."
//! adrs = [15, 26]        # each must exist under docs/adr/
//! owners = ["@maintainers-architecture"]
//! ```
//!
//! The check holds four things for each crate, from `cargo metadata`:
//!
//! 1. `README.md` exists beside the manifest.
//! 2. The card is complete: a known layer, a one-line purpose, ADRs that exist, and owners. Every
//!    owner must be a team `CODEOWNERS` already routes to. Where a `CODEOWNERS` line covers the crate's
//!    directory, the owners must be that line's owners.
//! 3. The README's `**Layer** … · **Owners** …` line says what the card says.
//! 4. The README's `**Depends on**` and `**Used by**` lines list exactly the workspace crates
//!    `cargo metadata` reports. A dependency only tests use is listed after "in tests".
//!
//! Each finding prints the line the README should have, so the fix is a paste.

use std::collections::BTreeSet;
use std::path::Path;

use super::{Error, repo_root};
use crate::Finding;

const RULE: &str = "crates";

/// The layers ADR-0170's target layout names.
pub const LAYERS: [&str; 7] = [
    "core", "port", "adapter", "app", "testing", "tool", "country",
];

/// What the check needs to know about one workspace crate.
#[derive(Debug, Clone, Default)]
pub struct Crate {
    /// The package name.
    pub name: String,
    /// The directory, relative to the repository root.
    pub dir: String,
    /// Workspace crates it depends on outside tests (normal and build dependencies).
    pub normal: BTreeSet<String>,
    /// Workspace crates it depends on in tests only.
    pub dev: BTreeSet<String>,
    /// `package.metadata.pos`, if present.
    pub card: Option<serde_json::Value>,
    /// The README's text, if there is one.
    pub readme: Option<String>,
    /// Whether it has a library target, which is what doc tests run against.
    pub lib: bool,
}

/// Runs the check. `--check` is accepted, and is also what it does without it.
///
/// # Errors
///
/// [`Error`] if `cargo metadata`, `CODEOWNERS` or `docs/adr/` cannot be read, or an argument is
/// unknown.
pub fn run(args: &[String]) -> Result<Vec<Finding>, Error> {
    if let Some(other) = args.iter().find(|argument| *argument != "--check") {
        return Err(
            format!("unknown argument `{other}`; usage: cargo xtask crates [--check]").into(),
        );
    }
    let root = repo_root();
    let crates = load(&root)?;
    let codeowners = std::fs::read_to_string(root.join("CODEOWNERS"))?;
    let adrs = adr_numbers(&root.join("docs/adr"))?;
    Ok(check(&crates, &codeowners, &adrs))
}

/// The workspace crates, from `cargo metadata --no-deps` run at `root`, sorted by directory.
///
/// # Errors
///
/// [`Error`] if `cargo metadata` fails or prints something that is not JSON.
pub fn load(root: &Path) -> Result<Vec<Crate>, Error> {
    let output =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .current_dir(root)
            .output()?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(crates_from(root, &metadata))
}

/// The workspace crates in `cargo metadata --no-deps` output, with their READMEs read from disk.
fn crates_from(root: &Path, metadata: &serde_json::Value) -> Vec<Crate> {
    let packages = metadata["packages"].as_array().cloned().unwrap_or_default();
    let names: BTreeSet<String> = packages
        .iter()
        .filter_map(|p| p["name"].as_str().map(str::to_owned))
        .collect();
    let mut crates = Vec::new();
    for package in &packages {
        let manifest = Path::new(package["manifest_path"].as_str().unwrap_or_default());
        let dir = manifest.parent().unwrap_or(root);
        let mut normal = BTreeSet::new();
        let mut dev = BTreeSet::new();
        for dependency in package["dependencies"].as_array().into_iter().flatten() {
            let Some(name) = dependency["name"]
                .as_str()
                .filter(|name| names.contains(*name))
            else {
                continue;
            };
            if dependency["kind"].as_str() == Some("dev") {
                dev.insert(name.to_owned());
            } else {
                normal.insert(name.to_owned());
            }
        }
        dev.retain(|name| !normal.contains(name));
        crates.push(Crate {
            name: package["name"].as_str().unwrap_or_default().to_owned(),
            dir: dir
                .strip_prefix(root)
                .unwrap_or(dir)
                .to_string_lossy()
                .replace('\\', "/"),
            normal,
            dev,
            card: package["metadata"].get("pos").cloned(),
            readme: std::fs::read_to_string(dir.join("README.md")).ok(),
            lib: package["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|target| {
                    target["kind"].as_array().into_iter().flatten().any(|kind| {
                        matches!(kind.as_str(), Some("lib" | "rlib" | "dylib" | "proc-macro"))
                    })
                }),
        });
    }
    crates.sort_by(|a, b| a.dir.cmp(&b.dir));
    crates
}

/// The numbers of the ADR files under `dir` (`0170-…md` gives 170).
fn adr_numbers(dir: &Path) -> Result<BTreeSet<u32>, Error> {
    let mut numbers = BTreeSet::new();
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(number) = name.get(..4).and_then(|digits| digits.parse().ok()) {
            numbers.insert(number);
        }
    }
    Ok(numbers)
}

/// Evaluates every rule. Pure, so the tests drive it with fixtures.
#[must_use]
pub fn check(crates: &[Crate], codeowners: &str, adrs: &BTreeSet<u32>) -> Vec<Finding> {
    let routes = codeowner_lines(codeowners);
    let teams: BTreeSet<&str> = routes
        .iter()
        .flat_map(|(_, owners)| owners.iter().copied())
        .collect();
    let mut findings = Vec::new();
    for krate in crates {
        let manifest = format!("{}/Cargo.toml", krate.dir);
        let readme_path = format!("{}/README.md", krate.dir);
        let card = card_of(krate, adrs, &teams, &routes, &mut findings, &manifest);
        let Some(readme) = &krate.readme else {
            findings.push(
                Finding::new(
                    &readme_path,
                    RULE,
                    format!("`{}` has no README.md", krate.name),
                )
                .with_hint(
                    "add one with the six headings (What it is, Owns, Depends on / used by, Entry \
                     points, Rules, How to test); crates/pos-core/README.md is an example",
                ),
            );
            continue;
        };
        if let Some((layer, owners)) = card {
            let expected = format!("**Layer** `{layer}` · **Owners** {}", owners.join(", "));
            expect_line(&mut findings, &readme_path, readme, "**Layer** ", &expected);
        }
        let used_by = dependents(crates, &krate.name);
        let depends = format!(
            "- **Depends on** (workspace crates): {}.",
            listing(&krate.normal, &krate.dev)
        );
        let used = format!(
            "- **Used by** (workspace crates): {}.",
            listing(&used_by.0, &used_by.1)
        );
        expect_line(
            &mut findings,
            &readme_path,
            readme,
            "- **Depends on** ",
            &depends,
        );
        expect_line(&mut findings, &readme_path, readme, "- **Used by** ", &used);
    }
    findings
}

/// Checks the card, pushing a finding per problem, and returns its layer and owners when both are
/// usable.
fn card_of(
    krate: &Crate,
    adrs: &BTreeSet<u32>,
    teams: &BTreeSet<&str>,
    routes: &[(String, Vec<&str>)],
    findings: &mut Vec<Finding>,
    manifest: &str,
) -> Option<(String, Vec<String>)> {
    let fail = |message: String| {
        Finding::new(manifest, RULE, message).with_hint(
            "`[package.metadata.pos]` needs layer, purpose, adrs and owners; see the doc comment of \
             xtask/src/checks/crates.rs",
        )
    };
    let Some(card) = &krate.card else {
        findings.push(fail(format!(
            "`{}` has no `[package.metadata.pos]` contract card",
            krate.name
        )));
        return None;
    };
    let layer = card["layer"].as_str().unwrap_or_default().to_owned();
    if !LAYERS.contains(&layer.as_str()) {
        findings.push(fail(format!(
            "`{}`'s layer `{layer}` is not one of {}",
            krate.name,
            LAYERS.join(", ")
        )));
    }
    let purpose = card["purpose"].as_str().unwrap_or_default();
    if purpose.trim().is_empty() || purpose.contains('\n') {
        findings.push(fail(format!(
            "`{}`'s purpose must be one non-empty line",
            krate.name
        )));
    }
    match card["adrs"].as_array() {
        None => findings.push(fail(format!("`{}`'s card has no `adrs` list", krate.name))),
        Some(listed) => {
            for adr in listed {
                let known = adr
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .is_some_and(|n| adrs.contains(&n));
                if !known {
                    findings.push(fail(format!(
                        "`{}` cites ADR {adr}, which is not under docs/adr/",
                        krate.name
                    )));
                }
            }
        }
    }
    let owners: Vec<String> = card["owners"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|owner| owner.as_str().map(str::to_owned))
        .collect();
    if owners.is_empty() {
        findings.push(fail(format!("`{}`'s card names no owners", krate.name)));
    }
    for owner in &owners {
        if !teams.contains(owner.as_str()) {
            findings.push(fail(format!(
                "`{}` names owner `{owner}`, a team CODEOWNERS does not route to; add the team to \
                 CODEOWNERS first, or use one it names",
                krate.name
            )));
        }
    }
    if let Some((pattern, routed)) = route_for(routes, &krate.dir)
        && routed.iter().copied().ne(owners.iter().map(String::as_str))
    {
        findings.push(fail(format!(
            "`{}`'s owners are {owners:?}, but CODEOWNERS routes `{pattern}` to {routed:?}; make them match",
            krate.name
        )));
    }
    LAYERS.contains(&layer.as_str()).then_some((layer, owners))
}

/// Pushes a finding unless `readme` has a line starting with `prefix` that equals `expected`.
fn expect_line(
    findings: &mut Vec<Finding>,
    path: &str,
    readme: &str,
    prefix: &str,
    expected: &str,
) {
    let found = readme
        .lines()
        .enumerate()
        .find(|(_, line)| line.starts_with(prefix));
    match found {
        Some((_, line)) if line == expected => {}
        Some((index, _)) => findings.push(
            Finding::new(
                path,
                RULE,
                format!("this line disagrees with the workspace; it should read: {expected}"),
            )
            .at_line(u32::try_from(index + 1).unwrap_or(u32::MAX)),
        ),
        None => findings.push(Finding::new(
            path,
            RULE,
            format!("no line starting `{prefix}`; add: {expected}"),
        )),
    }
}

/// The crates that depend on `name`, outside tests and in tests only.
pub fn dependents(crates: &[Crate], name: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut normal = BTreeSet::new();
    let mut dev = BTreeSet::new();
    for other in crates {
        if other.normal.contains(name) {
            normal.insert(other.name.clone());
        } else if other.dev.contains(name) {
            dev.insert(other.name.clone());
        }
    }
    (normal, dev)
}

/// `` `a`, `b`; in tests, `c` ``, or "no workspace crate".
fn listing(normal: &BTreeSet<String>, dev: &BTreeSet<String>) -> String {
    let quote = |names: &BTreeSet<String>| {
        names
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut text = if normal.is_empty() {
        "no workspace crate".to_owned()
    } else {
        quote(normal)
    };
    if !dev.is_empty() {
        text.push_str("; in tests, ");
        text.push_str(&quote(dev));
    }
    text
}

/// `CODEOWNERS` as (pattern, owners) pairs, in file order, ignoring comments and blank lines.
fn codeowner_lines(text: &str) -> Vec<(String, Vec<&str>)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pattern = fields.next()?.to_owned();
            Some((pattern, fields.collect()))
        })
        .collect()
}

/// The last `CODEOWNERS` line whose directory pattern covers `dir`, as GitHub resolves it.
///
/// Only the patterns this file uses are understood: `*`, and a rooted directory such as
/// `/crates/pos-core/`. A file pattern never covers a crate directory.
fn route_for<'a>(
    routes: &'a [(String, Vec<&'a str>)],
    dir: &str,
) -> Option<(&'a str, &'a [&'a str])> {
    let path = format!("/{dir}/");
    routes
        .iter()
        .rev()
        .find(|(pattern, _)| {
            pattern == "*" || (pattern.ends_with('/') && path.starts_with(pattern.as_str()))
        })
        .map(|(pattern, owners)| (pattern.as_str(), owners.as_slice()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Crate, check};

    const CODEOWNERS: &str = "# routing\n/crates/core/  @team-a\n/docs/x.md @team-b\n";

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    fn card(layer: &str, owners: &[&str]) -> serde_json::Value {
        serde_json::json!({"layer": layer, "purpose": "One line.", "adrs": [1], "owners": owners})
    }

    fn readme(layer: &str, owners: &str, depends: &str, used: &str) -> String {
        format!(
            "# x\n\n**Layer** `{layer}` · **Owners** {owners}\n\n- **Depends on** (workspace crates): {depends}.\n\
             - **Used by** (workspace crates): {used}.\n"
        )
    }

    /// Two crates that agree with everything: `core` used by `app`, and `app` testing with `core`.
    fn good() -> Vec<Crate> {
        vec![
            Crate {
                name: "core".into(),
                dir: "crates/core".into(),
                card: Some(card("core", &["@team-a"])),
                readme: Some(readme(
                    "core",
                    "@team-a",
                    "no workspace crate",
                    "`app`; in tests, `tool`",
                )),
                ..Crate::default()
            },
            Crate {
                name: "app".into(),
                dir: "crates/app".into(),
                normal: set(&["core"]),
                card: Some(card("app", &["@team-b"])),
                readme: Some(readme("app", "@team-b", "`core`", "no workspace crate")),
                ..Crate::default()
            },
            Crate {
                name: "tool".into(),
                dir: "tool".into(),
                dev: set(&["core"]),
                card: Some(card("tool", &["@team-a"])),
                readme: Some(readme(
                    "tool",
                    "@team-a",
                    "no workspace crate; in tests, `core`",
                    "no workspace crate",
                )),
                ..Crate::default()
            },
        ]
    }

    fn adrs() -> BTreeSet<u32> {
        BTreeSet::from([1])
    }

    fn messages(crates: &[Crate]) -> Vec<String> {
        check(crates, CODEOWNERS, &adrs())
            .into_iter()
            .map(|f| f.message)
            .collect()
    }

    #[test]
    fn a_workspace_that_agrees_passes() {
        assert_eq!(messages(&good()), Vec::<String>::new());
    }

    #[test]
    fn a_missing_readme_or_card_fails() {
        let mut crates = good();
        crates[0].readme = None;
        crates[1].card = None;
        let found = messages(&crates);
        assert!(
            found.iter().any(|m| m.contains("`core` has no README.md")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|m| m.contains("`app` has no `[package.metadata.pos]`")),
            "{found:?}"
        );
    }

    #[test]
    fn an_unknown_layer_a_missing_adr_and_an_unrouted_team_fail() {
        let mut crates = good();
        crates[1].card = Some(serde_json::json!({
            "layer": "service", "purpose": "", "adrs": [1, 9999], "owners": ["@nobody"]
        }));
        let found = messages(&crates);
        for needle in [
            "layer `service`",
            "purpose must be",
            "ADR 9999",
            "`@nobody`",
        ] {
            assert!(
                found.iter().any(|m| m.contains(needle)),
                "{needle}: {found:?}"
            );
        }
    }

    #[test]
    fn owners_must_be_what_codeowners_routes_the_directory_to() {
        let mut crates = good();
        crates[0].card = Some(card("core", &["@team-b"]));
        crates[0].readme = Some(readme(
            "core",
            "@team-b",
            "no workspace crate",
            "`app`; in tests, `tool`",
        ));
        let found = messages(&crates);
        assert!(
            found
                .iter()
                .any(|m| m.contains("CODEOWNERS routes `/crates/core/`")),
            "{found:?}"
        );
    }

    #[test]
    fn a_readme_that_disagrees_is_told_the_line_it_should_have() {
        let mut crates = good();
        crates[1].readme = Some(readme(
            "core",
            "@team-b",
            "`core`, `tool`",
            "no workspace crate",
        ));
        let found = check(&crates, CODEOWNERS, &adrs());
        assert!(
            found.iter().any(|f| f
                .message
                .ends_with("should read: **Layer** `app` · **Owners** @team-b")
                && f.line == Some(3)),
            "{found:?}"
        );
        assert!(
            found.iter().any(|f| f
                .message
                .ends_with("should read: - **Depends on** (workspace crates): `core`.")),
            "{found:?}"
        );
    }

    #[test]
    fn a_readme_without_the_lines_is_told_what_to_add() {
        let mut crates = good();
        crates[2].readme = Some("# tool\n".into());
        let found = messages(&crates);
        assert!(
            found
                .iter()
                .any(|m| m.starts_with("no line starting `**Layer** `")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|m| m.contains("add: - **Used by** (workspace crates): no workspace crate.")),
            "{found:?}"
        );
    }
}
