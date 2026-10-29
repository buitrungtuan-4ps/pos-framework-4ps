// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Each front end's generated API types were generated from the OpenAPI document as it is committed
//! ([ADR-0177](../../../docs/adr/0177-openapi-is-the-route-map.md) decision 6).
//!
//! `pnpm gen:api` writes the types and heads them with the git blob id of the document they came
//! from. This check compares that id with `git hash-object` of the committed document, so a
//! document regenerated without its types fails `just preflight`, and the gate needs no Node. `tsc`,
//! already in each front end's build, proves the types compile.

use super::{Error, repo_root};
use crate::Finding;

const RULE: &str = "api-types";

/// Each generated file, the document it comes from, and the front end that generates it.
pub const GENERATED: &[(&str, &str, &str)] = &[
    (
        "ui/src/api/generated/edge.d.ts",
        "docs/openapi-edge.json",
        "ui",
    ),
    (
        "dashboard/src/api/generated/admin.d.ts",
        "docs/openapi-admin.json",
        "dashboard",
    ),
];

/// The blob id a generated file's first line names, if it has the header `pnpm gen:api` writes.
#[must_use]
pub fn named_blob(first_line: &str) -> Option<&str> {
    let rest = first_line.strip_prefix("// @generated from ")?;
    let (_, after) = rest.split_once(", git blob ")?;
    let blob = after.split(',').next()?;
    (blob.len() == 40 && blob.chars().all(|character| character.is_ascii_hexdigit()))
        .then_some(blob)
}

/// The finding for a generated file that names `named` (or nothing) while its document is `actual`.
#[must_use]
pub fn compare(
    generated: &str,
    document: &str,
    front_end: &str,
    named: Option<&str>,
    actual: &str,
) -> Option<Finding> {
    let hint = format!("run `pnpm gen:api` in {front_end}/ and commit the result");
    match named {
        Some(blob) if blob == actual => None,
        Some(blob) => Some(
            Finding::new(
                generated,
                RULE,
                format!(
                    "was generated from `{document}` at blob {blob}, but the committed document is \
                     {actual}"
                ),
            )
            .with_hint(hint),
        ),
        None => Some(
            Finding::new(
                generated,
                RULE,
                "has no `// @generated from …, git blob …` header",
            )
            .with_hint(hint),
        ),
    }
}

/// `git hash-object` of `path`, relative to the repository root.
fn blob_of(root: &std::path::Path, path: &str) -> Result<String, Error> {
    let output = std::process::Command::new("git")
        .args(["hash-object", "--", path])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git hash-object {path} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Runs the check. `--check` is accepted and is the only mode: the types are written by Node, which
/// the gate does not need.
///
/// # Errors
///
/// When `git hash-object` cannot run. A missing or stale file is a finding.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let mut findings = Vec::new();
    for (generated, document, front_end) in GENERATED {
        let actual = blob_of(&root, document)?;
        let Ok(text) = std::fs::read_to_string(root.join(generated)) else {
            findings.push(
                Finding::new(*generated, RULE, "is missing").with_hint(format!(
                    "run `pnpm gen:api` in {front_end}/ and commit the result"
                )),
            );
            continue;
        };
        let named = named_blob(text.lines().next().unwrap_or_default());
        findings.extend(compare(generated, document, front_end, named, &actual));
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::{compare, named_blob};

    const BLOB: &str = "89ac29f5d2404f5fcb600167dd2b29ae7f2178e5";

    fn header(blob: &str) -> String {
        format!(
            "// @generated from docs/openapi-edge.json, git blob {blob}, by `pnpm gen:api` \
             (ADR-0177). Do not edit."
        )
    }

    #[test]
    fn reads_the_blob_the_header_names() {
        assert_eq!(named_blob(&header(BLOB)), Some(BLOB));
    }

    #[test]
    fn a_file_without_the_header_names_nothing() {
        assert_eq!(named_blob("/**"), None);
        assert_eq!(named_blob(&header("not-a-blob")), None);
    }

    #[test]
    fn current_types_pass_and_stale_or_headless_ones_do_not() {
        let file = "ui/src/api/generated/edge.d.ts";
        let document = "docs/openapi-edge.json";
        assert!(compare(file, document, "ui", Some(BLOB), BLOB).is_none());
        let stale = compare(file, document, "ui", Some(BLOB), &"0".repeat(40));
        assert!(
            stale.is_some_and(|finding| finding.message.contains("was generated from")),
            "a stale file passed"
        );
        let headless = compare(file, document, "ui", None, BLOB);
        assert!(
            headless.is_some_and(|finding| finding.message.contains("no `// @generated")),
            "a file with no header passed"
        );
    }
}
