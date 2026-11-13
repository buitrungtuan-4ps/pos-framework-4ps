// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The gateway's idempotency record over PostgreSQL
//! ([ADR-0198](../../../../docs/adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md)
//! decision 6, migration 0099). The adapter keeps only the SQL and hands back raw JSON text;
//! `pos-cloud` owns the answer's shape. Every instant comes from the caller's clock.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One kept call: the endpoint its key was first used for, and the answer as JSON text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayCallRow {
    /// The endpoint's id.
    pub gateway_endpoint_id: String,
    /// The answer as JSON text.
    pub answer_json: String,
}

/// The idempotency record over a shared pool. Built by
/// [`PostgresStore::gateway_calls`](crate::PostgresStore::gateway_calls).
#[derive(Clone, Debug)]
pub struct PostgresGatewayCalls {
    pool: Pool,
}

impl PostgresGatewayCalls {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// The call kept for `key` at the store, unless it was kept at or before `older_than_ms`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn recall(
        &self,
        tenant_id: &str,
        store_id: &str,
        key: &str,
        older_than_ms: i64,
    ) -> Result<Option<GatewayCallRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT gateway_endpoint_id, answer::text FROM gateway_calls \
                 WHERE tenant_id = $1 AND store_id = $2 AND idempotency_key = $3 \
                 AND kept_at_ms > $4",
                &[&tenant_id, &store_id, &key, &older_than_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| GatewayCallRow {
            gateway_endpoint_id: row.get(0),
            answer_json: row.get(1),
        }))
    }

    /// Keeps `call` for `key` at `kept_at_ms`, replacing a row kept at or before `older_than_ms`
    /// and never a younger one, and deletes the store's rows that old.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn keep(
        &self,
        tenant_id: &str,
        store_id: &str,
        key: &str,
        call: &GatewayCallRow,
        kept_at_ms: i64,
        older_than_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "DELETE FROM gateway_calls WHERE tenant_id = $1 AND store_id = $2 \
                 AND kept_at_ms <= $3",
                &[&tenant_id, &store_id, &older_than_ms],
            )
            .await
            .map_err(unavailable)?;
        connection
            .execute(
                "INSERT INTO gateway_calls \
                 (tenant_id, store_id, idempotency_key, gateway_endpoint_id, answer, kept_at_ms) \
                 VALUES ($1, $2, $3, $4, $5::text::jsonb, $6) \
                 ON CONFLICT (tenant_id, store_id, idempotency_key) DO UPDATE \
                 SET gateway_endpoint_id = EXCLUDED.gateway_endpoint_id, \
                     answer = EXCLUDED.answer, kept_at_ms = EXCLUDED.kept_at_ms \
                 WHERE gateway_calls.kept_at_ms <= $7",
                &[
                    &tenant_id,
                    &store_id,
                    &key,
                    &call.gateway_endpoint_id,
                    &call.answer_json,
                    &kept_at_ms,
                    &older_than_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}

impl crate::PostgresStore {
    /// The gateway's idempotency record over this pool (ADR-0198 decision 6, migration 0099). Here
    /// rather than in `store.rs`, which is at its size baseline.
    #[must_use]
    pub fn gateway_calls(&self) -> PostgresGatewayCalls {
        PostgresGatewayCalls::new(self.pool.clone())
    }
}
