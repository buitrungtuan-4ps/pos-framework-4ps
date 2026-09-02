// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud org registry over PostgreSQL (P-WS-C, [ADR-0065](../../../docs/adr/0065-cloud-org-registry.md)).
//!
//! The four named entities — Tenant, Brand, Store, Device — the cloud has always addressed by opaque
//! ULID but never recorded. This adapter keeps only the SQL and returns plain rows; `pos-cloud`
//! implements its `RegistryStore` seam over this type. Tenant scoping is an explicit `WHERE tenant_id
//! = $1` filter (the cloud connects as the trusted pool owner, which bypasses RLS; the migration's
//! policy is the belt-and-suspenders second line), exactly as every other cloud adapter does. Its
//! methods use distinct verbs from the seam (`insert`/`fetch`/`set`) so the seam impl calls the SQL
//! and never itself.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{RowUpdate, pool_unavailable, unavailable};

/// A tenant as listed — the root of the tree.
#[derive(Clone, Debug)]
pub struct TenantRow {
    /// The tenant id (a ULID string).
    pub tenant_id: String,
    /// The human name.
    pub name: String,
    /// `active` or `archived`.
    pub status: String,
    /// The version the row was read at, for a conditional write
    /// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)). Opaque: this is
    /// `xmin::text`, and nothing above this crate may assume that.
    pub version: String,
}

/// A brand as listed — grouped under a tenant.
#[derive(Clone, Debug)]
pub struct BrandRow {
    /// The brand id (a ULID string).
    pub brand_id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The human name.
    pub name: String,
    /// `active` or `archived`.
    pub status: String,
    /// The version the row was read at, for a conditional write
    /// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)). Opaque: this is
    /// `xmin::text`, and nothing above this crate may assume that.
    pub version: String,
}

/// A store as listed — grouped under a tenant and, optionally, a brand.
#[derive(Clone, Debug)]
pub struct StoreRow {
    /// The store id (a ULID string), shared with its `config_trees` row.
    pub store_id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The owning brand, or `None` if the store has no brand yet.
    pub brand_id: Option<String>,
    /// The human name (a placeholder like `Store 01J9…` until an operator renames it).
    pub name: String,
    /// `active` or `archived`.
    pub status: String,
    /// The version the row was read at, for a conditional write
    /// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)). Opaque: this is
    /// `xmin::text`, and nothing above this crate may assume that.
    pub version: String,
}

/// A device as listed — the canonical device identity, grouped under a store.
#[derive(Clone, Debug)]
pub struct DeviceRow {
    /// The device id (a ULID string), shared with `device_proposals` / `device_credentials`.
    pub device_id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The owning store.
    pub store_id: String,
    /// The human name.
    pub name: String,
    /// `pos`, `printer`, `kds`, `tablet`, `unknown`, …
    pub kind: String,
    /// `active` or `archived`.
    pub status: String,
    /// The version the row was read at, for a conditional write
    /// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)). Opaque: this is
    /// `xmin::text`, and nothing above this crate may assume that.
    pub version: String,
}

/// The org-registry store over a shared pool. Built by
/// [`PostgresStore::registry`](crate::PostgresStore::registry).
#[derive(Clone, Debug)]
pub struct PostgresRegistry {
    pool: Pool,
}

impl PostgresRegistry {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    // --- tenants (root; no tenant filter — administered by the trusted connection) ---

    /// Inserts a tenant, returning the `xmin` it starts at (ADR-0094).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_tenant(&self, tenant_id: &str, name: &str) -> Result<String, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_one(
                "INSERT INTO tenants (tenant_id, name) VALUES ($1, $2) RETURNING xmin::text",
                &[&tenant_id, &name],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.get(0))
    }

    /// Lists every tenant, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_tenants(&self) -> Result<Vec<TenantRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT tenant_id, name, status, xmin::text FROM tenants \
                 ORDER BY created_at DESC",
                &[],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| TenantRow {
                tenant_id: row.get(0),
                name: row.get(1),
                status: row.get(2),
                version: row.get(3),
            })
            .collect())
    }

    /// Renames a tenant and sets its status. Applies only if the row is still at `expected`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_tenant(
        &self,
        tenant_id: &str,
        name: &str,
        status: &str,
        expected: &str,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let updated = connection
            .query_opt(
                "UPDATE tenants SET name = $2, status = $3, updated_at = now() \
                 WHERE tenant_id = $1 AND xmin::text = $4 RETURNING xmin::text",
                &[&tenant_id, &name, &status, &expected],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = updated {
            return Ok(RowUpdate::Updated(row.get(0)));
        }
        let present = connection
            .query_opt("SELECT 1 FROM tenants WHERE tenant_id = $1", &[&tenant_id])
            .await
            .map_err(unavailable)?;
        Ok(if present.is_some() {
            RowUpdate::VersionMismatch
        } else {
            RowUpdate::NotFound
        })
    }

    // --- brands (tenant-scoped) ---

    /// Inserts a brand under a tenant, returning the `xmin` it starts at (ADR-0094).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_brand(
        &self,
        brand_id: &str,
        tenant_id: &str,
        name: &str,
    ) -> Result<String, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_one(
                "INSERT INTO brands (brand_id, tenant_id, name) VALUES ($1, $2, $3) \
                 RETURNING xmin::text",
                &[&brand_id, &tenant_id, &name],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.get(0))
    }

    /// Lists a tenant's brands, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_brands(&self, tenant_id: &str) -> Result<Vec<BrandRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT brand_id, tenant_id, name, status, xmin::text FROM brands \
                 WHERE tenant_id = $1 ORDER BY created_at DESC",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| BrandRow {
                brand_id: row.get(0),
                tenant_id: row.get(1),
                name: row.get(2),
                status: row.get(3),
                version: row.get(4),
            })
            .collect())
    }

    /// Renames a brand and sets its status, within its tenant. Applies only if the row is still at `expected`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_brand(
        &self,
        tenant_id: &str,
        brand_id: &str,
        name: &str,
        status: &str,
        expected: &str,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let updated = connection
            .query_opt(
                "UPDATE brands SET name = $3, status = $4, updated_at = now() \
                 WHERE tenant_id = $1 AND brand_id = $2 AND xmin::text = $5 \
                 RETURNING xmin::text",
                &[&tenant_id, &brand_id, &name, &status, &expected],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = updated {
            return Ok(RowUpdate::Updated(row.get(0)));
        }
        let present = connection
            .query_opt(
                "SELECT 1 FROM brands WHERE tenant_id = $1 AND brand_id = $2",
                &[&tenant_id, &brand_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(if present.is_some() {
            RowUpdate::VersionMismatch
        } else {
            RowUpdate::NotFound
        })
    }

    // --- stores (tenant-scoped) ---

    /// Inserts a store under a tenant, with an optional brand, returning its starting `xmin`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_store(
        &self,
        store_id: &str,
        tenant_id: &str,
        brand_id: Option<&str>,
        name: &str,
    ) -> Result<String, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_one(
                "INSERT INTO stores (store_id, tenant_id, brand_id, name) \
                 VALUES ($1, $2, $3, $4) RETURNING xmin::text",
                &[&store_id, &tenant_id, &brand_id, &name],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.get(0))
    }

    /// Lists a tenant's stores, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_stores(&self, tenant_id: &str) -> Result<Vec<StoreRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT store_id, tenant_id, brand_id, name, status, xmin::text FROM stores \
                 WHERE tenant_id = $1 ORDER BY created_at DESC",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| StoreRow {
                store_id: row.get(0),
                tenant_id: row.get(1),
                brand_id: row.get(2),
                name: row.get(3),
                status: row.get(4),
                version: row.get(5),
            })
            .collect())
    }

    /// Renames a store, (re)assigns or clears its brand, and sets its status, within its tenant.
    /// Applies only if the row is still at `expected`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_store(
        &self,
        tenant_id: &str,
        store_id: &str,
        brand_id: Option<&str>,
        name: &str,
        status: &str,
        expected: &str,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let updated = connection
            .query_opt(
                "UPDATE stores SET brand_id = $3, name = $4, status = $5, updated_at = now() \
                 WHERE tenant_id = $1 AND store_id = $2 AND xmin::text = $6 \
                 RETURNING xmin::text",
                &[&tenant_id, &store_id, &brand_id, &name, &status, &expected],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = updated {
            return Ok(RowUpdate::Updated(row.get(0)));
        }
        let present = connection
            .query_opt(
                "SELECT 1 FROM stores WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant_id, &store_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(if present.is_some() {
            RowUpdate::VersionMismatch
        } else {
            RowUpdate::NotFound
        })
    }

    // --- devices (tenant-scoped) ---

    /// Inserts a device under a store, returning the `xmin` it starts at (ADR-0094).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_device(
        &self,
        device_id: &str,
        tenant_id: &str,
        store_id: &str,
        name: &str,
        kind: &str,
    ) -> Result<String, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_one(
                "INSERT INTO devices (device_id, tenant_id, store_id, name, kind) \
                 VALUES ($1, $2, $3, $4, $5) RETURNING xmin::text",
                &[&device_id, &tenant_id, &store_id, &name, &kind],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.get(0))
    }

    /// Lists a store's devices within a tenant, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_devices(
        &self,
        tenant_id: &str,
        store_id: &str,
    ) -> Result<Vec<DeviceRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT device_id, tenant_id, store_id, name, kind, status, xmin::text \
                 FROM devices WHERE tenant_id = $1 AND store_id = $2 ORDER BY created_at DESC",
                &[&tenant_id, &store_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| DeviceRow {
                device_id: row.get(0),
                tenant_id: row.get(1),
                store_id: row.get(2),
                name: row.get(3),
                kind: row.get(4),
                status: row.get(5),
                version: row.get(6),
            })
            .collect())
    }

    /// Renames a device, sets its kind, and sets its status, within its tenant. Applies only if the row is
    /// still at `expected`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_device(
        &self,
        tenant_id: &str,
        device_id: &str,
        name: &str,
        kind: &str,
        status: &str,
        expected: &str,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let updated = connection
            .query_opt(
                "UPDATE devices SET name = $3, kind = $4, status = $5, updated_at = now() \
                 WHERE tenant_id = $1 AND device_id = $2 AND xmin::text = $6 \
                 RETURNING xmin::text",
                &[&tenant_id, &device_id, &name, &kind, &status, &expected],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = updated {
            return Ok(RowUpdate::Updated(row.get(0)));
        }
        let present = connection
            .query_opt(
                "SELECT 1 FROM devices WHERE tenant_id = $1 AND device_id = $2",
                &[&tenant_id, &device_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(if present.is_some() {
            RowUpdate::VersionMismatch
        } else {
            RowUpdate::NotFound
        })
    }
}
