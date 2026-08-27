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

/// The columns of a new admin session, as plain types — the input to
/// [`insert_session`](PostgresAdmin::insert_session). Bundled into a struct rather than passed as
/// eight positional arguments. All times are Unix milliseconds; `idle_ttl_ms` is a duration.
///
/// `absolute_expires_at_ms`/`idle_ttl_ms` are `None` only for a legacy fixed-TTL session (one that
/// never slides); the login flow always sets both.
#[derive(Clone, Debug)]
pub struct NewSessionRow<'a> {
    /// `SHA-256(token)` — the row's primary key; the raw token is never stored.
    pub token_hash: &'a [u8],
    /// When the session was minted (Unix ms), from the caller's clock.
    pub created_at_ms: i64,
    /// When the session next expires if not slid before then (Unix ms).
    pub expires_at_ms: i64,
    /// The hard ceiling the sliding TTL can never pass (Unix ms), or `None` for a legacy session.
    pub absolute_expires_at_ms: Option<i64>,
    /// The idle window a real request slides the session forward by (ms), or `None` for a legacy one.
    pub idle_ttl_ms: Option<i64>,
    /// The `admin_users` id the session belongs to, or `None` for a legacy session.
    pub admin_id: Option<&'a str>,
    /// The client IP the session was minted for (for the admin's own session list), if known.
    pub ip: Option<&'a str>,
    /// The client user-agent the session was minted for, if known.
    pub user_agent: Option<&'a str>,
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

    /// Inserts a session ([`NewSessionRow`]), keyed by `token_hash` and owned by `admin_id` (an
    /// `admin_users` id, or `None` for a legacy session).
    ///
    /// `created_at` is written from the caller's clock (Unix ms → `timestamptz`) rather than the
    /// database default, so it matches the rest of the cloud's clock-as-source-of-truth posture and is
    /// deterministic under test. `absolute_expires_at`/`idle_ttl_ms` drive the sliding TTL
    /// ([`fetch_session_admin`](Self::fetch_session_admin)); both `None` mints a legacy fixed-TTL
    /// session that never slides. `ON CONFLICT DO NOTHING` makes it idempotent — a 256-bit random
    /// token never collides in practice, and if it somehow did the incumbent wins rather than being
    /// overwritten.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn insert_session(&self, session: NewSessionRow<'_>) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO admin_sessions \
                 (token_hash, created_at, expires_at, absolute_expires_at, idle_ttl_ms, \
                  admin_id, ip, user_agent) \
                 VALUES ($1, to_timestamp($2::bigint / 1000.0), $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (token_hash) DO NOTHING",
                &[
                    &session.token_hash,
                    &session.created_at_ms,
                    &session.expires_at_ms,
                    &session.absolute_expires_at_ms,
                    &session.idle_ttl_ms,
                    &session.admin_id,
                    &session.ip,
                    &session.user_agent,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Whether a session with `token_hash` exists and has not expired as of `now_ms` (Unix ms). A pure
    /// read that does **not** slide the TTL — the lightweight "am I signed in?" liveness check, so a
    /// background poll cannot keep an otherwise-idle session alive.
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

    /// The `admin_id` of a live session (`token_hash`, not expired as of `now_ms`), *sliding* its idle
    /// TTL as a side effect — the guard behind every real `/admin` action
    /// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 4).
    ///
    /// A single `UPDATE … RETURNING` validates and slides atomically: for a modern session (both
    /// `absolute_expires_at` and `idle_ttl_ms` set) it advances `expires_at` to
    /// `LEAST(now + idle_ttl_ms, absolute_expires_at)` — extending it within the idle window but never
    /// past the absolute cap; for a legacy session (either column `NULL`) it leaves `expires_at`
    /// untouched, so a pre-slice-4 session keeps its original fixed expiry. The `WHERE expires_at >
    /// now` guard means an already-expired session matches nothing and is not resurrected.
    ///
    /// The outer `Option` distinguishes "no live session" (`None`) from "a live session" (`Some`);
    /// the inner `Option<String>` is the owning admin's id, or `None` for a legacy session minted
    /// before multi-admin.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_session_admin(
        &self,
        token_hash: &[u8],
        now_ms: i64,
    ) -> Result<Option<Option<String>>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "UPDATE admin_sessions SET expires_at = CASE \
                     WHEN absolute_expires_at IS NULL OR idle_ttl_ms IS NULL THEN expires_at \
                     ELSE LEAST($2 + idle_ttl_ms, absolute_expires_at) \
                 END \
                 WHERE token_hash = $1 AND expires_at > $2 \
                 RETURNING admin_id",
                &[&token_hash, &now_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Lists the caller's own live sessions (owned by `admin_id`, not expired as of `now_ms`), newest
    /// first — the self-service "my sessions" view. Scoped to the one admin, so no admin sees
    /// another's sessions.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_admin_sessions(
        &self,
        admin_id: &str,
        now_ms: i64,
    ) -> Result<Vec<AdminSessionRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT token_hash, ip, user_agent, \
                        (extract(epoch FROM created_at) * 1000)::bigint, expires_at \
                 FROM admin_sessions \
                 WHERE admin_id = $1 AND expires_at > $2 \
                 ORDER BY created_at DESC, token_hash",
                &[&admin_id, &now_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(row_to_admin_session).collect())
    }

    /// Deletes one of the caller's own sessions by `token_hash`, scoped to `admin_id` so an admin can
    /// only revoke a session that is theirs. Returns whether a row was removed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete_admin_session(
        &self,
        admin_id: &str,
        token_hash: &[u8],
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .execute(
                "DELETE FROM admin_sessions WHERE admin_id = $1 AND token_hash = $2",
                &[&admin_id, &token_hash],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows == 1)
    }

    /// Deletes all of the caller's sessions *except* the one named by `except_token_hash` (their
    /// current one) — "sign out everywhere else". Returns how many were removed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete_other_admin_sessions(
        &self,
        admin_id: &str,
        except_token_hash: &[u8],
    ) -> Result<u64, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let removed = connection
            .execute(
                "DELETE FROM admin_sessions WHERE admin_id = $1 AND token_hash <> $2",
                &[&admin_id, &except_token_hash],
            )
            .await
            .map_err(unavailable)?;
        Ok(removed)
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

    // ---- Multi-admin surface ([ADR-0067]) ----

    /// Inserts a console admin, first-writer-wins on the email: `ON CONFLICT (lower(email)) DO
    /// NOTHING` matches the case-insensitive unique index, so a second insert for an existing address
    /// writes nothing. Returns whether it inserted the row.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn insert_admin_user(
        &self,
        id: &str,
        email: &str,
        name: &str,
        role: &str,
        status: &str,
        password_phc: &str,
        totp_secret: &[u8],
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .execute(
                "INSERT INTO admin_users \
                 (id, email, name, role, status, password_phc, totp_secret, last_used_totp_step) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, NULL) \
                 ON CONFLICT (lower(email)) DO NOTHING",
                &[
                    &id,
                    &email,
                    &name,
                    &role,
                    &status,
                    &password_phc,
                    &totp_secret,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows == 1)
    }

    /// Lists every console admin — identity and role only, oldest first. No credential column is
    /// selected, so nothing sensitive crosses the boundary.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_admin_users(&self) -> Result<Vec<AdminUserRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT id, email, name, role, status FROM admin_users ORDER BY created_at, id",
                &[],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(row_to_admin_user).collect())
    }

    /// Fetches one console admin by id, or `None`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_admin_user(&self, id: &str) -> Result<Option<AdminUserRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT id, email, name, role, status FROM admin_users WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(row_to_admin_user))
    }

    /// Fetches one console admin by email, compared case-insensitively, or `None`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_admin_user_by_email(
        &self,
        email: &str,
    ) -> Result<Option<AdminUserRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT id, email, name, role, status FROM admin_users WHERE lower(email) = lower($1)",
                &[&email],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(row_to_admin_user))
    }

    /// Sets an admin's role. Returns whether a row matched `id`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_admin_user_role(&self, id: &str, role: &str) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .execute(
                "UPDATE admin_users SET role = $2, updated_at = now() WHERE id = $1",
                &[&id, &role],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows == 1)
    }

    /// Sets an admin's status. Returns whether a row matched `id`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_admin_user_status(&self, id: &str, status: &str) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .execute(
                "UPDATE admin_users SET status = $2, updated_at = now() WHERE id = $1",
                &[&id, &status],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows == 1)
    }

    /// How many admins are both `owner` and `active`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn count_active_owners(&self) -> Result<i64, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_one(
                "SELECT count(*) FROM admin_users WHERE role = 'owner' AND status = 'active'",
                &[],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.get(0))
    }

    // ---- Invitations ([ADR-0067]) ----

    /// Inserts a pending invitation. Only `SHA-256(token)` is stored.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn insert_invite(
        &self,
        id: &str,
        email: &str,
        name: &str,
        role: &str,
        token_hash: &[u8],
        invited_by: &str,
        expires_at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO admin_invites \
                 (id, email, name, role, token_hash, invited_by, expires_at, accepted_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, NULL)",
                &[
                    &id,
                    &email,
                    &name,
                    &role,
                    &token_hash,
                    &invited_by,
                    &expires_at_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// The still-acceptable invitation for `token_hash` as of `now_ms` — not accepted, not expired —
    /// or `None`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_pending_invite_by_token(
        &self,
        token_hash: &[u8],
        now_ms: i64,
    ) -> Result<Option<AdminInviteRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT id, email, name, role, invited_by, (accepted_at IS NOT NULL) \
                 FROM admin_invites \
                 WHERE token_hash = $1 AND accepted_at IS NULL AND expires_at > $2",
                &[&token_hash, &now_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(row_to_admin_invite))
    }

    /// Marks the invite `id` accepted at `accepted_at_ms`, single-use: the `WHERE accepted_at IS NULL`
    /// guard makes it claim the invite exactly once, so a concurrent or replayed acceptance that does
    /// not match writes nothing. Returns whether this call claimed it.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn mark_invite_accepted(
        &self,
        id: &str,
        accepted_at_ms: i64,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .execute(
                "UPDATE admin_invites SET accepted_at = $2 WHERE id = $1 AND accepted_at IS NULL",
                &[&id, &accepted_at_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows == 1)
    }

    /// Lists invitations still pending as of `now_ms` — not accepted, not expired — oldest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_pending_invites(
        &self,
        now_ms: i64,
    ) -> Result<Vec<AdminInviteRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT id, email, name, role, invited_by, (accepted_at IS NOT NULL) \
                 FROM admin_invites \
                 WHERE accepted_at IS NULL AND expires_at > $1 ORDER BY created_at, id",
                &[&now_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(row_to_admin_invite).collect())
    }

    /// Deletes a pending invitation by id (an accepted one is left as the record of enrolment).
    /// Returns whether a row was removed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete_pending_invite(&self, id: &str) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .execute(
                "DELETE FROM admin_invites WHERE id = $1 AND accepted_at IS NULL",
                &[&id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows == 1)
    }

    // ---- Credential recovery + rotation ([ADR-0067] slice 6) ----

    /// Replaces the single super-admin's TOTP secret and resets its last-used step to `NULL`, so a
    /// freshly-enrolled authenticator's codes verify from step zero — the store half of a TOTP
    /// re-enrolment.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn rotate_totp_secret(&self, secret: &[u8]) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "UPDATE super_admin SET totp_secret = $1, last_used_totp_step = NULL, \
                 updated_at = now() WHERE id",
                &[&secret],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Replaces `admin_id`'s recovery codes with `codes` (each an `(id, SHA-256(code))` pair):
    /// regenerating the set deletes whatever was there, then inserts the new codes on the same
    /// connection. Only the hash is stored, never the code.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn replace_recovery_codes(
        &self,
        admin_id: &str,
        codes: &[(String, Vec<u8>)],
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "DELETE FROM admin_recovery_codes WHERE admin_id = $1",
                &[&admin_id],
            )
            .await
            .map_err(unavailable)?;
        for (id, code_hash) in codes {
            let id = id.as_str();
            let code_hash = code_hash.as_slice();
            connection
                .execute(
                    "INSERT INTO admin_recovery_codes (id, admin_id, code_hash, used_at) \
                     VALUES ($1, $2, $3, NULL)",
                    &[&id, &admin_id, &code_hash],
                )
                .await
                .map_err(unavailable)?;
        }
        Ok(())
    }

    /// Consumes an unused recovery code for `admin_id` matching `code_hash`, stamping `used_at`
    /// single-use: the `WHERE used_at IS NULL` guard means the first caller claims it and a replay
    /// (or a never-issued code) matches nothing. Returns whether a row was claimed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn consume_recovery_code(
        &self,
        admin_id: &str,
        code_hash: &[u8],
        used_at_ms: i64,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .execute(
                "UPDATE admin_recovery_codes SET used_at = $3 \
                 WHERE admin_id = $1 AND code_hash = $2 AND used_at IS NULL",
                &[&admin_id, &code_hash, &used_at_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows == 1)
    }

    /// How many of `admin_id`'s recovery codes are still unused.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn count_recovery_codes(&self, admin_id: &str) -> Result<i64, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_one(
                "SELECT count(*) FROM admin_recovery_codes WHERE admin_id = $1 AND used_at IS NULL",
                &[&admin_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.get(0))
    }
}

/// A console admin as listed — identity and role, no credential (P7, [ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)).
///
/// `pos-cloud` converts this into its `AdminUser`: `role` and `status` are the stored tokens
/// (`owner`/`admin`/`ops`/`viewer`, `active`/`suspended`) it maps to its enums.
#[derive(Clone, Debug)]
pub struct AdminUserRow {
    /// The admin's ULID id (a string).
    pub id: String,
    /// The login identity.
    pub email: String,
    /// The display name.
    pub name: String,
    /// The stored role token.
    pub role: String,
    /// The stored status token.
    pub status: String,
}

/// Reads an `admin_users` row selected as `(id, email, name, role, status)`.
fn row_to_admin_user(row: &tokio_postgres::Row) -> AdminUserRow {
    AdminUserRow {
        id: row.get(0),
        email: row.get(1),
        name: row.get(2),
        role: row.get(3),
        status: row.get(4),
    }
}

/// An invitation as listed — no token (only its hash is stored) (P7, [ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)).
///
/// `pos-cloud` converts this into its `AdminInvite`: `role` is the stored token, `accepted` reflects
/// whether `accepted_at` is set.
#[derive(Clone, Debug)]
pub struct AdminInviteRow {
    /// The invite's ULID id.
    pub id: String,
    /// The invitee's email.
    pub email: String,
    /// The display name.
    pub name: String,
    /// The stored role token.
    pub role: String,
    /// The id of the inviting admin.
    pub invited_by: String,
    /// Whether the invite has been accepted.
    pub accepted: bool,
}

/// One of an admin's own live sessions, as listed — no token (only its hash is stored), no expiry
/// policy internals (P7, [ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 4).
///
/// `pos-cloud` converts this into its `SessionSummary`: `token_hash` is the opaque revocation handle
/// (`SHA-256(token)`, never reversible to the token), and `created_at_ms`/`expires_at_ms` are Unix
/// milliseconds.
#[derive(Clone, Debug)]
pub struct AdminSessionRow {
    /// `SHA-256(token)` — the opaque handle the console revokes the session by.
    pub token_hash: Vec<u8>,
    /// The client IP the session was minted for, if it was known.
    pub ip: Option<String>,
    /// The client user-agent the session was minted for, if it was known.
    pub user_agent: Option<String>,
    /// When the session was minted (Unix ms).
    pub created_at_ms: i64,
    /// When the session currently expires (Unix ms), after any sliding.
    pub expires_at_ms: i64,
}

/// Reads an `admin_sessions` row selected as `(token_hash, ip, user_agent, created_at_ms, expires_at)`.
fn row_to_admin_session(row: &tokio_postgres::Row) -> AdminSessionRow {
    AdminSessionRow {
        token_hash: row.get(0),
        ip: row.get(1),
        user_agent: row.get(2),
        created_at_ms: row.get(3),
        expires_at_ms: row.get(4),
    }
}

/// Reads an `admin_invites` row selected as `(id, email, name, role, invited_by, accepted)`.
fn row_to_admin_invite(row: &tokio_postgres::Row) -> AdminInviteRow {
    AdminInviteRow {
        id: row.get(0),
        email: row.get(1),
        name: row.get(2),
        role: row.get(3),
        invited_by: row.get(4),
        accepted: row.get(5),
    }
}
