// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The webhook-endpoint seam over its `store-postgres` table
//! ([ADR-0032](../../../../docs/adr/0032-webhooks.md)): moved out of `persistence.rs` when the
//! event-type filter (roadmap-v3 B6.2) grew it past that file's size baseline.

use pos_proto::ids::{EventId, StoreId, TenantId};
use pos_proto::ulid::Ulid;
use store_postgres::PostgresWebhooks;

use crate::webhook::sign::SigningSecret;
use crate::webhook::store::{
    PersistedWebhook, WebhookEndpointId, WebhookEndpointStore, WebhookStoreError, WebhookSummary,
};

impl WebhookEndpointStore for PostgresWebhooks {
    async fn insert(&self, endpoint: &PersistedWebhook) -> Result<(), WebhookStoreError> {
        // A freshly registered endpoint always has no cursor and is enabled; the adapter's `create`
        // defaults those, so only the durable identity + destination + secret cross here.
        self.create(
            &endpoint.id.to_string(),
            &endpoint.tenant_id.to_string(),
            &endpoint.store_id.to_string(),
            &endpoint.url,
            endpoint.secret.expose_secret(),
            &endpoint.event_types,
        )
        .await
        .map_err(|error| WebhookStoreError::new(error.to_string()))
    }

    async fn list_for_tenant(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<WebhookSummary>, WebhookStoreError> {
        let rows = self
            .list(&tenant_id.to_string())
            .await
            .map_err(|error| WebhookStoreError::new(error.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| WebhookSummary {
                id: row.id,
                store_id: row.store_id,
                url: row.url,
                cursor: row.cursor,
                disabled: row.disabled,
                event_types: row.event_types,
            })
            .collect())
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        id: WebhookEndpointId,
    ) -> Result<bool, WebhookStoreError> {
        self.remove(&tenant_id.to_string(), &id.to_string())
            .await
            .map_err(|error| WebhookStoreError::new(error.to_string()))
    }

    async fn load_enabled(&self) -> Result<Vec<PersistedWebhook>, WebhookStoreError> {
        let rows = self
            .fetch_enabled()
            .await
            .map_err(|error| WebhookStoreError::new(error.to_string()))?;
        let mut endpoints = Vec::with_capacity(rows.len());
        for row in rows {
            let id = row
                .id
                .parse::<Ulid>()
                .map(WebhookEndpointId::new)
                .map_err(|_| {
                    WebhookStoreError::new(format!("endpoint id is not a ULID: {}", row.id))
                })?;
            let tenant_id = row
                .tenant_id
                .parse::<TenantId>()
                .map_err(|_| WebhookStoreError::new("a webhook tenant id is not a ULID"))?;
            let store_id = row
                .store_id
                .parse::<StoreId>()
                .map_err(|_| WebhookStoreError::new("a webhook store id is not a ULID"))?;
            let cursor = match row.cursor {
                Some(text) => Some(text.parse::<EventId>().map_err(|_| {
                    WebhookStoreError::new("a webhook cursor is not an event-id ULID")
                })?),
                None => None,
            };
            endpoints.push(PersistedWebhook {
                id,
                tenant_id,
                store_id,
                url: row.url,
                secret: SigningSecret::new(row.secret),
                cursor,
                // `fetch_enabled` returns only enabled rows.
                disabled: false,
                event_types: row.event_types,
            });
        }
        Ok(endpoints)
    }

    async fn save_cursor(
        &self,
        id: WebhookEndpointId,
        cursor: EventId,
    ) -> Result<(), WebhookStoreError> {
        self.advance_cursor(&id.to_string(), &cursor.to_string())
            .await
            .map_err(|error| WebhookStoreError::new(error.to_string()))
    }

    async fn set_disabled(
        &self,
        id: WebhookEndpointId,
        disabled: bool,
    ) -> Result<(), WebhookStoreError> {
        self.mark_disabled(&id.to_string(), disabled)
            .await
            .map_err(|error| WebhookStoreError::new(error.to_string()))
    }
}
