// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The fleet read model over PostgreSQL ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 3).
//!
//! The console's one-glance answer to "is the fleet there, and is it in sync?" is a join, not a
//! table: a store's identity (`stores`), its liveness (`store_liveness`), its lease
//! (`store_lease`), its config drift
//! (`config_trees`), and its relay backlog (`order_queue`) each live in their own table, written by
//! their own path. This adapter reads them together, per tenant, and hands back a flat row; `pos-cloud`
//! implements its `FleetStore` seam over this type and derives online/offline at read time. Nothing is
//! written here — it is purely a read across four existing tables.
//!
//! Tenant scoping is the explicit `WHERE s.tenant_id = $1` filter every cloud adapter carries (the
//! server connects as the trusted pool owner, which bypasses RLS; the tables' policies are the
//! belt-and-suspenders second line). The relay backlog and published version are computed in SQL so
//! the console never pulls a store's whole config tree or order queue to answer a one-line summary.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One store's fleet row: identity, liveness, config drift, and relay backlog, joined across the four
/// tables. Every liveness/backlog field is optional because a store may be registered but never yet
/// seen, un-configured, or have an empty queue.
#[derive(Clone, Debug)]
pub struct FleetStoreRow {
    /// The store id (a ULID string).
    pub store_id: String,
    /// The human name.
    pub name: String,
    /// `active` or `archived` (the registry status).
    pub status: String,
    /// Unix ms of the store's most recent contact (a config pull or a heartbeat), or `None` if it has
    /// never checked in.
    pub last_seen_at_ms: Option<i64>,
    /// Unix ms of the store's most recent *config pull* specifically, or `None`.
    pub last_config_pull_at_ms: Option<i64>,
    /// The config version the store reported holding on its last pull (a ULID string), or `None`.
    pub config_version_held: Option<String>,
    /// The store's currently-published config version (a ULID string), or `None` if nothing has been
    /// published to it.
    pub config_version_published: Option<String>,
    /// How many orders are queued and not yet reported by the store's POS.
    pub relay_backlog: i64,
    /// Unix ms the oldest still-pending queued order arrived, or `None` if the queue is empty.
    pub oldest_pending_at_ms: Option<i64>,
    /// The binary version the store last reported running (ADR-0078), or `None` if it has never
    /// reported.
    pub installed_version: Option<String>,
    /// Whether the store's last post-install self-test passed, or `None`.
    pub self_test_ok: Option<bool>,
    /// Unix ms of the store's most recent OTA report, or `None`.
    pub reported_at_ms: Option<i64>,
    /// How many events the store reported having committed and not yet published on its last
    /// heartbeat, or `None` if it has never said. Its *own* backlog — the opposite direction from
    /// `relay_backlog`, which counts orders queued *for* it.
    pub outbox_depth: Option<i64>,
    /// Unix ms of the heartbeat that reported `outbox_depth`, or `None`. Carried so a stale depth
    /// reads as stale rather than as current.
    pub outbox_reported_at_ms: Option<i64>,
    /// The lease generation the box last reported holding (ADR-0108), or `None` if it has never
    /// said. Read beside `lease_generation_authoritative`: the pair is what makes a **split**
    /// legible — a box that has been replaced looks nothing like one that is merely quiet, but only
    /// if both numbers are in front of the operator.
    pub lease_generation_held: Option<i64>,
    /// Unix ms of the heartbeat that reported it, or `None`.
    pub lease_reported_at_ms: Option<i64>,
    /// The store's authoritative lease generation (ADR-0108), or `None` if the cloud has never
    /// issued this store one — which is every store until an operator does, and reads as "no lease
    /// in force" rather than as generation `0`.
    pub lease_generation_authoritative: Option<i64>,
}

/// The columns and joins shared by the list and the single-store read. `$1` is always the tenant.
/// The published version is the id of the last element of the config tree's published history
/// (`ConfigTreeState.history`, oldest first), extracted in SQL so the whole tree never crosses the
/// wire. The oldest-pending instant is cast to `bigint` (Unix ms) rather than returned as a timestamp,
/// so the row carries the same millisecond shape `store_liveness` does.
const FLEET_SELECT: &str = "SELECT \
     s.store_id, \
     s.name, \
     s.status, \
     l.last_seen_at, \
     l.last_config_pull_at, \
     l.config_version_held, \
     ct.state -> 'history' -> -1 ->> 'id' AS config_version_published, \
     COALESCE(b.pending_count, 0) AS relay_backlog, \
     b.oldest_pending_ms, \
     l.installed_version, \
     l.self_test_ok, \
     l.reported_at, \
     l.outbox_depth, \
     l.outbox_reported_at, \
     l.lease_generation, \
     l.lease_reported_at, \
     lease.generation AS lease_generation_authoritative \
     FROM stores s \
     LEFT JOIN store_liveness l ON l.tenant_id = s.tenant_id AND l.store_id = s.store_id \
     LEFT JOIN store_lease lease \
         ON lease.tenant_id = s.tenant_id AND lease.store_id = s.store_id \
     LEFT JOIN config_trees ct ON ct.tenant_id = s.tenant_id AND ct.store_id = s.store_id \
     LEFT JOIN ( \
         SELECT store_id, \
                count(*) AS pending_count, \
                (extract(epoch FROM min(created_at)) * 1000)::bigint AS oldest_pending_ms \
         FROM order_queue \
         WHERE tenant_id = $1 AND status = 'pending' \
         GROUP BY store_id \
     ) b ON b.store_id = s.store_id";

/// The fleet read model over a shared pool. Built by [`PostgresStore::fleet`](crate::PostgresStore::fleet).
#[derive(Clone, Debug)]
pub struct PostgresFleet {
    pool: Pool,
}

impl PostgresFleet {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Reads every store's fleet row for a tenant, newest store first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list(&self, tenant_id: &str) -> Result<Vec<FleetStoreRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let sql = format!("{FLEET_SELECT} WHERE s.tenant_id = $1 ORDER BY s.created_at DESC");
        let rows = connection
            .query(&sql, &[&tenant_id])
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(fleet_row).collect())
    }

    /// Reads one store's fleet row within its tenant, or `None` if the tenant has no such store.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_one(
        &self,
        tenant_id: &str,
        store_id: &str,
    ) -> Result<Option<FleetStoreRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let sql = format!("{FLEET_SELECT} WHERE s.tenant_id = $1 AND s.store_id = $2");
        let row = connection
            .query_opt(&sql, &[&tenant_id, &store_id])
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(fleet_row))
    }
}

/// Reads one queried row into a [`FleetStoreRow`]. The column order matches [`FLEET_SELECT`].
fn fleet_row(row: &tokio_postgres::Row) -> FleetStoreRow {
    FleetStoreRow {
        store_id: row.get(0),
        name: row.get(1),
        status: row.get(2),
        last_seen_at_ms: row.get(3),
        last_config_pull_at_ms: row.get(4),
        config_version_held: row.get(5),
        config_version_published: row.get(6),
        relay_backlog: row.get(7),
        oldest_pending_at_ms: row.get(8),
        installed_version: row.get(9),
        self_test_ok: row.get(10),
        reported_at_ms: row.get(11),
        outbox_depth: row.get(12),
        outbox_reported_at_ms: row.get(13),
        lease_generation_held: row.get(14),
        lease_reported_at_ms: row.get(15),
        lease_generation_authoritative: row.get(16),
    }
}
