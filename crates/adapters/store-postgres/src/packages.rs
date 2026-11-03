// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's packages and their published versions over PostgreSQL
//! ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 1).
//!
//! `packages` and `package_versions` (migration 0089) hold each row's whole record as `jsonb` in
//! `doc`. This adapter keeps only the SQL and hands back the raw JSON text; `pos-cloud-config`
//! implements its `PackageStore` seam over this type and does the (de)serialisation, the split
//! `settings` uses. There is no update and no delete: a published version never changes. Tenant
//! scoping is an explicit `WHERE tenant_id = $1` (the cloud connects as the trusted pool owner,
//! which bypasses RLS; the migration's policy and its insert-only grant are the second line).

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// The package store over a shared pool. Built by
/// [`PostgresStore::packages`](crate::PostgresStore::packages).
#[derive(Clone, Debug)]
pub struct PostgresPackages {
    pool: Pool,
}

impl PostgresPackages {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Every package of a tenant, as JSON text, in id order.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_packages(&self, tenant_id: &str) -> Result<Vec<String>, PortError> {
        self.texts(
            "SELECT doc::text FROM packages WHERE tenant_id = $1 ORDER BY package_id",
            &[&tenant_id],
        )
        .await
    }

    /// One package, as JSON text.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_package(
        &self,
        tenant_id: &str,
        package_id: &str,
    ) -> Result<Option<String>, PortError> {
        Ok(self
            .texts(
                "SELECT doc::text FROM packages WHERE tenant_id = $1 AND package_id = $2",
                &[&tenant_id, &package_id],
            )
            .await?
            .pop())
    }

    /// Inserts a package, and says whether it was inserted: `false` when one already holds its id.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn insert_package(
        &self,
        tenant_id: &str,
        package_id: &str,
        doc_json: &str,
    ) -> Result<bool, PortError> {
        self.insert(
            "INSERT INTO packages (tenant_id, package_id, doc) VALUES ($1, $2, $3::text::jsonb) \
             ON CONFLICT (tenant_id, package_id) DO NOTHING",
            &[&tenant_id, &package_id, &doc_json],
        )
        .await
    }

    /// Every version of one package, as JSON text, in id order.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_versions(
        &self,
        tenant_id: &str,
        package_id: &str,
    ) -> Result<Vec<String>, PortError> {
        self.texts(
            "SELECT doc::text FROM package_versions WHERE tenant_id = $1 AND package_id = $2 \
             ORDER BY package_version_id",
            &[&tenant_id, &package_id],
        )
        .await
    }

    /// One version, as JSON text.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_version(
        &self,
        tenant_id: &str,
        package_version_id: &str,
    ) -> Result<Option<String>, PortError> {
        Ok(self
            .texts(
                "SELECT doc::text FROM package_versions \
                 WHERE tenant_id = $1 AND package_version_id = $2",
                &[&tenant_id, &package_version_id],
            )
            .await?
            .pop())
    }

    /// Stores a version, and says whether it was stored: `false` when one already holds its id,
    /// which is left as it was.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn insert_version(
        &self,
        tenant_id: &str,
        package_version_id: &str,
        package_id: &str,
        doc_json: &str,
    ) -> Result<bool, PortError> {
        self.insert(
            "INSERT INTO package_versions (tenant_id, package_version_id, package_id, doc) \
             VALUES ($1, $2, $3, $4::text::jsonb) \
             ON CONFLICT (tenant_id, package_version_id) DO NOTHING",
            &[&tenant_id, &package_version_id, &package_id, &doc_json],
        )
        .await
    }

    async fn texts(
        &self,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<Vec<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection.query(sql, params).await.map_err(unavailable)?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    async fn insert(
        &self,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let inserted = connection.execute(sql, params).await.map_err(unavailable)?;
        Ok(inserted > 0)
    }
}
