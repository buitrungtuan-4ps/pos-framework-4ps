// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Every path `docs/repository-map.md` names in backticks exists (P11 item 5).
//!
//! The map is how a contributor finds the files a change touches, so a pointer to a file that moved
//! is worse than none. `xtask links` already holds its Markdown links; this holds the paths written
//! as code. A token counts as a path when it has a `/` and its first segment is a top-level entry
//! of the repository (`crates/…`, `ui/…`, `.github/…`). One with a placeholder (`<area>`) or a
//! wildcard (`*`) names the directory before it, which must exist.

use super::{Error, repo_root};
use crate::Finding;

const RULE: &str = "repo-map";

/// The document this checks.
pub const MAP: &str = "docs/repository-map.md";

/// Fewer paths than this means the reader broke, not that the map shrank: it names about a hundred.
const MIN_PATHS: usize = 50;

/// Each path-like token in `text` with its 1-based line, for top-level entries `tops`.
#[must_use]
pub fn paths(text: &str, tops: &[String]) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        for (position, token) in line.split('`').enumerate() {
            // Odd positions are inside backticks.
            if position % 2 == 0 || token.contains(' ') || !token.contains('/') {
                continue;
            }
            let first = token.split('/').next().unwrap_or_default();
            if tops.iter().any(|top| top == first) {
                found.push((index + 1, token.to_owned()));
            }
        }
    }
    found
}

/// What must exist for `token`: itself, or for a placeholder or wildcard the directory it starts in.
#[must_use]
pub fn must_exist(token: &str) -> String {
    let cut = token.find(['<', '*']).map_or(token, |at| {
        let before = token.get(..at).unwrap_or_default();
        before
            .rsplit_once('/')
            .map_or(before, |(directory, _)| directory)
    });
    cut.trim_end_matches('/').to_owned()
}

/// Runs the check.
///
/// # Errors
///
/// When the map or the repository root cannot be read.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let text = std::fs::read_to_string(root.join(MAP))?;
    let tops: Vec<String> = std::fs::read_dir(&root)?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != ".git")
        .collect();
    let named = paths(&text, &tops);
    if named.len() < MIN_PATHS {
        return Ok(vec![Finding::new(
            MAP,
            RULE,
            format!(
                "only {} paths were read, which means the reader stopped matching the map rather \
                 than that the map lost them",
                named.len()
            ),
        )]);
    }
    Ok(named
        .into_iter()
        .filter(|(_, token)| !root.join(must_exist(token)).exists())
        .map(|(line, token)| {
            Finding::new(MAP, RULE, format!("names `{token}`, which does not exist"))
                .at_line(u32::try_from(line).unwrap_or(u32::MAX))
                .with_hint("point it at where the file is now, or drop the row")
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{must_exist, paths};

    #[test]
    fn reads_only_paths_under_a_top_level_entry() {
        let tops = vec!["crates".to_owned(), ".github".to_owned()];
        let text = "| `crates/pos-core/src/billing.rs` and `app/mod.rs` |\n\
                    `.github/workflows/pr.yml`, `cargo xtask new crate <name>`, `billing.rs`";
        assert_eq!(
            paths(text, &tops),
            [
                (1, "crates/pos-core/src/billing.rs".to_owned()),
                (2, ".github/workflows/pr.yml".to_owned()),
            ]
        );
    }

    #[test]
    fn a_placeholder_or_wildcard_names_its_directory() {
        assert_eq!(
            must_exist("crates/pos-edge/src/http/<area>.rs"),
            "crates/pos-edge/src/http"
        );
        assert_eq!(must_exist("crates/adapters/<name>/"), "crates/adapters");
        assert_eq!(
            must_exist("ui/src/api/generated/*.d.ts"),
            "ui/src/api/generated"
        );
        assert_eq!(must_exist("ui/src/App.tsx"), "ui/src/App.tsx");
        assert_eq!(must_exist("countries/<cc>/"), "countries");
    }
}
