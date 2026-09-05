// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The order-queue table over PostgreSQL (P7).
//!
//! The cloud's durable inbox of orders bound for a store's POS. Unlike a webhook — a cursor over the
//! event log that holds no backlog — a row here *is* a backlog item: a sales channel offers an order,
//! it is queued idempotently by `(tenant, store, sales_channel, external_reference)`, the store's
//! device pulls the pending orders, and the device reports each one's outcome back. This adapter keeps
//! only the SQL and returns plain types; `pos-cloud` implements its `OrderQueueStore` seam over this
//! type, converting rows to and from its own order and outcome shapes.
//!
//! The store→tenant directory ([`PostgresStoreDirectory`]) rides in the same module: a single read
//! that resolves which tenant a store belongs to, run as the trusted role across tenants — the same
//! posture as the webhook delivery task's fleet-wide load.

use deadpool_postgres::Pool;

use pos_ports::{PortError, PortName};

use crate::store::{pool_unavailable, unavailable};

/// A queued order's current standing — its handle, whether it has been reported, and the outcome once
/// it has. Returned by [`PostgresOrderQueue::enqueue`] and [`PostgresOrderQueue::outcome`].
#[derive(Clone, Debug)]
pub struct OrderQueueRow {
    /// The cloud's handle for the queued order (a ULID string).
    pub queued_id: String,
    /// `'pending'` until the device reports, then `'reported'`.
    pub status: String,
    /// The device's reported outcome, or `None` while the order is still pending.
    pub outcome: Option<serde_json::Value>,
}

/// One pending order for the device to act on — its handle and the order payload.
#[derive(Clone, Debug)]
pub struct PendingOrderRow {
    /// The cloud's handle for the queued order (a ULID string).
    pub queued_id: String,
    /// The order as the sales channel offered it.
    pub payload: serde_json::Value,
}

/// The order-queue store over a shared pool. Built by
/// [`PostgresStore::order_queue`](crate::PostgresStore::order_queue).
#[derive(Clone, Debug)]
pub struct PostgresOrderQueue {
    pool: Pool,
}

impl PostgresOrderQueue {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Queues an order idempotently and returns its current standing — the row just inserted, or the
    /// one already there when the `(tenant, store, sales_channel, external_reference)` key has been
    /// seen before. The pre-existing row wins; the incoming offer is discarded.
    ///
    /// The `$6::text::jsonb` cast pins the bound payload's inference to `text` before jsonb, the same
    /// reason the config-tree and rollup tables cast their bound documents.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached, or [`PortError::internal`] if the
    /// payload cannot be (de)serialised.
    pub async fn enqueue(
        &self,
        tenant_id: &str,
        store_id: &str,
        sales_channel: &str,
        external_reference: &str,
        queued_id: &str,
        payload: &serde_json::Value,
    ) -> Result<OrderQueueRow, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let payload_json = serde_json::to_string(payload).map_err(encode)?;
        connection
            .execute(
                "INSERT INTO order_queue \
                 (tenant_id, store_id, sales_channel, external_reference, queued_id, payload) \
                 VALUES ($1, $2, $3, $4, $5, $6::text::jsonb) \
                 ON CONFLICT (tenant_id, store_id, sales_channel, external_reference) DO NOTHING",
                &[
                    &tenant_id,
                    &store_id,
                    &sales_channel,
                    &external_reference,
                    &queued_id,
                    &payload_json,
                ],
            )
            .await
            .map_err(unavailable)?;
        let row = connection
            .query_one(
                "SELECT queued_id, status, outcome::text FROM order_queue \
                 WHERE tenant_id = $1 AND store_id = $2 AND sales_channel = $3 \
                 AND external_reference = $4",
                &[&tenant_id, &store_id, &sales_channel, &external_reference],
            )
            .await
            .map_err(unavailable)?;
        Ok(OrderQueueRow {
            queued_id: row.get(0),
            status: row.get(1),
            outcome: decode_opt(row.get(2))?,
        })
    }

    /// Loads a queued order's current standing by its full idempotency key, or `None` if the key has
    /// never been queued.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached, or [`PortError::internal`] if the
    /// stored outcome cannot be deserialised.
    pub async fn outcome(
        &self,
        tenant_id: &str,
        store_id: &str,
        sales_channel: &str,
        external_reference: &str,
    ) -> Result<Option<OrderQueueRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT queued_id, status, outcome::text FROM order_queue \
                 WHERE tenant_id = $1 AND store_id = $2 AND sales_channel = $3 \
                 AND external_reference = $4",
                &[&tenant_id, &store_id, &sales_channel, &external_reference],
            )
            .await
            .map_err(unavailable)?;
        match row {
            Some(row) => Ok(Some(OrderQueueRow {
                queued_id: row.get(0),
                status: row.get(1),
                outcome: decode_opt(row.get(2))?,
            })),
            None => Ok(None),
        }
    }

    /// The store's pending orders, oldest first, up to `limit` — each the device's handle and the
    /// order payload to act on.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached, or [`PortError::internal`] if a
    /// stored payload cannot be deserialised.
    pub async fn pull_pending(
        &self,
        tenant_id: &str,
        store_id: &str,
        limit: i64,
    ) -> Result<Vec<PendingOrderRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT queued_id, payload::text FROM order_queue \
                 WHERE tenant_id = $1 AND store_id = $2 AND status = 'pending' \
                 ORDER BY created_at ASC LIMIT $3",
                &[&tenant_id, &store_id, &limit],
            )
            .await
            .map_err(unavailable)?;
        let mut orders = Vec::with_capacity(rows.len());
        for row in rows {
            orders.push(PendingOrderRow {
                queued_id: row.get(0),
                payload: decode(row.get(1))?,
            });
        }
        Ok(orders)
    }

    /// Records the device's outcome for a pending order and marks it reported, returning whether a
    /// pending row matched — a second report for the same order finds nothing pending and returns
    /// `false`.
    ///
    /// The `$4::text::jsonb` cast pins the bound outcome's inference to `text` before jsonb, the same
    /// reason the config-tree and rollup tables cast their bound documents.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached, or [`PortError::internal`] if the
    /// outcome cannot be serialised.
    pub async fn record_outcome(
        &self,
        tenant_id: &str,
        store_id: &str,
        queued_id: &str,
        outcome: &serde_json::Value,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let outcome_json = serde_json::to_string(outcome).map_err(encode)?;
        let updated = connection
            .execute(
                "UPDATE order_queue SET status = 'reported', outcome = $4::text::jsonb \
                 WHERE tenant_id = $1 AND store_id = $2 AND queued_id = $3 AND status = 'pending'",
                &[&tenant_id, &store_id, &queued_id, &outcome_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(updated > 0)
    }
}

/// The store→tenant directory over a shared pool. Built by
/// [`PostgresStore::store_directory`](crate::PostgresStore::store_directory).
#[derive(Clone, Debug)]
pub struct PostgresStoreDirectory {
    pool: Pool,
}

impl PostgresStoreDirectory {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Resolves which tenant a store belongs to, or `None` if the store has no config tree yet.
    ///
    /// Read as the trusted role, so it spans every tenant (RLS bypassed) — the caller is resolving a
    /// store it cannot yet name a tenant for, the same posture as the webhook delivery task's
    /// fleet-wide load.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn tenant_of(&self, store_id: &str) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT tenant_id FROM config_trees WHERE store_id = $1 LIMIT 1",
                &[&store_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Resolves which tenant and brand own a store, from the **registry**, or `None` if no store by
    /// that id has been provisioned.
    ///
    /// The registry rather than `config_trees` (which [`Self::tenant_of`] reads): the console creates
    /// the `stores` row when a store is provisioned, before anything is ever published to it, so this
    /// answers for a box that has traded but never been configured. It also carries the brand, which
    /// the config tree does not. Archived stores are included — a reconciliation re-push of a closed
    /// store's history still has to be filed under its owner
    /// ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
    ///
    /// Read as the trusted role, spanning every tenant: the caller is resolving a store it cannot yet
    /// name a tenant for, the same posture as the webhook delivery task's fleet-wide load.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn owner_of(
        &self,
        store_id: &str,
    ) -> Result<Option<(String, Option<String>)>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT tenant_id, brand_id FROM stores WHERE store_id = $1 LIMIT 1",
                &[&store_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| (row.get(0), row.get(1))))
    }
}

/// Parses a jsonb column read back as text into a [`serde_json::Value`].
fn decode(text: &str) -> Result<serde_json::Value, PortError> {
    serde_json::from_str(text).map_err(encode)
}

/// Parses a nullable jsonb column read back as text into an optional [`serde_json::Value`].
fn decode_opt(text: Option<String>) -> Result<Option<serde_json::Value>, PortError> {
    text.map(|text| decode(&text)).transpose()
}

/// Maps an order payload/outcome (de)serialisation failure to the port's internal status.
fn encode(error: serde_json::Error) -> PortError {
    PortError::internal(
        PortName::EventStore,
        "could not (de)serialise an order-queue payload",
    )
    .with_source(error)
}
