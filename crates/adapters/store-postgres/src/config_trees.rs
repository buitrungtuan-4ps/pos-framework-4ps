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

use crate::store::{RowUpdate, pool_unavailable, unavailable};

/// The config-tree store over a shared pool. Built by [`PostgresStore::config_trees`](crate::PostgresStore::config_trees).
#[derive(Clone, Debug)]
pub struct PostgresConfigTrees {
    pool: Pool,
}

impl PostgresConfigTrees {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Loads a store's tree state as the raw JSON text of a `ConfigTreeState` **and the version the
    /// row was read at**, or `None` if the `(tenant, store)` pair has no row yet.
    ///
    /// The version is `xmin::text`, the same opaque token every other conditional write in this
    /// adapter uses ([ADR-0094](../../../../docs/adr/0094-console-optimistic-concurrency.md)). It is
    /// not the tree's `ConfigVersionId`: that one lives *inside* the document and is the caller's
    /// concern, while this one is the row's and is what [`Self::save_state`] compares.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn load_state(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Option<(String, String)>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT state::text, xmin::text FROM config_trees \
                 WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| (row.get(0), row.get(1))))
    }

    /// Writes a store's tree state (the raw `ConfigTreeState` JSON) **only if the row is still at
    /// `expected`** ([ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md)).
    ///
    /// This replaces an unconditional upsert, and the two cases are deliberately different
    /// statements rather than one `ON CONFLICT` that papers over them:
    ///
    /// - `expected = None` — the caller read no row, so this must *create* one. `ON CONFLICT DO
    ///   NOTHING` returns zero rows if another publish created it first, which is a
    ///   [`RowUpdate::VersionMismatch`], not a silent overwrite. An upsert here would clobber that
    ///   other publish entirely.
    /// - `expected = Some(v)` — the row must still be at `v`. Zero rows means either the version
    ///   moved or the row is gone, and the probe on the failure path separates them, exactly as the
    ///   record-shaped writes in this adapter do.
    ///
    /// The `$3::text::jsonb` cast pins the bound parameter's inference to `text` before jsonb, the
    /// same reason the rollup and event tables cast their bound documents. The comparison is on
    /// `xmin::text` rather than a cast of `expected` to `xid`, because casting caller-supplied text
    /// to `xid` raises `invalid input syntax for type xid` and would turn a stale token into a `500`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn save_state(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        state_json: &str,
        expected: Option<&str>,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let tenant_text = tenant.to_string();
        let store_text = store_id.to_string();

        let Some(expected) = expected else {
            let inserted = connection
                .query_opt(
                    "INSERT INTO config_trees (tenant_id, store_id, state) \
                     VALUES ($1, $2, $3::text::jsonb) \
                     ON CONFLICT (tenant_id, store_id) DO NOTHING \
                     RETURNING xmin::text",
                    &[&tenant_text, &store_text, &state_json],
                )
                .await
                .map_err(unavailable)?;
            return Ok(inserted.map_or(RowUpdate::VersionMismatch, |row| {
                RowUpdate::Updated(row.get(0))
            }));
        };

        let updated = connection
            .query_opt(
                "UPDATE config_trees SET state = $3::text::jsonb, updated_at = now() \
                 WHERE tenant_id = $1 AND store_id = $2 \
                 AND xmin::text = $4 RETURNING xmin::text",
                &[&tenant_text, &store_text, &state_json, &expected],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = updated {
            return Ok(RowUpdate::Updated(row.get(0)));
        }

        // Zero rows is ambiguous on its own: the version moved, or the row is not there. The probe
        // is what makes a conflict distinguishable from an absence.
        let present = connection
            .query_opt(
                "SELECT 1 FROM config_trees WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant_text, &store_text],
            )
            .await
            .map_err(unavailable)?;
        Ok(if present.is_some() {
            RowUpdate::VersionMismatch
        } else {
            RowUpdate::NotFound
        })
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

    /// Records a store's OTA report ([ADR-0078](../../../../docs/adr/0078-sync-and-ota-closure.md)): the
    /// version it is now running and whether its self-test passed, onto the liveness read model. A
    /// report is contact, so it advances `last_seen_at` too (which lets a fresh row satisfy the
    /// `NOT NULL` on `last_seen_at` when a store reports before it has ever pulled config). `reported_at_ms`
    /// is Unix milliseconds.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_ota_report(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        installed: &str,
        self_test_passed: Option<bool>,
        reported_at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO store_liveness \
                 (tenant_id, store_id, last_seen_at, installed_version, self_test_ok, reported_at) \
                 VALUES ($1, $2, $3, $4, $5, $3) \
                 ON CONFLICT (tenant_id, store_id) DO UPDATE SET \
                 last_seen_at = EXCLUDED.last_seen_at, \
                 installed_version = EXCLUDED.installed_version, \
                 self_test_ok = EXCLUDED.self_test_ok, \
                 reported_at = EXCLUDED.reported_at",
                &[
                    &tenant.to_string(),
                    &store_id.to_string(),
                    &reported_at_ms,
                    &installed,
                    &self_test_passed,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Issues this store's next lease generation and returns it — a **bump**, and the only write
    /// this adapter offers ([ADR-0108](../../../../docs/adr/0108-the-lease-generation-is-authority.md)).
    ///
    /// A store with no row starts at generation `0`, which ADR-0049 names as "the first lease a
    /// store ever issues"; an existing row moves to `generation + 1`. There is deliberately no
    /// set-to-a-value and no decrement: an authority that takes a number from its caller is not one,
    /// and a generation that can move backwards is not monotonic, which is the entire mechanism.
    ///
    /// One statement, so two admins bumping at once serialise on the row rather than racing to the
    /// same number.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn bump_store_lease(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        issued_at_ms: i64,
    ) -> Result<i64, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_one(
                "INSERT INTO store_lease (tenant_id, store_id, generation, issued_at) \
                 VALUES ($1, $2, 0, $3) \
                 ON CONFLICT (tenant_id, store_id) DO UPDATE SET \
                 generation = store_lease.generation + 1, \
                 issued_at = EXCLUDED.issued_at \
                 RETURNING generation",
                &[&tenant.to_string(), &store_id.to_string(), &issued_at_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.get(0))
    }

    /// The store's authoritative lease generation, or `None` if it has never been issued one.
    ///
    /// `None` is not `0`: a store that has never been issued a lease is one no box can be superseded
    /// against, and a store on generation `0` has exactly one machine that may be.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn store_lease(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Option<i64>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT generation FROM store_lease WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Advances a store's `last_seen_at` from a heartbeat ([ADR-0068](../../../../docs/adr/0068-fleet-liveness.md)),
    /// leaving `config_version_held` and `last_config_pull_at` untouched on an existing row (a fresh
    /// row gets them `NULL`, since a heartbeat carries no config-pull facts). `seen_at_ms` is Unix
    /// milliseconds.
    ///
    /// `outbox_depth` is the store's own publish backlog if it reported one, stamped with
    /// `seen_at_ms` as `outbox_reported_at`. A `None` — an older edge, or one that could not read its
    /// log — leaves both columns exactly as they were: "did not say" is not "zero", and overwriting a
    /// real backlog with a fabricated zero would read as a store that had caught up.
    ///
    /// `lease_generation` is the generation the box says it holds
    /// ([ADR-0108](../../../../docs/adr/0108-the-lease-generation-is-authority.md)), under the same
    /// rule and for a sharper reason: generation `0` is a store's *first* real lease, so writing a
    /// zero for a box that said nothing would report a replaced machine as current.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_heartbeat(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        seen_at_ms: i64,
        outbox_depth: Option<i64>,
        lease_generation: Option<i64>,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO store_liveness \
                 (tenant_id, store_id, last_seen_at, outbox_depth, outbox_reported_at, \
                  lease_generation, lease_reported_at) \
                 VALUES ($1, $2, $3, $4, CASE WHEN $4::bigint IS NULL THEN NULL ELSE $3 END, \
                         $5, CASE WHEN $5::bigint IS NULL THEN NULL ELSE $3 END) \
                 ON CONFLICT (tenant_id, store_id) DO UPDATE SET \
                 last_seen_at = EXCLUDED.last_seen_at, \
                 outbox_depth = COALESCE(EXCLUDED.outbox_depth, store_liveness.outbox_depth), \
                 outbox_reported_at = \
                     COALESCE(EXCLUDED.outbox_reported_at, store_liveness.outbox_reported_at), \
                 lease_generation = \
                     COALESCE(EXCLUDED.lease_generation, store_liveness.lease_generation), \
                 lease_reported_at = \
                     COALESCE(EXCLUDED.lease_reported_at, store_liveness.lease_reported_at)",
                &[
                    &tenant.to_string(),
                    &store_id.to_string(),
                    &seen_at_ms,
                    &outbox_depth,
                    &lease_generation,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
