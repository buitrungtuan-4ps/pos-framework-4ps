// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The gateway's seams over their `store-postgres` tables: the endpoints (migration 0098), whose
//! records are (de)serialised here with their secret headers already sealed, so no key reaches the
//! adapter, and the idempotency record (migration 0099).

use pos_cloud_config::persistence::{create_outcome, update_outcome};
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;
use store_postgres::{
    GatewayCallRow, GatewayEndpointRow, PostgresGatewayCalls, PostgresGatewayEndpoints,
};

use super::calls::{GatewayCallStore, GatewayCallStoreError, KEPT_FOR_MS, KeptCall};
use super::endpoints::{GatewayEndpoint, GatewayEndpointStore, GatewayEndpointStoreError};
use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};

/// Pairs a stored row with the version the read saw and the instant it was written.
fn versioned(
    row: &GatewayEndpointRow,
) -> Result<Versioned<GatewayEndpoint>, GatewayEndpointStoreError> {
    let endpoint = serde_json::from_str(&row.doc_json).map_err(|error| {
        GatewayEndpointStoreError::new(format!(
            "a stored gateway endpoint could not be decoded: {error}"
        ))
    })?;
    Ok(Versioned::edited(
        endpoint,
        Version::new(row.version.clone()),
        row.updated_at_ms,
    ))
}

/// Serializes an endpoint for storage. Its secret headers are already sealed.
fn doc(endpoint: &GatewayEndpoint) -> Result<String, GatewayEndpointStoreError> {
    serde_json::to_string(endpoint).map_err(|error| {
        GatewayEndpointStoreError::new(format!("could not serialize a gateway endpoint: {error}"))
    })
}

fn failed(error: impl core::fmt::Display) -> GatewayEndpointStoreError {
    GatewayEndpointStoreError::new(error.to_string())
}

impl GatewayEndpointStore for PostgresGatewayEndpoints {
    async fn list(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<GatewayEndpoint>>, GatewayEndpointStoreError> {
        let rows = self.fetch(&tenant_id.to_string()).await.map_err(failed)?;
        rows.iter().map(versioned).collect()
    }

    async fn get(
        &self,
        tenant_id: TenantId,
        gateway_endpoint_id: &str,
    ) -> Result<Option<Versioned<GatewayEndpoint>>, GatewayEndpointStoreError> {
        let row = self
            .fetch_one(&tenant_id.to_string(), gateway_endpoint_id)
            .await
            .map_err(failed)?;
        row.as_ref().map(versioned).transpose()
    }

    async fn create(
        &self,
        tenant_id: TenantId,
        endpoint: &GatewayEndpoint,
    ) -> Result<CreateOutcome, GatewayEndpointStoreError> {
        let json = doc(endpoint)?;
        self.insert(&tenant_id.to_string(), &endpoint.gateway_endpoint_id, &json)
            .await
            .map(create_outcome)
            .map_err(failed)
    }

    async fn update(
        &self,
        tenant_id: TenantId,
        endpoint: &GatewayEndpoint,
        expected: &Version,
    ) -> Result<UpdateOutcome, GatewayEndpointStoreError> {
        let json = doc(endpoint)?;
        self.update_at(
            &tenant_id.to_string(),
            &endpoint.gateway_endpoint_id,
            &json,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(failed)
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        gateway_endpoint_id: &str,
    ) -> Result<(), GatewayEndpointStoreError> {
        PostgresGatewayEndpoints::delete(self, &tenant_id.to_string(), gateway_endpoint_id)
            .await
            .map_err(failed)
    }
}

impl GatewayCallStore for PostgresGatewayCalls {
    async fn recall(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        idempotency_key: &str,
        now: Timestamp,
    ) -> Result<Option<KeptCall>, GatewayCallStoreError> {
        let older_than = now
            .as_milliseconds_since_epoch()
            .saturating_sub(KEPT_FOR_MS);
        let row = PostgresGatewayCalls::recall(
            self,
            &tenant_id.to_string(),
            &store_id.to_string(),
            idempotency_key,
            older_than,
        )
        .await
        .map_err(|error| GatewayCallStoreError::new(error.to_string()))?;
        row.map(|row| {
            serde_json::from_str(&row.answer_json)
                .map(|answer| KeptCall {
                    gateway_endpoint_id: row.gateway_endpoint_id,
                    answer,
                })
                .map_err(|error| {
                    GatewayCallStoreError::new(format!(
                        "a kept gateway answer could not be decoded: {error}"
                    ))
                })
        })
        .transpose()
    }

    async fn keep(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        idempotency_key: &str,
        call: &KeptCall,
        now: Timestamp,
    ) -> Result<(), GatewayCallStoreError> {
        let answer_json = serde_json::to_string(&call.answer)
            .map_err(|error| GatewayCallStoreError::new(error.to_string()))?;
        let now_ms = now.as_milliseconds_since_epoch();
        PostgresGatewayCalls::keep(
            self,
            &tenant_id.to_string(),
            &store_id.to_string(),
            idempotency_key,
            &GatewayCallRow {
                gateway_endpoint_id: call.gateway_endpoint_id.clone(),
                answer_json,
            },
            now_ms,
            now_ms.saturating_sub(KEPT_FOR_MS),
        )
        .await
        .map_err(|error| GatewayCallStoreError::new(error.to_string()))
    }
}
