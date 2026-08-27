// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Operational alerts over PostgreSQL ([ADR-0073](../../../docs/adr/0073-alerting.md), Track O2).
//!
//! One row per alert with an open→resolved lifecycle (`alerts`, migration 0027). This adapter keeps
//! only the SQL and returns plain rows; `pos-cloud` implements its `AlertStore` seam over this type.
//! Written and read by the trusted pool-owner connection (across tenants, and the server-wide
//! NULL-tenant rows), so it never sets `app.tenant_id`. `detail` is bound and read as text around the
//! `jsonb` column (the `$7::text::jsonb` cast in, `detail::text` out), exactly as `task_health` does.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// An alert as stored: identity, scope, the condition, its severity/summary/detail, and the lifecycle
/// timestamps (all Unix ms). `detail_json` is the jsonb column as text.
#[derive(Clone, Debug)]
pub struct AlertRow {
    /// The alert's ULID.
    pub id: String,
    /// The owning tenant, or `None` for a server-wide alert.
    pub tenant_id: Option<String>,
    /// The condition kind (`AlertKind::as_str`).
    pub kind: String,
    /// The scope within the kind (store id, endpoint id, or empty).
    pub dedup_key: String,
    /// The severity (`AlertSeverity::as_str`).
    pub severity: String,
    /// The one-line human summary.
    pub summary: String,
    /// The numbers behind the alert, as a JSON document (text).
    pub detail_json: String,
    /// Unix ms the alert was opened.
    pub first_seen_at_ms: i64,
    /// Unix ms the condition was last observed firing.
    pub last_seen_at_ms: i64,
    /// Unix ms the condition cleared, or `None` while active.
    pub resolved_at_ms: Option<i64>,
    /// Unix ms an operator acknowledged it, or `None`.
    pub acknowledged_at_ms: Option<i64>,
}

/// The alert store over a shared pool. Built by [`PostgresStore::alerts`](crate::PostgresStore::alerts).
#[derive(Clone, Debug)]
pub struct PostgresAlerts {
    pool: Pool,
}

/// The columns every read selects, in `AlertRow` field order (`detail` as text).
const COLUMNS: &str = "id, tenant_id, kind, dedup_key, severity, summary, detail::text, \
                       first_seen_at, last_seen_at, resolved_at, acknowledged_at";

impl PostgresAlerts {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Opens a new alert, or refreshes the existing open one for the same `(tenant, kind, dedup_key)`.
    ///
    /// The partial unique index `alerts_open_key` is the conflict target: while an alert is unresolved
    /// a second open of the same condition updates its severity/summary/detail and advances
    /// `last_seen_at`, keeping the original id and `first_seen_at`. A resolved alert of the same key is
    /// history and does not conflict, so the next firing opens a fresh alert.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    #[expect(
        clippy::too_many_arguments,
        reason = "one bind per alert column; a struct param would just be unpacked here"
    )]
    pub async fn upsert(
        &self,
        id: &str,
        tenant_id: Option<&str>,
        kind: &str,
        dedup_key: &str,
        severity: &str,
        summary: &str,
        detail_json: &str,
        first_seen_at_ms: i64,
        last_seen_at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO alerts \
                 (id, tenant_id, kind, dedup_key, severity, summary, detail, \
                  first_seen_at, last_seen_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7::text::jsonb, $8, $9) \
                 ON CONFLICT (coalesce(tenant_id, ''), kind, dedup_key) WHERE resolved_at IS NULL \
                 DO UPDATE SET \
                 severity = EXCLUDED.severity, \
                 summary = EXCLUDED.summary, \
                 detail = EXCLUDED.detail, \
                 last_seen_at = EXCLUDED.last_seen_at",
                &[
                    &id,
                    &tenant_id,
                    &kind,
                    &dedup_key,
                    &severity,
                    &summary,
                    &detail_json,
                    &first_seen_at_ms,
                    &last_seen_at_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Resolves an open alert (idempotent — a resolved alert stays resolved).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn resolve(&self, id: &str, resolved_at_ms: i64) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "UPDATE alerts SET resolved_at = $2 WHERE id = $1 AND resolved_at IS NULL",
                &[&id, &resolved_at_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Marks an alert acknowledged.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn acknowledge(&self, id: &str, acknowledged_at_ms: i64) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "UPDATE alerts SET acknowledged_at = $2 WHERE id = $1",
                &[&id, &acknowledged_at_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Every active (unresolved) alert, most-recently-seen first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_active(&self) -> Result<Vec<AlertRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {COLUMNS} FROM alerts WHERE resolved_at IS NULL \
                     ORDER BY last_seen_at DESC"
                ),
                &[],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(row_to_alert).collect())
    }

    /// The most recent alerts (active and resolved), newest-seen first, capped at `limit`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_recent(&self, limit: i64) -> Result<Vec<AlertRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!("SELECT {COLUMNS} FROM alerts ORDER BY last_seen_at DESC LIMIT $1"),
                &[&limit],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(row_to_alert).collect())
    }
}

fn row_to_alert(row: &tokio_postgres::Row) -> AlertRow {
    AlertRow {
        id: row.get(0),
        tenant_id: row.get(1),
        kind: row.get(2),
        dedup_key: row.get(3),
        severity: row.get(4),
        summary: row.get(5),
        detail_json: row.get(6),
        first_seen_at_ms: row.get(7),
        last_seen_at_ms: row.get(8),
        resolved_at_ms: row.get(9),
        acknowledged_at_ms: row.get(10),
    }
}
