// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The gateway endpoint seam over its `store-postgres` table (migration 0098): the record is
//! (de)serialised here, its secret headers already sealed, so no key reaches the adapter.

use pos_cloud_config::persistence::{create_outcome, update_outcome};
use pos_proto::ids::TenantId;
use store_postgres::{GatewayEndpointRow, PostgresGatewayEndpoints};

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
