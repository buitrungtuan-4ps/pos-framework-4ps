// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The front ends' ESLint baselines may only shrink
//! ([ADR-0172](../../../docs/adr/0172-eslint-checks-the-front-ends.md)), as the Rust one does.
//!
//! Each of `ui/` and `dashboard/` keeps an `eslint-baseline.json`: file → rule → figure, for code
//! that broke a fail level when the ratchet began. ESLint holds each listed file to its figures, and
//! the front end's `pnpm lint` fails on an entry that is no longer needed. What neither can see is
//! the file being edited to raise a figure or add an entry. So `code-stats --check --base` compares
//! each file with its copy at the merge base, and reports the growth the same way as the Rust
//! baseline's: a failure, unless the pull request says `size-baseline: grow <reason>`.

use std::collections::BTreeMap;
use std::path::Path;

use super::ratchet::{HINT_GROW, RULE};
use crate::Finding;
use crate::checks::{Error, read_at_ref};

/// The baselines, from the repository root.
pub const BASELINES: [&str; 2] = ["ui/eslint-baseline.json", "dashboard/eslint-baseline.json"];

/// File → rule → figure.
pub type Table = BTreeMap<String, BTreeMap<String, u64>>;

/// Every growth in each baseline since `merge_base`. A baseline absent there has nothing to compare.
///
/// # Errors
///
/// [`Error`] if a baseline is not valid JSON of the expected shape, or git cannot be run.
pub fn grown_in(root: &Path, merge_base: &str) -> Result<Vec<Finding>, Error> {
    let mut findings = Vec::new();
    for path in BASELINES {
        let Some(before) = read_at_ref(root, merge_base, path)? else {
            continue;
        };
        let now = std::fs::read_to_string(root.join(path)).unwrap_or_else(|_| "{}".to_owned());
        findings.extend(grown(
            path,
            &serde_json::from_str(&before)?,
            &serde_json::from_str(&now)?,
        ));
    }
    Ok(findings)
}

/// One finding per entry of `now` that `base` lacks, or whose figure is higher.
#[must_use]
pub fn grown(path: &str, base: &Table, now: &Table) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (file, rules) in now {
        for (rule, figure) in rules {
            let message = match base.get(file).and_then(|was| was.get(rule)) {
                None => format!("adds `{file}` {rule} {figure}"),
                Some(was) if figure > was => {
                    format!("raises `{file}` {rule} from {was} to {figure}")
                }
                Some(_) => continue,
            };
            findings.push(Finding::new(path, RULE, message).with_hint(HINT_GROW));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::{Table, grown};

    fn table(entries: &[(&str, &str, u64)]) -> Table {
        let mut table = Table::new();
        for (file, rule, figure) in entries {
            table
                .entry((*file).to_owned())
                .or_default()
                .insert((*rule).to_owned(), *figure);
        }
        table
    }

    #[test]
    fn an_eslint_baseline_may_only_shrink() {
        let base = table(&[
            ("src/a.tsx", "max-lines", 1_200),
            ("src/b.tsx", "max-lines-per-function", 150),
        ]);
        let shrunk = table(&[("src/a.tsx", "max-lines", 1_100)]);
        assert!(grown("ui/eslint-baseline.json", &base, &shrunk).is_empty());
        let more = table(&[
            ("src/a.tsx", "max-lines", 1_201),
            ("src/a.tsx", "max-depth", 6),
            ("src/c.tsx", "max-lines", 1_001),
        ]);
        let found: Vec<String> = grown("ui/eslint-baseline.json", &base, &more)
            .into_iter()
            .map(|f| f.message)
            .collect();
        assert_eq!(
            found,
            [
                "adds `src/a.tsx` max-depth 6",
                "raises `src/a.tsx` max-lines from 1200 to 1201",
                "adds `src/c.tsx` max-lines 1001",
            ]
        );
    }
}
