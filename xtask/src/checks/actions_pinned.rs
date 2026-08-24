// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Every GitHub action must be pinned to a full commit SHA, never a tag.
//!
//! A tag is mutable. Whoever controls the action's repository can move `v4` to new
//! code, and that code runs with this workflow's token — the supply-chain risk
//! `docs/engineering-guide.md` §12 names explicitly. A 40-character SHA cannot be
//! moved.
//!
//! Local composite actions (`uses: ./.github/actions/...`) are exempt: they are in
//! this repository and travel with the commit under review.

use super::{Error, repo_root};
use crate::Finding;

/// Scans every workflow and local action definition.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let mut findings = Vec::new();

    for path in yaml_files(&root.join(".github"))? {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        for (index, line) in std::fs::read_to_string(&path)?.lines().enumerate() {
            let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
            let Some(reference) = uses_reference(line) else {
                continue;
            };
            if reference.starts_with("./") || reference.starts_with("docker://") {
                continue;
            }
            match reference.split_once('@') {
                Some((action, rev)) if is_sha(rev) => {
                    let _ = action;
                }
                Some((action, rev)) => findings.push(
                    Finding::new(&rel, "actions-pinned", format!(
                        "`{action}` is pinned to `{rev}`, which is a mutable reference"
                    ))
                    .at_line(line_number)
                    .with_hint("pin to the full 40-character commit SHA, with the tag in a trailing comment"),
                ),
                None => findings.push(
                    Finding::new(&rel, "actions-pinned", format!("`{reference}` has no version at all"))
                        .at_line(line_number)
                        .with_hint("pin to a full 40-character commit SHA"),
                ),
            }
        }
    }
    Ok(findings)
}

/// Extracts the reference from a `uses:` line, ignoring comments.
fn uses_reference(line: &str) -> Option<&str> {
    let trimmed = line.trim().trim_start_matches("- ").trim();
    let rest = trimmed.strip_prefix("uses:")?;
    let value = rest.split('#').next().unwrap_or("").trim();
    (!value.is_empty()).then_some(value)
}

fn is_sha(rev: &str) -> bool {
    rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit())
}

fn yaml_files(dir: &std::path::Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            out.extend(yaml_files(&path)?);
        } else if path.extension().is_some_and(|e| e == "yml" || e == "yaml") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}
