// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The webhook-endpoint table over PostgreSQL (P7, [ADR-0032](../../../docs/adr/0032-webhooks.md)).
//!
//! A webhook is a cursor over the event log, so a row holds only the subscription's durable facts —
//! destination, signing secret, cursor, disabled flag — never a backlog. The admin CRUD filters by
//! tenant; the delivery task loads enabled endpoints fleet-wide as the trusted role. This adapter
//! keeps only the SQL and returns plain types; `pos-cloud` implements its `WebhookEndpointStore` seam
//! over this type, converting rows to and from its `PersistedWebhook` / `WebhookSummary`.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One endpoint's metadata for a listing — never the secret.
#[derive(Clone, Debug)]
pub struct WebhookSummaryRow {
    /// The endpoint id (a ULID string).
    pub id: String,
    /// The store the endpoint follows (a ULID string).
    pub store_id: String,
    /// The destination URL.
    pub url: String,
    /// The cursor position (an event-id ULID string), or `None` if nothing has been delivered.
    pub cursor: Option<String>,
    /// Whether the endpoint is auto-disabled.
    pub disabled: bool,
}

/// One endpoint in full, including the signing secret — for the delivery task to sign with.
#[derive(Clone, Debug)]
pub struct WebhookRow {
    /// The endpoint id (a ULID string).
    pub id: String,
    /// The tenant that owns the subscription.
    pub tenant_id: String,
    /// The store the endpoint follows.
    pub store_id: String,
    /// The destination URL.
    pub url: String,
    /// The HMAC signing secret.
    pub secret: String,
    /// The cursor position (an event-id ULID string), or `None` if nothing has been delivered.
    pub cursor: Option<String>,
}

/// The webhook-endpoint store over a shared pool. Built by
/// [`PostgresStore::webhooks`](crate::PostgresStore::webhooks).
#[derive(Clone, Debug)]
pub struct PostgresWebhooks {
    pool: Pool,
}

impl PostgresWebhooks {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Inserts a freshly registered endpoint (cursor unset, enabled).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails (a duplicate
    /// id among them — a CSPRNG id makes that astronomically unlikely).
    pub async fn create(
        &self,
        id: &str,
        tenant_id: &str,
        store_id: &str,
        url: &str,
        secret: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO webhook_endpoints (id, tenant_id, store_id, url, secret) \
                 VALUES ($1, $2, $3, $4, $5)",
                &[&id, &tenant_id, &store_id, &url, &secret],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's endpoints as metadata only, newest first. The secret is never selected.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list(&self, tenant_id: &str) -> Result<Vec<WebhookSummaryRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT id, store_id, url, cursor, disabled FROM webhook_endpoints \
                 WHERE tenant_id = $1 ORDER BY created_at DESC",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| WebhookSummaryRow {
                id: row.get(0),
                store_id: row.get(1),
                url: row.get(2),
                cursor: row.get(3),
                disabled: row.get(4),
            })
            .collect())
    }

    /// Deletes the endpoint `id` within `tenant_id`, returning whether a row was removed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn remove(&self, tenant_id: &str, id: &str) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let removed = connection
            .execute(
                "DELETE FROM webhook_endpoints WHERE tenant_id = $1 AND id = $2",
                &[&tenant_id, &id],
            )
            .await
            .map_err(unavailable)?;
        Ok(removed == 1)
    }

    /// Every enabled endpoint across the fleet, in full (including the secret), for the delivery task.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_enabled(&self) -> Result<Vec<WebhookRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT id, tenant_id, store_id, url, secret, cursor FROM webhook_endpoints \
                 WHERE disabled = false",
                &[],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| WebhookRow {
                id: row.get(0),
                tenant_id: row.get(1),
                store_id: row.get(2),
                url: row.get(3),
                secret: row.get(4),
                cursor: row.get(5),
            })
            .collect())
    }

    /// Advances an endpoint's cursor to `cursor` (an event-id ULID string).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn advance_cursor(&self, id: &str, cursor: &str) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "UPDATE webhook_endpoints SET cursor = $2 WHERE id = $1",
                &[&id, &cursor],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Sets an endpoint's `disabled` flag.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn mark_disabled(&self, id: &str, disabled: bool) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "UPDATE webhook_endpoints SET disabled = $2 WHERE id = $1",
                &[&id, &disabled],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
