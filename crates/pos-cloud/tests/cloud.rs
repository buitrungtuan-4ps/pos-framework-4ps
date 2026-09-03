// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `pos_cloud`'s ingest and rollup spine and its public `/v1` surface, against the in-memory fakes.
//!
//! The same handler code runs here against `pos-fakes` and, in the binary, against `store-postgres`
//! (ADR-0026) — so idempotent ingest, the materialised rollup read, and the `/v1` bearer check are
//! proven without a database, while the store-specific behaviour (RLS, partitioning, the rollup and
//! API-key tables) is proven by `store-postgres`'s own integration suite.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use argon2::password_hash::SaltString;

use pos_cloud::activation::{
    ActivationCodeStore, ActivationStoreError, DeviceCredential, IssuedCode, hash_code,
};
use pos_cloud::alerts::{AlertKind, AlertRecord, AlertStore, AlertStoreError};
use pos_cloud::audit::{
    AuditActor, AuditEntry, AuditId, AuditQuery, AuditRecorder, AuditSink, AuditStore,
    AuditStoreError, NoopAuditRecorder, TrailOrder,
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
    ItemCategory, ItemListFilter, ItemSort, ItemSubcategory, LayoutButton, Menu, MenuId,
    MenuPlacement, MenuSection, ModifierGroup, TaxClass,
};
use pos_cloud::config_tree::{ConfigStoreError, ConfigTreeState, ConfigTreeStore};
use pos_cloud::dashboard::{RollupError, RollupStore, StoredRollups, project};
use pos_cloud::devices::{
    DeviceKind, DeviceProposalError, DeviceProposalId, DeviceProposalStatus, DeviceProposalStore,
    DeviceProposalSummary, PersistedDeviceProposal,
};
use pos_cloud::fleet::{FleetRow, FleetStore, FleetStoreError, OtaReportStore};
use pos_cloud::floorplan::{
    Area, AreaStore, AreaUpdate, FloorStoreError, NewArea, NewRoutingRule, NewStation, NewTable,
    RoutingRule, RoutingRuleId, RoutingRuleStore, Station, StationStore, StationUpdate, Table,
    TableStore, TableUpdate,
};
use pos_cloud::health::{self, TaskHealth, TaskHealthError, TaskHealthStore};
use pos_cloud::http::CloudApp;
use pos_cloud::inventory::{InventoryStore, InventoryStoreError};
use pos_cloud::media::{
    MediaId, MediaStore, MediaStoreError, MediaSummary, NewMediaAsset, Rendition,
};
use pos_cloud::orders::{StoreDirectory, orders_router};
use pos_cloud::paging::{Page, PageRequest};
use pos_cloud::people::{
    Assignment, AssignmentId, AssignmentStore, AssignmentStoreError, Employee, EmployeeId,
    EmployeeStore, EmployeeStoreError, EmployeeUpdate, NewAssignment, NewEmployee, NewRoleTemplate,
    RoleTemplate, RoleTemplateId, RoleTemplateStore, RoleTemplateStoreError, RoleTemplateUpdate,
    is_known_permission, permission_catalogue,
};
use pos_cloud::qr::{TableTokenSecret, mint_table_token};
use pos_cloud::qr_http::qr_router;
use pos_cloud::reconcile::{ReconcileError, ReconcileRun, ReconcileRunStore, ReconcileStore};
use pos_cloud::registry::{
    BrandRecord, DeviceRecord, EntityStatus, RegistryStore, RegistryStoreError, StoreRecord,
    TenantRecord,
};
use pos_cloud::relay::{
    OrderQueueId, OrderQueueStore, OrderRecord, OrderRelay, OrderStatus, PendingOrder,
    QueuedOrderPayload, StoreOutcome, orders_sync_router_with_cap,
};
use pos_cloud::retention::{RetentionError, SubjectRecord, SubjectStore};
use pos_cloud::tax::{TaxRateEntry, TaxRateStore, TaxRateStoreError};
use pos_cloud::translations::{TranslationGrid, TranslationStore, TranslationStoreError};
use pos_cloud::version::{CreateOutcome, UpdateOutcome, Version, Versioned};
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
use pos_proto::display::GridPosition;
use pos_proto::enums::SalesChannel;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{
    AreaId, ConfigVersionId, CourseId, DeviceId, EventId, IngredientId, MenuItemId, StationId,
    StoreId, SubjectId, SupplierId, TableId, TaxClassId, TenantId,
};
use pos_proto::inventory::{PublishedIngredient, PublishedRecipe, PublishedSupplier};
use pos_proto::locale::TaxRate;
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

/// Every store's tree state, each carrying the version this fake last wrote it at — the `state` and
/// `xmin` columns of one `config_trees` row.
type ConfigRows = HashMap<(TenantId, StoreId), Versioned<ConfigTreeState>>;

/// The config-tree store, keyed by `(tenant, store)` exactly as the real table. `seen` mirrors the
/// `store_liveness` upsert so a test can assert the config pull recorded the store's contact.
#[derive(Clone, Default)]
struct FakeConfigTrees {
    rows: Arc<Mutex<ConfigRows>>,
    seen: Arc<Mutex<HashMap<(TenantId, StoreId), RecordedSeen>>>,
    next_version: Arc<Mutex<u64>>,
    /// A competing publish to land *between* the next read and its write, so a test can produce the
    /// race the retry exists for. `(key, value)` is set on the Store layer, as another node publish
    /// would set it.
    interpose: Arc<Mutex<Option<(String, serde_json::Value)>>>,
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

impl FakeConfigTrees {
    /// The next row version, as the adapter's `xmin::text` is: a token, not a number a caller may
    /// reason about (ADR-0095).
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }
}

impl ConfigTreeStore for FakeConfigTrees {
    async fn load(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> Result<Option<Versioned<ConfigTreeState>>, ConfigStoreError> {
        let handed_out = self
            .rows
            .lock()
            .expect("lock")
            .get(&(tenant, store))
            .cloned();

        // Land the competing publish now, after this caller has its (already stale) read. Its save
        // will find the version moved — which is precisely the interleave a node publish must
        // survive without troubling anyone.
        if let Some((key, value)) = self.interpose.lock().expect("lock").take() {
            let version = self.mint();
            let mut rows = self.rows.lock().expect("lock");
            let mut state = rows.get(&(tenant, store)).map_or_else(
                || ConfigTreeState {
                    layers: [
                        serde_json::json!({}),
                        serde_json::json!({}),
                        serde_json::json!({}),
                        serde_json::json!({}),
                    ],
                    history: Vec::new(),
                    k: 8,
                },
                |row| row.record.clone(),
            );
            if let serde_json::Value::Object(map) = &mut state.layers[2] {
                map.insert(key, value);
            }
            rows.insert((tenant, store), Versioned::new(state, version));
        }
        Ok(handed_out)
    }

    async fn save(
        &self,
        tenant: TenantId,
        store: StoreId,
        state: &ConfigTreeState,
        expected: Option<&Version>,
    ) -> Result<UpdateOutcome, ConfigStoreError> {
        let version = self.mint();
        let mut rows = self.rows.lock().expect("lock");
        // The same four answers `store-postgres` gives, so a test that passes here is not passing on
        // a laxer store: a first publish must actually be first, a version-gated one needs a row to
        // gate on, and the version it names must still be the stored one.
        let refusal = match (rows.get(&(tenant, store)), expected) {
            (None, None) => None,
            (None, Some(_)) => Some(UpdateOutcome::NotFound),
            (Some(_), None) => Some(UpdateOutcome::VersionMismatch),
            (Some(existing), Some(expected)) => {
                (&existing.etag != expected).then_some(UpdateOutcome::VersionMismatch)
            }
        };
        if let Some(refusal) = refusal {
            return Ok(refusal);
        }
        rows.insert(
            (tenant, store),
            Versioned::new(state.clone(), version.clone()),
        );
        Ok(UpdateOutcome::Updated(version))
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
    // Every test app carries the `/internal` secret, because `CloudConfig::validate` means a booted
    // process always does (ADR-0097). The refusal path is exercised deliberately, not by default.
    CloudApp::new(cloud, rollups, keys, clock(), admin, config_trees, webhooks)
        .with_internal_shared_secret(Some(internal_secret()))
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

/// A POST carrying the version the caller read the tree at — the rollback's conditional write.
fn post_config_with_etag(
    uri: &str,
    body: &serde_json::Value,
    cookie: &str,
    etag: &str,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .header("if-match", format!("\"{etag}\""))
        .body(Body::from(
            serde_json::to_vec(body).expect("serialise the body"),
        ))
        .expect("build the request")
}

/// A JSON PUT carrying the version the caller read at — an authored config layer, a tax-rate grid,
/// a translation grid.
///
/// `If-Match: *` asserts "nothing is published here yet", which is how a store's first authored
/// publish says so (ADR-0095). Node publishes need none of this: they set one key, they commute with
/// the other keys, and they retry rather than refuse.
fn put_with_etag(uri: &str, body: &serde_json::Value, cookie: &str, etag: &str) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .header(
            "if-match",
            if etag == "*" {
                "*".to_owned()
            } else {
                format!("\"{etag}\"")
            },
        )
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

/// A PATCH request carrying both a `Cookie` and the `If-Match` a mutating `/admin` route requires
/// (ADR-0094). `etag` is the token as read back, without the quotes the header wants.
fn patch_with_etag(uri: &str, body: &serde_json::Value, cookie: &str, etag: &str) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .header("if-match", format!("\"{etag}\""))
        .body(Body::from(
            serde_json::to_vec(body).expect("serialise the body"),
        ))
        .expect("build the request")
}

/// The version a response carried in its `ETag` header, unquoted — the token a client hands back on
/// its next conditional write (ADR-0094). Panics if the response carried none, because every route
/// that answers a read-one or a write of a versioned record is required to stamp one.
fn etag_of(response: &axum::response::Response) -> String {
    response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(|raw| raw.trim_matches('"').to_owned())
        .expect("the response carried an ETag")
}

/// As [`patch_with_etag`], but sending `if-match` exactly as given — for the cases where the point
/// is that the header is *not* a well-formed strong entity-tag.
fn patch_with_raw_if_match(
    uri: &str,
    body: &serde_json::Value,
    cookie: &str,
    raw: &str,
) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .header("if-match", raw)
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

/// The response body as raw bytes — for comparing two responses that must be indistinguishable.
async fn body_bytes(response: axum::response::Response) -> axum::body::Bytes {
    response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes()
}

/// The response body as a UTF-8 string — for the CSV export routes (ADR-0075).
async fn text_body(response: axum::response::Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    String::from_utf8(bytes.to_vec()).expect("body is valid UTF-8")
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
            .header("X-Pos-Internal-Key", internal_secret().expose())
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
async fn setup_with_a_short_password_names_the_field_it_is_about() {
    // A single field out of range is a `400`, not the `422` this answered before ADR-0096: there is
    // a field to go and fix, and the refusal has to say which one.
    let (router, admin) = setup_router(Some(SETUP_TOKEN));
    let body = serde_json::json!({ "setup_token": SETUP_TOKEN, "password": "short" });
    let response = router
        .oneshot(post_json("/admin/setup", &body))
        .await
        .expect("route the setup");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "too short a password is refused before anything is written"
    );
    let body = json_body(response).await;
    assert_eq!(body["error"]["status"], "INVALID_ARGUMENT", "got {body}");
    assert_eq!(body["error"]["details"][0]["field"], "password");
    assert_eq!(body["error"]["details"][0]["reason"], "OUT_OF_RANGE");
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
    let body = json_body(response).await;
    assert_eq!(body["error"]["status"], "INVALID_ARGUMENT", "got {body}");
    assert_eq!(body["error"]["details"][0]["field"], "scopes");
    assert_eq!(body["error"]["details"][0]["reason"], "INVALID_ENUM_VALUE");
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
        .oneshot(put_with_etag(
            &format!("{base}/tenant?tenant_id={tenant_ulid}"),
            &tenant_doc,
            &cookie,
            "*",
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let published = json_body(published).await;
    let first_version = published["config_version_id"]
        .as_str()
        .expect("a successful publish returns the new version id")
        .to_owned();

    let store_doc = serde_json::json!({ "tips_enabled": true });
    let published2 = router
        .clone()
        .oneshot(put_with_etag(
            &format!("{base}/store?tenant_id={tenant_ulid}"),
            &store_doc,
            &cookie,
            &first_version,
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

/// A node publish survives another publish landing between its read and its write, and neither
/// operator loses anything.
///
/// This is the case the retry exists for, and the one the old code got wrong: both publishes composed
/// a whole tree from the same stale read, so whichever saved second erased the other's node entirely.
/// The layers are a map, so writes to *different* keys have no reason to conflict — and after this
/// both keys are present.
#[tokio::test]
async fn a_node_publish_survives_a_concurrent_publish_of_a_different_node() {
    let config_trees = FakeConfigTrees::default();
    let router = config_capabilities_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Arm a competing `locale` publish to land after the next read but before its write.
    *config_trees.interpose.lock().expect("lock") =
        Some(("locale".to_owned(), serde_json::json!({ "country": "VN" })));

    let published = router
        .oneshot(put_with_cookie(
            "/admin/config/capabilities",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "flags": { "tables_enabled": false },
            }),
            &cookie,
        ))
        .await
        .expect("route the capabilities publish");
    assert_eq!(
        published.status(),
        StatusCode::OK,
        "a node publish retries past a concurrent one rather than refusing"
    );

    let stored = config_trees
        .rows
        .lock()
        .expect("lock")
        .get(&(tenant(), store_id()))
        .cloned()
        .expect("the tree is there");
    let layer = &stored.record.layers[2];
    assert!(
        layer.get("tables_enabled").is_some(),
        "this publish's own node landed"
    );
    assert!(
        layer.get("locale").is_some(),
        "and the concurrent publish's node was not erased — the bug this slice removes"
    );
}

/// The authored layer publish is the other half: there a second writer really does destroy work an
/// operator typed, so it is refused rather than retried.
#[tokio::test]
async fn an_authored_layer_publish_made_against_a_stale_read_is_refused() {
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

    let first = router
        .clone()
        .oneshot(put_with_etag(
            &format!("{base}/store?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "tips_enabled": false }),
            &cookie,
            "*",
        ))
        .await
        .expect("route the first publish");
    assert_eq!(first.status(), StatusCode::OK);
    let first_version = json_body(first).await["config_version_id"]
        .as_str()
        .expect("a version id")
        .to_owned();

    // A second writer still asserting the store had nothing published is refused.
    let stale_wildcard = router
        .clone()
        .oneshot(put_with_etag(
            &format!("{base}/store?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "tips_enabled": true }),
            &cookie,
            "*",
        ))
        .await
        .expect("route the stale publish");
    assert_eq!(stale_wildcard.status(), StatusCode::PRECONDITION_FAILED);

    // And naming the version it read lets it through, moving the tree on.
    let fresh = router
        .clone()
        .oneshot(put_with_etag(
            &format!("{base}/store?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "tips_enabled": true }),
            &cookie,
            &first_version,
        ))
        .await
        .expect("route the current publish");
    assert_eq!(fresh.status(), StatusCode::OK);

    // Replaying the version that just stopped being current is the lost update, refused.
    let replayed = router
        .oneshot(put_with_etag(
            &format!("{base}/store?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "tips_enabled": false }),
            &cookie,
            &first_version,
        ))
        .await
        .expect("route the replay");
    assert_eq!(replayed.status(), StatusCode::PRECONDITION_FAILED);
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
    let publish = |doc: serde_json::Value, level: &'static str, etag: String| {
        let router = router.clone();
        let cookie = cookie.clone();
        let uri = format!("{base}/{level}?tenant_id={tenant_ulid}");
        async move {
            let response = router
                .oneshot(put_with_etag(&uri, &doc, &cookie, &etag))
                .await
                .expect("route the publish");
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await["config_version_id"]
                .as_str()
                .expect("a version id")
                .to_owned()
        }
    };
    let v1 = publish(
        serde_json::json!({ "currency_code": "VND" }),
        "tenant",
        "*".to_owned(),
    )
    .await;
    let v2 = publish(
        serde_json::json!({ "currency_code": "SGD" }),
        "store",
        v1.clone(),
    )
    .await;

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
        .oneshot(post_config_with_etag(
            &format!("{base}/rollback?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "version_id": v1 }),
            &cookie,
            &v2,
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
        .oneshot(put_with_etag(
            &format!("/admin/stores/{store_ulid}/config/store?tenant_id={tenant_ulid}"),
            &bad,
            &cookie,
            "*",
        ))
        .await
        .expect("route the publish");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(response).await;
    assert_eq!(body["error"]["status"], "UNPROCESSABLE", "got {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "the rejection names the violated rule(s): {body}"
    );
    assert!(
        body["error"]["details"].is_null(),
        "prose violations carry no field, so no `details` is invented for them: {body}"
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
        .oneshot(put_with_etag(
            &format!("/admin/stores/{store_ulid}/config/store?tenant_id={tenant_ulid}"),
            &doc,
            &cookie,
            "*",
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

/// A refusal on the **store-facing** surface is enveloped too, and names the field that was wrong
/// rather than the other one on the same route.
///
/// `GET /sync/stores/{id}/config` can refuse for two different fields — the path's store id or the
/// query's `held_version` — and a store that sent a good id and a bad version needs to be told
/// which. Before this slice both answered the same shape of bare string.
#[tokio::test]
async fn a_store_facing_refusal_names_the_field_that_was_actually_wrong() {
    let keys = FakeKeys::default();
    let (router, token, store_ulid, _version) = published_config(&keys).await;

    let response = router
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config?held_version=not-a-ulid"),
            Some(&token),
        ))
        .await
        .expect("route the sync");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["status"], "INVALID_ARGUMENT", "got {body}");
    assert_eq!(body["error"]["details"][0]["field"], "held_version");
    assert_eq!(body["error"]["details"][0]["reason"], "NOT_A_ULID");
    assert!(
        body["error"]["details"][1].is_null(),
        "only the field that was wrong is named — the store id in the path was fine: {body}"
    );
}

/// A refusal about two fields names **only the ones actually missing.**
///
/// `propose_device` refuses when either `name` or `address` is blank, and its message says "name
/// and address are required" for both cases. A store that sent a name and forgot the address was
/// being told to check both. The condition is unchanged; the answer got specific.
#[tokio::test]
async fn a_refusal_about_two_fields_names_only_the_ones_missing() {
    let keys = FakeKeys::default();
    let router = device_app(provisioned_admin(), keys.clone(), FakeDevices::default());
    let token = issue_key(&keys, tenant(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let devices_uri = format!("/sync/stores/{store_ulid}/devices");

    // A name, and a blank address.
    let response = router
        .clone()
        .oneshot(post_json_bearer(
            &devices_uri,
            &serde_json::json!({ "kind": "printer", "name": "Kitchen 1", "address": "   " }),
            &token,
        ))
        .await
        .expect("route the proposal");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(
        body["error"]["details"][0]["field"], "address",
        "got {body}"
    );
    assert_eq!(body["error"]["details"][0]["reason"], "REQUIRED");
    assert!(
        body["error"]["details"][1].is_null(),
        "the name was fine, so it is not named: {body}"
    );

    // Both blank: both named, in the order the fields are declared.
    let response = router
        .oneshot(post_json_bearer(
            &devices_uri,
            &serde_json::json!({ "kind": "printer", "name": "", "address": "" }),
            &token,
        ))
        .await
        .expect("route the proposal");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["details"][0]["field"], "name", "got {body}");
    assert_eq!(body["error"]["details"][1]["field"], "address");
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
        .oneshot(put_with_etag(
            &format!("/admin/stores/{store_ulid}/config/store?tenant_id={tenant_ulid}"),
            &doc,
            &cookie,
            "*",
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

// --- The `/internal` shared secret (ADR-0097) ---------------------------------------------------

/// A `POST` to an `/internal` route with a wrong key, or none at all.
fn post_internal_with(uri: &str, body: &serde_json::Value, key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(key) = key {
        builder = builder.header("X-Pos-Internal-Key", key);
    }
    builder
        .body(Body::from(body.to_string()))
        .expect("build the request")
}

#[tokio::test]
async fn the_internal_routes_refuse_a_request_without_the_shared_secret() {
    // All three, because the guard is threaded per handler rather than applied as one layer, so
    // "the other two are covered" is not something the type system says here.
    let reconcile_body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
        "event_ids": [event_ulid(1)],
    });
    let cases: Vec<(&str, serde_json::Value, axum::Router)> = vec![
        (
            "/internal/reconcile",
            reconcile_body,
            http::reconcile_router(
                FakeReconcile::with_present(HashSet::new()),
                provisioned_admin(),
                clock(),
                Some(internal_secret()),
            ),
        ),
        (
            "/internal/ota/report",
            report_body(None),
            ota_report_app(FakeOtaReports::default()),
        ),
    ];

    for (uri, body, router) in cases {
        for key in [None, Some("not-the-secret"), Some("")] {
            let response = router
                .clone()
                .oneshot(post_internal_with(uri, &body, key))
                .await
                .expect("route the request");
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{uri} with key {key:?} must be refused"
            );
            // A `404`, and the same one every time: `403` would confirm the route is there, which is
            // what the proxy denies and ADR-0050's activation refusal both decline to confirm.
            let envelope = json_body(response).await;
            assert_eq!(envelope["error"]["status"], "NOT_FOUND", "got {envelope}");
            assert_eq!(envelope["error"]["message"], "no such route");
        }
    }
}

#[tokio::test]
async fn the_admin_reconcile_read_does_not_want_the_internal_key() {
    // `/admin/reconcile` shares a router with `/internal/reconcile`, which is exactly why the guard
    // is on the handler and not the router: a console read behind a permission must not start
    // demanding a secret only the cloud operator holds.
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let router = http::reconcile_router(
        FakeReconcile::with_present(HashSet::new()),
        admin,
        clock(),
        Some(internal_secret()),
    );
    let tenant_ulid = tenant().as_ulid().to_string();
    let response = router
        .oneshot(get_with_cookie(
            &format!("/admin/reconcile?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the console read is reached without any `X-Pos-Internal-Key`"
    );
}

#[tokio::test]
async fn an_internal_route_with_no_secret_configured_refuses_rather_than_admits() {
    // `CloudConfig::validate` means a booted process always has one, so this is the fork-that-wires-
    // the-router-by-hand case. It must land closed.
    let router = http::ota_report_router(FakeOtaReports::default(), clock(), None);
    let response = router
        .oneshot(post_internal("/internal/ota/report", &report_body(None)))
        .await
        .expect("route the report");
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "an unconfigured secret closes the route, it does not open it"
    );
}

// --- Reconciliation diff (`POST /internal/reconcile`) -------------------------------------------

/// A reconciliation store that "has" a fixed set of ids; the missing ones are the complement. It also
/// records the runs it is asked to persist, so a test can assert the history was written.
#[derive(Clone, Default)]
struct FakeReconcile {
    present: HashSet<EventId>,
    runs: Arc<Mutex<Vec<(TenantId, ReconcileRun)>>>,
}

impl FakeReconcile {
    fn with_present(present: HashSet<EventId>) -> Self {
        Self {
            present,
            runs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recorded(&self) -> Vec<(TenantId, ReconcileRun)> {
        self.runs.lock().expect("lock").clone()
    }
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

impl ReconcileRunStore for FakeReconcile {
    async fn record_run(&self, tenant: TenantId, run: &ReconcileRun) -> Result<(), ReconcileError> {
        self.runs.lock().expect("lock").push((tenant, run.clone()));
        Ok(())
    }

    async fn list_runs(
        &self,
        tenant: TenantId,
        store: Option<StoreId>,
        limit: u32,
    ) -> Result<Vec<ReconcileRun>, ReconcileError> {
        let mut runs: Vec<ReconcileRun> = self
            .runs
            .lock()
            .expect("lock")
            .iter()
            .filter(|(row_tenant, run)| {
                *row_tenant == tenant && store.is_none_or(|wanted| run.store == wanted)
            })
            .map(|(_, run)| run.clone())
            .collect();
        // Newest first, exactly as the SQL adapter orders by `ran_at DESC`.
        runs.sort_by(|a, b| b.ran_at.cmp(&a.ran_at));
        runs.truncate(limit as usize);
        Ok(runs)
    }
}

/// The main router (for `/admin/login`) merged with the reconcile sub-router, one shared admin store.
fn reconcile_app(admin: FakeAdmin, store: FakeReconcile) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::reconcile_router(
        store,
        admin,
        clock(),
        Some(internal_secret()),
    ))
}

/// The `/internal` shared secret the test routers are built with
/// ([ADR-0097](../../../docs/adr/0097-internal-route-authentication.md)).
fn internal_secret() -> pos_cloud::config::InternalSecret {
    pos_cloud::config::InternalSecret::new("test-internal-shared-secret-0123456789abcdef")
}

/// A `POST` to an `/internal` route, carrying the shared secret the test routers expect.
fn post_internal(uri: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Pos-Internal-Key", internal_secret().expose())
        .body(Body::from(body.to_string()))
        .expect("build the request")
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
    let store = FakeReconcile::with_present(present);
    let router = http::reconcile_router(
        store.clone(),
        provisioned_admin(),
        clock(),
        Some(internal_secret()),
    );
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
        "event_ids": [event_ulid(1), event_ulid(2), event_ulid(3), event_ulid(4)],
    });
    let response = router
        .oneshot(post_internal("/internal/reconcile", &body))
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
    // The diff also recorded a run: four ids offered, two missing.
    let recorded = store.recorded();
    assert_eq!(recorded.len(), 1, "one run was recorded for the diff");
    let (recorded_tenant, run) = &recorded[0];
    assert_eq!(*recorded_tenant, tenant());
    assert_eq!(run.store, store_id());
    assert_eq!(run.candidates_offered, 4);
    assert_eq!(run.missing_found, 2);
}

#[tokio::test]
async fn reconcile_rejects_a_malformed_id() {
    let store = FakeReconcile::default();
    let router = http::reconcile_router(
        store.clone(),
        provisioned_admin(),
        clock(),
        Some(internal_secret()),
    );
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
        "event_ids": ["not-a-ulid"],
    });
    let response = router
        .oneshot(post_internal("/internal/reconcile", &body))
        .await
        .expect("route the reconcile");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a manifest carrying a non-ULID id is rejected, not silently dropped"
    );
    assert!(
        store.recorded().is_empty(),
        "a rejected manifest records no run"
    );
}

#[tokio::test]
async fn reconcile_history_lists_the_runs_a_diff_recorded() {
    let store = FakeReconcile::with_present(HashSet::new());
    let router = reconcile_app(provisioned_admin(), store);
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Run a diff so there is a run to read back: two ids offered, both missing (empty cloud).
    let body = serde_json::json!({
        "tenant_id": tenant_ulid,
        "store_id": store_ulid,
        "event_ids": [event_ulid(7), event_ulid(9)],
    });
    let diff = router
        .clone()
        .oneshot(post_internal("/internal/reconcile", &body))
        .await
        .expect("route the reconcile");
    assert_eq!(diff.status(), StatusCode::OK);

    // The console read lists it.
    let listed = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/admin/reconcile?tenant_id={tenant_ulid}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("route the history read");
    assert_eq!(listed.status(), StatusCode::OK);
    let runs = json_body(listed).await;
    let runs = runs.as_array().expect("a runs array");
    assert_eq!(runs.len(), 1, "the one recorded run is listed");
    assert_eq!(runs[0]["store_id"], store_ulid);
    assert_eq!(runs[0]["candidates_offered"], 2);
    assert_eq!(runs[0]["missing_found"], 2);
}

#[tokio::test]
async fn reconcile_history_needs_a_session() {
    let router = reconcile_app(provisioned_admin(), FakeReconcile::default());
    let tenant_ulid = tenant().as_ulid().to_string();
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/admin/reconcile?tenant_id={tenant_ulid}"))
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("route the history read");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the reconciliation history is behind a session"
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

/// The translation store, one grid per tenant, each carrying the version it was last written at —
/// the `grid` and `xmin` of one `translations` row ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
#[derive(Clone, Default)]
struct FakeTranslations {
    rows: Arc<Mutex<HashMap<TenantId, Versioned<TranslationGrid>>>>,
    next_version: Arc<Mutex<u64>>,
    /// A competing save to land *between* the next read and its write, so a test can produce the
    /// race the CSV import's retry exists for. `(key, locale, value)` is merged into the grid.
    interpose: Arc<Mutex<Option<(String, String, String)>>>,
}

impl FakeTranslations {
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(format!("tg{next}"))
    }
}

impl TranslationStore for FakeTranslations {
    async fn load(
        &self,
        tenant: TenantId,
    ) -> Result<Option<Versioned<TranslationGrid>>, TranslationStoreError> {
        let handed_out = self.rows.lock().expect("lock").get(&tenant).cloned();
        // The competing save lands after this read has been taken but before the caller can write,
        // which is exactly the interleave a retry has to survive.
        if let Some((key, locale, value)) = self.interpose.lock().expect("lock").take() {
            let version = self.mint();
            let mut rows = self.rows.lock().expect("lock");
            let mut entries = rows
                .get(&tenant)
                .map(|loaded| loaded.record.as_map().clone())
                .unwrap_or_default();
            entries.entry(key).or_default().insert(locale, value);
            rows.insert(
                tenant,
                Versioned::new(TranslationGrid::new(entries), version),
            );
        }
        Ok(handed_out)
    }

    async fn save(
        &self,
        tenant: TenantId,
        grid: &TranslationGrid,
        expected: Option<&Version>,
    ) -> Result<UpdateOutcome, TranslationStoreError> {
        let version = self.mint();
        let mut rows = self.rows.lock().expect("lock");
        // The same four answers `store-postgres` gives, so a test passing here is not passing on a
        // laxer store.
        let refusal = match (rows.get(&tenant), expected) {
            (None, None) => None,
            (None, Some(_)) => Some(UpdateOutcome::NotFound),
            (Some(_), None) => Some(UpdateOutcome::VersionMismatch),
            (Some(stored), Some(expected)) => {
                (&stored.etag != expected).then_some(UpdateOutcome::VersionMismatch)
            }
        };
        if let Some(refusal) = refusal {
            return Ok(refusal);
        }
        rows.insert(tenant, Versioned::new(grid.clone(), version.clone()));
        Ok(UpdateOutcome::Updated(version))
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

    // A grid with en on every key publishes and round-trips through GET. `If-Match: *` is how a
    // tenant that has authored nothing yet says so (ADR-0095).
    let good = serde_json::json!({
        "menu.pho": { "en": "Pho", "vi": "Phở" },
        "menu.tea": { "en": "Tea" },
    });
    let put = router
        .clone()
        .oneshot(put_with_etag(&uri, &good, &cookie, "*"))
        .await
        .expect("route the publish");
    assert_eq!(put.status(), StatusCode::NO_CONTENT);
    let got = router
        .clone()
        .oneshot(get_with_cookie(&uri, &cookie))
        .await
        .expect("route the read");
    assert_eq!(got.status(), StatusCode::OK);
    let current = etag_of(&got);
    assert_eq!(json_body(got).await, good, "the grid round-trips");

    // A grid missing en on a key is a 422 naming it, and does not overwrite the good grid.
    let bad = serde_json::json!({ "menu.rice": { "vi": "Cơm" } });
    let rejected = router
        .clone()
        .oneshot(put_with_etag(&uri, &bad, &cookie, &current))
        .await
        .expect("route the bad publish");
    assert_eq!(rejected.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let refusal = json_body(rejected).await;
    assert_eq!(refusal["error"]["status"], "UNPROCESSABLE", "got {refusal}");
    // The whole point of ADR-0096 for this route: the offending key reaches the console as a field
    // it can mark, not as a raw `{"missing_fallback":[…]}` body nothing there reads.
    assert_eq!(
        refusal["error"]["details"][0]["field"], "menu.rice.en",
        "the rejection names the key lacking an en fallback: {refusal}"
    );
    assert_eq!(refusal["error"]["details"][0]["reason"], "REQUIRED");
    assert!(
        refusal["error"]["details"][1].is_null(),
        "one detail per offending key, and only one key is bad: {refusal}"
    );
    assert!(
        refusal["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("menu.rice")),
        "and the sentence a person reads names it too: {refusal}"
    );
    let unchanged = router
        .clone()
        .oneshot(get_with_cookie(&uri, &cookie))
        .await
        .expect("route the re-read");
    assert_eq!(
        json_body(unchanged).await,
        good,
        "a rejected publish left the last good grid current"
    );

    // Replaying `*` after the grid exists is the claim "nothing is saved here", and it is false.
    let stale_wildcard = router
        .clone()
        .oneshot(put_with_etag(&uri, &good, &cookie, "*"))
        .await
        .expect("route the stale publish");
    assert_eq!(
        stale_wildcard.status(),
        StatusCode::PRECONDITION_FAILED,
        "a wildcard is an assertion about an empty grid, not a waiver"
    );

    // A save at the current version applies and moves it; replaying that version is then the lost
    // update this exists to refuse.
    let second = serde_json::json!({ "menu.pho": { "en": "Pho noodles" } });
    let applied = router
        .clone()
        .oneshot(put_with_etag(&uri, &second, &cookie, &current))
        .await
        .expect("route the second publish");
    assert_eq!(applied.status(), StatusCode::NO_CONTENT);
    let replayed = router
        .clone()
        .oneshot(put_with_etag(&uri, &good, &cookie, &current))
        .await
        .expect("route the replay");
    assert_eq!(replayed.status(), StatusCode::PRECONDITION_FAILED);
    let survived = router
        .oneshot(get_with_cookie(&uri, &cookie))
        .await
        .expect("route the final read");
    assert_eq!(
        json_body(survived).await,
        second,
        "the refused replay did not overwrite the edit that won"
    );
}

/// A CSV import merges, so it retries around a competing save instead of refusing it (ADR-0095).
///
/// The two writers touch different keys. The import composes its rows onto whatever grid it read, so
/// when it loses the race the fix is to re-read and re-apply — not to hand an operator a conflict
/// about a key their CSV never mentioned. This is the same reasoning as the ten config node
/// publishes, and the opposite of `PUT /admin/translations`, which replaces a grid a human typed.
#[tokio::test]
async fn a_csv_import_survives_a_concurrent_save_of_a_different_key() {
    let translations = FakeTranslations::default();
    let router = translation_app(provisioned_admin(), translations.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let grid_uri = format!("/admin/translations?tenant_id={tenant_ulid}");

    let base = serde_json::json!({ "menu.pho": { "en": "Pho" } });
    let seeded = router
        .clone()
        .oneshot(put_with_etag(&grid_uri, &base, &cookie, "*"))
        .await
        .expect("route the seed");
    assert_eq!(seeded.status(), StatusCode::NO_CONTENT);

    // Somebody else saves `menu.rice` between this import's read and its write.
    *translations.interpose.lock().expect("lock") =
        Some(("menu.rice".to_owned(), "en".to_owned(), "Rice".to_owned()));

    let apply = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/translations/import/apply?tenant_id={tenant_ulid}"),
            b"key,en\nmenu.tea,Tea\n".to_vec(),
            "text/csv",
            &cookie,
        ))
        .await
        .expect("route the apply");
    assert_eq!(
        apply.status(),
        StatusCode::OK,
        "an import that lost a race retries rather than refusing"
    );

    let merged = json_body(
        router
            .oneshot(get_with_cookie(&grid_uri, &cookie))
            .await
            .expect("route the read"),
    )
    .await;
    assert_eq!(
        merged["menu.tea"]["en"], "Tea",
        "the import's own key landed"
    );
    assert_eq!(
        merged["menu.rice"]["en"], "Rice",
        "and it did not clobber the save that beat it"
    );
    assert_eq!(merged["menu.pho"]["en"], "Pho", "nor the key already there");
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

/// The CSV export streams the grid as `text/csv` with a union-of-locales header and a row per key
/// (ADR-0075, Track M5); unauthenticated it is a 401.
#[tokio::test]
async fn translation_export_streams_csv_and_needs_a_session() {
    let router = translation_app(provisioned_admin(), FakeTranslations::default());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    let grid = serde_json::json!({
        "menu.pho": { "en": "Pho", "vi": "Phở" },
        "menu.tea": { "en": "Tea" },
    });
    let put = router
        .clone()
        .oneshot(put_with_etag(
            &format!("/admin/translations?tenant_id={tenant_ulid}"),
            &grid,
            &cookie,
            "*",
        ))
        .await
        .expect("route the publish");
    assert_eq!(put.status(), StatusCode::NO_CONTENT);

    let export_uri = format!("/admin/translations/export?tenant_id={tenant_ulid}");
    let unauth = router
        .clone()
        .oneshot(get(&export_uri, None))
        .await
        .expect("route");
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let exported = router
        .oneshot(get_with_cookie(&export_uri, &cookie))
        .await
        .expect("route the export");
    assert_eq!(exported.status(), StatusCode::OK);
    let content_type = exported
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("text/csv"), "served as CSV");
    let csv = text_body(exported).await;
    let mut lines = csv.lines();
    assert_eq!(lines.next().unwrap(), "key,en,vi");
    assert_eq!(lines.next().unwrap(), "menu.pho,Pho,Phở");
    // The tea key has no vi value, so its vi cell is an empty trailing field.
    assert_eq!(lines.next().unwrap(), "menu.tea,Tea,");
}

/// A CSV import is dry-run-first (ADR-0075, Track M5): the dry-run classifies every row and writes
/// nothing; the apply merges the valid rows onto the grid and skips the rejected ones.
#[tokio::test]
async fn translation_import_dry_runs_then_applies() {
    let router = translation_app(provisioned_admin(), FakeTranslations::default());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let grid_uri = format!("/admin/translations?tenant_id={tenant_ulid}");

    // A starting grid with one key.
    let base = serde_json::json!({ "menu.pho": { "en": "Pho" } });
    let put = router
        .clone()
        .oneshot(put_with_etag(&grid_uri, &base, &cookie, "*"))
        .await
        .expect("route the publish");
    assert_eq!(put.status(), StatusCode::NO_CONTENT);

    // A CSV that updates menu.pho, adds menu.tea, and has one row with no en (rejected).
    let csv = "key,en,vi\nmenu.pho,Pho noodles,Phở\nmenu.tea,Tea,Trà\nmenu.rice,,Cơm\n";

    // Dry-run: the report classifies the rows and the grid is untouched.
    let dry = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/translations/import/dry-run?tenant_id={tenant_ulid}"),
            csv.as_bytes().to_vec(),
            "text/csv",
            &cookie,
        ))
        .await
        .expect("route the dry-run");
    assert_eq!(dry.status(), StatusCode::OK);
    let report = json_body(dry).await;
    assert_eq!(report["create_count"], 1);
    assert_eq!(report["update_count"], 1);
    assert_eq!(report["reject_count"], 1);
    let after_dry = router
        .clone()
        .oneshot(get_with_cookie(&grid_uri, &cookie))
        .await
        .expect("route the read");
    assert_eq!(json_body(after_dry).await, base, "a dry-run writes nothing");

    // Apply: the valid rows merge in; the rejected row is skipped.
    let apply = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/translations/import/apply?tenant_id={tenant_ulid}"),
            csv.as_bytes().to_vec(),
            "text/csv",
            &cookie,
        ))
        .await
        .expect("route the apply");
    assert_eq!(apply.status(), StatusCode::OK);
    let applied = router
        .oneshot(get_with_cookie(&grid_uri, &cookie))
        .await
        .expect("route the re-read");
    let grid = json_body(applied).await;
    assert_eq!(grid["menu.pho"]["en"], "Pho noodles", "the update landed");
    assert_eq!(grid["menu.tea"]["vi"], "Trà", "the create landed");
    assert!(
        grid.get("menu.rice").is_none(),
        "the rejected row was skipped"
    );
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
    // And the refusal does not repeat the address back. With an IP literal the caller already knows
    // it, but the same body renders a *resolved* address when the host is a name, and this is the
    // assertion that catches the day someone reinstates it.
    let body = String::from_utf8(
        axum::body::to_bytes(refused.into_body(), usize::MAX)
            .await
            .expect("read the refusal body")
            .to_vec(),
    )
    .expect("the refusal body is text");
    assert!(
        !body.contains("127.0.0.1") && !body.contains("loopback"),
        "the refusal must not report what the address was: {body}"
    );
    // And it is the envelope now, naming the field without re-opening the oracle the message
    // closed: a resolution refusal and a not-resolved refusal share one reason.
    let envelope: serde_json::Value =
        serde_json::from_str(&body).expect("the refusal is an envelope");
    assert_eq!(
        envelope["error"]["status"], "INVALID_ARGUMENT",
        "got {envelope}"
    );
    assert_eq!(envelope["error"]["details"][0]["field"], "url");
    assert_eq!(
        envelope["error"]["details"][0]["reason"], "FORBIDDEN_DESTINATION",
        "not INVALID_FORMAT — the URL's shape is fine, its destination is not: {envelope}"
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

// --- The AIP-193 error envelope (roadmap v3 Q3a, ADR-0026 §27) ----------------------------------

/// A refusal answers the envelope — and two different refusals still answer *identically*.
///
/// The second assertion is the one worth having. `activation_refused` collapses a spent, revoked,
/// unknown and raced code into one response on purpose ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)),
/// so a prober learns nothing from trying. Giving that refusal a *richer* body is exactly the change
/// that could have reintroduced the oracle it was built to close, so this compares the two bodies
/// byte for byte rather than trusting that they are still the same.
#[tokio::test]
async fn a_refused_activation_answers_the_envelope_and_still_gives_no_oracle() {
    let spent = ActivationCode::from_entropy([31; pos_core::activation::PAYLOAD_LEN]);
    let device = DeviceId::new(Ulid::from_u128(0x00C0_FFEE));
    let activations = FakeActivations::with_issued(hash_code(&spent), tenant(), store_id(), device);
    let router = http::activation_router(activations, FakeAdmin::default(), clock());
    let refuse = |code: String| {
        let router = router.clone();
        async move {
            router
                .oneshot(post_json("/activate", &serde_json::json!({ "code": code })))
                .await
                .expect("route the exchange")
        }
    };

    // Spend the code, then present it again: the refusal path.
    let minted = refuse(spent.as_str().to_owned()).await;
    assert_eq!(
        minted.status(),
        StatusCode::CREATED,
        "the code is spent here"
    );
    let replayed = refuse(spent.as_str().to_owned()).await;
    assert_eq!(replayed.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        replayed
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json"),
        "the refusal is the envelope, not plain text"
    );
    let replayed_bytes = body_bytes(replayed).await;
    let body: serde_json::Value =
        serde_json::from_slice(&replayed_bytes).expect("the refusal parses as the envelope");
    assert_eq!(body["error"]["code"], 403);
    assert_eq!(body["error"]["status"], "PERMISSION_DENIED");
    assert_eq!(body["error"]["message"], "activation refused");
    assert!(
        body["error"].get("details").is_none(),
        "a deliberately generic refusal names no field: {body}"
    );

    // A well-formed code that was never issued must be indistinguishable from the spent one.
    let unknown = ActivationCode::from_entropy([200; pos_core::activation::PAYLOAD_LEN]);
    let missed = refuse(unknown.as_str().to_owned()).await;
    assert_eq!(missed.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_bytes(missed).await,
        replayed_bytes,
        "an unknown code and a spent one must answer byte-for-byte alike, or the body is an oracle"
    );
}

/// A throttled login answers the envelope *and* keeps its `Retry-After`.
///
/// The header is the half the body cannot carry: `RESOURCE_EXHAUSTED` says a limit was reached,
/// only `Retry-After` says when to come back. Rebuilding the response around the JSON body is
/// precisely where a header gets dropped, so it is asserted here rather than assumed.
#[tokio::test]
async fn a_rate_limited_login_answers_the_envelope_and_keeps_its_retry_after() {
    let router = login_rate_limited_router(1, 60);
    let bogus = serde_json::json!({ "password": "wrong-passphrase", "totp_code": "000000" });
    let attempt = || {
        let router = router.clone();
        let bogus = bogus.clone();
        async move {
            router
                .oneshot(post_json("/admin/login", &bogus))
                .await
                .expect("route login")
        }
    };
    assert_eq!(
        attempt().await.status(),
        StatusCode::UNAUTHORIZED,
        "the first attempt is inside the limit"
    );

    let throttled = attempt().await;
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        throttled.headers().get("retry-after").is_some(),
        "the envelope must not have cost the client its Retry-After"
    );
    let body = json_body(throttled).await;
    assert_eq!(body["error"]["code"], 429);
    assert_eq!(body["error"]["status"], "RESOURCE_EXHAUSTED");
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

/// Every `/v1` refusal comes back in the documented envelope, not as plain text
/// (`docs/naming-and-api.md` §4, AIP-193).
///
/// `/v1` is the surface with callers outside this repository, so it is the one where the shape of a
/// refusal is a contract rather than a detail. Before this slice the bodies were plain strings and
/// **nothing asserted them** — which is how they stayed plain while the envelope existed — so these
/// assertions are the coverage, not just a regression guard.
#[tokio::test]
async fn a_v1_refusal_comes_back_in_the_documented_envelope() {
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let (known, _price) = known_menu_item();

    // A store the caller's tenant does not own: one generic 404, and now a structured one.
    let response = orders_app(keys, None)
        .oneshot(post_json_bearer(
            "/v1/orders",
            &order_body("api-envelope", known, None),
            &token,
        ))
        .await
        .expect("route the unknown-store order");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], 404, "got {body}");
    assert_eq!(body["error"]["status"], "NOT_FOUND");
    assert_eq!(body["error"]["message"], "no such store");
    assert!(
        body["error"]["details"].is_null(),
        "a refusal that is not about a field carries no details at all, rather than an empty \
         array: {body}"
    );
}

/// A field-level refusal names the field and a stable reason.
///
/// The `message` is prose a person reads and may be reworded; `reason` is the token a client
/// branches on. Asserting both is what keeps them from collapsing into one.
#[tokio::test]
async fn a_v1_refusal_about_a_field_names_the_field_and_a_stable_reason() {
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
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], 400, "got {body}");
    assert_eq!(body["error"]["status"], "INVALID_ARGUMENT");
    assert_eq!(body["error"]["details"][0]["field"], "store_id");
    assert_eq!(body["error"]["details"][0]["reason"], "NOT_A_ULID");
}

/// The `401` keeps its `WWW-Authenticate` header now that its body is built by the shared helper.
///
/// The header is the part a conversion like this quietly drops: it is set on the response *after*
/// the body is built, so replacing the body construction is exactly where it would be lost. A
/// client that cannot see the scheme does not know how to present a key at all.
#[tokio::test]
async fn an_unauthenticated_v1_call_is_enveloped_and_still_names_the_scheme() {
    let response = http::router(app(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
    ))
    .oneshot(get("/v1/stores/whatever/rollups/daily", None))
    .await
    .expect("route the request");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        response.headers().contains_key("www-authenticate"),
        "the challenge header survives the envelope"
    );
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], 401, "got {body}");
    assert_eq!(body["error"]["status"], "UNAUTHENTICATED");
    assert!(
        body["error"]["details"].is_null(),
        "no field-level reason on a credential failure — that would be the oracle the one generic \
         401 exists to avoid: {body}"
    );
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
    ) -> Result<Option<Versioned<ConfigTreeState>>, ConfigStoreError> {
        Ok(None)
    }

    async fn save(
        &self,
        _tenant: TenantId,
        _store: StoreId,
        _state: &ConfigTreeState,
        _expected: Option<&Version>,
    ) -> Result<UpdateOutcome, ConfigStoreError> {
        Ok(UpdateOutcome::Updated(Version::new("1")))
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
///
/// Each row carries the [`Version`] it is at, minted from one shared counter — the fake's stand-in
/// for Postgres's `xmin` (ADR-0094). The point is not to imitate `xmin`'s shape but to satisfy the
/// same contract: a version that changes on every successful update, and an update that applies
/// only when the caller's version still matches. That is what lets the `412` path be proven in the
/// in-process suite on every pull request, where no database is running.
#[derive(Clone, Default)]
struct FakeRegistry {
    tenants: Arc<Mutex<Vec<Versioned<TenantRecord>>>>,
    brands: Arc<Mutex<Vec<Versioned<BrandRecord>>>>,
    stores: Arc<Mutex<Vec<Versioned<StoreRecord>>>>,
    devices: Arc<Mutex<Vec<Versioned<DeviceRecord>>>>,
    next_version: Arc<Mutex<u64>>,
}

impl FakeRegistry {
    /// The next version, as the store-postgres adapter's `xmin::text` is: a token, not a number the
    /// caller may reason about.
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }
}

impl RegistryStore for FakeRegistry {
    async fn create_tenant(&self, tenant: &TenantRecord) -> Result<Version, RegistryStoreError> {
        let version = self.mint();
        self.tenants
            .lock()
            .expect("lock")
            .push(Versioned::new(tenant.clone(), version.clone()));
        Ok(version)
    }

    async fn list_tenants(&self) -> Result<Vec<Versioned<TenantRecord>>, RegistryStoreError> {
        Ok(self.tenants.lock().expect("lock").clone())
    }

    async fn update_tenant(
        &self,
        tenant: &TenantRecord,
        expected: &Version,
    ) -> Result<UpdateOutcome, RegistryStoreError> {
        let version = self.mint();
        let mut rows = self.tenants.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.tenant_id == tenant.tenant_id {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record.name.clone_from(&tenant.name);
                row.record.status = tenant.status;
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
    }

    async fn create_brand(&self, brand: &BrandRecord) -> Result<Version, RegistryStoreError> {
        let version = self.mint();
        self.brands
            .lock()
            .expect("lock")
            .push(Versioned::new(brand.clone(), version.clone()));
        Ok(version)
    }

    async fn list_brands(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<BrandRecord>>, RegistryStoreError> {
        Ok(self
            .brands
            .lock()
            .expect("lock")
            .iter()
            .filter(|brand| brand.record.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_brand(
        &self,
        brand: &BrandRecord,
        expected: &Version,
    ) -> Result<UpdateOutcome, RegistryStoreError> {
        let version = self.mint();
        let mut rows = self.brands.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.brand_id == brand.brand_id && row.record.tenant_id == brand.tenant_id {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record.name.clone_from(&brand.name);
                row.record.status = brand.status;
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
    }

    async fn create_store(&self, store: &StoreRecord) -> Result<Version, RegistryStoreError> {
        let version = self.mint();
        self.stores
            .lock()
            .expect("lock")
            .push(Versioned::new(store.clone(), version.clone()));
        Ok(version)
    }

    async fn list_stores(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<StoreRecord>>, RegistryStoreError> {
        Ok(self
            .stores
            .lock()
            .expect("lock")
            .iter()
            .filter(|store| store.record.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_store(
        &self,
        store: &StoreRecord,
        expected: &Version,
    ) -> Result<UpdateOutcome, RegistryStoreError> {
        let version = self.mint();
        let mut rows = self.stores.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.store_id == store.store_id && row.record.tenant_id == store.tenant_id {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record.name.clone_from(&store.name);
                row.record.brand_id = store.brand_id;
                row.record.status = store.status;
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
    }

    async fn create_device(&self, device: &DeviceRecord) -> Result<Version, RegistryStoreError> {
        let version = self.mint();
        self.devices
            .lock()
            .expect("lock")
            .push(Versioned::new(device.clone(), version.clone()));
        Ok(version)
    }

    async fn list_devices(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<DeviceRecord>>, RegistryStoreError> {
        Ok(self
            .devices
            .lock()
            .expect("lock")
            .iter()
            .filter(|device| {
                device.record.tenant_id == tenant_id && device.record.store_id == store_id
            })
            .cloned()
            .collect())
    }

    async fn update_device(
        &self,
        device: &DeviceRecord,
        expected: &Version,
    ) -> Result<UpdateOutcome, RegistryStoreError> {
        let version = self.mint();
        let mut rows = self.devices.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.device_id == device.device_id && row.record.tenant_id == device.tenant_id
            {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record.name.clone_from(&device.name);
                row.record.kind.clone_from(&device.kind);
                row.record.status = device.status;
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
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
                installed_version: Some("v1.2.3".to_owned()),
                self_test_ok: Some(true),
                reported_at: Some(seen_ago(1_000)),
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
                installed_version: None,
                self_test_ok: None,
                reported_at: None,
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
    assert_eq!(
        online["installed_version"], "v1.2.3",
        "the fleet read surfaces the reported OTA version"
    );
    assert_eq!(online["self_test_ok"], true, "and its self-test outcome");

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
            installed_version: None,
            self_test_ok: None,
            reported_at: None,
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
            installed_version: None,
            self_test_ok: None,
            reported_at: None,
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

    async fn query(
        &self,
        filter: &AuditQuery,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AuditStoreError> {
        let mut rows = self.matching(filter);
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn query_page(
        &self,
        filter: &AuditQuery,
        page: PageRequest,
        order: TrailOrder,
    ) -> Result<Page<AuditEntry>, AuditStoreError> {
        let mut matching = self.matching(filter);
        let total = u32::try_from(matching.len()).unwrap_or(u32::MAX);
        // `matching` is newest-first. Reversing the whole matching set before the window, never the
        // window after it, is what the adapter's `ORDER BY … LIMIT … OFFSET` does — reversing after
        // would flip 25 rows in place and leave every page holding the same rows it did before.
        match order {
            TrailOrder::Newest => {}
            TrailOrder::Oldest => matching.reverse(),
        }
        let items = matching
            .into_iter()
            .skip(page.offset() as usize)
            .take(page.limit() as usize)
            .collect();
        Ok(Page::new(items, total))
    }
}

impl FakeAudit {
    /// Every entry matching `filter`, newest-first and unbounded.
    ///
    /// Shared by both filtered reads so the page cannot match a different set than its own total
    /// counts — the divergence the store-postgres impl avoids by having both queries read one
    /// `AUDIT_FILTERS` string.
    fn matching(&self, filter: &AuditQuery) -> Vec<AuditEntry> {
        let mut rows: Vec<AuditEntry> = self
            .entries
            .lock()
            .expect("lock")
            .iter()
            .filter(|entry| filter.tenant.is_none() || entry.tenant_id == filter.tenant)
            .filter(|entry| {
                filter
                    .entity_type
                    .as_ref()
                    .is_none_or(|value| &entry.entity_type == value)
            })
            .filter(|entry| {
                filter
                    .entity_id
                    .as_ref()
                    .is_none_or(|value| &entry.entity_id == value)
            })
            .filter(|entry| {
                filter
                    .action
                    .as_ref()
                    .is_none_or(|value| &entry.action == value)
            })
            .filter(|entry| {
                filter
                    .actor_admin_id
                    .as_ref()
                    .is_none_or(|value| &entry.actor.admin_id == value)
            })
            .filter(|entry| {
                filter
                    .since_ms
                    .is_none_or(|since| entry.at.as_milliseconds_since_epoch() >= since)
            })
            .filter(|entry| {
                filter
                    .until_ms
                    .is_none_or(|until| entry.at.as_milliseconds_since_epoch() <= until)
            })
            .cloned()
            .collect();
        rows.reverse(); // stored oldest-first; the read is newest-first.
        rows
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

/// Every page of the trail in one order, stitched into the sequence a caller paging through sees.
///
/// Reads until a page comes back empty rather than dividing by a total, so a page that dropped or
/// repeated a row shows up in the stitched sequence instead of being masked by the arithmetic.
async fn stitched_trail(
    audit: &FakeAudit,
    filter: &AuditQuery,
    order: TrailOrder,
    limit: u32,
) -> Vec<String> {
    let mut actions: Vec<String> = Vec::new();
    let mut offset = 0;
    loop {
        let page = PageRequest::new(limit, offset).expect("a valid page");
        let read = audit
            .query_page(filter, page, order)
            .await
            .expect("a page of the trail");
        if read.items.is_empty() {
            return actions;
        }
        actions.extend(read.items.iter().map(|entry| entry.action.clone()));
        offset += limit;
    }
}

/// The paged trail reads from either end, and both orders window one set.
///
/// Asserted as a whole stitched sequence rather than as two literal first pages: reversing *after*
/// the window — flipping one page's rows in place — would satisfy "the first page starts at the
/// other end" and still leave every page holding exactly the rows it held before.
#[tokio::test]
async fn the_paged_trail_reads_from_either_end_and_both_orders_window_the_same_set() {
    let audit = FakeAudit::default();
    let entry = |step: i64| AuditEntry {
        id: AuditId::new(Ulid::from_u128(u128::try_from(step).expect("a small step"))),
        tenant_id: Some(tenant()),
        actor: AuditActor {
            admin_id: "01ADMIN0000000000000000OPS".to_owned(),
            email: "ops@pizza4ps.test".to_owned(),
            role: AdminRole::Owner,
        },
        action: format!("store.step{step}"),
        entity_type: "store".to_owned(),
        entity_id: store_id().to_string(),
        before: None,
        after: None,
        request_id: None,
        at: Timestamp::from_milliseconds_since_epoch(NOW_MS - (5 - step) * 1_000)
            .expect("a valid instant"),
    };
    // Five entries a second apart — an odd count, so the last page is short in either direction.
    for step in 1..=5 {
        audit.append(&entry(step)).await.expect("append");
    }
    let filter = AuditQuery {
        tenant: Some(tenant()),
        ..AuditQuery::default()
    };

    let newest = stitched_trail(&audit, &filter, TrailOrder::Newest, 2).await;
    let oldest = stitched_trail(&audit, &filter, TrailOrder::Oldest, 2).await;
    assert_eq!(
        newest,
        vec![
            "store.step5".to_owned(),
            "store.step4".to_owned(),
            "store.step3".to_owned(),
            "store.step2".to_owned(),
            "store.step1".to_owned(),
        ],
        "newest-first is the default and is unchanged by the order existing",
    );
    let mut reversed = oldest.clone();
    reversed.reverse();
    assert_eq!(
        reversed, newest,
        "one order is the other read backwards — every page of it, not one page flipped in place",
    );

    // The order chooses which page a row lands on; it never changes which rows matched.
    let first = PageRequest::new(2, 0).expect("a valid page");
    let counted = audit
        .query_page(&filter, first, TrailOrder::Oldest)
        .await
        .expect("the first page oldest-first");
    assert_eq!(counted.total, 5, "the total is the match count either way");
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

/// A router over a trail of three entries: two `store` rows and one `tenant` row, oldest first.
///
/// Shared by the filter test and the paging test so both read the same trail — a page whose set
/// differed from the filtered read's would prove nothing about the two agreeing.
async fn audit_trail_app() -> axum::Router {
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
    http::router(app).merge(http::audit_router(audit, admin, clock()))
}

#[tokio::test]
async fn the_audit_read_filters_and_needs_a_session() {
    let router = audit_trail_app().await;

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
        .clone()
        .oneshot(get_with_cookie("/admin/audit?action=store.update", &cookie))
        .await
        .expect("route the action-filtered read");
    let updates = json_body(updates).await;
    assert_eq!(updates.as_array().expect("array").len(), 1);
    assert_eq!(updates[0]["action"], "store.update");
}

#[tokio::test]
async fn the_audit_read_pages_when_asked_for_an_offset_and_windows_when_not() {
    let router = audit_trail_app().await;
    let cookie = admin_cookie(&router).await;

    // A limit alone is still the *windowed* read ADR-0069 shipped: the newest N as a bare array,
    // not a page. This is why `offset` and not `limit` is what asks this route for a page — a
    // caller sending `?limit=200` today must keep getting what it gets today (ADR-0098).
    let windowed = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit?limit=2", &cookie))
        .await
        .expect("route the windowed read");
    let windowed = json_body(windowed).await;
    let windowed = windowed
        .as_array()
        .expect("a bare array, not a paged envelope");
    assert_eq!(windowed.len(), 2, "the newest two");
    assert_eq!(windowed[0]["action"], "tenant.update", "still newest-first");

    // An offset asks for the paged form: the window, plus how many matched in total.
    let page = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit?limit=2&offset=0", &cookie))
        .await
        .expect("route the first page");
    let page = json_body(page).await;
    assert_eq!(page["items"].as_array().expect("the window").len(), 2);
    assert_eq!(
        page["total"], 3,
        "the total is the match count, not the page"
    );
    assert_eq!(page["limit"], 2);
    assert_eq!(page["offset"], 0);

    let tail = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit?limit=2&offset=2", &cookie))
        .await
        .expect("route the second page");
    let tail = json_body(tail).await;
    assert_eq!(tail["items"].as_array().expect("the window").len(), 1);
    assert_eq!(tail["total"], 3);
    assert_eq!(
        tail["items"][0]["action"], "store.create",
        "the oldest entry is on the last page"
    );

    // The total counts what the *filters* matched, not the whole log — otherwise a pager over a
    // filtered view would offer pages that are empty.
    let filtered = router
        .clone()
        .oneshot(get_with_cookie(
            "/admin/audit?entity_type=store&limit=1&offset=0",
            &cookie,
        ))
        .await
        .expect("route the filtered page");
    let filtered = json_body(filtered).await;
    assert_eq!(filtered["items"].as_array().expect("the window").len(), 1);
    assert_eq!(filtered["total"], 2, "two store entries, not three entries");

    // The windowed form clamps an over-large limit (ADR-0069's behaviour, kept); the paged form
    // refuses it, because there a clamp answers a different question than the one asked.
    let clamped = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit?limit=100000", &cookie))
        .await
        .expect("route the over-large window");
    assert_eq!(
        clamped.status(),
        StatusCode::OK,
        "the windowed read pulls the bound into range rather than refusing"
    );
    assert_eq!(json_body(clamped).await.as_array().expect("array").len(), 3);

    for (query, field) in [
        ("limit=100000&offset=0", "limit"),
        ("limit=0&offset=0", "limit"),
        ("limit=2&offset=lots", "offset"),
        ("offset=2", "offset"),
        ("limit=lots", "limit"),
    ] {
        let refused = router
            .clone()
            .oneshot(get_with_cookie(&format!("/admin/audit?{query}"), &cookie))
            .await
            .expect("route the bad bound");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "`{query}` is a client mistake, not a page",
        );
        assert_eq!(
            json_body(refused).await["error"]["details"][0]["field"],
            field,
            "`{query}` names the parameter that was wrong",
        );
    }
}

#[tokio::test]
async fn the_paged_audit_read_takes_an_order_and_the_windowed_one_refuses_it() {
    let router = audit_trail_app().await;
    let cookie = admin_cookie(&router).await;
    let read = |query: &str| {
        let uri = format!("/admin/audit?{query}");
        let cookie = cookie.clone();
        let router = router.clone();
        async move {
            router
                .oneshot(get_with_cookie(&uri, &cookie))
                .await
                .expect("route the read")
        }
    };

    // Naming the default changes nothing: a caller that omits `order` and one that spells out the
    // trail's own order get the same page.
    let defaulted = json_body(read("limit=3&offset=0").await).await;
    let named = json_body(read("limit=3&offset=0&order=newest").await).await;
    assert_eq!(
        defaulted["items"], named["items"],
        "`newest` is the default"
    );
    assert_eq!(defaulted["items"][0]["action"], "tenant.update");

    // The other end. The trail is `store.create`, `store.update`, `tenant.update` oldest-first.
    let oldest = json_body(read("limit=1&offset=0&order=oldest").await).await;
    assert_eq!(
        oldest["items"][0]["action"], "store.create",
        "oldest-first starts at the beginning of the trail"
    );
    assert_eq!(
        oldest["total"], 3,
        "the total is the match count, which the order does not change"
    );
    let tail = json_body(read("limit=1&offset=2&order=oldest").await).await;
    assert_eq!(
        tail["items"][0]["action"], "tenant.update",
        "and the window walks to the newest entry rather than flipping one page in place"
    );

    // The windowed read refuses the order rather than ignoring it: there `limit` already means "the
    // most recent this many", so an order has two honest readings and the route will not guess.
    let refused = read("order=oldest").await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let refused = json_body(refused).await;
    assert_eq!(refused["error"]["details"][0]["field"], "order");
    assert_eq!(
        refused["error"]["details"][0]["reason"], "MISSING_DEPENDENT_FIELD",
        "the fix is to name an offset, not to change the value"
    );

    // A token that names no order is refused with the two that do.
    let unknown = read("limit=1&offset=0&order=ascending").await;
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    let unknown = json_body(unknown).await;
    assert_eq!(unknown["error"]["details"][0]["field"], "order");
    assert_eq!(
        unknown["error"]["message"], "order must be newest or oldest",
        "the refusal names what this route accepts, so a caller need not guess again"
    );
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
    let created_body = json_body(created).await;
    let tenant_id = created_body["tenant_id"]
        .as_str()
        .expect("a tenant id")
        .to_owned();
    // A create hands back the version it starts at, so an edit needs no second round trip
    // (ADR-0094).
    let etag = created_body["etag"].as_str().expect("an etag").to_owned();

    // Rename it, naming the version being replaced.
    let renamed = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/tenants/{tenant_id}"),
            &serde_json::json!({ "name": "Pizza 4P's", "status": "active" }),
            &cookie,
            &etag,
        ))
        .await
        .expect("route rename");
    assert_eq!(renamed.status(), StatusCode::OK);
    let renamed_body = json_body(renamed).await;
    assert_eq!(renamed_body["name"], "Pizza 4P's");
    let next_etag = renamed_body["etag"].as_str().expect("a new etag");
    assert_ne!(
        next_etag, etag,
        "an applied update moves the version, or the next write would be unguarded"
    );

    // Renaming an unknown tenant is a 404, not a silent success.
    let missing = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/tenants/{}", Ulid::from_u128(9_999)),
            &serde_json::json!({ "name": "Nope", "status": "active" }),
            &cookie,
            &etag,
        ))
        .await
        .expect("route rename missing");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

// --- Optimistic concurrency on `/admin` (Q3c, ADR-0094) -----------------------------------------
//
// Before this, every `/admin` PATCH was last-write-wins: the body carries the whole record, so an
// admin saving a form they loaded a minute ago wrote their stale copy of every *other* field back
// over whoever edited in between, and both saw success. These cover the mechanism that closes it —
// the refusal, what it protects, and the two headers a client could get wrong.

/// The defect ADR-0094 closes, end to end: a second writer holding a stale copy is refused, and the
/// first writer's edit is still there afterwards.
///
/// The second assertion is the one that matters. A `412` that still let the write through would be
/// worse than no check at all, because the caller would be told it had been stopped.
#[tokio::test]
async fn a_second_writer_holding_a_stale_version_is_refused_and_clobbers_nothing() {
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
    let created_body = json_body(created).await;
    let tenant_id = created_body["tenant_id"]
        .as_str()
        .expect("a tenant id")
        .to_owned();
    // Both admins load the record at the same version.
    let both_read = created_body["etag"].as_str().expect("an etag").to_owned();

    // The first admin saves.
    let first = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/tenants/{tenant_id}"),
            &serde_json::json!({ "name": "Pizza 4P's", "status": "active" }),
            &cookie,
            &both_read,
        ))
        .await
        .expect("route the first save");
    assert_eq!(first.status(), StatusCode::OK);

    // The second admin saves, still holding the version they read before the first save.
    let second = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/tenants/{tenant_id}"),
            &serde_json::json!({ "name": "Stale Overwrite", "status": "archived" }),
            &cookie,
            &both_read,
        ))
        .await
        .expect("route the second save");
    assert_eq!(second.status(), StatusCode::PRECONDITION_FAILED);
    let body = json_body(second).await;
    assert_eq!(body["error"]["code"], 412, "got {body}");
    assert_eq!(body["error"]["status"], "VERSION_MISMATCH");
    assert!(
        body["error"]["details"].is_null(),
        "the caller's fields were all fine; what went stale is its copy: {body}"
    );

    // And the first admin's edit survived.
    let listed = router
        .oneshot(get_with_cookie("/admin/tenants", &cookie))
        .await
        .expect("route the listing");
    let rows = json_body(listed).await;
    assert_eq!(
        rows[0]["name"], "Pizza 4P's",
        "the refused write must not have applied: {rows}"
    );
    assert_eq!(
        rows[0]["status"], "active",
        "nor any other field it carried: {rows}"
    );
}

/// A list row's `etag` is the token a write sends back, unchanged.
///
/// A header cannot carry a version per row, so a list carries one per row in the body. If the two
/// forms were not the same string a client would have to know which endpoint it read from before it
/// could write, which is exactly the coupling the opaque token exists to avoid.
#[tokio::test]
async fn the_etag_a_listing_hands_out_is_the_one_a_write_sends_back() {
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

    let listed = router
        .clone()
        .oneshot(get_with_cookie("/admin/tenants", &cookie))
        .await
        .expect("route the listing");
    let rows = json_body(listed).await;
    let from_list = rows[0]["etag"].as_str().expect("a row etag").to_owned();

    let renamed = router
        .oneshot(patch_with_etag(
            &format!("/admin/tenants/{tenant_id}"),
            &serde_json::json!({ "name": "Pizza 4P's", "status": "active" }),
            &cookie,
            &from_list,
        ))
        .await
        .expect("route the rename");
    assert_eq!(
        renamed.status(),
        StatusCode::OK,
        "the token read from a list row is accepted verbatim as an If-Match"
    );
    // And the write answers with an `ETag` header carrying the same string as the body's `etag`,
    // so a client has one thing to remember rather than two that could drift.
    let header = renamed
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .expect("an ETag header")
        .to_owned();
    let after = json_body(renamed).await["etag"]
        .as_str()
        .expect("an etag")
        .to_owned();
    assert_eq!(header, format!("\"{after}\""));
}

/// An absent `If-Match` is an ordinary missing-field refusal, not a status of its own.
///
/// And it is a refusal at all: treating "no opinion" as "overwrite" would leave the silent clobber
/// in place as the default for every caller that had not been updated.
#[tokio::test]
async fn a_write_without_if_match_is_refused_and_names_the_header_as_the_field() {
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

    let refused = router
        .oneshot(patch_with_cookie(
            &format!("/admin/tenants/{tenant_id}"),
            &serde_json::json!({ "name": "Pizza 4P's", "status": "active" }),
            &cookie,
        ))
        .await
        .expect("route the update");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["status"], "INVALID_ARGUMENT", "got {body}");
    assert_eq!(body["error"]["details"][0]["field"], "if-match");
    assert_eq!(body["error"]["details"][0]["reason"], "REQUIRED");
}

/// `If-Match: *` is refused, and refused *differently* from a malformed one.
///
/// It is the one header that would quietly restore last-write-wins while looking like compliance,
/// so the caller is told what is wrong with it rather than that it could not be parsed.
#[tokio::test]
async fn a_wildcard_if_match_is_refused_on_its_own_terms() {
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

    let refused = router
        .clone()
        .oneshot(patch_with_raw_if_match(
            &format!("/admin/tenants/{tenant_id}"),
            &serde_json::json!({ "name": "Pizza 4P's", "status": "active" }),
            &cookie,
            "*",
        ))
        .await
        .expect("route the update");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["details"][0]["field"], "if-match");
    assert_eq!(
        body["error"]["details"][0]["reason"], "WILDCARD_NOT_ACCEPTED",
        "distinct from INVALID_FORMAT: the header parses, it just asks for the wrong thing"
    );

    let malformed = router
        .oneshot(patch_with_raw_if_match(
            &format!("/admin/tenants/{tenant_id}"),
            &serde_json::json!({ "name": "Pizza 4P's", "status": "active" }),
            &cookie,
            "W/\"1\"",
        ))
        .await
        .expect("route the update");
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    let body = json_body(malformed).await;
    assert_eq!(body["error"]["details"][0]["reason"], "INVALID_FORMAT");
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
    // ADR-0096: one field out of range is a `400` naming it, not a `422`.
    assert_eq!(short_password.status(), StatusCode::BAD_REQUEST);
    let body = json_body(short_password).await;
    assert_eq!(
        body["error"]["details"][0]["field"], "password",
        "got {body}"
    );
    assert_eq!(body["error"]["details"][0]["reason"], "OUT_OF_RANGE");
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
    items: Arc<Mutex<Vec<Versioned<CatalogItem>>>>,
    tax_classes: Arc<Mutex<Vec<Versioned<TaxClass>>>>,
    categories: Arc<Mutex<Vec<Versioned<ItemCategory>>>>,
    subcategories: Arc<Mutex<Vec<Versioned<ItemSubcategory>>>>,
    display_categories: Arc<Mutex<Vec<Versioned<DisplayCategory>>>>,
    display_subcategories: Arc<Mutex<Vec<Versioned<DisplaySubcategory>>>>,
    layout_buttons: Arc<Mutex<Vec<Versioned<LayoutButton>>>>,
    modifier_groups: Arc<Mutex<Vec<Versioned<ModifierGroup>>>>,
    menus: Arc<Mutex<Vec<Versioned<Menu>>>>,
    menu_sections: Arc<Mutex<Vec<Versioned<MenuSection>>>>,
    placements: Arc<Mutex<Vec<Versioned<MenuPlacement>>>>,
    next_version: Arc<Mutex<u64>>,
}

impl FakeCatalog {
    /// The fake's stand-in for `xmin` (ADR-0094): a token that changes on every successful write,
    /// which is the only property the seam contract needs.
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }

    /// The tenant's items, in insertion order — shared by the whole-set and paged reads.
    fn tenant_items(&self, tenant_id: TenantId) -> Vec<Versioned<CatalogItem>> {
        self.items
            .lock()
            .expect("lock")
            .iter()
            .filter(|item| item.record.tenant_id == tenant_id)
            .cloned()
            .collect()
    }

    /// The tenant's items matching `filter`, in the order it asks for — newest-first by default,
    /// with every tie broken on the item id so each order is total (ADR-0098 decision 9).
    fn filtered_items(
        &self,
        tenant_id: TenantId,
        filter: &ItemListFilter,
    ) -> Vec<Versioned<CatalogItem>> {
        let needle = filter.search.as_ref().map(|text| text.to_lowercase());
        let mut matching: Vec<Versioned<CatalogItem>> = self
            .tenant_items(tenant_id)
            .into_iter()
            .filter(|item| {
                needle.as_ref().is_none_or(|needle| {
                    item.record.name.to_lowercase().contains(needle)
                        || item
                            .record
                            .name_translations
                            .values()
                            .any(|value| value.to_lowercase().contains(needle))
                })
            })
            .collect();
        matching.reverse();
        match filter.sort {
            ItemSort::Newest => {}
            ItemSort::Name => matching.sort_by(|left, right| {
                left.record
                    .name
                    .cmp(&right.record.name)
                    .then_with(|| right.record.menu_item_id.cmp(&left.record.menu_item_id))
            }),
            ItemSort::Status => matching.sort_by(|left, right| {
                left.record
                    .status
                    .as_str()
                    .cmp(right.record.status.as_str())
                    .then_with(|| right.record.menu_item_id.cmp(&left.record.menu_item_id))
            }),
        }
        if filter.descending {
            matching.reverse();
        }
        matching
    }
}

impl CatalogStore for FakeCatalog {
    async fn create_item(&self, item: &CatalogItem) -> Result<Version, CatalogStoreError> {
        let version = self.mint();
        self.items
            .lock()
            .expect("lock")
            .push(Versioned::new(item.clone(), version.clone()));
        Ok(version)
    }

    async fn list_items(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<CatalogItem>>, CatalogStoreError> {
        Ok(self.tenant_items(tenant_id))
    }

    async fn list_items_page(
        &self,
        tenant_id: TenantId,
        page: PageRequest,
        filter: &ItemListFilter,
    ) -> Result<Page<Versioned<CatalogItem>>, CatalogStoreError> {
        let matching = self.filtered_items(tenant_id, filter);
        let total = u32::try_from(matching.len()).unwrap_or(u32::MAX);
        let items = matching
            .into_iter()
            .skip(page.offset() as usize)
            .take(page.limit() as usize)
            .collect();
        Ok(Page::new(items, total))
    }

    async fn update_item(
        &self,
        item: &CatalogItem,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.items.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.menu_item_id == item.menu_item_id
                && row.record.tenant_id == item.tenant_id
            {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record.name.clone_from(&item.name);
                row.record
                    .name_translations
                    .clone_from(&item.name_translations);
                row.record.tax_class_id = item.tax_class_id;
                row.record.item_category_id = item.item_category_id;
                row.record.item_subcategory_id = item.item_subcategory_id;
                row.record.image_ref = item.image_ref;
                row.record.status = item.status;
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
    }

    async fn create_tax_class(&self, tax_class: &TaxClass) -> Result<Version, CatalogStoreError> {
        let version = self.mint();
        self.tax_classes
            .lock()
            .expect("lock")
            .push(Versioned::new(tax_class.clone(), version.clone()));
        Ok(version)
    }

    async fn list_tax_classes(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<TaxClass>>, CatalogStoreError> {
        Ok(self
            .tax_classes
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.record.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_tax_class(
        &self,
        tax_class: &TaxClass,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.tax_classes.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.tax_class_id == tax_class.tax_class_id
                && row.record.tenant_id == tax_class.tenant_id
            {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record.name.clone_from(&tax_class.name);
                row.record.status = tax_class.status;
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
    }

    async fn create_item_category(
        &self,
        category: &ItemCategory,
    ) -> Result<Version, CatalogStoreError> {
        let version = self.mint();
        self.categories
            .lock()
            .expect("lock")
            .push(Versioned::new(category.clone(), version.clone()));
        Ok(version)
    }

    async fn list_item_categories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<ItemCategory>>, CatalogStoreError> {
        Ok(self
            .categories
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.record.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_item_category(
        &self,
        category: &ItemCategory,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.categories.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.item_category_id == category.item_category_id
                && row.record.tenant_id == category.tenant_id
            {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record.name.clone_from(&category.name);
                row.record.status = category.status;
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
    }

    async fn create_item_subcategory(
        &self,
        subcategory: &ItemSubcategory,
    ) -> Result<Version, CatalogStoreError> {
        let version = self.mint();
        self.subcategories
            .lock()
            .expect("lock")
            .push(Versioned::new(subcategory.clone(), version.clone()));
        Ok(version)
    }

    async fn list_item_subcategories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<ItemSubcategory>>, CatalogStoreError> {
        Ok(self
            .subcategories
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.record.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_item_subcategory(
        &self,
        subcategory: &ItemSubcategory,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.subcategories.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.item_subcategory_id == subcategory.item_subcategory_id
                && row.record.tenant_id == subcategory.tenant_id
            {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record.name.clone_from(&subcategory.name);
                row.record.item_category_id = subcategory.item_category_id;
                row.record.status = subcategory.status;
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
    }

    async fn create_display_category(
        &self,
        category: &DisplayCategory,
    ) -> Result<Version, CatalogStoreError> {
        let version = self.mint();
        self.display_categories
            .lock()
            .expect("lock")
            .push(Versioned::new(category.clone(), version.clone()));
        Ok(version)
    }

    async fn list_display_categories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<DisplayCategory>>, CatalogStoreError> {
        Ok(self
            .display_categories
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.record.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_display_category(
        &self,
        category: &DisplayCategory,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.display_categories.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.display_category_id == category.display_category_id
                && row.record.tenant_id == category.tenant_id
            {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record.name.clone_from(&category.name);
                row.record.status = category.status;
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
    }

    async fn create_display_subcategory(
        &self,
        subcategory: &DisplaySubcategory,
    ) -> Result<Version, CatalogStoreError> {
        let version = self.mint();
        self.display_subcategories
            .lock()
            .expect("lock")
            .push(Versioned::new(subcategory.clone(), version.clone()));
        Ok(version)
    }

    async fn list_display_subcategories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<DisplaySubcategory>>, CatalogStoreError> {
        Ok(self
            .display_subcategories
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.record.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_display_subcategory(
        &self,
        subcategory: &DisplaySubcategory,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.display_subcategories.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.display_subcategory_id == subcategory.display_subcategory_id
                && row.record.tenant_id == subcategory.tenant_id
            {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record.name.clone_from(&subcategory.name);
                row.record.display_category_id = subcategory.display_category_id;
                row.record.status = subcategory.status;
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
    }

    async fn create_layout_button(
        &self,
        button: &LayoutButton,
    ) -> Result<CreateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.layout_buttons.lock().expect("lock");
        if rows.iter().any(|row| {
            row.record.tenant_id == button.tenant_id
                && row.record.sales_channel == button.sales_channel
                && row.record.menu_item_id == button.menu_item_id
        }) {
            return Ok(CreateOutcome::AlreadyExists);
        }
        rows.push(Versioned::new(button.clone(), version.clone()));
        Ok(CreateOutcome::Created(version))
    }

    async fn update_layout_button(
        &self,
        button: &LayoutButton,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.layout_buttons.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|row| {
            row.record.tenant_id == button.tenant_id
                && row.record.sales_channel == button.sales_channel
                && row.record.menu_item_id == button.menu_item_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record = button.clone();
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }

    async fn list_layout_buttons(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<LayoutButton>>, CatalogStoreError> {
        Ok(self
            .layout_buttons
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.record.tenant_id == tenant_id)
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
            !(row.record.tenant_id == tenant_id
                && row.record.sales_channel == sales_channel
                && row.record.menu_item_id == menu_item_id)
        });
        Ok(rows.len() != before)
    }

    async fn create_modifier_group(
        &self,
        group: &ModifierGroup,
    ) -> Result<Version, CatalogStoreError> {
        let version = self.mint();
        self.modifier_groups
            .lock()
            .expect("lock")
            .push(Versioned::new(group.clone(), version.clone()));
        Ok(version)
    }

    async fn list_modifier_groups(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<ModifierGroup>>, CatalogStoreError> {
        Ok(self
            .modifier_groups
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.record.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_modifier_group(
        &self,
        group: &ModifierGroup,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.modifier_groups.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.modifier_group_id == group.modifier_group_id
                && row.record.tenant_id == group.tenant_id
            {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record = group.clone();
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
    }

    async fn create_menu(&self, menu: &Menu) -> Result<Version, CatalogStoreError> {
        let version = self.mint();
        self.menus
            .lock()
            .expect("lock")
            .push(Versioned::new(menu.clone(), version.clone()));
        Ok(version)
    }

    async fn list_menus(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<Menu>>, CatalogStoreError> {
        Ok(self
            .menus
            .lock()
            .expect("lock")
            .iter()
            .filter(|menu| menu.record.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn update_menu(
        &self,
        menu: &Menu,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.menus.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.menu_id == menu.menu_id && row.record.tenant_id == menu.tenant_id {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record.name.clone_from(&menu.name);
                row.record.parent_menu_id = menu.parent_menu_id;
                row.record.status = menu.status;
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
    }

    async fn create_menu_section(
        &self,
        section: &MenuSection,
    ) -> Result<Version, CatalogStoreError> {
        let version = self.mint();
        self.menu_sections
            .lock()
            .expect("lock")
            .push(Versioned::new(section.clone(), version.clone()));
        Ok(version)
    }

    async fn list_menu_sections(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> Result<Vec<Versioned<MenuSection>>, CatalogStoreError> {
        Ok(self
            .menu_sections
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.record.tenant_id == tenant_id && row.record.menu_id == menu_id)
            .cloned()
            .collect())
    }

    async fn update_menu_section(
        &self,
        section: &MenuSection,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.menu_sections.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.record.menu_section_id == section.menu_section_id
                && row.record.tenant_id == section.tenant_id
            {
                if &row.etag != expected {
                    return Ok(UpdateOutcome::VersionMismatch);
                }
                row.record.name.clone_from(&section.name);
                row.record.sort = section.sort;
                row.record.status = section.status;
                row.etag = version.clone();
                return Ok(UpdateOutcome::Updated(version));
            }
        }
        Ok(UpdateOutcome::NotFound)
    }

    async fn create_placement(
        &self,
        placement: &MenuPlacement,
    ) -> Result<CreateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.placements.lock().expect("lock");
        if rows.iter().any(|row| {
            row.record.tenant_id == placement.tenant_id
                && row.record.menu_id == placement.menu_id
                && row.record.menu_item_id == placement.menu_item_id
        }) {
            return Ok(CreateOutcome::AlreadyExists);
        }
        rows.push(Versioned::new(placement.clone(), version.clone()));
        Ok(CreateOutcome::Created(version))
    }

    async fn update_placement(
        &self,
        placement: &MenuPlacement,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.placements.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|row| {
            row.record.tenant_id == placement.tenant_id
                && row.record.menu_id == placement.menu_id
                && row.record.menu_item_id == placement.menu_item_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record = placement.clone();
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }

    async fn list_placements(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> Result<Vec<Versioned<MenuPlacement>>, CatalogStoreError> {
        Ok(self
            .placements
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.record.tenant_id == tenant_id && row.record.menu_id == menu_id)
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
            !(row.record.tenant_id == tenant_id
                && row.record.menu_id == menu_id
                && row.record.menu_item_id == menu_item_id)
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

/// Clearing an optional id reference means "unset", on every field that has one.
///
/// `brand_id` on a store and `parent_menu_id` on a menu were the two fields whose parse skipped the
/// trim and the empty filter, so an empty string was a `400` for them and "unset" for the other five
/// optional-ULID fields on the same surface (#280). The console only avoided the refusal because
/// every call site mapped `""` to `null` first; nothing on the server said it had to. Both routes
/// are exercised here because the unit test on `parse_optional_ulid` cannot see a handler that stops
/// calling it.
#[tokio::test]
async fn an_empty_optional_id_means_unset_on_the_routes_that_disagreed() {
    let tenant = ulid_text(1);

    // `parent_menu_id: ""` — a top-level menu, not a malformed request.
    let catalog = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&catalog).await;
    let created = catalog
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/menus",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard", "parent_menu_id": "" }),
            &cookie,
        ))
        .await
        .expect("route create menu");
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(
        json_body(created).await["parent_menu_id"],
        serde_json::Value::Null,
        "a cleared parent is no parent"
    );

    // And a present-but-malformed value is still the caller's error, naming the field.
    let refused = catalog
        .oneshot(post_with_cookie(
            "/admin/catalog/menus",
            &serde_json::json!({ "tenant_id": tenant, "name": "Broken", "parent_menu_id": "nope" }),
            &cookie,
        ))
        .await
        .expect("route the malformed create");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["details"][0]["field"], "parent_menu_id");

    // `brand_id: ""` — a store with no brand.
    let registry = registry_app(provisioned_admin(), FakeRegistry::default());
    let cookie = admin_cookie(&registry).await;
    let created = registry
        .clone()
        .oneshot(post_with_cookie(
            "/admin/stores",
            &serde_json::json!({ "tenant_id": tenant, "name": "Bến Thành", "brand_id": "" }),
            &cookie,
        ))
        .await
        .expect("route create store");
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(
        json_body(created).await["brand_id"],
        serde_json::Value::Null,
        "a cleared brand is no brand"
    );

    let refused = registry
        .oneshot(post_with_cookie(
            "/admin/stores",
            &serde_json::json!({ "tenant_id": tenant, "name": "Broken", "brand_id": "nope" }),
            &cookie,
        ))
        .await
        .expect("route the malformed create");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["details"][0]["field"], "brand_id");
}

/// A ULID string an operator never types — the routes accept it in the body/path, the fake scopes by
/// it. Distinct constants keep the tenant, a menu, an item and a tax class from colliding.
fn ulid_text(n: u128) -> String {
    Ulid::from_u128(n).to_string()
}

/// An in-memory `TaxRateStore` for the route tests (ADR-0074, Track M4), tenant-scoped like the real
/// adapter; `set` replaces the tenant's rows and leaves other tenants alone.
///
/// `versions` mirrors the `catalog_tax_rate_versions` row (migration 0039): one token per tenant,
/// moved by every applied save, because the rate rows themselves are deleted and reinserted and so
/// have no version that survives one ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
#[derive(Clone, Default)]
struct FakeTaxRates {
    rows: Arc<Mutex<Vec<(TenantId, TaxRateEntry)>>>,
    versions: Arc<Mutex<HashMap<TenantId, Version>>>,
    next_version: Arc<Mutex<u64>>,
}

impl FakeTaxRates {
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(format!("tx{next}"))
    }
}

impl TaxRateStore for FakeTaxRates {
    async fn list_tax_rates(
        &self,
        tenant_id: TenantId,
    ) -> Result<(Vec<TaxRateEntry>, Option<Version>), TaxRateStoreError> {
        let entries = self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|(owner, _entry)| *owner == tenant_id)
            .map(|(_owner, entry)| *entry)
            .collect();
        let version = self.versions.lock().expect("lock").get(&tenant_id).cloned();
        Ok((entries, version))
    }

    async fn set_tax_rates(
        &self,
        tenant_id: TenantId,
        entries: &[TaxRateEntry],
        expected: Option<&Version>,
    ) -> Result<UpdateOutcome, TaxRateStoreError> {
        let version = self.mint();
        let mut versions = self.versions.lock().expect("lock");
        let refusal = match (versions.get(&tenant_id), expected) {
            (None, None) => None,
            (None, Some(_)) => Some(UpdateOutcome::NotFound),
            (Some(_), None) => Some(UpdateOutcome::VersionMismatch),
            (Some(stored), Some(expected)) => {
                (stored != expected).then_some(UpdateOutcome::VersionMismatch)
            }
        };
        if let Some(refusal) = refusal {
            return Ok(refusal);
        }
        versions.insert(tenant_id, version.clone());
        let mut rows = self.rows.lock().expect("lock");
        rows.retain(|(owner, _entry)| *owner != tenant_id);
        rows.extend(entries.iter().map(|entry| (tenant_id, *entry)));
        Ok(UpdateOutcome::Updated(version))
    }
}

fn tax_rate_app(admin: FakeAdmin, catalog: FakeCatalog, tax_rates: FakeTaxRates) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::tax_rate_router(
        tax_rates,
        catalog,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// Seeds a tax class straight into the fake, so a rate can name a known class without the catalog
/// create route.
fn seed_tax_class(catalog: &FakeCatalog, tenant: &str, tax_class_id: &str, name: &str) {
    catalog
        .tax_classes
        .lock()
        .expect("lock")
        .push(Versioned::new(
            TaxClass {
                tax_class_id: tax_class_id
                    .parse::<Ulid>()
                    .map(TaxClassId::new)
                    .expect("a class ULID"),
                tenant_id: tenant
                    .parse::<Ulid>()
                    .map(TenantId::new)
                    .expect("a tenant ULID"),
                name: name.to_owned(),
                status: EntityStatus::Active,
            },
            // Seeded straight in, so it starts at a version nothing has read yet.
            catalog.mint(),
        ));
}

/// An in-memory `MediaStore` for the route tests (ADR-0075), tenant-scoped like the real adapter.
#[derive(Clone, Default)]
struct FakeMedia {
    assets: Arc<Mutex<Vec<NewMediaAsset>>>,
}

impl MediaStore for FakeMedia {
    async fn put(&self, asset: &NewMediaAsset) -> Result<(), MediaStoreError> {
        self.assets.lock().expect("lock").push(asset.clone());
        Ok(())
    }

    async fn get(
        &self,
        tenant_id: TenantId,
        media_id: MediaId,
        rendition: Rendition,
    ) -> Result<Option<Vec<u8>>, MediaStoreError> {
        Ok(self
            .assets
            .lock()
            .expect("lock")
            .iter()
            .find(|asset| asset.tenant_id == tenant_id && asset.media_id == media_id)
            .map(|asset| match rendition {
                Rendition::Thumbnail => asset.thumbnail.clone(),
                Rendition::Detail => asset.detail.clone(),
            }))
    }

    async fn list(&self, tenant_id: TenantId) -> Result<Vec<MediaSummary>, MediaStoreError> {
        Ok(self.summaries(tenant_id))
    }

    async fn list_page(
        &self,
        tenant_id: TenantId,
        page: PageRequest,
    ) -> Result<Page<MediaSummary>, MediaStoreError> {
        let matching = self.summaries(tenant_id);
        let total = u32::try_from(matching.len()).unwrap_or(u32::MAX);
        let items = matching
            .into_iter()
            .skip(page.offset() as usize)
            .take(page.limit() as usize)
            .collect();
        Ok(Page::new(items, total))
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        media_id: MediaId,
    ) -> Result<bool, MediaStoreError> {
        let mut assets = self.assets.lock().expect("lock");
        let before = assets.len();
        assets.retain(|asset| !(asset.tenant_id == tenant_id && asset.media_id == media_id));
        Ok(assets.len() < before)
    }
}

impl FakeMedia {
    /// The tenant's assets as summaries, in insertion order.
    ///
    /// Shared by both reads so the paged one cannot filter differently from the unpaged one.
    fn summaries(&self, tenant_id: TenantId) -> Vec<MediaSummary> {
        self.assets
            .lock()
            .expect("lock")
            .iter()
            .filter(|asset| asset.tenant_id == tenant_id)
            .map(|asset| MediaSummary {
                media_id: asset.media_id,
                content_type: asset.content_type.clone(),
                detail_bytes: asset.detail.len(),
                created_at_ms: 0,
            })
            .collect()
    }
}

fn media_app(admin: FakeAdmin, media: FakeMedia) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::media_router(
        media,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// A small valid PNG for the upload path — a 4×4 solid image the ADR-0042 pipeline decodes and
/// re-encodes to two JPEG renditions.
fn tiny_png() -> Vec<u8> {
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        4,
        4,
        image::Rgb([200, 40, 40]),
    ));
    let mut buffer = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buffer, image::ImageFormat::Png)
        .expect("encode a test png");
    buffer.into_inner()
}

/// A raw-binary POST (an image upload) carrying a `Cookie` header.
fn post_bytes_with_cookie(
    uri: &str,
    bytes: Vec<u8>,
    content_type: &str,
    cookie: &str,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", content_type)
        .header("cookie", cookie)
        .body(Body::from(bytes))
        .expect("build the request")
}

#[tokio::test]
async fn media_uploads_serves_lists_and_deletes_and_rejects_a_non_image() {
    let router = media_app(provisioned_admin(), FakeMedia::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    // A non-image body is refused before anything is stored.
    let bad = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/media?tenant_id={tenant}"),
            b"this is not an image".to_vec(),
            "application/octet-stream",
            &cookie,
        ))
        .await
        .expect("route the bad upload");
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // A valid image is re-encoded and stored; the reply carries the new id.
    let created = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/media?tenant_id={tenant}"),
            tiny_png(),
            "image/png",
            &cookie,
        ))
        .await
        .expect("route the upload");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let media_id = created["media_id"].as_str().expect("a media id").to_owned();
    assert!(created["detail_bytes"].as_u64().expect("size") > 0);

    // The thumbnail serves as JPEG.
    let thumb = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/media/{media_id}/thumbnail?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the thumbnail");
    assert_eq!(thumb.status(), StatusCode::OK);
    assert_eq!(
        thumb
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("image/jpeg"),
    );

    // The listing shows the one asset.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/media?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the list");
    assert_eq!(listed.status(), StatusCode::OK);
    let items = json_body(listed).await;
    assert_eq!(items.as_array().expect("array").len(), 1);
    assert_eq!(items[0]["media_id"], media_id);

    // Delete removes it; a second delete is a 404.
    let deleted = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/media/{media_id}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the delete");
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let gone = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/media/{media_id}/thumbnail?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the gone thumbnail");
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_media_list_returns_a_bare_array_unpaged_and_an_envelope_when_a_limit_is_named() {
    // The two shapes of the same route (ADR-0098). The image picker sends no `limit` and must keep
    // getting a plain array of the whole library; the Media screen's table sends one and gets the
    // window plus the library's size. A route that answered one shape for both requests would
    // either break the picker's `.map` or leave the table with no count to page by.
    let router = media_app(provisioned_admin(), FakeMedia::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    for _upload in 0..3 {
        let created = router
            .clone()
            .oneshot(post_bytes_with_cookie(
                &format!("/admin/media?tenant_id={tenant}"),
                tiny_png(),
                "image/png",
                &cookie,
            ))
            .await
            .expect("route the upload");
        assert_eq!(created.status(), StatusCode::CREATED);
    }

    let unpaged = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/media?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the unpaged list");
    assert_eq!(unpaged.status(), StatusCode::OK);
    let unpaged = json_body(unpaged).await;
    assert_eq!(
        unpaged
            .as_array()
            .expect("a bare array, not an envelope")
            .len(),
        3,
    );

    let paged = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/media?tenant_id={tenant}&limit=2"),
            &cookie,
        ))
        .await
        .expect("route the paged list");
    assert_eq!(paged.status(), StatusCode::OK);
    let paged = json_body(paged).await;
    assert_eq!(paged["items"].as_array().expect("the window").len(), 2);
    assert_eq!(paged["total"], 3, "the total is the library, not the page");
    assert_eq!(paged["limit"], 2, "the bounds used are echoed back");
    assert_eq!(paged["offset"], 0);

    // A second page carries the remaining asset and the same total.
    let tail = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/media?tenant_id={tenant}&limit=2&offset=2"),
            &cookie,
        ))
        .await
        .expect("route the second page");
    let tail = json_body(tail).await;
    assert_eq!(tail["items"].as_array().expect("the window").len(), 1);
    assert_eq!(tail["total"], 3);
    assert_eq!(tail["offset"], 2);
}

#[tokio::test]
async fn the_media_list_refuses_a_page_bound_it_cannot_serve() {
    // The refusals `parse_page` builds, reached through a real route so the wiring is covered too:
    // a limit past the cap, a limit that is not a number, and an offset with no limit to skip into.
    let router = media_app(provisioned_admin(), FakeMedia::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    for (query, field) in [
        ("limit=100000", "limit"),
        ("limit=0", "limit"),
        ("limit=lots", "limit"),
        ("offset=25", "offset"),
    ] {
        let refused = router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/media?tenant_id={tenant}&{query}"),
                &cookie,
            ))
            .await
            .expect("route the bad bound");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "`{query}` is a client mistake, not a page",
        );
        let body = json_body(refused).await;
        assert_eq!(
            body["error"]["details"][0]["field"], field,
            "`{query}` names the parameter that was wrong",
        );
    }
}

#[tokio::test]
async fn media_routes_require_a_session() {
    let router = media_app(provisioned_admin(), FakeMedia::default());
    let tenant = ulid_text(1);
    let anonymous = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/media?tenant_id={tenant}"))
                .body(Body::empty())
                .expect("build"),
        )
        .await
        .expect("route without a cookie");
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tax_rates_set_lists_and_validates() {
    let catalog = FakeCatalog::default();
    let tenant = ulid_text(1);
    let class = ulid_text(7);
    seed_tax_class(&catalog, &tenant, &class, "Standard");
    let router = tax_rate_app(provisioned_admin(), catalog, FakeTaxRates::default());
    let cookie = admin_cookie(&router).await;

    // PUT the tenant's table: one class taxed 10% dine-in and 8% takeaway. `If-Match: *` is how a
    // tenant that has never saved rates says so (ADR-0095); the response carries the new version.
    let set = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 1000 },
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_TAKEAWAY", "rate_bps": 800 },
            ] }),
            &cookie,
            "*",
        ))
        .await
        .expect("route set tax rates");
    assert_eq!(set.status(), StatusCode::OK);
    let after_set = etag_of(&set);

    // GET reads them back, and hands out the same version the save returned.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/tax-rates?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list tax rates");
    assert_eq!(listed.status(), StatusCode::OK);
    let current = etag_of(&listed);
    assert_eq!(
        current, after_set,
        "the version a save returns is the one the next read hands out"
    );
    let rows = json_body(listed).await;
    assert_eq!(rows.as_array().expect("array").len(), 2);

    // A rate naming an unknown class is a 400, not a store error.
    let bad_class = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": ulid_text(99), "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 1000 },
            ] }),
            &cookie,
            &current,
        ))
        .await
        .expect("route unknown class");
    assert_eq!(bad_class.status(), StatusCode::BAD_REQUEST);

    // A rate above 100% is a 400.
    let bad_rate = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 10_001 },
            ] }),
            &cookie,
            &current,
        ))
        .await
        .expect("route bad rate");
    assert_eq!(bad_rate.status(), StatusCode::BAD_REQUEST);
}

/// A save against a version the grid no longer holds is refused, and changes nothing (ADR-0095).
#[tokio::test]
async fn a_tax_rate_save_against_a_stale_version_is_refused() {
    let catalog = FakeCatalog::default();
    let tenant = ulid_text(1);
    let class = ulid_text(7);
    seed_tax_class(&catalog, &tenant, &class, "Standard");
    let router = tax_rate_app(provisioned_admin(), catalog, FakeTaxRates::default());
    let cookie = admin_cookie(&router).await;

    let seeded = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 1000 },
            ] }),
            &cookie,
            "*",
        ))
        .await
        .expect("route the first save");
    assert_eq!(seeded.status(), StatusCode::OK);
    let current = etag_of(&seeded);

    // A save at the current version applies; replaying that version afterwards is the lost update
    // the collection version exists to refuse, and the winning grid survives it.
    let winner = serde_json::json!({ "tenant_id": tenant, "rates": [
        { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 500 },
    ] });
    let applied = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &winner,
            &cookie,
            &current,
        ))
        .await
        .expect("route the winning save");
    assert_eq!(applied.status(), StatusCode::OK);

    let replayed = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 9_900 },
            ] }),
            &cookie,
            &current,
        ))
        .await
        .expect("route the stale save");
    assert_eq!(replayed.status(), StatusCode::PRECONDITION_FAILED);

    // And `*` after the table exists is the same false claim the config tree refuses.
    let stale_wildcard = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &winner,
            &cookie,
            "*",
        ))
        .await
        .expect("route the wildcard save");
    assert_eq!(stale_wildcard.status(), StatusCode::PRECONDITION_FAILED);

    let survived = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/tax-rates?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the final read");
    let final_rows = json_body(survived).await;
    assert_eq!(
        final_rows.as_array().expect("array").len(),
        1,
        "no refused save changed the table"
    );
    assert_eq!(final_rows[0]["rate_bps"], 500);
}

#[tokio::test]
async fn tax_rate_routes_require_a_session() {
    let router = tax_rate_app(
        provisioned_admin(),
        FakeCatalog::default(),
        FakeTaxRates::default(),
    );
    let tenant = ulid_text(1);
    let listed = router
        .oneshot(get(
            &format!("/admin/catalog/tax-rates?tenant_id={tenant}"),
            None,
        ))
        .await
        .expect("route");
    assert_eq!(listed.status(), StatusCode::UNAUTHORIZED);
}

fn tax_publish_app(
    admin: FakeAdmin,
    tax_rates: FakeTaxRates,
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
    http::router(app).merge(http::config_tax_router(
        tax_rates,
        config_trees,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

#[tokio::test]
async fn tax_publish_writes_the_tax_node_onto_the_store_layer() {
    let tax_rates = FakeTaxRates::default();
    let class = TaxClassId::new(Ulid::from_u128(7));
    tax_rates.rows.lock().expect("lock").push((
        tenant(),
        TaxRateEntry {
            tax_class_id: class,
            sales_channel: SalesChannel::DineIn,
            rate: TaxRate::from_percent(10),
        },
    ));
    let config_trees = FakeConfigTrees::default();
    let router = tax_publish_app(provisioned_admin(), tax_rates, config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let published = router
        .oneshot(put_with_cookie(
            "/admin/config/tax",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            &cookie,
        ))
        .await
        .expect("route tax publish");
    assert_eq!(published.status(), StatusCode::OK);

    // The store's Store config layer (index 2) now carries the `tax` node — the serialized rate table.
    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    let tax = &state.record.layers[2]["tax"];
    assert!(tax.is_array(), "the tax node is the serialized rate table");
    assert_eq!(tax.as_array().expect("array").len(), 1);
}

/// A capturing [`OtaReportStore`] — what the `/internal/ota/report` route actually recorded.
///
/// The route had no test coverage before ADR-0078 Amendment 1, which is how a two-state field
/// survived into a three-state read model unnoticed.
#[derive(Clone, Default)]
struct FakeOtaReports {
    rows: Arc<Mutex<Vec<RecordedReport>>>,
}

/// One captured report: who reported, the version, and the tri-state verdict.
type RecordedReport = (TenantId, StoreId, String, Option<bool>);

impl OtaReportStore for FakeOtaReports {
    async fn record_report(
        &self,
        tenant: TenantId,
        store: StoreId,
        installed: &str,
        self_test_passed: Option<bool>,
        _reported_at: Timestamp,
    ) -> Result<(), FleetStoreError> {
        self.rows.lock().expect("lock").push((
            tenant,
            store,
            installed.to_owned(),
            self_test_passed,
        ));
        Ok(())
    }
}

/// The router with the OTA-report ingest merged in.
fn ota_report_app(reports: FakeOtaReports) -> axum::Router {
    let app = app(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
    );
    http::router(app).merge(http::ota_report_router(
        reports,
        clock(),
        Some(internal_secret()),
    ))
}

/// The body an edge posts, with `self_test_passed` supplied by the caller so each case can choose
/// present-and-true, present-and-false, or absent.
fn report_body(self_test: Option<bool>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
        "installed": "1.4.0",
    });
    if let Some(passed) = self_test {
        body["self_test_passed"] = serde_json::json!(passed);
    }
    body
}

#[tokio::test]
async fn an_ota_report_records_all_three_self_test_states() {
    // A store that has never installed anything omits the field. This is the case the amendment
    // exists for: the report still says which binary the store runs, and the verdict is recorded as
    // absent rather than as a pass or a failure it never earned.
    let reports = FakeOtaReports::default();
    let accepted = ota_report_app(reports.clone())
        .oneshot(post_internal("/internal/ota/report", &report_body(None)))
        .await
        .expect("route the report");
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        reports.rows.lock().expect("lock").as_slice(),
        [(tenant(), store_id(), "1.4.0".to_owned(), None)],
        "an omitted self-test is recorded as absent, not defaulted"
    );

    // An edge built before the amendment still posts the field, and its verdict is read unchanged —
    // in both directions, because a dropped `false` would hide exactly the failure the column warns
    // about.
    for passed in [true, false] {
        let reports = FakeOtaReports::default();
        let accepted = ota_report_app(reports.clone())
            .oneshot(post_internal(
                "/internal/ota/report",
                &report_body(Some(passed)),
            ))
            .await
            .expect("route the report");
        assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            reports.rows.lock().expect("lock").as_slice(),
            [(tenant(), store_id(), "1.4.0".to_owned(), Some(passed))],
            "an explicit self_test_passed={passed} is recorded as given"
        );
    }
}

#[tokio::test]
async fn a_malformed_ota_report_is_refused_and_records_nothing() {
    let reports = FakeOtaReports::default();
    let router = ota_report_app(reports.clone());

    // A store id that is not a ULID.
    let mut bad_id = report_body(Some(true));
    bad_id["store_id"] = serde_json::json!("not-a-ulid");
    let refused = router
        .clone()
        .oneshot(post_internal("/internal/ota/report", &bad_id))
        .await
        .expect("route the report");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

    // An empty installed version — the one field a report cannot be useful without.
    let mut blank = report_body(Some(true));
    blank["installed"] = serde_json::json!("   ");
    let refused = router
        .oneshot(post_internal("/internal/ota/report", &blank))
        .await
        .expect("route the report");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

    assert!(
        reports.rows.lock().expect("lock").is_empty(),
        "a refused report writes nothing to the read model"
    );
}

/// The router with the OTA levers merged in ([ADR-0052](../../docs/adr/0052-ota-rollout-config.md),
/// ADR-0078) — the rollout publish/halt/read and the placement publish/read.
fn ota_app(admin: FakeAdmin, config_trees: FakeConfigTrees) -> axum::Router {
    let app = app_full(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
    );
    http::router(app).merge(http::ota_config_router(
        config_trees,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

#[tokio::test]
async fn a_rollout_and_a_placement_land_on_their_own_config_layers() {
    let config_trees = FakeConfigTrees::default();
    let router = ota_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let published = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/ota",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "target_version": "1.4.0",
                "min_ring": "fleet",
                "rollout_percent": 40,
                "signing_key_id": "a1a1a1a1a1a1a1a1",
            }),
            &cookie,
        ))
        .await
        .expect("route the rollout publish");
    assert_eq!(published.status(), StatusCode::OK);

    let placed = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/ota/placement",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "ring": "fleet",
                "canary_bucket": 10,
            }),
            &cookie,
        ))
        .await
        .expect("route the placement publish");
    assert_eq!(placed.status(), StatusCode::OK);

    // The two nodes sit on different layers, and publishing the second does not disturb the first —
    // the rollout is a Store-level fact, the placement a Device-level one.
    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    assert_eq!(
        state.record.layers[2]["fleet_update"]["target_version"],
        "1.4.0"
    );
    assert_eq!(
        state.record.layers[2]["fleet_update"]["rollout_percent"],
        40
    );
    assert_eq!(state.record.layers[3]["device_ota"]["ring"], "fleet");
    assert_eq!(state.record.layers[3]["device_ota"]["canary_bucket"], 10);

    // And both reach the *effective* document, which is what a store actually pulls — a node authored
    // onto a layer nothing merges would be invisible to the edge that has to read it.
    let effective = &state
        .record
        .history
        .last()
        .expect("a published version")
        .effective;
    assert_eq!(effective["fleet_update"]["min_ring"], "fleet");
    assert_eq!(effective["device_ota"]["canary_bucket"], 10);

    // The read-backs answer from the layer each node was authored onto.
    let rollout = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/config/ota?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the rollout read");
    assert_eq!(rollout.status(), StatusCode::OK);
    assert_eq!(json_body(rollout).await["target_version"], "1.4.0");

    let placement = router
        .oneshot(get_with_cookie(
            &format!("/admin/config/ota/placement?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the placement read");
    assert_eq!(placement.status(), StatusCode::OK);
    assert_eq!(json_body(placement).await["canary_bucket"], 10);
}

#[tokio::test]
async fn an_unplaced_store_reads_null_and_an_illegal_placement_is_refused_with_reasons() {
    let config_trees = FakeConfigTrees::default();
    let router = ota_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // A store nobody has placed reads `null`, not a fabricated ring. The edge treats that as "no
    // placement" and installs nothing, which is the safe end of the trade (ADR-0048).
    let unplaced = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/config/ota/placement?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the placement read");
    assert_eq!(unplaced.status(), StatusCode::OK);
    assert!(json_body(unplaced).await.is_null());

    // A ring that names no ring and a bucket past 99 are refused by the shared `pos-core` rules the
    // edge also runs, with every violation at once rather than one per attempt.
    let refused = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/ota/placement",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "ring": "everyone",
                "canary_bucket": 200,
            }),
            &cookie,
        ))
        .await
        .expect("route the placement publish");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let violations = json_body(refused).await;
    assert_eq!(violations["error"]["status"], "UNPROCESSABLE");
    let listed = violations["error"]["message"]
        .as_str()
        .expect("the violations join into the message");
    assert!(
        listed.contains("device_ota.ring"),
        "the bad ring is named: {listed:?}"
    );
    assert!(
        listed.contains("device_ota.canary_bucket"),
        "the bad bucket is named: {listed:?}"
    );

    // The refusal changed nothing: the store is still unplaced, so a rejected publish cannot leave a
    // half-applied placement behind.
    assert!(
        config_trees
            .load(tenant(), store_id())
            .await
            .expect("load")
            .is_none(),
        "a refused publish persists no tree"
    );
}

#[tokio::test]
async fn the_placement_routes_require_a_session() {
    let router = ota_app(provisioned_admin(), FakeConfigTrees::default());
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let write = router
        .clone()
        .oneshot(put_json(
            "/admin/config/ota/placement",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "ring": "fleet",
                "canary_bucket": 10,
            }),
        ))
        .await
        .expect("route the placement publish");
    assert_eq!(write.status(), StatusCode::UNAUTHORIZED);

    let read = router
        .oneshot(get(
            &format!("/admin/config/ota/placement?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            None,
        ))
        .await
        .expect("route the placement read");
    assert_eq!(read.status(), StatusCode::UNAUTHORIZED);
}

fn country_app(admin: FakeAdmin) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::country_router(
        &pos_cloud::countries::registry(),
        admin,
        clock(),
    ))
}

#[tokio::test]
async fn countries_and_locales_are_read_only_master_data() {
    let router = country_app(provisioned_admin());
    let cookie = admin_cookie(&router).await;

    // The default build compiles in the reference module, so the catalogue is non-empty.
    let countries = router
        .clone()
        .oneshot(get_with_cookie("/admin/countries", &cookie))
        .await
        .expect("route countries");
    assert_eq!(countries.status(), StatusCode::OK);
    let body = json_body(countries).await;
    let list = body.as_array().expect("array");
    assert!(
        !list.is_empty(),
        "the reference country module is compiled in"
    );
    assert!(
        list.iter()
            .all(|country| country["currency_code"].is_string()),
        "each country carries a currency"
    );

    // `en` is always in the locale catalogue (the enforced fallback).
    let locales = router
        .clone()
        .oneshot(get_with_cookie("/admin/locales", &cookie))
        .await
        .expect("route locales");
    assert_eq!(locales.status(), StatusCode::OK);
    let langs = json_body(locales).await;
    assert!(
        langs
            .as_array()
            .expect("array")
            .iter()
            .any(|lang| lang == "en"),
        "en is the enforced fallback locale"
    );

    // Read-only master data still requires a session.
    let anon = router
        .oneshot(get("/admin/countries", None))
        .await
        .expect("route anon");
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
}

fn locale_publish_app(admin: FakeAdmin, config_trees: FakeConfigTrees) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::config_locale_router(
        config_trees,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

#[tokio::test]
async fn locale_publish_validates_and_writes_the_locale_node() {
    let config_trees = FakeConfigTrees::default();
    let router = locale_publish_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let ok = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/locale",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "currency_code": "VND",
                "timezone": "Asia/Ho_Chi_Minh",
                "cutoff_hour": 4,
            }),
            &cookie,
        ))
        .await
        .expect("route locale publish");
    assert_eq!(ok.status(), StatusCode::OK);
    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    assert_eq!(
        state.record.layers[2]["locale"]["timezone"],
        "Asia/Ho_Chi_Minh"
    );
    assert_eq!(state.record.layers[2]["locale"]["currency_code"], "VND");

    // A malformed IANA timezone is a 400, validated before anything is written.
    let bad = router
        .oneshot(put_with_cookie(
            "/admin/config/locale",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "currency_code": "VND",
                "timezone": "Nowhere/Nope",
                "cutoff_hour": 4,
            }),
            &cookie,
        ))
        .await
        .expect("route bad timezone");
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
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
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Margherita",
                "tax_class_id": ulid_text(7),
                // Per-locale names (ADR-0074): a real one, plus a blank row the handler drops.
                "name_translations": { "vi": "Bánh Margherita", "": "ignored", "ja": "  " },
                // An item photo (ADR-0075) — a media id round-tripped as image_ref.
                "image_ref": ulid_text(42),
            }),
            &cookie,
        ))
        .await
        .expect("route create item");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(created["name"], "Margherita");
    assert_eq!(created["status"], "active");
    assert_eq!(
        created["image_ref"],
        ulid_text(42),
        "the item photo round-trips"
    );
    assert_eq!(
        created["name_translations"]["vi"], "Bánh Margherita",
        "a real per-locale name is kept"
    );
    assert!(
        created["name_translations"].get("").is_none()
            && created["name_translations"].get("ja").is_none(),
        "a blank-key or blank-value translation row is dropped"
    );
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
    assert_eq!(items[0]["name_translations"]["vi"], "Bánh Margherita");

    // The CSV export (ADR-0075, Track M5): the item master as text/csv, no price column.
    let exported = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/export/items?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route export items");
    assert_eq!(exported.status(), StatusCode::OK);
    let csv = text_body(exported).await;
    let mut lines = csv.lines();
    assert_eq!(
        lines.next().unwrap(),
        "menu_item_id,name,status,tax_class_id,item_category_id,item_subcategory_id,image_ref"
    );
    let row = lines.next().expect("one item row");
    assert!(row.contains("Margherita") && row.contains(&item_id));
    assert!(row.contains(&ulid_text(42)), "the image ref is a column");
    assert!(
        !csv.to_lowercase().contains("price"),
        "no price is exported"
    );

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
async fn a_paged_item_read_searches_and_sorts_on_the_server() {
    // The wire half of B3-3: `?q=`, `?sort=` and `?order=` have to reach the store, not be parsed
    // and dropped. Each assertion here would pass against a handler that ignored its parameter if
    // the fixture were smaller — so the fixture is three items whose name order differs from their
    // creation order.
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    for name in ["Zucchini", "Anchovy", "Margherita"] {
        let created = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/catalog/items",
                &serde_json::json!({
                    "tenant_id": tenant,
                    "name": name,
                    "tax_class_id": ulid_text(7),
                    "name_translations": { "vi": format!("Bánh {name}") },
                }),
                &cookie,
            ))
            .await
            .expect("route create item");
        assert_eq!(created.status(), StatusCode::CREATED);
    }

    let names = |body: &serde_json::Value| -> Vec<String> {
        body["items"]
            .as_array()
            .expect("the window")
            .iter()
            .map(|row| row["name"].as_str().expect("a name").to_owned())
            .collect()
    };
    let read = |query: &str| {
        let router = router.clone();
        let cookie = cookie.clone();
        let uri = format!("/admin/catalog/items?tenant_id={tenant}&{query}");
        async move {
            let response = router
                .oneshot(get_with_cookie(&uri, &cookie))
                .await
                .expect("route the paged read");
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await
        }
    };

    // Default order is newest-first, unchanged from the unpaged read.
    let newest = read("limit=10").await;
    assert_eq!(names(&newest), ["Margherita", "Anchovy", "Zucchini"]);

    // `?sort=name` is a different order, and `?order=desc` reverses it.
    let by_name = read("limit=10&sort=name").await;
    assert_eq!(names(&by_name), ["Anchovy", "Margherita", "Zucchini"]);
    let reversed = read("limit=10&sort=name&order=desc").await;
    assert_eq!(names(&reversed), ["Zucchini", "Margherita", "Anchovy"]);

    // `?q=` narrows the rows *and* the total, so the pager does not offer empty pages.
    let searched = read("limit=10&q=anch").await;
    assert_eq!(names(&searched), ["Anchovy"]);
    assert_eq!(searched["total"], 1, "the total counts the match");

    // The search reaches the per-locale names too (ADR-0074): "Bánh" is in no primary name.
    let vietnamese = read("limit=10&q=B%C3%A1nh").await;
    assert_eq!(
        vietnamese["total"], 3,
        "every item's Vietnamese name matches"
    );

    // A search and a sort compose, and the page still carries the matching total.
    //
    // The needle is `i`, chosen after this test first asserted the wrong number: every per-locale
    // name here begins "Bánh", so a needle containing `n`, `a` or `h` matches all three through the
    // translation and proves nothing about narrowing. `i` appears in Zucchini and Margherita, in
    // neither Anchovy nor "Bánh".
    let both = read("limit=1&sort=name&q=i").await;
    assert_eq!(
        names(&both),
        ["Margherita"],
        "first by name among the matches, not the first match in the default order",
    );
    assert_eq!(both["total"], 2, "Margherita and Zucchini contain an i");
}

#[tokio::test]
async fn the_item_list_returns_the_whole_master_unpaged_and_a_page_when_a_limit_is_named() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/items",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Margherita",
                "tax_class_id": ulid_text(7),
            }),
            &cookie,
        ))
        .await
        .expect("route create item");
    assert_eq!(created.status(), StatusCode::CREATED);
    let item_id = json_body(created).await["menu_item_id"]
        .as_str()
        .expect("an item id")
        .to_owned();

    // An absent limit is the whole item master as a bare array, which the menu compiler and the
    // item pickers depend on — five of the six console consumers of this read are not tables.
    let whole = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/items?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the whole-set list");
    assert_eq!(
        json_body(whole)
            .await
            .as_array()
            .expect("a bare array, not an envelope")
            .len(),
        1,
    );

    // Naming a limit asks for a page instead (ADR-0098): the window, plus the size of the master.
    let paged = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/items?tenant_id={tenant}&limit=1"),
            &cookie,
        ))
        .await
        .expect("route the paged list");
    assert_eq!(paged.status(), StatusCode::OK);
    let paged = json_body(paged).await;
    assert_eq!(paged["items"].as_array().expect("the window").len(), 1);
    assert_eq!(paged["items"][0]["menu_item_id"], item_id);
    assert_eq!(paged["total"], 1, "one item in the master");
    assert_eq!(paged["limit"], 1, "the bounds used are echoed back");
    assert_eq!(paged["offset"], 0);
    assert!(
        paged["items"][0]["etag"].is_string(),
        "a paged row keeps the per-row etag ADR-0095 put on it",
    );

    // A page past the end is empty and not an error, and still reports the master's size.
    let beyond = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/items?tenant_id={tenant}&limit=10&offset=50"),
            &cookie,
        ))
        .await
        .expect("route the page past the end");
    let beyond = json_body(beyond).await;
    assert!(beyond["items"].as_array().expect("the window").is_empty());
    assert_eq!(beyond["total"], 1);

    for (bound, field) in [
        ("limit=100000", "limit"),
        ("limit=0", "limit"),
        ("offset=1", "offset"),
        // A page-shaping parameter without a limit: the fix is "add a limit", not "correct the
        // value", so it gets its own sentence and names the parameter it arrived with.
        ("q=marg", "q"),
        ("sort=name", "sort"),
        ("order=desc", "order"),
        // Inside the paged form, a token outside the route's closed set is refused with the list of
        // what it accepts, rather than answered with the default order.
        ("limit=5&sort=price", "sort"),
        ("limit=5&order=sideways", "order"),
    ] {
        let refused = router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/catalog/items?tenant_id={tenant}&{bound}"),
                &cookie,
            ))
            .await
            .expect("route the bad bound");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "`{bound}` is a client mistake, not a page",
        );
        assert_eq!(
            json_body(refused).await["error"]["details"][0]["field"],
            field,
            "`{bound}` names the parameter that was wrong",
        );
    }
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
    // The create hands back the version the record starts at (ADR-0094), so the edit below needs no
    // second round trip to learn it.
    let etag = created["etag"].as_str().expect("an etag").to_owned();

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
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/tax-classes/{tax_class_id}"),
            &serde_json::json!({ "tenant_id": tenant, "name": "Alcohol", "status": "archived" }),
            &cookie,
            &etag,
        ))
        .await
        .expect("route rename tax class");
    assert_eq!(renamed.status(), StatusCode::OK);
    let renamed = json_body(renamed).await;
    assert_eq!(renamed["name"], "Alcohol");
    assert_eq!(renamed["status"], "archived");

    // A PATCH to an unknown id is a 404, not a silent success — and not a 412 either: a
    // well-formed version for a row that does not exist is an absence, not a conflict (ADR-0094).
    let missing = router
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/tax-classes/{}", ulid_text(999)),
            &serde_json::json!({ "tenant_id": tenant, "name": "Nope", "status": "active" }),
            &cookie,
            &etag,
        ))
        .await
        .expect("route rename unknown tax class");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

/// The catalog's nine record-shaped entities are on the same conditional-write mechanism as the
/// registry (ADR-0094), and this is the case that matters: a second editor holding a stale copy is
/// refused, and the first editor's change is still there.
///
/// One entity stands for the nine because the seam, adapter and handler shape is identical across
/// them — what a per-entity copy of this test would prove is that the same code was pasted nine
/// times, not that nine behaviours are right.
#[tokio::test]
async fn a_stale_catalog_write_is_refused_and_the_first_edit_survives() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/tax-classes",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard 10%" }),
            &cookie,
        ))
        .await
        .expect("route create tax class");
    let created = json_body(created).await;
    let tax_class_id = created["tax_class_id"].as_str().expect("an id").to_owned();
    // Both editors load the record at the same version.
    let both_read = created["etag"].as_str().expect("an etag").to_owned();

    let first = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/tax-classes/{tax_class_id}"),
            &serde_json::json!({ "tenant_id": tenant, "name": "Alcohol", "status": "active" }),
            &cookie,
            &both_read,
        ))
        .await
        .expect("route the first save");
    assert_eq!(first.status(), StatusCode::OK);

    let second = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/tax-classes/{tax_class_id}"),
            &serde_json::json!({
                "tenant_id": tenant, "name": "Stale Overwrite", "status": "archived",
            }),
            &cookie,
            &both_read,
        ))
        .await
        .expect("route the second save");
    assert_eq!(second.status(), StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        json_body(second).await["error"]["status"],
        "VERSION_MISMATCH"
    );

    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/tax-classes?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the listing");
    let rows = json_body(listed).await;
    assert_eq!(
        rows[0]["name"], "Alcohol",
        "the refused write must not have applied: {rows}"
    );
    assert_eq!(
        rows[0]["status"], "active",
        "nor any other field it carried: {rows}"
    );
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

/// A layout button's slot is the caller-supplied `(channel, item)` pair, so a second create at the
/// same slot is refused rather than relabelling and re-positioning the button already there, and a
/// relabel applies only at the version the list carried (ADR-0095).
///
/// The button needs a display category to sit under, which the round-trip test above creates; this
/// one takes the shortcut of an id that need not exist, because the layout compiler is forgiving of
/// a stale reference and these routes never resolve it.
#[tokio::test]
async fn a_layout_button_is_placed_once_and_relabelled_at_the_version_the_list_carried() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let item = ulid_text(500);
    let category = ulid_text(700);
    let row = format!("/admin/catalog/layout-buttons/SALES_CHANNEL_DINE_IN/{item}");
    let button = |label: &str| {
        serde_json::json!({
            "sales_channel": "SALES_CHANNEL_DINE_IN",
            "menu_item_id": item,
            "tenant_id": tenant,
            "display_category_id": category,
            "label": label,
            "grid_column": 0,
            "grid_row": 0,
            "sort": 0,
        })
    };

    let placed = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/layout-buttons",
            &button("Margherita"),
            &cookie,
        ))
        .await
        .expect("route create layout button");
    assert_eq!(placed.status(), StatusCode::CREATED);
    let at_create = etag_of(&placed);

    let again = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/layout-buttons",
            &button("Margherita (classic)"),
            &cookie,
        ))
        .await
        .expect("route the duplicate create");
    assert_eq!(again.status(), StatusCode::CONFLICT);

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/layout-buttons?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list layout buttons");
    let rows = json_body(listed).await;
    assert_eq!(rows.as_array().expect("array").len(), 1);
    assert_eq!(
        rows[0]["label"], "Margherita",
        "a refused create leaves the label it refused to overwrite alone"
    );
    assert_eq!(
        rows[0]["etag"].as_str().expect("the row carries an etag"),
        at_create,
        "and the list row carries the version a relabel has to send back"
    );

    let unconditional = router
        .clone()
        .oneshot(put_with_cookie(
            &row,
            &button("Margherita (classic)"),
            &cookie,
        ))
        .await
        .expect("route the unconditional relabel");
    assert_eq!(unconditional.status(), StatusCode::BAD_REQUEST);

    let relabelled = router
        .clone()
        .oneshot(put_with_etag(
            &row,
            &button("Margherita (classic)"),
            &cookie,
            &at_create,
        ))
        .await
        .expect("route relabel layout button");
    assert_eq!(relabelled.status(), StatusCode::OK);

    let replayed = router
        .oneshot(put_with_etag(
            &row,
            &button("Margherita (again)"),
            &cookie,
            &at_create,
        ))
        .await
        .expect("route the stale relabel");
    assert_eq!(replayed.status(), StatusCode::PRECONDITION_FAILED);
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
        .oneshot(post_with_cookie(
            "/admin/catalog/layout-buttons",
            &serde_json::json!({
                "sales_channel": "SALES_CHANNEL_DINE_IN",
                "menu_item_id": item,
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
        .expect("route create layout button");
    assert_eq!(placed.status(), StatusCode::CREATED);

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
    let etag = created["etag"].as_str().expect("an etag").to_owned();
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
        .oneshot(patch_with_etag(
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
            &etag,
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

/// The `(menu, item)` pair a placement is keyed by, and the two bodies the routes take: a `POST` to
/// the collection names the pair, a `PUT` to the row takes it from the path.
fn placement_bodies(
    tenant: &str,
    menu: &str,
    item: &str,
    price: i64,
) -> (serde_json::Value, serde_json::Value) {
    let write = serde_json::json!({
        "tenant_id": tenant,
        "prices": [{ "sales_channel": "DINE_IN", "unit_price": { "currency_code": "VND", "amount_minor": price } }],
        "available": true,
    });
    let mut create = write.clone();
    create["menu_id"] = serde_json::Value::String(menu.to_owned());
    create["menu_item_id"] = serde_json::Value::String(item.to_owned());
    (create, write)
}

#[tokio::test]
async fn adding_an_item_already_on_a_menu_is_refused_rather_than_repricing_it() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let menu = ulid_text(10);
    let item = ulid_text(500);
    let base = format!("/admin/catalog/menus/{menu}/placements");

    // Adding an item to a menu is a POST to the collection, and it answers with the version the row
    // starts at.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            &base,
            &placement_bodies(&tenant, &menu, &item, 150_000).0,
            &cookie,
        ))
        .await
        .expect("route create placement");
    assert_eq!(created.status(), StatusCode::CREATED);
    let at_create = etag_of(&created);

    // A second create at the same pair is refused rather than repricing the one already there —
    // the overwrite this route used to perform silently (ADR-0095). Because the per-channel prices
    // are the price-change journal (ADR-0069), that overwrite left no `before` behind.
    let again = router
        .clone()
        .oneshot(post_with_cookie(
            &base,
            &placement_bodies(&tenant, &menu, &item, 160_000).0,
            &cookie,
        ))
        .await
        .expect("route the duplicate create");
    assert_eq!(again.status(), StatusCode::CONFLICT);

    // The list carries a version per row, which is where a reprice gets the token it must send.
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
        "a refused create adds no row"
    );
    assert_eq!(
        rows[0]["prices"][0]["unit_price"]["amount_minor"], 150_000,
        "and does not reprice the placement it refused to overwrite"
    );
    assert_eq!(
        rows[0]["etag"].as_str().expect("the row carries an etag"),
        at_create,
        "byte-identical to the token the create answered with"
    );

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
async fn a_reprice_needs_the_version_the_list_carried() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let menu = ulid_text(10);
    let item = ulid_text(500);
    let base = format!("/admin/catalog/menus/{menu}/placements");
    let row = format!("{base}/{item}");

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            &base,
            &placement_bodies(&tenant, &menu, &item, 150_000).0,
            &cookie,
        ))
        .await
        .expect("route create placement");
    assert_eq!(created.status(), StatusCode::CREATED);
    let at_create = etag_of(&created);

    // A reprice without the version is refused outright, then applies with it.
    let unconditional = router
        .clone()
        .oneshot(put_with_cookie(
            &row,
            &placement_bodies(&tenant, &menu, &item, 160_000).1,
            &cookie,
        ))
        .await
        .expect("route the unconditional reprice");
    assert_eq!(unconditional.status(), StatusCode::BAD_REQUEST);

    let repriced = router
        .clone()
        .oneshot(put_with_etag(
            &row,
            &placement_bodies(&tenant, &menu, &item, 160_000).1,
            &cookie,
            &at_create,
        ))
        .await
        .expect("route reprice placement");
    assert_eq!(repriced.status(), StatusCode::OK);

    // And replaying the version just spent is the lost update, refused.
    let replayed = router
        .clone()
        .oneshot(put_with_etag(
            &row,
            &placement_bodies(&tenant, &menu, &item, 170_000).1,
            &cookie,
            &at_create,
        ))
        .await
        .expect("route the stale reprice");
    assert_eq!(replayed.status(), StatusCode::PRECONDITION_FAILED);

    let listed = router
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
        "an update does not add a row"
    );
    assert_eq!(rows[0]["prices"][0]["unit_price"]["amount_minor"], 160_000);
    assert_ne!(
        rows[0]["etag"].as_str().expect("an etag"),
        at_create,
        "and the version moves with the write"
    );
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
    let etag = section["etag"].as_str().expect("an etag").to_owned();
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
        .oneshot(patch_with_etag(
            &format!("{sections_base}/{section_id}"),
            &serde_json::json!({
                "tenant_id": tenant, "name": "Appetizers", "sort": 2, "status": "active",
            }),
            &cookie,
            &etag,
        ))
        .await
        .expect("route rename section");
    assert_eq!(renamed.status(), StatusCode::OK);
    assert_eq!(json_body(renamed).await["name"], "Appetizers");

    // A placement can name the section it sits under, and the listing carries it back.
    let placement = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/catalog/menus/{menu}/placements"),
            &serde_json::json!({
                "menu_id": menu,
                "menu_item_id": item,
                "tenant_id": tenant,
                "menu_section_id": section_id,
                "prices": [{ "sales_channel": "DINE_IN", "unit_price": { "currency_code": "VND", "amount_minor": 150_000 } }],
                "available": true,
            }),
            &cookie,
        ))
        .await
        .expect("route create placement in section");
    assert_eq!(placement.status(), StatusCode::CREATED);

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
        .oneshot(post_with_cookie(
            &format!("/admin/catalog/menus/{menu_id}/placements"),
            &serde_json::json!({
                "menu_id": menu_id,
                "menu_item_id": item_id,
                "tenant_id": tenant,
                "prices": [{ "sales_channel": "SALES_CHANNEL_DINE_IN", "unit_price": { "currency_code": "VND", "amount_minor": 150_000 } }],
                "available": true,
            }),
            &cookie,
        ))
        .await
        .expect("route place item");
    assert_eq!(placed.status(), StatusCode::CREATED);

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
        .oneshot(post_with_cookie(
            "/admin/catalog/layout-buttons",
            &serde_json::json!({
                "sales_channel": "SALES_CHANNEL_DINE_IN",
                "menu_item_id": item,
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
        .expect("route create layout button");
    assert_eq!(placed.status(), StatusCode::CREATED);

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
    next_version: Arc<Mutex<u64>>,
}

impl FakeEmployees {
    /// The next version, as the store-postgres adapter's `xmin::text` is: a token, not a number the
    /// caller may reason about.
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }
}

#[derive(Clone)]
struct FakeEmployeeRow {
    employee_id: EmployeeId,
    tenant_id: TenantId,
    code: String,
    name: String,
    status: EntityStatus,
    pin_phc: Option<String>,
    version: Version,
}

impl FakeEmployeeRow {
    fn view(&self) -> Versioned<Employee> {
        Versioned::new(
            Employee {
                employee_id: self.employee_id,
                tenant_id: self.tenant_id,
                code: self.code.clone(),
                name: self.name.clone(),
                status: self.status,
                has_pin: self.pin_phc.is_some(),
            },
            self.version.clone(),
        )
    }
}

impl EmployeeStore for FakeEmployees {
    async fn create(&self, employee: &NewEmployee) -> Result<Version, EmployeeStoreError> {
        let version = self.mint();
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
            version: version.clone(),
        });
        Ok(version)
    }

    async fn list(&self, tenant: TenantId) -> Result<Vec<Versioned<Employee>>, EmployeeStoreError> {
        let mut rows: Vec<Versioned<Employee>> = self
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
    ) -> Result<Option<Versioned<Employee>>, EmployeeStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|row| row.tenant_id == tenant && row.employee_id == employee_id)
            .map(FakeEmployeeRow::view))
    }

    async fn update(
        &self,
        employee: &EmployeeUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, EmployeeStoreError> {
        let version = self.mint();
        let mut rows = self.rows.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|row| {
            row.tenant_id == employee.tenant_id && row.employee_id == employee.employee_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.version != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.name.clone_from(&employee.name);
        row.status = employee.status;
        row.version = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }

    async fn set_pin(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
        pin_phc: &str,
    ) -> Result<bool, EmployeeStoreError> {
        let version = self.mint();
        let mut rows = self.rows.lock().expect("lock");
        let Some(row) = rows
            .iter_mut()
            .find(|row| row.tenant_id == tenant && row.employee_id == employee_id)
        else {
            return Ok(false);
        };
        row.pin_phc = Some(pin_phc.to_owned());
        // A PIN write is a write to the row, so it moves the row's version — as the adapter's
        // `UPDATE` moves `xmin`. A fake that left the version alone here would hide a conflict the
        // real store reports (ADR-0094).
        row.version = version;
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
    assert_eq!(listed[0].record.name, "Alice");
    assert!(!listed[0].record.has_pin, "a new employee has no PIN set");

    // Rename + archive, at the version the listing handed out (ADR-0094).
    assert!(
        matches!(
            store
                .update(
                    &EmployeeUpdate {
                        employee_id: alice,
                        tenant_id: mine,
                        name: "Alice Nguyen".to_owned(),
                        status: EntityStatus::Archived,
                    },
                    &listed[0].etag,
                )
                .await
                .expect("update"),
            UpdateOutcome::Updated(_)
        ),
        "the row was found at the version the reader held, and changed"
    );
    let alice_view = store.get(mine, alice).await.expect("get").expect("present");
    assert_eq!(alice_view.record.name, "Alice Nguyen");
    assert_eq!(alice_view.record.status, EntityStatus::Archived);

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
            .record
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

// --- Operational alerts (`/admin/alerts`, ADR-0073, Track O2) -----------------------------------

/// An in-memory [`AlertStore`] for the alerts-route tests: the same open→resolved lifecycle the store
/// contract test covers, behind an `Arc<Mutex<…>>` so the router can clone it.
#[derive(Clone, Default)]
struct FakeAlerts {
    rows: Arc<Mutex<Vec<AlertRecord>>>,
}

impl FakeAlerts {
    fn seed(&self, rows: Vec<AlertRecord>) {
        *self.rows.lock().expect("lock") = rows;
    }
}

impl AlertStore for FakeAlerts {
    async fn upsert(&self, record: &AlertRecord) -> Result<(), AlertStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        if let Some(open) = rows
            .iter_mut()
            .find(|r| r.resolved_at.is_none() && r.key() == record.key())
        {
            open.severity = record.severity;
            open.summary.clone_from(&record.summary);
            open.detail = record.detail.clone();
            open.last_seen_at = record.last_seen_at;
        } else {
            rows.push(record.clone());
        }
        Ok(())
    }

    async fn resolve(&self, id: &str, resolved_at: Timestamp) -> Result<(), AlertStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        if let Some(row) = rows
            .iter_mut()
            .find(|r| r.id == id && r.resolved_at.is_none())
        {
            row.resolved_at = Some(resolved_at);
        }
        Ok(())
    }

    async fn acknowledge(
        &self,
        id: &str,
        acknowledged_at: Timestamp,
    ) -> Result<(), AlertStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        if let Some(row) = rows.iter_mut().find(|r| r.id == id) {
            row.acknowledged_at = Some(acknowledged_at);
        }
        Ok(())
    }

    async fn list_active(&self) -> Result<Vec<AlertRecord>, AlertStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|r| r.resolved_at.is_none())
            .cloned()
            .collect())
    }

    async fn list_recent(&self, limit: u32) -> Result<Vec<AlertRecord>, AlertStoreError> {
        let mut rows = self.rows.lock().expect("lock").clone();
        rows.truncate(limit as usize);
        Ok(rows)
    }
}

fn alert_at(
    id: &str,
    kind: AlertKind,
    tenant_id: Option<TenantId>,
    dedup: &str,
    resolved: bool,
) -> AlertRecord {
    AlertRecord {
        id: id.to_owned(),
        tenant_id,
        kind,
        dedup_key: dedup.to_owned(),
        severity: kind.default_severity(),
        summary: "a condition".to_owned(),
        detail: serde_json::json!({}),
        first_seen_at: seen_ago(0),
        last_seen_at: seen_ago(0),
        resolved_at: resolved.then(|| seen_ago(0)),
        acknowledged_at: None,
    }
}

/// The main router (for `/admin/login`) merged with the alerts sub-router, one shared admin store.
fn alerts_app(admin: FakeAdmin, alerts: FakeAlerts) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::alerts_router(
        alerts,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

#[tokio::test]
async fn alerts_list_active_and_recent_then_acknowledge_and_resolve() {
    let alerts = FakeAlerts::default();
    alerts.seed(vec![
        alert_at("alert-jet", AlertKind::JetstreamCapacity, None, "", false),
        alert_at(
            "alert-store",
            AlertKind::StoreOffline,
            Some(tenant()),
            "store-x",
            false,
        ),
        alert_at(
            "alert-old",
            AlertKind::RelayBacklog,
            Some(tenant()),
            "store-y",
            true,
        ),
    ]);
    let router = alerts_app(provisioned_admin(), alerts);
    let cookie = admin_cookie(&router).await;

    // The active list is the two unresolved alerts, and a server-wide one carries a null tenant.
    let active = router
        .clone()
        .oneshot(get_with_cookie("/admin/alerts", &cookie))
        .await
        .expect("route the active list");
    assert_eq!(active.status(), StatusCode::OK);
    let rows = json_body(active).await;
    let array = rows.as_array().expect("an array of alerts");
    assert_eq!(array.len(), 2, "only the unresolved alerts");
    assert!(
        array
            .iter()
            .any(|a| a["kind"] == "jetstream_capacity" && a["tenant_id"].is_null()),
        "the server-wide alert carries a null tenant"
    );

    // Recent history includes the resolved one.
    let recent = router
        .clone()
        .oneshot(get_with_cookie("/admin/alerts?recent=true", &cookie))
        .await
        .expect("route the recent list");
    let rows = json_body(recent).await;
    assert_eq!(rows.as_array().expect("an array").len(), 3);

    // Acknowledge and resolve are idempotent 204s.
    let ack = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/alerts/alert-store/ack",
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route the ack");
    assert_eq!(ack.status(), StatusCode::NO_CONTENT);
    let resolved = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/alerts/alert-store/resolve",
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route the resolve");
    assert_eq!(resolved.status(), StatusCode::NO_CONTENT);

    // After the resolve, the active list drops to the server-wide alert alone.
    let active = router
        .oneshot(get_with_cookie("/admin/alerts", &cookie))
        .await
        .expect("route the active list again");
    let rows = json_body(active).await;
    assert_eq!(rows.as_array().expect("an array").len(), 1);
}

#[tokio::test]
async fn alerts_require_a_session() {
    let router = alerts_app(provisioned_admin(), FakeAlerts::default());
    let denied = router
        .oneshot(get("/admin/alerts", None))
        .await
        .expect("route without a cookie");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}

/// The floor master-data store as two in-memory lists — the same `AreaStore`/`TableStore` seams the
/// binary implements over the tenant-and-store-scoped `floor_areas`/`floor_tables` tables. Areas and
/// tables are archived, never removed (Track M2, ADR-0072).
#[derive(Clone, Default)]
struct FakeFloor {
    areas: Arc<Mutex<Vec<Versioned<Area>>>>,
    tables: Arc<Mutex<Vec<Versioned<Table>>>>,
    stations: Arc<Mutex<Vec<Versioned<Station>>>>,
    rules: Arc<Mutex<Vec<RoutingRule>>>,
    next_version: Arc<Mutex<u64>>,
}

impl FakeFloor {
    /// The next version, as the store-postgres adapter's `xmin::text` is: a token, not a number the
    /// caller may reason about. One counter across all three entities — a version is compared only
    /// against the row it came from, so sharing the sequence proves nothing depends on it.
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }
}

impl AreaStore for FakeFloor {
    async fn create(&self, area: &NewArea) -> Result<Version, FloorStoreError> {
        let version = self.mint();
        self.areas.lock().expect("lock").push(Versioned::new(
            Area {
                area_id: area.area_id,
                tenant_id: area.tenant_id,
                store_id: area.store_id,
                name: area.name.clone(),
                status: EntityStatus::Active,
            },
            version.clone(),
        ));
        Ok(version)
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<Area>>, FloorStoreError> {
        let mut rows: Vec<Versioned<Area>> = self
            .areas
            .lock()
            .expect("lock")
            .iter()
            .filter(|area| area.record.tenant_id == tenant && area.record.store_id == store_id)
            .cloned()
            .collect();
        rows.reverse(); // stored oldest-first; the read is newest-first.
        Ok(rows)
    }

    async fn get(
        &self,
        tenant: TenantId,
        area_id: AreaId,
    ) -> Result<Option<Versioned<Area>>, FloorStoreError> {
        Ok(self
            .areas
            .lock()
            .expect("lock")
            .iter()
            .find(|area| area.record.tenant_id == tenant && area.record.area_id == area_id)
            .cloned())
    }

    async fn update(
        &self,
        update: &AreaUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, FloorStoreError> {
        let version = self.mint();
        let mut rows = self.areas.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|area| {
            area.record.tenant_id == update.tenant_id && area.record.area_id == update.area_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record.name.clone_from(&update.name);
        row.record.status = update.status;
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }
}

impl TableStore for FakeFloor {
    async fn create(&self, table: &NewTable) -> Result<Version, FloorStoreError> {
        let version = self.mint();
        self.tables.lock().expect("lock").push(Versioned::new(
            Table {
                table_id: table.table_id,
                tenant_id: table.tenant_id,
                store_id: table.store_id,
                area_id: table.area_id,
                label: table.label.clone(),
                seats: table.seats,
                position: table.position,
                status: EntityStatus::Active,
            },
            version.clone(),
        ));
        Ok(version)
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<Table>>, FloorStoreError> {
        let mut rows: Vec<Versioned<Table>> = self
            .tables
            .lock()
            .expect("lock")
            .iter()
            .filter(|table| table.record.tenant_id == tenant && table.record.store_id == store_id)
            .cloned()
            .collect();
        rows.reverse();
        Ok(rows)
    }

    async fn get(
        &self,
        tenant: TenantId,
        table_id: TableId,
    ) -> Result<Option<Versioned<Table>>, FloorStoreError> {
        Ok(self
            .tables
            .lock()
            .expect("lock")
            .iter()
            .find(|table| table.record.tenant_id == tenant && table.record.table_id == table_id)
            .cloned())
    }

    async fn update(
        &self,
        update: &TableUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, FloorStoreError> {
        let version = self.mint();
        let mut rows = self.tables.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|table| {
            table.record.tenant_id == update.tenant_id && table.record.table_id == update.table_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record.area_id = update.area_id;
        row.record.label.clone_from(&update.label);
        row.record.seats = update.seats;
        row.record.position = update.position;
        row.record.status = update.status;
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end exercise of both floor seams (area + table CRUD, placement, and \
              tenant/store isolation) reads better as a single narrative than split fixtures"
)]
async fn floor_store_creates_lists_updates_scoped_by_tenant_and_store() {
    let store = FakeFloor::default();
    let mine = tenant();
    let other = TenantId::new(Ulid::from_u128(0xB0B));
    let front = StoreId::new(Ulid::from_u128(0x5701));
    let back = StoreId::new(Ulid::from_u128(0x5702));
    let terrace = AreaId::new(Ulid::from_u128(1));
    let hall = AreaId::new(Ulid::from_u128(2));

    AreaStore::create(
        &store,
        &NewArea {
            area_id: terrace,
            tenant_id: mine,
            store_id: front,
            name: "Terrace".to_owned(),
        },
    )
    .await
    .expect("create terrace");
    // A same-id area in another tenant/store must not leak into `mine`'s front-store list.
    AreaStore::create(
        &store,
        &NewArea {
            area_id: hall,
            tenant_id: other,
            store_id: back,
            name: "Hall".to_owned(),
        },
    )
    .await
    .expect("create hall");

    let areas = AreaStore::list(&store, mine, front)
        .await
        .expect("list areas");
    assert_eq!(areas.len(), 1);
    assert_eq!(areas[0].record.name, "Terrace");

    // Rename + archive the area, at the version the listing handed out (ADR-0094).
    assert!(matches!(
        AreaStore::update(
            &store,
            &AreaUpdate {
                area_id: terrace,
                tenant_id: mine,
                name: "Front terrace".to_owned(),
                status: EntityStatus::Archived,
            },
            &areas[0].etag,
        )
        .await
        .expect("update area"),
        UpdateOutcome::Updated(_)
    ));
    let terrace_view = AreaStore::get(&store, mine, terrace)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(terrace_view.record.name, "Front terrace");
    assert_eq!(terrace_view.record.status, EntityStatus::Archived);

    // A table carries an optional grid position; create one placed, then move + reseat it.
    let table_one = TableId::new(Ulid::from_u128(10));
    TableStore::create(
        &store,
        &NewTable {
            table_id: table_one,
            tenant_id: mine,
            store_id: front,
            area_id: terrace,
            label: "T1".to_owned(),
            seats: 4,
            position: Some(GridPosition { column: 0, row: 0 }),
        },
    )
    .await
    .expect("create table");

    let tables = TableStore::list(&store, mine, front)
        .await
        .expect("list tables");
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].record.seats, 4);
    assert_eq!(
        tables[0].record.position,
        Some(GridPosition { column: 0, row: 0 })
    );

    assert!(matches!(
        TableStore::update(
            &store,
            &TableUpdate {
                table_id: table_one,
                tenant_id: mine,
                area_id: terrace,
                label: "T1".to_owned(),
                seats: 6,
                position: None,
                status: EntityStatus::Active,
            },
            &tables[0].etag,
        )
        .await
        .expect("update table"),
        UpdateOutcome::Updated(_)
    ));
    let table_view = TableStore::get(&store, mine, table_one)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(table_view.record.seats, 6);
    assert_eq!(table_view.record.position, None, "the table was unplaced");

    // Cross-tenant/store isolation: mine's back store and the other tenant see none of mine's front.
    assert!(
        AreaStore::list(&store, mine, back)
            .await
            .expect("list")
            .is_empty()
    );
    assert!(
        AreaStore::get(&store, other, terrace)
            .await
            .expect("get")
            .is_none()
    );
}

impl StationStore for FakeFloor {
    async fn create(&self, station: &NewStation) -> Result<Version, FloorStoreError> {
        let version = self.mint();
        self.stations.lock().expect("lock").push(Versioned::new(
            Station {
                station_id: station.station_id,
                tenant_id: station.tenant_id,
                store_id: station.store_id,
                name: station.name.clone(),
                backup_station_id: station.backup_station_id,
                is_default: station.is_default,
                status: EntityStatus::Active,
            },
            version.clone(),
        ));
        Ok(version)
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<Station>>, FloorStoreError> {
        let mut rows: Vec<Versioned<Station>> = self
            .stations
            .lock()
            .expect("lock")
            .iter()
            .filter(|station| {
                station.record.tenant_id == tenant && station.record.store_id == store_id
            })
            .cloned()
            .collect();
        rows.reverse();
        Ok(rows)
    }

    async fn get(
        &self,
        tenant: TenantId,
        station_id: StationId,
    ) -> Result<Option<Versioned<Station>>, FloorStoreError> {
        Ok(self
            .stations
            .lock()
            .expect("lock")
            .iter()
            .find(|station| {
                station.record.tenant_id == tenant && station.record.station_id == station_id
            })
            .cloned())
    }

    async fn update(
        &self,
        update: &StationUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, FloorStoreError> {
        let version = self.mint();
        let mut rows = self.stations.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|station| {
            station.record.tenant_id == update.tenant_id
                && station.record.station_id == update.station_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record.name.clone_from(&update.name);
        row.record.backup_station_id = update.backup_station_id;
        row.record.is_default = update.is_default;
        row.record.status = update.status;
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }
}

impl RoutingRuleStore for FakeFloor {
    async fn create(&self, rule: &NewRoutingRule) -> Result<(), FloorStoreError> {
        self.rules.lock().expect("lock").push(RoutingRule {
            rule_id: rule.rule_id,
            tenant_id: rule.tenant_id,
            store_id: rule.store_id,
            station_id: rule.station_id,
            menu_item_id: rule.menu_item_id,
            course_id: rule.course_id,
            sort: rule.sort,
        });
        Ok(())
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<RoutingRule>, FloorStoreError> {
        let mut rows: Vec<RoutingRule> = self
            .rules
            .lock()
            .expect("lock")
            .iter()
            .filter(|rule| rule.tenant_id == tenant && rule.store_id == store_id)
            .cloned()
            .collect();
        rows.sort_by_key(|rule| rule.sort); // the seam reads rules in `sort` order.
        Ok(rows)
    }

    async fn remove(
        &self,
        tenant: TenantId,
        rule_id: RoutingRuleId,
    ) -> Result<bool, FloorStoreError> {
        let mut rows = self.rules.lock().expect("lock");
        let before = rows.len();
        rows.retain(|rule| !(rule.tenant_id == tenant && rule.rule_id == rule_id));
        Ok(rows.len() != before)
    }
}

#[tokio::test]
async fn kitchen_store_creates_lists_stations_and_removes_routing_rules() {
    let store = FakeFloor::default();
    let mine = tenant();
    let front = StoreId::new(Ulid::from_u128(0x5701));
    let oven = StationId::new(Ulid::from_u128(1));
    let bar = StationId::new(Ulid::from_u128(2));

    StationStore::create(
        &store,
        &NewStation {
            station_id: oven,
            tenant_id: mine,
            store_id: front,
            name: "Oven".to_owned(),
            backup_station_id: Some(bar),
            is_default: true,
        },
    )
    .await
    .expect("create oven");
    let stations = StationStore::list(&store, mine, front).await.expect("list");
    assert_eq!(stations.len(), 1);

    // Update: drop the backup, keep default off now — at the version the listing handed out.
    assert!(matches!(
        StationStore::update(
            &store,
            &StationUpdate {
                station_id: oven,
                tenant_id: mine,
                name: "Pizza oven".to_owned(),
                backup_station_id: None,
                is_default: false,
                status: EntityStatus::Active,
            },
            &stations[0].etag,
        )
        .await
        .expect("update"),
        UpdateOutcome::Updated(_)
    ));
    let oven_view = StationStore::get(&store, mine, oven)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(oven_view.record.name, "Pizza oven");
    assert_eq!(oven_view.record.backup_station_id, None);
    assert!(!oven_view.record.is_default);

    // Two routing rules — one by item, one by course — then remove one (returns false the 2nd time).
    let item_rule = RoutingRuleId::new(Ulid::from_u128(50));
    let course_rule = RoutingRuleId::new(Ulid::from_u128(51));
    RoutingRuleStore::create(
        &store,
        &NewRoutingRule {
            rule_id: item_rule,
            tenant_id: mine,
            store_id: front,
            station_id: oven,
            menu_item_id: Some(MenuItemId::new(Ulid::from_u128(100))),
            course_id: None,
            sort: 0,
        },
    )
    .await
    .expect("create item rule");
    RoutingRuleStore::create(
        &store,
        &NewRoutingRule {
            rule_id: course_rule,
            tenant_id: mine,
            store_id: front,
            station_id: bar,
            menu_item_id: None,
            course_id: Some(CourseId::new(Ulid::from_u128(200))),
            sort: 1,
        },
    )
    .await
    .expect("create course rule");
    assert_eq!(
        RoutingRuleStore::list(&store, mine, front)
            .await
            .expect("list rules")
            .len(),
        2
    );
    assert!(
        RoutingRuleStore::remove(&store, mine, item_rule)
            .await
            .expect("remove")
    );
    assert!(
        !RoutingRuleStore::remove(&store, mine, item_rule)
            .await
            .expect("remove again"),
        "a rule already removed reports no change"
    );
    assert_eq!(
        RoutingRuleStore::list(&store, mine, front)
            .await
            .expect("list rules")
            .len(),
        1,
        "the course rule remains"
    );
}

/// The role-template store as an in-memory list — the same seam the binary implements over a
/// tenant-scoped table. Roles are archived, never removed.
#[derive(Clone, Default)]
struct FakeRoleTemplates {
    rows: Arc<Mutex<Vec<Versioned<RoleTemplate>>>>,
    next_version: Arc<Mutex<u64>>,
}

impl FakeRoleTemplates {
    /// The next version, as the store-postgres adapter's `xmin::text` is: a token, not a number the
    /// caller may reason about.
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }
}

impl RoleTemplateStore for FakeRoleTemplates {
    async fn create(&self, template: &NewRoleTemplate) -> Result<Version, RoleTemplateStoreError> {
        let version = self.mint();
        let mut rows = self.rows.lock().expect("lock");
        if rows.iter().any(|row| {
            row.record.tenant_id == template.tenant_id && row.record.name == template.name
        }) {
            return Err(RoleTemplateStoreError::new(
                "duplicate role name within the tenant",
            ));
        }
        rows.push(Versioned::new(
            RoleTemplate {
                role_template_id: template.role_template_id,
                tenant_id: template.tenant_id,
                name: template.name.clone(),
                permissions: template.permissions.clone(),
                status: EntityStatus::Active,
            },
            version.clone(),
        ));
        Ok(version)
    }

    async fn list(
        &self,
        tenant: TenantId,
    ) -> Result<Vec<Versioned<RoleTemplate>>, RoleTemplateStoreError> {
        let mut rows: Vec<Versioned<RoleTemplate>> = self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.record.tenant_id == tenant)
            .cloned()
            .collect();
        rows.reverse();
        Ok(rows)
    }

    async fn get(
        &self,
        tenant: TenantId,
        role_template_id: RoleTemplateId,
    ) -> Result<Option<Versioned<RoleTemplate>>, RoleTemplateStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|row| {
                row.record.tenant_id == tenant && row.record.role_template_id == role_template_id
            })
            .cloned())
    }

    async fn update(
        &self,
        template: &RoleTemplateUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, RoleTemplateStoreError> {
        let version = self.mint();
        let mut rows = self.rows.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|row| {
            row.record.tenant_id == template.tenant_id
                && row.record.role_template_id == template.role_template_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record.name.clone_from(&template.name);
        row.record.permissions.clone_from(&template.permissions);
        row.record.status = template.status;
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
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
    assert_eq!(listed[0].record.permissions.len(), 2);

    // Edit the permission set and archive, at the version the listing handed out (ADR-0094).
    assert!(matches!(
        store
            .update(
                &RoleTemplateUpdate {
                    role_template_id: cashier,
                    tenant_id: mine,
                    name: "Cashier".to_owned(),
                    permissions: vec!["sales.item.open".to_owned()],
                    status: EntityStatus::Archived,
                },
                &listed[0].etag,
            )
            .await
            .expect("update"),
        UpdateOutcome::Updated(_)
    ));
    let view = store
        .get(mine, cashier)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(view.record.permissions, vec!["sales.item.open".to_owned()]);
    assert_eq!(view.record.status, EntityStatus::Archived);
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
    async fn create(&self, employee: &NewEmployee) -> Result<Version, EmployeeStoreError> {
        self.employees.create(employee).await
    }
    async fn list(&self, tenant: TenantId) -> Result<Vec<Versioned<Employee>>, EmployeeStoreError> {
        self.employees.list(tenant).await
    }
    async fn get(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Option<Versioned<Employee>>, EmployeeStoreError> {
        self.employees.get(tenant, employee_id).await
    }
    async fn update(
        &self,
        employee: &EmployeeUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, EmployeeStoreError> {
        self.employees.update(employee, expected).await
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
    async fn create(&self, template: &NewRoleTemplate) -> Result<Version, RoleTemplateStoreError> {
        self.roles.create(template).await
    }
    async fn list(
        &self,
        tenant: TenantId,
    ) -> Result<Vec<Versioned<RoleTemplate>>, RoleTemplateStoreError> {
        self.roles.list(tenant).await
    }
    async fn get(
        &self,
        tenant: TenantId,
        role_template_id: RoleTemplateId,
    ) -> Result<Option<Versioned<RoleTemplate>>, RoleTemplateStoreError> {
        self.roles.get(tenant, role_template_id).await
    }
    async fn update(
        &self,
        template: &RoleTemplateUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, RoleTemplateStoreError> {
        self.roles.update(template, expected).await
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

/// The main app merged with the floor & kitchen router, wired to a caller-supplied audit recorder
/// (Track M2, ADR-0072).
fn floor_app_with_audit(
    admin: FakeAdmin,
    floor: FakeFloor,
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
    http::router(app).merge(http::floor_router(floor, admin, clock(), audit))
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end lifecycle over the floor & kitchen routes — create an area, a table, a \
              station, and a routing rule; reject a rule matching neither item nor course; list each; \
              archive the area; remove the rule — kept together against the same data"
)]
async fn floor_routes_crud_lifecycle_audited() {
    let admin = provisioned_admin();
    let audit = FakeAudit::default();
    let router = floor_app_with_audit(
        admin,
        FakeFloor::default(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Create an area.
    let area = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/areas",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid, "name": "Terrace" }),
            &cookie,
        ))
        .await
        .expect("route create area");
    assert_eq!(area.status(), StatusCode::CREATED);
    let area_etag = etag_of(&area);
    let area_id = json_body(area).await["id"].as_str().expect("id").to_owned();

    // Create a table in that area, placed on the grid.
    let table = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/tables",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "area_id": area_id,
                "name": "T1",
                "seats": 4,
                "grid_column": 0,
                "grid_row": 0,
            }),
            &cookie,
        ))
        .await
        .expect("route create table");
    assert_eq!(table.status(), StatusCode::CREATED);

    // Create a station.
    let station = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/stations",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "name": "Oven",
                "is_default": true,
            }),
            &cookie,
        ))
        .await
        .expect("route create station");
    assert_eq!(station.status(), StatusCode::CREATED);
    let station_id = json_body(station).await["id"]
        .as_str()
        .expect("id")
        .to_owned();

    // A routing rule that matches neither an item nor a course is refused.
    let bad_rule = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/routing",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "station_id": station_id,
            }),
            &cookie,
        ))
        .await
        .expect("route bad rule");
    assert_eq!(bad_rule.status(), StatusCode::BAD_REQUEST);

    // A well-formed item rule is accepted.
    let item_ulid = Ulid::from_u128(0xB0B).to_string();
    let rule = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/routing",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "station_id": station_id,
                "menu_item_id": item_ulid,
            }),
            &cookie,
        ))
        .await
        .expect("route create rule");
    assert_eq!(rule.status(), StatusCode::CREATED);
    let rule_id = json_body(rule).await["id"].as_str().expect("id").to_owned();

    // Each list reads back one row.
    for path in [
        format!("/admin/floor/areas?tenant_id={tenant_ulid}&store_id={store_ulid}"),
        format!("/admin/floor/tables?tenant_id={tenant_ulid}&store_id={store_ulid}"),
        format!("/admin/kitchen/stations?tenant_id={tenant_ulid}&store_id={store_ulid}"),
        format!("/admin/kitchen/routing?tenant_id={tenant_ulid}&store_id={store_ulid}"),
    ] {
        let listed = router
            .clone()
            .oneshot(get_with_cookie(&path, &cookie))
            .await
            .expect("route list");
        assert_eq!(listed.status(), StatusCode::OK, "{path}");
        assert_eq!(
            json_body(listed).await.as_array().expect("array").len(),
            1,
            "{path}"
        );
    }

    // Archive the area, at the version its creation returned (ADR-0094).
    let archived = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/floor/areas/{area_id}"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Terrace", "status": "archived" }),
            &cookie,
            &area_etag,
        ))
        .await
        .expect("route archive area");
    assert_eq!(archived.status(), StatusCode::NO_CONTENT);
    assert_ne!(
        etag_of(&archived),
        area_etag,
        "a write moves the record to a new version, and says which"
    );

    // A second console still holding the version from before that archive is refused, and the first
    // edit stands. Areas, tables and stations share one guard, so proving it once here proves it for
    // the family; the per-entity wiring is what the lifecycle above exercises.
    let stale = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/floor/areas/{area_id}"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Patio", "status": "active" }),
            &cookie,
            &area_etag,
        ))
        .await
        .expect("route stale area update");
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);
    let after = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/floor/areas/{area_id}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route read area back");
    assert_eq!(
        json_body(after).await["name"],
        serde_json::json!("Terrace"),
        "the refused write changed nothing"
    );

    // Remove the routing rule.
    let removed = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/kitchen/routing/{rule_id}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route remove rule");
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);

    // The write path is audited: the actions were recorded (floor data is not PII).
    let recorded = audit.list(None, 20).await.expect("list audit entries");
    let actions: Vec<&str> = recorded.iter().map(|entry| entry.action.as_str()).collect();
    for action in [
        "floor.area.create",
        "floor.table.create",
        "kitchen.station.create",
        "kitchen.routing.create",
        "floor.area.update",
        "kitchen.routing.remove",
    ] {
        assert!(actions.contains(&action), "missing audit {action}");
    }
}

/// The main app (sharing one config-tree store) merged with the floor CRUD and floor-publish routers,
/// so a test can author master data and then publish it, reading the effective config back (ADR-0072).
fn floor_publish_app(
    admin: FakeAdmin,
    floor: FakeFloor,
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
    http::router(app)
        .merge(http::floor_router(
            floor.clone(),
            admin.clone(),
            clock(),
            Arc::clone(&audit),
        ))
        .merge(http::floor_publish_router(
            floor,
            config,
            admin,
            clock(),
            audit,
        ))
}

#[tokio::test]
async fn floor_publish_compiles_and_writes_the_floor_and_stations_nodes() {
    let admin = provisioned_admin();
    let config = FakeConfigTrees::default();
    let router = floor_publish_app(
        admin,
        FakeFloor::default(),
        config,
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Author one area with a table and one station via the CRUD routes.
    let area = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/areas",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid, "name": "Terrace" }),
            &cookie,
        ))
        .await
        .expect("create area");
    let area_id = json_body(area).await["id"].as_str().expect("id").to_owned();
    let table = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/tables",
            &serde_json::json!({
                "tenant_id": tenant_ulid, "store_id": store_ulid, "area_id": area_id,
                "name": "T1", "seats": 4,
            }),
            &cookie,
        ))
        .await
        .expect("create table");
    assert_eq!(table.status(), StatusCode::CREATED);
    let station = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/stations",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid, "name": "Oven", "is_default": true }),
            &cookie,
        ))
        .await
        .expect("create station");
    assert_eq!(station.status(), StatusCode::CREATED);

    // Publish: the two nodes compile and version through the config tree.
    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/publish",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            &cookie,
        ))
        .await
        .expect("publish floor");
    assert_eq!(published.status(), StatusCode::OK);

    // The effective config now carries top-level `floor` and `stations` nodes with the authored data.
    let effective = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store_ulid}/config?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("read effective config");
    assert_eq!(effective.status(), StatusCode::OK);
    let doc = json_body(effective).await;
    assert_eq!(
        doc["floor"]["areas"][0]["name"],
        serde_json::json!("Terrace")
    );
    assert_eq!(
        doc["floor"]["areas"][0]["tables"][0]["label"],
        serde_json::json!("T1")
    );
    assert_eq!(
        doc["stations"]["stations"][0]["name"],
        serde_json::json!("Oven")
    );
    assert!(
        doc["stations"]["default_station_id"].as_str().is_some(),
        "the default station rode into the node"
    );
}

#[tokio::test]
async fn table_qr_mints_a_signed_token_per_active_table() {
    let admin = provisioned_admin();
    let floor = FakeFloor::default();
    let mine = tenant();
    let store = store_id();
    let area = AreaId::new(Ulid::from_u128(1));
    let active = TableId::new(Ulid::from_u128(0xA1));
    let archived = TableId::new(Ulid::from_u128(0xA2));
    let mut archived_version = None;
    for (table_id, label) in [(active, "T1"), (archived, "T2")] {
        let version = TableStore::create(
            &floor,
            &NewTable {
                table_id,
                tenant_id: mine,
                store_id: store,
                area_id: area,
                label: label.to_owned(),
                seats: 4,
                position: None,
            },
        )
        .await
        .expect("seed table");
        if table_id == archived {
            archived_version = Some(version);
        }
    }
    // Archive T2 — an archived table is not printed on the QR sheet. The write is conditional on the
    // version the create returned (ADR-0094), the same token a reader would have been handed.
    TableStore::update(
        &floor,
        &TableUpdate {
            table_id: archived,
            tenant_id: mine,
            area_id: area,
            label: "T2".to_owned(),
            seats: 4,
            position: None,
            status: EntityStatus::Archived,
        },
        &archived_version.expect("T2 was seeded"),
    )
    .await
    .expect("archive T2");

    let secret = TableTokenSecret::new("a-test-secret");
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    ))
    .merge(http::table_qr_router(floor, admin, clock(), secret.clone()));
    let cookie = admin_cookie(&router).await;

    let response = router
        .oneshot(get_with_cookie(
            &format!(
                "/admin/floor/qr?tenant_id={}&store_id={}",
                mine.as_ulid(),
                store.as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route table qr");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let tokens = body["tokens"].as_array().expect("tokens array");
    assert_eq!(tokens.len(), 1, "only the active table is printed");
    let token = tokens[0]["token"].as_str().expect("a token");
    // The minted token verifies and names the active table — the same value the guest QR carries.
    let table_ref = pos_cloud::qr::verify_table_token(&secret, token).expect("verifies");
    assert_eq!(table_ref.table_id, active);
    assert_eq!(table_ref.store_id, store);
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
    let employee_etag = etag_of(&one);
    let one_body = json_body(one).await;
    assert_eq!(one_body["has_pin"], serde_json::json!(true));
    assert_eq!(
        one_body["etag"], employee_etag,
        "the header and the body name the same version"
    );

    // Rename + archive, at the version the read-one handed out (ADR-0094).
    let updated = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/employees/{employee_id}"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Alice Nguyen", "status": "archived" }),
            &cookie,
            &employee_etag,
        ))
        .await
        .expect("route update");
    assert_eq!(updated.status(), StatusCode::NO_CONTENT);

    // A second writer still holding the version from before that rename is refused, and the first
    // edit survives.
    let stale = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/employees/{employee_id}"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Someone Else", "status": "active" }),
            &cookie,
            &employee_etag,
        ))
        .await
        .expect("route stale update");
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);

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
    let refusal = json_body(bad_role).await;
    assert_eq!(
        refusal["error"]["details"][0]["field"], "permissions",
        "the field named is the one the caller sent, not a paraphrase: {refusal}"
    );
    assert_eq!(
        refusal["error"]["details"][0]["reason"],
        "INVALID_ENUM_VALUE"
    );

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
    let node = &state.record.layers[2]["permissions"];
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

/// The main app merged with the capability-catalogue router.
fn capabilities_app(admin: FakeAdmin) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::capabilities_router(admin, clock()))
}

#[tokio::test]
async fn capability_catalogue_serves_flags_presets_and_rules_to_any_admin() {
    let admin = provisioned_admin();
    let router = capabilities_app(admin.clone());

    // Read is open to every console role — a Viewer may load the catalogue.
    let viewer = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let response = router
        .clone()
        .oneshot(get_with_cookie("/admin/capabilities", &viewer))
        .await
        .expect("route the catalogue");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    // Flags carry key/default/description; the well-known ones are present.
    let flags = body["flags"].as_array().expect("flags array");
    assert!(!flags.is_empty());
    let keys: Vec<&str> = flags
        .iter()
        .map(|flag| flag["key"].as_str().expect("a key"))
        .collect();
    assert!(keys.contains(&"tables_enabled"));
    assert!(keys.contains(&"pay_first_enabled"));
    let tables = flags
        .iter()
        .find(|flag| flag["key"] == serde_json::json!("tables_enabled"))
        .expect("the tables flag");
    assert_eq!(tables["default_on"], serde_json::json!(true));
    assert!(tables["description"].as_str().expect("a description").len() > 5);

    // The three presets are offered, each as a set of keys.
    let presets = body["presets"].as_array().expect("presets array");
    let preset_ids: Vec<&str> = presets
        .iter()
        .map(|preset| preset["id"].as_str().expect("an id"))
        .collect();
    assert!(preset_ids.contains(&"full_service"));
    assert!(preset_ids.contains(&"counter"));
    assert!(preset_ids.contains(&"retail"));

    // The inter-flag rules are served for the console's conflict preview.
    let rules = body["rules"].as_array().expect("rules array");
    let rule_ids: Vec<&str> = rules
        .iter()
        .map(|rule| rule["id"].as_str().expect("an id"))
        .collect();
    assert!(rule_ids.contains(&"pay_first.excludes.tables"));
    assert!(rule_ids.contains(&"seats.requires.tables"));
}

/// The main app merged with the capability-publish router, sharing one config-tree fake so the test
/// can read back the flags the publish merged onto the Store layer.
fn config_capabilities_app(admin: FakeAdmin, config: FakeConfigTrees) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config.clone(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::config_capabilities_router(
        config,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

#[tokio::test]
async fn publishing_capability_flags_merges_the_store_layer_and_rejects_conflicts() {
    let admin = provisioned_admin();
    let config = FakeConfigTrees::default();
    let router = config_capabilities_app(admin, config.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let publish = |flags: serde_json::Value| {
        router.clone().oneshot(put_with_cookie(
            "/admin/config/capabilities",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid, "flags": flags }),
            &cookie,
        ))
    };

    // First publish sets one flag.
    let first = publish(serde_json::json!({ "tables_enabled": false }))
        .await
        .expect("route first publish");
    assert_eq!(first.status(), StatusCode::OK);

    // Second publish names a different flag — the first must survive (a merge, not a replace).
    let second = publish(serde_json::json!({ "kds_enabled": false }))
        .await
        .expect("route second publish");
    assert_eq!(second.status(), StatusCode::OK);

    let state = config
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    let store_layer = &state.record.layers[2];
    assert_eq!(
        store_layer["tables_enabled"],
        serde_json::json!(false),
        "the flag from the first publish survives the second (merge, not replace)"
    );
    assert_eq!(store_layer["kds_enabled"], serde_json::json!(false));

    // An unknown flag key is refused.
    let unknown = publish(serde_json::json!({ "not_a_flag": true }))
        .await
        .expect("route unknown");
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);

    // A §10-invalid combination (pay-first with table service) is a 422, not a stored state.
    let conflict =
        publish(serde_json::json!({ "tables_enabled": true, "pay_first_enabled": true }))
            .await
            .expect("route conflict");
    assert_eq!(conflict.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let violations = json_body(conflict).await;
    assert_eq!(violations["error"]["status"], "UNPROCESSABLE");
    assert!(
        violations["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "the inter-flag rule is reported: {violations}"
    );
}

// --- Subject-request tooling (`/admin/subjects`, owner-only, ADR-0076) ---------------------------

/// An in-memory subject store: rows keyed by `(tenant, record)`. Only `fetch` and `save_masked` are
/// exercised by the tooling; `due_before` is the cron's and returns nothing here.
#[derive(Clone, Default)]
struct FakeSubjects {
    rows: Arc<Mutex<Vec<(TenantId, SubjectRecord)>>>,
}

impl FakeSubjects {
    fn with(tenant: TenantId, record: SubjectRecord) -> Self {
        Self {
            rows: Arc::new(Mutex::new(vec![(tenant, record)])),
        }
    }
}

impl SubjectStore for FakeSubjects {
    async fn due_before(
        &self,
        _cutoff: Timestamp,
        _limit: u32,
    ) -> Result<Vec<SubjectRecord>, RetentionError> {
        Ok(Vec::new())
    }

    async fn save_masked(&self, masked: &[SubjectRecord]) -> Result<u64, RetentionError> {
        let mut rows = self.rows.lock().expect("lock");
        let mut saved = 0;
        for update in masked {
            if let Some(entry) = rows
                .iter_mut()
                .find(|(_, row)| row.subject_id == update.subject_id)
            {
                entry.1 = update.clone();
                saved += 1;
            }
        }
        Ok(saved)
    }

    async fn fetch(
        &self,
        tenant: TenantId,
        subject_id: SubjectId,
    ) -> Result<Option<SubjectRecord>, RetentionError> {
        let rows = self.rows.lock().expect("lock");
        Ok(rows
            .iter()
            .find(|(row_tenant, row)| *row_tenant == tenant && row.subject_id == subject_id)
            .map(|(_, row)| row.clone()))
    }
}

/// The main router (for the session guard) plus the subject-request sub-router.
fn subjects_app(admin: FakeAdmin, subjects: FakeSubjects) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::subjects_router(
        subjects,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// A synthetic subject record — placeholder values, never anything resembling a real person.
fn a_subject(subject_id: SubjectId) -> SubjectRecord {
    SubjectRecord {
        subject_id,
        collected_at: Timestamp::from_milliseconds_since_epoch(NOW_MS).expect("valid"),
        fields: BTreeMap::from([
            ("name".to_owned(), "Test Subject".to_owned()),
            ("phone".to_owned(), "N/A".to_owned()),
        ]),
        masked_at: None,
    }
}

/// Lookup returns status without values; export returns values; erase masks; and every route is
/// owner-only (ADR-0076). An admin (not owner) is refused.
#[tokio::test]
async fn subject_lookup_export_and_erase_are_owner_only_and_audited() {
    let admin = provisioned_admin();
    let subject_id = SubjectId::new(Ulid::from_u128(0x0ABC));
    let subjects = FakeSubjects::with(tenant(), a_subject(subject_id));
    let router = subjects_app(admin.clone(), subjects);
    let owner = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let non_owner = role_session_cookie(&admin, AdminRole::Admin, "admin-token").await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let sid = subject_id.to_string();
    let lookup_uri = format!("/admin/subjects/{sid}?tenant_id={tenant_ulid}");

    // An admin (not owner) is refused — owner-only, a 403 distinct from the 401 of no session.
    let denied = router
        .clone()
        .oneshot(get_with_cookie(&lookup_uri, &non_owner))
        .await
        .expect("route");
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    // The owner looks up: metadata only, no field values.
    let looked = router
        .clone()
        .oneshot(get_with_cookie(&lookup_uri, &owner))
        .await
        .expect("route");
    assert_eq!(looked.status(), StatusCode::OK);
    let meta = json_body(looked).await;
    assert_eq!(meta["masked"], false);
    assert_eq!(meta["field_count"], 2);
    assert!(meta.get("fields").is_none(), "a lookup returns no values");

    // Export returns the field values — the portability payload.
    let exported = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/subjects/{sid}/export?tenant_id={tenant_ulid}"),
            &owner,
        ))
        .await
        .expect("route");
    assert_eq!(exported.status(), StatusCode::OK);
    assert_eq!(json_body(exported).await["fields"]["name"], "Test Subject");

    // Erase masks the record.
    let erased = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/subjects/{sid}/erase?tenant_id={tenant_ulid}"),
            &serde_json::json!({}),
            &owner,
        ))
        .await
        .expect("route");
    assert_eq!(erased.status(), StatusCode::OK);
    assert_eq!(json_body(erased).await["already_masked"], false);

    // After erasure the lookup shows masked, and an export returns the redaction sentinel.
    let after = router
        .clone()
        .oneshot(get_with_cookie(&lookup_uri, &owner))
        .await
        .expect("route");
    assert_eq!(json_body(after).await["masked"], true);
    let re_export = router
        .oneshot(get_with_cookie(
            &format!("/admin/subjects/{sid}/export?tenant_id={tenant_ulid}"),
            &owner,
        ))
        .await
        .expect("route");
    assert_eq!(
        json_body(re_export).await["fields"]["name"],
        "[REDACTED]",
        "erasure masked the value"
    );
}

/// An unknown id is a 404, and the routes need a session.
#[tokio::test]
async fn subject_lookup_404s_for_an_unknown_id_and_needs_a_session() {
    let admin = provisioned_admin();
    let router = subjects_app(admin.clone(), FakeSubjects::default());
    let owner = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let sid = Ulid::from_u128(0x0999).to_string();
    let uri = format!("/admin/subjects/{sid}?tenant_id={tenant_ulid}");

    let missing = router
        .clone()
        .oneshot(get_with_cookie(&uri, &owner))
        .await
        .expect("route");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let no_session = router.oneshot(get(&uri, None)).await.expect("route");
    assert_eq!(no_session.status(), StatusCode::UNAUTHORIZED);
}

// --- Per-field ULID refusals on `/admin` (Q3b slice 3) ------------------------------------------
//
// Half of the cloud's refusals — 177 sites — were a ULID that would not parse, and every one of
// them was hand-written: 63 phrasings of one sentence, no `details` array for a console to mark the
// offending input with, and about 120 that named *every* field the handler had looked at rather
// than the one that was wrong. `parse_ulid_fields` owns all of them now. These three tests are the
// coverage those refusals never had: the full suite passed both before and after every one of the
// 177 bodies changed shape, which is exactly how they drifted in the first place.

/// A `/admin` request with **one** bad id names that field, with the stable reason beside it.
#[tokio::test]
async fn an_admin_refusal_about_one_id_names_the_field_and_a_stable_reason() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    let cookie = admin_cookie(&router).await;

    let refused = router
        .oneshot(get_with_cookie(
            "/admin/stores?tenant_id=not-a-ulid",
            &cookie,
        ))
        .await
        .expect("route the listing");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["status"], "INVALID_ARGUMENT", "got {body}");
    assert_eq!(body["error"]["message"], "tenant_id is not a ULID");
    assert_eq!(body["error"]["details"][0]["field"], "tenant_id");
    assert_eq!(body["error"]["details"][0]["reason"], "NOT_A_ULID");
}

/// A request carrying **two** ids, one of them wrong, names only the wrong one.
///
/// This is the defect the helper exists to make unwriteable. `PATCH /admin/stores/{store_id}` parses
/// the path's store id and the body's `tenant_id` together, and used to refuse with
/// `"the store id or tenant_id is not a ULID"` whichever one had failed — so an operator whose
/// tenant id was fine was told to go and check it. The console cannot mark a field from a message
/// like that either.
#[tokio::test]
async fn an_admin_refusal_about_two_ids_names_only_the_one_that_was_wrong() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    let cookie = admin_cookie(&router).await;
    let good_tenant = TenantId::new(
        "01J0000000000000000000TEN0"
            .parse()
            .expect("a valid test ULID"),
    );

    // A real tenant id in the body, a broken store id in the path.
    let refused = router
        .oneshot(patch_with_cookie(
            "/admin/stores/not-a-ulid",
            &serde_json::json!({
                "tenant_id": good_tenant.as_ulid().to_string(),
                "name": "Bến Thành",
                "status": "active",
            }),
            &cookie,
        ))
        .await
        .expect("route the update");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["message"], "store_id is not a ULID");
    assert_eq!(body["error"]["details"][0]["field"], "store_id");
    assert!(
        body["error"]["details"][1].is_null(),
        "the tenant_id in the body parsed, so it is not named: {body}"
    );
}

/// Both ids wrong names both, and the prose says so in the plural.
#[tokio::test]
async fn an_admin_refusal_about_two_bad_ids_names_both_of_them() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    let cookie = admin_cookie(&router).await;

    let refused = router
        .oneshot(patch_with_cookie(
            "/admin/stores/still-not-a-ulid",
            &serde_json::json!({
                "tenant_id": "also-not-a-ulid",
                "name": "Bến Thành",
                "status": "active",
            }),
            &cookie,
        ))
        .await
        .expect("route the update");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(
        body["error"]["message"], "store_id and tenant_id are not ULIDs",
        "got {body}"
    );
    assert_eq!(body["error"]["details"][0]["field"], "store_id");
    assert_eq!(body["error"]["details"][1]["field"], "tenant_id");
    assert!(
        body["error"]["details"][2].is_null(),
        "one detail per failed field, no more: {body}"
    );
}

// --- Closed-set and range refusals on `/admin` (Q3b slice 4) ------------------------------------
//
// 22 refusals were about a value outside a closed set, and each spelled the set into its own
// sentence — `"status must be active or archived"` alone at eighteen routes. `enum_refusal` builds
// the prose from the set it is handed, and each set now has one home on its own enum with the
// parser derived from it too; `crates/pos-cloud/src/http.rs`'s `closed_set_tests` pins that the
// prose still reads as it did and that every listed token is one the parser accepts. These two
// cover what only HTTP can show: the body a caller receives.

/// A closed-set refusal names the field, carries a stable reason, and lists what is accepted.
#[tokio::test]
async fn a_closed_set_refusal_names_the_field_and_a_stable_reason() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    let cookie = admin_cookie(&router).await;
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/tenants",
            &serde_json::json!({ "name": "Pizza 4P's" }),
            &cookie,
        ))
        .await
        .expect("route create tenant");
    let tenant_id = json_body(created).await["tenant_id"]
        .as_str()
        .expect("a tenant id")
        .to_owned();

    let refused = router
        .oneshot(patch_with_cookie(
            &format!("/admin/tenants/{tenant_id}"),
            &serde_json::json!({ "name": "Pizza 4P's", "status": "retired" }),
            &cookie,
        ))
        .await
        .expect("route the update");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["status"], "INVALID_ARGUMENT", "got {body}");
    assert_eq!(
        body["error"]["message"], "status must be active or archived",
        "generated from EntityStatus::ALL, and unchanged from the hand-written sentence"
    );
    assert_eq!(body["error"]["details"][0]["field"], "status");
    assert_eq!(body["error"]["details"][0]["reason"], "INVALID_ENUM_VALUE");
}

/// A range refusal about two fields names only the one actually out of range.
///
/// `"open_hour and close_hour must be in 0..=23"` had the same over-naming the ULID refusals did,
/// in a different guise: it named both hours whichever one was wrong. A range is not a set, so
/// there is no prose to generate and the message is unchanged — but `details` now says which.
#[tokio::test]
async fn a_range_refusal_about_two_fields_names_only_the_one_out_of_range() {
    let admin = provisioned_admin();
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    ))
    .merge(http::config_channels_router(
        FakeConfigTrees::default(),
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ));
    let cookie = admin_cookie(&router).await;

    let refused = router
        .oneshot(put_with_cookie(
            "/admin/config/qr",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id().as_ulid().to_string(),
                "enabled": true,
                "staff_confirmation_required": true,
                "per_table_limit": 5,
                "rate_window_secs": 60,
                // A sane opening hour and an impossible closing one.
                "business_hours": { "open_hour": 9, "close_hour": 99 },
            }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(
        body["error"]["details"][0]["field"], "close_hour",
        "got {body}"
    );
    assert_eq!(body["error"]["details"][0]["reason"], "OUT_OF_RANGE");
    assert!(
        body["error"]["details"][1].is_null(),
        "open_hour was 9, which is in range, so it is not named: {body}"
    );
}

/// The two `parse_known_tokens` call sites name the field the request actually carries.
///
/// `http.rs`'s unit tests pin that the helper reports whatever field it is handed and lists the
/// accepted set. They cannot pin that the *call sites* hand it the right name — and that is exactly
/// the failure #150 found five of, where a `details` entry named `tax_rates` on a request whose
/// field is `rates`. A console marking `details.field` would mark an input the caller never sent,
/// which is worse than no detail at all because it is confidently wrong. So the two names go
/// through the real routes.
#[tokio::test]
async fn an_unknown_token_names_the_request_field_it_arrived_in() {
    let admin = provisioned_admin();
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    ))
    .merge(http::config_channels_router(
        FakeConfigTrees::default(),
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ));
    let cookie = admin_cookie(&router).await;

    // `PublishChannelsRequest` carries `enabled`; `PublishTenderRequest` carries `accepted`.
    for (uri, field) in [
        ("/admin/config/channels", "enabled"),
        ("/admin/config/tender", "accepted"),
    ] {
        let refused = router
            .clone()
            .oneshot(put_with_cookie(
                uri,
                &serde_json::json!({
                    "tenant_id": tenant().as_ulid().to_string(),
                    "store_id": store_id().as_ulid().to_string(),
                    field: ["NOT_A_REAL_TOKEN"],
                }),
                &cookie,
            ))
            .await
            .expect("route the publish");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{uri}");
        let body = json_body(refused).await;
        assert_eq!(
            body["error"]["details"][0]["field"], field,
            "{uri} must name the field the body carries: {body}"
        );
        assert_eq!(
            body["error"]["details"][0]["reason"], "INVALID_ENUM_VALUE",
            "{uri}: {body}"
        );
        // And the prose lists the set, so a caller learns what to send instead of guessing.
        let message = body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            !message.contains("UNSPECIFIED"),
            "{uri} must not offer the token its own parser refuses: {message}"
        );
    }
}

/// An absence answers in the envelope, and carries **no** `details` array.
///
/// 103 refusals about an absent entity or a down dependency moved onto `not_found` and
/// `service_unavailable` (Q3b slice 4b). Neither takes a field, deliberately: `details` names a
/// field the caller got wrong, and a caller asking after a store that does not exist got its fields
/// right. Naming one would send it to fix an input that was fine. `http.rs`'s `absence_tests` pins
/// the retryable/terminal split between the two; this pins the body.
#[tokio::test]
async fn an_absence_is_enveloped_and_names_no_field() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    let cookie = admin_cookie(&router).await;
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/tenants",
            &serde_json::json!({ "name": "Pizza 4P's" }),
            &cookie,
        ))
        .await
        .expect("route create tenant");
    let tenant_id = json_body(created).await["tenant_id"]
        .as_str()
        .expect("a tenant id")
        .to_owned();

    // A well-formed store id that names no store.
    let missing = StoreId::new(
        "01JZZZZZZZZZZZZZZZZZZZZZZZ"
            .parse()
            .expect("a valid test ULID"),
    );
    // A well-formed `If-Match` for a row that does not exist: the answer is that the store is
    // absent (`404`), not that the version is stale (`412`). The two are distinguishable, which is
    // the point of the seam returning three outcomes rather than a bool (ADR-0094).
    let refused = router
        .oneshot(patch_with_etag(
            &format!("/admin/stores/{}", missing.as_ulid()),
            &serde_json::json!({ "tenant_id": tenant_id, "name": "Bến Thành", "status": "active" }),
            &cookie,
            "1",
        ))
        .await
        .expect("route the update");
    assert_eq!(refused.status(), StatusCode::NOT_FOUND);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["status"], "NOT_FOUND", "got {body}");
    assert_eq!(body["error"]["message"], "no such store");
    assert!(
        body["error"]["details"].is_null(),
        "an absent store is not a bad field: {body}"
    );
}

// --- The per-handler refusal tail (Q3b slice 4c) -------------------------------------------------
//
// The last 44 refusals, none of them a repeated family, so this is a conversion rather than a
// helper. Three properties are worth pinning even so, and each is one a future reader could
// reasonably get backwards.

/// An authorisation refusal carries **no** `details`, ever.
///
/// This is the rule every slice of Q3b has held to and the only one that is a security property
/// rather than an ergonomic one: naming the missing permission would tell a caller probing its own
/// reach exactly which grant to go after. The `401`/`403`/credential refusals are the sites where
/// the temptation to be helpful is strongest, so the absence is asserted rather than assumed.
#[tokio::test]
async fn an_authorisation_refusal_names_no_field() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let router = registry_app(admin, FakeRegistry::default());

    let refused = router
        .oneshot(post_with_cookie(
            "/admin/tenants",
            &serde_json::json!({ "name": "Pizza 4P's" }),
            &cookie,
        ))
        .await
        .expect("route create tenant");
    assert_eq!(
        refused.status(),
        StatusCode::FORBIDDEN,
        "a signed-in viewer lacks console.orgs.manage"
    );
    let body = json_body(refused).await;
    assert_eq!(body["error"]["status"], "PERMISSION_DENIED", "got {body}");
    assert_eq!(body["error"]["message"], "insufficient permissions");
    assert!(
        body["error"]["details"].is_null(),
        "which permission was missing is exactly what a prober wants: {body}"
    );
}

/// A mutual-exclusion refusal names **both** fields — the one case where that is right.
///
/// Every other multi-field refusal in Q3b was narrowed to name only what was wrong. This one is the
/// deliberate exception, and the distinction is worth a test because it looks like the same bug:
/// `/admin/assignments` wants exactly one of `store_id` or `employee_id`, so when the caller sends
/// neither or both, *the pair* is the mistake and neither field alone is at fault.
#[tokio::test]
async fn a_mutual_exclusion_refusal_names_both_fields_because_the_pair_is_the_mistake() {
    let router = people_app_with_audit(
        provisioned_admin(),
        FakePeople::default(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    // Neither filter given, so the handler cannot tell which listing was wanted.
    let refused = router
        .oneshot(get_with_cookie(
            &format!("/admin/assignments?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the listing");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(
        body["error"]["message"], "name exactly one of store_id or employee_id",
        "got {body}"
    );
    assert_eq!(body["error"]["details"][0]["field"], "store_id");
    assert_eq!(body["error"]["details"][0]["reason"], "MUTUALLY_EXCLUSIVE");
    assert_eq!(body["error"]["details"][1]["field"], "employee_id");
}

// --- Inventory authoring admin routes (ADR-0079, ADR-0095) --------------------------------------

/// An in-memory `InventoryStore` for the route tests.
///
/// Tenant-scoped and version-carrying like the real thing, because that is exactly what the routes
/// under test depend on: a create refuses a taken key, an update applies only at the version a read
/// handed back, and the list is where a caller obtains it.
#[derive(Default, Clone)]
struct FakeInventory {
    ingredients: Arc<Mutex<Vec<Versioned<PublishedIngredient>>>>,
    recipes: Arc<Mutex<Vec<Versioned<PublishedRecipe>>>>,
    suppliers: Arc<Mutex<Vec<Versioned<PublishedSupplier>>>>,
    next_version: Arc<Mutex<u64>>,
}

impl FakeInventory {
    /// The fake's stand-in for `xmin` (ADR-0094): a token that changes on every successful write.
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }
}

impl InventoryStore for FakeInventory {
    async fn list_ingredients(
        &self,
        _tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedIngredient>>, InventoryStoreError> {
        Ok(self.ingredients.lock().expect("lock").clone())
    }

    async fn create_ingredient(
        &self,
        _tenant_id: TenantId,
        ingredient: &PublishedIngredient,
    ) -> Result<CreateOutcome, InventoryStoreError> {
        let version = self.mint();
        let mut rows = self.ingredients.lock().expect("lock");
        if rows.iter().any(|row| row.record.id == ingredient.id) {
            return Ok(CreateOutcome::AlreadyExists);
        }
        rows.push(Versioned::new(ingredient.clone(), version.clone()));
        Ok(CreateOutcome::Created(version))
    }

    async fn update_ingredient(
        &self,
        _tenant_id: TenantId,
        ingredient: &PublishedIngredient,
        expected: &Version,
    ) -> Result<UpdateOutcome, InventoryStoreError> {
        let version = self.mint();
        let mut rows = self.ingredients.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|row| row.record.id == ingredient.id) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record = ingredient.clone();
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }

    async fn delete_ingredient(
        &self,
        _tenant_id: TenantId,
        ingredient_id: IngredientId,
    ) -> Result<(), InventoryStoreError> {
        self.ingredients
            .lock()
            .expect("lock")
            .retain(|row| row.record.id != ingredient_id);
        Ok(())
    }

    async fn list_recipes(
        &self,
        _tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedRecipe>>, InventoryStoreError> {
        Ok(self.recipes.lock().expect("lock").clone())
    }

    async fn create_recipe(
        &self,
        _tenant_id: TenantId,
        recipe: &PublishedRecipe,
    ) -> Result<CreateOutcome, InventoryStoreError> {
        let version = self.mint();
        let mut rows = self.recipes.lock().expect("lock");
        if rows.iter().any(|row| row.record.item == recipe.item) {
            return Ok(CreateOutcome::AlreadyExists);
        }
        rows.push(Versioned::new(recipe.clone(), version.clone()));
        Ok(CreateOutcome::Created(version))
    }

    async fn update_recipe(
        &self,
        _tenant_id: TenantId,
        recipe: &PublishedRecipe,
        expected: &Version,
    ) -> Result<UpdateOutcome, InventoryStoreError> {
        let version = self.mint();
        let mut rows = self.recipes.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|row| row.record.item == recipe.item) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record = recipe.clone();
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }

    async fn delete_recipe(
        &self,
        _tenant_id: TenantId,
        item: MenuItemId,
    ) -> Result<(), InventoryStoreError> {
        self.recipes
            .lock()
            .expect("lock")
            .retain(|row| row.record.item != item);
        Ok(())
    }

    async fn list_suppliers(
        &self,
        _tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedSupplier>>, InventoryStoreError> {
        Ok(self.suppliers.lock().expect("lock").clone())
    }

    async fn create_supplier(
        &self,
        _tenant_id: TenantId,
        supplier: &PublishedSupplier,
    ) -> Result<CreateOutcome, InventoryStoreError> {
        let version = self.mint();
        let mut rows = self.suppliers.lock().expect("lock");
        if rows.iter().any(|row| row.record.id == supplier.id) {
            return Ok(CreateOutcome::AlreadyExists);
        }
        rows.push(Versioned::new(supplier.clone(), version.clone()));
        Ok(CreateOutcome::Created(version))
    }

    async fn update_supplier(
        &self,
        _tenant_id: TenantId,
        supplier: &PublishedSupplier,
        expected: &Version,
    ) -> Result<UpdateOutcome, InventoryStoreError> {
        let version = self.mint();
        let mut rows = self.suppliers.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|row| row.record.id == supplier.id) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record = supplier.clone();
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }

    async fn delete_supplier(
        &self,
        _tenant_id: TenantId,
        supplier_id: SupplierId,
    ) -> Result<(), InventoryStoreError> {
        self.suppliers
            .lock()
            .expect("lock")
            .retain(|row| row.record.id != supplier_id);
        Ok(())
    }
}

/// The admin surface plus the inventory router, on fakes.
fn inventory_app(admin: FakeAdmin, inventory: FakeInventory) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::inventory_router(
        inventory,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// A recipe write body, and the `POST` variant that also names the item it makes.
fn recipe_bodies(
    tenant: &str,
    item: &str,
    threshold: i64,
) -> (serde_json::Value, serde_json::Value) {
    let write =
        serde_json::json!({ "tenant_id": tenant, "lines": [], "auto_86_threshold": threshold });
    let mut create = write.clone();
    create["item_id"] = serde_json::Value::String(item.to_owned());
    (create, write)
}

/// A recipe's key is the item it makes, and that id comes from the caller — so this was the seam
/// that lost data: the `PUT` was a create-or-replace, and "add a recipe" for an item that already
/// had one silently discarded its bill of materials (ADR-0095).
#[tokio::test]
async fn adding_a_recipe_for_an_item_that_has_one_is_refused_rather_than_replacing_it() {
    let router = inventory_app(provisioned_admin(), FakeInventory::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let item = ulid_text(500);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/inventory/recipes",
            &recipe_bodies(&tenant, &item, 2).0,
            &cookie,
        ))
        .await
        .expect("route create recipe");
    assert_eq!(created.status(), StatusCode::CREATED);
    let at_create = etag_of(&created);

    let again = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/inventory/recipes",
            &recipe_bodies(&tenant, &item, 9).0,
            &cookie,
        ))
        .await
        .expect("route the duplicate create");
    assert_eq!(again.status(), StatusCode::CONFLICT);

    // The read-one carries the version as an `ETag` and as an `etag` field, and the threshold the
    // refused create did not get to overwrite.
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/inventory/recipes/{item}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route read recipe");
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(etag_of(&read), at_create);
    let fetched = json_body(read).await;
    assert_eq!(fetched["auto_86_threshold"], 2);
    assert_eq!(fetched["etag"].as_str().expect("an etag field"), at_create);

    // Deleting frees the item, so a create at it succeeds again.
    let removed = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/inventory/recipes/{item}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route delete recipe");
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);

    let recreated = router
        .oneshot(post_with_cookie(
            "/admin/inventory/recipes",
            &recipe_bodies(&tenant, &item, 2).0,
            &cookie,
        ))
        .await
        .expect("route create after delete");
    assert_eq!(recreated.status(), StatusCode::CREATED);
}

/// Editing a recipe needs the version it was read at: unconditional is refused, the right one
/// applies, and a spent one is a `412`.
#[tokio::test]
async fn editing_a_recipe_needs_the_version_the_read_carried() {
    let router = inventory_app(provisioned_admin(), FakeInventory::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let item = ulid_text(500);
    let row = format!("/admin/inventory/recipes/{item}");

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/inventory/recipes",
            &recipe_bodies(&tenant, &item, 2).0,
            &cookie,
        ))
        .await
        .expect("route create recipe");
    let at_create = etag_of(&created);

    let unconditional = router
        .clone()
        .oneshot(put_with_cookie(
            &row,
            &recipe_bodies(&tenant, &item, 5).1,
            &cookie,
        ))
        .await
        .expect("route the unconditional edit");
    assert_eq!(unconditional.status(), StatusCode::BAD_REQUEST);

    let edited = router
        .clone()
        .oneshot(put_with_etag(
            &row,
            &recipe_bodies(&tenant, &item, 5).1,
            &cookie,
            &at_create,
        ))
        .await
        .expect("route edit recipe");
    assert_eq!(edited.status(), StatusCode::OK);
    let at_edit = etag_of(&edited);
    assert_ne!(at_edit, at_create, "an applied edit moves the version");

    let replayed = router
        .clone()
        .oneshot(put_with_etag(
            &row,
            &recipe_bodies(&tenant, &item, 7).1,
            &cookie,
            &at_create,
        ))
        .await
        .expect("route the stale edit");
    assert_eq!(replayed.status(), StatusCode::PRECONDITION_FAILED);

    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/inventory/recipes?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list recipes");
    let rows = json_body(listed).await;
    assert_eq!(
        rows.as_array().expect("array").len(),
        1,
        "neither the refused edit nor the applied one added a row"
    );
    assert_eq!(rows[0]["auto_86_threshold"], 5);
    assert_eq!(rows[0]["etag"].as_str().expect("a row etag"), at_edit);
}
