// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Where tenant keys are kept: every version of each tenant's key, wrapped by the environment key
//! (ADR-0217 decision 1, `store-postgres` migration 0100). The store holds wrapped bytes only.

use core::future::Future;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use pos_ports::key_keeper::WrappedKey;
use pos_proto::ids::TenantId;
use store_postgres::{PostgresTenantKeys, TenantKeyRow};

/// One version of a tenant's key, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TenantKey {
    /// The tenant key's version, from 1. A value sealed under it names it.
    pub key_version: u32,
    /// The key, wrapped by the environment key.
    pub wrapped: WrappedKey,
    /// When the cloud generated it, in Unix milliseconds.
    pub created_at_ms: i64,
}

/// A failure of the tenant-key store. Never carries a key.
#[derive(Debug, thiserror::Error)]
#[error("the tenant key store failed: {0}")]
pub struct TenantKeyStoreError(String);

/// Keeps every version of each tenant's key.
pub trait TenantKeyStore: Send + Sync {
    /// Every version of the tenant's key, oldest first.
    fn versions(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<TenantKey>, TenantKeyStoreError>> + Send;

    /// Stores a new version; `false` when the tenant already has it, because another writer
    /// stored it first.
    fn insert(
        &self,
        tenant_id: TenantId,
        key: &TenantKey,
    ) -> impl Future<Output = Result<bool, TenantKeyStoreError>> + Send;
}

impl TenantKeyStore for PostgresTenantKeys {
    async fn versions(&self, tenant_id: TenantId) -> Result<Vec<TenantKey>, TenantKeyStoreError> {
        let rows = PostgresTenantKeys::versions(self, &tenant_id.to_string())
            .await
            .map_err(|error| TenantKeyStoreError(error.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| TenantKey {
                key_version: row.key_version,
                wrapped: WrappedKey {
                    keeper_version: row.keeper_version,
                    bytes: row.wrapped,
                },
                created_at_ms: row.created_at_ms,
            })
            .collect())
    }

    async fn insert(
        &self,
        tenant_id: TenantId,
        key: &TenantKey,
    ) -> Result<bool, TenantKeyStoreError> {
        let row = TenantKeyRow {
            key_version: key.key_version,
            keeper_version: key.wrapped.keeper_version,
            wrapped: key.wrapped.bytes.clone(),
            created_at_ms: key.created_at_ms,
        };
        PostgresTenantKeys::insert(self, &tenant_id.to_string(), &row)
            .await
            .map_err(|error| TenantKeyStoreError(error.to_string()))
    }
}

/// The most tenants [`MemoryTenantKeys`] holds: it is for tests and a demonstration cloud.
const MEMORY_TENANTS_MAX: usize = 10_000;

/// Tenant keys in memory, for tests and a demonstration cloud without a database.
#[derive(Clone, Debug, Default)]
pub struct MemoryTenantKeys(Arc<Mutex<BTreeMap<TenantId, Vec<TenantKey>>>>);

impl TenantKeyStore for MemoryTenantKeys {
    async fn versions(&self, tenant_id: TenantId) -> Result<Vec<TenantKey>, TenantKeyStoreError> {
        let held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(held.get(&tenant_id).cloned().unwrap_or_default())
    }

    async fn insert(
        &self,
        tenant_id: TenantId,
        key: &TenantKey,
    ) -> Result<bool, TenantKeyStoreError> {
        let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !held.contains_key(&tenant_id) && held.len() >= MEMORY_TENANTS_MAX {
            return Err(TenantKeyStoreError(format!(
                "an in-memory key store holds at most {MEMORY_TENANTS_MAX} tenants"
            )));
        }
        let versions = held.entry(tenant_id).or_default();
        if versions
            .iter()
            .any(|held| held.key_version == key.key_version)
        {
            return Ok(false);
        }
        versions.push(key.clone());
        versions.sort_by_key(|held| held.key_version);
        Ok(true)
    }
}
