// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The configuration-tree table over PostgreSQL (P7, [ADR-0033](../../../docs/adr/0033-config-tree.md)).
//!
//! One row per `(tenant, store)`; the row's `state` column is the whole `ConfigTreeState` — the four
//! authored layers and the published version history — held as `jsonb`. This adapter keeps only the
//! SQL and hands back the raw JSON text; `pos-cloud` implements its `ConfigTreeStore` seam over this
//! type and does the `ConfigTreeState` (de)serialisation, so no cloud-domain type leaks into the
//! adapter — the same split the rollup table uses.
//!
//! Tenant isolation is the `(tenant_id, store_id)` key: a load names both, so it can only ever return
//! the caller's own tenant's row (the migration also enables RLS as a second line for a query role).

use deadpool_postgres::Pool;

use pos_ports::PortError;
use pos_proto::ids::{StoreId, TenantId};

use crate::store::{pool_unavailable, unavailable};

/// The config-tree store over a shared pool. Built by [`PostgresStore::config_trees`](crate::PostgresStore::config_trees).
#[derive(Clone, Debug)]
pub struct PostgresConfigTrees {
    pool: Pool,
}

impl PostgresConfigTrees {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Loads a store's tree state as the raw JSON text of a `ConfigTreeState`, or `None` if the
    /// `(tenant, store)` pair has no row yet.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn load_state(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT state::text FROM config_trees WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Upserts a store's tree state (the raw `ConfigTreeState` JSON).
    ///
    /// The `$3::text::jsonb` cast pins the bound parameter's inference to `text` before jsonb, the
    /// same reason the rollup and event tables cast their bound documents.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn save_state(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        state_json: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO config_trees (tenant_id, store_id, state) \
                 VALUES ($1, $2, $3::text::jsonb) \
                 ON CONFLICT (tenant_id, store_id) \
                 DO UPDATE SET state = EXCLUDED.state, updated_at = now()",
                &[&tenant.to_string(), &store_id.to_string(), &state_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Upserts a store's liveness row from a config pull ([ADR-0068](../../../../docs/adr/0068-fleet-liveness.md)):
    /// records the contact instant, the config version the edge reported holding, and that this
    /// contact was a config pull. `held_version` is the raw ULID string the edge sent, or `None` if it
    /// holds nothing yet. `seen_at_ms` is Unix milliseconds.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_seen(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        held_version: Option<&str>,
        seen_at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO store_liveness \
                 (tenant_id, store_id, last_seen_at, config_version_held, last_config_pull_at) \
                 VALUES ($1, $2, $3, $4, $3) \
                 ON CONFLICT (tenant_id, store_id) DO UPDATE SET \
                 last_seen_at = EXCLUDED.last_seen_at, \
                 config_version_held = EXCLUDED.config_version_held, \
                 last_config_pull_at = EXCLUDED.last_config_pull_at",
                &[
                    &tenant.to_string(),
                    &store_id.to_string(),
                    &seen_at_ms,
                    &held_version,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Advances a store's `last_seen_at` from a heartbeat ([ADR-0068](../../../../docs/adr/0068-fleet-liveness.md)),
    /// leaving `config_version_held` and `last_config_pull_at` untouched on an existing row (a fresh
    /// row gets them `NULL`, since a heartbeat carries no config-pull facts). `seen_at_ms` is Unix
    /// milliseconds.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_heartbeat(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        seen_at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO store_liveness (tenant_id, store_id, last_seen_at) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (tenant_id, store_id) DO UPDATE SET last_seen_at = EXCLUDED.last_seen_at",
                &[&tenant.to_string(), &store_id.to_string(), &seen_at_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
