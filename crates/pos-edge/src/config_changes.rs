// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which configuration nodes a newly applied document changed
//! ([ADR-0173](../../../docs/adr/0173-the-till-declares-each-piece-of-store-data-once.md)
//! decision 2).
//!
//! The edge names them on `config_applied`, so a till re-reads only what derives from them. Where
//! it cannot say, the frame names nothing, and a till re-reads everything it draws from
//! configuration, as it did before the field existed.

use std::sync::Mutex;

/// The top-level nodes of `after` whose content differs from `before`, sorted: a node added,
/// removed or changed. `None` when either document is not an object, because then nothing can be
/// said node by node.
#[must_use]
pub fn changed_nodes(before: &serde_json::Value, after: &serde_json::Value) -> Option<Vec<String>> {
    let (before, after) = (before.as_object()?, after.as_object()?);
    let mut changed: Vec<String> = before
        .keys()
        .chain(after.keys())
        .filter(|node| before.get(*node) != after.get(*node))
        .cloned()
        .collect();
    changed.sort_unstable();
    changed.dedup();
    Some(changed)
}

/// The document the config client last applied: one, replaced on every apply, `None` until the
/// first, which is why the first apply after a start names no nodes.
#[derive(Debug, Default)]
pub(crate) struct AppliedDocument(Mutex<Option<serde_json::Value>>);

impl AppliedDocument {
    /// Holds `next` as the applied document, and returns the nodes it changed from the one before.
    ///
    /// # Panics
    ///
    /// If the lock is poisoned — unreachable, as its critical section only swaps a value.
    pub(crate) fn replace(&self, next: &serde_json::Value) -> Option<Vec<String>> {
        let mut held = self.0.lock().expect("applied-document lock");
        let changed = held.as_ref().and_then(|before| changed_nodes(before, next));
        *held = Some(next.clone());
        changed
    }
}
