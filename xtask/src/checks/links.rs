// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Internal documentation links must resolve.
//!
//! Documentation rots silently, and a specification whose cross-references are
//! broken is worse than one with none: a reader who follows a dead link concludes
//! the rule does not exist. Only repository-relative links are checked — no
//! network, so this stays in the fast pull-request job.

use std::collections::BTreeSet;

use super::{Error, repo_root};
use crate::Finding;

/// Checks every markdown file outside the frozen archive and `target/`.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let mut findings = Vec::new();

    for path in markdown_files(&root)? {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let text = std::fs::read_to_string(&path)?;
        let dir = path.parent().unwrap_or(&root);

        for (index, line) in text.lines().enumerate() {
            let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
            for target in link_targets(line) {
                if target.starts_with("http://")
                    || target.starts_with("https://")
                    || target.starts_with("mailto:")
                {
                    continue;
                }
                // A bare `#anchor` points inside this file; heading-slug checking
                // is a separate rule, deliberately not conflated with this one.
                let (file_part, _anchor) = target.split_once('#').unwrap_or((target, ""));
                if file_part.is_empty() {
                    continue;
                }
                let resolved = dir.join(file_part);
                if !resolved.exists() {
                    findings.push(
                        Finding::new(&rel, "links", format!("`{target}` does not exist"))
                            .at_line(line_number),
                    );
                }
            }
        }
    }
    Ok(findings)
}

/// Extracts every `](target)` from a line of markdown.
fn link_targets(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b'(' {
            let start = i + 2;
            if let Some(len) = line[start..].find(')') {
                let target = line[start..start + len].trim();
                // Skip inline titles: `](path "Title")`.
                let target = target.split_whitespace().next().unwrap_or(target);
                if !target.is_empty() {
                    out.push(target);
                }
                i = start + len;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn markdown_files(root: &std::path::Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    // The Vietnamese archive is frozen and unmaintained by design, so its internal
    // links are not this check's business.
    let skip: BTreeSet<&str> = [
        "target",
        ".git",
        "vendor",
        "node_modules",
        "vietnamese-design-archive",
    ]
    .into_iter()
    .collect();
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                if !skip.contains(name) {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}
