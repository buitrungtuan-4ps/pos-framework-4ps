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

    let workspace = std::fs::read_to_string(root.join("Cargo.toml"))?;
    let members = workspace_members(&workspace);
    let binaries = binary_manifests(&root)?;

    let mut findings = Vec::new();
    let mut codes = Vec::new();

    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&countries_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    entries.sort();

    for path in entries {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let rel = format!("{COUNTRIES_DIR}/{name}");

        if !is_country_code(name) {
            findings.push(
                Finding::new(
                    format!("{rel}/Cargo.toml"),
                    RULE,
                    format!("`{name}` is not a lower-case ISO 3166-1 alpha-2 country code"),
                )
                .with_hint(
                    "rename the directory to two lower-case letters, e.g. `countries/vn`. The \
                     feature name, the package name and the registry all derive from it"
                        .to_owned(),
                ),
            );
            continue;
        }
        codes.push(name.to_owned());

        let manifest_path = path.join("Cargo.toml");
        let manifest = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => text,
            Err(_) => {
                findings.push(
                    Finding::new(
                        format!("{rel}/Cargo.toml"),
                        RULE,
                        format!("`{rel}` has no Cargo.toml, so nothing there is compiled"),
                    )
                    .with_hint("add a crate, or delete the directory".to_owned()),
                );
                continue;
            }
        };

        let expected_package = format!("pos-country-{name}");
        match package_name(&manifest) {
            Some(actual) if actual == expected_package => {}
            Some(actual) => findings.push(
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
            None => findings.push(Finding::new(
                format!("{rel}/Cargo.toml"),
                RULE,
                "no `[package] name` found".to_owned(),
            )),
        }

        if !members.contains(rel.as_str()) {
            findings.push(
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
            );
        }

        // Rule 4. Skipped, loudly, until there is a binary to check against.
        if binaries.is_empty() {
            continue;
        }
        let feature = format!("country-{name}");
        let selectable = binaries
            .iter()
            .any(|(_, manifest)| declares_feature(manifest, &feature, &expected_package));
        if !selectable {
            findings.push(
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
            );
        }
    }

    if binaries.is_empty() {
        eprintln!(
            "xtask countries: {} module(s) checked for naming and workspace membership. \
             No binary crates found, so feature selectability is not yet checked — that starts when \
             pos_edge and pos_cloud arrive (docs/roadmap.md P5, P7).",
            codes.len()
        );
    }

    Ok(findings)
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
    use super::{declares_feature, is_country_code, package_name, workspace_members};

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
