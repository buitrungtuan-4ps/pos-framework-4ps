// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The API-key table over PostgreSQL (P7, [ADR-0037](../../../docs/adr/0037-api-keys.md)).
//!
//! A key is looked up by its public `id` — the ULID half of the `pos_<id>_<secret>` token — which is
//! the primary key, so the lookup is a single-row fetch. This adapter keeps only the SQL and returns
//! the row's columns as an [`ApiKeyRow`] of plain types; `pos-cloud` implements its `ApiKeyStore`
//! seam over this type, rebuilding its `StoredApiKey` from the row and then verifying the presented
//! secret against `secret_hash` in constant time. Only the hash is stored, never the secret.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One API-key row, as plain types — no cloud-domain type crosses the adapter boundary.
///
/// `pos-cloud` converts this into its `StoredApiKey`: `tenant_id` parses to a `TenantId`,
/// `secret_hash` is the 32-byte SHA-256, `scopes` are the wire names (unknown ones dropped,
/// deny-by-default), and `expires_at_ms` is milliseconds since the Unix epoch.
#[derive(Clone, Debug)]
pub struct ApiKeyRow {
    /// The tenant the key acts for.
    pub tenant_id: String,
    /// `SHA-256(secret)` — 32 bytes.
    pub secret_hash: Vec<u8>,
    /// The granted scopes, as their wire names.
    pub scopes: Vec<String>,
    /// Whether the key has been revoked.
    pub revoked: bool,
    /// When the key expires, in milliseconds since the Unix epoch, if ever.
    pub expires_at_ms: Option<i64>,
}

/// One row of a key listing — metadata only, never the secret hash.
///
/// `pos-cloud` converts this into its `ApiKeySummary` for the `/admin/api-keys` list response.
#[derive(Clone, Debug)]
pub struct ApiKeySummaryRow {
    /// The public id (the ULID half of the token).
    pub id: String,
    /// The granted scopes, as their wire names.
    pub scopes: Vec<String>,
    /// Whether the key has been revoked.
    pub revoked: bool,
    /// When the key expires, in milliseconds since the Unix epoch, if ever.
    pub expires_at_ms: Option<i64>,
}

/// The API-key store over a shared pool. Built by [`PostgresStore::api_keys`](crate::PostgresStore::api_keys).
#[derive(Clone, Debug)]
pub struct PostgresApiKeys {
    pool: Pool,
}

impl PostgresApiKeys {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Fetches the key with the public `id` (a ULID string), or `None` if there is no such key.
    ///
    /// A miss is `Ok(None)`, not an error: an unknown key is refused exactly as a bad secret is, so
    /// the two cannot be told apart. An `Err` means the database itself was unreachable.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(&self, id: &str) -> Result<Option<ApiKeyRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT tenant_id, secret_hash, scopes, revoked, expires_at \
                 FROM api_keys WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| ApiKeyRow {
            tenant_id: row.get(0),
            secret_hash: row.get(1),
            scopes: row.get(2),
            revoked: row.get(3),
            expires_at_ms: row.get(4),
        }))
    }

    /// Inserts a freshly issued key. Only the `secret_hash` is stored, never the secret.
    ///
    /// A duplicate `id` is a conflict (an error), not an overwrite — a CSPRNG id makes that
    /// astronomically unlikely, and silently replacing a key would be worse than failing loudly.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails (a duplicate
    /// id among them).
    pub async fn insert(
        &self,
        id: &str,
        tenant_id: &str,
        secret_hash: &[u8],
        scopes: &[String],
        expires_at_ms: Option<i64>,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO api_keys (id, tenant_id, secret_hash, scopes, expires_at) \
                 VALUES ($1, $2, $3, $4, $5)",
                &[&id, &tenant_id, &secret_hash, &scopes, &expires_at_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's keys as metadata only, newest first. The secret hash is never selected.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_for_tenant(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<ApiKeySummaryRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT id, scopes, revoked, expires_at FROM api_keys \
                 WHERE tenant_id = $1 ORDER BY created_at DESC",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| ApiKeySummaryRow {
                id: row.get(0),
                scopes: row.get(1),
                revoked: row.get(2),
                expires_at_ms: row.get(3),
            })
            .collect())
    }

    /// Revokes the key with `id`, returning whether a live key was found and revoked.
    ///
    /// Idempotent: revoking an already-revoked or absent key changes no row and returns `false`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn revoke(&self, id: &str) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE api_keys SET revoked = true WHERE id = $1 AND revoked = false",
                &[&id],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }
}
