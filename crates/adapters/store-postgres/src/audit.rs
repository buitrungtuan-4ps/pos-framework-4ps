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

use crate::store::{pool_unavailable, unavailable, window_total};

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

/// Which end of the trail a read walks from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AuditOrder {
    /// Most recent first — every read's order before the paged one could be asked for another.
    #[default]
    Newest,
    /// Earliest first.
    Oldest,
}

/// The `ORDER BY` for one order, as SQL. Always a literal, never assembled from caller input.
///
/// Both are *total*: each ends in `id`, the table's primary key, so a `LIMIT`/`OFFSET` window over
/// either cannot return a row on two pages or on neither
/// ([ADR-0098](../../../docs/adr/0098-paged-admin-reads.md) decision 9).
///
/// `Oldest` is the exact reverse of `Newest`, every column of it — which is why this order needed no
/// new migration. `audit_log_by_tenant_newest` (migration 0042) is `(tenant_id, at DESC, id DESC)`,
/// and a btree walks backwards as cheaply as forwards, so it covers both.
const fn audit_order(order: AuditOrder) -> &'static str {
    match order {
        AuditOrder::Newest => "ORDER BY at DESC, id DESC",
        AuditOrder::Oldest => "ORDER BY at ASC, id ASC",
    }
}

/// The order the unpaged reads impose, and the paged read's default.
///
/// Derived from [`audit_order`] rather than written out again: a page must be a window onto the same
/// sequence the unpaged reads return, and an `ORDER BY` that drifted on one of them would make
/// "page 2" name rows from an order nothing else uses.
const AUDIT_ORDER: &str = audit_order(AuditOrder::Newest);

/// The filter predicates the two filtered reads share, as `$1..$7`.
///
/// One string rather than two copies: [`search`](PostgresAudit::search) and
/// [`search_page`](PostgresAudit::search_page) must agree about *which rows match*, or a page would
/// be a window onto a different set than its own total counts. Each filter is bound unconditionally
/// and applied as `($n IS NULL OR column = $n)`, so a `None` matches every row and the filters run
/// in SQL *before* the bound — a narrow filter still reaches older matches.
const AUDIT_FILTERS: &str = "($1::text IS NULL OR tenant_id = $1) \
       AND ($2::text IS NULL OR entity_type = $2) \
       AND ($3::text IS NULL OR entity_id = $3) \
       AND ($4::text IS NULL OR action = $4) \
       AND ($5::text IS NULL OR actor_admin_id = $5) \
       AND ($6::bigint IS NULL OR at >= $6) \
       AND ($7::bigint IS NULL OR at <= $7)";

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
                             {AUDIT_ORDER} LIMIT $2"
                        ),
                        &[&tenant, &limit],
                    )
                    .await
            }
            None => {
                connection
                    .query(
                        &format!("SELECT {AUDIT_COLUMNS} FROM audit_log {AUDIT_ORDER} LIMIT $1"),
                        &[&limit],
                    )
                    .await
            }
        }
        .map_err(unavailable)?;
        Ok(rows.iter().map(audit_row).collect())
    }

    /// Reads audit rows newest-first matching every non-`None` filter, up to `limit` — see
    /// [`AUDIT_FILTERS`] for how a `None` matches everything. `tenant_id = $1` (when `Some`) excludes
    /// the tenant-global `NULL` rows, exactly as [`fetch`](Self::fetch) does; `None` reads across
    /// every tenant including the global rows.
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
                     WHERE {AUDIT_FILTERS} \
                     {AUDIT_ORDER} LIMIT $8"
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

    /// One page of the rows matching every non-`None` filter, in the order `order` asks for, with
    /// how many matched.
    ///
    /// The same predicates as [`search`](Self::search) — both read [`AUDIT_FILTERS`], so a filter
    /// cannot be tightened on one read and left on the other — and a total `ORDER BY` either way
    /// (see [`audit_order`]). `count(*) OVER()` rides on the windowed `SELECT`: one round trip, one
    /// snapshot, so the count cannot disagree with the page. An empty window carries no count at
    /// all, which [`window_total`] answers with a second query rather than a misleading zero. The
    /// order does not affect the count: the same rows match either way, so only *which* page they
    /// land on changes.
    ///
    /// The count is the expensive half. `LIMIT` can stop the index scan; `count(*) OVER()` cannot —
    /// it walks every matching row. `audit_log_by_tenant_newest` (migration 0042) makes that walk
    /// index-only for the console's tenant-scoped read, which is the case that matters, and covers
    /// both orders because a btree scans backwards as cheaply as forwards.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    #[expect(
        clippy::too_many_arguments,
        reason = "the same independent filter columns `search` takes, plus the order and the page's \
                  two bounds"
    )]
    pub async fn search_page(
        &self,
        tenant_id: Option<&str>,
        entity_type: Option<&str>,
        entity_id: Option<&str>,
        action: Option<&str>,
        actor_admin_id: Option<&str>,
        since_ms: Option<i64>,
        until_ms: Option<i64>,
        order: AuditOrder,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<AuditLogRow>, i64), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let order_by = audit_order(order);
        let rows = connection
            .query(
                &format!(
                    "SELECT {AUDIT_COLUMNS}, count(*) OVER() FROM audit_log \
                     WHERE {AUDIT_FILTERS} \
                     {order_by} LIMIT $8 OFFSET $9"
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
                    &offset,
                ],
            )
            .await
            .map_err(unavailable)?;
        // The fallback repeats every predicate from the same constant, so it counts what the
        // filters matched rather than the whole log.
        let total = window_total(
            &connection,
            &rows,
            12,
            &format!("SELECT count(*) FROM audit_log WHERE {AUDIT_FILTERS}"),
            &[
                &tenant_id,
                &entity_type,
                &entity_id,
                &action,
                &actor_admin_id,
                &since_ms,
                &until_ms,
            ],
        )
        .await?;
        Ok((rows.iter().map(audit_row).collect(), total))
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

#[cfg(test)]
mod tests {
    use super::{AUDIT_ORDER, AuditOrder, audit_order};

    /// Every order this adapter can produce is *total*, because each ends in the primary key.
    ///
    /// This test exists because nothing else in the tree can see it. The integration suite's
    /// `EXPLAIN` guards assert the plan of a query *they* write, so they catch migration 0042 being
    /// dropped — but if this adapter's own `ORDER BY` lost its tiebreaker they would keep passing,
    /// and so would the page-partition tests: with the index present, the index walk supplies the
    /// missing order for free. The same blind spot was found by mutation on the catalog fragments.
    ///
    /// Asserting the property rather than the two literals means an order added later is covered
    /// the day it is written.
    #[test]
    fn both_orders_end_in_the_primary_key_so_a_window_over_either_is_unambiguous() {
        for order in [AuditOrder::Newest, AuditOrder::Oldest] {
            let fragment = audit_order(order);
            assert!(
                fragment.ends_with("id ASC") || fragment.ends_with("id DESC"),
                "{order:?} must break ties on the primary key, or LIMIT/OFFSET over it can repeat \
                 or skip a row; got {fragment:?}",
            );
        }
    }

    /// The two orders are exact reverses, every column of them.
    ///
    /// This is the property that let `?order=oldest` ship without a migration: reading
    /// `(tenant_id, at DESC, id DESC)` backwards is the whole of the oldest-first order, so
    /// migration 0042's index covers both. An `at ASC, id DESC` — a plausible slip — would still be
    /// total, would still look right on one page, and would no longer be that index read backwards.
    #[test]
    fn the_oldest_order_is_the_newest_order_reversed_in_every_column() {
        let newest = audit_order(AuditOrder::Newest);
        let oldest = audit_order(AuditOrder::Oldest);
        assert_eq!(
            newest.matches("DESC").count(),
            oldest.matches("ASC").count(),
            "every column that descends newest-first ascends oldest-first",
        );
        assert_eq!(
            newest.matches("ASC").count(),
            oldest.matches("DESC").count(),
            "and the other way around: {newest:?} against {oldest:?}",
        );
        assert_eq!(
            newest.replace("DESC", "ASC"),
            oldest,
            "the two name the same columns in the same sequence",
        );
    }

    /// The unpaged reads' order is the paged read's default, and they are one string.
    ///
    /// A page must be a window onto the same sequence the unpaged reads return. If these drifted,
    /// "page 2" would name rows from an order nothing else uses.
    #[test]
    fn the_unpaged_reads_use_the_paged_default_order() {
        assert_eq!(AUDIT_ORDER, audit_order(AuditOrder::Newest));
    }
}
