// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! This crate's stores over PostgreSQL: [`AdminStore`], [`ApiKeyStore`], [`ApiKeyAdminStore`] and
//! [`AuditStore`] for the `store-postgres` types that hold their tables.
//!
//! Moved from `pos-cloud`'s `persistence` module with the traits (ADR-0170 decision 4): Rust lets a
//! trait be implemented for another crate's type only in the trait's own crate, so the glue lives
//! beside the traits. The SQL stays in `store-postgres`.

use store_postgres::{
    AdminInviteRow, AdminLoginRow, AdminSessionRow, AdminUserRow, AuditLogRow, AuditOrder,
    NewSessionRow, PostgresAdmin, PostgresApiKeys, PostgresAudit,
};

use store_postgres::{PostgresTaskHealth, TaskHealthRow};

use pos_proto::ids::TenantId;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

use crate::audit::{
    AuditActor, AuditEntry, AuditId, AuditQuery, AuditStore, AuditStoreError, TrailOrder,
};
use crate::auth::SuperAdminCredential;
use crate::auth::admin::{
    AdminCredential, AdminInvite, AdminLogin, AdminRole, AdminStatus, AdminStore, AdminStoreError,
    AdminUser, LiveSession, NewAdminInvite, NewAdminSession, NewAdminUser, NewRecoveryCode,
    SessionSummary,
};
use crate::auth::apikey::{
    ApiKeyAdminStore, ApiKeyId, ApiKeyStore, ApiKeyStoreError, ApiKeySummary, StoredApiKey,
};
use crate::auth::totp::TotpSecret;
use crate::health::{TaskHealth, TaskHealthError, TaskHealthStore};
use crate::paging::{Page, PageRequest};

impl AdminStore for PostgresAdmin {
    async fn load_credential(&self) -> Result<Option<AdminCredential>, AdminStoreError> {
        let row = self
            .fetch_credential()
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        Ok(row.map(|row| AdminCredential {
            credential: SuperAdminCredential::new(
                row.password_phc,
                TotpSecret::new(row.totp_secret),
            ),
            // A stored step is always a value this adapter itself wrote, so it fits u64; a stray
            // negative reads as "never used" rather than failing the load.
            last_used_totp_step: row
                .last_used_totp_step
                .and_then(|step| u64::try_from(step).ok()),
        }))
    }

    async fn provision_credential(
        &self,
        password_phc: String,
        totp_secret: Vec<u8>,
    ) -> Result<bool, AdminStoreError> {
        self.insert_credential(&password_phc, &totp_secret)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn record_totp_step(&self, step: u64) -> Result<(), AdminStoreError> {
        let step = i64::try_from(step).unwrap_or(i64::MAX);
        self.advance_totp_step(step)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn create_session(&self, session: NewAdminSession) -> Result<(), AdminStoreError> {
        self.insert_session(NewSessionRow {
            token_hash: &session.token_hash,
            created_at_ms: session.created_at.as_milliseconds_since_epoch(),
            expires_at_ms: session.expires_at.as_milliseconds_since_epoch(),
            absolute_expires_at_ms: Some(session.absolute_expires_at.as_milliseconds_since_epoch()),
            idle_ttl_ms: Some(session.idle_ttl_ms),
            admin_id: session.admin_id.as_deref(),
            ip: session.ip.as_deref(),
            user_agent: session.user_agent.as_deref(),
        })
        .await
        .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn session_is_valid(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<bool, AdminStoreError> {
        self.session_valid(&token_hash, now.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn session_admin(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<Option<LiveSession>, AdminStoreError> {
        let found = self
            .fetch_session_admin(&token_hash, now.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        Ok(found.map(|admin_id| LiveSession { admin_id }))
    }

    async fn revoke_session(&self, token_hash: [u8; 32]) -> Result<(), AdminStoreError> {
        self.delete_session(&token_hash)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn list_admin_sessions(
        &self,
        admin_id: &str,
        now: Timestamp,
    ) -> Result<Vec<SessionSummary>, AdminStoreError> {
        let rows =
            PostgresAdmin::list_admin_sessions(self, admin_id, now.as_milliseconds_since_epoch())
                .await
                .map_err(|error| AdminStoreError::new(error.to_string()))?;
        rows.into_iter().map(session_summary_from_row).collect()
    }

    async fn revoke_admin_session(
        &self,
        admin_id: &str,
        token_hash: [u8; 32],
    ) -> Result<bool, AdminStoreError> {
        self.delete_admin_session(admin_id, &token_hash)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn revoke_other_admin_sessions(
        &self,
        admin_id: &str,
        except_token_hash: [u8; 32],
    ) -> Result<u64, AdminStoreError> {
        self.delete_other_admin_sessions(admin_id, &except_token_hash)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn rotate_totp_secret(&self, secret: Vec<u8>) -> Result<(), AdminStoreError> {
        PostgresAdmin::rotate_totp_secret(self, &secret)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn store_recovery_codes(
        &self,
        admin_id: &str,
        codes: Vec<NewRecoveryCode>,
    ) -> Result<(), AdminStoreError> {
        let codes: Vec<(String, Vec<u8>)> = codes
            .into_iter()
            .map(|code| (code.id, code.code_hash.to_vec()))
            .collect();
        self.replace_recovery_codes(admin_id, &codes)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn consume_recovery_code(
        &self,
        admin_id: &str,
        code_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<bool, AdminStoreError> {
        PostgresAdmin::consume_recovery_code(
            self,
            admin_id,
            &code_hash,
            now.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn count_recovery_codes(&self, admin_id: &str) -> Result<u64, AdminStoreError> {
        let count = PostgresAdmin::count_recovery_codes(self, admin_id)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    async fn create_admin_user(&self, user: NewAdminUser) -> Result<bool, AdminStoreError> {
        self.insert_admin_user(
            &user.id,
            &user.email,
            &user.name,
            user.role.as_token(),
            AdminStatus::Active.as_token(),
            &user.password_phc,
            &user.totp_secret,
        )
        .await
        .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn list_admin_users(&self) -> Result<Vec<AdminUser>, AdminStoreError> {
        let rows = PostgresAdmin::list_admin_users(self)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        rows.into_iter().map(admin_user_from_row).collect()
    }

    async fn get_admin_user(&self, id: &str) -> Result<Option<AdminUser>, AdminStoreError> {
        let row = self
            .fetch_admin_user(id)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        row.map(admin_user_from_row).transpose()
    }

    async fn find_admin_user_by_email(
        &self,
        email: &str,
    ) -> Result<Option<AdminUser>, AdminStoreError> {
        let row = self
            .fetch_admin_user_by_email(email)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        row.map(admin_user_from_row).transpose()
    }

    async fn find_admin_login_by_email(
        &self,
        email: &str,
    ) -> Result<Option<AdminLogin>, AdminStoreError> {
        let row = self
            .fetch_admin_login_by_email(email)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        row.map(admin_login_from_row).transpose()
    }

    async fn record_admin_totp_step(
        &self,
        admin_id: &str,
        step: u64,
    ) -> Result<(), AdminStoreError> {
        // Saturating rather than failing: a step past `i64::MAX` is 292 billion years away, and a
        // clamp keeps the column monotone either way. Matches `record_totp_step` exactly.
        let step = i64::try_from(step).unwrap_or(i64::MAX);
        self.advance_admin_totp_step(admin_id, step)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn rotate_admin_totp_secret(
        &self,
        admin_id: &str,
        secret: Vec<u8>,
    ) -> Result<(), AdminStoreError> {
        PostgresAdmin::rotate_admin_totp_secret(self, admin_id, &secret)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn set_admin_user_role(
        &self,
        id: &str,
        role: AdminRole,
    ) -> Result<bool, AdminStoreError> {
        PostgresAdmin::set_admin_user_role(self, id, role.as_token())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn set_admin_user_status(
        &self,
        id: &str,
        status: AdminStatus,
    ) -> Result<bool, AdminStoreError> {
        PostgresAdmin::set_admin_user_status(self, id, status.as_token())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn count_active_owners(&self) -> Result<u64, AdminStoreError> {
        let count = PostgresAdmin::count_active_owners(self)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    async fn create_invite(&self, invite: NewAdminInvite) -> Result<(), AdminStoreError> {
        self.insert_invite(
            &invite.id,
            &invite.email,
            &invite.name,
            invite.role.as_token(),
            &invite.token_hash,
            &invite.invited_by,
            invite.expires_at.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn find_pending_invite_by_token(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<Option<AdminInvite>, AdminStoreError> {
        let row = self
            .fetch_pending_invite_by_token(&token_hash, now.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        row.map(admin_invite_from_row).transpose()
    }

    async fn mark_invite_accepted(
        &self,
        id: &str,
        accepted_at: Timestamp,
    ) -> Result<bool, AdminStoreError> {
        PostgresAdmin::mark_invite_accepted(self, id, accepted_at.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn list_pending_invites(
        &self,
        now: Timestamp,
    ) -> Result<Vec<AdminInvite>, AdminStoreError> {
        let rows = PostgresAdmin::list_pending_invites(self, now.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        rows.into_iter().map(admin_invite_from_row).collect()
    }

    async fn revoke_invite(&self, id: &str) -> Result<bool, AdminStoreError> {
        self.delete_pending_invite(id)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }
}

/// Converts a stored `admin_users` row into the domain [`AdminUser`], failing loudly if the role or
/// status token is unrecognised — the table's CHECK constraints keep them within the known
/// vocabularies, so an unparseable value is store corruption, not an ordinary absence.
fn admin_user_from_row(row: AdminUserRow) -> Result<AdminUser, AdminStoreError> {
    let role = AdminRole::from_token(&row.role)
        .ok_or_else(|| AdminStoreError::new(format!("unknown admin role token: {}", row.role)))?;
    let status = AdminStatus::from_token(&row.status).ok_or_else(|| {
        AdminStoreError::new(format!("unknown admin status token: {}", row.status))
    })?;
    Ok(AdminUser {
        id: row.id,
        email: row.email,
        name: row.name,
        role,
        status,
    })
}

/// Converts a stored `admin_users` row *with its credential* into the domain [`AdminLogin`] — the
/// per-admin sign-in read
/// ([ADR-0119](../../../docs/adr/0119-each-admin-signs-in-as-themselves.md)).
///
/// Delegates the identity half to [`admin_user_from_row`], so an unknown role or status token fails
/// here exactly as it does on a listing read rather than silently authenticating someone whose role
/// this build cannot name.
fn admin_login_from_row(row: AdminLoginRow) -> Result<AdminLogin, AdminStoreError> {
    let credential = AdminCredential {
        credential: SuperAdminCredential::new(row.password_phc, TotpSecret::new(row.totp_secret)),
        // A stored step is always a value this adapter itself wrote, so it fits u64; a stray negative
        // reads as "never used" rather than failing the load, as `load_credential` does.
        last_used_totp_step: row
            .last_used_totp_step
            .and_then(|step| u64::try_from(step).ok()),
    };
    let user = admin_user_from_row(AdminUserRow {
        id: row.id,
        email: row.email,
        name: row.name,
        role: row.role,
        status: row.status,
    })?;
    Ok(AdminLogin { user, credential })
}

/// Converts a stored `admin_sessions` row into the domain [`SessionSummary`], failing loudly if the
/// stored token hash is not 32 bytes or a timestamp is out of range — either is store corruption, not
/// an ordinary absence.
fn session_summary_from_row(row: AdminSessionRow) -> Result<SessionSummary, AdminStoreError> {
    let token_hash: [u8; 32] = row.token_hash.try_into().map_err(|_ignored: Vec<u8>| {
        AdminStoreError::new("a stored session token hash is not 32 bytes")
    })?;
    let created_at = Timestamp::from_milliseconds_since_epoch(row.created_at_ms)
        .map_err(|_ignored| AdminStoreError::new("a session created_at is out of range"))?;
    let expires_at = Timestamp::from_milliseconds_since_epoch(row.expires_at_ms)
        .map_err(|_ignored| AdminStoreError::new("a session expires_at is out of range"))?;
    Ok(SessionSummary {
        token_hash,
        ip: row.ip,
        user_agent: row.user_agent,
        created_at,
        expires_at,
    })
}

/// Converts a stored `admin_invites` row into the domain [`AdminInvite`], failing loudly on an
/// unrecognised role token (the table's CHECK keeps it within the vocabulary).
fn admin_invite_from_row(row: AdminInviteRow) -> Result<AdminInvite, AdminStoreError> {
    let role = AdminRole::from_token(&row.role)
        .ok_or_else(|| AdminStoreError::new(format!("unknown admin role token: {}", row.role)))?;
    Ok(AdminInvite {
        id: row.id,
        email: row.email,
        name: row.name,
        role,
        invited_by: row.invited_by,
        accepted: row.accepted,
    })
}

impl ApiKeyAdminStore for PostgresApiKeys {
    async fn insert(&self, key: &StoredApiKey) -> Result<(), ApiKeyStoreError> {
        let store_id = key.store_id.map(|id| id.to_string());
        PostgresApiKeys::insert(
            self,
            &key.id.to_string(),
            &key.tenant_id.to_string(),
            store_id.as_deref(),
            &key.secret_hash(),
            &key.scope_wire_names(),
            key.expires_at_ms(),
        )
        .await
        .map_err(|error| ApiKeyStoreError::new(error.to_string()))
    }

    async fn list_for_tenant(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<ApiKeySummary>, ApiKeyStoreError> {
        let rows = PostgresApiKeys::list_for_tenant(self, &tenant_id.to_string())
            .await
            .map_err(|error| ApiKeyStoreError::new(error.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| ApiKeySummary {
                id: row.id,
                store_id: row.store_id,
                scopes: row.scopes,
                revoked: row.revoked,
                expires_at_ms: row.expires_at_ms,
            })
            .collect())
    }

    async fn revoke(&self, id: ApiKeyId) -> Result<bool, ApiKeyStoreError> {
        PostgresApiKeys::revoke(self, &id.to_string())
            .await
            .map_err(|error| ApiKeyStoreError::new(error.to_string()))
    }
}

impl ApiKeyStore for PostgresApiKeys {
    async fn lookup(&self, id: ApiKeyId) -> Result<Option<StoredApiKey>, ApiKeyStoreError> {
        let row = self
            .fetch(&id.to_string())
            .await
            .map_err(|error| ApiKeyStoreError::new(error.to_string()))?;
        match row {
            Some(row) => {
                let stored = StoredApiKey::from_parts(
                    id,
                    &row.tenant_id,
                    row.store_id.as_deref(),
                    &row.secret_hash,
                    &row.scopes,
                    row.revoked,
                    row.expires_at_ms,
                )
                .map_err(ApiKeyStoreError::new)?;
                Ok(Some(stored))
            }
            None => Ok(None),
        }
    }

    async fn lookup_device(&self, id: ApiKeyId) -> Result<Option<StoredApiKey>, ApiKeyStoreError> {
        let row = self
            .fetch_device_credential(&id.to_string())
            .await
            .map_err(|error| ApiKeyStoreError::new(error.to_string()))?;
        // An archived device's credential is refused exactly as an unknown one is (ADR-0143).
        match row {
            Some(row) if !row.archived => {
                StoredApiKey::for_device(id, &row.tenant_id, &row.store_id, &row.secret_hash)
                    .map(Some)
                    .map_err(ApiKeyStoreError::new)
            }
            Some(_) | None => Ok(None),
        }
    }
}

/// Converts one stored `audit_log` row into the cloud's [`AuditEntry`], failing loudly on a
/// malformed id/tenant/role/timestamp or an undecodable before/after — every one of those is store
/// corruption (this cloud wrote well-formed values), not an ordinary absence.
fn audit_entry(row: AuditLogRow) -> Result<AuditEntry, AuditStoreError> {
    let id = row
        .id
        .parse::<Ulid>()
        .map(AuditId::new)
        .map_err(|_ignored| {
            AuditStoreError::new(format!("an audit id is not a ULID: {}", row.id))
        })?;
    let tenant_id = match row.tenant_id {
        Some(text) => Some(
            text.parse::<Ulid>()
                .map(TenantId::new)
                .map_err(|_ignored| AuditStoreError::new("an audit tenant id is not a ULID"))?,
        ),
        None => None,
    };
    let role = AdminRole::from_token(&row.actor_role).ok_or_else(|| {
        AuditStoreError::new(format!(
            "unknown audit actor role token: {}",
            row.actor_role
        ))
    })?;
    let before = decode_audit_value(row.before_json, "before")?;
    let after = decode_audit_value(row.after_json, "after")?;
    let at = Timestamp::from_milliseconds_since_epoch(row.at_ms)
        .map_err(|_ignored| AuditStoreError::new("an audit timestamp is out of range"))?;
    Ok(AuditEntry {
        id,
        tenant_id,
        actor: AuditActor {
            admin_id: row.actor_admin_id,
            email: row.actor_email,
            role,
        },
        action: row.action,
        entity_type: row.entity_type,
        entity_id: row.entity_id,
        before,
        after,
        request_id: row.request_id,
        at,
    })
}

/// Decodes a stored audit before/after JSON document, or `None` if the column was `NULL`.
fn decode_audit_value(
    text: Option<String>,
    which: &str,
) -> Result<Option<serde_json::Value>, AuditStoreError> {
    match text {
        Some(json) => serde_json::from_str(&json).map(Some).map_err(|error| {
            AuditStoreError::new(format!("decoding an audit {which} value failed: {error}"))
        }),
        None => Ok(None),
    }
}

/// Encodes an audit before/after value to JSON text for storage, or `None` if absent.
fn encode_audit_value(
    value: Option<&serde_json::Value>,
    which: &str,
) -> Result<Option<String>, AuditStoreError> {
    match value {
        Some(value) => serde_json::to_string(value).map(Some).map_err(|error| {
            AuditStoreError::new(format!("encoding an audit {which} value failed: {error}"))
        }),
        None => Ok(None),
    }
}

impl AuditStore for PostgresAudit {
    async fn append(&self, entry: &AuditEntry) -> Result<(), AuditStoreError> {
        let tenant = entry.tenant_id.map(|tenant| tenant.to_string());
        let before = encode_audit_value(entry.before.as_ref(), "before")?;
        let after = encode_audit_value(entry.after.as_ref(), "after")?;
        self.insert(
            &entry.id.to_string(),
            tenant.as_deref(),
            &entry.actor.admin_id,
            &entry.actor.email,
            entry.actor.role.as_token(),
            &entry.action,
            &entry.entity_type,
            &entry.entity_id,
            before.as_deref(),
            after.as_deref(),
            entry.request_id.as_deref(),
            entry.at.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| AuditStoreError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: Option<TenantId>,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AuditStoreError> {
        let tenant = tenant.map(|tenant| tenant.to_string());
        let rows = self
            .fetch(tenant.as_deref(), i64::from(limit))
            .await
            .map_err(|error| AuditStoreError::new(error.to_string()))?;
        rows.into_iter().map(audit_entry).collect()
    }

    async fn query(
        &self,
        filter: &AuditQuery,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AuditStoreError> {
        let tenant = filter.tenant.map(|tenant| tenant.to_string());
        let rows = self
            .search(
                tenant.as_deref(),
                filter.entity_type.as_deref(),
                filter.entity_id.as_deref(),
                filter.action.as_deref(),
                filter.actor_admin_id.as_deref(),
                filter.since_ms,
                filter.until_ms,
                i64::from(limit),
            )
            .await
            .map_err(|error| AuditStoreError::new(error.to_string()))?;
        rows.into_iter().map(audit_entry).collect()
    }

    async fn query_page(
        &self,
        filter: &AuditQuery,
        page: PageRequest,
        order: TrailOrder,
    ) -> Result<Page<AuditEntry>, AuditStoreError> {
        let tenant = filter.tenant.map(|tenant| tenant.to_string());
        // The one place the trail's order vocabulary meets the adapter's. Exhaustive by necessity:
        // `clippy::wildcard_enum_match_arm` is denied, so a new `TrailOrder` variant fails to
        // compile here rather than quietly falling back to newest-first.
        let order = match order {
            TrailOrder::Newest => AuditOrder::Newest,
            TrailOrder::Oldest => AuditOrder::Oldest,
        };
        let (rows, total) = self
            .search_page(
                tenant.as_deref(),
                filter.entity_type.as_deref(),
                filter.entity_id.as_deref(),
                filter.action.as_deref(),
                filter.actor_admin_id.as_deref(),
                filter.since_ms,
                filter.until_ms,
                order,
                i64::from(page.limit()),
                i64::from(page.offset()),
            )
            .await
            .map_err(|error| AuditStoreError::new(error.to_string()))?;
        let entries: Vec<AuditEntry> = rows
            .into_iter()
            .map(audit_entry)
            .collect::<Result<_, _>>()?;
        Ok(Page::new(entries, u32::try_from(total).unwrap_or(u32::MAX)))
    }
}

// --- Background-task health (ADR-0068 slice 4) --------------------------------------------------

/// Converts one `store-postgres` task-health row into the cloud's [`TaskHealth`]. A tick instant out
/// of range (impossible for values this cloud wrote) or a detail that will not parse fails safe — the
/// instant to the epoch, the detail to an empty object — rather than dropping the whole listing, since
/// this is health telemetry and a decode fault must not itself read as "no health data".
fn task_health(row: TaskHealthRow) -> TaskHealth {
    let last_tick_at =
        Timestamp::from_milliseconds_since_epoch(row.last_tick_at_ms).unwrap_or(Timestamp::EPOCH);
    let detail = serde_json::from_str(&row.detail_json)
        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
    TaskHealth {
        task: row.task,
        last_tick_at,
        detail,
    }
}

impl TaskHealthStore for PostgresTaskHealth {
    async fn record_tick(
        &self,
        task: &str,
        at: Timestamp,
        detail: &serde_json::Value,
    ) -> Result<(), TaskHealthError> {
        let detail_json = serde_json::to_string(detail).map_err(|error| {
            TaskHealthError::new(format!("encoding a task-health detail failed: {error}"))
        })?;
        self.record(task, at.as_milliseconds_since_epoch(), &detail_json)
            .await
            .map_err(|error| TaskHealthError::new(error.to_string()))
    }

    async fn list_health(&self) -> Result<Vec<TaskHealth>, TaskHealthError> {
        let rows = self
            .fetch_all()
            .await
            .map_err(|error| TaskHealthError::new(error.to_string()))?;
        Ok(rows.into_iter().map(task_health).collect())
    }
}
