// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Refuses a removal from a committed snapshot.
//!
//! The snapshot files themselves are rendered by the crates that own them — `pos-proto`
//! writes `docs/snapshots/events.txt` from its own catalogue, and a test there fails when
//! the two disagree. This check answers the other question, which only git can: has
//! anything **disappeared** since the base branch?
//!
//! That distinction matters because the two failures need different remedies. A snapshot
//! that does not match its code is regenerated. A snapshot that lost a line means a
//! published contract was broken, and the remedy is to put it back and deprecate instead.
//!
//! # What may change and what may not
//!
//! A `field=` or event-type line may only be added. Those are contracts: an event type or
//! a payload field, once published, may be added to but never renamed or removed
//! (`docs/naming-and-api.md` §1, §13). An older edge that has been offline for a week
//! still expects them.
//!
//! A `schema_version=` line may change, because bumping one is a deliberate act that
//! already carries its own obligations — a mandatory upgrade note in the changelog, and
//! two releases running in parallel.

use std::collections::BTreeSet;

use super::{Error, repo_root};
use crate::Finding;

/// Snapshot files under this check's protection.
const SNAPSHOTS: &[&str] = &["docs/snapshots/events.txt"];

/// Lines whose disappearance is a deliberate change rather than a broken contract.
fn is_mutable(line: &str) -> bool {
    line.starts_with('#') || line.contains("\tschema_version=")
}

/// Compares each snapshot against the base ref.
pub fn run(args: &[String]) -> Result<Vec<Finding>, Error> {
    let base = base_ref(args);
    let root = repo_root();
    let mut findings = Vec::new();

    for path in SNAPSHOTS {
        let Some(previous) = read_at_ref(&root, &base, path)? else {
            // Not present on the base branch, so this pull request introduces it.
            // Nothing can have been removed from a file that did not exist.
            continue;
        };
        let current = std::fs::read_to_string(root.join(path)).unwrap_or_default();

        let before: BTreeSet<&str> = previous.lines().filter(|l| !is_mutable(l)).collect();
        let after: BTreeSet<&str> = current.lines().filter(|l| !is_mutable(l)).collect();

        for removed in before.difference(&after) {
            findings.push(
                Finding::new(
                    *path,
                    "snapshot",
                    format!("`{removed}` was removed from the snapshot"),
                )
                .with_hint(
                    "a published event type or payload field is a contract: add to it, \
                     or deprecate it for at least two releases, but do not remove it — \
                     an edge that has been offline for a week still expects it",
                ),
            );
        }
    }
    Ok(findings)
}

fn base_ref(args: &[String]) -> String {
    let mut iter = args.iter();
    while let Some(argument) = iter.next() {
        if argument == "--base"
            && let Some(value) = iter.next()
        {
            return value.clone();
        }
    }
    "origin/main".to_owned()
}

/// The file's contents at `reference`, or `None` if it did not exist there.
fn read_at_ref(
    root: &std::path::Path,
    reference: &str,
    path: &str,
) -> Result<Option<String>, Error> {
    let output = std::process::Command::new("git")
        .args(["show", &format!("{reference}:{path}")])
        .current_dir(root)
        .output()?;
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::is_mutable;

    #[test]
    fn a_field_line_is_a_contract() {
        assert!(!is_mutable("sales.order.opened\tfield=order_id"));
    }

    #[test]
    fn a_version_line_may_change() {
        // Bumping a schema version is deliberate and already carries a mandatory
        // upgrade note, so it does not need this gate as well.
        assert!(is_mutable("sales.order.opened\tschema_version=1"));
    }

    #[test]
    fn comments_are_ignored() {
        assert!(is_mutable("# Event catalogue snapshot. Generated."));
    }
}
