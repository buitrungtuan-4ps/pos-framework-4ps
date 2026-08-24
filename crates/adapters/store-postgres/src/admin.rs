// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The super-admin credential and session tables over PostgreSQL (P7, [ADR-0034](../../../docs/adr/0034-super-admin-auth.md)).
//!
//! There is exactly one super-admin, so [`fetch_credential`](PostgresAdmin::fetch_credential) reads a
//! single row and [`advance_totp_step`](PostgresAdmin::advance_totp_step) updates it in place, only
//! ever forward. Sessions are keyed by `SHA-256(token)` — the raw token is never stored — so
//! [`session_valid`](PostgresAdmin::session_valid) is a primary-key existence check bounded by expiry.
//! This adapter keeps only the SQL and returns plain types; `pos-cloud` implements its `AdminStore`
//! seam over this type, rebuilding its `SuperAdminCredential` from the row.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// The single super-admin credential row, as plain types — no cloud-domain type crosses the boundary.
///
/// `pos-cloud` converts this into its `AdminCredential`: `password_phc` is the Argon2id PHC string,
/// `totp_secret` the raw RFC 6238 secret bytes, and `last_used_totp_step` the newest TOTP step spent
/// (`None` until the first login).
#[derive(Clone, Debug)]
pub struct AdminCredentialRow {
    /// The Argon2id PHC string — the hash, never the password.
    pub password_phc: String,
    /// The raw RFC 6238 TOTP shared secret.
    pub totp_secret: Vec<u8>,
    /// The newest TOTP step already used, or `None` if the admin has never signed in.
    pub last_used_totp_step: Option<i64>,
}

/// The super-admin credential and session store over a shared pool. Built by
/// [`PostgresStore::admin`](crate::PostgresStore::admin).
#[derive(Clone, Debug)]
pub struct PostgresAdmin {
    pool: Pool,
}

impl PostgresAdmin {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Fetches the single super-admin credential, or `None` if one has not been provisioned.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_credential(&self) -> Result<Option<AdminCredentialRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT password_phc, totp_secret, last_used_totp_step FROM super_admin WHERE id",
                &[],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| AdminCredentialRow {
            password_phc: row.get(0),
            totp_secret: row.get(1),
            last_used_totp_step: row.get(2),
        }))
    }

    /// Provisions the single super-admin credential if none exists yet — the first-boot enrolment
    /// ([ADR-0045](../../../docs/adr/0045-first-boot-admin-enrolment.md)). `ON CONFLICT (id) DO
    /// NOTHING` makes it first-writer-wins on the single-row table, so a second call writes nothing;
    /// `last_used_totp_step` starts `NULL`, marking a credential that has never signed in.
    ///
    /// Returns whether it inserted the row.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn insert_credential(
        &self,
        password_phc: &str,
        totp_secret: &[u8],
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .execute(
                "INSERT INTO super_admin (id, password_phc, totp_secret, last_used_totp_step) \
                 VALUES (true, $1, $2, NULL) ON CONFLICT (id) DO NOTHING",
                &[&password_phc, &totp_secret],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows == 1)
    }

    /// Records `step` as the newest TOTP step spent, advancing only forward.
    ///
    /// The `WHERE last_used_totp_step IS NULL OR last_used_totp_step < $1` guard makes this monotonic
    /// and idempotent: two concurrent logins cannot lower the value, and recording a step already at
    /// or below the stored one is a harmless no-op.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn advance_totp_step(&self, step: i64) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "UPDATE super_admin SET last_used_totp_step = $1, updated_at = now() \
                 WHERE id AND (last_used_totp_step IS NULL OR last_used_totp_step < $1)",
                &[&step],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Inserts a session keyed by `token_hash`, valid until `expires_at_ms` (Unix milliseconds).
    ///
    /// `ON CONFLICT DO NOTHING` makes it idempotent — a 256-bit random token never collides in
    /// practice, and if it somehow did the incumbent wins rather than being overwritten.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn insert_session(
        &self,
        token_hash: &[u8],
        expires_at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO admin_sessions (token_hash, expires_at) VALUES ($1, $2) \
                 ON CONFLICT (token_hash) DO NOTHING",
                &[&token_hash, &expires_at_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Whether a session with `token_hash` exists and has not expired as of `now_ms` (Unix ms).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn session_valid(&self, token_hash: &[u8], now_ms: i64) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM admin_sessions \
                 WHERE token_hash = $1 AND expires_at > $2)",
                &[&token_hash, &now_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.get(0))
    }

    /// Deletes the session with `token_hash`. Idempotent — deleting an absent session is a no-op.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete_session(&self, token_hash: &[u8]) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "DELETE FROM admin_sessions WHERE token_hash = $1",
                &[&token_hash],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
