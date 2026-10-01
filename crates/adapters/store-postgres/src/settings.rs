// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's setting values over PostgreSQL
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 3).
//!
//! One row per `(tenant, setting_key, scope, scope_id)`; the `doc` column is the whole value as
//! `jsonb` (`setting_values`, migration 0072). This adapter keeps only the SQL and hands back the raw
//! JSON text; `pos-cloud` implements its `SettingsStore` seam over this type and does the
//! (de)serialisation, so no cloud-domain type reaches the adapter — the split `connections` uses.
//! Tenant scoping is an explicit `WHERE tenant_id = $1` (the cloud connects as the trusted pool
//! owner, which bypasses RLS; the migration's policy is the second line).

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// Where one value sits: its setting, its scope and the id it was written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingSlot<'a> {
    /// The setting's `node.field` key.
    pub setting_key: &'a str,
    /// The scope's wire token, such as `SETTING_SCOPE_STORE`.
    pub scope: &'a str,
    /// The tenant, brand, store group or store the value was written for.
    pub scope_id: &'a str,
}

/// The setting store over a shared pool. Built by
/// [`PostgresStore::settings`](crate::PostgresStore::settings).
#[derive(Clone, Debug)]
pub struct PostgresSettings {
    pool: Pool,
}

impl PostgresSettings {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Every value a tenant has written, as JSON text, in key, scope and scope-id order.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(&self, tenant_id: &str) -> Result<Vec<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT doc::text FROM setting_values WHERE tenant_id = $1 \
                 ORDER BY setting_key, scope, scope_id",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    /// Writes a value, replacing the one already in its slot.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn upsert(
        &self,
        tenant_id: &str,
        slot: SettingSlot<'_>,
        doc_json: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO setting_values (tenant_id, setting_key, scope, scope_id, doc) \
                 VALUES ($1, $2, $3, $4, $5::text::jsonb) \
                 ON CONFLICT (tenant_id, setting_key, scope, scope_id) \
                 DO UPDATE SET doc = EXCLUDED.doc, update_time = now()",
                &[
                    &tenant_id,
                    &slot.setting_key,
                    &slot.scope,
                    &slot.scope_id,
                    &doc_json,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Removes the value in one slot, and says whether there was one. Removing one that is not
    /// there is not an error.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete(&self, tenant_id: &str, slot: SettingSlot<'_>) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let removed = connection
            .execute(
                "DELETE FROM setting_values \
                 WHERE tenant_id = $1 AND setting_key = $2 AND scope = $3 AND scope_id = $4",
                &[&tenant_id, &slot.setting_key, &slot.scope, &slot.scope_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(removed > 0)
    }
}
