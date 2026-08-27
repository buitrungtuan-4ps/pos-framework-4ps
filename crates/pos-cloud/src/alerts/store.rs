// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The alert store seam ([ADR-0073](../../../docs/adr/0073-alerting.md), Track O2).
//!
//! One durable row per alert with an open→resolved lifecycle. The evaluator loop [`upsert`]s an alert
//! while its condition fires and [`resolve`]s it when the condition clears; the console reads the
//! active and recent lists and [`acknowledge`]s one. A trait so it runs against an in-memory fake in
//! tests and a `store-postgres` table in the cloud (the impl lives in [`crate::persistence`], the SQL
//! in `store-postgres`).
//!
//! [`upsert`]: AlertStore::upsert
//! [`resolve`]: AlertStore::resolve
//! [`acknowledge`]: AlertStore::acknowledge

use core::future::Future;

use pos_proto::ids::TenantId;
use pos_proto::time::Timestamp;

use super::model::{AlertKind, AlertSeverity};

/// A stored alert: its identity and scope, the condition, its severity/summary/detail, and the
/// lifecycle instants. `first_seen_at` is when it opened, `last_seen_at` the most recent tick it was
/// still firing, `resolved_at` when the condition cleared (or `None` while active), `acknowledged_at`
/// when an operator acknowledged it.
#[derive(Debug, Clone)]
pub struct AlertRecord {
    /// The alert's ULID (minted when opened).
    pub id: String,
    /// The owning tenant, or `None` for a server-wide alert.
    pub tenant_id: Option<TenantId>,
    /// The condition kind.
    pub kind: AlertKind,
    /// The scope within the kind (store id, endpoint id, or empty for a singleton).
    pub dedup_key: String,
    /// The alert's severity.
    pub severity: AlertSeverity,
    /// The one-line human summary.
    pub summary: String,
    /// The numbers behind the alert, as a small JSON object.
    pub detail: serde_json::Value,
    /// When the alert was opened.
    pub first_seen_at: Timestamp,
    /// When the condition was last observed still firing.
    pub last_seen_at: Timestamp,
    /// When the condition cleared, or `None` while active.
    pub resolved_at: Option<Timestamp>,
    /// When an operator acknowledged it, or `None`.
    pub acknowledged_at: Option<Timestamp>,
}

impl AlertRecord {
    /// The dedup identity of the alert — the tuple the one *open* alert of a condition is keyed by.
    #[must_use]
    pub fn key(&self) -> (Option<TenantId>, AlertKind, String) {
        (self.tenant_id, self.kind, self.dedup_key.clone())
    }
}

/// Persists operational alerts with an open→resolved lifecycle.
pub trait AlertStore {
    /// Opens `record` as a new alert, or refreshes the existing open one for the same
    /// `(tenant_id, kind, dedup_key)` — advancing `last_seen_at` and updating severity/summary/detail
    /// while keeping the original id and `first_seen_at`.
    ///
    /// # Errors
    ///
    /// [`AlertStoreError`] if the row could not be written.
    fn upsert(
        &self,
        record: &AlertRecord,
    ) -> impl Future<Output = Result<(), AlertStoreError>> + Send;

    /// Resolves an open alert (idempotent).
    ///
    /// # Errors
    ///
    /// [`AlertStoreError`] if the row could not be written.
    fn resolve(
        &self,
        id: &str,
        resolved_at: Timestamp,
    ) -> impl Future<Output = Result<(), AlertStoreError>> + Send;

    /// Marks an alert acknowledged.
    ///
    /// # Errors
    ///
    /// [`AlertStoreError`] if the row could not be written.
    fn acknowledge(
        &self,
        id: &str,
        acknowledged_at: Timestamp,
    ) -> impl Future<Output = Result<(), AlertStoreError>> + Send;

    /// Every active (unresolved) alert, most-recently-seen first.
    ///
    /// # Errors
    ///
    /// [`AlertStoreError`] if the rows could not be read.
    fn list_active(&self)
    -> impl Future<Output = Result<Vec<AlertRecord>, AlertStoreError>> + Send;

    /// The most recent alerts (active and resolved), newest-seen first, capped at `limit`.
    ///
    /// # Errors
    ///
    /// [`AlertStoreError`] if the rows could not be read.
    fn list_recent(
        &self,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<AlertRecord>, AlertStoreError>> + Send;
}

/// A failure of the alert store itself — the database is unreachable, or a stored value could not be
/// decoded.
#[derive(Debug, thiserror::Error)]
#[error("the alert store failed: {0}")]
pub struct AlertStoreError(String);

impl AlertStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pos_proto::ids::TenantId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;
    use serde_json::json;

    use super::{AlertRecord, AlertStore, AlertStoreError};
    use crate::alerts::model::{AlertKind, AlertSeverity};

    /// An in-memory [`AlertStore`] mirroring the partial-unique-index lifecycle: one open row per
    /// `(tenant, kind, dedup_key)`, refreshed in place; a resolved row is history the next open
    /// steps past.
    #[derive(Default)]
    struct FakeAlerts {
        rows: Mutex<Vec<AlertRecord>>,
    }

    impl AlertStore for FakeAlerts {
        async fn upsert(&self, record: &AlertRecord) -> Result<(), AlertStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            if let Some(open) = rows
                .iter_mut()
                .find(|r| r.resolved_at.is_none() && r.key() == record.key())
            {
                open.severity = record.severity;
                open.summary.clone_from(&record.summary);
                open.detail = record.detail.clone();
                open.last_seen_at = record.last_seen_at;
            } else {
                rows.push(record.clone());
            }
            Ok(())
        }

        async fn resolve(&self, id: &str, resolved_at: Timestamp) -> Result<(), AlertStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            if let Some(row) = rows
                .iter_mut()
                .find(|r| r.id == id && r.resolved_at.is_none())
            {
                row.resolved_at = Some(resolved_at);
            }
            Ok(())
        }

        async fn acknowledge(
            &self,
            id: &str,
            acknowledged_at: Timestamp,
        ) -> Result<(), AlertStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            if let Some(row) = rows.iter_mut().find(|r| r.id == id) {
                row.acknowledged_at = Some(acknowledged_at);
            }
            Ok(())
        }

        async fn list_active(&self) -> Result<Vec<AlertRecord>, AlertStoreError> {
            let mut rows: Vec<AlertRecord> = self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|r| r.resolved_at.is_none())
                .cloned()
                .collect();
            rows.sort_by_key(|r| core::cmp::Reverse(r.last_seen_at.as_milliseconds_since_epoch()));
            Ok(rows)
        }

        async fn list_recent(&self, limit: u32) -> Result<Vec<AlertRecord>, AlertStoreError> {
            let mut rows = self.rows.lock().expect("lock").clone();
            rows.sort_by_key(|r| core::cmp::Reverse(r.last_seen_at.as_milliseconds_since_epoch()));
            rows.truncate(limit as usize);
            Ok(rows)
        }
    }

    fn ts(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).expect("valid timestamp")
    }

    fn record(id: &str, opened: i64) -> AlertRecord {
        AlertRecord {
            id: id.to_owned(),
            tenant_id: Some(TenantId::new(Ulid::from_u128(1))),
            kind: AlertKind::StoreOffline,
            dedup_key: "store-a".to_owned(),
            severity: AlertSeverity::Warning,
            summary: "offline".to_owned(),
            detail: json!({ "minutes_offline": 6 }),
            first_seen_at: ts(opened),
            last_seen_at: ts(opened),
            resolved_at: None,
            acknowledged_at: None,
        }
    }

    #[tokio::test]
    async fn upsert_opens_then_refreshes_and_resolve_clears_the_active_list() {
        let store = FakeAlerts::default();

        // First upsert opens the alert.
        store.upsert(&record("alert-1", 1_000)).await.expect("open");
        assert_eq!(store.list_active().await.expect("active").len(), 1);

        // A second upsert with the same key refreshes in place — no duplicate, first_seen kept,
        // last_seen and detail advanced.
        let mut again = record("alert-2", 5_000); // a different id, ignored on refresh
        again.detail = json!({ "minutes_offline": 12 });
        store.upsert(&again).await.expect("refresh");
        let active = store.list_active().await.expect("active");
        assert_eq!(active.len(), 1, "the same condition does not duplicate");
        assert_eq!(active[0].id, "alert-1", "the original id is kept");
        assert_eq!(active[0].first_seen_at, ts(1_000), "first_seen is kept");
        assert_eq!(active[0].last_seen_at, ts(5_000), "last_seen advanced");
        assert_eq!(active[0].detail, json!({ "minutes_offline": 12 }));

        // Resolving drops it from the active list but keeps it in the recent history.
        store.resolve("alert-1", ts(9_000)).await.expect("resolve");
        assert!(store.list_active().await.expect("active").is_empty());
        let recent = store.list_recent(10).await.expect("recent");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].resolved_at, Some(ts(9_000)));

        // After resolution the same condition opens a fresh alert (past the resolved one).
        store
            .upsert(&record("alert-3", 12_000))
            .await
            .expect("reopen");
        assert_eq!(store.list_active().await.expect("active").len(), 1);
        assert_eq!(store.list_recent(10).await.expect("recent").len(), 2);
    }

    #[tokio::test]
    async fn acknowledge_marks_an_alert_without_resolving_it() {
        let store = FakeAlerts::default();
        store.upsert(&record("alert-1", 1_000)).await.expect("open");
        store.acknowledge("alert-1", ts(2_000)).await.expect("ack");
        let active = store.list_active().await.expect("active");
        assert_eq!(active.len(), 1, "an acknowledged alert stays active");
        assert_eq!(active[0].acknowledged_at, Some(ts(2_000)));
    }
}
