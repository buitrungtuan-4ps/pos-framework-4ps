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
//!
//! The permission snapshot (`docs/snapshots/permissions.txt`) follows the same shape: a
//! bare permission id is the contract — a role synced from an older cloud may still
//! reference it, so it may be added but never removed (deprecate instead) — while the
//! tabbed metadata lines (`group=`, `risk=`, `pin_required=`, `default_role=`) are
//! mutable, because re-grouping a permission or adjusting a default role is a deliberate
//! change to a seed, not a broken contract.
//!
//! The capability snapshot (`docs/snapshots/capabilities.txt`) is the same again: a bare
//! flag key is a term in the configuration document a synced edge reads, so it may not
//! disappear, while its tabbed `default=` may change (a default change owes an upgrade
//! note, but is allowed).
//!
//! The route snapshot (`docs/snapshots/routes.txt`) has no mutable half at all: every
//! line is `METHOD /path`, and every line is a contract
//! ([ADR-0111](../../../docs/adr/0111-a-second-origin-may-address-the-edge.md)). A till
//! that has not updated still calls the route it was built against, and the edge's asset
//! fallback answers an unmatched path with `200 text/html` rather than `404` — so a rename
//! reaches the operator as a JSON parse error naming neither the route nor the release
//! that moved it. The `pos-edge-version` header that record introduces compares one-sidedly
//! on the strength of this file; a route is deprecated in place instead.

use std::collections::BTreeSet;

use super::{Error, base_ref, read_at_ref, repo_root};
use crate::Finding;

/// Snapshot files under this check's protection.
const SNAPSHOTS: &[&str] = &[
    "docs/snapshots/events.txt",
    "docs/snapshots/permissions.txt",
    "docs/snapshots/capabilities.txt",
    "docs/snapshots/routes.txt",
];

/// Tab-prefixed metadata keys that may change or disappear without breaking a contract.
const MUTABLE_KEYS: &[&str] = &[
    "\tschema_version=",
    "\tgroup=",
    "\trisk=",
    "\tpin_required=",
    "\tdefault_role=",
    "\tdefault=",
];

/// Lines whose disappearance is a deliberate change rather than a broken contract.
fn is_mutable(line: &str) -> bool {
    line.starts_with('#') || MUTABLE_KEYS.iter().any(|key| line.contains(key))
}

/// Why the removed line mattered, in the terms of the file it was removed from.
///
/// One hint for every snapshot would have to be vague enough to cover all of them, and a vague
/// hint is the kind a reader skips. The remedy is the same in each case — deprecate in place —
/// but *who breaks* differs, and naming them is what makes the remedy land.
fn hint_for(path: &str) -> &'static str {
    if path.ends_with("routes.txt") {
        "a published edge route is a contract: deprecate it in place, but do not rename or \
         remove it — a till that has not updated still calls it, and the asset fallback answers \
         an unmatched path with 200 text/html, so the operator sees a parse error naming neither \
         the route nor the release that moved it (ADR-0111)"
    } else {
        "a published event type or payload field is a contract: add to it, \
         or deprecate it for at least two releases, but do not remove it — \
         an edge that has been offline for a week still expects it"
    }
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
                .with_hint(hint_for(path)),
            );
        }
    }
    Ok(findings)
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
    fn a_removed_route_is_explained_as_a_route() {
        // The hint is the whole message a contributor acts on. Handing them the event-catalogue
        // sentence for a renamed route sends them looking for a payload field that does not exist.
        assert!(super::hint_for("docs/snapshots/routes.txt").contains("edge route"));
        assert!(super::hint_for("docs/snapshots/events.txt").contains("event type"));
    }

    #[test]
    fn comments_are_ignored() {
        assert!(is_mutable("# Event catalogue snapshot. Generated."));
    }

    #[test]
    fn a_bare_permission_id_is_a_contract() {
        // No tab: the id itself, which a role synced from an older cloud may still name.
        assert!(!is_mutable("billing.bill.void"));
    }

    #[test]
    fn permission_metadata_may_change() {
        // Re-grouping a permission or adjusting a default role is a deliberate change to a
        // seed, not a broken contract, so these do not need the removal gate.
        assert!(is_mutable("billing.bill.void\tgroup=BILLING"));
        assert!(is_mutable("billing.bill.void\trisk=HIGH"));
        assert!(is_mutable("billing.bill.void\tpin_required=true"));
        assert!(is_mutable("billing.bill.void\tdefault_role=OWNER"));
    }

    #[test]
    fn a_bare_capability_key_is_a_contract() {
        assert!(!is_mutable("tables_enabled"));
    }

    #[test]
    fn a_capability_default_may_change() {
        assert!(is_mutable("tables_enabled\tdefault=true"));
    }
}
