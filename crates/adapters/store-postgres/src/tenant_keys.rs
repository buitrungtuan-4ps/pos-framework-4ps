// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Tenant keys over PostgreSQL
//! ([ADR-0217](../../../../docs/adr/0217-secrets-are-sealed-under-versioned-tenant-keys.md),
//! migration 0100). Each row is one version of a tenant's key, wrapped by the environment key;
//! the adapter stores and returns the wrapped bytes and never sees a key.

use deadpool_postgres::Pool;

use pos_ports::{PortError, PortName};

use crate::store::{pool_unavailable, unavailable};

/// One version of a tenant's key, as stored.
#[derive(Clone, PartialEq, Eq)]
pub struct TenantKeyRow {
    /// The tenant key's version, from 1.
    pub key_version: u32,
    /// The environment-key version that wrapped it.
    pub keeper_version: u32,
    /// The wrapped key.
    pub wrapped: Vec<u8>,
    /// When it was generated, in Unix milliseconds.
    pub created_at_ms: i64,
}

impl core::fmt::Debug for TenantKeyRow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TenantKeyRow")
            .field("key_version", &self.key_version)
            .field("keeper_version", &self.keeper_version)
            .finish_non_exhaustive()
    }
}

/// Tenant keys over a shared pool. Built by
/// [`PostgresStore::tenant_keys`](crate::PostgresStore::tenant_keys).
#[derive(Clone, Debug)]
pub struct PostgresTenantKeys {
    pool: Pool,
}

fn version(value: i32) -> u32 {
    u32::try_from(value).unwrap_or_default()
}

impl PostgresTenantKeys {
    /// Every version of the tenant's key, oldest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn versions(&self, tenant_id: &str) -> Result<Vec<TenantKeyRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT key_version, keeper_version, wrapped, created_at_ms FROM tenant_keys \
                 WHERE tenant_id = $1 ORDER BY key_version",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| TenantKeyRow {
                key_version: version(row.get(0)),
                keeper_version: version(row.get(1)),
                wrapped: row.get(2),
                created_at_ms: row.get(3),
            })
            .collect())
    }

    /// Stores a new version; `false` when the tenant already has that version, which another
    /// writer stored first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn insert(&self, tenant_id: &str, key: &TenantKeyRow) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let too_large =
            |_error| PortError::invalid_argument(PortName::KeyKeeper, "a key version is too large");
        let key_version = i32::try_from(key.key_version).map_err(too_large)?;
        let keeper_version = i32::try_from(key.keeper_version).map_err(too_large)?;
        let inserted = connection
            .execute(
                "INSERT INTO tenant_keys \
                 (tenant_id, key_version, keeper_version, wrapped, created_at_ms) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                &[
                    &tenant_id,
                    &key_version,
                    &keeper_version,
                    &key.wrapped,
                    &key.created_at_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(inserted == 1)
    }
}

impl crate::PostgresStore {
    /// Tenant keys over this pool (ADR-0217, migration 0100). Here rather than in `store.rs`,
    /// which is at its size baseline.
    #[must_use]
    pub fn tenant_keys(&self) -> PostgresTenantKeys {
        PostgresTenantKeys {
            pool: self.pool.clone(),
        }
    }
}
