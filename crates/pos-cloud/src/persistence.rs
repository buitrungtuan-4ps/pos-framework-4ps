// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Wiring the cloud's persistence seams to their `store-postgres` tables.
//!
//! The `RollupStore` ([ADR-0036](../../../docs/adr/0036-materialised-rollups.md)), `ApiKeyStore`
//! ([ADR-0037](../../../docs/adr/0037-api-keys.md)) and `AdminStore`
//! ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)) traits live here in the cloud, where the
//! handlers that consume them are; the Postgres tables behind them live in `store-postgres`, the
//! cloud's one Postgres adapter ([ADR-0016](../../../docs/adr/0016-postgres-access.md)). This module
//! is the thin seam between the two: it implements each cloud trait for the adapter's query type,
//! turning the plain row the adapter returns into the cloud's domain shape. All SQL stays in the
//! adapter; all domain conversion stays here — the adapter never learns a cloud type, and the cloud
//! never writes SQL.

use store_postgres::{PostgresAdmin, PostgresApiKeys, PostgresRollups, PostgresStore};

use pos_ports::PortError;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;

use crate::auth::SuperAdminCredential;
use crate::auth::admin::{AdminCredential, AdminStore, AdminStoreError};
use crate::auth::apikey::{ApiKeyId, ApiKeyStore, ApiKeyStoreError, StoredApiKey};
use crate::auth::totp::TotpSecret;
use crate::dashboard::projection::{RollupError, RollupStore, StoredRollups};
use crate::dashboard::projector::StoreCatalog;

impl RollupStore for PostgresRollups {
    async fn load(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<StoredRollups, RollupError> {
        match self
            .load_state(tenant, store_id)
            .await
            .map_err(|error| RollupError::store(error.to_string()))?
        {
            Some(json) => serde_json::from_str(&json).map_err(|error| {
                RollupError::store(format!("decoding the stored rollup state failed: {error}"))
            }),
            // A store with no row yet reads as an empty, cursor-less rollup — the same default the
            // projector starts from, so a first read and a first projection agree.
            None => Ok(StoredRollups::default()),
        }
    }

    async fn save(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        rollups: &StoredRollups,
    ) -> Result<(), RollupError> {
        let json = serde_json::to_string(rollups).map_err(|error| {
            RollupError::store(format!("encoding the rollup state failed: {error}"))
        })?;
        self.save_state(tenant, store_id, &json)
            .await
            .map_err(|error| RollupError::store(error.to_string()))
    }
}

impl StoreCatalog for PostgresStore {
    async fn active_stores(&self) -> Result<Vec<(TenantId, StoreId)>, PortError> {
        self.list_active_stores().await
    }
}

impl AdminStore for PostgresAdmin {
    async fn load_credential(&self) -> Result<Option<AdminCredential>, AdminStoreError> {
        let row = self
            .fetch_credential()
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        Ok(row.map(|row| AdminCredential {
            credential: SuperAdminCredential::new(
                row.password_phc,
                TotpSecret::new(row.totp_secret),
            ),
            // A stored step is always a value this adapter itself wrote, so it fits u64; a stray
            // negative reads as "never used" rather than failing the load.
            last_used_totp_step: row
                .last_used_totp_step
                .and_then(|step| u64::try_from(step).ok()),
        }))
    }

    async fn record_totp_step(&self, step: u64) -> Result<(), AdminStoreError> {
        let step = i64::try_from(step).unwrap_or(i64::MAX);
        self.advance_totp_step(step)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn create_session(
        &self,
        token_hash: [u8; 32],
        expires_at: Timestamp,
    ) -> Result<(), AdminStoreError> {
        self.insert_session(&token_hash, expires_at.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn session_is_valid(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<bool, AdminStoreError> {
        self.session_valid(&token_hash, now.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn revoke_session(&self, token_hash: [u8; 32]) -> Result<(), AdminStoreError> {
        self.delete_session(&token_hash)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }
}

impl ApiKeyStore for PostgresApiKeys {
    async fn lookup(&self, id: ApiKeyId) -> Result<Option<StoredApiKey>, ApiKeyStoreError> {
        let row = self
            .fetch(&id.to_string())
            .await
            .map_err(|error| ApiKeyStoreError::new(error.to_string()))?;
        match row {
            Some(row) => {
                let stored = StoredApiKey::from_parts(
                    id,
                    &row.tenant_id,
                    &row.secret_hash,
                    &row.scopes,
                    row.revoked,
                    row.expires_at_ms,
                )
                .map_err(ApiKeyStoreError::new)?;
                Ok(Some(stored))
            }
            None => Ok(None),
        }
    }
}
