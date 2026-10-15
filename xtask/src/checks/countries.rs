// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Keeps a country module reachable from the moment it exists on disk.
//!
//! [ADR-0027](../../../docs/adr/0027-country-modules.md) makes adding a country a three-edit
//! operation: its own manifest, the workspace `members`, and a `country-<cc>` feature in every binary
//! that should carry it. Three edits in three files is fine when they fail loudly. Two of them do not.
//!
//! A directory under `countries/` that is **not a workspace member** is not compiled, not linted and
//! not tested — it looks finished, `cargo test` says everything passes, and the omission surfaces when
//! somebody tries to enable it. A crate that is a member but is named something other than
//! `pos-country-<cc>` breaks the convention the feature names and the registry macro both rely on.
//! And a country wired into the workspace but into no binary's features compiles, passes its own
//! tests, and can never be selected.
//!
//! So this check enforces, for every directory under `countries/`:
//!
//! 1. the directory name is a lower-case ISO 3166-1 alpha-2 code;
//! 2. it holds a `Cargo.toml` whose package is named `pos-country-<cc>`;
//! 3. it is listed in the workspace `members`;
//! 4. at least one binary crate declares a `country-<cc>` feature enabling it.
//!
//! Rule 4 has nothing to check until `pos_edge` and `pos_cloud` exist (`docs/roadmap.md` P5 and P7).
//! Rather than pass silently and start enforcing by surprise, the check *reports* that it found no
//! binaries — a check that quietly does nothing is worse than no check, because it is mistaken for
//! coverage.

use std::collections::BTreeSet;

use super::{Error, repo_root};
use crate::Finding;

const RULE: &str = "country-module";

/// Where a country module lives, relative to the repository root.
const COUNTRIES_DIR: &str = "countries";

/// Checks every country module is named, wired in, and selectable.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let countries_dir = root.join(COUNTRIES_DIR);
    if !countries_dir.is_dir() {
        // No countries yet is a legitimate state for a fork that has removed them all.
        eprintln!("xtask countries: no {COUNTRIES_DIR}/ directory, nothing to check");
        return Ok(Vec::new());
    }

    let tree = read_tree(&root, &countries_dir)?;
    let findings = check(&tree);

    if tree.binaries.is_empty() {
        let codes = tree
            .countries
            .iter()
            .filter(|(name, _)| is_country_code(name))
            .count();
        eprintln!(
            "xtask countries: {codes} module(s) checked for naming and workspace membership. \
             No binary crates found, so feature selectability is not yet checked — that starts when \
             pos_edge and pos_cloud arrive (docs/roadmap.md P5, P7)."
        );
    }

    Ok(findings)
}

/// What the check reads from the tree, so the rules themselves run on text.
struct Tree {
    /// The workspace `members`.
    members: BTreeSet<String>,
    /// Every binary crate's manifest path and text, from [`binary_manifests`].
    binaries: Vec<(String, String)>,
    /// Each directory under `countries/`, sorted by name, with its `Cargo.toml` if one reads.
    countries: Vec<(String, Option<String>)>,
}

/// Reads the workspace manifest, the binary manifests and every country directory.
fn read_tree(root: &std::path::Path, countries_dir: &std::path::Path) -> Result<Tree, Error> {
    let workspace = std::fs::read_to_string(root.join("Cargo.toml"))?;
    let members = workspace_members(&workspace);
    let binaries = binary_manifests(root)?;

    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(countries_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    entries.sort();

    let countries = entries
        .iter()
        .filter_map(|path| {
            let name = path.file_name().and_then(|name| name.to_str())?;
            let manifest = std::fs::read_to_string(path.join("Cargo.toml")).ok();
            Some((name.to_owned(), manifest))
        })
        .collect();
    Ok(Tree {
        members,
        binaries,
        countries,
    })
}

/// The findings for every country directory in `tree`, in the order the directories were read.
fn check(tree: &Tree) -> Vec<Finding> {
    tree.countries
        .iter()
        .flat_map(|(name, manifest)| country_findings(tree, name, manifest.as_deref()))
        .collect()
}

/// One directory's findings: rules 1–4 in order. A directory that is not a code, or has no
/// manifest, is reported for that alone, because every later rule derives from both.
fn country_findings(tree: &Tree, name: &str, manifest: Option<&str>) -> Vec<Finding> {
    let rel = format!("{COUNTRIES_DIR}/{name}");
    if !is_country_code(name) {
        return vec![not_a_code(&rel, name)];
    }
    let Some(manifest) = manifest else {
        return vec![no_manifest(&rel)];
    };

    let expected_package = format!("pos-country-{name}");
    let mut findings = Vec::new();
    findings.extend(misnamed_package(&rel, manifest, &expected_package));
    findings.extend(not_a_member(&rel, &tree.members));
    findings.extend(not_selectable(
        &rel,
        name,
        &expected_package,
        &tree.binaries,
    ));
    findings
}

/// Rule 1: the directory name is a country code.
fn not_a_code(rel: &str, name: &str) -> Finding {
    Finding::new(
        format!("{rel}/Cargo.toml"),
        RULE,
        format!("`{name}` is not a lower-case ISO 3166-1 alpha-2 country code"),
    )
    .with_hint(
        "rename the directory to two lower-case letters, e.g. `countries/vn`. The \
         feature name, the package name and the registry all derive from it"
            .to_owned(),
    )
}

/// Rule 2, first half: there is a manifest at all.
fn no_manifest(rel: &str) -> Finding {
    Finding::new(
        format!("{rel}/Cargo.toml"),
        RULE,
        format!("`{rel}` has no Cargo.toml, so nothing there is compiled"),
    )
    .with_hint("add a crate, or delete the directory".to_owned())
}

/// Rule 2, second half: the package is named `pos-country-<cc>`.
fn misnamed_package(rel: &str, manifest: &str, expected_package: &str) -> Option<Finding> {
    match package_name(manifest) {
        Some(actual) if actual == expected_package => None,
        Some(actual) => Some(
            Finding::new(
                format!("{rel}/Cargo.toml"),
                RULE,
                format!("package is `{actual}`, expected `{expected_package}`"),
            )
            .with_hint(
                "the feature name `country-<cc>` and the registry macro both derive from the \
                 directory code, so the package has to agree with it"
                    .to_owned(),
            ),
        ),
        None => Some(Finding::new(
            format!("{rel}/Cargo.toml"),
            RULE,
            "no `[package] name` found".to_owned(),
        )),
    }
}

/// Rule 3: the directory is a workspace member.
fn not_a_member(rel: &str, members: &BTreeSet<String>) -> Option<Finding> {
    if members.contains(rel) {
        return None;
    }
    Some(
        Finding::new(
            "Cargo.toml",
            RULE,
            format!(
                "`{rel}` is not a workspace member, so it is never compiled, linted or \
                 tested — and `cargo test` will report success without it"
            ),
        )
        .with_hint(format!(
            "add \"{rel}\" to `members` in the workspace Cargo.toml"
        )),
    )
}

/// Rule 4: some binary declares a `country-<cc>` feature enabling the package. Skipped, loudly
/// (see [`run`]), until there is a binary to check against.
fn not_selectable(
    rel: &str,
    name: &str,
    expected_package: &str,
    binaries: &[(String, String)],
) -> Option<Finding> {
    if binaries.is_empty() {
        return None;
    }
    let feature = format!("country-{name}");
    let selectable = binaries
        .iter()
        .any(|(_, manifest)| declares_feature(manifest, &feature, expected_package));
    if selectable {
        return None;
    }
    Some(
        Finding::new(
            format!("{rel}/Cargo.toml"),
            RULE,
            format!(
                "no binary declares a `{feature}` feature enabling `{expected_package}`, so \
                 this country compiles but can never be selected"
            ),
        )
        .with_hint(format!(
            "add `{feature} = [\"{expected_package}\"]` to the `[features]` table of \
             pos_edge, pos_cloud, or both, and an arm to their `country_registry!` call"
        )),
    )
}

/// Whether `name` is two lower-case ASCII letters.
fn is_country_code(name: &str) -> bool {
    let bytes = name.as_bytes();
    matches!(bytes, [first, second] if first.is_ascii_lowercase() && second.is_ascii_lowercase())
}

/// The `members` entries of a workspace manifest.
///
/// Parsed with `toml` rather than by scanning lines, so a comment mentioning a path cannot be
/// mistaken for membership — which is exactly the false pass that would make this check useless.
fn workspace_members(manifest: &str) -> BTreeSet<String> {
    let Ok(value) = toml::from_str::<toml::Value>(manifest) else {
        return BTreeSet::new();
    };
    value
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .map(|members| {
            members
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The `[package] name` of a manifest.
fn package_name(manifest: &str) -> Option<String> {
    toml::from_str::<toml::Value>(manifest)
        .ok()?
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_owned)
}

/// Every manifest under `crates/` that produces a binary, with its text.
///
/// A crate counts as a binary when it has a `[[bin]]` table or a `src/main.rs`. Both, because a
/// binary declared implicitly by file layout is the common case and one declared explicitly is the
/// one a workspace reaches for when it needs two.
fn binary_manifests(root: &std::path::Path) -> Result<Vec<(String, String)>, Error> {
    let crates_dir = root.join("crates");
    if !crates_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    let mut stack = vec![crates_dir];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)?.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                // `target` can appear inside a crate directory and holds nothing worth reading.
                if path.file_name().and_then(|n| n.to_str()) != Some("target") {
                    stack.push(path);
                }
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) != Some("Cargo.toml") {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            let has_bin_table = toml::from_str::<toml::Value>(&text)
                .ok()
                .is_some_and(|value| value.get("bin").is_some());
            let crate_dir = path.parent().unwrap_or(&path);
            if has_bin_table || crate_dir.join("src/main.rs").is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                found.push((rel, text));
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Whether `manifest` declares `feature` and that feature enables `package`.
///
/// Both halves matter. A feature that exists but enables nothing is a switch wired to no lamp, and it
/// would satisfy a check that only looked for the name.
fn declares_feature(manifest: &str, feature: &str, package: &str) -> bool {
    toml::from_str::<toml::Value>(manifest)
        .ok()
        .and_then(|value| value.get("features")?.get(feature)?.as_array().cloned())
        .is_some_and(|enables| {
            enables.iter().filter_map(toml::Value::as_str).any(|entry| {
                // Cargo writes an optional dependency as `dep:name` or as the bare name.
                entry == package || entry == format!("dep:{package}")
            })
        })
}

#[cfg(test)]
mod tests {
    use super::{Tree, check, declares_feature, is_country_code, package_name, workspace_members};

    const WIRED_BINARY: &str = "[package]\nname = \"pos_edge\"\n[features]\n\
                                country-vn = [\"dep:pos-country-vn\"]\n";

    /// A tree whose `members` are `members`, with one binary manifest when `binary` is given.
    fn tree(members: &[&str], binary: Option<&str>, countries: &[(&str, Option<&str>)]) -> Tree {
        Tree {
            members: members.iter().map(|member| (*member).to_owned()).collect(),
            binaries: binary
                .map(|text| vec![("crates/pos-edge/Cargo.toml".to_owned(), text.to_owned())])
                .unwrap_or_default(),
            countries: countries
                .iter()
                .map(|(name, manifest)| ((*name).to_owned(), manifest.map(str::to_owned)))
                .collect(),
        }
    }

    fn manifest(package: &str) -> String {
        format!("[package]\nname = \"{package}\"\n")
    }

    /// Each finding as `file: message`, in the order the check reported them.
    fn reported(tree: &Tree) -> Vec<String> {
        check(tree)
            .iter()
            .map(|finding| format!("{}: {}", finding.file, finding.message))
            .collect()
    }

    #[test]
    fn a_country_named_wired_and_selectable_has_no_finding() {
        let vn = manifest("pos-country-vn");
        let tree = tree(
            &["countries/vn"],
            Some(WIRED_BINARY),
            &[("vn", Some(vn.as_str()))],
        );
        assert!(reported(&tree).is_empty(), "{:?}", reported(&tree));
    }

    #[test]
    fn a_directory_that_is_not_a_code_is_reported_once_and_checked_no_further() {
        // A member with the wrong package, and wired into nothing: had the check gone on, each of
        // those would be a finding of its own.
        let tree = tree(
            &["countries/VN"],
            Some(WIRED_BINARY),
            &[("VN", Some("[package]\nname = \"x\"\n"))],
        );
        assert_eq!(
            reported(&tree),
            ["countries/VN/Cargo.toml: `VN` is not a lower-case ISO 3166-1 alpha-2 country code"]
        );
        assert!(
            check(&tree)[0]
                .fix_hint
                .as_deref()
                .is_some_and(|hint| hint.contains("rename the directory"))
        );
    }

    #[test]
    fn a_directory_with_no_manifest_is_reported_once_and_checked_no_further() {
        let tree = tree(&[], Some(WIRED_BINARY), &[("zz", None)]);
        assert_eq!(
            reported(&tree),
            [
                "countries/zz/Cargo.toml: `countries/zz` has no Cargo.toml, so nothing there is compiled"
            ]
        );
    }

    #[test]
    fn every_rule_a_country_breaks_is_reported_in_rule_order() {
        // The package, then the membership, then the feature: the order a reviewer fixes them in.
        let jp = manifest("pos-country-japan");
        let tree = tree(&[], Some(WIRED_BINARY), &[("jp", Some(jp.as_str()))]);
        assert_eq!(
            reported(&tree),
            [
                "countries/jp/Cargo.toml: package is `pos-country-japan`, expected `pos-country-jp`",
                "Cargo.toml: `countries/jp` is not a workspace member, so it is never compiled, \
                 linted or tested — and `cargo test` will report success without it",
                "countries/jp/Cargo.toml: no binary declares a `country-jp` feature enabling \
                 `pos-country-jp`, so this country compiles but can never be selected",
            ]
        );
    }

    #[test]
    fn a_manifest_without_a_package_name_says_so() {
        let tree = tree(&["countries/in"], None, &[("in", Some("[dependencies]\n"))]);
        assert_eq!(
            reported(&tree),
            ["countries/in/Cargo.toml: no `[package] name` found"]
        );
    }

    #[test]
    fn with_no_binary_selectability_is_not_checked() {
        let vn = manifest("pos-country-vn");
        let unwired = tree(
            &["countries/vn"],
            Some("[package]\nname = \"pos_edge\"\n"),
            &[("vn", Some(vn.as_str()))],
        );
        assert_eq!(
            check(&unwired).len(),
            1,
            "a binary that wires nothing is a finding"
        );

        let no_binary = tree(&["countries/vn"], None, &[("vn", Some(vn.as_str()))]);
        assert!(check(&no_binary).is_empty());
    }

    #[test]
    fn countries_are_reported_in_the_order_they_were_read() {
        let tree = tree(&[], None, &[("zz", None), ("aa", None), ("Bad", None)]);
        let files: Vec<String> = check(&tree)
            .into_iter()
            .map(|finding| finding.file)
            .collect();
        assert_eq!(
            files,
            [
                "countries/zz/Cargo.toml",
                "countries/aa/Cargo.toml",
                "countries/Bad/Cargo.toml"
            ]
        );
    }

    #[test]
    fn a_country_code_is_two_lower_case_letters() {
        assert!(is_country_code("vn"));
        assert!(is_country_code("zz"));
        assert!(
            !is_country_code("VN"),
            "upper-case is the code, not the directory"
        );
        assert!(!is_country_code("vnm"));
        assert!(!is_country_code("v"));
        assert!(!is_country_code(""));
        assert!(!is_country_code("v1"));
    }

    #[test]
    fn members_are_parsed_rather_than_grepped() {
        // The false pass this prevents: a comment naming a path would satisfy a line scan, and the
        // country would then be reported as wired in while never being compiled.
        let manifest = r#"
[workspace]
members = ["crates/pos-proto", "countries/zz"]
# countries/jp is deliberately not a member yet
"#;
        let members = workspace_members(manifest);
        assert!(members.contains("countries/zz"));
        assert!(
            !members.contains("countries/jp"),
            "a path inside a comment is not membership"
        );
    }

    #[test]
    fn a_malformed_workspace_manifest_yields_no_members() {
        // Which reports every country as unwired rather than reporting all of them as fine. Failing
        // loudly on a manifest this check cannot read is the safe direction.
        assert!(workspace_members("[workspace\nmembers = broken").is_empty());
    }

    #[test]
    fn the_package_name_is_read_from_the_package_table() {
        let manifest = "[package]\nname = \"pos-country-vn\"\nversion = \"0.1.0\"\n";
        assert_eq!(package_name(manifest).as_deref(), Some("pos-country-vn"));
        assert!(package_name("[dependencies]\nserde = \"1\"\n").is_none());
    }

    #[test]
    fn a_feature_must_actually_enable_the_country_crate() {
        let wired = r#"
[features]
country-vn = ["pos-country-vn"]
"#;
        assert!(declares_feature(wired, "country-vn", "pos-country-vn"));

        let dep_prefixed = r#"
[features]
country-vn = ["dep:pos-country-vn"]
"#;
        assert!(
            declares_feature(dep_prefixed, "country-vn", "pos-country-vn"),
            "Cargo's `dep:` spelling is the same wiring"
        );

        // The trap: a feature that exists and enables nothing. A check looking only for the name
        // would pass this, and the country would still be unselectable.
        let empty = "[features]\ncountry-vn = []\n";
        assert!(!declares_feature(empty, "country-vn", "pos-country-vn"));

        let wrong_crate = r#"
[features]
country-vn = ["pos-country-jp"]
"#;
        assert!(!declares_feature(
            wrong_crate,
            "country-vn",
            "pos-country-vn"
        ));

        assert!(!declares_feature(
            "[features]\nother = []\n",
            "country-vn",
            "pos-country-vn"
        ));
    }
}
