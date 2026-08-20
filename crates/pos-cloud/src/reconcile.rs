// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The reconciliation seam ([ADR-0040](../../../docs/adr/0040-reconciliation.md)).
//!
//! Reconciliation is a diff: an edge reports the event ids it holds for a store, and the cloud
//! answers with the subset its own event log lacks — the ids to re-push through `/internal/ingest`.
//! The cloud cannot compute this alone, because ULIDs are not a dense sequence and a gap is invisible
//! from the cloud's rows; only the edge knows the true set. This seam is the cloud's half — the
//! set-membership query — behind which `store-postgres` answers `event_id = ANY(candidates)` and a
//! fake answers from a set.

use core::future::Future;

use pos_proto::ids::{EventId, StoreId, TenantId};

/// A failure of the reconciliation store itself — the database is unreachable.
#[derive(Debug, thiserror::Error)]
#[error("the reconciliation store failed: {0}")]
pub struct ReconcileError(String);

impl ReconcileError {
    /// A store failure carrying a human-readable reason (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Answers "which of these ids am I missing?" for one store.
pub trait ReconcileStore {
    /// Of the `candidates` an edge reports holding for `(tenant, store)`, returns exactly those the
    /// cloud's event log does **not** contain — the ids the edge should re-push. The order and
    /// duplicates of `candidates` need not be preserved; membership is what matters.
    ///
    /// # Errors
    ///
    /// [`ReconcileError`] if the store could not be read.
    fn absent_event_ids(
        &self,
        tenant: TenantId,
        store: StoreId,
        candidates: &[EventId],
    ) -> impl Future<Output = Result<Vec<EventId>, ReconcileError>> + Send;
}
