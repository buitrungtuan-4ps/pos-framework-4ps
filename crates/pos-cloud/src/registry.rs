// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud org registry seam ([ADR-0065](../../../docs/adr/0065-cloud-org-registry.md)).
//!
//! The four named entities — Tenant → Brand → Store → Device — the cloud has always addressed by
//! opaque ULID but never recorded. This is the durable *identity and naming* of the tree; the
//! [`config_tree`](crate::config_tree) keeps owning a store's *configuration*, unchanged and still
//! keyed `(tenant_id, store_id)`. A `store` record and its config-tree row share that key.
//!
//! The seam is a trait so it runs against an in-memory fake in tests and a `store-postgres` table in
//! the cloud (the impl lives in [`crate::persistence`], the SQL in `store-postgres`). Names are
//! internal business metadata (T3): mutable, non-sensitive, never customer or employee PII, so the
//! registry holds nothing the retention/masking rules touch ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)).

use core::fmt;
use core::future::Future;

use serde::Serialize;

use pos_proto::ids::{DeviceId, StoreId, TenantId};
use pos_proto::ulid::Ulid;

/// A brand's public identifier — a ULID minted at creation. The other three entities already have id
/// types in [`pos_proto::ids`]; a brand is new here, so its id is defined alongside the seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct BrandId(Ulid);

impl BrandId {
    /// Wraps a ULID as a brand id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for BrandId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Whether an entity is in use or has been retired. Entities are **archived, never hard-deleted**, so
/// foreign references (a store's config tree, a device's credentials) and history stay valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityStatus {
    /// In use.
    Active,
    /// Retired — hidden from the default pickers, kept for history.
    Archived,
}

impl EntityStatus {
    /// The column value stored in PostgreSQL.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }

    /// Reads a stored status; anything but `"archived"` is treated as active, so an unrecognised value
    /// fails safe to visible rather than hiding a store.
    #[must_use]
    pub fn from_db(value: &str) -> Self {
        if value == "archived" {
            Self::Archived
        } else {
            Self::Active
        }
    }
}

/// A tenant — the root of the tree.
#[derive(Debug, Clone, Serialize)]
pub struct TenantRecord {
    /// The tenant id.
    pub tenant_id: TenantId,
    /// The human name.
    pub name: String,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A brand — grouped under a tenant.
#[derive(Debug, Clone, Serialize)]
pub struct BrandRecord {
    /// The brand id.
    pub brand_id: BrandId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The human name.
    pub name: String,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A store — grouped under a tenant and, optionally, a brand. Shares its `store_id` with its
/// `config_trees` row.
#[derive(Debug, Clone, Serialize)]
pub struct StoreRecord {
    /// The store id.
    pub store_id: StoreId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The owning brand, or `None` until an operator assigns one.
    pub brand_id: Option<BrandId>,
    /// The human name (a placeholder until renamed).
    pub name: String,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A device — the canonical device identity, grouped under a store. Shares its `device_id` with
/// `device_proposals` / `device_credentials`.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceRecord {
    /// The device id.
    pub device_id: DeviceId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The owning store.
    pub store_id: StoreId,
    /// The human name.
    pub name: String,
    /// `pos`, `printer`, `kds`, `tablet`, `unknown`, …
    pub kind: String,
    /// Active or archived.
    pub status: EntityStatus,
}

/// Persists and reads the org registry.
///
/// Each entity has the same shape: `create` (a freshly-minted record), `list` (scoped to its parent),
/// and `update` (rename and/or set status, by the id in the record — returning whether a row changed,
/// so a handler can answer `404` for an unknown id). `create_*`/`update_*` never author configuration;
/// they only record identity and naming.
pub trait RegistryStore {
    /// Inserts a tenant.
    fn create_tenant(
        &self,
        tenant: &TenantRecord,
    ) -> impl Future<Output = Result<(), RegistryStoreError>> + Send;

    /// Lists every tenant.
    fn list_tenants(
        &self,
    ) -> impl Future<Output = Result<Vec<TenantRecord>, RegistryStoreError>> + Send;

    /// Renames a tenant and/or sets its status. Returns whether a row was found and changed.
    fn update_tenant(
        &self,
        tenant: &TenantRecord,
    ) -> impl Future<Output = Result<bool, RegistryStoreError>> + Send;

    /// Inserts a brand under a tenant.
    fn create_brand(
        &self,
        brand: &BrandRecord,
    ) -> impl Future<Output = Result<(), RegistryStoreError>> + Send;

    /// Lists a tenant's brands.
    fn list_brands(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<BrandRecord>, RegistryStoreError>> + Send;

    /// Renames a brand and/or sets its status, within its tenant. Returns whether a row changed.
    fn update_brand(
        &self,
        brand: &BrandRecord,
    ) -> impl Future<Output = Result<bool, RegistryStoreError>> + Send;

    /// Inserts a store under a tenant, with an optional brand.
    fn create_store(
        &self,
        store: &StoreRecord,
    ) -> impl Future<Output = Result<(), RegistryStoreError>> + Send;

    /// Lists a tenant's stores.
    fn list_stores(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<StoreRecord>, RegistryStoreError>> + Send;

    /// Renames a store, (re)assigns or clears its brand, and/or sets its status, within its tenant.
    /// Returns whether a row changed.
    fn update_store(
        &self,
        store: &StoreRecord,
    ) -> impl Future<Output = Result<bool, RegistryStoreError>> + Send;

    /// Inserts a device under a store.
    fn create_device(
        &self,
        device: &DeviceRecord,
    ) -> impl Future<Output = Result<(), RegistryStoreError>> + Send;

    /// Lists a store's devices, within its tenant.
    fn list_devices(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Vec<DeviceRecord>, RegistryStoreError>> + Send;

    /// Renames a device, sets its kind, and/or sets its status, within its tenant. Returns whether a
    /// row changed.
    fn update_device(
        &self,
        device: &DeviceRecord,
    ) -> impl Future<Output = Result<bool, RegistryStoreError>> + Send;
}

/// A failure of the registry store itself — the database is unreachable.
#[derive(Debug, thiserror::Error)]
#[error("the registry store failed: {0}")]
pub struct RegistryStoreError(String);

impl RegistryStoreError {
    /// Wraps a message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}
