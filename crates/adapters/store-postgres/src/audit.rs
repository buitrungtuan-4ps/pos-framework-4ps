// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The console audit trail over PostgreSQL ([ADR-0069](../../../docs/adr/0069-audit-trail.md), Track G2).
//!
//! One append-only row per console mutation. This adapter keeps only the SQL and returns plain rows;
//! `pos-cloud` implements its `AuditStore` seam over this type. Append-only is enforced at the grant
//! (the migration gives the query role `SELECT`/`INSERT` only), so there is deliberately no update or
//! delete method here. `before`/`after` are bound and read as text around a `jsonb` column (the
//! `$N::text::jsonb` cast on the way in, `::text` on the way out), exactly as `order_queue` handles
//! its payloads — no dependency on a serde-json column feature.
//!
//! Tenant scoping is the explicit `WHERE tenant_id = $1` filter every cloud adapter carries (the
//! server connects as the trusted pool owner, which bypasses RLS; the table's policy is the
//! belt-and-suspenders second line). A tenant-global action (a tenant create, admin management)
//! carries `tenant_id = NULL`, visible only to the trusted connection.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One audit-log row as stored: the acting admin snapshot, the action, the affected entity, and the
/// before/after of the change (each still as JSON text around the `jsonb` column).
#[derive(Clone, Debug)]
pub struct AuditLogRow {
    /// The row's ULID id (minted at the edge).
    pub id: String,
    /// The tenant the action belongs to, or `None` for a tenant-global action.
    pub tenant_id: Option<String>,
    /// The acting admin's id, snapshotted at action time.
    pub actor_admin_id: String,
    /// The acting admin's email, snapshotted at action time.
    pub actor_email: String,
    /// The acting admin's role token, snapshotted at action time.
    pub actor_role: String,
    /// The action, `resource.verb` (e.g. `store.update`).
    pub action: String,
    /// The affected entity's type (e.g. `store`).
    pub entity_type: String,
    /// The affected entity's id.
    pub entity_id: String,
    /// The prior value as a JSON document, or `None` for a create.
    pub before_json: Option<String>,
    /// The new value as a JSON document, or `None` for a delete.
    pub after_json: Option<String>,
    /// A correlation id, or `None`.
    pub request_id: Option<String>,
    /// Unix ms of the action.
    pub at_ms: i64,
}

/// The columns every read returns, in a stable order matching [`audit_row`].
const AUDIT_COLUMNS: &str = "id, tenant_id, actor_admin_id, actor_email, actor_role, action, \
     entity_type, entity_id, before::text, after::text, request_id, at";

/// The console audit trail over a shared pool. Built by [`PostgresStore::audit`](crate::PostgresStore::audit).
#[derive(Clone, Debug)]
pub struct PostgresAudit {
    pool: Pool,
}

impl PostgresAudit {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Appends one audit row. The `before`/`after` params are bound as text and cast
    /// `$9::text::jsonb` / `$10::text::jsonb`, the same reason `order_queue` casts its payloads;
    /// `NULL` binds through the cast as a `NULL` jsonb. `at_ms` is Unix milliseconds.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    #[expect(
        clippy::too_many_arguments,
        reason = "the audit row is a flat record of eleven independent columns; a parameter struct \
                  here would only re-list them one call-site away"
    )]
    pub async fn insert(
        &self,
        id: &str,
        tenant_id: Option<&str>,
        actor_admin_id: &str,
        actor_email: &str,
        actor_role: &str,
        action: &str,
        entity_type: &str,
        entity_id: &str,
        before_json: Option<&str>,
        after_json: Option<&str>,
        request_id: Option<&str>,
        at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO audit_log \
                 (id, tenant_id, actor_admin_id, actor_email, actor_role, action, entity_type, \
                  entity_id, before, after, request_id, at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::text::jsonb, $10::text::jsonb, $11, $12)",
                &[
                    &id,
                    &tenant_id,
                    &actor_admin_id,
                    &actor_email,
                    &actor_role,
                    &action,
                    &entity_type,
                    &entity_id,
                    &before_json,
                    &after_json,
                    &request_id,
                    &at_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists audit rows newest-first, up to `limit`. When `tenant_id` is `Some`, only that tenant's
    /// rows; when `None`, every row (the trusted connection's fleet-wide read, including the
    /// tenant-global `NULL` rows).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(
        &self,
        tenant_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AuditLogRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = match tenant_id {
            Some(tenant) => {
                connection
                    .query(
                        &format!(
                            "SELECT {AUDIT_COLUMNS} FROM audit_log WHERE tenant_id = $1 \
                             ORDER BY at DESC, id DESC LIMIT $2"
                        ),
                        &[&tenant, &limit],
                    )
                    .await
            }
            None => connection
                .query(
                    &format!(
                        "SELECT {AUDIT_COLUMNS} FROM audit_log ORDER BY at DESC, id DESC LIMIT $1"
                    ),
                    &[&limit],
                )
                .await,
        }
        .map_err(unavailable)?;
        Ok(rows.iter().map(audit_row).collect())
    }

    /// Reads audit rows newest-first matching every non-`None` filter, up to `limit`. Each filter is
    /// bound unconditionally and applied as `($n IS NULL OR column = $n)`, so a `None` matches every
    /// row and the filters run in SQL *before* the `LIMIT` — a narrow filter still reaches older
    /// matches. `tenant_id = $1` (when `Some`) excludes the tenant-global `NULL` rows, exactly as
    /// [`fetch`](Self::fetch) does; `None` reads across every tenant including the global rows.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    #[expect(
        clippy::too_many_arguments,
        reason = "each parameter is one independent, optional filter column; a struct here would only \
                  re-list the same columns one call-site away, as `insert` already notes"
    )]
    pub async fn search(
        &self,
        tenant_id: Option<&str>,
        entity_type: Option<&str>,
        entity_id: Option<&str>,
        action: Option<&str>,
        actor_admin_id: Option<&str>,
        since_ms: Option<i64>,
        until_ms: Option<i64>,
        limit: i64,
    ) -> Result<Vec<AuditLogRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {AUDIT_COLUMNS} FROM audit_log \
                     WHERE ($1::text IS NULL OR tenant_id = $1) \
                       AND ($2::text IS NULL OR entity_type = $2) \
                       AND ($3::text IS NULL OR entity_id = $3) \
                       AND ($4::text IS NULL OR action = $4) \
                       AND ($5::text IS NULL OR actor_admin_id = $5) \
                       AND ($6::bigint IS NULL OR at >= $6) \
                       AND ($7::bigint IS NULL OR at <= $7) \
                     ORDER BY at DESC, id DESC LIMIT $8"
                ),
                &[
                    &tenant_id,
                    &entity_type,
                    &entity_id,
                    &action,
                    &actor_admin_id,
                    &since_ms,
                    &until_ms,
                    &limit,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(audit_row).collect())
    }
}

/// Reads one queried row into an [`AuditLogRow`]. The column order matches [`AUDIT_COLUMNS`].
fn audit_row(row: &tokio_postgres::Row) -> AuditLogRow {
    AuditLogRow {
        id: row.get(0),
        tenant_id: row.get(1),
        actor_admin_id: row.get(2),
        actor_email: row.get(3),
        actor_role: row.get(4),
        action: row.get(5),
        entity_type: row.get(6),
        entity_id: row.get(7),
        before_json: row.get(8),
        after_json: row.get(9),
        request_id: row.get(10),
        at_ms: row.get(11),
    }
}
