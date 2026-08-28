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

use pos_proto::Timestamp;
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

/// One reconciliation run, for the console history read model
/// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)). Counts and a timestamp only — a run
/// is device/store telemetry, never event contents or a customer identifier.
#[derive(Debug, Clone)]
pub struct ReconcileRun {
    /// The run's ULID string (chronological when ordered).
    pub run_id: String,
    /// The store the diff was for.
    pub store: StoreId,
    /// How many ids the edge offered in its manifest (the window it reconciled).
    pub candidates_offered: u32,
    /// How many of them the cloud was missing — the ids it asked the edge to re-push.
    pub missing_found: u32,
    /// When the diff ran, stamped from the server clock.
    pub ran_at: Timestamp,
}

/// Records and lists reconciliation runs — the history behind `POST /internal/reconcile` (which
/// records one run per diff) and `GET /admin/reconcile` (which lists them). Kept beside
/// [`ReconcileStore`] because the same store answers both.
pub trait ReconcileRunStore {
    /// Appends one run to `tenant`'s history. Best-effort at the call site: a failure to record must
    /// not fail the diff the edge is waiting on.
    ///
    /// # Errors
    ///
    /// [`ReconcileError`] if the store could not be written.
    fn record_run(
        &self,
        tenant: TenantId,
        run: &ReconcileRun,
    ) -> impl Future<Output = Result<(), ReconcileError>> + Send;

    /// Lists `tenant`'s most recent runs, newest first, capped at `limit`. An optional `store` narrows
    /// to one store; `None` reads across the tenant's stores.
    ///
    /// # Errors
    ///
    /// [`ReconcileError`] if the store could not be read.
    fn list_runs(
        &self,
        tenant: TenantId,
        store: Option<StoreId>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<ReconcileRun>, ReconcileError>> + Send;
}
