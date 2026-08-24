// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The activation-code and device-credential tables over PostgreSQL (P9, [ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
//!
//! A code is looked up by its SHA-256 hash — the code itself is never stored — and redeemed in one
//! transaction that also mints the device credential. The single-use guard is the
//! `UPDATE … WHERE status = 'issued'` row count, so a replayed or raced code changes nothing.
//! `pos-cloud` implements its `ActivationCodeStore` seam over this type; only hashes are stored,
//! never a code or a secret.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One activation-code row, as plain types — no cloud-domain type crosses the adapter boundary.
///
/// `pos-cloud` converts this into its own shape: `tenant_id`/`store_id`/`device_id` parse to their
/// ULID id types, and `status` maps to `pos_core::activation::CodeStatus`.
#[derive(Clone, Debug)]
pub struct ActivationCodeRow {
    /// The tenant the code — and the credential it mints — belongs to.
    pub tenant_id: String,
    /// The store the code belongs to.
    pub store_id: String,
    /// The device slot the code activates.
    pub device_id: String,
    /// The lifecycle state: `issued`, `redeemed`, or `revoked`.
    pub status: String,
}

/// The activation store over a shared pool. Built by
/// [`PostgresStore::activation_codes`](crate::PostgresStore::activation_codes).
#[derive(Clone, Debug)]
pub struct PostgresActivationCodes {
    pool: Pool,
}

impl PostgresActivationCodes {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Inserts a freshly issued code (status `issued`). Only the `code_hash` is stored, never the code.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails — a duplicate
    /// hash among them, which a 55-bit code makes astronomically unlikely.
    pub async fn issue(
        &self,
        code_hash: &[u8],
        tenant_id: &str,
        store_id: &str,
        device_id: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO activation_codes (code_hash, tenant_id, store_id, device_id) \
                 VALUES ($1, $2, $3, $4)",
                &[&code_hash, &tenant_id, &store_id, &device_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Looks up a code by its hash, returning its slot and lifecycle state, or `None` if unknown.
    ///
    /// A miss is `Ok(None)`, not an error — an unknown code is refused exactly as a spent one is, so
    /// the two cannot be told apart.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn lookup(&self, code_hash: &[u8]) -> Result<Option<ActivationCodeRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT tenant_id, store_id, device_id, status FROM activation_codes \
                 WHERE code_hash = $1",
                &[&code_hash],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| ActivationCodeRow {
            tenant_id: row.get(0),
            store_id: row.get(1),
            device_id: row.get(2),
            status: row.get(3),
        }))
    }

    /// Redeems the code and mints the device credential in one transaction.
    ///
    /// The `UPDATE … WHERE status = 'issued'` is the single-use guard: it flips the one issued code to
    /// `redeemed`, returns its slot, and the credential is inserted for that slot in the same
    /// transaction — so the credential can never inherit a slot other than the code's, and the two
    /// commit together. If no issued row matches — the code was already redeemed or revoked, or a
    /// concurrent request won the race — nothing changes and `Ok(false)` is returned (the transaction
    /// rolls back when it is dropped).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or a statement fails.
    pub async fn consume_and_provision(
        &self,
        code_hash: &[u8],
        credential_id: &str,
        secret_hash: &[u8],
    ) -> Result<bool, PortError> {
        let mut connection = self.pool.get().await.map_err(pool_unavailable)?;
        let transaction = connection.transaction().await.map_err(unavailable)?;
        let redeemed = transaction
            .query_opt(
                "UPDATE activation_codes SET status = 'redeemed', redeemed_at = now() \
                 WHERE code_hash = $1 AND status = 'issued' \
                 RETURNING tenant_id, store_id, device_id",
                &[&code_hash],
            )
            .await
            .map_err(unavailable)?;
        let Some(slot) = redeemed else {
            return Ok(false);
        };
        let tenant_id: String = slot.get(0);
        let store_id: String = slot.get(1);
        let device_id: String = slot.get(2);
        transaction
            .execute(
                "INSERT INTO device_credentials (id, tenant_id, store_id, device_id, secret_hash) \
                 VALUES ($1, $2, $3, $4, $5)",
                &[
                    &credential_id,
                    &tenant_id,
                    &store_id,
                    &device_id,
                    &secret_hash,
                ],
            )
            .await
            .map_err(unavailable)?;
        transaction.commit().await.map_err(unavailable)?;
        Ok(true)
    }

    /// Revokes every still-issued code for a device slot, returning how many were cancelled.
    ///
    /// Idempotent: a slot with no issued codes changes nothing and returns `0`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn revoke_slot(
        &self,
        tenant_id: &str,
        store_id: &str,
        device_id: &str,
    ) -> Result<u64, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE activation_codes SET status = 'revoked' \
                 WHERE tenant_id = $1 AND store_id = $2 AND device_id = $3 AND status = 'issued'",
                &[&tenant_id, &store_id, &device_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed)
    }
}
