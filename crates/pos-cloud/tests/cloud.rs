// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `pos_cloud`'s ingest and rollup spine and its public `/v1` surface, against the in-memory fakes.
//!
//! The same handler code runs here against `pos-fakes` and, in the binary, against `store-postgres`
//! (ADR-0026) — so idempotent ingest, the materialised rollup read, and the `/v1` bearer check are
//! proven without a database, while the store-specific behaviour (RLS, partitioning, the rollup and
//! API-key tables) is proven by `store-postgres`'s own integration suite.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use argon2::password_hash::SaltString;

use pos_cloud::activation::{
    ActivationCodeStore, ActivationStoreError, DeviceCredential, IssuedCode, hash_code,
};
use pos_cloud::audit::{
    AuditActor, AuditEntry, AuditId, AuditQuery, AuditRecorder, AuditSink, AuditStore,
    AuditStoreError, NoopAuditRecorder,
};
use pos_cloud::auth::SuperAdminCredential;
use pos_cloud::auth::admin::{
    AdminCredential, AdminInvite, AdminRole, AdminStatus, AdminStore, AdminStoreError, AdminUser,
    LiveSession, NewAdminInvite, NewAdminSession, NewAdminUser, NewRecoveryCode, SessionSummary,
    hash_session_token,
};
use pos_cloud::auth::apikey::{
    ApiKeyAdminStore, ApiKeyId, ApiKeyStore, ApiKeyStoreError, ApiKeySummary, Scope, StoredApiKey,
    issue,
};
use pos_cloud::auth::password::hash_password;
use pos_cloud::auth::totp::{DIGITS, TotpSecret, code_at};
use pos_cloud::catalog::{
    CatalogItem, CatalogStore, CatalogStoreError, DisplayCategory, DisplaySubcategory,
    ItemCategory, ItemSubcategory, LayoutButton, Menu, MenuId, MenuPlacement, MenuSection,
    ModifierGroup, TaxClass,
};
use pos_cloud::config_tree::{ConfigStoreError, ConfigTreeState, ConfigTreeStore};
use pos_cloud::dashboard::{RollupError, RollupStore, StoredRollups, project};
use pos_cloud::devices::{
    DeviceKind, DeviceProposalError, DeviceProposalId, DeviceProposalStatus, DeviceProposalStore,
    DeviceProposalSummary, PersistedDeviceProposal,
};
use pos_cloud::fleet::{FleetRow, FleetStore, FleetStoreError};
use pos_cloud::health::{self, TaskHealth, TaskHealthError, TaskHealthStore};
use pos_cloud::http::CloudApp;
use pos_cloud::orders::{StoreDirectory, orders_router};
use pos_cloud::people::{
    Assignment, AssignmentId, AssignmentStore, AssignmentStoreError, Employee, EmployeeId,
    EmployeeStore, EmployeeStoreError, EmployeeUpdate, NewAssignment, NewEmployee, NewRoleTemplate,
    RoleTemplate, RoleTemplateId, RoleTemplateStore, RoleTemplateStoreError, RoleTemplateUpdate,
    is_known_permission, permission_catalogue,
};
use pos_cloud::qr::{TableTokenSecret, mint_table_token};
use pos_cloud::qr_http::qr_router;
use pos_cloud::reconcile::{ReconcileError, ReconcileStore};
use pos_cloud::registry::{
    BrandRecord, DeviceRecord, EntityStatus, RegistryStore, RegistryStoreError, StoreRecord,
    TenantRecord,
};
use pos_cloud::relay::{
    OrderQueueId, OrderQueueStore, OrderRecord, OrderRelay, OrderStatus, PendingOrder,
    QueuedOrderPayload, StoreOutcome, orders_sync_router_with_cap,
};
use pos_cloud::translations::{TranslationGrid, TranslationStore, TranslationStoreError};
use pos_cloud::webhook::{
    PersistedWebhook, WebhookEndpointId, WebhookEndpointStore, WebhookStoreError, WebhookSummary,
};
use pos_cloud::{Cloud, IngestOutcome, http};
use pos_contract_tests::fixtures;
use pos_core::activation::{ActivationCode, CodeStatus};
use pos_fakes::vendors::{known_menu_item, unknown_menu_item};
use pos_fakes::{FakeClock, FakeIntake, FakeStore};
use pos_ports::PortError;
use pos_proto::BusinessDate;
use pos_proto::enums::SalesChannel;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{ConfigVersionId, DeviceId, EventId, MenuItemId, StoreId, TableId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

/// The instant `clock()` is fixed at, in milliseconds and in seconds — the second form is what the
/// TOTP code the admin tests submit is computed for.
const NOW_MS: i64 = 1_700_000_000_000;
const NOW_UNIX_SECS: u64 = 1_700_000_000;
/// The obviously-fake super-admin secrets the admin tests use; never real credentials.
const ADMIN_PASSWORD: &str = "a-strong-admin-passphrase";
const ADMIN_TOTP_SEED: &[u8] = b"12345678901234567890123456789012";

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0x0ADA))
}

fn tenant() -> TenantId {
    TenantId::new(Ulid::from_u128(0x7E11A))
}

/// A low-entropy, obviously-fake secret so no real key material is committed.
const FAKE_SECRET: &str = "fakesecretfortestsonly";

/// A clock fixed well past the epoch, so an issued key (with no expiry) is live.
fn clock() -> FakeClock {
    FakeClock::new(Timestamp::from_milliseconds_since_epoch(NOW_MS).expect("valid"))
}

/// A run of activation events, re-dated onto `business_date` (`YYYY-MM-DD`).
fn dated(
    first_seed: u32,
    count: u32,
    year: i16,
    month: u8,
    day: u8,
) -> Vec<EventEnvelope<RawPayload>> {
    let date = BusinessDate::from_ymd(year, month, day).expect("a valid date");
    let mut events = fixtures::activations(store_id(), first_seed, count);
    for event in &mut events {
        event.business_date = date;
    }
    events
}

// --- In-memory collaborators for the router (the binary uses `store-postgres`) ------------------

/// The materialised rollup read model, keyed by `(tenant, store)` exactly as the real table.
#[derive(Clone, Default)]
struct FakeRollups {
    rows: Arc<Mutex<HashMap<(TenantId, StoreId), StoredRollups>>>,
}

impl RollupStore for FakeRollups {
    async fn load(&self, tenant: TenantId, store: StoreId) -> Result<StoredRollups, RollupError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .get(&(tenant, store))
            .cloned()
            .unwrap_or_default())
    }

    async fn save(
        &self,
        tenant: TenantId,
        store: StoreId,
        rollups: &StoredRollups,
    ) -> Result<(), RollupError> {
        self.rows
            .lock()
            .expect("lock")
            .insert((tenant, store), rollups.clone());
        Ok(())
    }
}

/// The API-key store the bearer check consults, keyed by the public id.
#[derive(Clone, Default)]
struct FakeKeys {
    rows: Arc<Mutex<HashMap<ApiKeyId, StoredApiKey>>>,
}

impl FakeKeys {
    fn insert(&self, key: StoredApiKey) {
        self.rows.lock().expect("lock").insert(key.id, key);
    }
}

impl ApiKeyStore for FakeKeys {
    async fn lookup(&self, id: ApiKeyId) -> Result<Option<StoredApiKey>, ApiKeyStoreError> {
        Ok(self.rows.lock().expect("lock").get(&id).cloned())
    }
}

impl ApiKeyAdminStore for FakeKeys {
    async fn insert(&self, key: &StoredApiKey) -> Result<(), ApiKeyStoreError> {
        self.rows.lock().expect("lock").insert(key.id, key.clone());
        Ok(())
    }

    async fn list_for_tenant(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<ApiKeySummary>, ApiKeyStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .values()
            .filter(|key| key.tenant_id == tenant_id)
            .map(|key| ApiKeySummary {
                id: key.id.to_string(),
                scopes: key.scope_wire_names(),
                revoked: key.revoked,
                expires_at_ms: key.expires_at_ms(),
            })
            .collect())
    }

    async fn revoke(&self, id: ApiKeyId) -> Result<bool, ApiKeyStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        match rows.get_mut(&id) {
            Some(key) if !key.revoked => {
                key.revoked = true;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

/// Issues a key for `tenant_id` with `scopes` into `keys`, and returns the one-time token to present.
///
/// Each call mints a distinct id, so a test issuing more than one key does not have the second
/// silently overwrite the first in the fake's map.
fn issue_key(keys: &FakeKeys, tenant_id: TenantId, scopes: &[Scope]) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(0x00A1_1CE0);
    let id = ApiKeyId::new(Ulid::from_u128(u128::from(
        NEXT_ID.fetch_add(1, Ordering::Relaxed),
    )));
    let (stored, token) = issue(
        id,
        tenant_id,
        scopes.iter().copied().collect(),
        FAKE_SECRET,
        None,
    );
    keys.insert(stored);
    token
}

/// The super-admin store the `/admin` login and session guard consult, keyed to the one super-admin.
/// A stored session row, keyed in the table by `SHA-256(token)`: its sliding expiry, the absolute cap
/// and idle window that drive the slide, the id of the admin it belongs to (`None` for a legacy
/// session), and the client details captured for the admin's own session list.
#[derive(Clone)]
struct SessionRow {
    created_at: Timestamp,
    expires_at: Timestamp,
    absolute_expires_at: Option<Timestamp>,
    idle_ttl_ms: Option<i64>,
    admin_id: Option<String>,
    ip: Option<String>,
    user_agent: Option<String>,
}

type SessionRows = HashMap<[u8; 32], SessionRow>;

/// A stored invitation row in the integration fake, keyed for acceptance by its token hash.
#[derive(Clone)]
struct StoredInvite {
    id: String,
    email: String,
    name: String,
    role: AdminRole,
    invited_by: String,
    token_hash: [u8; 32],
    expires_at: Timestamp,
    accepted: bool,
}

/// A stored recovery code in the integration fake: its hash and whether it has been spent.
#[derive(Clone)]
struct StoredRecoveryCode {
    code_hash: [u8; 32],
    used: bool,
}

#[derive(Clone, Default)]
struct FakeAdmin {
    credential: Arc<Mutex<Option<SuperAdminCredential>>>,
    last_used_totp_step: Arc<Mutex<Option<u64>>>,
    sessions: Arc<Mutex<SessionRows>>,
    admin_users: Arc<Mutex<Vec<AdminUser>>>,
    invites: Arc<Mutex<Vec<StoredInvite>>>,
    recovery_codes: Arc<Mutex<HashMap<String, Vec<StoredRecoveryCode>>>>,
}

impl FakeAdmin {
    fn provisioned(credential: SuperAdminCredential) -> Self {
        Self {
            credential: Arc::new(Mutex::new(Some(credential))),
            ..Self::default()
        }
    }
}

impl AdminStore for FakeAdmin {
    async fn load_credential(&self) -> Result<Option<AdminCredential>, AdminStoreError> {
        Ok(self
            .credential
            .lock()
            .expect("lock")
            .clone()
            .map(|credential| AdminCredential {
                credential,
                last_used_totp_step: *self.last_used_totp_step.lock().expect("lock"),
            }))
    }

    async fn provision_credential(
        &self,
        password_phc: String,
        totp_secret: Vec<u8>,
    ) -> Result<bool, AdminStoreError> {
        let mut slot = self.credential.lock().expect("lock");
        if slot.is_some() {
            return Ok(false);
        }
        *slot = Some(SuperAdminCredential::new(
            password_phc,
            TotpSecret::new(totp_secret),
        ));
        Ok(true)
    }

    async fn rotate_totp_secret(&self, secret: Vec<u8>) -> Result<(), AdminStoreError> {
        let mut slot = self.credential.lock().expect("lock");
        if let Some(credential) = slot.take() {
            *slot = Some(credential.with_totp(TotpSecret::new(secret)));
        }
        *self.last_used_totp_step.lock().expect("lock") = None;
        Ok(())
    }

    async fn store_recovery_codes(
        &self,
        admin_id: &str,
        codes: Vec<NewRecoveryCode>,
    ) -> Result<(), AdminStoreError> {
        let rows = codes
            .into_iter()
            .map(|code| StoredRecoveryCode {
                code_hash: code.code_hash,
                used: false,
            })
            .collect();
        self.recovery_codes
            .lock()
            .expect("lock")
            .insert(admin_id.to_owned(), rows);
        Ok(())
    }

    async fn consume_recovery_code(
        &self,
        admin_id: &str,
        code_hash: [u8; 32],
        _now: Timestamp,
    ) -> Result<bool, AdminStoreError> {
        let mut map = self.recovery_codes.lock().expect("lock");
        let Some(rows) = map.get_mut(admin_id) else {
            return Ok(false);
        };
        match rows
            .iter_mut()
            .find(|row| !row.used && row.code_hash == code_hash)
        {
            Some(row) => {
                row.used = true;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn count_recovery_codes(&self, admin_id: &str) -> Result<u64, AdminStoreError> {
        let map = self.recovery_codes.lock().expect("lock");
        let unused = map
            .get(admin_id)
            .map_or(0, |rows| rows.iter().filter(|row| !row.used).count());
        Ok(u64::try_from(unused).unwrap_or(u64::MAX))
    }

    async fn record_totp_step(&self, step: u64) -> Result<(), AdminStoreError> {
        let mut last = self.last_used_totp_step.lock().expect("lock");
        if last.is_none_or(|current| step > current) {
            *last = Some(step);
        }
        Ok(())
    }

    async fn create_session(&self, session: NewAdminSession) -> Result<(), AdminStoreError> {
        self.sessions.lock().expect("lock").insert(
            session.token_hash,
            SessionRow {
                created_at: session.created_at,
                expires_at: session.expires_at,
                absolute_expires_at: Some(session.absolute_expires_at),
                idle_ttl_ms: Some(session.idle_ttl_ms),
                admin_id: session.admin_id,
                ip: session.ip,
                user_agent: session.user_agent,
            },
        );
        Ok(())
    }

    async fn session_is_valid(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<bool, AdminStoreError> {
        // A pure read — no sliding, as the poll's SQL is.
        Ok(self
            .sessions
            .lock()
            .expect("lock")
            .get(&token_hash)
            .is_some_and(|row| row.expires_at > now))
    }

    async fn session_admin(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<Option<LiveSession>, AdminStoreError> {
        let mut sessions = self.sessions.lock().expect("lock");
        let Some(row) = sessions
            .get_mut(&token_hash)
            .filter(|row| row.expires_at > now)
        else {
            return Ok(None);
        };
        // Slide the idle TTL up to the absolute cap, as the guard's SQL does; a legacy row is left as
        // it is.
        if let (Some(cap), Some(idle_ms)) = (row.absolute_expires_at, row.idle_ttl_ms) {
            let slid = Timestamp::from_milliseconds_since_epoch(
                now.as_milliseconds_since_epoch().saturating_add(idle_ms),
            )
            .unwrap_or(now);
            row.expires_at = if slid <= cap { slid } else { cap };
        }
        Ok(Some(LiveSession {
            admin_id: row.admin_id.clone(),
        }))
    }

    async fn revoke_session(&self, token_hash: [u8; 32]) -> Result<(), AdminStoreError> {
        self.sessions.lock().expect("lock").remove(&token_hash);
        Ok(())
    }

    async fn list_admin_sessions(
        &self,
        admin_id: &str,
        now: Timestamp,
    ) -> Result<Vec<SessionSummary>, AdminStoreError> {
        let mut summaries: Vec<SessionSummary> = self
            .sessions
            .lock()
            .expect("lock")
            .iter()
            .filter(|(_, row)| row.admin_id.as_deref() == Some(admin_id) && row.expires_at > now)
            .map(|(token_hash, row)| SessionSummary {
                token_hash: *token_hash,
                ip: row.ip.clone(),
                user_agent: row.user_agent.clone(),
                created_at: row.created_at,
                expires_at: row.expires_at,
            })
            .collect();
        summaries.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.token_hash.cmp(&b.token_hash))
        });
        Ok(summaries)
    }

    async fn revoke_admin_session(
        &self,
        admin_id: &str,
        token_hash: [u8; 32],
    ) -> Result<bool, AdminStoreError> {
        let mut sessions = self.sessions.lock().expect("lock");
        if sessions
            .get(&token_hash)
            .is_some_and(|row| row.admin_id.as_deref() == Some(admin_id))
        {
            sessions.remove(&token_hash);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn revoke_other_admin_sessions(
        &self,
        admin_id: &str,
        except_token_hash: [u8; 32],
    ) -> Result<u64, AdminStoreError> {
        let mut sessions = self.sessions.lock().expect("lock");
        let before = sessions.len();
        sessions.retain(|token_hash, row| {
            row.admin_id.as_deref() != Some(admin_id) || *token_hash == except_token_hash
        });
        Ok(u64::try_from(before - sessions.len()).unwrap_or(u64::MAX))
    }

    async fn create_admin_user(&self, user: NewAdminUser) -> Result<bool, AdminStoreError> {
        let mut users = self.admin_users.lock().expect("lock");
        if users
            .iter()
            .any(|existing| existing.email.eq_ignore_ascii_case(&user.email))
        {
            return Ok(false);
        }
        users.push(AdminUser {
            id: user.id,
            email: user.email,
            name: user.name,
            role: user.role,
            status: AdminStatus::Active,
        });
        Ok(true)
    }

    async fn list_admin_users(&self) -> Result<Vec<AdminUser>, AdminStoreError> {
        Ok(self.admin_users.lock().expect("lock").clone())
    }

    async fn get_admin_user(&self, id: &str) -> Result<Option<AdminUser>, AdminStoreError> {
        Ok(self
            .admin_users
            .lock()
            .expect("lock")
            .iter()
            .find(|user| user.id == id)
            .cloned())
    }

    async fn find_admin_user_by_email(
        &self,
        email: &str,
    ) -> Result<Option<AdminUser>, AdminStoreError> {
        Ok(self
            .admin_users
            .lock()
            .expect("lock")
            .iter()
            .find(|user| user.email.eq_ignore_ascii_case(email))
            .cloned())
    }

    async fn set_admin_user_role(
        &self,
        id: &str,
        role: AdminRole,
    ) -> Result<bool, AdminStoreError> {
        let mut users = self.admin_users.lock().expect("lock");
        match users.iter_mut().find(|user| user.id == id) {
            Some(user) => {
                user.role = role;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn set_admin_user_status(
        &self,
        id: &str,
        status: AdminStatus,
    ) -> Result<bool, AdminStoreError> {
        let mut users = self.admin_users.lock().expect("lock");
        match users.iter_mut().find(|user| user.id == id) {
            Some(user) => {
                user.status = status;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn count_active_owners(&self) -> Result<u64, AdminStoreError> {
        let count = self
            .admin_users
            .lock()
            .expect("lock")
            .iter()
            .filter(|user| user.role == AdminRole::Owner && user.status == AdminStatus::Active)
            .count();
        Ok(u64::try_from(count).unwrap_or(u64::MAX))
    }

    async fn create_invite(&self, invite: NewAdminInvite) -> Result<(), AdminStoreError> {
        self.invites.lock().expect("lock").push(StoredInvite {
            id: invite.id,
            email: invite.email,
            name: invite.name,
            role: invite.role,
            invited_by: invite.invited_by,
            token_hash: invite.token_hash,
            expires_at: invite.expires_at,
            accepted: false,
        });
        Ok(())
    }

    async fn find_pending_invite_by_token(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<Option<AdminInvite>, AdminStoreError> {
        Ok(self
            .invites
            .lock()
            .expect("lock")
            .iter()
            .find(|invite| {
                invite.token_hash == token_hash && !invite.accepted && invite.expires_at > now
            })
            .map(fake_invite_to_domain))
    }

    async fn mark_invite_accepted(
        &self,
        id: &str,
        _accepted_at: Timestamp,
    ) -> Result<bool, AdminStoreError> {
        let mut invites = self.invites.lock().expect("lock");
        match invites
            .iter_mut()
            .find(|invite| invite.id == id && !invite.accepted)
        {
            Some(invite) => {
                invite.accepted = true;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn list_pending_invites(
        &self,
        now: Timestamp,
    ) -> Result<Vec<AdminInvite>, AdminStoreError> {
        Ok(self
            .invites
            .lock()
            .expect("lock")
            .iter()
            .filter(|invite| !invite.accepted && invite.expires_at > now)
            .map(fake_invite_to_domain)
            .collect())
    }

    async fn revoke_invite(&self, id: &str) -> Result<bool, AdminStoreError> {
        let mut invites = self.invites.lock().expect("lock");
        let before = invites.len();
        invites.retain(|invite| invite.id != id || invite.accepted);
        Ok(invites.len() != before)
    }
}

/// Projects a stored fake invite into the domain [`AdminInvite`] (no token crosses the boundary).
fn fake_invite_to_domain(invite: &StoredInvite) -> AdminInvite {
    AdminInvite {
        id: invite.id.clone(),
        email: invite.email.clone(),
        name: invite.name.clone(),
        role: invite.role,
        invited_by: invite.invited_by.clone(),
        accepted: invite.accepted,
    }
}

/// A super-admin provisioned with a known password and TOTP seed, for the login tests.
fn provisioned_admin() -> FakeAdmin {
    let salt = SaltString::encode_b64(b"cloud-admin-test-salt").expect("salt");
    let phc = hash_password(ADMIN_PASSWORD, &salt).expect("hash");
    FakeAdmin::provisioned(SuperAdminCredential::new(
        phc,
        TotpSecret::new(ADMIN_TOTP_SEED.to_vec()),
    ))
}

/// The current valid TOTP code for the provisioned admin at `clock()`'s instant.
fn admin_totp_code() -> String {
    code_at(
        &TotpSecret::new(ADMIN_TOTP_SEED.to_vec()),
        NOW_UNIX_SECS,
        DIGITS,
    )
}

/// A recorded liveness contact: the version the store reported holding (or `None`) and the contact
/// instant in Unix ms — mirroring the `store_liveness` row's `(config_version_held, last_seen_at)`.
type RecordedSeen = (Option<ConfigVersionId>, i64);

/// The config-tree store, keyed by `(tenant, store)` exactly as the real table. `seen` mirrors the
/// `store_liveness` upsert so a test can assert the config pull recorded the store's contact.
#[derive(Clone, Default)]
struct FakeConfigTrees {
    rows: Arc<Mutex<HashMap<(TenantId, StoreId), ConfigTreeState>>>,
    seen: Arc<Mutex<HashMap<(TenantId, StoreId), RecordedSeen>>>,
}

impl FakeConfigTrees {
    /// The `(held_version, seen_at_ms)` last recorded for a store, or `None` if it never checked in.
    fn recorded_seen(&self, tenant: TenantId, store: StoreId) -> Option<RecordedSeen> {
        self.seen
            .lock()
            .expect("lock")
            .get(&(tenant, store))
            .copied()
    }
}

impl ConfigTreeStore for FakeConfigTrees {
    async fn load(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> Result<Option<ConfigTreeState>, ConfigStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .get(&(tenant, store))
            .cloned())
    }

    async fn save(
        &self,
        tenant: TenantId,
        store: StoreId,
        state: &ConfigTreeState,
    ) -> Result<(), ConfigStoreError> {
        self.rows
            .lock()
            .expect("lock")
            .insert((tenant, store), state.clone());
        Ok(())
    }

    async fn record_store_seen(
        &self,
        tenant: TenantId,
        store: StoreId,
        held_version: Option<ConfigVersionId>,
        seen_at: Timestamp,
    ) -> Result<(), ConfigStoreError> {
        self.seen.lock().expect("lock").insert(
            (tenant, store),
            (held_version, seen_at.as_milliseconds_since_epoch()),
        );
        Ok(())
    }

    async fn record_store_heartbeat(
        &self,
        tenant: TenantId,
        store: StoreId,
        seen_at: Timestamp,
    ) -> Result<(), ConfigStoreError> {
        // A heartbeat advances last_seen only, keeping any held version a prior pull recorded.
        self.seen
            .lock()
            .expect("lock")
            .entry((tenant, store))
            .and_modify(|entry| entry.1 = seen_at.as_milliseconds_since_epoch())
            .or_insert((None, seen_at.as_milliseconds_since_epoch()));
        Ok(())
    }
}

/// The webhook-endpoint store, a flat list exactly as `fetch_enabled`/`list_for_tenant` read the
/// real table.
#[derive(Clone, Default)]
struct FakeWebhooks {
    rows: Arc<Mutex<Vec<PersistedWebhook>>>,
}

impl WebhookEndpointStore for FakeWebhooks {
    async fn insert(&self, endpoint: &PersistedWebhook) -> Result<(), WebhookStoreError> {
        self.rows.lock().expect("lock").push(endpoint.clone());
        Ok(())
    }

    async fn list_for_tenant(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<WebhookSummary>, WebhookStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant_id)
            .map(|row| WebhookSummary {
                id: row.id.to_string(),
                store_id: row.store_id.to_string(),
                url: row.url.clone(),
                cursor: row.cursor.as_ref().map(ToString::to_string),
                disabled: row.disabled,
            })
            .collect())
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        id: WebhookEndpointId,
    ) -> Result<bool, WebhookStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        let before = rows.len();
        rows.retain(|row| !(row.tenant_id == tenant_id && row.id == id));
        Ok(rows.len() != before)
    }

    async fn load_enabled(&self) -> Result<Vec<PersistedWebhook>, WebhookStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| !row.disabled)
            .cloned()
            .collect())
    }

    async fn save_cursor(
        &self,
        id: WebhookEndpointId,
        cursor: EventId,
    ) -> Result<(), WebhookStoreError> {
        for row in self.rows.lock().expect("lock").iter_mut() {
            if row.id == id {
                row.cursor = Some(cursor);
            }
        }
        Ok(())
    }

    async fn set_disabled(
        &self,
        id: WebhookEndpointId,
        disabled: bool,
    ) -> Result<(), WebhookStoreError> {
        for row in self.rows.lock().expect("lock").iter_mut() {
            if row.id == id {
                row.disabled = disabled;
            }
        }
        Ok(())
    }
}

/// The full application state type over the fakes.
type FakeApp =
    CloudApp<FakeStore, FakeRollups, FakeKeys, FakeClock, FakeAdmin, FakeConfigTrees, FakeWebhooks>;

/// Builds an application state over the fakes, with an unprovisioned admin (the `/admin` routes are
/// reachable but no login can succeed) — enough for the ingest and `/v1` tests.
fn app(cloud: Cloud<FakeStore>, rollups: FakeRollups, keys: FakeKeys) -> FakeApp {
    app_with_admin(cloud, rollups, keys, FakeAdmin::default())
}

/// Builds an application state over the fakes with a specific admin store, for the `/admin` tests.
fn app_with_admin(
    cloud: Cloud<FakeStore>,
    rollups: FakeRollups,
    keys: FakeKeys,
    admin: FakeAdmin,
) -> FakeApp {
    app_full(cloud, rollups, keys, admin, FakeConfigTrees::default())
}

/// Builds an application state over the fakes with specific admin and config-tree stores, for the
/// config-authoring tests that inspect the persisted tree.
fn app_full(
    cloud: Cloud<FakeStore>,
    rollups: FakeRollups,
    keys: FakeKeys,
    admin: FakeAdmin,
    config_trees: FakeConfigTrees,
) -> FakeApp {
    app_all(
        cloud,
        rollups,
        keys,
        admin,
        config_trees,
        FakeWebhooks::default(),
    )
}

/// Builds an application state over the fakes with a specific webhook store too, for the webhook
/// admin-route tests that inspect what was registered.
fn app_all(
    cloud: Cloud<FakeStore>,
    rollups: FakeRollups,
    keys: FakeKeys,
    admin: FakeAdmin,
    config_trees: FakeConfigTrees,
    webhooks: FakeWebhooks,
) -> FakeApp {
    CloudApp::new(cloud, rollups, keys, clock(), admin, config_trees, webhooks)
}

/// A GET request for `uri`, optionally carrying a `Bearer` token.
fn get(uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(uri);
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("build the request")
}

/// A GET request for `uri` carrying `cookie` as the `Cookie` header.
fn get_with_cookie(uri: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("cookie", cookie)
        .body(Body::empty())
        .expect("build the request")
}

/// A POST request for `uri` with a JSON body.
fn post_json(uri: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(body).expect("serialise the body"),
        ))
        .expect("build the request")
}

/// A POST request for `uri` with a JSON body and a `Bearer` token — a store client call.
fn post_json_bearer(uri: &str, body: &serde_json::Value, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(
            serde_json::to_vec(body).expect("serialise the body"),
        ))
        .expect("build the request")
}

/// A PUT request for `uri` with a JSON body and no cookie — for the guard tests.
fn put_json(uri: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(body).expect("serialise the body"),
        ))
        .expect("build the request")
}

/// The `name=value` pair from a `Set-Cookie` header value (its first `;`-separated segment).
fn cookie_pair(set_cookie: &str) -> &str {
    set_cookie.split(';').next().unwrap_or(set_cookie)
}

/// A POST request for `uri` with a JSON body and a `Cookie` header.
fn post_with_cookie(uri: &str, body: &serde_json::Value, cookie: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .body(Body::from(
            serde_json::to_vec(body).expect("serialise the body"),
        ))
        .expect("build the request")
}

/// A PUT request for `uri` with a JSON body and a `Cookie` header.
fn put_with_cookie(uri: &str, body: &serde_json::Value, cookie: &str) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .body(Body::from(
            serde_json::to_vec(body).expect("serialise the body"),
        ))
        .expect("build the request")
}

/// A PATCH request for `uri` with a JSON body and a `Cookie` header.
fn patch_with_cookie(uri: &str, body: &serde_json::Value, cookie: &str) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .body(Body::from(
            serde_json::to_vec(body).expect("serialise the body"),
        ))
        .expect("build the request")
}

/// A DELETE request for `uri` carrying a `Cookie` header.
fn delete_with_cookie(uri: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("cookie", cookie)
        .body(Body::empty())
        .expect("build the request")
}

/// Logs the provisioned admin in and returns the session cookie pair.
async fn admin_cookie(router: &axum::Router) -> String {
    let body = serde_json::json!({ "password": ADMIN_PASSWORD, "totp_code": admin_totp_code() });
    let login = router
        .clone()
        .oneshot(post_json("/admin/login", &body))
        .await
        .expect("route the login");
    assert_eq!(login.status(), StatusCode::NO_CONTENT, "the login succeeds");
    cookie_pair(
        login
            .headers()
            .get("set-cookie")
            .expect("a session cookie")
            .to_str()
            .expect("ascii"),
    )
    .to_owned()
}

/// Reads a response body as JSON.
async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("parse the body as JSON")
}

// --- The application spine, exercised directly (no HTTP) ----------------------------------------

#[tokio::test]
async fn ingest_is_idempotent_by_event_id() {
    let cloud = Cloud::new(FakeStore::new());
    let events = fixtures::activations(store_id(), 1, 4);

    let first = cloud.ingest(&events).await.expect("first ingest");
    assert_eq!(
        first,
        IngestOutcome {
            appended: 4,
            duplicates: 0
        }
    );

    // At-least-once delivery replays this batch; the cloud must store nothing and report duplicates.
    let second = cloud.ingest(&events).await.expect("replayed ingest");
    assert_eq!(
        second,
        IngestOutcome {
            appended: 0,
            duplicates: 4
        }
    );

    let total: u64 = cloud
        .daily_rollups(store_id())
        .await
        .expect("rollups")
        .iter()
        .map(|day| day.total_events)
        .sum();
    assert_eq!(total, 4, "a replay must not grow the log");
}

#[tokio::test]
async fn rollups_fold_events_by_trading_day_and_type() {
    let cloud = Cloud::new(FakeStore::new());
    cloud
        .ingest(&dated(1, 3, 2026, 3, 15))
        .await
        .expect("march ingest");
    cloud
        .ingest(&dated(100, 2, 2026, 7, 1))
        .await
        .expect("july ingest");

    let rollups = cloud.daily_rollups(store_id()).await.expect("rollups");
    assert_eq!(rollups.len(), 2, "two distinct trading days");

    let march = &rollups[0];
    assert_eq!(march.business_date, "2026-03-15");
    assert_eq!(march.total_events, 3);
    // Every activation carries the same event type, so it is the only key, counted three times.
    assert_eq!(
        march.by_type.get("device.activation.completed"),
        Some(&3),
        "counts are folded per event type"
    );

    let july = &rollups[1];
    assert_eq!(july.business_date, "2026-07-01");
    assert_eq!(july.total_events, 2);
}

// --- The HTTP surface ---------------------------------------------------------------------------

#[tokio::test]
async fn the_ingest_endpoint_accepts_a_batch_and_health_answers() {
    let events = fixtures::activations(store_id(), 1, 5);
    let body = serde_json::to_vec(&events).expect("serialise the batch");

    let response = http::router(app(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
    ))
    .oneshot(
        Request::builder()
            .method("POST")
            .uri("/internal/ingest")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("build the request"),
    )
    .await
    .expect("route the request");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    let outcome: IngestOutcome = serde_json::from_slice(&bytes).expect("parse the outcome");
    assert_eq!(
        outcome,
        IngestOutcome {
            appended: 5,
            duplicates: 0
        }
    );

    let health = http::router(app(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
    ))
    .oneshot(get("/health", None))
    .await
    .expect("route health");
    assert_eq!(health.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_v1_rollups_endpoint_answers_from_the_materialised_store_for_an_authorised_key() {
    let cloud = Cloud::new(FakeStore::new());
    cloud
        .ingest(&dated(1, 3, 2026, 3, 15))
        .await
        .expect("ingest");

    // Materialise the rollup the way the projector does, for this store's tenant.
    let rollups = FakeRollups::default();
    project(cloud.store(), &rollups, tenant(), store_id())
        .await
        .expect("project the rollup");

    // A key for the store's tenant, scoped to read rollups.
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::ReadRollups]);
    let ulid = store_id().as_ulid().to_string();

    let response = http::router(app(cloud, rollups, keys))
        .oneshot(get(
            &format!("/v1/stores/{ulid}/rollups/daily"),
            Some(&token),
        ))
        .await
        .expect("route the request");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    let rollups: serde_json::Value = serde_json::from_slice(&bytes).expect("parse the rollups");
    let days = rollups.as_array().expect("an array of days");
    assert_eq!(days.len(), 1);
    assert_eq!(days[0]["business_date"], "2026-03-15");
    assert_eq!(days[0]["total_events"], 3);
}

#[tokio::test]
async fn a_request_without_a_key_is_unauthorised() {
    let ulid = store_id().as_ulid().to_string();
    let response = http::router(app(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
    ))
    .oneshot(get(&format!("/v1/stores/{ulid}/rollups/daily"), None))
    .await
    .expect("route the request");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a /v1 data route is closed without a key"
    );
    assert_eq!(
        response
            .headers()
            .get("www-authenticate")
            .expect("the scheme is advertised"),
        "Bearer"
    );
}

#[tokio::test]
async fn a_key_without_the_scope_is_forbidden() {
    // A valid key, but granted only ManageWebhooks — it may not read rollups.
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::ManageWebhooks]);
    let ulid = store_id().as_ulid().to_string();

    let response = http::router(app(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        keys,
    ))
    .oneshot(get(
        &format!("/v1/stores/{ulid}/rollups/daily"),
        Some(&token),
    ))
    .await
    .expect("route the request");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_key_for_another_tenant_reads_no_rollups() {
    // Tenant A materialises a rollup for the store.
    let cloud = Cloud::new(FakeStore::new());
    cloud
        .ingest(&dated(1, 3, 2026, 3, 15))
        .await
        .expect("ingest");
    let rollups = FakeRollups::default();
    project(cloud.store(), &rollups, tenant(), store_id())
        .await
        .expect("project");

    // A key belonging to a *different* tenant, correctly scoped, asks for the same store id.
    let other_tenant = TenantId::new(Ulid::from_u128(0xB0B));
    let keys = FakeKeys::default();
    let token = issue_key(&keys, other_tenant, &[Scope::ReadRollups]);
    let ulid = store_id().as_ulid().to_string();

    let response = http::router(app(cloud, rollups, keys))
        .oneshot(get(
            &format!("/v1/stores/{ulid}/rollups/daily"),
            Some(&token),
        ))
        .await
        .expect("route the request");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a valid key never errors — it just sees nothing outside its tenant"
    );
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    let rollups: serde_json::Value = serde_json::from_slice(&bytes).expect("parse the rollups");
    assert_eq!(
        rollups.as_array().expect("an array").len(),
        0,
        "the tenant comes from the grant, so another tenant's store reads back empty, not leaked"
    );
}

#[tokio::test]
async fn a_malformed_store_id_is_a_bad_request() {
    // Present a valid, scoped key so the request reaches the store-id parse rather than stopping at
    // authentication.
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::ReadRollups]);

    let response = http::router(app(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        keys,
    ))
    .oneshot(get("/v1/stores/not-a-ulid/rollups/daily", Some(&token)))
    .await
    .expect("route the request");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_openapi_document_is_served() {
    let response = http::router(app(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
    ))
    .oneshot(get("/v1/openapi.json", None))
    .await
    .expect("route the request");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    let document: serde_json::Value = serde_json::from_slice(&bytes).expect("parse the document");
    assert_eq!(document["openapi"], "3.1.0");
    assert!(
        document["paths"]["/v1/stores/{store_id}/rollups/daily"].is_object(),
        "the rollups path is described"
    );
    assert!(
        document["components"]["securitySchemes"]["api_key"].is_object(),
        "the bearer security scheme is declared in the generated document"
    );
    assert!(
        document["paths"]["/admin/login"].is_null(),
        "the admin surface is not part of the public OpenAPI contract"
    );
}

// --- The interactive super-admin surface (`/admin`) ---------------------------------------------

#[tokio::test]
async fn a_correct_admin_login_sets_a_host_only_cookie_the_guard_then_accepts() {
    let admin = provisioned_admin();
    let router = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin,
    ));

    // Log in with the correct password and current code.
    let body = serde_json::json!({ "password": ADMIN_PASSWORD, "totp_code": admin_totp_code() });
    let response = router
        .clone()
        .oneshot(post_json("/admin/login", &body))
        .await
        .expect("route the login");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .expect("a session cookie is set")
        .to_str()
        .expect("ascii");
    assert!(
        set_cookie.starts_with("__Host-pos_admin_session="),
        "the host-only session cookie is issued: {set_cookie}"
    );
    assert!(
        !set_cookie.contains("Domain"),
        "a Domain attribute would leak the session across subdomains: {set_cookie}"
    );
    assert!(set_cookie.contains("Secure") && set_cookie.contains("HttpOnly"));

    // The guard accepts the cookie the login issued.
    let session = router
        .oneshot(get_with_cookie("/admin/session", cookie_pair(set_cookie)))
        .await
        .expect("route the session check");
    assert_eq!(
        session.status(),
        StatusCode::NO_CONTENT,
        "the issued session authenticates the guard"
    );
}

#[tokio::test]
async fn the_admin_session_guard_refuses_a_request_without_a_cookie() {
    let response = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ))
    .oneshot(get("/admin/session", None))
    .await
    .expect("route the request");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "no session cookie means no admin session"
    );
}

#[tokio::test]
async fn a_wrong_admin_password_is_refused_and_sets_no_cookie() {
    let body =
        serde_json::json!({ "password": "not-the-password", "totp_code": admin_totp_code() });
    let response = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ))
    .oneshot(post_json("/admin/login", &body))
    .await
    .expect("route the login");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        response.headers().get("set-cookie").is_none(),
        "a refused login issues no session cookie"
    );
}

/// An obviously-fake first-boot setup token; never a real credential.
const SETUP_TOKEN: &str = "a-one-time-setup-token-abc123";

/// A router with first-boot enrolment enabled (or not, per `token`) over an *unprovisioned* admin,
/// returning the admin handle so a test can assert whether a credential got written.
fn setup_router(token: Option<&str>) -> (axum::Router, FakeAdmin) {
    let admin = FakeAdmin::default();
    let app = app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
    )
    .with_admin_setup_token(token.map(str::to_owned));
    (http::router(app), admin)
}

#[tokio::test]
async fn first_boot_setup_enrols_the_admin_then_refuses_a_second() {
    let (router, admin) = setup_router(Some(SETUP_TOKEN));
    let body = serde_json::json!({ "setup_token": SETUP_TOKEN, "password": ADMIN_PASSWORD });

    let response = router
        .clone()
        .oneshot(post_json("/admin/setup", &body))
        .await
        .expect("route the setup");
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "first-boot enrolment succeeds"
    );
    let enrolment = json_body(response).await;
    let uri = enrolment["otpauth_uri"].as_str().expect("an otpauth uri");
    assert!(
        uri.starts_with("otpauth://totp/Pizza4Ps:super-admin?secret="),
        "the enrolment carries a provisioning uri: {uri}"
    );
    assert!(
        uri.contains("algorithm=SHA1"),
        "the uri fixes HMAC-SHA1 — the algorithm every authenticator app computes (ADR-0034): {uri}"
    );
    assert!(
        enrolment["secret_base32"]
            .as_str()
            .is_some_and(|secret| !secret.is_empty()),
        "a base32 secret is returned for manual entry"
    );
    assert!(
        admin.load_credential().await.expect("load").is_some(),
        "a credential is now provisioned"
    );

    // A second enrolment is refused — first-boot is over, even with the right token.
    let again = router
        .oneshot(post_json("/admin/setup", &body))
        .await
        .expect("route the setup");
    assert_eq!(
        again.status(),
        StatusCode::CONFLICT,
        "a second enrolment against an existing admin is refused"
    );
}

#[tokio::test]
async fn setup_is_404_when_no_token_is_configured() {
    let (router, admin) = setup_router(None);
    let body = serde_json::json!({ "setup_token": "anything", "password": ADMIN_PASSWORD });
    let response = router
        .oneshot(post_json("/admin/setup", &body))
        .await
        .expect("route the setup");
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "setup is off when no token is configured"
    );
    assert!(
        admin.load_credential().await.expect("load").is_none(),
        "nothing was provisioned"
    );
}

#[tokio::test]
async fn setup_with_a_wrong_token_is_401_and_provisions_nothing() {
    let (router, admin) = setup_router(Some(SETUP_TOKEN));
    let body = serde_json::json!({ "setup_token": "the-wrong-token", "password": ADMIN_PASSWORD });
    let response = router
        .oneshot(post_json("/admin/setup", &body))
        .await
        .expect("route the setup");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a wrong setup token is refused"
    );
    assert!(
        admin.load_credential().await.expect("load").is_none(),
        "a refused setup provisions nothing"
    );
}

#[tokio::test]
async fn setup_with_a_short_password_is_422() {
    let (router, admin) = setup_router(Some(SETUP_TOKEN));
    let body = serde_json::json!({ "setup_token": SETUP_TOKEN, "password": "short" });
    let response = router
        .oneshot(post_json("/admin/setup", &body))
        .await
        .expect("route the setup");
    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "too short a password is refused before anything is written"
    );
    assert!(
        admin.load_credential().await.expect("load").is_none(),
        "a refused setup provisions nothing"
    );
}

#[tokio::test]
async fn logout_clears_the_cookie_and_revokes_the_session() {
    let router = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ));

    // Log in and capture the session cookie.
    let body = serde_json::json!({ "password": ADMIN_PASSWORD, "totp_code": admin_totp_code() });
    let login = router
        .clone()
        .oneshot(post_json("/admin/login", &body))
        .await
        .expect("route the login");
    let cookie = cookie_pair(
        login
            .headers()
            .get("set-cookie")
            .expect("cookie")
            .to_str()
            .expect("ascii"),
    )
    .to_owned();

    // Log out: the response clears the cookie...
    let logout = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/logout")
                .header("cookie", &cookie)
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("route the logout");
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    let cleared = logout
        .headers()
        .get("set-cookie")
        .expect("logout clears the cookie")
        .to_str()
        .expect("ascii");
    assert!(
        cleared.contains("Max-Age=0"),
        "the cookie is expired: {cleared}"
    );

    // ...and the revoked session no longer authenticates the guard.
    let session = router
        .oneshot(get_with_cookie("/admin/session", &cookie))
        .await
        .expect("route the session check");
    assert_eq!(
        session.status(),
        StatusCode::UNAUTHORIZED,
        "a revoked session is no longer accepted"
    );
}

// --- API-key provisioning (`/admin/api-keys`, behind the session guard) -------------------------

#[tokio::test]
async fn a_provisioned_key_authenticates_v1_then_stops_after_revoke() {
    let cloud = Cloud::new(FakeStore::new());
    cloud
        .ingest(&dated(1, 3, 2026, 3, 15))
        .await
        .expect("ingest");
    let rollups = FakeRollups::default();
    project(cloud.store(), &rollups, tenant(), store_id())
        .await
        .expect("project");
    let router = http::router(app_with_admin(
        cloud,
        rollups,
        FakeKeys::default(),
        provisioned_admin(),
    ));
    let cookie = admin_cookie(&router).await;

    // Provision a read_rollups key for the tenant.
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "scopes": ["read_rollups"],
    });
    let created = router
        .clone()
        .oneshot(post_with_cookie("/admin/api-keys", &body, &cookie))
        .await
        .expect("route the provisioning");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let token = created["token"]
        .as_str()
        .expect("a one-time token")
        .to_owned();
    let id = created["id"].as_str().expect("the key id").to_owned();
    assert!(
        token.starts_with("pos_"),
        "the token is the real value, shown once"
    );

    // The issued token authenticates a /v1 read for its tenant.
    let ulid = store_id().as_ulid().to_string();
    let read = router
        .clone()
        .oneshot(get(
            &format!("/v1/stores/{ulid}/rollups/daily"),
            Some(&token),
        ))
        .await
        .expect("route the read");
    assert_eq!(
        read.status(),
        StatusCode::OK,
        "the freshly issued key authenticates the public API"
    );

    // It appears in the tenant's listing, without any secret.
    let list = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/api-keys?tenant_id={}", tenant().as_ulid()),
            &cookie,
        ))
        .await
        .expect("route the list");
    assert_eq!(list.status(), StatusCode::OK);
    let list = json_body(list).await;
    let keys = list.as_array().expect("an array of summaries");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["id"], id);
    assert_eq!(keys[0]["scopes"][0], "read_rollups");
    assert!(
        keys[0].get("secret").is_none() && keys[0].get("secret_hash").is_none(),
        "a listing never carries the secret or its hash"
    );

    // Revoke it, and the same token no longer authenticates.
    let revoked = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/api-keys/{id}"),
            &cookie,
        ))
        .await
        .expect("route the revoke");
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    let read_after = router
        .oneshot(get(
            &format!("/v1/stores/{ulid}/rollups/daily"),
            Some(&token),
        ))
        .await
        .expect("route the read");
    assert_eq!(
        read_after.status(),
        StatusCode::UNAUTHORIZED,
        "a revoked key is refused"
    );
}

#[tokio::test]
async fn provisioning_without_a_session_is_unauthorised() {
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "scopes": ["read_rollups"],
    });
    let response = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ))
    .oneshot(post_json("/admin/api-keys", &body))
    .await
    .expect("route the request");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "provisioning is closed without an admin session"
    );
}

#[tokio::test]
async fn provisioning_with_an_unknown_scope_is_rejected() {
    let router = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ));
    let cookie = admin_cookie(&router).await;
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "scopes": ["not_a_real_scope"],
    });
    let response = router
        .oneshot(post_with_cookie("/admin/api-keys", &body, &cookie))
        .await
        .expect("route the request");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "an unknown scope name is a 400, never a silent no-op grant"
    );
}

// --- Config-tree admin authoring (`/admin/stores/{id}/config`, behind the session guard) --------

#[tokio::test]
async fn config_publish_composes_validates_and_reads_back_effective() {
    let router = http::router(app_full(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
        FakeConfigTrees::default(),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let base = format!("/admin/stores/{store_ulid}/config");

    // Author the tenant layer, then override one key at the store layer.
    let tenant_doc = serde_json::json!({ "currency_code": "VND", "tips_enabled": false });
    let published = router
        .clone()
        .oneshot(put_with_cookie(
            &format!("{base}/tenant?tenant_id={tenant_ulid}"),
            &tenant_doc,
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let published = json_body(published).await;
    assert!(
        published["config_version_id"].as_str().is_some(),
        "a successful publish returns the new version id"
    );

    let store_doc = serde_json::json!({ "tips_enabled": true });
    let published2 = router
        .clone()
        .oneshot(put_with_cookie(
            &format!("{base}/store?tenant_id={tenant_ulid}"),
            &store_doc,
            &cookie,
        ))
        .await
        .expect("route the second publish");
    assert_eq!(published2.status(), StatusCode::OK);

    // The effective document is the deep merge, most-specific winning.
    let effective = router
        .oneshot(get_with_cookie(
            &format!("{base}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(effective.status(), StatusCode::OK);
    assert_eq!(
        json_body(effective).await,
        serde_json::json!({ "currency_code": "VND", "tips_enabled": true }),
        "the store layer overrode the tenant layer"
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: publish two versions, list them, read one back for the diff \
              view, roll back, and assert the append-only restore — splitting it would duplicate the \
              multi-publish setup"
)]
async fn config_versions_list_read_back_and_roll_back_append_only() {
    let router = http::router(app_full(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
        FakeConfigTrees::default(),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let base = format!("/admin/stores/{store_ulid}/config");

    // Two published versions: v1 sets the currency, v2 overrides it at the store layer.
    let publish = |doc: serde_json::Value, level: &'static str| {
        let router = router.clone();
        let cookie = cookie.clone();
        let uri = format!("{base}/{level}?tenant_id={tenant_ulid}");
        async move {
            let response = router
                .oneshot(put_with_cookie(&uri, &doc, &cookie))
                .await
                .expect("route the publish");
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await["config_version_id"]
                .as_str()
                .expect("a version id")
                .to_owned()
        }
    };
    let v1 = publish(serde_json::json!({ "currency_code": "VND" }), "tenant").await;
    let _v2 = publish(serde_json::json!({ "currency_code": "SGD" }), "store").await;

    // The history lists both, newest first, with the latest flagged current.
    let versions = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{base}/versions?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the versions list");
    assert_eq!(versions.status(), StatusCode::OK);
    let versions = json_body(versions).await;
    let rows = versions.as_array().expect("array");
    assert_eq!(rows.len(), 2, "both published versions are listed");
    assert_eq!(rows[0]["current"], true, "the newest is current");
    assert_eq!(rows[1]["current"], false);
    assert!(
        rows[0]["at_ms"].as_i64().is_some(),
        "each version carries its instant"
    );

    // v1's effective is readable for the diff view.
    let v1_effective = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{base}/versions/{v1}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the version read");
    assert_eq!(v1_effective.status(), StatusCode::OK);
    assert_eq!(
        json_body(v1_effective).await,
        serde_json::json!({ "currency_code": "VND" }),
        "v1's effective is the tenant-only document"
    );

    // Roll back to v1: a new current version is appended, restoring v1's effective.
    let rollback = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("{base}/rollback?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "version_id": v1 }),
            &cookie,
        ))
        .await
        .expect("route the rollback");
    assert_eq!(rollback.status(), StatusCode::OK);
    let restored = json_body(rollback).await["config_version_id"]
        .as_str()
        .expect("a new version id")
        .to_owned();
    assert_ne!(
        restored, v1,
        "rollback appends a new version, never mutates history"
    );

    // The store's current effective is back to v1's, and the history now holds three versions.
    let effective = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{base}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the effective read");
    assert_eq!(
        json_body(effective).await,
        serde_json::json!({ "currency_code": "VND" }),
        "rollback restored v1's effective config"
    );
    let after = router
        .oneshot(get_with_cookie(
            &format!("{base}/versions?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the versions list");
    assert_eq!(
        json_body(after).await.as_array().expect("array").len(),
        3,
        "the rollback is a third, appended version"
    );
}

#[tokio::test]
async fn an_incoherent_config_is_rejected_with_violations() {
    let router = http::router(app_full(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
        FakeConfigTrees::default(),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // pay_first_enabled and tables_enabled are mutually exclusive (pos-core §10).
    let bad = serde_json::json!({ "pay_first_enabled": true, "tables_enabled": true });
    let response = router
        .oneshot(put_with_cookie(
            &format!("/admin/stores/{store_ulid}/config/store?tenant_id={tenant_ulid}"),
            &bad,
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(response).await;
    assert!(
        !body["violations"]
            .as_array()
            .expect("violations array")
            .is_empty(),
        "the rejection names the violated rule(s)"
    );
}

#[tokio::test]
async fn config_routes_require_a_session() {
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();
    let router = http::router(app_full(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
        FakeConfigTrees::default(),
    ));

    let publish = router
        .clone()
        .oneshot(put_json(
            &format!("/admin/stores/{store_ulid}/config/tenant?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "a": 1 }),
        ))
        .await
        .expect("route the publish");
    assert_eq!(publish.status(), StatusCode::UNAUTHORIZED);

    // And an unpublished store, once past the guard, reads 404 rather than an empty 200.
    let cookie = admin_cookie(&router).await;
    let read = router
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store_ulid}/config?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(
        read.status(),
        StatusCode::NOT_FOUND,
        "a store with no published config has no effective document"
    );
}

// --- Store-facing config sync (`GET /sync/stores/{id}/config`, bearer + read_config) ------------

/// Publishes one config version through the admin route, then returns the router, the `read_config`
/// bearer, the store ULID, and the published version id — the fixture the sync tests share.
async fn published_config(keys: &FakeKeys) -> (axum::Router, String, String, String) {
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        keys.clone(),
        provisioned_admin(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let doc = serde_json::json!({ "currency_code": "VND", "tips_enabled": false });
    let published = router
        .clone()
        .oneshot(put_with_cookie(
            &format!("/admin/stores/{store_ulid}/config/store?tenant_id={tenant_ulid}"),
            &doc,
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let version = json_body(published).await["config_version_id"]
        .as_str()
        .expect("a version id")
        .to_owned();
    let token = issue_key(keys, tenant(), &[Scope::ReadConfig]);
    (router, token, store_ulid, version)
}

#[tokio::test]
async fn config_sync_serves_an_update_then_reports_up_to_date() {
    let keys = FakeKeys::default();
    let (router, token, store_ulid, version) = published_config(&keys).await;

    // A store holding nothing gets an update to apply (a full snapshot for a first sync).
    let fresh = router
        .clone()
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config"),
            Some(&token),
        ))
        .await
        .expect("route the sync");
    assert_eq!(fresh.status(), StatusCode::OK);
    let body = json_body(fresh).await;
    assert_eq!(
        body["status"], "update",
        "a store with nothing gets an update"
    );
    assert!(
        !body["update"].is_null(),
        "the update carries a snapshot/delta"
    );

    // A store already holding the current version is told it is up to date.
    let current = router
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config?held_version={version}"),
            Some(&token),
        ))
        .await
        .expect("route the sync");
    assert_eq!(current.status(), StatusCode::OK);
    assert_eq!(
        json_body(current).await["status"],
        "up_to_date",
        "holding the current version, the store applies nothing"
    );
}

#[tokio::test]
async fn config_sync_records_store_liveness() {
    // The config pull is the fleet-liveness signal (ADR-0068): each pull records the store's contact
    // and the version it reported holding, without altering the sync response.
    let keys = FakeKeys::default();
    let config_trees = FakeConfigTrees::default();
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        keys.clone(),
        provisioned_admin(),
        config_trees.clone(),
        FakeWebhooks::default(),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let doc = serde_json::json!({ "currency_code": "VND", "tips_enabled": false });
    let published = router
        .clone()
        .oneshot(put_with_cookie(
            &format!("/admin/stores/{store_ulid}/config/store?tenant_id={tenant_ulid}"),
            &doc,
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let version = json_body(published).await["config_version_id"]
        .as_str()
        .expect("a version id")
        .to_owned();
    let token = issue_key(&keys, tenant(), &[Scope::ReadConfig]);

    // Nothing is recorded until the store actually pulls.
    assert!(
        config_trees.recorded_seen(tenant(), store_id()).is_none(),
        "no liveness before any pull"
    );

    // A pull holding nothing records the contact at the clock's instant, with a null held version.
    let fresh = router
        .clone()
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config"),
            Some(&token),
        ))
        .await
        .expect("route the sync");
    assert_eq!(fresh.status(), StatusCode::OK);
    let (held, seen_at) = config_trees
        .recorded_seen(tenant(), store_id())
        .expect("the pull recorded liveness");
    assert_eq!(
        held, None,
        "a store holding nothing records a null held version"
    );
    assert_eq!(
        seen_at, NOW_MS,
        "the contact instant is the server clock's now"
    );

    // A pull holding the current version records exactly that version.
    let current = router
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config?held_version={version}"),
            Some(&token),
        ))
        .await
        .expect("route the sync");
    assert_eq!(current.status(), StatusCode::OK);
    let (held, _) = config_trees
        .recorded_seen(tenant(), store_id())
        .expect("liveness recorded");
    assert_eq!(
        held.map(|version| version.to_string()),
        Some(version),
        "the held version the edge reported is recorded verbatim"
    );
}

#[tokio::test]
async fn heartbeat_records_liveness_and_needs_the_read_config_scope() {
    // A lightweight heartbeat (ADR-0068 slice 2) records the store's contact without a config pull,
    // gated by the same read_config scope, and preserves any version a prior pull recorded.
    let keys = FakeKeys::default();
    let config_trees = FakeConfigTrees::default();
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        keys.clone(),
        provisioned_admin(),
        config_trees.clone(),
        FakeWebhooks::default(),
    ));
    let store_ulid = store_id().as_ulid().to_string();
    let uri = format!("/sync/stores/{store_ulid}/heartbeat");

    // No bearer → 401; a key scoped elsewhere → 403 (closed, not merely empty).
    let anon = router
        .clone()
        .oneshot(post_json(&uri, &serde_json::json!({})))
        .await
        .expect("route");
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
    let rollups_only = issue_key(&keys, tenant(), &[Scope::ReadRollups]);
    let wrong_scope = router
        .clone()
        .oneshot(post_json_bearer(
            &uri,
            &serde_json::json!({}),
            &rollups_only,
        ))
        .await
        .expect("route");
    assert_eq!(wrong_scope.status(), StatusCode::FORBIDDEN);

    // A read_config key records the contact and answers 204.
    let token = issue_key(&keys, tenant(), &[Scope::ReadConfig]);
    let beat = router
        .oneshot(post_json_bearer(&uri, &serde_json::json!({}), &token))
        .await
        .expect("route the heartbeat");
    assert_eq!(beat.status(), StatusCode::NO_CONTENT);
    let (held, seen_at) = config_trees
        .recorded_seen(tenant(), store_id())
        .expect("the heartbeat recorded liveness");
    assert_eq!(held, None, "a heartbeat carries no held version");
    assert_eq!(
        seen_at, NOW_MS,
        "the contact instant is the server clock's now"
    );
}

#[tokio::test]
async fn config_sync_is_closed_without_the_read_config_scope() {
    let keys = FakeKeys::default();
    let (router, config_token, store_ulid, _version) = published_config(&keys).await;
    let uri = format!("/sync/stores/{store_ulid}/config");

    // No bearer at all.
    let anon = router
        .clone()
        .oneshot(get(&uri, None))
        .await
        .expect("route");
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    // A key scoped to something else is forbidden, not merely empty.
    let rollups_only = issue_key(&keys, tenant(), &[Scope::ReadRollups]);
    let wrong_scope = router
        .clone()
        .oneshot(get(&uri, Some(&rollups_only)))
        .await
        .expect("route");
    assert_eq!(
        wrong_scope.status(),
        StatusCode::FORBIDDEN,
        "read_rollups does not authorise config pull"
    );

    // The right scope, but a store with no published config, is a 404 — not a leak of another's tree.
    let other_store = Ulid::from_u128(0xBEEF).to_string();
    let unknown = router
        .oneshot(get(
            &format!("/sync/stores/{other_store}/config"),
            Some(&config_token),
        ))
        .await
        .expect("route");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

// --- Rollup reset-cursor-and-replay (`POST /admin/stores/{id}/rollups/reset`) -------------------

#[tokio::test]
async fn rollups_reset_clears_the_cursor_so_the_projector_replays() {
    // A rollup seeded with an advanced cursor and a day of activity.
    let rollups = FakeRollups::default();
    let seeded = StoredRollups {
        cursor: Some(EventId::new(Ulid::from_u128(0x00C0_FFEE))),
        ..StoredRollups::default()
    };
    rollups
        .save(tenant(), store_id(), &seeded)
        .await
        .expect("seed the rollup");

    let router = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        rollups.clone(),
        FakeKeys::default(),
        provisioned_admin(),
    ));
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let uri = format!("/admin/stores/{store_ulid}/rollups/reset?tenant_id={tenant_ulid}");

    // Closed without a session.
    let unauth = router
        .clone()
        .oneshot(post_json(&uri, &serde_json::json!({})))
        .await
        .expect("route");
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    // With a session, the cursor is cleared so the next projector pass re-folds from the log.
    let cookie = admin_cookie(&router).await;
    let reset = router
        .oneshot(post_with_cookie(&uri, &serde_json::json!({}), &cookie))
        .await
        .expect("route");
    assert_eq!(reset.status(), StatusCode::NO_CONTENT);
    let after = rollups.load(tenant(), store_id()).await.expect("load");
    assert!(
        after.cursor.is_none() && after.days.is_empty(),
        "reset returns the rollup to the empty default, so the projector replays from the start"
    );
}

// --- Reconciliation diff (`POST /internal/reconcile`) -------------------------------------------

/// A reconciliation store that "has" a fixed set of ids; the missing ones are the complement.
#[derive(Clone)]
struct FakeReconcile {
    present: HashSet<EventId>,
}

impl ReconcileStore for FakeReconcile {
    async fn absent_event_ids(
        &self,
        _tenant: TenantId,
        _store: StoreId,
        candidates: &[EventId],
    ) -> Result<Vec<EventId>, ReconcileError> {
        Ok(candidates
            .iter()
            .filter(|id| !self.present.contains(id))
            .copied()
            .collect())
    }
}

/// An event id ULID string for the small integer `n`.
fn event_ulid(n: u128) -> String {
    Ulid::from_u128(n).to_string()
}

#[tokio::test]
async fn reconcile_returns_only_the_ids_the_cloud_is_missing() {
    // The cloud holds 1 and 3; the edge reports holding 1, 2, 3, 4 — so 2 and 4 must be re-pushed.
    let present: HashSet<EventId> = [1_u128, 3]
        .into_iter()
        .map(|n| EventId::new(Ulid::from_u128(n)))
        .collect();
    let router = http::reconcile_router(FakeReconcile { present });
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
        "event_ids": [event_ulid(1), event_ulid(2), event_ulid(3), event_ulid(4)],
    });
    let response = router
        .oneshot(post_json("/internal/reconcile", &body))
        .await
        .expect("route the reconcile");
    assert_eq!(response.status(), StatusCode::OK);
    let missing = json_body(response).await["missing"]
        .as_array()
        .expect("a missing array")
        .iter()
        .map(|value| value.as_str().expect("a string").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        missing,
        vec![event_ulid(2), event_ulid(4)],
        "only the ids the cloud lacks are returned, in the manifest's order"
    );
}

#[tokio::test]
async fn reconcile_rejects_a_malformed_id() {
    let router = http::reconcile_router(FakeReconcile {
        present: HashSet::new(),
    });
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
        "event_ids": ["not-a-ulid"],
    });
    let response = router
        .oneshot(post_json("/internal/reconcile", &body))
        .await
        .expect("route the reconcile");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a manifest carrying a non-ULID id is rejected, not silently dropped"
    );
}

// --- Device onboarding (`/sync/.../devices` + `/admin/devices/proposals`) -----------------------

/// One stored proposal, carrying the status a bare `PersistedDeviceProposal` does not.
#[derive(Clone)]
struct DeviceRow {
    id: DeviceProposalId,
    tenant: TenantId,
    store: StoreId,
    kind: DeviceKind,
    name: String,
    address: String,
    status: DeviceProposalStatus,
}

/// The device-proposal store as a flat list, exactly as the real table reads.
#[derive(Clone, Default)]
struct FakeDevices {
    rows: Arc<Mutex<Vec<DeviceRow>>>,
}

impl DeviceProposalStore for FakeDevices {
    async fn propose(&self, proposal: &PersistedDeviceProposal) -> Result<(), DeviceProposalError> {
        self.rows.lock().expect("lock").push(DeviceRow {
            id: proposal.id,
            tenant: proposal.tenant_id,
            store: proposal.store_id,
            kind: proposal.kind,
            name: proposal.name.clone(),
            address: proposal.address.clone(),
            status: DeviceProposalStatus::Pending,
        });
        Ok(())
    }

    async fn list(
        &self,
        tenant: TenantId,
        store: Option<StoreId>,
        status: DeviceProposalStatus,
    ) -> Result<Vec<DeviceProposalSummary>, DeviceProposalError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| {
                row.tenant == tenant
                    && row.status == status
                    && store.is_none_or(|only| row.store == only)
            })
            .map(|row| DeviceProposalSummary {
                id: row.id.to_string(),
                store_id: row.store.to_string(),
                kind: row.kind.as_wire().to_owned(),
                name: row.name.clone(),
                address: row.address.clone(),
                status: row.status.as_wire().to_owned(),
            })
            .collect())
    }

    async fn resolve(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        approved: bool,
    ) -> Result<bool, DeviceProposalError> {
        let mut rows = self.rows.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.tenant == tenant && row.id == id && row.status == DeviceProposalStatus::Pending {
                row.status = if approved {
                    DeviceProposalStatus::Approved
                } else {
                    DeviceProposalStatus::Rejected
                };
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// The main router (for `/admin/login`) and the device sub-router, sharing one admin and one key
/// store, plus the read_config-issuing key store — production's `merge`, in a test.
fn device_app(admin: FakeAdmin, keys: FakeKeys, devices: FakeDevices) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        keys.clone(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::device_router(
        devices,
        admin,
        keys,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

#[tokio::test]
async fn device_onboarding_propose_then_approve_then_appears_approved() {
    let keys = FakeKeys::default();
    let devices = FakeDevices::default();
    let router = device_app(provisioned_admin(), keys.clone(), devices);
    let cookie = admin_cookie(&router).await;
    let token = issue_key(&keys, tenant(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();
    let devices_uri = format!("/sync/stores/{store_ulid}/devices");

    // The store proposes a discovered printer.
    let proposal = serde_json::json!({ "kind": "printer", "name": "Kitchen 1", "address": "192.168.1.50:9100" });
    let created = router
        .clone()
        .oneshot(post_json_bearer(&devices_uri, &proposal, &token))
        .await
        .expect("route the proposal");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(created["status"], "pending");
    let id = created["id"].as_str().expect("an id").to_owned();

    // It shows in the admin pending queue.
    let pending = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/devices/proposals?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the queue");
    assert_eq!(pending.status(), StatusCode::OK);
    let queue = json_body(pending).await;
    assert_eq!(queue.as_array().expect("array").len(), 1);
    assert_eq!(queue[0]["id"], id);
    assert_eq!(queue[0]["kind"], "printer");

    // Before approval the store sees no approved devices.
    let before = router
        .clone()
        .oneshot(get(&devices_uri, Some(&token)))
        .await
        .expect("route the store read");
    assert_eq!(
        json_body(before).await.as_array().expect("array").len(),
        0,
        "nothing is usable until an operator approves it"
    );

    // The admin approves; it then appears in the store's approved list.
    let approve = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/devices/proposals/{id}/approve?tenant_id={tenant_ulid}"),
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route the approve");
    assert_eq!(approve.status(), StatusCode::NO_CONTENT);
    let after = router
        .oneshot(get(&devices_uri, Some(&token)))
        .await
        .expect("route the store read");
    let approved = json_body(after).await;
    assert_eq!(approved.as_array().expect("array").len(), 1);
    assert_eq!(approved[0]["address"], "192.168.1.50:9100");
}

#[tokio::test]
async fn approving_a_device_proposal_records_to_the_audit_trail() {
    let keys = FakeKeys::default();
    let devices = FakeDevices::default();
    let audit = FakeAudit::default();
    let admin = provisioned_admin();
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        keys.clone(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    let sink: Arc<dyn AuditRecorder> = Arc::new(AuditSink::new(audit.clone()));
    let router = http::router(app).merge(http::device_router(
        devices,
        admin,
        keys.clone(),
        clock(),
        sink,
    ));
    let cookie = admin_cookie(&router).await;
    let token = issue_key(&keys, tenant(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();

    let created = router
        .clone()
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{store_ulid}/devices"),
            &serde_json::json!({ "kind": "printer", "name": "Kitchen 1", "address": "192.168.1.50:9100" }),
            &token,
        ))
        .await
        .expect("route the proposal");
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_body(created).await["id"]
        .as_str()
        .expect("an id")
        .to_owned();

    let approve = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/devices/proposals/{id}/approve?tenant_id={tenant_ulid}"),
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route the approve");
    assert_eq!(approve.status(), StatusCode::NO_CONTENT);

    let recorded = audit.list(None, 10).await.expect("list audit entries");
    let entry = recorded
        .iter()
        .find(|entry| entry.entity_type == "device_proposal")
        .expect("the resolve was recorded");
    assert_eq!(
        entry.action, "device_proposal.approve",
        "approve is distinguished from reject"
    );
    assert_eq!(
        entry.entity_id, id,
        "the resolved proposal's id is recorded"
    );
    assert_eq!(
        entry.tenant_id.map(|id| id.to_string()),
        Some(tenant_ulid),
        "the entry is scoped to the tenant"
    );
    assert!(
        !entry.actor.email.is_empty(),
        "the resolving admin is snapshotted onto the entry"
    );
}

#[tokio::test]
async fn device_routes_enforce_their_scopes_and_the_session() {
    let keys = FakeKeys::default();
    let router = device_app(provisioned_admin(), keys.clone(), FakeDevices::default());
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();
    let devices_uri = format!("/sync/stores/{store_ulid}/devices");
    let proposal = serde_json::json!({ "kind": "kds", "name": "Expo", "address": "192.168.1.9" });

    // No bearer: closed.
    let anon = router
        .clone()
        .oneshot(post_json(&devices_uri, &proposal))
        .await
        .expect("route");
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    // A key without manage_devices: forbidden.
    let rollups_only = issue_key(&keys, tenant(), &[Scope::ReadRollups]);
    let wrong = router
        .clone()
        .oneshot(post_json_bearer(&devices_uri, &proposal, &rollups_only))
        .await
        .expect("route");
    assert_eq!(wrong.status(), StatusCode::FORBIDDEN);

    // The admin queue is closed without a session.
    let no_session = router
        .oneshot(get(
            &format!("/admin/devices/proposals?tenant_id={tenant_ulid}"),
            None,
        ))
        .await
        .expect("route");
    assert_eq!(no_session.status(), StatusCode::UNAUTHORIZED);
}

// --- Translation grid (`/admin/translations`, behind the session guard) -------------------------

/// The translation store, one grid per tenant.
#[derive(Clone, Default)]
struct FakeTranslations {
    rows: Arc<Mutex<HashMap<TenantId, TranslationGrid>>>,
}

impl TranslationStore for FakeTranslations {
    async fn load(
        &self,
        tenant: TenantId,
    ) -> Result<Option<TranslationGrid>, TranslationStoreError> {
        Ok(self.rows.lock().expect("lock").get(&tenant).cloned())
    }

    async fn save(
        &self,
        tenant: TenantId,
        grid: &TranslationGrid,
    ) -> Result<(), TranslationStoreError> {
        self.rows.lock().expect("lock").insert(tenant, grid.clone());
        Ok(())
    }
}

/// The main router (for `/admin/login`) and the translation sub-router, sharing one admin store.
fn translation_app(admin: FakeAdmin, translations: FakeTranslations) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::translation_router(
        translations,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

#[tokio::test]
async fn translation_grid_round_trips_and_enforces_the_en_fallback() {
    let router = translation_app(provisioned_admin(), FakeTranslations::default());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let uri = format!("/admin/translations?tenant_id={tenant_ulid}");

    // A grid with en on every key publishes and round-trips through GET.
    let good = serde_json::json!({
        "menu.pho": { "en": "Pho", "vi": "Phở" },
        "menu.tea": { "en": "Tea" },
    });
    let put = router
        .clone()
        .oneshot(put_with_cookie(&uri, &good, &cookie))
        .await
        .expect("route the publish");
    assert_eq!(put.status(), StatusCode::NO_CONTENT);
    let got = router
        .clone()
        .oneshot(get_with_cookie(&uri, &cookie))
        .await
        .expect("route the read");
    assert_eq!(got.status(), StatusCode::OK);
    assert_eq!(json_body(got).await, good, "the grid round-trips");

    // A grid missing en on a key is a 422 naming it, and does not overwrite the good grid.
    let bad = serde_json::json!({ "menu.rice": { "vi": "Cơm" } });
    let rejected = router
        .clone()
        .oneshot(put_with_cookie(&uri, &bad, &cookie))
        .await
        .expect("route the bad publish");
    assert_eq!(rejected.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(rejected).await["missing_fallback"],
        serde_json::json!(["menu.rice"]),
        "the rejection names the key lacking an en fallback"
    );
    let unchanged = router
        .oneshot(get_with_cookie(&uri, &cookie))
        .await
        .expect("route the re-read");
    assert_eq!(
        json_body(unchanged).await,
        good,
        "a rejected publish left the last good grid current"
    );
}

#[tokio::test]
async fn translation_routes_require_a_session() {
    let router = translation_app(provisioned_admin(), FakeTranslations::default());
    let tenant_ulid = tenant().as_ulid().to_string();
    let response = router
        .oneshot(get(
            &format!("/admin/translations?tenant_id={tenant_ulid}"),
            None,
        ))
        .await
        .expect("route");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// --- Webhook admin routes (`/admin/webhooks`, behind the session guard) --------------------------

/// Registering returns the signing secret once, the listing shows the endpoint without any secret,
/// and deleting removes it. IP-literal URLs are used throughout so `vet` classifies them without a
/// DNS lookup — the test needs no network.
#[tokio::test]
async fn webhook_register_lists_and_deletes() {
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Register a public IP-literal destination (no DNS needed to vet it).
    let body = serde_json::json!({
        "tenant_id": tenant_ulid,
        "store_id": store_ulid,
        "url": "https://93.184.216.34/hook",
    });
    let created = router
        .clone()
        .oneshot(post_with_cookie("/admin/webhooks", &body, &cookie))
        .await
        .expect("route the registration");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let id = created["id"].as_str().expect("an id").to_owned();
    assert!(
        created["signing_secret"]
            .as_str()
            .is_some_and(|secret| secret.len() == 64),
        "the 256-bit signing secret is returned once, as 64 hex chars"
    );
    assert_eq!(created["url"], "https://93.184.216.34/hook");

    // The listing shows the endpoint as metadata only — never a secret.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/webhooks?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the listing");
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = json_body(listed).await;
    let rows = listed.as_array().expect("an array");
    assert_eq!(rows.len(), 1, "the one registered endpoint is listed");
    let only = rows.first().expect("one row");
    assert_eq!(only["id"], id);
    assert_eq!(only["url"], "https://93.184.216.34/hook");
    assert!(only["cursor"].is_null(), "nothing delivered yet");
    assert_eq!(only["disabled"], false);
    assert!(
        only.get("secret").is_none() && only.get("signing_secret").is_none(),
        "a listing never carries the signing secret"
    );

    // Delete it; the listing is then empty.
    let deleted = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/webhooks/{id}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the delete");
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/webhooks?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the re-listing");
    assert_eq!(json_body(listed).await.as_array().expect("array").len(), 0);
}

/// Registration is closed without a session, and an inward-pointing or plaintext URL is refused.
#[tokio::test]
async fn webhook_register_requires_a_session_and_refuses_ssrf() {
    let router = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ));
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let good = serde_json::json!({
        "tenant_id": tenant_ulid,
        "store_id": store_ulid,
        "url": "https://93.184.216.34/hook",
    });

    // No cookie: closed.
    let unauth = router
        .clone()
        .oneshot(post_json("/admin/webhooks", &good))
        .await
        .expect("route the request");
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let cookie = admin_cookie(&router).await;

    // Loopback: the classic SSRF target, refused as a bad request (IP literal, so no DNS).
    let loopback = serde_json::json!({
        "tenant_id": tenant_ulid,
        "store_id": store_ulid,
        "url": "https://127.0.0.1/hook",
    });
    let refused = router
        .clone()
        .oneshot(post_with_cookie("/admin/webhooks", &loopback, &cookie))
        .await
        .expect("route the request");
    assert_eq!(
        refused.status(),
        StatusCode::BAD_REQUEST,
        "a loopback destination is refused before anything is stored"
    );

    // Plaintext http is refused even to a public address.
    let plaintext = serde_json::json!({
        "tenant_id": tenant_ulid,
        "store_id": store_ulid,
        "url": "http://93.184.216.34/hook",
    });
    let refused = router
        .oneshot(post_with_cookie("/admin/webhooks", &plaintext, &cookie))
        .await
        .expect("route the request");
    assert_eq!(
        refused.status(),
        StatusCode::BAD_REQUEST,
        "a webhook must use https"
    );
}

// --- Device activation exchange (ADR-0050) ------------------------------------------------------

/// The activation store, keyed by code hash exactly as the real table. The exchange flips a code to
/// redeemed and counts the credentials it mints, so a test can assert single-use.
#[derive(Clone, Default)]
struct FakeActivations {
    codes: Arc<Mutex<HashMap<[u8; 32], IssuedCode>>>,
    minted: Arc<Mutex<u32>>,
}

impl FakeActivations {
    /// Seeds one issued code for a slot, as the admin issue route would.
    fn with_issued(
        code_hash: [u8; 32],
        tenant: TenantId,
        store: StoreId,
        device: DeviceId,
    ) -> Self {
        let mut codes = HashMap::new();
        codes.insert(
            code_hash,
            IssuedCode {
                tenant_id: tenant,
                store_id: store,
                device_id: device,
                status: CodeStatus::Issued,
            },
        );
        Self {
            codes: Arc::new(Mutex::new(codes)),
            minted: Arc::new(Mutex::new(0)),
        }
    }

    /// How many credentials have been provisioned.
    fn minted(&self) -> u32 {
        *self.minted.lock().expect("lock")
    }
}

impl ActivationCodeStore for FakeActivations {
    async fn issue(
        &self,
        code_hash: [u8; 32],
        tenant_id: TenantId,
        store_id: StoreId,
        device_id: DeviceId,
    ) -> Result<(), ActivationStoreError> {
        self.codes.lock().expect("lock").insert(
            code_hash,
            IssuedCode {
                tenant_id,
                store_id,
                device_id,
                status: CodeStatus::Issued,
            },
        );
        Ok(())
    }

    async fn lookup(
        &self,
        code_hash: [u8; 32],
    ) -> Result<Option<IssuedCode>, ActivationStoreError> {
        Ok(self.codes.lock().expect("lock").get(&code_hash).cloned())
    }

    async fn consume_and_provision(
        &self,
        code_hash: [u8; 32],
        _credential: &DeviceCredential,
    ) -> Result<bool, ActivationStoreError> {
        let mut codes = self.codes.lock().expect("lock");
        match codes.get_mut(&code_hash) {
            Some(code) if code.status == CodeStatus::Issued => {
                code.status = CodeStatus::Redeemed;
                *self.minted.lock().expect("lock") += 1;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn revoke_slot(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        device_id: DeviceId,
    ) -> Result<u64, ActivationStoreError> {
        let mut count: u64 = 0;
        for code in self.codes.lock().expect("lock").values_mut() {
            if code.status == CodeStatus::Issued
                && code.tenant_id == tenant_id
                && code.store_id == store_id
                && code.device_id == device_id
            {
                code.status = CodeStatus::Revoked;
                count += 1;
            }
        }
        Ok(count)
    }
}

#[tokio::test]
async fn the_activation_exchange_is_single_use_and_gives_no_oracle() {
    let code = ActivationCode::from_entropy([13; pos_core::activation::PAYLOAD_LEN]);
    let device = DeviceId::new(Ulid::from_u128(0xDECAF));
    let activations = FakeActivations::with_issued(hash_code(&code), tenant(), store_id(), device);
    let router = http::activation_router(activations.clone(), FakeAdmin::default(), clock());

    // A device presents its valid, unredeemed code and receives a minted credential, shown once.
    let first = router
        .clone()
        .oneshot(post_json(
            "/activate",
            &serde_json::json!({ "code": code.as_str() }),
        ))
        .await
        .expect("route the exchange");
    assert_eq!(first.status(), StatusCode::CREATED);
    let body = json_body(first).await;
    assert!(
        body["credential"]
            .as_str()
            .expect("a credential")
            .starts_with("posdev_"),
        "the credential is the real value, shown once"
    );
    assert_eq!(
        body["device_id"].as_str().expect("a device id"),
        device.to_string()
    );
    assert_eq!(activations.minted(), 1);

    // The same code again is refused — activation is single-use.
    let replay = router
        .clone()
        .oneshot(post_json(
            "/activate",
            &serde_json::json!({ "code": code.as_str() }),
        ))
        .await
        .expect("route the replay");
    assert_eq!(
        replay.status(),
        StatusCode::FORBIDDEN,
        "a spent code is refused"
    );
    assert_eq!(activations.minted(), 1, "no second credential is minted");

    // An unknown but well-formed code is refused identically — no oracle tells them apart.
    let unknown = ActivationCode::from_entropy([200; pos_core::activation::PAYLOAD_LEN]);
    let miss = router
        .clone()
        .oneshot(post_json(
            "/activate",
            &serde_json::json!({ "code": unknown.as_str() }),
        ))
        .await
        .expect("route the unknown code");
    assert_eq!(
        miss.status(),
        StatusCode::FORBIDDEN,
        "an unknown code is refused exactly as a spent one"
    );

    // A malformed code is a plain client error, not a refusal — it never named a real code.
    let malformed = router
        .oneshot(post_json(
            "/activate",
            &serde_json::json!({ "code": "not-a-valid-code" }),
        ))
        .await
        .expect("route the malformed code");
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
}

// --- Public order intake (POST /v1/orders) — P11a, ADR-0056 -------------------------------------

/// A store directory that reports a fixed owner for every store, so a test can make the request's
/// store belong to the caller's tenant, to another tenant, or to no store at all.
#[derive(Clone)]
struct FakeDirectory {
    owner: Option<TenantId>,
}

impl StoreDirectory for FakeDirectory {
    async fn tenant_of(&self, _store_id: StoreId) -> Result<Option<TenantId>, PortError> {
        Ok(self.owner)
    }
}

/// The store every order test targets.
fn order_store() -> StoreId {
    StoreId::new(Ulid::from_u128(0x5_709E))
}

/// Builds the intake router over a fresh fake intake and a directory that says `owner` owns the
/// store, plus the `keys` a test issued a token into.
fn orders_app(keys: FakeKeys, owner: Option<TenantId>) -> axum::Router {
    orders_router(FakeIntake::new(), keys, clock(), FakeDirectory { owner })
}

/// A one-line order body naming `menu_item` on the public-API channel.
fn order_body(
    reference: &str,
    menu_item: MenuItemId,
    quoted: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut line = serde_json::json!({
        "menu_item_id": menu_item.to_string(),
        "quantity_milli": 1000,
    });
    if let Some(quoted) = quoted {
        line["quoted_unit_price"] = quoted;
    }
    serde_json::json!({
        "external_reference": reference,
        "sales_channel": "SALES_CHANNEL_API",
        "store_id": order_store().to_string(),
        "lines": [line],
        "placed_at_ms": NOW_MS,
    })
}

#[tokio::test]
async fn orders_submit_accepts_and_creates() {
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let (known, _price) = known_menu_item();
    let response = orders_app(keys, Some(tenant()))
        .oneshot(post_json_bearer(
            "/v1/orders",
            &order_body("api-1", known, None),
            &token,
        ))
        .await
        .expect("route the order");
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(value["created"].as_bool(), Some(true));
    assert!(value["order_id"].as_str().is_some(), "an id was assigned");
}

#[tokio::test]
async fn orders_submit_is_idempotent() {
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let (known, _price) = known_menu_item();
    let body = order_body("api-dup", known, None);
    // One router, cloned across two calls, so both submits reach the same fake intake state.
    let router = orders_app(keys, Some(tenant()));
    let first = router
        .clone()
        .oneshot(post_json_bearer("/v1/orders", &body, &token))
        .await
        .expect("route the first submit");
    assert_eq!(first.status(), StatusCode::CREATED);
    let second = router
        .oneshot(post_json_bearer("/v1/orders", &body, &token))
        .await
        .expect("route the repeat");
    assert_eq!(
        second.status(),
        StatusCode::OK,
        "a repeat is not created anew"
    );
    let bytes = second.into_body().collect().await.expect("body").to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(value["created"].as_bool(), Some(false));
}

#[tokio::test]
async fn orders_unknown_item_is_bad_request() {
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let response = orders_app(keys, Some(tenant()))
        .oneshot(post_json_bearer(
            "/v1/orders",
            &order_body("api-x", unknown_menu_item(), None),
            &token,
        ))
        .await
        .expect("route the unknown-item order");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn orders_for_another_tenants_store_is_not_found() {
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let (known, _price) = known_menu_item();
    // The store belongs to a different tenant: a generic 404, no oracle.
    let other = TenantId::new(Ulid::from_u128(0xB0B));
    let response = orders_app(keys, Some(other))
        .oneshot(post_json_bearer(
            "/v1/orders",
            &order_body("api-2", known, None),
            &token,
        ))
        .await
        .expect("route the cross-tenant order");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn orders_for_an_unknown_store_is_not_found() {
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let (known, _price) = known_menu_item();
    let response = orders_app(keys, None)
        .oneshot(post_json_bearer(
            "/v1/orders",
            &order_body("api-3", known, None),
            &token,
        ))
        .await
        .expect("route the unknown-store order");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn orders_without_the_place_orders_scope_is_forbidden() {
    let keys = FakeKeys::default();
    // A valid key, but only a read scope — never authorised to write.
    let token = issue_key(&keys, tenant(), &[Scope::ReadRollups]);
    let (known, _price) = known_menu_item();
    let response = orders_app(keys, Some(tenant()))
        .oneshot(post_json_bearer(
            "/v1/orders",
            &order_body("api-4", known, None),
            &token,
        ))
        .await
        .expect("route the under-scoped order");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn orders_without_a_bearer_is_unauthorized() {
    let keys = FakeKeys::default();
    let (known, _price) = known_menu_item();
    let response = orders_app(keys, Some(tenant()))
        .oneshot(post_json("/v1/orders", &order_body("api-5", known, None)))
        .await
        .expect("route the unauthenticated order");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_qr_order_awaits_staff_confirmation_and_a_stale_quote_is_repriced() {
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let (known, price) = known_menu_item();
    // A quote that differs from the store's price, and a table id (a QR order).
    let stale = serde_json::json!({
        "currency_code": price.currency_code.as_str(),
        "amount_minor": price.amount_minor.saturating_add(1),
    });
    let mut body = order_body("api-qr", known, Some(stale));
    body["table_id"] = serde_json::json!(Ulid::from_u128(0x7AB1E).to_string());
    let response = orders_app(keys, Some(tenant()))
        .oneshot(post_json_bearer("/v1/orders", &body, &token))
        .await
        .expect("route the QR order");
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        value["awaiting_staff_confirmation"].as_bool(),
        Some(true),
        "a QR order waits for staff (ADR-0012)"
    );
    assert_eq!(
        value["repriced"].as_bool(),
        Some(true),
        "a stale quote is reported, not honoured"
    );
}

// --- Guest QR ordering (POST /v1/qr/orders) — P11a-2, ADR-0057 ---------------------------------

/// The table a QR test signs a token for, in `order_store()`.
fn qr_table() -> TableId {
    TableId::new(Ulid::from_u128(0x7AB1E))
}

/// A guest QR body: no store/table/channel on the wire (they ride the token), just the reference,
/// the lines, and when it was placed.
fn qr_body(token: &str, reference: &str, menu_item: MenuItemId) -> serde_json::Value {
    serde_json::json!({
        "table_token": token,
        "external_reference": reference,
        "lines": [{ "menu_item_id": menu_item.to_string(), "quantity_milli": 1000 }],
        "placed_at_ms": NOW_MS,
    })
}

#[tokio::test]
async fn a_signed_qr_order_is_accepted_and_awaits_staff_confirmation() {
    let secret = TableTokenSecret::new("qr-endpoint-secret");
    let token = mint_table_token(&secret, tenant(), order_store(), qr_table());
    let (known, _price) = known_menu_item();
    let router = qr_router(secret, FakeIntake::new(), EmptyConfigTrees, clock());
    let response = router
        .oneshot(post_json("/v1/qr/orders", &qr_body(&token, "qr-1", known)))
        .await
        .expect("route the QR order");
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(value["created"].as_bool(), Some(true));
    assert_eq!(
        value["awaiting_staff_confirmation"].as_bool(),
        Some(true),
        "a QR order (a table order) waits for staff by default (ADR-0057)"
    );
}

#[tokio::test]
async fn an_unsigned_qr_token_is_forbidden_and_never_reaches_intake() {
    let secret = TableTokenSecret::new("qr-endpoint-secret");
    // A token minted with a different secret does not verify against ours.
    let forged = mint_table_token(
        &TableTokenSecret::new("someone-elses-secret"),
        tenant(),
        order_store(),
        qr_table(),
    );
    let (known, _price) = known_menu_item();
    let router = qr_router(secret, FakeIntake::new(), EmptyConfigTrees, clock());
    let response = router
        .oneshot(post_json("/v1/qr/orders", &qr_body(&forged, "qr-x", known)))
        .await
        .expect("route the forged QR order");
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "an untrusted table token is refused before intake"
    );
}

// --- Order relay (POST /v1/orders over the durable queue) — P11a-2, ADR-0061 -------------------

/// A config-tree store that has published nothing, so the relay falls back to its defaults (intake
/// enabled, the default park). Enough to exercise the queue and the pull/ack path.
#[derive(Clone)]
struct EmptyConfigTrees;

impl ConfigTreeStore for EmptyConfigTrees {
    async fn load(
        &self,
        _tenant: TenantId,
        _store: StoreId,
    ) -> Result<Option<ConfigTreeState>, ConfigStoreError> {
        Ok(None)
    }

    async fn save(
        &self,
        _tenant: TenantId,
        _store: StoreId,
        _state: &ConfigTreeState,
    ) -> Result<(), ConfigStoreError> {
        Ok(())
    }

    async fn record_store_seen(
        &self,
        _tenant: TenantId,
        _store: StoreId,
        _held_version: Option<ConfigVersionId>,
        _seen_at: Timestamp,
    ) -> Result<(), ConfigStoreError> {
        Ok(())
    }

    async fn record_store_heartbeat(
        &self,
        _tenant: TenantId,
        _store: StoreId,
        _seen_at: Timestamp,
    ) -> Result<(), ConfigStoreError> {
        Ok(())
    }
}

/// One queued order the fake holds.
#[derive(Clone)]
struct QueueEntry {
    tenant: String,
    queued_id: OrderQueueId,
    payload: QueuedOrderPayload,
    status: OrderStatus,
}

/// An in-memory [`OrderQueueStore`]. Clones share one store, so the relay's `submit` and the
/// store-facing pull/ack see the same queue.
#[derive(Clone, Default)]
struct FakeOrderQueue {
    entries: Arc<Mutex<Vec<QueueEntry>>>,
}

impl FakeOrderQueue {
    fn new() -> Self {
        Self::default()
    }
}

impl OrderQueueStore for FakeOrderQueue {
    async fn enqueue(
        &self,
        tenant: TenantId,
        queued_id: OrderQueueId,
        payload: &QueuedOrderPayload,
    ) -> Result<OrderRecord, PortError> {
        let mut entries = self.entries.lock().expect("queue lock");
        let tenant = tenant.to_string();
        if let Some(found) = entries.iter().find(|entry| {
            entry.tenant == tenant
                && entry.payload.store_id == payload.store_id
                && entry.payload.sales_channel == payload.sales_channel
                && entry.payload.external_reference == payload.external_reference
        }) {
            return Ok(OrderRecord {
                queued_id: found.queued_id,
                status: found.status.clone(),
            });
        }
        entries.push(QueueEntry {
            tenant,
            queued_id,
            payload: payload.clone(),
            status: OrderStatus::Pending,
        });
        Ok(OrderRecord {
            queued_id,
            status: OrderStatus::Pending,
        })
    }

    async fn outcome(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        sales_channel: &str,
        external_reference: &str,
    ) -> Result<Option<OrderRecord>, PortError> {
        let entries = self.entries.lock().expect("queue lock");
        let tenant = tenant.to_string();
        let store = store_id.to_string();
        Ok(entries
            .iter()
            .find(|entry| {
                entry.tenant == tenant
                    && entry.payload.store_id == store
                    && entry.payload.sales_channel == sales_channel
                    && entry.payload.external_reference == external_reference
            })
            .map(|entry| OrderRecord {
                queued_id: entry.queued_id,
                status: entry.status.clone(),
            }))
    }

    async fn pull_pending(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        limit: u32,
    ) -> Result<Vec<PendingOrder>, PortError> {
        let entries = self.entries.lock().expect("queue lock");
        let tenant = tenant.to_string();
        let store = store_id.to_string();
        let cap = usize::try_from(limit).unwrap_or(usize::MAX);
        Ok(entries
            .iter()
            .filter(|entry| {
                entry.tenant == tenant
                    && entry.payload.store_id == store
                    && matches!(entry.status, OrderStatus::Pending)
            })
            .take(cap)
            .map(|entry| PendingOrder {
                queued_id: entry.queued_id,
                payload: entry.payload.clone(),
            })
            .collect())
    }

    async fn record_outcome(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        queued_id: OrderQueueId,
        outcome: &StoreOutcome,
    ) -> Result<bool, PortError> {
        let mut entries = self.entries.lock().expect("queue lock");
        let tenant = tenant.to_string();
        let store = store_id.to_string();
        if let Some(entry) = entries.iter_mut().find(|entry| {
            entry.tenant == tenant
                && entry.payload.store_id == store
                && entry.queued_id == queued_id
                && matches!(entry.status, OrderStatus::Pending)
        }) {
            entry.status = OrderStatus::Reported(outcome.clone());
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[tokio::test(start_paused = true)]
async fn an_unconfirmed_order_queues_then_pull_ack_lookup_resolves() {
    let keys = FakeKeys::default();
    let place = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let relay_token = issue_key(&keys, tenant(), &[Scope::RelayOrders]);
    let (known, _price) = known_menu_item();
    let queue = FakeOrderQueue::new();
    let app = || {
        orders_router(
            OrderRelay::new(
                FakeDirectory {
                    owner: Some(tenant()),
                },
                EmptyConfigTrees,
                queue.clone(),
                clock(),
            ),
            keys.clone(),
            clock(),
            FakeDirectory {
                owner: Some(tenant()),
            },
        )
        .merge(orders_sync_router_with_cap(
            queue.clone(),
            keys.clone(),
            clock(),
            std::time::Duration::ZERO,
        ))
    };

    // Store silent: submit parks (instantly, under paused time) and reports the order queued.
    let submitted = app()
        .oneshot(post_json_bearer(
            "/v1/orders",
            &order_body("relay-1", known, None),
            &place,
        ))
        .await
        .expect("route submit");
    assert_eq!(submitted.status(), StatusCode::SERVICE_UNAVAILABLE);

    // The store pulls its pending order.
    let store = order_store().to_string();
    let pulled = app()
        .oneshot(get(
            &format!("/sync/stores/{store}/orders"),
            Some(&relay_token),
        ))
        .await
        .expect("route pull");
    assert_eq!(pulled.status(), StatusCode::OK);
    let body = json_body(pulled).await;
    assert_eq!(body.as_array().map(Vec::len), Some(1));
    let queued_id = body[0]["queued_id"]
        .as_str()
        .expect("a queued id")
        .to_owned();

    // The store reports the acceptance it decided locally.
    let order_id = order_store().as_ulid().to_string();
    let ack_body = serde_json::json!({
        "outcome": "accepted",
        "order_id": order_id,
        "created": true,
        "total": { "currency_code": "VND", "amount_minor": 150_000 },
        "repriced": false,
        "awaiting_staff_confirmation": false,
    });
    let acked = app()
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{store}/orders/{queued_id}/ack"),
            &ack_body,
            &relay_token,
        ))
        .await
        .expect("route ack");
    assert_eq!(acked.status(), StatusCode::NO_CONTENT);

    // The caller resolves the timed-out submit by looking the reference up.
    let looked = app()
        .oneshot(get(
            &format!(
                "/v1/orders?store_id={store}&sales_channel=SALES_CHANNEL_API&external_reference=relay-1"
            ),
            Some(&place),
        ))
        .await
        .expect("route look-up");
    assert_eq!(looked.status(), StatusCode::OK);
    let resolved = json_body(looked).await;
    assert_eq!(resolved["order_id"].as_str(), Some(order_id.as_str()));

    // A second pull sees nothing pending — the order is no longer queued.
    let again = app()
        .oneshot(get(
            &format!("/sync/stores/{store}/orders"),
            Some(&relay_token),
        ))
        .await
        .expect("route pull again");
    let again_body = json_body(again).await;
    assert_eq!(again_body.as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn pulling_orders_requires_the_relay_orders_scope() {
    let keys = FakeKeys::default();
    // A valid key, but only PlaceOrders — it may submit, not pull the store's queue.
    let place = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let app = orders_sync_router_with_cap(
        FakeOrderQueue::new(),
        keys.clone(),
        clock(),
        std::time::Duration::ZERO,
    );
    let store = order_store().to_string();
    let response = app
        .oneshot(get(&format!("/sync/stores/{store}/orders"), Some(&place)))
        .await
        .expect("route pull");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// --- The org registry (ADR-0065) ---------------------------------------------------------------

/// The registry as four flat lists, mirroring how the real tables read and scope by tenant.
#[derive(Clone, Default)]
struct FakeRegistry {
    tenants: Arc<Mutex<Vec<TenantRecord>>>,
    brands: Arc<Mutex<Vec<BrandRecord>>>,
    stores: Arc<Mutex<Vec<StoreRecord>>>,
    devices: Arc<Mutex<Vec<DeviceRecord>>>,
}

impl RegistryStore for FakeRegistry {
    async fn create_tenant(&self, tenant: &TenantRecord) -> Result<(), RegistryStoreError> {
        self.tenants.lock().expect("lock").push(tenant.clone());
        Ok(())
    }

    async fn list_tenants(&self) -> Result<Vec<TenantRecord>, RegistryStoreError> {
        Ok(self.tenants.lock().expect("lock").clone())
    }

    async fn update_tenant(&self, tenant: &TenantRecord) -> Result<bool, RegistryStoreError> {
        let mut rows = self.tenants.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.tenant_id == tenant.tenant_id {
                row.name.clone_from(&tenant.name);
                row.status = tenant.status;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn create_brand(&self, brand: &BrandRecord) -> Result<(), RegistryStoreError> {
        self.brands.lock().expect("lock").push(brand.clone());
        Ok(())
    }

    async fn list_brands(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<BrandRecord>, RegistryStoreError> {
        Ok(self
            .brands
            .lock()
            .expect("lock")
            .iter()
            .filter(|brand| brand.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_brand(&self, brand: &BrandRecord) -> Result<bool, RegistryStoreError> {
        let mut rows = self.brands.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.brand_id == brand.brand_id && row.tenant_id == brand.tenant_id {
                row.name.clone_from(&brand.name);
                row.status = brand.status;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn create_store(&self, store: &StoreRecord) -> Result<(), RegistryStoreError> {
        self.stores.lock().expect("lock").push(store.clone());
        Ok(())
    }

    async fn list_stores(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<StoreRecord>, RegistryStoreError> {
        Ok(self
            .stores
            .lock()
            .expect("lock")
            .iter()
            .filter(|store| store.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_store(&self, store: &StoreRecord) -> Result<bool, RegistryStoreError> {
        let mut rows = self.stores.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.store_id == store.store_id && row.tenant_id == store.tenant_id {
                row.name.clone_from(&store.name);
                row.brand_id = store.brand_id;
                row.status = store.status;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn create_device(&self, device: &DeviceRecord) -> Result<(), RegistryStoreError> {
        self.devices.lock().expect("lock").push(device.clone());
        Ok(())
    }

    async fn list_devices(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<DeviceRecord>, RegistryStoreError> {
        Ok(self
            .devices
            .lock()
            .expect("lock")
            .iter()
            .filter(|device| device.tenant_id == tenant_id && device.store_id == store_id)
            .cloned()
            .collect())
    }

    async fn update_device(&self, device: &DeviceRecord) -> Result<bool, RegistryStoreError> {
        let mut rows = self.devices.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.device_id == device.device_id && row.tenant_id == device.tenant_id {
                row.name.clone_from(&device.name);
                row.kind.clone_from(&device.kind);
                row.status = device.status;
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// The main router (for `/admin/login`) and the registry sub-router, sharing one admin store —
/// production's `merge`, in a test. Audit is a no-op here; the emission path is asserted by
/// [`registry_app_with_audit`].
fn registry_app(admin: FakeAdmin, registry: FakeRegistry) -> axum::Router {
    registry_app_with_audit(admin, registry, Arc::new(NoopAuditRecorder))
}

/// As [`registry_app`], but with a caller-supplied audit recorder so a test can assert that a
/// registry write records to the audit trail (ADR-0069).
fn registry_app_with_audit(
    admin: FakeAdmin,
    registry: FakeRegistry,
    audit: Arc<dyn AuditRecorder>,
) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::registry_router(registry, admin, clock(), audit))
}

// --- Fleet liveness read model (ADR-0068 slice 3) ----------------------------------------------

/// The fleet read model as an in-memory list of `(tenant, row)` — the binary joins four real tables,
/// but the handler and its online/offline derivation are the same code here.
#[derive(Clone, Default)]
struct FakeFleet {
    rows: Arc<Mutex<Vec<(TenantId, FleetRow)>>>,
}

impl FakeFleet {
    /// Seeds one store's fleet row under a tenant.
    fn with_row(self, tenant: TenantId, row: FleetRow) -> Self {
        self.rows.lock().expect("lock").push((tenant, row));
        self
    }
}

impl FleetStore for FakeFleet {
    async fn list_fleet(&self, tenant: TenantId) -> Result<Vec<FleetRow>, FleetStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|(row_tenant, _)| *row_tenant == tenant)
            .map(|(_, row)| row.clone())
            .collect())
    }

    async fn store_detail(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> Result<Option<FleetRow>, FleetStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|(row_tenant, row)| *row_tenant == tenant && row.store_id == store)
            .map(|(_, row)| row.clone()))
    }
}

/// A timestamp `offset_ms` before the fixed test clock's instant.
fn seen_ago(offset_ms: i64) -> Timestamp {
    Timestamp::from_milliseconds_since_epoch(NOW_MS - offset_ms).expect("a valid instant")
}

/// The main router (for `/admin/login`) merged with the fleet sub-router, one shared admin store.
fn fleet_app(admin: FakeAdmin, fleet: FakeFleet) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::fleet_router(fleet, admin, clock()))
}

#[tokio::test]
async fn fleet_lists_stores_with_online_and_config_drift_derived_at_read() {
    let online_store = StoreId::new(Ulid::from_u128(0x00F1_EE7A));
    let offline_store = StoreId::new(Ulid::from_u128(0x00F1_EE7B));
    // One store seen a second ago, holding the published version — online and in sync. One seen ten
    // minutes ago, holding an old version, with a relay backlog — offline and drifted.
    let fleet = FakeFleet::default()
        .with_row(
            tenant(),
            FleetRow {
                store_id: online_store,
                name: "Bến Thành".to_owned(),
                status: EntityStatus::Active,
                last_seen_at: Some(seen_ago(1_000)),
                last_config_pull_at: Some(seen_ago(1_000)),
                config_version_held: Some("v-current".to_owned()),
                config_version_published: Some("v-current".to_owned()),
                relay_backlog: 0,
                relay_oldest_pending_at: None,
            },
        )
        .with_row(
            tenant(),
            FleetRow {
                store_id: offline_store,
                name: "Xuân Thủy".to_owned(),
                status: EntityStatus::Active,
                last_seen_at: Some(seen_ago(600_000)),
                last_config_pull_at: Some(seen_ago(600_000)),
                config_version_held: Some("v-old".to_owned()),
                config_version_published: Some("v-current".to_owned()),
                relay_backlog: 3,
                relay_oldest_pending_at: Some(seen_ago(120_000)),
            },
        );
    let router = fleet_app(provisioned_admin(), fleet);
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/fleet?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the fleet list");
    assert_eq!(listed.status(), StatusCode::OK);
    let body = json_body(listed).await;
    let rows = body.as_array().expect("an array of stores");
    assert_eq!(rows.len(), 2, "both of the tenant's stores are listed");

    let online = &rows[0];
    assert_eq!(online["store_id"], online_store.as_ulid().to_string());
    assert_eq!(online["name"], "Bến Thành");
    assert_eq!(online["online"], true, "seen a second ago reads as online");
    assert_eq!(
        online["config_current"], true,
        "held equals published, so it is current"
    );
    assert_eq!(online["relay_backlog"], 0);

    let offline = &rows[1];
    assert_eq!(
        offline["online"], false,
        "seen ten minutes ago is past the freshness window"
    );
    assert_eq!(
        offline["config_current"], false,
        "holding an old version is a drift"
    );
    assert_eq!(offline["config_version_held"], "v-old");
    assert_eq!(offline["config_version_published"], "v-current");
    assert_eq!(offline["relay_backlog"], 3);
    assert_eq!(offline["relay_oldest_pending_at_ms"], NOW_MS - 120_000);
}

#[tokio::test]
async fn fleet_never_seen_store_is_offline_and_not_current() {
    let store = StoreId::new(Ulid::from_u128(0x00F1_EE7C));
    let fleet = FakeFleet::default().with_row(
        tenant(),
        FleetRow {
            store_id: store,
            name: "Phú Mỹ Hưng".to_owned(),
            status: EntityStatus::Active,
            last_seen_at: None,
            last_config_pull_at: None,
            config_version_held: None,
            config_version_published: Some("v-current".to_owned()),
            relay_backlog: 0,
            relay_oldest_pending_at: None,
        },
    );
    let router = fleet_app(provisioned_admin(), fleet);
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/fleet?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the fleet list");
    let row = &json_body(listed).await[0];
    assert_eq!(
        row["online"], false,
        "a store that never checked in is offline"
    );
    assert_eq!(
        row["last_seen_at_ms"],
        serde_json::Value::Null,
        "and carries no last-seen instant"
    );
    assert_eq!(
        row["config_current"], false,
        "a store holding nothing is not current, even against a published version"
    );
}

#[tokio::test]
async fn fleet_reads_one_store_and_404s_an_unknown_one() {
    let store = StoreId::new(Ulid::from_u128(0x00F1_EE7D));
    let fleet = FakeFleet::default().with_row(
        tenant(),
        FleetRow {
            store_id: store,
            name: "Thảo Điền".to_owned(),
            status: EntityStatus::Active,
            last_seen_at: Some(seen_ago(1_000)),
            last_config_pull_at: Some(seen_ago(1_000)),
            config_version_held: Some("v-current".to_owned()),
            config_version_published: Some("v-current".to_owned()),
            relay_backlog: 0,
            relay_oldest_pending_at: None,
        },
    );
    let router = fleet_app(provisioned_admin(), fleet);
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store.as_ulid().to_string();

    let found = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/fleet/{store_ulid}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the store detail");
    assert_eq!(found.status(), StatusCode::OK);
    assert_eq!(json_body(found).await["name"], "Thảo Điền");

    let unknown = StoreId::new(Ulid::from_u128(0xDEAD)).as_ulid().to_string();
    let missing = router
        .oneshot(get_with_cookie(
            &format!("/admin/fleet/{unknown}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the store detail");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fleet_needs_a_session() {
    let router = fleet_app(provisioned_admin(), FakeFleet::default());
    let tenant_ulid = tenant().as_ulid().to_string();
    let denied = router
        .oneshot(get(&format!("/admin/fleet?tenant_id={tenant_ulid}"), None))
        .await
        .expect("route the fleet list");
    assert_eq!(
        denied.status(),
        StatusCode::UNAUTHORIZED,
        "the fleet view is behind the admin session guard"
    );
}

// --- Background-task health (ADR-0068 slice 4) -------------------------------------------------

/// The task-health store as an in-memory map of `task -> (last_tick_ms, detail)` — the binary upserts
/// a real table, but the handler and its staleness derivation are the same code here.
#[derive(Clone, Default)]
struct FakeTaskHealth {
    rows: Arc<Mutex<HashMap<String, (i64, serde_json::Value)>>>,
}

impl FakeTaskHealth {
    /// Seeds a loop's most recent tick.
    fn with_tick(self, task: &str, at_ms: i64, detail: serde_json::Value) -> Self {
        self.rows
            .lock()
            .expect("lock")
            .insert(task.to_owned(), (at_ms, detail));
        self
    }
}

impl TaskHealthStore for FakeTaskHealth {
    async fn record_tick(
        &self,
        task: &str,
        at: Timestamp,
        detail: &serde_json::Value,
    ) -> Result<(), TaskHealthError> {
        self.rows.lock().expect("lock").insert(
            task.to_owned(),
            (at.as_milliseconds_since_epoch(), detail.clone()),
        );
        Ok(())
    }

    async fn list_health(&self) -> Result<Vec<TaskHealth>, TaskHealthError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .map(|(task, (ms, detail))| TaskHealth {
                task: task.clone(),
                last_tick_at: Timestamp::from_milliseconds_since_epoch(*ms).expect("valid"),
                detail: detail.clone(),
            })
            .collect())
    }
}

/// The main router (for `/admin/login`) merged with the health sub-router, one shared admin store.
fn health_app(admin: FakeAdmin, health: FakeTaskHealth, expected: Vec<String>) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::health_router(health, admin, clock(), expected))
}

#[tokio::test]
async fn task_health_reports_fresh_stale_and_never_ticked() {
    // The projector ticked 10s ago on a 30s interval (fresh); the dispatcher an hour ago (stale);
    // retention is expected but has never ticked (dead since boot).
    let store = FakeTaskHealth::default()
        .with_tick(
            health::ROLLUP_PROJECTOR,
            NOW_MS - 10_000,
            serde_json::json!({ "ok": true, "interval_secs": 30, "folded": 4 }),
        )
        .with_tick(
            health::WEBHOOK_DISPATCHER,
            NOW_MS - 3_600_000,
            serde_json::json!({ "ok": true, "interval_secs": 30 }),
        );
    let expected = vec![
        health::ROLLUP_PROJECTOR.to_owned(),
        health::WEBHOOK_DISPATCHER.to_owned(),
        health::RETENTION.to_owned(),
    ];
    let router = health_app(provisioned_admin(), store, expected);
    let cookie = admin_cookie(&router).await;

    let response = router
        .oneshot(get_with_cookie("/admin/health/tasks", &cookie))
        .await
        .expect("route the health read");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(
        body["healthy"], false,
        "one expected loop is stale and one never ticked"
    );
    let tasks = body["tasks"].as_array().expect("a tasks array");
    assert_eq!(tasks.len(), 3);

    let projector = tasks
        .iter()
        .find(|task| task["task"] == health::ROLLUP_PROJECTOR)
        .expect("the projector is listed");
    assert_eq!(
        projector["healthy"], true,
        "a tick 10s ago within a 30s interval is fresh"
    );
    assert_eq!(projector["seconds_since"], 10);
    assert_eq!(
        projector["detail"]["folded"], 4,
        "the tick's detail is echoed"
    );

    let webhook = tasks
        .iter()
        .find(|task| task["task"] == health::WEBHOOK_DISPATCHER)
        .expect("the dispatcher is listed");
    assert_eq!(
        webhook["healthy"], false,
        "an hour-old tick is well past 30s times the slack"
    );

    let retention = tasks
        .iter()
        .find(|task| task["task"] == health::RETENTION)
        .expect("retention is listed even though it never ticked");
    assert_eq!(retention["expected"], true);
    assert_eq!(retention["healthy"], false, "never-ticked is never healthy");
    assert_eq!(retention["last_tick_at_ms"], serde_json::Value::Null);
}

#[tokio::test]
async fn task_health_flags_a_fresh_but_failing_loop() {
    // A loop that ticked recently but whose last tick's work failed is unhealthy — alive is not enough.
    let store = FakeTaskHealth::default().with_tick(
        health::ROLLUP_PROJECTOR,
        NOW_MS - 5_000,
        serde_json::json!({ "ok": false, "interval_secs": 30 }),
    );
    let router = health_app(
        provisioned_admin(),
        store,
        vec![health::ROLLUP_PROJECTOR.to_owned()],
    );
    let cookie = admin_cookie(&router).await;
    let body = json_body(
        router
            .oneshot(get_with_cookie("/admin/health/tasks", &cookie))
            .await
            .expect("route"),
    )
    .await;
    assert_eq!(
        body["healthy"], false,
        "a recent tick whose work failed is still unhealthy"
    );
    assert_eq!(body["tasks"][0]["healthy"], false);
}

#[tokio::test]
async fn task_health_needs_a_session() {
    let router = health_app(provisioned_admin(), FakeTaskHealth::default(), Vec::new());
    let denied = router
        .oneshot(get("/admin/health/tasks", None))
        .await
        .expect("route the health read");
    assert_eq!(
        denied.status(),
        StatusCode::UNAUTHORIZED,
        "the health view is behind the admin session guard"
    );
}

// --- Console audit trail (ADR-0069 slice 1) ----------------------------------------------------

/// The audit trail as an in-memory append-only list — the binary appends to a real table, but the
/// seam and its recent-first, tenant-scoped read are the same code here.
#[derive(Clone, Default)]
struct FakeAudit {
    entries: Arc<Mutex<Vec<AuditEntry>>>,
}

impl AuditStore for FakeAudit {
    async fn append(&self, entry: &AuditEntry) -> Result<(), AuditStoreError> {
        self.entries.lock().expect("lock").push(entry.clone());
        Ok(())
    }

    async fn list(
        &self,
        tenant: Option<TenantId>,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AuditStoreError> {
        let mut rows: Vec<AuditEntry> = self
            .entries
            .lock()
            .expect("lock")
            .iter()
            .filter(|entry| tenant.is_none() || entry.tenant_id == tenant)
            .cloned()
            .collect();
        rows.reverse(); // stored oldest-first; the read is newest-first.
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn query(&self, query: &AuditQuery) -> Result<Vec<AuditEntry>, AuditStoreError> {
        let mut rows: Vec<AuditEntry> = self
            .entries
            .lock()
            .expect("lock")
            .iter()
            .filter(|entry| query.tenant.is_none() || entry.tenant_id == query.tenant)
            .filter(|entry| {
                query
                    .entity_type
                    .as_ref()
                    .is_none_or(|value| &entry.entity_type == value)
            })
            .filter(|entry| {
                query
                    .entity_id
                    .as_ref()
                    .is_none_or(|value| &entry.entity_id == value)
            })
            .filter(|entry| {
                query
                    .action
                    .as_ref()
                    .is_none_or(|value| &entry.action == value)
            })
            .filter(|entry| {
                query
                    .actor_admin_id
                    .as_ref()
                    .is_none_or(|value| &entry.actor.admin_id == value)
            })
            .filter(|entry| {
                query
                    .since_ms
                    .is_none_or(|since| entry.at.as_milliseconds_since_epoch() >= since)
            })
            .filter(|entry| {
                query
                    .until_ms
                    .is_none_or(|until| entry.at.as_milliseconds_since_epoch() <= until)
            })
            .cloned()
            .collect();
        rows.reverse(); // stored oldest-first; the read is newest-first.
        rows.truncate(query.limit as usize);
        Ok(rows)
    }
}

#[tokio::test]
async fn audit_appends_and_lists_newest_first_scoped_by_tenant() {
    let audit = FakeAudit::default();
    let entry = |id: u128, tenant: Option<TenantId>, action: &str, at_ms: i64| AuditEntry {
        id: AuditId::new(Ulid::from_u128(id)),
        tenant_id: tenant,
        actor: AuditActor {
            admin_id: "01ADMIN0000000000000000OPS".to_owned(),
            email: "ops@pizza4ps.test".to_owned(),
            role: AdminRole::Ops,
        },
        action: action.to_owned(),
        entity_type: "store".to_owned(),
        entity_id: store_id().to_string(),
        before: None,
        after: Some(serde_json::json!({ "name": "Bến Thành" })),
        request_id: None,
        at: Timestamp::from_milliseconds_since_epoch(at_ms).expect("a valid instant"),
    };
    let mine = tenant();
    let other = TenantId::new(Ulid::from_u128(0xB0B));
    audit
        .append(&entry(1, Some(mine), "store.update", NOW_MS - 2_000))
        .await
        .expect("append 1");
    audit
        .append(&entry(2, Some(mine), "store.archive", NOW_MS - 1_000))
        .await
        .expect("append 2");
    audit
        .append(&entry(3, Some(other), "store.update", NOW_MS))
        .await
        .expect("append for another tenant");

    let listed = audit.list(Some(mine), 10).await.expect("list this tenant");
    assert_eq!(listed.len(), 2, "only this tenant's entries are listed");
    let newest = listed.first().expect("an entry");
    assert_eq!(
        newest.action, "store.archive",
        "the newest entry sorts first"
    );
    assert_eq!(
        newest.actor.email, "ops@pizza4ps.test",
        "the acting admin is snapshotted onto the entry"
    );
    assert!(
        listed.iter().any(|entry| entry.action == "store.update"),
        "the tenant's earlier entry is present too"
    );

    let all = audit.list(None, 10).await.expect("list across tenants");
    assert_eq!(all.len(), 3, "no tenant filter reads across every tenant");
}

#[tokio::test]
async fn registry_writes_record_to_the_audit_trail() {
    let audit = FakeAudit::default();
    let sink: Arc<dyn AuditRecorder> = Arc::new(AuditSink::new(audit.clone()));
    let router = registry_app_with_audit(provisioned_admin(), FakeRegistry::default(), sink);
    let cookie = admin_cookie(&router).await;

    // A tenant create mints its id server-side and, on success, records one audit entry.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/tenants",
            &serde_json::json!({ "name": "Pizza 4P's" }),
            &cookie,
        ))
        .await
        .expect("route create tenant");
    assert_eq!(created.status(), StatusCode::CREATED);
    let tenant_id = json_body(created).await["tenant_id"]
        .as_str()
        .expect("a tenant id")
        .to_owned();

    // A store create under it records a second entry, scoped to the tenant.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/stores",
            &serde_json::json!({ "tenant_id": tenant_id, "name": "Bến Thành" }),
            &cookie,
        ))
        .await
        .expect("route create store");
    assert_eq!(created.status(), StatusCode::CREATED);
    let store_id = json_body(created).await["store_id"]
        .as_str()
        .expect("a store id")
        .to_owned();

    let recorded = audit.list(None, 10).await.expect("list audit entries");
    assert_eq!(recorded.len(), 2, "each successful write records one entry");
    let store_entry = recorded
        .iter()
        .find(|entry| entry.action == "store.create")
        .expect("the store create was recorded");
    assert_eq!(store_entry.entity_type, "store");
    assert_eq!(
        store_entry.entity_id, store_id,
        "the new entity's id is recorded"
    );
    assert_eq!(
        store_entry.tenant_id.map(|id| id.to_string()),
        Some(tenant_id.clone()),
        "the entry is scoped to the owning tenant"
    );
    assert!(store_entry.before.is_none(), "a create has no prior value");
    assert!(
        store_entry.after.is_some(),
        "a create records the new value"
    );
    assert!(
        !store_entry.actor.email.is_empty() && !store_entry.actor.admin_id.is_empty(),
        "the acting admin is snapshotted onto the entry"
    );
    assert!(
        recorded.iter().any(|entry| entry.action == "tenant.create"),
        "the tenant create was recorded too"
    );
}

#[tokio::test]
async fn the_audit_read_filters_and_needs_a_session() {
    let admin = provisioned_admin();
    let audit = FakeAudit::default();
    let entry = |id: u128, action: &str, entity_type: &str, at_ms: i64| AuditEntry {
        id: AuditId::new(Ulid::from_u128(id)),
        tenant_id: Some(tenant()),
        actor: AuditActor {
            admin_id: "01ADMIN0000000000000000OPS".to_owned(),
            email: "ops@pizza4ps.test".to_owned(),
            role: AdminRole::Owner,
        },
        action: action.to_owned(),
        entity_type: entity_type.to_owned(),
        entity_id: store_id().to_string(),
        before: None,
        after: Some(serde_json::json!({ "name": "Bến Thành" })),
        request_id: None,
        at: Timestamp::from_milliseconds_since_epoch(at_ms).expect("a valid instant"),
    };
    audit
        .append(&entry(1, "store.create", "store", NOW_MS - 2_000))
        .await
        .expect("append 1");
    audit
        .append(&entry(2, "store.update", "store", NOW_MS - 1_000))
        .await
        .expect("append 2");
    audit
        .append(&entry(3, "tenant.update", "tenant", NOW_MS))
        .await
        .expect("append 3");

    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    let router = http::router(app).merge(http::audit_router(audit, admin, clock()));

    // No session → the trail is behind the guard.
    let denied = router
        .clone()
        .oneshot(get("/admin/audit", None))
        .await
        .expect("route the unauthenticated read");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let cookie = admin_cookie(&router).await;

    // Unfiltered: every entry, newest first.
    let all = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit", &cookie))
        .await
        .expect("route the read");
    assert_eq!(all.status(), StatusCode::OK);
    let all = json_body(all).await;
    let rows = all.as_array().expect("array");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["action"], "tenant.update", "newest first");
    assert_eq!(
        rows[0]["actor_email"], "ops@pizza4ps.test",
        "the actor snapshot is flattened onto the view"
    );

    // Filter by entity type.
    let stores = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit?entity_type=store", &cookie))
        .await
        .expect("route the filtered read");
    let stores = json_body(stores).await;
    assert_eq!(stores.as_array().expect("array").len(), 2);
    assert!(
        stores
            .as_array()
            .expect("array")
            .iter()
            .all(|row| row["entity_type"] == "store"),
        "only store entries survive the filter"
    );

    // Filter by action.
    let updates = router
        .oneshot(get_with_cookie("/admin/audit?action=store.update", &cookie))
        .await
        .expect("route the action-filtered read");
    let updates = json_body(updates).await;
    assert_eq!(updates.as_array().expect("array").len(), 1);
    assert_eq!(updates[0]["action"], "store.update");
}

#[tokio::test]
async fn registry_creates_and_lists_named_tenant_and_store_without_typing_a_ulid() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    let cookie = admin_cookie(&router).await;

    // Create a tenant by name; the id is minted server-side and returned once.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/tenants",
            &serde_json::json!({ "name": "Pizza 4P's" }),
            &cookie,
        ))
        .await
        .expect("route create tenant");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(created["name"], "Pizza 4P's");
    assert_eq!(created["status"], "active");
    let tenant_id = created["tenant_id"]
        .as_str()
        .expect("a tenant id")
        .to_owned();

    // It shows in the listing the picker reads.
    let listed = router
        .clone()
        .oneshot(get_with_cookie("/admin/tenants", &cookie))
        .await
        .expect("route list tenants");
    assert_eq!(listed.status(), StatusCode::OK);
    let tenants = json_body(listed).await;
    assert_eq!(tenants.as_array().expect("array").len(), 1);
    assert_eq!(tenants[0]["tenant_id"], tenant_id);

    // Create a store under it — again, no ULID typed by the operator.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/stores",
            &serde_json::json!({ "tenant_id": tenant_id, "name": "Bến Thành" }),
            &cookie,
        ))
        .await
        .expect("route create store");
    assert_eq!(created.status(), StatusCode::CREATED);
    let store = json_body(created).await;
    assert_eq!(store["name"], "Bến Thành");
    assert_eq!(store["brand_id"], serde_json::Value::Null);
    let store_id = store["store_id"].as_str().expect("a store id").to_owned();

    // And it lists for its tenant.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/stores?tenant_id={tenant_id}"),
            &cookie,
        ))
        .await
        .expect("route list stores");
    let stores = json_body(listed).await;
    assert_eq!(stores.as_array().expect("array").len(), 1);
    assert_eq!(stores[0]["store_id"], store_id);
}

#[tokio::test]
async fn registry_renames_a_tenant_and_404s_an_unknown_one() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    let cookie = admin_cookie(&router).await;

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/tenants",
            &serde_json::json!({ "name": "Placeholder" }),
            &cookie,
        ))
        .await
        .expect("route create");
    let tenant_id = json_body(created).await["tenant_id"]
        .as_str()
        .expect("a tenant id")
        .to_owned();

    // Rename it.
    let renamed = router
        .clone()
        .oneshot(patch_with_cookie(
            &format!("/admin/tenants/{tenant_id}"),
            &serde_json::json!({ "name": "Pizza 4P's", "status": "active" }),
            &cookie,
        ))
        .await
        .expect("route rename");
    assert_eq!(renamed.status(), StatusCode::OK);
    assert_eq!(json_body(renamed).await["name"], "Pizza 4P's");

    // Renaming an unknown tenant is a 404, not a silent success.
    let missing = router
        .clone()
        .oneshot(patch_with_cookie(
            &format!("/admin/tenants/{}", Ulid::from_u128(9_999)),
            &serde_json::json!({ "name": "Nope", "status": "active" }),
            &cookie,
        ))
        .await
        .expect("route rename missing");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn registry_is_behind_the_session_guard() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    // No session cookie → the guard denies before any listing is revealed.
    let denied = router
        .oneshot(get("/admin/tenants", None))
        .await
        .expect("route unauthenticated");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}

/// Seeds a console admin of `role` and a live session bound to it, returning the `name=value` cookie
/// that names that session — the way a specific-role caller is simulated end-to-end (ADR-0067).
async fn role_session_cookie(admin: &FakeAdmin, role: AdminRole, token: &str) -> String {
    let id = format!("id-{}", role.as_token());
    admin
        .create_admin_user(NewAdminUser {
            id: id.clone(),
            email: format!("{}@example.test", role.as_token()),
            name: "N".to_owned(),
            role,
            password_phc: "$argon2id$not-a-real-hash".to_owned(),
            totp_secret: b"not-a-real-totp-secret".to_vec(),
        })
        .await
        .expect("seed admin");
    let expiry = Timestamp::from_milliseconds_since_epoch(NOW_MS + 3_600_000).expect("valid");
    admin
        .create_session(NewAdminSession {
            token_hash: hash_session_token(token),
            created_at: Timestamp::from_milliseconds_since_epoch(NOW_MS).expect("valid"),
            expires_at: expiry,
            absolute_expires_at: expiry,
            idle_ttl_ms: 3_600_000,
            admin_id: Some(id.clone()),
            ip: None,
            user_agent: None,
        })
        .await
        .expect("seed session");
    format!("__Host-pos_admin_session={token}")
}

#[tokio::test]
async fn a_viewer_may_read_the_registry_but_not_create() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let router = registry_app(admin, FakeRegistry::default());

    // `console.data.read` is granted to every role, so a viewer's listing succeeds.
    let listed = router
        .clone()
        .oneshot(get_with_cookie("/admin/tenants", &cookie))
        .await
        .expect("route list");
    assert_eq!(
        listed.status(),
        StatusCode::OK,
        "a viewer may read the registry"
    );

    // Creating a tenant needs `console.orgs.manage`, which a viewer lacks — a 403, distinct from the
    // 401 an unauthenticated caller gets: the viewer is signed in, only under-privileged.
    let denied = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/tenants",
            &serde_json::json!({ "name": "X" }),
            &cookie,
        ))
        .await
        .expect("route create");
    assert_eq!(
        denied.status(),
        StatusCode::FORBIDDEN,
        "a viewer cannot create a tenant"
    );
}

#[tokio::test]
async fn an_ops_admin_cannot_create_in_the_registry() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Ops, "ops-token").await;
    let router = registry_app(admin, FakeRegistry::default());
    let denied = router
        .oneshot(post_with_cookie(
            "/admin/tenants",
            &serde_json::json!({ "name": "X" }),
            &cookie,
        ))
        .await
        .expect("route create");
    assert_eq!(
        denied.status(),
        StatusCode::FORBIDDEN,
        "ops has no tenant/brand creation"
    );
}

#[tokio::test]
async fn an_owner_session_may_create_in_the_registry() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let router = registry_app(admin, FakeRegistry::default());
    let created = router
        .oneshot(post_with_cookie(
            "/admin/tenants",
            &serde_json::json!({ "name": "Pizza 4P's" }),
            &cookie,
        ))
        .await
        .expect("route create");
    assert_eq!(
        created.status(),
        StatusCode::CREATED,
        "an owner may create a tenant"
    );
}

// --- Self-service sessions (G1 slice 4, ADR-0067) -----------------------------------------------

/// Seeds an extra live session bound to `admin_id` (a second signed-in device), for the session-list
/// tests. The idle window equals the cap, so the seeded session neither slides nor idles out during
/// a test.
async fn seed_extra_session(admin: &FakeAdmin, token: &str, admin_id: &str, ip: Option<&str>) {
    let expiry = Timestamp::from_milliseconds_since_epoch(NOW_MS + 3_600_000).expect("valid");
    admin
        .create_session(NewAdminSession {
            token_hash: hash_session_token(token),
            created_at: Timestamp::from_milliseconds_since_epoch(NOW_MS).expect("valid"),
            expires_at: expiry,
            absolute_expires_at: expiry,
            idle_ttl_ms: 3_600_000,
            admin_id: Some(admin_id.to_owned()),
            ip: ip.map(str::to_owned),
            user_agent: None,
        })
        .await
        .expect("seed extra session");
}

#[tokio::test]
async fn listing_sessions_requires_a_session() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    let denied = router
        .oneshot(get("/admin/sessions", None))
        .await
        .expect("route unauthenticated");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_admin_lists_and_revokes_their_own_sessions() {
    let admin = provisioned_admin();
    // Even a viewer — least privilege — manages their own sessions: it is self-service, not
    // role-gated.
    let cookie = role_session_cookie(&admin, AdminRole::Viewer, "current-token").await;
    seed_extra_session(&admin, "phone-token", "id-viewer", Some("203.0.113.4")).await;
    let router = registry_app(admin, FakeRegistry::default());

    let listed = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/sessions", &cookie))
            .await
            .expect("route list"),
    )
    .await;
    let sessions = listed.as_array().expect("array");
    assert_eq!(sessions.len(), 2, "both of the admin's sessions are listed");
    // Exactly one is flagged as the session making this request.
    let current: Vec<&serde_json::Value> =
        sessions.iter().filter(|s| s["current"] == true).collect();
    assert_eq!(current.len(), 1, "exactly one session is the current one");
    let other = sessions
        .iter()
        .find(|s| s["current"] == false)
        .expect("a non-current session");
    let other_id = other["id"].as_str().expect("a handle").to_owned();

    // Revoke the other device by its opaque handle.
    let revoked = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/sessions/{other_id}"),
            &cookie,
        ))
        .await
        .expect("route revoke");
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);

    let after = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/sessions", &cookie))
            .await
            .expect("route list"),
    )
    .await;
    let remaining = after.as_array().expect("array");
    assert_eq!(remaining.len(), 1, "only the current session remains");
    assert_eq!(remaining[0]["current"], true);
}

#[tokio::test]
async fn revoking_a_bad_handle_or_a_missing_session() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let router = registry_app(admin, FakeRegistry::default());

    // A non-hex handle is a 400.
    let bad = router
        .clone()
        .oneshot(delete_with_cookie("/admin/sessions/not-a-handle", &cookie))
        .await
        .expect("route revoke");
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // A well-formed handle that names no session of theirs is a 404.
    let absent_handle = "0".repeat(64);
    let absent = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/sessions/{absent_handle}"),
            &cookie,
        ))
        .await
        .expect("route revoke");
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn revoke_others_signs_out_every_session_but_the_current_one() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "keep-token").await;
    seed_extra_session(&admin, "device-a", "id-owner", None).await;
    seed_extra_session(&admin, "device-b", "id-owner", None).await;
    let router = registry_app(admin, FakeRegistry::default());

    let before = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/sessions", &cookie))
            .await
            .expect("route list"),
    )
    .await;
    assert_eq!(before.as_array().expect("array").len(), 3);

    let signed_out = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/sessions/revoke-others",
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route revoke others");
    assert_eq!(signed_out.status(), StatusCode::NO_CONTENT);

    let after = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/sessions", &cookie))
            .await
            .expect("route list"),
    )
    .await;
    let remaining = after.as_array().expect("array");
    assert_eq!(remaining.len(), 1, "only the current session survives");
    assert_eq!(remaining[0]["current"], true);
}

// --- Login rate-limit + security headers (G1 slice 5, ADR-0067) ---------------------------------

/// The main router with a specific `/admin/login` rate limit, for the throttle test.
fn login_rate_limited_router(max_attempts: usize, window_secs: u64) -> axum::Router {
    let app = CloudApp::new(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        clock(),
        provisioned_admin(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    )
    .with_login_rate_limit(max_attempts, window_secs);
    http::router(app)
}

#[tokio::test]
async fn login_is_rate_limited_after_too_many_attempts() {
    let router = login_rate_limited_router(3, 60);
    // Bogus credentials: each attempt reaches the credential check and is refused, and is recorded by
    // the limiter. The limiter runs before the credential check, so it can never leak whether the
    // credential was right.
    let bogus = serde_json::json!({ "password": "wrong-passphrase", "totp_code": "000000" });
    for _ in 0..3 {
        let refused = router
            .clone()
            .oneshot(post_json("/admin/login", &bogus))
            .await
            .expect("route login");
        assert_eq!(
            refused.status(),
            StatusCode::UNAUTHORIZED,
            "an attempt within the limit reaches the credential check"
        );
    }
    // The fourth attempt trips the limiter first: a 429 carrying a Retry-After.
    let throttled = router
        .clone()
        .oneshot(post_json("/admin/login", &bogus))
        .await
        .expect("route login");
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        throttled.headers().get("retry-after").is_some(),
        "a rate-limited login carries a Retry-After for the client"
    );
}

#[tokio::test]
async fn every_response_carries_the_admin_security_headers() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    // The layer wraps every response, so even an unauthenticated probe carries the headers.
    let response = router
        .oneshot(get("/admin/session", None))
        .await
        .expect("route session probe");
    let headers = response.headers();
    assert_eq!(
        headers
            .get("x-content-type-options")
            .expect("nosniff header")
            .to_str()
            .expect("ascii"),
        "nosniff"
    );
    assert_eq!(
        headers
            .get("x-frame-options")
            .expect("frame-options header")
            .to_str()
            .expect("ascii"),
        "DENY"
    );
    assert_eq!(
        headers
            .get("referrer-policy")
            .expect("referrer-policy header")
            .to_str()
            .expect("ascii"),
        "no-referrer"
    );
    let csp = headers
        .get("content-security-policy")
        .expect("a CSP header")
        .to_str()
        .expect("ascii");
    assert!(
        csp.contains("default-src 'self'"),
        "CSP pins the default origin"
    );
    assert!(
        csp.contains("script-src 'self'"),
        "CSP locks scripts to self"
    );
    assert!(
        csp.contains("frame-ancestors 'none'"),
        "CSP backs up X-Frame-Options against clickjacking"
    );
}

// --- Self-service security: TOTP re-enrol + recovery codes (G1 slice 6, ADR-0067) --------------

/// A router over a provisioned super-admin with a seeded active owner, so a real sign-in binds the
/// session to an `admin_users` id — the shape the recovery-code and re-enrol tests need.
async fn security_router() -> axum::Router {
    let admin = provisioned_admin();
    admin
        .create_admin_user(NewAdminUser {
            id: "id-owner".to_owned(),
            email: "owner@example.test".to_owned(),
            name: "Owner".to_owned(),
            role: AdminRole::Owner,
            // The login uses the super-admin credential, not this row's — these are placeholders.
            password_phc: "$argon2id$not-a-real-hash".to_owned(),
            totp_secret: b"not-a-real-totp-secret".to_vec(),
        })
        .await
        .expect("seed owner");
    registry_app(admin, FakeRegistry::default())
}

#[tokio::test]
async fn recovery_codes_generate_once_and_sign_in_in_place_of_totp() {
    let router = security_router().await;
    let cookie = admin_cookie(&router).await;

    // Generate the codes — returned once, with the count.
    let generated = json_body(
        router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/recovery-codes",
                &serde_json::json!({}),
                &cookie,
            ))
            .await
            .expect("route generate"),
    )
    .await;
    let codes = generated["codes"].as_array().expect("codes array");
    assert_eq!(codes.len(), 10);
    assert_eq!(generated["remaining"], 10);
    let code = codes[0].as_str().expect("a code").to_owned();

    // The status endpoint reports the count, never the codes.
    let status = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/recovery-codes", &cookie))
            .await
            .expect("route status"),
    )
    .await;
    assert_eq!(status["remaining"], 10);

    // A recovery code signs in in place of the TOTP code.
    let signed_in = router
        .clone()
        .oneshot(post_json(
            "/admin/login",
            &serde_json::json!({ "password": ADMIN_PASSWORD, "recovery_code": code }),
        ))
        .await
        .expect("route recovery login");
    assert_eq!(signed_in.status(), StatusCode::NO_CONTENT);

    // Single-use: the same code cannot sign in again.
    let replay = router
        .clone()
        .oneshot(post_json(
            "/admin/login",
            &serde_json::json!({ "password": ADMIN_PASSWORD, "recovery_code": code }),
        ))
        .await
        .expect("route recovery login");
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);

    // One code spent leaves nine.
    let status = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/recovery-codes", &cookie))
            .await
            .expect("route status"),
    )
    .await;
    assert_eq!(status["remaining"], 9);
}

#[tokio::test]
async fn totp_reenrol_needs_the_current_password() {
    let router = security_router().await;
    let cookie = admin_cookie(&router).await;

    // Signed in but wrong password: a distinct 403, the knowledge factor not re-proved.
    let refused = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/totp",
            &serde_json::json!({ "password": "wrong-password" }),
            &cookie,
        ))
        .await
        .expect("route reenrol");
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    // Correct password: a fresh one-time enrolment (QR + base32 secret).
    let enrolled = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/totp",
            &serde_json::json!({ "password": ADMIN_PASSWORD }),
            &cookie,
        ))
        .await
        .expect("route reenrol");
    assert_eq!(enrolled.status(), StatusCode::OK);
    let body = json_body(enrolled).await;
    assert!(
        body["otpauth_uri"]
            .as_str()
            .expect("an otpauth uri")
            .contains("otpauth://totp/"),
    );
    assert!(body["secret_base32"].as_str().is_some());
}

#[tokio::test]
async fn the_self_service_security_routes_require_a_session() {
    let router = security_router().await;
    for request in [
        post_json("/admin/totp", &serde_json::json!({ "password": "x" })),
        post_json("/admin/recovery-codes", &serde_json::json!({})),
        get("/admin/recovery-codes", None),
    ] {
        let denied = router.clone().oneshot(request).await.expect("route");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }
}

// --- Console identity: whoami (G1 slice 7, ADR-0067) --------------------------------------------

#[tokio::test]
async fn whoami_needs_a_session() {
    let router = security_router().await;
    let denied = router
        .oneshot(get("/admin/whoami", None))
        .await
        .expect("route whoami");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn whoami_reports_the_acting_admins_identity_and_role() {
    let router = security_router().await;
    let cookie = admin_cookie(&router).await;
    let me = json_body(
        router
            .oneshot(get_with_cookie("/admin/whoami", &cookie))
            .await
            .expect("route whoami"),
    )
    .await;
    // The session bound to the one active owner, so whoami names that owner — role included, and
    // never a credential (no password hash, no TOTP secret in the safe listing shape).
    assert_eq!(me["id"], "id-owner");
    assert_eq!(me["email"], "owner@example.test");
    assert_eq!(me["role"], "owner");
    assert_eq!(me["status"], "active");
    assert!(me.get("password_phc").is_none());
    assert!(me.get("totp_secret").is_none());
}

// --- Invitations and admin management (G1 slice 3, ADR-0067) ------------------------------------

#[tokio::test]
async fn owner_invites_and_the_invitee_self_enrols() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let router = registry_app(admin, FakeRegistry::default());

    // The owner invites an ops admin; the single-use token is returned once.
    let invited = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/invites",
            &serde_json::json!({ "email": "New.Ops@Example.test", "name": "New Ops", "role": "ops" }),
            &cookie,
        ))
        .await
        .expect("route invite");
    assert_eq!(invited.status(), StatusCode::CREATED);
    let body = json_body(invited).await;
    let token = body["token"].as_str().expect("a token").to_owned();
    assert!(body["invite_id"].as_str().is_some());

    // It lists as pending, with the email normalised to lower-case.
    let pending_invites = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/invites", &cookie))
            .await
            .expect("route list invites"),
    )
    .await;
    assert_eq!(pending_invites.as_array().expect("array").len(), 1);
    assert_eq!(pending_invites[0]["email"], "new.ops@example.test");
    assert_eq!(pending_invites[0]["role"], "ops");

    // The invitee self-enrols with the token — no session — and gets a one-time TOTP enrolment.
    let accepted = router
        .clone()
        .oneshot(post_json(
            "/admin/invites/accept",
            &serde_json::json!({ "token": token, "password": "a-strong-passphrase" }),
        ))
        .await
        .expect("route accept");
    assert_eq!(accepted.status(), StatusCode::CREATED);
    let enrolment = json_body(accepted).await;
    assert!(enrolment["otpauth_uri"].as_str().is_some());
    assert!(enrolment["secret_base32"].as_str().is_some());

    // The new admin is on the roster, the invite is no longer pending, and the token cannot be reused.
    let admin_roster = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/admins", &cookie))
            .await
            .expect("route roster"),
    )
    .await;
    let has_new = admin_roster
        .as_array()
        .expect("array")
        .iter()
        .any(|entry| entry["email"] == "new.ops@example.test" && entry["role"] == "ops");
    assert!(has_new, "the accepted admin appears on the roster");

    let remaining = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/invites", &cookie))
            .await
            .expect("route list invites"),
    )
    .await;
    assert!(remaining.as_array().expect("array").is_empty());

    let replay = router
        .oneshot(post_json(
            "/admin/invites/accept",
            &serde_json::json!({ "token": token, "password": "a-strong-passphrase" }),
        ))
        .await
        .expect("route replay");
    assert_eq!(
        replay.status(),
        StatusCode::UNAUTHORIZED,
        "an accepted invite cannot be replayed"
    );
}

#[tokio::test]
async fn an_admin_cannot_invite_an_owner() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Admin, "admin-token").await;
    let router = registry_app(admin, FakeRegistry::default());
    let denied = router
        .oneshot(post_with_cookie(
            "/admin/invites",
            &serde_json::json!({ "email": "boss@example.test", "name": "Boss", "role": "owner" }),
            &cookie,
        ))
        .await
        .expect("route invite");
    assert_eq!(
        denied.status(),
        StatusCode::FORBIDDEN,
        "an admin may not invite an owner"
    );
}

#[tokio::test]
async fn a_viewer_cannot_invite() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let router = registry_app(admin, FakeRegistry::default());
    let denied = router
        .oneshot(post_with_cookie(
            "/admin/invites",
            &serde_json::json!({ "email": "x@example.test", "name": "X", "role": "viewer" }),
            &cookie,
        ))
        .await
        .expect("route invite");
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn accept_rejects_a_bad_token_and_a_short_password() {
    let admin = provisioned_admin();
    let router = registry_app(admin, FakeRegistry::default());
    let bad_token = router
        .clone()
        .oneshot(post_json(
            "/admin/invites/accept",
            &serde_json::json!({ "token": "not-a-real-token", "password": "a-strong-passphrase" }),
        ))
        .await
        .expect("route accept");
    assert_eq!(
        bad_token.status(),
        StatusCode::UNAUTHORIZED,
        "an unknown token is a generic 401"
    );
    let short_password = router
        .oneshot(post_json(
            "/admin/invites/accept",
            &serde_json::json!({ "token": "not-a-real-token", "password": "short" }),
        ))
        .await
        .expect("route accept");
    assert_eq!(short_password.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn the_last_active_owner_cannot_be_demoted_or_suspended() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let router = registry_app(admin, FakeRegistry::default());
    let demote = router
        .clone()
        .oneshot(patch_with_cookie(
            "/admin/admins/id-owner/role",
            &serde_json::json!({ "role": "admin" }),
            &cookie,
        ))
        .await
        .expect("route demote");
    assert_eq!(
        demote.status(),
        StatusCode::CONFLICT,
        "the last active owner cannot be demoted"
    );
    let suspend = router
        .oneshot(patch_with_cookie(
            "/admin/admins/id-owner/status",
            &serde_json::json!({ "status": "suspended" }),
            &cookie,
        ))
        .await
        .expect("route suspend");
    assert_eq!(
        suspend.status(),
        StatusCode::CONFLICT,
        "the last active owner cannot be suspended"
    );
}

#[tokio::test]
async fn an_owner_can_be_demoted_when_another_owner_remains() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    // A second active owner, so demoting the first no longer removes the last owner.
    admin
        .create_admin_user(NewAdminUser {
            id: "id-owner-2".to_owned(),
            email: "owner2@example.test".to_owned(),
            name: "O2".to_owned(),
            role: AdminRole::Owner,
            password_phc: "$argon2id$not-a-real-hash".to_owned(),
            totp_secret: b"not-a-real-totp-secret".to_vec(),
        })
        .await
        .expect("seed second owner");
    let router = registry_app(admin, FakeRegistry::default());
    let demote = router
        .oneshot(patch_with_cookie(
            "/admin/admins/id-owner-2/role",
            &serde_json::json!({ "role": "admin" }),
            &cookie,
        ))
        .await
        .expect("route demote");
    assert_eq!(
        demote.status(),
        StatusCode::NO_CONTENT,
        "demotion is allowed while another owner remains"
    );
}

// --- Catalog authoring admin routes (ADR-0066) --------------------------------------------------

#[derive(Default, Clone)]
struct FakeCatalog {
    items: Arc<Mutex<Vec<CatalogItem>>>,
    tax_classes: Arc<Mutex<Vec<TaxClass>>>,
    categories: Arc<Mutex<Vec<ItemCategory>>>,
    subcategories: Arc<Mutex<Vec<ItemSubcategory>>>,
    display_categories: Arc<Mutex<Vec<DisplayCategory>>>,
    display_subcategories: Arc<Mutex<Vec<DisplaySubcategory>>>,
    layout_buttons: Arc<Mutex<Vec<LayoutButton>>>,
    modifier_groups: Arc<Mutex<Vec<ModifierGroup>>>,
    menus: Arc<Mutex<Vec<Menu>>>,
    menu_sections: Arc<Mutex<Vec<MenuSection>>>,
    placements: Arc<Mutex<Vec<MenuPlacement>>>,
}

impl CatalogStore for FakeCatalog {
    async fn create_item(&self, item: &CatalogItem) -> Result<(), CatalogStoreError> {
        self.items.lock().expect("lock").push(item.clone());
        Ok(())
    }

    async fn list_items(&self, tenant_id: TenantId) -> Result<Vec<CatalogItem>, CatalogStoreError> {
        Ok(self
            .items
            .lock()
            .expect("lock")
            .iter()
            .filter(|item| item.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_item(&self, item: &CatalogItem) -> Result<bool, CatalogStoreError> {
        let mut rows = self.items.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.menu_item_id == item.menu_item_id && row.tenant_id == item.tenant_id {
                row.name.clone_from(&item.name);
                row.tax_class_id = item.tax_class_id;
                row.item_category_id = item.item_category_id;
                row.item_subcategory_id = item.item_subcategory_id;
                row.status = item.status;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn create_tax_class(&self, tax_class: &TaxClass) -> Result<(), CatalogStoreError> {
        self.tax_classes
            .lock()
            .expect("lock")
            .push(tax_class.clone());
        Ok(())
    }

    async fn list_tax_classes(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<TaxClass>, CatalogStoreError> {
        Ok(self
            .tax_classes
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_tax_class(&self, tax_class: &TaxClass) -> Result<bool, CatalogStoreError> {
        let mut rows = self.tax_classes.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.tax_class_id == tax_class.tax_class_id && row.tenant_id == tax_class.tenant_id {
                row.name.clone_from(&tax_class.name);
                row.status = tax_class.status;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn create_item_category(&self, category: &ItemCategory) -> Result<(), CatalogStoreError> {
        self.categories.lock().expect("lock").push(category.clone());
        Ok(())
    }

    async fn list_item_categories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<ItemCategory>, CatalogStoreError> {
        Ok(self
            .categories
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_item_category(
        &self,
        category: &ItemCategory,
    ) -> Result<bool, CatalogStoreError> {
        let mut rows = self.categories.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.item_category_id == category.item_category_id
                && row.tenant_id == category.tenant_id
            {
                row.name.clone_from(&category.name);
                row.status = category.status;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn create_item_subcategory(
        &self,
        subcategory: &ItemSubcategory,
    ) -> Result<(), CatalogStoreError> {
        self.subcategories
            .lock()
            .expect("lock")
            .push(subcategory.clone());
        Ok(())
    }

    async fn list_item_subcategories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<ItemSubcategory>, CatalogStoreError> {
        Ok(self
            .subcategories
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_item_subcategory(
        &self,
        subcategory: &ItemSubcategory,
    ) -> Result<bool, CatalogStoreError> {
        let mut rows = self.subcategories.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.item_subcategory_id == subcategory.item_subcategory_id
                && row.tenant_id == subcategory.tenant_id
            {
                row.name.clone_from(&subcategory.name);
                row.item_category_id = subcategory.item_category_id;
                row.status = subcategory.status;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn create_display_category(
        &self,
        category: &DisplayCategory,
    ) -> Result<(), CatalogStoreError> {
        self.display_categories
            .lock()
            .expect("lock")
            .push(category.clone());
        Ok(())
    }

    async fn list_display_categories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<DisplayCategory>, CatalogStoreError> {
        Ok(self
            .display_categories
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_display_category(
        &self,
        category: &DisplayCategory,
    ) -> Result<bool, CatalogStoreError> {
        let mut rows = self.display_categories.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.display_category_id == category.display_category_id
                && row.tenant_id == category.tenant_id
            {
                row.name.clone_from(&category.name);
                row.status = category.status;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn create_display_subcategory(
        &self,
        subcategory: &DisplaySubcategory,
    ) -> Result<(), CatalogStoreError> {
        self.display_subcategories
            .lock()
            .expect("lock")
            .push(subcategory.clone());
        Ok(())
    }

    async fn list_display_subcategories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<DisplaySubcategory>, CatalogStoreError> {
        Ok(self
            .display_subcategories
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_display_subcategory(
        &self,
        subcategory: &DisplaySubcategory,
    ) -> Result<bool, CatalogStoreError> {
        let mut rows = self.display_subcategories.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.display_subcategory_id == subcategory.display_subcategory_id
                && row.tenant_id == subcategory.tenant_id
            {
                row.name.clone_from(&subcategory.name);
                row.display_category_id = subcategory.display_category_id;
                row.status = subcategory.status;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn set_layout_button(&self, button: &LayoutButton) -> Result<(), CatalogStoreError> {
        let mut rows = self.layout_buttons.lock().expect("lock");
        if let Some(row) = rows.iter_mut().find(|row| {
            row.tenant_id == button.tenant_id
                && row.sales_channel == button.sales_channel
                && row.menu_item_id == button.menu_item_id
        }) {
            *row = button.clone();
        } else {
            rows.push(button.clone());
        }
        Ok(())
    }

    async fn list_layout_buttons(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<LayoutButton>, CatalogStoreError> {
        Ok(self
            .layout_buttons
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn remove_layout_button(
        &self,
        tenant_id: TenantId,
        sales_channel: Open<SalesChannel>,
        menu_item_id: MenuItemId,
    ) -> Result<bool, CatalogStoreError> {
        let mut rows = self.layout_buttons.lock().expect("lock");
        let before = rows.len();
        rows.retain(|row| {
            !(row.tenant_id == tenant_id
                && row.sales_channel == sales_channel
                && row.menu_item_id == menu_item_id)
        });
        Ok(rows.len() != before)
    }

    async fn create_modifier_group(&self, group: &ModifierGroup) -> Result<(), CatalogStoreError> {
        self.modifier_groups
            .lock()
            .expect("lock")
            .push(group.clone());
        Ok(())
    }

    async fn list_modifier_groups(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<ModifierGroup>, CatalogStoreError> {
        Ok(self
            .modifier_groups
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_modifier_group(
        &self,
        group: &ModifierGroup,
    ) -> Result<bool, CatalogStoreError> {
        let mut rows = self.modifier_groups.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.modifier_group_id == group.modifier_group_id && row.tenant_id == group.tenant_id
            {
                *row = group.clone();
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn create_menu(&self, menu: &Menu) -> Result<(), CatalogStoreError> {
        self.menus.lock().expect("lock").push(menu.clone());
        Ok(())
    }

    async fn list_menus(&self, tenant_id: TenantId) -> Result<Vec<Menu>, CatalogStoreError> {
        Ok(self
            .menus
            .lock()
            .expect("lock")
            .iter()
            .filter(|menu| menu.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_menu(&self, menu: &Menu) -> Result<bool, CatalogStoreError> {
        let mut rows = self.menus.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.menu_id == menu.menu_id && row.tenant_id == menu.tenant_id {
                row.name.clone_from(&menu.name);
                row.parent_menu_id = menu.parent_menu_id;
                row.status = menu.status;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn create_menu_section(&self, section: &MenuSection) -> Result<(), CatalogStoreError> {
        self.menu_sections
            .lock()
            .expect("lock")
            .push(section.clone());
        Ok(())
    }

    async fn list_menu_sections(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> Result<Vec<MenuSection>, CatalogStoreError> {
        Ok(self
            .menu_sections
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant_id && row.menu_id == menu_id)
            .cloned()
            .collect())
    }

    async fn update_menu_section(&self, section: &MenuSection) -> Result<bool, CatalogStoreError> {
        let mut rows = self.menu_sections.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.menu_section_id == section.menu_section_id && row.tenant_id == section.tenant_id
            {
                row.name.clone_from(&section.name);
                row.sort = section.sort;
                row.status = section.status;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn set_placement(&self, placement: &MenuPlacement) -> Result<(), CatalogStoreError> {
        let mut rows = self.placements.lock().expect("lock");
        if let Some(row) = rows.iter_mut().find(|row| {
            row.tenant_id == placement.tenant_id
                && row.menu_id == placement.menu_id
                && row.menu_item_id == placement.menu_item_id
        }) {
            *row = placement.clone();
        } else {
            rows.push(placement.clone());
        }
        Ok(())
    }

    async fn list_placements(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> Result<Vec<MenuPlacement>, CatalogStoreError> {
        Ok(self
            .placements
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant_id && row.menu_id == menu_id)
            .cloned()
            .collect())
    }

    async fn remove_placement(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
        menu_item_id: MenuItemId,
    ) -> Result<bool, CatalogStoreError> {
        let mut rows = self.placements.lock().expect("lock");
        let before = rows.len();
        rows.retain(|row| {
            !(row.tenant_id == tenant_id
                && row.menu_id == menu_id
                && row.menu_item_id == menu_item_id)
        });
        Ok(rows.len() != before)
    }
}

/// The main router (for `/admin/login`) and the catalog sub-router, sharing one admin store.
fn catalog_app(admin: FakeAdmin, catalog: FakeCatalog) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::catalog_router(
        catalog,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// A ULID string an operator never types — the routes accept it in the body/path, the fake scopes by
/// it. Distinct constants keep the tenant, a menu, an item and a tax class from colliding.
fn ulid_text(n: u128) -> String {
    Ulid::from_u128(n).to_string()
}

#[tokio::test]
async fn catalog_creates_and_lists_an_item_and_a_menu() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/items",
            &serde_json::json!({ "tenant_id": tenant, "name": "Margherita", "tax_class_id": ulid_text(7) }),
            &cookie,
        ))
        .await
        .expect("route create item");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(created["name"], "Margherita");
    assert_eq!(created["status"], "active");
    let item_id = created["menu_item_id"]
        .as_str()
        .expect("an item id")
        .to_owned();

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/items?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list items");
    assert_eq!(listed.status(), StatusCode::OK);
    let items = json_body(listed).await;
    assert_eq!(items.as_array().expect("array").len(), 1);
    assert_eq!(items[0]["menu_item_id"], item_id);

    // A menu, optionally with a parent — created by name, id minted server-side.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/menus",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard" }),
            &cookie,
        ))
        .await
        .expect("route create menu");
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(
        json_body(created).await["parent_menu_id"],
        serde_json::Value::Null
    );

    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/menus?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list menus");
    assert_eq!(json_body(listed).await.as_array().expect("array").len(), 1);
}

#[tokio::test]
async fn catalog_creates_lists_and_renames_a_tax_class() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    // Created by name, the id minted server-side — an operator never types a tax-class ULID.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/tax-classes",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard 10%" }),
            &cookie,
        ))
        .await
        .expect("route create tax class");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(created["name"], "Standard 10%");
    assert_eq!(created["status"], "active");
    let tax_class_id = created["tax_class_id"]
        .as_str()
        .expect("a tax class id")
        .to_owned();

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/tax-classes?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list tax classes");
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(json_body(listed).await.as_array().expect("array").len(), 1);

    // Rename + archive in one PATCH.
    let renamed = router
        .clone()
        .oneshot(patch_with_cookie(
            &format!("/admin/catalog/tax-classes/{tax_class_id}"),
            &serde_json::json!({ "tenant_id": tenant, "name": "Alcohol", "status": "archived" }),
            &cookie,
        ))
        .await
        .expect("route rename tax class");
    assert_eq!(renamed.status(), StatusCode::OK);
    let renamed = json_body(renamed).await;
    assert_eq!(renamed["name"], "Alcohol");
    assert_eq!(renamed["status"], "archived");

    // A PATCH to an unknown id is a 404, not a silent success.
    let missing = router
        .oneshot(patch_with_cookie(
            &format!("/admin/catalog/tax-classes/{}", ulid_text(999)),
            &serde_json::json!({ "tenant_id": tenant, "name": "Nope", "status": "active" }),
            &cookie,
        ))
        .await
        .expect("route rename unknown tax class");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn catalog_item_taxonomy_categories_subcategories_and_item_linkage() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    // A category, by name.
    let category = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/item-categories",
            &serde_json::json!({ "tenant_id": tenant, "name": "Pizza" }),
            &cookie,
        ))
        .await
        .expect("route create category");
    assert_eq!(category.status(), StatusCode::CREATED);
    let category_id = json_body(category).await["item_category_id"]
        .as_str()
        .expect("a category id")
        .to_owned();

    // A sub-category under it.
    let subcategory = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/item-subcategories",
            &serde_json::json!({ "tenant_id": tenant, "item_category_id": category_id, "name": "Thin crust" }),
            &cookie,
        ))
        .await
        .expect("route create subcategory");
    assert_eq!(subcategory.status(), StatusCode::CREATED);
    let subcategory = json_body(subcategory).await;
    assert_eq!(subcategory["item_category_id"], category_id);
    let subcategory_id = subcategory["item_subcategory_id"]
        .as_str()
        .expect("a sub-category id")
        .to_owned();

    // An item that references both — the linkage round-trips through create and list.
    let item = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/items",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Margherita",
                "tax_class_id": ulid_text(7),
                "item_category_id": category_id,
                "item_subcategory_id": subcategory_id,
            }),
            &cookie,
        ))
        .await
        .expect("route create item");
    assert_eq!(item.status(), StatusCode::CREATED);
    let item = json_body(item).await;
    assert_eq!(item["item_category_id"], category_id);
    assert_eq!(item["item_subcategory_id"], subcategory_id);

    let categories = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/item-categories?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list categories");
    assert_eq!(
        json_body(categories).await.as_array().expect("array").len(),
        1
    );

    let subcategories = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/item-subcategories?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list subcategories");
    assert_eq!(
        json_body(subcategories)
            .await
            .as_array()
            .expect("array")
            .len(),
        1
    );
}

#[tokio::test]
async fn catalog_display_taxonomy_and_layout_buttons_round_trip() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let category = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/display-categories",
            &serde_json::json!({ "tenant_id": tenant, "name": "Summer specials" }),
            &cookie,
        ))
        .await
        .expect("route create display category");
    assert_eq!(category.status(), StatusCode::CREATED);
    let category_id = json_body(category).await["display_category_id"]
        .as_str()
        .expect("a display category id")
        .to_owned();

    let subcategory = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/display-subcategories",
            &serde_json::json!({ "tenant_id": tenant, "display_category_id": category_id, "name": "Cold" }),
            &cookie,
        ))
        .await
        .expect("route create display subcategory");
    assert_eq!(subcategory.status(), StatusCode::CREATED);

    // Upsert a button for an item onto the dine-in channel under the category.
    let item = ulid_text(500);
    let placed = router
        .clone()
        .oneshot(put_with_cookie(
            &format!("/admin/catalog/layout-buttons/SALES_CHANNEL_DINE_IN/{item}"),
            &serde_json::json!({
                "tenant_id": tenant,
                "display_category_id": category_id,
                "label": "Margherita",
                "grid_column": 0,
                "grid_row": 0,
                "sort": 0,
            }),
            &cookie,
        ))
        .await
        .expect("route set layout button");
    assert_eq!(placed.status(), StatusCode::OK);

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/layout-buttons?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list layout buttons");
    let buttons = json_body(listed).await;
    assert_eq!(buttons.as_array().expect("array").len(), 1);
    assert_eq!(buttons[0]["sales_channel"], "SALES_CHANNEL_DINE_IN");

    // Remove it — 204, then the list is empty.
    let removed = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!(
                "/admin/catalog/layout-buttons/SALES_CHANNEL_DINE_IN/{item}?tenant_id={tenant}"
            ),
            &cookie,
        ))
        .await
        .expect("route remove layout button");
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);

    let empty = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/layout-buttons?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list layout buttons again");
    assert_eq!(json_body(empty).await.as_array().expect("array").len(), 0);
}

#[tokio::test]
async fn catalog_modifier_groups_round_trip_with_members_and_attachments() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let modifier = ulid_text(600);
    let pizza = ulid_text(500);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/modifier-groups",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Size",
                "min_select": 1,
                "max_select": 1,
                "member_item_ids": [modifier],
                "attached_item_ids": [pizza],
            }),
            &cookie,
        ))
        .await
        .expect("route create modifier group");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(created["name"], "Size");
    assert_eq!(created["min_select"], 1);
    assert_eq!(created["member_item_ids"][0], modifier);
    assert_eq!(created["attached_item_ids"][0], pizza);
    let group_id = created["modifier_group_id"]
        .as_str()
        .expect("a group id")
        .to_owned();

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/modifier-groups?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list modifier groups");
    assert_eq!(json_body(listed).await.as_array().expect("array").len(), 1);

    // Update the rule + archive in one PATCH.
    let updated = router
        .clone()
        .oneshot(patch_with_cookie(
            &format!("/admin/catalog/modifier-groups/{group_id}"),
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Size",
                "min_select": 0,
                "max_select": 2,
                "member_item_ids": [modifier],
                "attached_item_ids": [],
                "status": "archived",
            }),
            &cookie,
        ))
        .await
        .expect("route update modifier group");
    assert_eq!(updated.status(), StatusCode::OK);
    let updated = json_body(updated).await;
    assert_eq!(updated["max_select"], 2);
    assert_eq!(updated["status"], "archived");
    assert_eq!(
        updated["attached_item_ids"]
            .as_array()
            .expect("array")
            .len(),
        0
    );

    // A malformed member id is rejected.
    let bad = router
        .oneshot(post_with_cookie(
            "/admin/catalog/modifier-groups",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Broken",
                "member_item_ids": ["not-a-ulid"],
            }),
            &cookie,
        ))
        .await
        .expect("route create with bad member");
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn catalog_upserts_lists_and_removes_a_placement() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let menu = ulid_text(10);
    let item = ulid_text(500);
    let base = format!("/admin/catalog/menus/{menu}/placements");

    let set = |price: i64| {
        serde_json::json!({
            "tenant_id": tenant,
            "prices": [{ "sales_channel": "DINE_IN", "unit_price": { "currency_code": "VND", "amount_minor": price } }],
            "available": true,
        })
    };

    // Upsert the placement, then upsert it again with a new price — the pair is replaced, not doubled.
    for price in [150_000, 160_000] {
        let put = router
            .clone()
            .oneshot(put_with_cookie(
                &format!("{base}/{item}"),
                &set(price),
                &cookie,
            ))
            .await
            .expect("route upsert placement");
        assert_eq!(put.status(), StatusCode::OK);
    }

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{base}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list placements");
    let rows = json_body(listed).await;
    assert_eq!(
        rows.as_array().expect("array").len(),
        1,
        "the pair replaces, not appends"
    );
    assert_eq!(rows[0]["prices"][0]["unit_price"]["amount_minor"], 160_000);

    // Remove it, then removing it again is a 404.
    let removed = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("{base}/{item}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route remove placement");
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);

    let gone = router
        .oneshot(delete_with_cookie(
            &format!("{base}/{item}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route remove missing");
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn catalog_menu_sections_group_placements_within_a_menu() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let menu = ulid_text(10);
    let item = ulid_text(500);
    let sections_base = format!("/admin/catalog/menus/{menu}/sections");

    // Create a section under the menu.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            &sections_base,
            &serde_json::json!({ "tenant_id": tenant, "name": "Starters", "sort": 1 }),
            &cookie,
        ))
        .await
        .expect("route create section");
    assert_eq!(created.status(), StatusCode::CREATED);
    let section = json_body(created).await;
    let section_id = section["menu_section_id"].as_str().expect("id").to_owned();
    assert_eq!(section["name"], "Starters");

    // List sections — the new one is there.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{sections_base}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list sections");
    let rows = json_body(listed).await;
    assert_eq!(rows.as_array().expect("array").len(), 1);
    assert_eq!(rows[0]["menu_section_id"], section_id);

    // Rename and re-sort it.
    let renamed = router
        .clone()
        .oneshot(patch_with_cookie(
            &format!("{sections_base}/{section_id}"),
            &serde_json::json!({
                "tenant_id": tenant, "name": "Appetizers", "sort": 2, "status": "active",
            }),
            &cookie,
        ))
        .await
        .expect("route rename section");
    assert_eq!(renamed.status(), StatusCode::OK);
    assert_eq!(json_body(renamed).await["name"], "Appetizers");

    // A placement can name the section it sits under, and the listing carries it back.
    let placement = router
        .clone()
        .oneshot(put_with_cookie(
            &format!("/admin/catalog/menus/{menu}/placements/{item}"),
            &serde_json::json!({
                "tenant_id": tenant,
                "menu_section_id": section_id,
                "prices": [{ "sales_channel": "DINE_IN", "unit_price": { "currency_code": "VND", "amount_minor": 150_000 } }],
                "available": true,
            }),
            &cookie,
        ))
        .await
        .expect("route set placement in section");
    assert_eq!(placement.status(), StatusCode::OK);

    let placements = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/menus/{menu}/placements?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list placements");
    let rows = json_body(placements).await;
    assert_eq!(rows[0]["menu_section_id"], section_id);
}

#[tokio::test]
async fn catalog_is_behind_the_session_guard() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    // A well-formed request (valid `tenant_id`) but no session cookie → the guard denies before any
    // listing is revealed. The query is well-formed so the guard, not the extractor, is what refuses.
    let denied = router
        .oneshot(get(
            &format!("/admin/catalog/items?tenant_id={}", ulid_text(1)),
            None,
        ))
        .await
        .expect("route unauthenticated");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}

/// The main router (for login + the effective-config read), the catalog CRUD router, and the publish
/// router — all sharing one admin, one catalog, and one config-tree store, as production merges them.
fn catalog_publish_app(
    admin: FakeAdmin,
    catalog: FakeCatalog,
    config_trees: FakeConfigTrees,
) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    );
    http::router(app)
        .merge(http::catalog_router(
            catalog.clone(),
            admin.clone(),
            clock(),
            Arc::new(NoopAuditRecorder),
        ))
        .merge(http::catalog_publish_router(
            catalog,
            config_trees,
            admin,
            clock(),
            Arc::new(NoopAuditRecorder),
        ))
}

#[tokio::test]
async fn publishing_a_menu_writes_the_compiled_book_onto_the_store_config() {
    let router = catalog_publish_app(
        provisioned_admin(),
        FakeCatalog::default(),
        FakeConfigTrees::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let store = ulid_text(2);

    // Author an item, a menu, and a dine-in placement.
    let item = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/items",
            &serde_json::json!({ "tenant_id": tenant, "name": "Margherita", "tax_class_id": ulid_text(7) }),
            &cookie,
        ))
        .await
        .expect("route create item");
    let item_id = json_body(item).await["menu_item_id"]
        .as_str()
        .expect("an item id")
        .to_owned();

    let menu = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/menus",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard" }),
            &cookie,
        ))
        .await
        .expect("route create menu");
    let menu_id = json_body(menu).await["menu_id"]
        .as_str()
        .expect("a menu id")
        .to_owned();

    let placed = router
        .clone()
        .oneshot(put_with_cookie(
            &format!("/admin/catalog/menus/{menu_id}/placements/{item_id}"),
            &serde_json::json!({
                "tenant_id": tenant,
                "prices": [{ "sales_channel": "SALES_CHANNEL_DINE_IN", "unit_price": { "currency_code": "VND", "amount_minor": 150_000 } }],
                "available": true,
            }),
            &cookie,
        ))
        .await
        .expect("route place item");
    assert_eq!(placed.status(), StatusCode::OK);

    // Publish the menu to the store.
    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/publish",
            &serde_json::json!({ "tenant_id": tenant, "store_id": store, "menu_id": menu_id }),
            &cookie,
        ))
        .await
        .expect("route publish");
    assert_eq!(published.status(), StatusCode::OK);

    // The store's effective config now carries the compiled MenuBook on its `menu` node.
    let effective = router
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store}/config?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route effective config");
    assert_eq!(effective.status(), StatusCode::OK);
    let doc = json_body(effective).await;
    let dine_in = &doc["menu"]["channels"][0];
    assert_eq!(dine_in["sales_channel"], "SALES_CHANNEL_DINE_IN");
    let entry = &dine_in["catalog"]["items"][0];
    assert_eq!(entry["menu_item_id"], item_id);
    assert_eq!(entry["unit_price"]["amount_minor"], 150_000);
    assert_eq!(entry["display_name"], "Margherita");
}

#[tokio::test]
async fn publishing_an_unknown_menu_is_refused() {
    let router = catalog_publish_app(
        provisioned_admin(),
        FakeCatalog::default(),
        FakeConfigTrees::default(),
    );
    let cookie = admin_cookie(&router).await;
    // No menu authored → the compiler refuses with a named error, surfaced as 422 (not a store 5xx).
    let refused = router
        .oneshot(post_with_cookie(
            "/admin/catalog/publish",
            &serde_json::json!({ "tenant_id": ulid_text(1), "store_id": ulid_text(2), "menu_id": ulid_text(10) }),
            &cookie,
        ))
        .await
        .expect("route publish unknown menu");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn publishing_also_writes_the_compiled_layout_onto_the_store_config() {
    let router = catalog_publish_app(
        provisioned_admin(),
        FakeCatalog::default(),
        FakeConfigTrees::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let store = ulid_text(2);

    // A menu to publish (empty is fine — the layout is compiled independently).
    let menu = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/menus",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard" }),
            &cookie,
        ))
        .await
        .expect("route create menu");
    let menu_id = json_body(menu).await["menu_id"]
        .as_str()
        .expect("a menu id")
        .to_owned();

    // A display category and a dine-in layout button under it.
    let category = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/display-categories",
            &serde_json::json!({ "tenant_id": tenant, "name": "Pizza" }),
            &cookie,
        ))
        .await
        .expect("route create display category");
    let category_id = json_body(category).await["display_category_id"]
        .as_str()
        .expect("a display category id")
        .to_owned();

    let item = ulid_text(500);
    let placed = router
        .clone()
        .oneshot(put_with_cookie(
            &format!("/admin/catalog/layout-buttons/SALES_CHANNEL_DINE_IN/{item}"),
            &serde_json::json!({
                "tenant_id": tenant,
                "display_category_id": category_id,
                "label": "Margherita",
                "grid_column": 0,
                "grid_row": 0,
                "sort": 0,
            }),
            &cookie,
        ))
        .await
        .expect("route set layout button");
    assert_eq!(placed.status(), StatusCode::OK);

    // Publish the menu; the layout rides along on the same publish.
    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/publish",
            &serde_json::json!({ "tenant_id": tenant, "store_id": store, "menu_id": menu_id }),
            &cookie,
        ))
        .await
        .expect("route publish");
    assert_eq!(published.status(), StatusCode::OK);

    // The store's effective config now carries a compiled DisplayPlan on its `layout` node.
    let effective = router
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store}/config?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route effective config");
    let doc = json_body(effective).await;
    let channel = &doc["layout"]["channels"][0];
    assert_eq!(channel["sales_channel"], "SALES_CHANNEL_DINE_IN");
    let category = &channel["plan"]["categories"][0];
    assert_eq!(category["name"], "Pizza");
    assert_eq!(category["buttons"][0]["label"], "Margherita");
    assert_eq!(category["buttons"][0]["menu_item_id"], item);
}

// --- People & access (Track M1, ADR-0070) ------------------------------------------------------

/// The employee store as an in-memory list — the binary writes a tenant-scoped table, but the seam
/// (create / list / get / update / set-or-reset PIN) is the same code here. The PIN is held only as
/// the opaque hash the caller passed, and never returned by a read — only whether one is set.
#[derive(Clone, Default)]
struct FakeEmployees {
    rows: Arc<Mutex<Vec<FakeEmployeeRow>>>,
}

#[derive(Clone)]
struct FakeEmployeeRow {
    employee_id: EmployeeId,
    tenant_id: TenantId,
    code: String,
    name: String,
    status: EntityStatus,
    pin_phc: Option<String>,
}

impl FakeEmployeeRow {
    fn view(&self) -> Employee {
        Employee {
            employee_id: self.employee_id,
            tenant_id: self.tenant_id,
            code: self.code.clone(),
            name: self.name.clone(),
            status: self.status,
            has_pin: self.pin_phc.is_some(),
        }
    }
}

impl EmployeeStore for FakeEmployees {
    async fn create(&self, employee: &NewEmployee) -> Result<(), EmployeeStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        if rows
            .iter()
            .any(|row| row.tenant_id == employee.tenant_id && row.code == employee.code)
        {
            return Err(EmployeeStoreError::new(
                "duplicate staff code within the tenant",
            ));
        }
        rows.push(FakeEmployeeRow {
            employee_id: employee.employee_id,
            tenant_id: employee.tenant_id,
            code: employee.code.clone(),
            name: employee.name.clone(),
            status: EntityStatus::Active,
            pin_phc: None,
        });
        Ok(())
    }

    async fn list(&self, tenant: TenantId) -> Result<Vec<Employee>, EmployeeStoreError> {
        let mut rows: Vec<Employee> = self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant)
            .map(FakeEmployeeRow::view)
            .collect();
        rows.reverse(); // stored oldest-first; the read is newest-first.
        Ok(rows)
    }

    async fn get(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Option<Employee>, EmployeeStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|row| row.tenant_id == tenant && row.employee_id == employee_id)
            .map(FakeEmployeeRow::view))
    }

    async fn update(&self, employee: &EmployeeUpdate) -> Result<bool, EmployeeStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|row| {
            row.tenant_id == employee.tenant_id && row.employee_id == employee.employee_id
        }) else {
            return Ok(false);
        };
        row.name.clone_from(&employee.name);
        row.status = employee.status;
        Ok(true)
    }

    async fn set_pin(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
        pin_phc: &str,
    ) -> Result<bool, EmployeeStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        let Some(row) = rows
            .iter_mut()
            .find(|row| row.tenant_id == tenant && row.employee_id == employee_id)
        else {
            return Ok(false);
        };
        row.pin_phc = Some(pin_phc.to_owned());
        Ok(true)
    }

    async fn pin_phc(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Option<String>, EmployeeStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|row| row.tenant_id == tenant && row.employee_id == employee_id)
            .and_then(|row| row.pin_phc.clone()))
    }
}

#[tokio::test]
async fn employee_store_creates_lists_updates_and_sets_pin_scoped_by_tenant() {
    let store = FakeEmployees::default();
    let mine = tenant();
    let other = TenantId::new(Ulid::from_u128(0xB0B));
    let alice = EmployeeId::new(Ulid::from_u128(1));
    let bob = EmployeeId::new(Ulid::from_u128(2));

    store
        .create(&NewEmployee {
            employee_id: alice,
            tenant_id: mine,
            code: "A01".to_owned(),
            name: "Alice".to_owned(),
        })
        .await
        .expect("create alice");
    store
        .create(&NewEmployee {
            employee_id: bob,
            tenant_id: other,
            code: "A01".to_owned(),
            name: "Bob".to_owned(),
        })
        .await
        .expect("a duplicate code is fine under a different tenant");

    // A duplicate code within the same tenant is refused.
    assert!(
        store
            .create(&NewEmployee {
                employee_id: EmployeeId::new(Ulid::from_u128(3)),
                tenant_id: mine,
                code: "A01".to_owned(),
                name: "Clone".to_owned(),
            })
            .await
            .is_err(),
        "staff codes are unique within a tenant"
    );

    // Listing is tenant-scoped and a fresh employee has no PIN.
    let listed = store.list(mine).await.expect("list");
    assert_eq!(listed.len(), 1, "only this tenant's employees");
    assert_eq!(listed[0].name, "Alice");
    assert!(!listed[0].has_pin, "a new employee has no PIN set");

    // Rename + archive.
    assert!(
        store
            .update(&EmployeeUpdate {
                employee_id: alice,
                tenant_id: mine,
                name: "Alice Nguyen".to_owned(),
                status: EntityStatus::Archived,
            })
            .await
            .expect("update"),
        "the row was found and changed"
    );
    let alice_view = store.get(mine, alice).await.expect("get").expect("present");
    assert_eq!(alice_view.name, "Alice Nguyen");
    assert_eq!(alice_view.status, EntityStatus::Archived);

    // Setting a PIN flips has_pin and round-trips the (opaque) hash — which the read never exposes.
    assert!(
        store
            .set_pin(mine, alice, "argon2id$fake$hash")
            .await
            .expect("set pin"),
        "the row was found"
    );
    assert!(
        store
            .get(mine, alice)
            .await
            .expect("get")
            .expect("present")
            .has_pin
    );
    assert_eq!(
        store.pin_phc(mine, alice).await.expect("pin"),
        Some("argon2id$fake$hash".to_owned()),
        "the trusted publish path reads the stored hash back"
    );

    // Cross-tenant isolation: mine cannot see or address the other tenant's employee.
    assert!(store.get(mine, bob).await.expect("get").is_none());
    assert!(
        !store
            .set_pin(mine, bob, "x")
            .await
            .expect("set pin across tenant"),
        "a PIN set is scoped to the tenant — no row matched"
    );
}

/// The role-template store as an in-memory list — the same seam the binary implements over a
/// tenant-scoped table. Roles are archived, never removed.
#[derive(Clone, Default)]
struct FakeRoleTemplates {
    rows: Arc<Mutex<Vec<RoleTemplate>>>,
}

impl RoleTemplateStore for FakeRoleTemplates {
    async fn create(&self, template: &NewRoleTemplate) -> Result<(), RoleTemplateStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        if rows
            .iter()
            .any(|row| row.tenant_id == template.tenant_id && row.name == template.name)
        {
            return Err(RoleTemplateStoreError::new(
                "duplicate role name within the tenant",
            ));
        }
        rows.push(RoleTemplate {
            role_template_id: template.role_template_id,
            tenant_id: template.tenant_id,
            name: template.name.clone(),
            permissions: template.permissions.clone(),
            status: EntityStatus::Active,
        });
        Ok(())
    }

    async fn list(&self, tenant: TenantId) -> Result<Vec<RoleTemplate>, RoleTemplateStoreError> {
        let mut rows: Vec<RoleTemplate> = self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant)
            .cloned()
            .collect();
        rows.reverse();
        Ok(rows)
    }

    async fn get(
        &self,
        tenant: TenantId,
        role_template_id: RoleTemplateId,
    ) -> Result<Option<RoleTemplate>, RoleTemplateStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|row| row.tenant_id == tenant && row.role_template_id == role_template_id)
            .cloned())
    }

    async fn update(&self, template: &RoleTemplateUpdate) -> Result<bool, RoleTemplateStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|row| {
            row.tenant_id == template.tenant_id && row.role_template_id == template.role_template_id
        }) else {
            return Ok(false);
        };
        row.name.clone_from(&template.name);
        row.permissions.clone_from(&template.permissions);
        row.status = template.status;
        Ok(true)
    }
}

/// The assignment store as an in-memory list — the same seam the binary implements. An assignment is
/// removed (offboarding), not archived.
#[derive(Clone, Default)]
struct FakeAssignments {
    rows: Arc<Mutex<Vec<Assignment>>>,
}

impl AssignmentStore for FakeAssignments {
    async fn assign(&self, assignment: &NewAssignment) -> Result<(), AssignmentStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        if rows.iter().any(|row| {
            row.tenant_id == assignment.tenant_id
                && row.employee_id == assignment.employee_id
                && row.store_id == assignment.store_id
        }) {
            return Err(AssignmentStoreError::new(
                "the employee is already assigned to that store",
            ));
        }
        rows.push(Assignment {
            assignment_id: assignment.assignment_id,
            tenant_id: assignment.tenant_id,
            employee_id: assignment.employee_id,
            store_id: assignment.store_id,
            role_template_id: assignment.role_template_id,
        });
        Ok(())
    }

    async fn list_for_store(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant && row.store_id == store_id)
            .cloned()
            .collect())
    }

    async fn list_for_employee(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant && row.employee_id == employee_id)
            .cloned()
            .collect())
    }

    async fn remove(
        &self,
        tenant: TenantId,
        assignment_id: AssignmentId,
    ) -> Result<bool, AssignmentStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        let before = rows.len();
        rows.retain(|row| !(row.tenant_id == tenant && row.assignment_id == assignment_id));
        Ok(rows.len() != before)
    }
}

#[test]
fn permission_catalogue_matches_pos_core_and_validates_ids() {
    let catalogue = permission_catalogue();
    assert!(!catalogue.is_empty(), "the catalogue is non-empty");
    // Every catalogue id is accepted, and something outside it is rejected — the check the routes use
    // so a role template never stores a permission that is not in the pos-core registry (§9).
    for info in &catalogue {
        assert!(is_known_permission(info.id), "{} is known", info.id);
    }
    assert!(is_known_permission("billing.discount.apply"));
    assert!(!is_known_permission("not.a.real.permission"));
}

#[tokio::test]
async fn role_template_store_creates_lists_updates_scoped_by_tenant() {
    let store = FakeRoleTemplates::default();
    let mine = tenant();
    let other = TenantId::new(Ulid::from_u128(0xB0B));
    let cashier = RoleTemplateId::new(Ulid::from_u128(10));

    store
        .create(&NewRoleTemplate {
            role_template_id: cashier,
            tenant_id: mine,
            name: "Cashier".to_owned(),
            permissions: vec![
                "billing.discount.apply".to_owned(),
                "sales.item.open".to_owned(),
            ],
        })
        .await
        .expect("create cashier");
    // The same name is fine under another tenant — templates are per-tenant.
    store
        .create(&NewRoleTemplate {
            role_template_id: RoleTemplateId::new(Ulid::from_u128(11)),
            tenant_id: other,
            name: "Cashier".to_owned(),
            permissions: vec![],
        })
        .await
        .expect("another tenant's Cashier");
    assert!(
        store
            .create(&NewRoleTemplate {
                role_template_id: RoleTemplateId::new(Ulid::from_u128(12)),
                tenant_id: mine,
                name: "Cashier".to_owned(),
                permissions: vec![],
            })
            .await
            .is_err(),
        "role names are unique within a tenant"
    );

    let listed = store.list(mine).await.expect("list");
    assert_eq!(listed.len(), 1, "only this tenant's roles");
    assert_eq!(listed[0].permissions.len(), 2);

    // Edit the permission set and archive.
    assert!(
        store
            .update(&RoleTemplateUpdate {
                role_template_id: cashier,
                tenant_id: mine,
                name: "Cashier".to_owned(),
                permissions: vec!["sales.item.open".to_owned()],
                status: EntityStatus::Archived,
            })
            .await
            .expect("update")
    );
    let view = store
        .get(mine, cashier)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(view.permissions, vec!["sales.item.open".to_owned()]);
    assert_eq!(view.status, EntityStatus::Archived);
}

#[tokio::test]
async fn assignment_store_binds_person_to_store_and_removes_scoped_by_tenant() {
    let store = FakeAssignments::default();
    let mine = tenant();
    let other = TenantId::new(Ulid::from_u128(0xB0B));
    let where_they_work = store_id();
    let alice = EmployeeId::new(Ulid::from_u128(1));
    let cashier = RoleTemplateId::new(Ulid::from_u128(10));
    let assignment = AssignmentId::new(Ulid::from_u128(20));

    store
        .assign(&NewAssignment {
            assignment_id: assignment,
            tenant_id: mine,
            employee_id: alice,
            store_id: where_they_work,
            role_template_id: cashier,
        })
        .await
        .expect("assign alice");
    // The same person at the same store twice is refused.
    assert!(
        store
            .assign(&NewAssignment {
                assignment_id: AssignmentId::new(Ulid::from_u128(21)),
                tenant_id: mine,
                employee_id: alice,
                store_id: where_they_work,
                role_template_id: cashier,
            })
            .await
            .is_err(),
        "a person is assigned to a store at most once"
    );

    // Readable both ways, and tenant-scoped.
    assert_eq!(
        store
            .list_for_store(mine, where_they_work)
            .await
            .expect("by store")
            .len(),
        1
    );
    assert_eq!(
        store
            .list_for_employee(mine, alice)
            .await
            .expect("by employee")
            .len(),
        1
    );
    assert!(
        store
            .list_for_store(other, where_they_work)
            .await
            .expect("other tenant")
            .is_empty(),
        "another tenant sees none of these assignments"
    );

    // Removing across a tenant boundary matches nothing; removing within the tenant offboards.
    assert!(
        !store
            .remove(other, assignment)
            .await
            .expect("remove across tenant")
    );
    assert!(store.remove(mine, assignment).await.expect("remove"));
    assert!(
        store
            .list_for_employee(mine, alice)
            .await
            .expect("after remove")
            .is_empty(),
        "the assignment is gone"
    );
}

/// The three people seams as one type — what a router that carries a single `people` state needs (the
/// binary's `PostgresPeople` implements all three). Delegates to the per-seam fakes above.
#[derive(Clone, Default)]
struct FakePeople {
    employees: FakeEmployees,
    roles: FakeRoleTemplates,
    assignments: FakeAssignments,
}

impl EmployeeStore for FakePeople {
    async fn create(&self, employee: &NewEmployee) -> Result<(), EmployeeStoreError> {
        self.employees.create(employee).await
    }
    async fn list(&self, tenant: TenantId) -> Result<Vec<Employee>, EmployeeStoreError> {
        self.employees.list(tenant).await
    }
    async fn get(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Option<Employee>, EmployeeStoreError> {
        self.employees.get(tenant, employee_id).await
    }
    async fn update(&self, employee: &EmployeeUpdate) -> Result<bool, EmployeeStoreError> {
        self.employees.update(employee).await
    }
    async fn set_pin(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
        pin_phc: &str,
    ) -> Result<bool, EmployeeStoreError> {
        self.employees.set_pin(tenant, employee_id, pin_phc).await
    }
    async fn pin_phc(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Option<String>, EmployeeStoreError> {
        self.employees.pin_phc(tenant, employee_id).await
    }
}

impl RoleTemplateStore for FakePeople {
    async fn create(&self, template: &NewRoleTemplate) -> Result<(), RoleTemplateStoreError> {
        self.roles.create(template).await
    }
    async fn list(&self, tenant: TenantId) -> Result<Vec<RoleTemplate>, RoleTemplateStoreError> {
        self.roles.list(tenant).await
    }
    async fn get(
        &self,
        tenant: TenantId,
        role_template_id: RoleTemplateId,
    ) -> Result<Option<RoleTemplate>, RoleTemplateStoreError> {
        self.roles.get(tenant, role_template_id).await
    }
    async fn update(&self, template: &RoleTemplateUpdate) -> Result<bool, RoleTemplateStoreError> {
        self.roles.update(template).await
    }
}

impl AssignmentStore for FakePeople {
    async fn assign(&self, assignment: &NewAssignment) -> Result<(), AssignmentStoreError> {
        self.assignments.assign(assignment).await
    }
    async fn list_for_store(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        self.assignments.list_for_store(tenant, store_id).await
    }
    async fn list_for_employee(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        self.assignments
            .list_for_employee(tenant, employee_id)
            .await
    }
    async fn remove(
        &self,
        tenant: TenantId,
        assignment_id: AssignmentId,
    ) -> Result<bool, AssignmentStoreError> {
        self.assignments.remove(tenant, assignment_id).await
    }
}

/// The main app merged with the people router, wired to a caller-supplied audit recorder so a test can
/// assert the audit trail (ADR-0070).
fn people_app_with_audit(
    admin: FakeAdmin,
    people: FakePeople,
    audit: Arc<dyn AuditRecorder>,
) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::people_router(people, admin, clock(), audit))
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end lifecycle over the people routes — create/read/set-PIN/update an \
              employee, create a role and reject an unknown permission, assign/list/remove, and the \
              audit + no-PII-in-the-trail assertions — kept together so the invariant is checked \
              against the same data the whole flow produced"
)]
async fn people_routes_crud_lifecycle_audited_without_pii() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let audit = FakeAudit::default();
    let router = people_app_with_audit(admin, people, Arc::new(AuditSink::new(audit.clone())));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Create an employee (identity only).
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/employees",
            &serde_json::json!({ "tenant_id": tenant_ulid, "code": "C01", "name": "Alice" }),
            &cookie,
        ))
        .await
        .expect("route create employee");
    assert_eq!(created.status(), StatusCode::CREATED);
    let employee_id = json_body(created).await["id"]
        .as_str()
        .expect("an id")
        .to_owned();

    // The roster read shows the employee, with no PIN yet.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/employees?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route list employees");
    assert_eq!(listed.status(), StatusCode::OK);
    let employees = json_body(listed).await;
    assert_eq!(employees.as_array().expect("array").len(), 1);
    assert_eq!(employees[0]["has_pin"], serde_json::json!(false));

    // Set a PIN: non-digits are rejected, four digits accepted.
    let bad_pin = router
        .clone()
        .oneshot(put_with_cookie(
            &format!("/admin/employees/{employee_id}/pin"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "pin": "abcd" }),
            &cookie,
        ))
        .await
        .expect("route bad pin");
    assert_eq!(bad_pin.status(), StatusCode::BAD_REQUEST);
    let set_pin = router
        .clone()
        .oneshot(put_with_cookie(
            &format!("/admin/employees/{employee_id}/pin"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "pin": "1234" }),
            &cookie,
        ))
        .await
        .expect("route set pin");
    assert_eq!(set_pin.status(), StatusCode::NO_CONTENT);

    // Now has_pin is true on a read-one.
    let one = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/employees/{employee_id}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route get one");
    assert_eq!(one.status(), StatusCode::OK);
    assert_eq!(json_body(one).await["has_pin"], serde_json::json!(true));

    // Rename + archive.
    let updated = router
        .clone()
        .oneshot(patch_with_cookie(
            &format!("/admin/employees/{employee_id}"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Alice Nguyen", "status": "archived" }),
            &cookie,
        ))
        .await
        .expect("route update");
    assert_eq!(updated.status(), StatusCode::NO_CONTENT);

    // The permission catalogue is offered for the role editor.
    let catalogue = router
        .clone()
        .oneshot(get_with_cookie("/admin/people/permissions", &cookie))
        .await
        .expect("route permissions");
    assert_eq!(catalogue.status(), StatusCode::OK);
    assert!(
        !json_body(catalogue)
            .await
            .as_array()
            .expect("array")
            .is_empty()
    );

    // A role naming an unknown permission is refused.
    let bad_role = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/roles",
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Bogus", "permissions": ["not.a.real.permission"] }),
            &cookie,
        ))
        .await
        .expect("route bad role");
    assert_eq!(bad_role.status(), StatusCode::BAD_REQUEST);

    // A valid role.
    let role = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/roles",
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Cashier", "permissions": ["billing.discount.apply"] }),
            &cookie,
        ))
        .await
        .expect("route create role");
    assert_eq!(role.status(), StatusCode::CREATED);
    let role_id = json_body(role).await["id"].as_str().expect("id").to_owned();

    // Assign the employee to a store with that role.
    let assign = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/assignments",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "employee_id": employee_id,
                "store_id": store_ulid,
                "role_template_id": role_id,
            }),
            &cookie,
        ))
        .await
        .expect("route assign");
    assert_eq!(assign.status(), StatusCode::CREATED);
    let assignment_id = json_body(assign).await["id"]
        .as_str()
        .expect("id")
        .to_owned();

    // Readable by store and by employee.
    let by_store = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/assignments?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route by store");
    assert_eq!(by_store.status(), StatusCode::OK);
    assert_eq!(
        json_body(by_store).await.as_array().expect("array").len(),
        1
    );
    let by_employee = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/assignments?tenant_id={tenant_ulid}&employee_id={employee_id}"),
            &cookie,
        ))
        .await
        .expect("route by employee");
    assert_eq!(by_employee.status(), StatusCode::OK);
    assert_eq!(
        json_body(by_employee)
            .await
            .as_array()
            .expect("array")
            .len(),
        1
    );

    // Remove (offboard).
    let remove = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/assignments/{assignment_id}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route remove");
    assert_eq!(remove.status(), StatusCode::NO_CONTENT);

    // Every write was audited — and the trail never carries the employee's name, PIN, or PIN hash.
    let recorded = audit.list(None, 50).await.expect("list audit");
    let actions: Vec<&str> = recorded.iter().map(|entry| entry.action.as_str()).collect();
    for expected in [
        "employee.create",
        "employee.set_pin",
        "employee.update",
        "role.create",
        "assignment.create",
        "assignment.remove",
    ] {
        assert!(actions.contains(&expected), "recorded {expected}");
    }
    let dump = serde_json::to_string(
        &recorded
            .iter()
            .map(|entry| {
                (
                    entry.action.clone(),
                    entry.before.clone(),
                    entry.after.clone(),
                )
            })
            .collect::<Vec<_>>(),
    )
    .expect("serialize the trail");
    assert!(
        !dump.contains("Alice"),
        "the employee's name never enters the audit trail"
    );
    assert!(
        !dump.contains("1234"),
        "the PIN never enters the audit trail"
    );
    assert!(
        !dump.to_lowercase().contains("argon2"),
        "the PIN hash never enters the audit trail"
    );
    // The employee.create entry does record the code (id/code/status), just not the name.
    assert!(dump.contains("C01"), "the staff code is recorded");
}

#[tokio::test]
async fn people_writes_require_manage_people_but_reads_need_only_read() {
    let admin = provisioned_admin();
    let router = people_app_with_audit(
        admin.clone(),
        FakePeople::default(),
        Arc::new(NoopAuditRecorder),
    );
    let tenant_ulid = tenant().as_ulid().to_string();

    // A Viewer may read the roster…
    let viewer = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/employees?tenant_id={tenant_ulid}"),
            &viewer,
        ))
        .await
        .expect("route read");
    assert_eq!(
        read.status(),
        StatusCode::OK,
        "read needs only console.data.read"
    );

    // …but not create an employee (that needs console.people.manage).
    let forbidden = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/employees",
            &serde_json::json!({ "tenant_id": tenant_ulid, "code": "C01", "name": "Alice" }),
            &viewer,
        ))
        .await
        .expect("route create");
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    // Ops cannot manage people either.
    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-token").await;
    let ops_forbidden = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/roles",
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Cashier", "permissions": [] }),
            &ops,
        ))
        .await
        .expect("route create role");
    assert_eq!(ops_forbidden.status(), StatusCode::FORBIDDEN);
}

/// The main app merged with the people-publish router, sharing one config-tree fake so the test can
/// read back the node the publish wrote.
fn people_publish_app(
    admin: FakeAdmin,
    people: FakePeople,
    config: FakeConfigTrees,
    audit: Arc<dyn AuditRecorder>,
) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config.clone(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::people_publish_router(
        people,
        config,
        admin,
        clock(),
        audit,
    ))
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end publish scenario: seed an employee+PIN+role+assignment through the seam, \
              publish, then assert the compiled node landed on the config tree with the flattened \
              permission and PIN hash and that the audit carries no name or PIN — kept together so the \
              invariant is checked against the same state the publish produced"
)]
async fn publishing_permissions_writes_the_config_node_without_pii_in_the_audit() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let mine = tenant();
    let store = store_id();
    let alice = EmployeeId::new(Ulid::from_u128(0xA11));
    let cashier = RoleTemplateId::new(Ulid::from_u128(0xCA5));

    // Seed a store's people directly through the seam: an employee with a PIN, a role, an assignment.
    EmployeeStore::create(
        &people,
        &NewEmployee {
            employee_id: alice,
            tenant_id: mine,
            code: "C01".to_owned(),
            name: "Alice".to_owned(),
        },
    )
    .await
    .expect("create employee");
    EmployeeStore::set_pin(&people, mine, alice, "argon2id$phc$alice")
        .await
        .expect("set pin");
    RoleTemplateStore::create(
        &people,
        &NewRoleTemplate {
            role_template_id: cashier,
            tenant_id: mine,
            name: "Cashier".to_owned(),
            permissions: vec!["billing.discount.apply".to_owned()],
        },
    )
    .await
    .expect("create role");
    AssignmentStore::assign(
        &people,
        &NewAssignment {
            assignment_id: AssignmentId::new(Ulid::from_u128(1)),
            tenant_id: mine,
            employee_id: alice,
            store_id: store,
            role_template_id: cashier,
        },
    )
    .await
    .expect("assign");

    let router = people_publish_app(
        admin,
        people.clone(),
        config.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/people/publish",
            &serde_json::json!({
                "tenant_id": mine.as_ulid().to_string(),
                "store_id": store.as_ulid().to_string(),
            }),
            &cookie,
        ))
        .await
        .expect("route publish");
    assert_eq!(published.status(), StatusCode::OK);
    let version = json_body(published).await["config_version_id"]
        .as_str()
        .expect("a version id")
        .to_owned();
    assert!(!version.is_empty());

    // The compiled node landed on the store layer, flattening the role and carrying the PIN hash.
    let state = config
        .load(mine, store)
        .await
        .expect("load")
        .expect("a published tree");
    let node = &state.layers[2]["permissions"];
    assert_eq!(
        node["store_id"],
        serde_json::json!(store.as_ulid().to_string())
    );
    let staff = node["staff"].as_array().expect("staff array");
    assert_eq!(staff.len(), 1);
    assert_eq!(staff[0]["code"], serde_json::json!("C01"));
    assert_eq!(
        staff[0]["permissions"],
        serde_json::json!(["billing.discount.apply"])
    );
    assert_eq!(
        staff[0]["pin_phc"],
        serde_json::json!("argon2id$phc$alice"),
        "the PIN hash rides to the store for offline verification"
    );

    // The audit records the publish — the config version and staff count, never a name or PIN.
    let recorded = audit.list(None, 10).await.expect("list audit");
    let entry = recorded
        .iter()
        .find(|entry| entry.action == "permissions.publish")
        .expect("the publish was recorded");
    let dump = serde_json::to_string(&entry.after).expect("serialize");
    assert!(dump.contains("staff_count"), "the staff count is recorded");
    assert!(
        !dump.contains("Alice"),
        "no employee name in the audit trail"
    );
    assert!(
        !dump.to_lowercase().contains("argon2"),
        "no PIN hash in the audit trail"
    );
}
