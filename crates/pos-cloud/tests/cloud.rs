// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `pos_cloud`'s ingest and rollup spine and its public `/v1` surface, against the in-memory fakes.
//!
//! The same handler code runs here against `pos-fakes` and, in the binary, against `store-postgres`
//! (ADR-0026) — so idempotent ingest, the materialised rollup read, and the `/v1` bearer check are
//! proven without a database, while the store-specific behaviour (RLS, partitioning, the rollup and
//! API-key tables) is proven by `store-postgres`'s own integration suite.

mod assignment_contract;
mod confirmed_values;
mod employee_contract;
// Each business area's route tests, in a directory beside this file.
#[path = "cloud/admin.rs"]
mod admin;
#[path = "cloud/admin_management.rs"]
mod admin_management;
#[path = "cloud/alerts.rs"]
mod alerts;
#[path = "cloud/archives.rs"]
mod archives;
#[path = "cloud/audit.rs"]
mod audit;
#[path = "cloud/capabilities.rs"]
mod capabilities;
#[path = "cloud/catalog_items.rs"]
mod catalog_items;
#[path = "cloud/catalog_menus.rs"]
mod catalog_menus;
#[path = "cloud/catalog_publish.rs"]
mod catalog_publish;
#[path = "cloud/catalog_taxonomy.rs"]
mod catalog_taxonomy;
#[path = "cloud/cell_standing.rs"]
mod cell_standing;
#[path = "cloud/chain.rs"]
mod chain;
#[path = "cloud/config_nodes.rs"]
mod config_nodes;
#[path = "cloud/config_preview.rs"]
mod config_preview;
#[path = "cloud/config_releases.rs"]
mod config_releases;
#[path = "cloud/config_rollback.rs"]
mod config_rollback;
#[path = "cloud/config_store.rs"]
mod config_store;
#[path = "cloud/countries.rs"]
mod countries;
#[path = "cloud/device_activation.rs"]
mod device_activation;
#[path = "cloud/device_admitted.rs"]
mod device_admitted;
#[path = "cloud/device_claims.rs"]
mod device_claims;
#[path = "cloud/device_publish.rs"]
mod device_publish;
#[path = "cloud/device_revoke.rs"]
mod device_revoke;
#[path = "cloud/device_tills.rs"]
mod device_tills;
#[path = "cloud/devices.rs"]
mod devices;
#[path = "cloud/fee_menu_publish.rs"]
mod fee_menu_publish;
#[path = "cloud/fee_preview.rs"]
mod fee_preview;
#[path = "cloud/fees.rs"]
mod fees;
#[path = "cloud/fleet.rs"]
mod fleet;
#[path = "cloud/floor.rs"]
mod floor;
#[path = "cloud/hq_rollups.rs"]
mod hq_rollups;
#[path = "cloud/integrations.rs"]
mod integrations;
#[path = "cloud/inventory.rs"]
mod inventory;
#[path = "cloud/media.rs"]
mod media;
#[path = "cloud/ota_artifacts.rs"]
mod ota_artifacts;
#[path = "cloud/ota_handovers.rs"]
mod ota_handovers;
#[path = "cloud/ota_releases.rs"]
mod ota_releases;
#[path = "cloud/ota_report.rs"]
mod ota_report;
#[path = "cloud/ota_rollout.rs"]
mod ota_rollout;
#[path = "cloud/people.rs"]
mod people;
#[path = "cloud/people_enforced.rs"]
mod people_enforced;
#[path = "cloud/people_pins.rs"]
mod people_pins;
mod people_republish_contract;
#[path = "cloud/people_republisher.rs"]
mod people_republisher;
#[path = "cloud/people_scopes.rs"]
mod people_scopes;
#[path = "cloud/reason_codes.rs"]
mod reason_codes;
#[path = "cloud/reconcile.rs"]
mod reconcile;
#[path = "cloud/registry.rs"]
mod registry;
#[path = "cloud/settings.rs"]
mod settings;
#[path = "cloud/settings_nodes.rs"]
mod settings_nodes;
#[path = "cloud/store_groups.rs"]
mod store_groups;
#[path = "cloud/subjects.rs"]
mod subjects;
#[path = "cloud/sync.rs"]
mod sync;
#[path = "cloud/task_health.rs"]
mod task_health;
#[path = "cloud/tax_rates.rs"]
mod tax_rates;
#[path = "cloud/translations.rs"]
mod translations;
#[path = "cloud/webhooks.rs"]
mod webhooks;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use pos_cloud::activation::{
    ActivationCodeStore, ActivationStoreError, DeviceCredential, IssuedCode, hash_code,
};
use pos_cloud::alerts::{AlertRecord, AlertStore, AlertStoreError};
use pos_cloud::audit::{
    AuditEntry, AuditQuery, AuditRecorder, AuditSink, AuditStore, AuditStoreError,
    NoopAuditRecorder, TrailOrder,
};
use pos_cloud::auth::SuperAdminCredential;
use pos_cloud::auth::admin::{
    AdminCredential, AdminInvite, AdminLogin, AdminRole, AdminStatus, AdminStore, AdminStoreError,
    AdminUser, IMPLICIT_OWNER_EMAIL, IMPLICIT_OWNER_ID, LiveSession, NewAdminInvite,
    NewAdminSession, NewAdminUser, NewRecoveryCode, SessionSummary, hash_session_token,
};
use pos_cloud::auth::apikey::{
    ApiKeyAdminStore, ApiKeyId, ApiKeyStore, ApiKeyStoreError, ApiKeySummary, Scope, StoredApiKey,
    issue,
};
use pos_cloud::auth::password::hash_password;
use pos_cloud::auth::rate_limit::SlidingRateLimiter;
use pos_cloud::auth::totp::{DIGITS, TotpSecret, code_at};
use pos_cloud::campaigns::{CampaignStore, CampaignStoreError};
use pos_cloud::catalog::{
    CatalogItem, CatalogStore, CatalogStoreError, Course, DisplayCategory, DisplaySubcategory,
    ItemCategory, ItemListFilter, ItemSort, ItemSubcategory, LayoutButton, Menu, MenuId,
    MenuPlacement, MenuSection, ModifierGroup, TaxClass,
};
use pos_cloud::config_tree::{
    CapabilityValidator, ConfigLevel, ConfigStoreError, ConfigTree, ConfigTreeState,
    ConfigTreeStore, PublishedVersion,
};
use pos_cloud::dashboard::{RollupError, RollupStore, StoredRollups, project};
use pos_cloud::data_migrations::{DataMigrations, InMemoryDataMigrations};
use pos_cloud::devices::{
    DeviceKind, DeviceProposalError, DeviceProposalId, DeviceProposalStatus, DeviceProposalStore,
    DeviceProposalSummary, DeviceWriteOutcome, PersistedDeviceProposal,
};
use pos_cloud::fees::InMemoryFeeRules;
use pos_cloud::fleet::{FleetRow, FleetStore, FleetStoreError, OtaReportStore, PrintAgentStanding};
use pos_cloud::floorplan::{
    Area, AreaStore, AreaUpdate, FloorStoreError, NewArea, NewRoutingRule, NewStation, NewTable,
    RoutingRule, RoutingRuleId, RoutingRuleStore, Station, StationStore, StationUpdate, Table,
    TableStore, TableUpdate,
};
use pos_cloud::health::{TaskHealth, TaskHealthError, TaskHealthStore};
use pos_cloud::http::CloudApp;
use pos_cloud::inventory::{InventoryStore, InventoryStoreError};
use pos_cloud::lease::{
    Region, RegionAcknowledgement, RegionAcknowledgementError, RegionAcknowledgementStore,
    RegionWrite, StorePlacement,
};
use pos_cloud::media::{
    MediaId, MediaStore, MediaStoreError, MediaSummary, NewMediaAsset, Rendition,
};
use pos_cloud::orders::{StoreDirectory, orders_router};
use pos_cloud::paging::{Page, PageRequest};
use pos_cloud::people::{
    Assignment, AssignmentId, AssignmentScope, AssignmentStore, AssignmentStoreError, Employee,
    EmployeeId, EmployeeListFilter, EmployeeSort, EmployeeStore, EmployeeStoreError,
    EmployeeUpdate, NewAssignment, NewEmployee, NewRoleTemplate, RoleTemplate, RoleTemplateId,
    RoleTemplateStore, RoleTemplateStoreError, RoleTemplateUpdate, is_known_permission,
    permission_catalogue,
};
use pos_cloud::qr::{TableTokenSecret, mint_table_token};
use pos_cloud::qr_http::qr_router;
use pos_cloud::qr_ordering;
use pos_cloud::reason_codes::{ReasonCodeStore, ReasonCodeStoreError};
use pos_cloud::reconcile::{ReconcileError, ReconcileRun, ReconcileRunStore, ReconcileStore};
use pos_cloud::registry::{
    BrandRecord, DeviceRecord, EntityStatus, RegistryStore, RegistryStoreError, StoreRecord,
    TenantRecord,
};
use pos_cloud::relay::{
    OrderQueueId, OrderQueueStore, OrderRecord, OrderRelay, OrderStatus, PendingOrder,
    QueuedOrderPayload, StoreOutcome, orders_sync_router_with_cap,
};
use pos_cloud::starting_roles::RoleSeeder;
use pos_cloud::store_groups::{
    BatchResult, ConfigBatch, ConfigBatchId, StoreGroup, StoreGroupId, StoreGroupStore,
    StoreGroupStoreError,
};
use pos_cloud::tax::{TaxRateEntry, TaxRateStore, TaxRateStoreError};
use pos_cloud::version::{CreateOutcome, UpdateOutcome, Version, Versioned};
use pos_cloud::wake::SharedWake;
use pos_cloud::webhook::{
    PersistedWebhook, WebhookEndpointId, WebhookEndpointStore, WebhookStoreError, WebhookSummary,
};
use pos_cloud::{Cloud, IngestOutcome, StoreOwner, StoreOwners, http};
use pos_contract_tests::fixtures;
use pos_core::activation::{ActivationCode, CodeStatus};
use pos_fakes::vendors::{known_menu_item, unknown_menu_item};
use pos_fakes::{FakeClock, FakeIntake, FakeStore};
use pos_ports::PortError;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::BusinessDate;
use pos_proto::campaign::PublishedCampaign;
use pos_proto::devices::{DeviceConnection, PaperWidth};
use pos_proto::display::GridPosition;
use pos_proto::enums::{EdgePlacement, SalesChannel};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::CampaignId;
use pos_proto::ids::{
    AreaId, BrandId, ConfigVersionId, CourseId, DeviceId, EventId, IngredientId, MenuItemId,
    ReasonCodeId, StationId, StoreId, SupplierId, TableId, TenantId,
};
use pos_proto::inventory::{PublishedIngredient, PublishedRecipe, PublishedSupplier};
use pos_proto::printing::{ReceiptLanguage, ReceiptSecondLanguage};
use pos_proto::reason_codes::PublishedReasonCode;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::{Open, WireEnum as _};
use sha2::Sha256;

/// A fixed 16-byte salt, so a hashed fixture is deterministic. Never used in production.
const SALT: &[u8] = b"a-fixed-test-slt";

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

/// The API-key store the bearer check consults, keyed by the public id. Device credentials live in
/// a table of their own, as in the real store (ADR-0143).
#[derive(Clone, Default)]
struct FakeKeys {
    rows: Arc<Mutex<HashMap<ApiKeyId, StoredApiKey>>>,
    devices: Arc<Mutex<HashMap<ApiKeyId, StoredApiKey>>>,
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

    async fn lookup_device(&self, id: ApiKeyId) -> Result<Option<StoredApiKey>, ApiKeyStoreError> {
        Ok(self.devices.lock().expect("lock").get(&id).cloned())
    }
}

/// Mints a box's device credential for `store` the way activation does, and returns the
/// `posdev_…` token the box would present (ADR-0143).
fn issue_device_credential(keys: &FakeKeys, tenant_id: TenantId, store: StoreId) -> String {
    let id = Ulid::from_u128(0x00DE_71CE);
    let (minted, token) = pos_cloud::activation::mint_device_credential(id, FAKE_SECRET);
    let stored = StoredApiKey::for_device(
        ApiKeyId::new(minted.id),
        &tenant_id.to_string(),
        &store.to_string(),
        &minted.secret_hash(),
    )
    .expect("a well-formed device row");
    keys.devices.lock().expect("lock").insert(stored.id, stored);
    token
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
                store_id: key.store_id.map(|id| id.to_string()),
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
    issue_key_for(keys, tenant_id, None, scopes)
}

/// As [`issue_key`], but bound to one store — a *store's* own credential (S1).
///
/// The `/sync/stores/{store_id}/…` routes require this: a key that names no store, or names another
/// one, is refused there however good its scopes.
fn issue_store_key(
    keys: &FakeKeys,
    tenant_id: TenantId,
    store: StoreId,
    scopes: &[Scope],
) -> String {
    issue_key_for(keys, tenant_id, Some(store), scopes)
}

fn issue_key_for(
    keys: &FakeKeys,
    tenant_id: TenantId,
    store: Option<StoreId>,
    scopes: &[Scope],
) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(0x00A1_1CE0);
    let id = ApiKeyId::new(Ulid::from_u128(u128::from(
        NEXT_ID.fetch_add(1, Ordering::Relaxed),
    )));
    let (stored, token) = issue(
        id,
        tenant_id,
        store,
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

/// `credentials` is each admin's own password hash and TOTP secret — the `admin_users` columns that
/// [ADR-0119](../../../docs/adr/0119-each-admin-signs-in-as-themselves.md) makes sign-in read.
/// `credential`/`last_used_totp_step` are the legacy single `super_admin` row, still on the seam.
#[derive(Clone, Default)]
struct FakeAdmin {
    credential: Arc<Mutex<Option<SuperAdminCredential>>>,
    last_used_totp_step: Arc<Mutex<Option<u64>>>,
    sessions: Arc<Mutex<SessionRows>>,
    admin_users: Arc<Mutex<Vec<AdminUser>>>,
    credentials: Arc<Mutex<HashMap<String, AdminCredential>>>,
    invites: Arc<Mutex<Vec<StoredInvite>>>,
    recovery_codes: Arc<Mutex<HashMap<String, Vec<StoredRecoveryCode>>>>,
}

impl FakeAdmin {
    /// An installation with one enrolled admin holding `credential` — the migrated/implicit owner,
    /// which is the shape every real installation has today.
    fn provisioned(credential: SuperAdminCredential) -> Self {
        Self::provisioned_as(IMPLICIT_OWNER_ID, IMPLICIT_OWNER_EMAIL, credential)
    }

    /// The same, but with the one admin under a named id and address — for the tests that assert on
    /// *which* admin a route resolved to, and which therefore cannot use the placeholder identity.
    fn provisioned_as(id: &str, email: &str, credential: SuperAdminCredential) -> Self {
        Self {
            credential: Arc::new(Mutex::new(Some(credential.clone()))),
            admin_users: Arc::new(Mutex::new(vec![AdminUser {
                id: id.to_owned(),
                email: email.to_owned(),
                name: "Owner".to_owned(),
                role: AdminRole::Owner,
                status: AdminStatus::Active,
            }])),
            credentials: Arc::new(Mutex::new(HashMap::from([(
                id.to_owned(),
                AdminCredential {
                    credential,
                    last_used_totp_step: None,
                },
            )]))),
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
        // The credential lands with the identity, as the single `admin_users` INSERT writes it —
        // which is what lets the new admin sign in at all (ADR-0119).
        self.credentials.lock().expect("lock").insert(
            user.id.clone(),
            AdminCredential {
                credential: SuperAdminCredential::new(
                    user.password_phc,
                    TotpSecret::new(user.totp_secret),
                ),
                last_used_totp_step: None,
            },
        );
        users.push(AdminUser {
            id: user.id,
            email: user.email,
            name: user.name,
            role: user.role,
            status: AdminStatus::Active,
        });
        Ok(true)
    }

    async fn find_admin_login_by_email(
        &self,
        email: &str,
    ) -> Result<Option<AdminLogin>, AdminStoreError> {
        let users = self.admin_users.lock().expect("lock");
        let Some(user) = users
            .iter()
            .find(|user| user.email.eq_ignore_ascii_case(email))
        else {
            return Ok(None);
        };
        let credentials = self.credentials.lock().expect("lock");
        Ok(credentials
            .get(&user.id)
            .cloned()
            .map(|credential| AdminLogin {
                user: user.clone(),
                credential,
            }))
    }

    async fn record_admin_totp_step(
        &self,
        admin_id: &str,
        step: u64,
    ) -> Result<(), AdminStoreError> {
        if let Some(stored) = self.credentials.lock().expect("lock").get_mut(admin_id) {
            // Forward only, as the `IS NULL OR < $2` guard makes the real UPDATE.
            if stored.last_used_totp_step.is_none_or(|seen| step > seen) {
                stored.last_used_totp_step = Some(step);
            }
        }
        Ok(())
    }

    async fn rotate_admin_totp_secret(
        &self,
        admin_id: &str,
        secret: Vec<u8>,
    ) -> Result<(), AdminStoreError> {
        if let Some(stored) = self.credentials.lock().expect("lock").get_mut(admin_id) {
            stored.credential = stored.credential.clone().with_totp(TotpSecret::new(secret));
            stored.last_used_totp_step = None;
        }
        Ok(())
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
    FakeAdmin::provisioned(admin_credential())
}

/// The same, under a named id and address — for the routes that report *which* admin acted, whose
/// assertions would otherwise read the synthetic placeholder identity.
fn provisioned_admin_as(id: &str, email: &str) -> FakeAdmin {
    FakeAdmin::provisioned_as(id, email, admin_credential())
}

/// The shared test credential: `ADMIN_PASSWORD` hashed under a fixed salt, and `ADMIN_TOTP_SEED`.
fn admin_credential() -> SuperAdminCredential {
    let phc = hash_password(ADMIN_PASSWORD, SALT).expect("hash");
    SuperAdminCredential::new(phc, TotpSecret::new(ADMIN_TOTP_SEED.to_vec()))
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

/// When each store's handover was retired and by whom — `store_lease.retired_at`/`retired_by`
/// (migration 0054). Named because the two columns are written and cleared together and mean
/// nothing apart: a schema in which one can exist without the other is one that will hold exactly
/// that.
type Retirements = HashMap<(TenantId, StoreId), (i64, String)>;

/// The print-agent standings a store last reported, keyed as `store_liveness` is (ADR-0112).
type RecordedAgents = HashMap<(TenantId, StoreId), Vec<PrintAgentStanding>>;

/// The config-tree store, keyed by `(tenant, store)` exactly as the real table. `seen` mirrors the
/// `store_liveness` upsert so a test can assert the config pull recorded the store's contact.
#[derive(Clone, Default)]
struct FakeConfigTrees {
    rows: Arc<Mutex<ConfigRows>>,
    seen: Arc<Mutex<HashMap<(TenantId, StoreId), RecordedSeen>>>,
    /// The publish backlog a heartbeat last reported, mirroring `store_liveness.outbox_depth`. A
    /// heartbeat that reported none leaves the entry alone, as the adapter's `COALESCE` does.
    outbox: Arc<Mutex<HashMap<(TenantId, StoreId), u64>>>,
    /// The print-agent standings a heartbeat last reported, mirroring `store_liveness.print_agents`
    /// ([ADR-0112](../../../docs/adr/0112-print-agents.md)). Same `COALESCE` rule as the depth, and
    /// the case that matters is the one it does *not* share: an empty list is a report, so it
    /// replaces the entry rather than leaving it — a manager releasing the last terminal must not
    /// leave a stale agent on the console.
    print_agents: Arc<Mutex<RecordedAgents>>,
    /// A store's recorded answer to its region difference, mirroring
    /// `store_region_acknowledgement` ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    /// One row per store and overwritten, exactly as the table is.
    acknowledgements: Arc<Mutex<HashMap<(TenantId, StoreId), RegionAcknowledgement>>>,
    /// The authoritative lease generation per store, mirroring `store_lease` (ADR-0108). Absent
    /// until the first bump, which is what "no lease in force" means.
    leases: Arc<Mutex<HashMap<(TenantId, StoreId), u64>>>,
    /// The edge placement per store, mirroring the `store_lease.edge_placement` column added by
    /// migration 0052 (ADR-0110). Keyed and written only by [`LeaseStore::bump`], exactly as the
    /// column is, so a test cannot set it by a route that does not exist.
    placements: Arc<Mutex<HashMap<(TenantId, StoreId), EdgePlacement>>>,
    /// The generation the last bump displaced and nothing has proved drained, mirroring the
    /// `store_lease.superseded_generation` column added by migration 0053 (ADR-0110). Written by
    /// the same `bump` call as the two maps above, under the same lock section, for the reason the
    /// placement's own note gives: a fake that updated them separately would pass tests the real
    /// column — one row, one statement — could not.
    superseded: Arc<Mutex<HashMap<(TenantId, StoreId), u64>>>,
    /// When a handover was retired and by whom, mirroring `store_lease.retired_at`/`retired_by`
    /// (migration 0054, ADR-0110). Written under the same lock section as the three maps above for
    /// the same reason, and — like the real statement — **cleared by every bump**: a new handover
    /// has a new outgoing machine, so a retirement from the previous one no longer describes
    /// anything.
    retired: Arc<Mutex<Retirements>>,
    /// Where each store's hosted machine is, mirroring the `store_lease.region_country` and
    /// `region_label` columns added by migration 0058 (ADR-0114). Absent is the honest state for an
    /// in-store placement: the machine is in the shop and has no region.
    ///
    /// One map holding the pair rather than two maps holding a half each, because the real
    /// statement writes and clears both columns together and there is no write anywhere that
    /// touches one alone — a fake with two maps would let a test produce a row the schema's writes
    /// cannot.
    regions: Arc<Mutex<HashMap<(TenantId, StoreId), Region>>>,
    next_version: Arc<Mutex<u64>>,
    /// A competing publish to land *between* the next read and its write, so a test can produce the
    /// race the retry exists for. `(key, value)` is set on the Store layer, as another node publish
    /// would set it.
    interpose: Arc<Mutex<Option<(String, serde_json::Value)>>>,
    /// The highest receipt number the fake's log holds for any store (ADR-0149), or `None` for a
    /// log with no settled bill.
    highest_receipt: Arc<Mutex<Option<u64>>>,
}

impl pos_cloud::lease::LeaseStore for FakeConfigTrees {
    async fn highest_receipt_number(
        &self,
        _tenant: TenantId,
        _store: StoreId,
    ) -> Result<Option<u64>, pos_cloud::lease::LeaseStoreError> {
        Ok(*self.highest_receipt.lock().expect("lock"))
    }

    /// Bumps in memory exactly as the adapter's one statement does: absent starts at generation 0,
    /// present moves to `generation + 1`, and there is no way to set an arbitrary value.
    ///
    /// The placement follows the same `COALESCE` the SQL does — a bump that names one sets it, a
    /// bump that names none keeps what is there, and a store's first bump with none falls to the
    /// column's `IN_STORE` default. Both maps are written under the same call, which is the property
    /// ADR-0110 is about; a fake that updated them separately would pass tests the real column
    /// could not.
    async fn bump(
        &self,
        tenant: TenantId,
        store: StoreId,
        _issued_at: Timestamp,
        edge_placement: Option<EdgePlacement>,
        region: RegionWrite,
        acknowledge_undrained: Option<pos_core::lease::LeaseGeneration>,
        expected_generation: Option<pos_core::lease::LeaseGeneration>,
    ) -> Result<pos_cloud::lease::LeaseBumpOutcome, pos_cloud::lease::LeaseStoreError> {
        let mut leases = self.leases.lock().expect("lock");
        let mut placements = self.placements.lock().expect("lock");
        let mut superseded = self.superseded.lock().expect("lock");
        let mut retired = self.retired.lock().expect("lock");
        let mut regions = self.regions.lock().expect("lock");
        let held = leases.get(&(tenant, store)).copied();
        // The two conditions the real statement evaluates in its own `WHERE`, in the same order and
        // for the same reason: a caller holding a stale read is told *that*, rather than being sent
        // to acknowledge a handover it has not seen. Checked before anything is written, because in
        // the adapter the check and the write are one statement.
        if held.map(pos_core::lease::LeaseGeneration::new) != expected_generation {
            return Ok(pos_cloud::lease::LeaseBumpOutcome::VersionMismatch {
                current: held.map(pos_core::lease::LeaseGeneration::new),
            });
        }
        if let Some(undrained) = superseded.get(&(tenant, store)).copied()
            && acknowledge_undrained != Some(pos_core::lease::LeaseGeneration::new(undrained))
        {
            return Ok(pos_cloud::lease::LeaseBumpOutcome::Undrained {
                superseded: pos_core::lease::LeaseGeneration::new(undrained),
            });
        }
        let next = held.map_or(0, |value| value.saturating_add(1));
        leases.insert((tenant, store), next);
        // The generation just displaced, mirroring `superseded_generation = store_lease.generation`
        // in the real upsert: the value *before* the increment, and absent for a first-ever lease,
        // which supersedes nobody. Read from `held` rather than recomputed from `next`, so a change
        // to how `next` is derived cannot silently make the two disagree.
        if let Some(previous) = held {
            superseded.insert((tenant, store), previous);
        }
        // `retired_at = NULL, retired_by = NULL` in the same SET clause. A bump starts a new
        // handover, so the previous one's retirement stops describing this row.
        retired.remove(&(tenant, store));
        let placement = edge_placement.unwrap_or_else(|| {
            placements
                .get(&(tenant, store))
                .copied()
                .unwrap_or(EdgePlacement::InStore)
        });
        placements.insert((tenant, store), placement);
        // The `CASE WHEN $7` the statement uses: `Keep` leaves both columns exactly as they are,
        // and the other two decide them. Written under the same lock section as the placement it
        // qualifies, because in the adapter they are one statement — a fake that wrote them
        // separately would pass tests the real row could not.
        match region {
            RegionWrite::Keep => {}
            RegionWrite::Clear => {
                regions.remove(&(tenant, store));
            }
            RegionWrite::Set(region) => {
                regions.insert((tenant, store), region);
            }
        }
        Ok(pos_cloud::lease::LeaseBumpOutcome::Issued(
            pos_cloud::lease::LeaseBump {
                generation: pos_core::lease::LeaseGeneration::new(next),
                edge_placement: placement,
                // Read back out of the map rather than echoed from the argument, as the adapter
                // reads it out of `RETURNING`: a swap in place reports the region the store has.
                region: regions.get(&(tenant, store)).cloned(),
                superseded_generation: superseded
                    .get(&(tenant, store))
                    .copied()
                    .map(pos_core::lease::LeaseGeneration::new),
            },
        ))
    }

    async fn current(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> Result<Option<pos_core::lease::LeaseGeneration>, pos_cloud::lease::LeaseStoreError> {
        Ok(self
            .leases
            .lock()
            .expect("lock")
            .get(&(tenant, store))
            .copied()
            .map(pos_core::lease::LeaseGeneration::new))
    }

    /// The adapter's `UPDATE … WHERE superseded_generation = $3`: clears only when the column holds
    /// the named value, and reports what it holds instead when it does not.
    async fn settle_handover(
        &self,
        tenant: TenantId,
        store: StoreId,
        superseded: pos_core::lease::LeaseGeneration,
    ) -> Result<pos_cloud::lease::SettleOutcome, pos_cloud::lease::LeaseStoreError> {
        let leases = self.leases.lock().expect("lock");
        let mut in_flight = self.superseded.lock().expect("lock");
        if !leases.contains_key(&(tenant, store)) {
            return Ok(pos_cloud::lease::SettleOutcome::NoLease);
        }
        let current = in_flight.get(&(tenant, store)).copied();
        if current != Some(superseded.value()) {
            return Ok(pos_cloud::lease::SettleOutcome::NotSuperseded {
                current: current.map(pos_core::lease::LeaseGeneration::new),
            });
        }
        in_flight.remove(&(tenant, store));
        Ok(pos_cloud::lease::SettleOutcome::Settled)
    }

    /// The adapter's `UPDATE … WHERE generation > 0 AND superseded_generation IS NULL AND
    /// retired_at IS NULL`: every precondition checked before anything is written, in the
    /// statement's own order, so a person looking at an in-flight handover is told *that* rather
    /// than being told it is already retired.
    async fn retire(
        &self,
        tenant: TenantId,
        store: StoreId,
        retired_at: Timestamp,
        retired_by: &str,
    ) -> Result<pos_cloud::lease::RetireOutcome, pos_cloud::lease::LeaseStoreError> {
        let leases = self.leases.lock().expect("lock");
        let in_flight = self.superseded.lock().expect("lock");
        let mut retired = self.retired.lock().expect("lock");
        let Some(generation) = leases.get(&(tenant, store)).copied() else {
            return Ok(pos_cloud::lease::RetireOutcome::NoLease);
        };
        if let Some(superseded) = in_flight.get(&(tenant, store)).copied() {
            return Ok(pos_cloud::lease::RetireOutcome::Undrained {
                superseded: pos_core::lease::LeaseGeneration::new(superseded),
            });
        }
        if let Some((at, by)) = retired.get(&(tenant, store)).cloned() {
            return Ok(pos_cloud::lease::RetireOutcome::AlreadyRetired {
                at: Timestamp::from_milliseconds_since_epoch(at).expect("a stored instant"),
                by,
            });
        }
        // Generation 0 is the store's first lease and supersedes nobody, so there is no previous
        // machine to call unnecessary — the same fact the fleet read reports as no handover state.
        if generation == 0 {
            return Ok(pos_cloud::lease::RetireOutcome::NeverSuperseded);
        }
        retired.insert(
            (tenant, store),
            (
                retired_at.as_milliseconds_since_epoch(),
                retired_by.to_owned(),
            ),
        );
        Ok(pos_cloud::lease::RetireOutcome::Retired)
    }
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

    /// The publish backlog a heartbeat last reported for a store, or `None` if none ever did.
    fn recorded_outbox_depth(&self, tenant: TenantId, store: StoreId) -> Option<u64> {
        self.outbox
            .lock()
            .expect("lock")
            .get(&(tenant, store))
            .copied()
    }

    /// The print-agent standings a heartbeat last reported, or `None` if none ever did.
    fn recorded_print_agents(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> Option<Vec<PrintAgentStanding>> {
        self.print_agents
            .lock()
            .expect("lock")
            .get(&(tenant, store))
            .cloned()
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

    /// Puts a store-layer document in the tree, as a publish would, so a read has something to
    /// find. Only the Store layer is written, which is where the console's node publishes write.
    fn seed_store_layer(&self, tenant: TenantId, store: StoreId, layer: serde_json::Value) {
        let version = self.mint();
        let state = ConfigTreeState {
            layers: [
                serde_json::json!({}),
                serde_json::json!({}),
                layer,
                serde_json::json!({}),
            ],
            history: Vec::new(),
            k: 8,
        };
        self.rows
            .lock()
            .expect("lock")
            .insert((tenant, store), Versioned::new(state, version));
    }
}

impl RegionAcknowledgementStore for FakeConfigTrees {
    /// Overwrites, as the adapter's `ON CONFLICT … DO UPDATE` does — a fake that appended would
    /// pass tests the real table could not.
    async fn record(
        &self,
        tenant: TenantId,
        store: StoreId,
        acknowledgement: &RegionAcknowledgement,
    ) -> Result<(), RegionAcknowledgementError> {
        self.acknowledgements
            .lock()
            .expect("lock")
            .insert((tenant, store), acknowledgement.clone());
        Ok(())
    }

    async fn current(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> Result<Option<RegionAcknowledgement>, RegionAcknowledgementError> {
        Ok(self
            .acknowledgements
            .lock()
            .expect("lock")
            .get(&(tenant, store))
            .cloned())
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
        outbox_depth: Option<u64>,
        _lease_generation: Option<u64>,
        print_agents: Option<Vec<PrintAgentStanding>>,
    ) -> Result<(), ConfigStoreError> {
        // A heartbeat advances last_seen only, keeping any held version a prior pull recorded.
        self.seen
            .lock()
            .expect("lock")
            .entry((tenant, store))
            .and_modify(|entry| entry.1 = seen_at.as_milliseconds_since_epoch())
            .or_insert((None, seen_at.as_milliseconds_since_epoch()));
        // A depth the store did not report leaves the last one it did alone — the adapter's
        // `COALESCE`, so a test that passes here is not passing on a laxer store.
        if let Some(depth) = outbox_depth {
            self.outbox
                .lock()
                .expect("lock")
                .insert((tenant, store), depth);
        }
        if let Some(agents) = print_agents {
            self.print_agents
                .lock()
                .expect("lock")
                .insert((tenant, store), agents);
        }
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

/// A POST for `uri` with a JSON body, a `Cookie` header, and an `If-Match` naming a lease
/// generation — or `*` for a store that has never been issued one.
///
/// The lease bump is conditional (ADR-0110, ADR-0094), so a test that omits the header is testing
/// the refusal rather than the route.
fn bump_with_if_match(body: &serde_json::Value, cookie: &str, if_match: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/admin/config/lease/bump")
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .header("if-match", if_match)
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

/// Reads a fake store's whole log back, for asserting on what was actually written.
async fn read_back(store: &FakeStore) -> Vec<EventEnvelope<RawPayload>> {
    let page = core::num::NonZeroU32::new(64).expect("positive");
    EventStore::read(store, &EventQuery::first(store_id(), page))
        .await
        .expect("read back")
}

/// A registry that answers for one store and nothing else (ADR-0101).
#[derive(Debug)]
struct FakeOwners {
    store: StoreId,
    owner: StoreOwner,
}

impl StoreOwners for FakeOwners {
    fn owner_of(
        &self,
        store_id: StoreId,
    ) -> pos_ports::dynamic::BoxFuture<'_, Result<Option<StoreOwner>, PortError>> {
        let answer = (store_id == self.store).then_some(self.owner);
        Box::pin(async move { Ok(answer) })
    }
}

#[tokio::test]
async fn an_ingested_event_is_filed_under_the_registrys_tenant_not_the_one_it_claimed() {
    // production-readiness S2 / ADR-0101. Every store in the fleet stamped `ULID(1)` for tenant and
    // brand, so the column row-level isolation is defined on held one constant fleet-wide. The stamp
    // is what makes it mean something.
    let real_tenant = TenantId::new(Ulid::from_u128(0x7E_4A_47));
    let real_brand = BrandId::new(Ulid::from_u128(0xB_4A_4D));
    let cloud = Cloud::with_store_owners(
        FakeStore::new(),
        Arc::new(FakeOwners {
            store: store_id(),
            owner: StoreOwner {
                tenant_id: real_tenant,
                brand_id: real_brand,
            },
        }),
    );

    let events = fixtures::activations(store_id(), 1, 2);
    assert_ne!(
        events[0].tenant_id, real_tenant,
        "the fixture claims something else, which is the point"
    );

    cloud.ingest(&events).await.expect("ingest");

    let stored = read_back(cloud.store()).await;
    assert_eq!(stored.len(), 2);
    for event in &stored {
        assert_eq!(event.tenant_id, real_tenant);
        assert_eq!(event.brand_id, real_brand);
    }
}

#[tokio::test]
async fn an_event_from_a_store_the_registry_does_not_know_is_kept_rather_than_lost() {
    // A provisioned store has a registry row by construction, so this is a diagnostic, not a path.
    // Refusing would stall the fleet's ingest behind one lookup; dropping would turn a provisioning
    // bug into lost trading history. It keeps its own claim, and the warning names the store.
    let stranger = StoreId::new(Ulid::from_u128(0x5_7A_46));
    let cloud = Cloud::with_store_owners(
        FakeStore::new(),
        Arc::new(FakeOwners {
            store: stranger,
            owner: StoreOwner {
                tenant_id: TenantId::new(Ulid::from_u128(1)),
                brand_id: BrandId::new(Ulid::from_u128(1)),
            },
        }),
    );

    let events = fixtures::activations(store_id(), 1, 1);
    let claimed = events[0].tenant_id;
    let outcome = cloud.ingest(&events).await.expect("ingest");

    assert_eq!(outcome.appended, 1, "the event is stored, not dropped");
    let stored = read_back(cloud.store()).await;
    assert_eq!(stored.first().map(|event| event.tenant_id), Some(claimed));
}

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
    // A valid key, but granted only PlaceOrders — it may not read rollups.
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
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

// --- Fees by code (`GET /admin/stores/{id}/revenue/fees/export`, ADR-0159 decision 4) -----------

#[tokio::test]
async fn the_fees_export_is_a_row_per_day_per_code_and_needs_the_revenue_permission() {
    // Seeded directly, in the stored blob's own shape, as the roster's read is: what is under test
    // is the route. The fold that fills `by_fee` has its own tests, and so has its
    // materialised-equals-a-re-scan property.
    let day = |date: &str, by_fee: serde_json::Value| {
        serde_json::json!({
            "business_date": date, "currency_code": "VND", "bills": 4, "gross": 400_000,
            "reductions": 0, "service_charge": 20_000, "tax": 32_000, "net": 452_000,
            "by_item": {}, "by_fee": by_fee,
        })
    };
    let seeded: StoredRollups =
        serde_json::from_value(serde_json::json!({ "days": {}, "revenue": {
            "2026-03-15": day("2026-03-15", serde_json::json!({
                "SVC": { "name": "Service charge", "bills": 3, "amount": 15_000, "tax": 1_200 },
                "PACK": { "name": "Packaging", "bills": 1, "amount": 5_000, "tax": 0 },
            })),
            "2026-03-16": day("2026-03-16", serde_json::json!({})),
            "2026-03-17": day("2026-03-17", serde_json::json!({
                "SVC": { "name": "Phí phục vụ", "bills": 2, "amount": 10_000, "tax": 800 },
            })),
        }}))
        .expect("a stored rollup");
    let rollups = FakeRollups::default();
    rollups
        .save(tenant(), store_id(), &seeded)
        .await
        .expect("seed the revenue");
    let audit = FakeAudit::default();
    let admin = provisioned_admin();
    let app = app_with_admin(
        Cloud::new(FakeStore::new()),
        rollups,
        FakeKeys::default(),
        admin.clone(),
    );
    let router = http::router(app.with_audit(Arc::new(AuditSink::new(audit.clone()))));
    let uri = format!(
        "/admin/stores/{}/revenue/fees/export?tenant_id={}&from=2026-03-15&to=2026-03-17",
        store_id().as_ulid(),
        tenant().as_ulid()
    );

    // A fee's takings are prices, so T2: a role that may read the console but not revenue is
    // refused, as the revenue export refuses it.
    for (role, token) in [
        (AdminRole::Viewer, "viewer-fees"),
        (AdminRole::Ops, "ops-fees"),
    ] {
        let cookie = role_session_cookie(&admin, role, token).await;
        let refused = router
            .clone()
            .oneshot(get_with_cookie(&uri, &cookie))
            .await
            .expect("route the export");
        assert_eq!(refused.status(), StatusCode::FORBIDDEN, "{role:?}");
    }

    let cookie = role_session_cookie(&admin, AdminRole::Admin, "admin-fees").await;
    let exported = router
        .oneshot(get_with_cookie(&uri, &cookie))
        .await
        .expect("route the export");
    assert_eq!(exported.status(), StatusCode::OK);
    let headers = exported.headers();
    assert_eq!(
        [&headers["content-type"], &headers["content-disposition"]],
        [
            "text/csv; charset=utf-8",
            "attachment; filename=\"revenue-fees.csv\""
        ]
    );
    // One row per day per code, oldest day first and then by code; the day with no fee has none.
    assert_eq!(
        text_body(exported).await,
        "business_date,currency_code,fee_code,fee_name,bills,amount,tax\n\
         2026-03-15,VND,PACK,Packaging,1,5000,0\n\
         2026-03-15,VND,SVC,Service charge,3,15000,1200\n\
         2026-03-17,VND,SVC,Phí phục vụ,2,10000,800\n"
    );

    // Audited by the rows it exported, never their contents.
    let recorded = audit.list(None, 10).await.expect("list audit entries");
    let entry = recorded
        .iter()
        .find(|entry| entry.action == "reports.export_revenue_fees")
        .expect("the export was recorded");
    assert_eq!(
        (
            entry.tenant_id,
            entry.entity_id.clone(),
            entry.after.clone()
        ),
        (
            Some(tenant()),
            store_id().to_string(),
            Some(serde_json::json!({ "domain": "revenue_fees", "rows": 3 }))
        )
    );
}

// --- Tax by component (`GET /admin/stores/{id}/revenue/tax/export`, ADR-0168 decision 5) --------

/// A day of a store's revenue in the stored blob's own shape, with `by_tax_component` where the day
/// recorded any. What is under test is the route; the fold that fills it has its own tests.
fn taxed_day(
    date: &str,
    currency: &str,
    components: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut day = serde_json::json!({
        "business_date": date, "currency_code": currency, "bills": 2, "gross": 200_000,
        "reductions": 0, "service_charge": 0, "tax": 10_000, "net": 210_000,
        "by_item": {}, "by_fee": {},
    });
    if let Some(components) = components {
        day["by_tax_component"] = components;
    }
    day
}

/// The router over a rollup store seeded with `revenue`, a store's days by date, and the admin and
/// the audit recorder it answers with.
async fn tax_export_app(revenue: serde_json::Value) -> (axum::Router, FakeAdmin, FakeAudit) {
    let seeded: StoredRollups =
        serde_json::from_value(serde_json::json!({ "days": {}, "revenue": revenue }))
            .expect("a stored rollup");
    let rollups = FakeRollups::default();
    rollups
        .save(tenant(), store_id(), &seeded)
        .await
        .expect("seed the revenue");
    let audit = FakeAudit::default();
    let admin = provisioned_admin();
    let app = app_with_admin(
        Cloud::new(FakeStore::new()),
        rollups,
        FakeKeys::default(),
        admin.clone(),
    );
    let router = http::router(app.with_audit(Arc::new(AuditSink::new(audit.clone()))));
    (router, admin, audit)
}

/// The tax export of [`store_id`] from 2026-03-15 to 2026-03-17.
fn tax_export_uri() -> String {
    format!(
        "/admin/stores/{}/revenue/tax/export?tenant_id={}&from=2026-03-15&to=2026-03-17",
        store_id().as_ulid(),
        tenant().as_ulid()
    )
}

/// What the audit trail recorded of the tax exports: the tenant, the store and the entry's value.
async fn tax_exports_audited(
    audit: &FakeAudit,
) -> Vec<(Option<TenantId>, String, Option<serde_json::Value>)> {
    let recorded = audit.list(None, 10).await.expect("list audit entries");
    recorded
        .iter()
        .filter(|entry| entry.action == "reports.export_revenue_tax")
        .map(|entry| {
            (
                entry.tenant_id,
                entry.entity_id.clone(),
                entry.after.clone(),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_tax_export_is_a_row_per_day_per_component_and_rate_and_needs_the_revenue_permission() {
    let gst = |tax: i64| {
        Some(serde_json::json!([
            { "component_name": "CGST", "rate_basis_points": 250, "tax": tax },
            { "component_name": "SGST", "rate_basis_points": 250, "tax": tax },
        ]))
    };
    let (router, admin, audit) = tax_export_app(serde_json::json!({
        // Outside the window, on either side of it.
        "2026-03-14": taxed_day("2026-03-14", "INR", gst(9)),
        "2026-03-15": taxed_day("2026-03-15", "INR", Some(serde_json::json!([
            { "component_name": "CGST", "rate_basis_points": 250, "tax": 2_500 },
            { "component_name": "CGST", "rate_basis_points": 900, "tax": 900 },
            { "component_name": "SGST", "rate_basis_points": 250, "tax": 2_500 },
            { "component_name": "SGST", "rate_basis_points": 900, "tax": 900 },
        ]))),
        // A day stored before components, which has none.
        "2026-03-16": taxed_day("2026-03-16", "INR", None),
        "2026-03-17": taxed_day("2026-03-17", "INR", gst(5_000)),
        "2026-03-18": taxed_day("2026-03-18", "INR", gst(9)),
    }))
    .await;

    // The tax on prices is T2, as the prices are.
    for (role, token) in [
        (AdminRole::Viewer, "viewer-tax"),
        (AdminRole::Ops, "ops-tax"),
    ] {
        let cookie = role_session_cookie(&admin, role, token).await;
        let refused = router
            .clone()
            .oneshot(get_with_cookie(&tax_export_uri(), &cookie))
            .await
            .expect("route the export");
        assert_eq!(refused.status(), StatusCode::FORBIDDEN, "{role:?}");
    }

    let cookie = role_session_cookie(&admin, AdminRole::Admin, "admin-tax").await;
    let exported = router
        .oneshot(get_with_cookie(&tax_export_uri(), &cookie))
        .await
        .expect("route the export");
    assert_eq!(exported.status(), StatusCode::OK);
    let headers = exported.headers();
    assert_eq!(
        [&headers["content-type"], &headers["content-disposition"]],
        [
            "text/csv; charset=utf-8",
            "attachment; filename=\"revenue-tax.csv\""
        ]
    );
    // One row per day per name and rate, oldest day first and then by name and rate, in the
    // window only; the day with none has no row.
    assert_eq!(
        text_body(exported).await,
        "business_date,currency_code,component_name,rate_basis_points,tax\n\
         2026-03-15,INR,CGST,250,2500\n\
         2026-03-15,INR,CGST,900,900\n\
         2026-03-15,INR,SGST,250,2500\n\
         2026-03-15,INR,SGST,900,900\n\
         2026-03-17,INR,CGST,250,5000\n\
         2026-03-17,INR,SGST,250,5000\n"
    );
    // Audited by the rows it held, never their contents.
    assert_eq!(
        tax_exports_audited(&audit).await,
        [(
            Some(tenant()),
            store_id().to_string(),
            Some(serde_json::json!({ "domain": "revenue_tax", "rows": 6 }))
        )]
    );
}

#[tokio::test]
async fn a_store_whose_tax_table_names_no_components_exports_only_the_tax_header() {
    // A Vietnamese store's days, stored with no such key, as every one is.
    let (router, admin, audit) = tax_export_app(serde_json::json!({
        "2026-03-15": taxed_day("2026-03-15", "VND", None),
        "2026-03-16": taxed_day("2026-03-16", "VND", None),
    }))
    .await;
    let cookie = role_session_cookie(&admin, AdminRole::Admin, "admin-tax").await;
    let exported = router
        .oneshot(get_with_cookie(&tax_export_uri(), &cookie))
        .await
        .expect("route the export");
    assert_eq!(exported.status(), StatusCode::OK);
    assert_eq!(
        text_body(exported).await,
        "business_date,currency_code,component_name,rate_basis_points,tax\n"
    );
    assert_eq!(
        tax_exports_audited(&audit).await,
        [(
            Some(tenant()),
            store_id().to_string(),
            Some(serde_json::json!({ "domain": "revenue_tax", "rows": 0 }))
        )]
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
                FakeKeys::default(),
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
        FakeKeys::default(),
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
    let router = http::ota_report_router(
        FakeOtaReports::default(),
        clock(),
        FakeKeys::default(),
        None,
    );
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
    /// Recorded at approval, not at discovery (ADR-0100).
    connection: Option<DeviceConnection>,
    station_id: Option<StationId>,
    /// Picked in the console, never discovered (ADR-0112). `None` — the ordinary case — means the
    /// edge opens the address itself.
    agent_device_id: Option<DeviceProposalId>,
    /// Ticked in the console, never discovered (ADR-0165). `false` until somebody does.
    drawer_attached: bool,
    /// Chosen in the console, never discovered (ADR-0160). `None` until somebody does.
    paper_width: Option<PaperWidth>,
    cuts_paper: Option<bool>,
    /// A till's own receipts, chosen in the console (ADR-0160 decision 4). `None` is the store's.
    receipt_printer_id: Option<DeviceProposalId>,
    receipt_language: Option<ReceiptLanguage>,
    receipt_second_language: Option<ReceiptSecondLanguage>,
    /// A till's own float, set in the console (ADR-0167 decision 4). `None` is the store's.
    opening_float_minor: Option<i64>,
    status: DeviceProposalStatus,
    /// This row's version, as the adapter's `xmin::text` is: a token, not a number a caller may
    /// reason about. Bumped by every write that changes the row.
    version: Version,
}

/// The device-proposal store as a flat list, exactly as the real table reads.
#[derive(Clone, Default)]
struct FakeDevices {
    rows: Arc<Mutex<Vec<DeviceRow>>>,
    next_version: Arc<Mutex<u64>>,
}

impl FakeDevices {
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }

    /// Applies `change` to the tenant's **approved** row `id` if it is still at `expected`, as the
    /// adapter's conditional `UPDATE … WHERE xmin::text = $n` does.
    fn write_approved(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        expected: &str,
        change: impl FnOnce(&mut DeviceRow),
    ) -> DeviceWriteOutcome {
        let mut rows = self.rows.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|row| {
            row.tenant == tenant && row.id == id && row.status == DeviceProposalStatus::Approved
        }) else {
            return DeviceWriteOutcome::NotFound;
        };
        if row.version.as_str() != expected {
            return DeviceWriteOutcome::VersionMismatch;
        }
        change(row);
        row.version = self.mint();
        DeviceWriteOutcome::Updated
    }
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
            // Pending genuinely does not know these: discovery cannot find how a printer is attached
            // or which station it serves (ADR-0100). Approval fills them in.
            connection: None,
            station_id: None,
            agent_device_id: None,
            drawer_attached: false,
            paper_width: None,
            cuts_paper: None,
            receipt_printer_id: None,
            receipt_language: None,
            receipt_second_language: None,
            opening_float_minor: None,
            status: DeviceProposalStatus::Pending,
            version: self.mint(),
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
                connection: row.connection.map(|kind| kind.short_name().to_owned()),
                station_id: row.station_id.map(|id| id.to_string()),
                agent_device_id: row.agent_device_id.map(|id| id.to_string()),
                drawer_attached: row.drawer_attached,
                paper_width: row.paper_width.map(|paper| paper.as_wire().to_owned()),
                cuts_paper: row.cuts_paper,
                receipt_printer_id: row.receipt_printer_id.map(|id| id.to_string()),
                receipt_language: row
                    .receipt_language
                    .map(|language| language.as_wire().to_owned()),
                receipt_second_language: row
                    .receipt_second_language
                    .map(|language| language.as_wire().to_owned()),
                opening_float_minor: row.opening_float_minor,
                status: row.status.as_wire().to_owned(),
                version: row.version.to_string(),
            })
            .collect())
    }

    async fn resolve(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        approved: bool,
        connection: Option<DeviceConnection>,
        station: Option<StationId>,
    ) -> Result<bool, DeviceProposalError> {
        let mut rows = self.rows.lock().expect("lock");
        for row in rows.iter_mut() {
            if row.tenant == tenant && row.id == id && row.status == DeviceProposalStatus::Pending {
                row.status = if approved {
                    DeviceProposalStatus::Approved
                } else {
                    DeviceProposalStatus::Rejected
                };
                row.connection = connection;
                row.station_id = station;
                row.version = self.mint();
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn create_terminal(
        &self,
        proposal: &PersistedDeviceProposal,
    ) -> Result<(), DeviceProposalError> {
        self.rows.lock().expect("lock").push(DeviceRow {
            id: proposal.id,
            tenant: proposal.tenant_id,
            store: proposal.store_id,
            kind: proposal.kind,
            name: proposal.name.clone(),
            address: proposal.address.clone(),
            // A terminal has neither: nothing dials it, and it serves no station — it is the thing
            // that *writes* to a printer, not a thing a fired line routes to.
            connection: None,
            station_id: None,
            agent_device_id: None,
            drawer_attached: false,
            paper_width: None,
            cuts_paper: None,
            receipt_printer_id: None,
            receipt_language: None,
            receipt_second_language: None,
            opening_float_minor: None,
            // Created resolved. The console write by a named admin *is* the decision, because
            // nothing on a LAN announces itself as a till for anyone to approve (ADR-0112).
            status: DeviceProposalStatus::Approved,
            version: self.mint(),
        });
        Ok(())
    }

    async fn set_agent(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        agent: Option<DeviceProposalId>,
        expected: &str,
    ) -> Result<DeviceWriteOutcome, DeviceProposalError> {
        Ok(self.write_approved(tenant, id, expected, |row| {
            row.agent_device_id = agent;
        }))
    }

    async fn set_drawer(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        drawer_attached: bool,
        expected: &str,
    ) -> Result<DeviceWriteOutcome, DeviceProposalError> {
        Ok(self.write_approved(tenant, id, expected, |row| {
            row.drawer_attached = drawer_attached;
        }))
    }

    async fn set_paper(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        paper_width: PaperWidth,
        cuts_paper: bool,
        expected: &str,
    ) -> Result<DeviceWriteOutcome, DeviceProposalError> {
        Ok(self.write_approved(tenant, id, expected, |row| {
            row.paper_width = Some(paper_width);
            row.cuts_paper = Some(cuts_paper);
        }))
    }

    async fn set_receipt(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        printer: Option<DeviceProposalId>,
        language: Option<ReceiptLanguage>,
        second_language: Option<ReceiptSecondLanguage>,
        expected: &str,
    ) -> Result<DeviceWriteOutcome, DeviceProposalError> {
        Ok(self.write_approved(tenant, id, expected, |row| {
            row.receipt_printer_id = printer;
            row.receipt_language = language;
            row.receipt_second_language = second_language;
        }))
    }

    async fn set_float(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        opening_float_minor: Option<i64>,
        expected: &str,
    ) -> Result<DeviceWriteOutcome, DeviceProposalError> {
        Ok(self.write_approved(tenant, id, expected, |row| {
            row.opening_float_minor = opening_float_minor;
        }))
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

/// The propose+approve surface and the publish surface over one device store and one config tree —
/// production's two `merge`s, in a test.
fn device_publish_app(keys: &FakeKeys, trees: FakeConfigTrees) -> axum::Router {
    device_publish_app_with(
        keys,
        trees,
        FakeRollups::default(),
        Arc::new(NoopAuditRecorder),
    )
}

/// [`device_publish_app`], reading its reports from `rollups` and recording to `audit`.
fn device_publish_app_with(
    keys: &FakeKeys,
    trees: FakeConfigTrees,
    rollups: FakeRollups,
    audit: Arc<dyn AuditRecorder>,
) -> axum::Router {
    let devices = FakeDevices::default();
    let admin = provisioned_admin();
    let app = app_all(
        Cloud::new(FakeStore::new()),
        rollups,
        keys.clone(),
        admin.clone(),
        trees.clone(),
        FakeWebhooks::default(),
    );
    http::router(app)
        .merge(http::device_router(
            devices.clone(),
            admin.clone(),
            keys.clone(),
            clock(),
            audit.clone(),
        ))
        .merge(http::device_publish_router(
            devices,
            trees,
            admin,
            clock(),
            audit,
        ))
}

/// Proposes one discovered printer and returns its id.
async fn propose_printer(
    router: &axum::Router,
    token: &str,
    store_ulid: &str,
    name: &str,
    address: &str,
) -> String {
    let proposal: serde_json::Value =
        serde_json::json!({ "kind": "printer", "name": name, "address": address });
    let created = router
        .clone()
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{store_ulid}/devices"),
            &proposal,
            token,
        ))
        .await
        .expect("route the propose");
    json_body(created).await["id"]
        .as_str()
        .expect("an id")
        .to_owned()
}

/// Creates a terminal in the console and returns its device id.
async fn create_terminal(
    router: &axum::Router,
    cookie: &str,
    tenant_ulid: &str,
    store_ulid: &str,
    name: &str,
) -> String {
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/devices/terminals",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "name": name,
            }),
            cookie,
        ))
        .await
        .expect("route the terminal create");
    assert_eq!(created.status(), StatusCode::CREATED);
    let body = json_body(created).await;
    assert_eq!(
        body["status"], "approved",
        "a terminal is created resolved: the console write is the decision"
    );
    body["id"].as_str().expect("an id").to_owned()
}

/// The version an approved device is currently at, read the way the console reads it.
async fn approved_device_version(
    router: &axum::Router,
    token: &str,
    store_ulid: &str,
    id: &str,
) -> String {
    let listed = router
        .clone()
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/devices"),
            Some(token),
        ))
        .await
        .expect("route the approved list");
    let rows = json_body(listed).await;
    rows.as_array()
        .expect("an array")
        .iter()
        .find(|row| row["id"] == id)
        .and_then(|row| row["version"].as_str())
        .expect("the device carries the version a conditional write needs")
        .to_owned()
}

/// Publishes a store's devices and reads one of them back out of the store's effective config,
/// which is what the store receives.
async fn published_device(
    router: &axum::Router,
    cookie: &str,
    tenant_ulid: &str,
    store_ulid: &str,
    device_id: &str,
) -> serde_json::Value {
    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/devices/publish",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let effective = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store_ulid}/config?tenant_id={tenant_ulid}"),
            cookie,
        ))
        .await
        .expect("route the config read");
    let document = json_body(effective).await;
    document["devices"]["devices"]
        .as_array()
        .expect("the devices node is an array")
        .iter()
        .find(|device| device["device_id"] == device_id)
        .expect("the device is published")
        .clone()
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
    orders_app_limited(keys, owner, generous_intake_limit())
}

/// The same intake router with a caller-chosen limiter, so a test can make the `/v1/orders` budget
/// small enough to reach (roadmap **Q5**).
fn orders_app_limited(
    keys: FakeKeys,
    owner: Option<TenantId>,
    limiter: SlidingRateLimiter,
) -> axum::Router {
    orders_router(
        FakeIntake::new(),
        keys,
        clock(),
        FakeDirectory { owner },
        limiter,
    )
}

/// A limit no ordinary test can reach, so every test that is not *about* throttling is unaffected by
/// it — the same reason the production default sits far above a real integrator's rate.
fn generous_intake_limit() -> SlidingRateLimiter {
    SlidingRateLimiter::new(1_000, 60)
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

/// The `/sync/*` budget (roadmap **Q5**), which nothing bounded before.
///
/// A wedged store — the "`403` on every poll, every five seconds" shape the fork checklist already
/// documents — cost the cloud a key lookup and a database round trip per iteration, forever, with no
/// ceiling. The layer refuses before authentication, so a hostile or broken box costs a header
/// comparison instead.
#[tokio::test]
async fn the_sync_surface_is_refused_once_the_connection_is_over_its_budget() {
    let keys = FakeKeys::default();
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);
    let state =
        app(Cloud::new(FakeStore::new()), FakeRollups::default(), keys).with_sync_rate_limit(1, 60);
    let throttle = state.sync_throttle();
    let router = http::router(state).layer(axum::middleware::from_fn_with_state(
        throttle,
        http::throttle_sync,
    ));
    let uri = format!("/sync/stores/{}/config", store_id().as_ulid());

    // The first pull is served — whatever it answers, it is not a refusal from the limiter.
    let first = router
        .clone()
        .oneshot(get(&uri, Some(&token)))
        .await
        .expect("route the first pull");
    assert_ne!(first.status(), StatusCode::TOO_MANY_REQUESTS);

    let second = router
        .clone()
        .oneshot(get(&uri, Some(&token)))
        .await
        .expect("route the second pull");
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(second.headers().contains_key("retry-after"));

    // And the layer is a prefix check, not a global one: the console is on its own budget, so a
    // store hammering `/sync` must not lock an admin out of `/admin`.
    let console = router
        .oneshot(get("/admin/login", None))
        .await
        .expect("route the console request");
    assert_ne!(
        console.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the /sync budget must not spend the console's"
    );
}

/// The `/v1/orders` budget (roadmap **Q5**), which nothing bounded before.
///
/// One tenant could hold the intake open indefinitely: every call authenticated, resolved the store
/// through the directory and reached the relay's queue, so a marketplace stuck in a retry loop cost
/// the cloud that work per iteration and consumed the capacity every other integrator needed. A
/// limit of one makes the second call the refusal.
#[tokio::test]
async fn orders_submit_is_refused_once_the_tenant_is_over_its_budget() {
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let (known, _price) = known_menu_item();
    let router = orders_app_limited(keys, Some(tenant()), SlidingRateLimiter::new(1, 60));

    let first = router
        .clone()
        .oneshot(post_json_bearer(
            "/v1/orders",
            &order_body("throttle-1", known, None),
            &token,
        ))
        .await
        .expect("route the first submit");
    assert_eq!(first.status(), StatusCode::CREATED);

    // A *different* external reference, so this is a second real order rather than an idempotent
    // repeat — the budget is what refuses it, not the ledger.
    let second = router
        .oneshot(post_json_bearer(
            "/v1/orders",
            &order_body("throttle-2", known, None),
            &token,
        ))
        .await
        .expect("route the second submit");
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        second.headers().contains_key("retry-after"),
        "a refusal tells the caller how long to hold off, or it will just retry immediately"
    );
}

/// The look-up spends from the same budget as the submit.
///
/// It is the path a caller retries when a submit times out, so exempting it would leave the loop
/// that most needs bounding unbounded — a caller polling "did my order land?" forever.
#[tokio::test]
async fn the_order_look_up_spends_the_same_budget_as_the_submit() {
    let keys = FakeKeys::default();
    let token = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let (known, _price) = known_menu_item();
    let router = orders_app_limited(keys, Some(tenant()), SlidingRateLimiter::new(1, 60));

    let submitted = router
        .clone()
        .oneshot(post_json_bearer(
            "/v1/orders",
            &order_body("throttle-3", known, None),
            &token,
        ))
        .await
        .expect("route the submit");
    assert_eq!(submitted.status(), StatusCode::CREATED);

    let looked_up = router
        .oneshot(get(
            &format!(
                "/v1/orders?store_id={}&sales_channel=SALES_CHANNEL_API&external_reference=throttle-3",
                order_store().as_ulid()
            ),
            Some(&token),
        ))
        .await
        .expect("route the look-up");
    assert_eq!(looked_up.status(), StatusCode::TOO_MANY_REQUESTS);
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
    // A store that has turned QR ordering on, which is all it publishes (ADR-0160 decision 5).
    let trees = FakeConfigTrees::default();
    seed_versions(
        &trees,
        tenant(),
        order_store(),
        &[(NOW_MS, serde_json::json!({ "qr_ordering_enabled": true }))],
    );
    let router = qr_router(secret, FakeIntake::new(), trees, clock());
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

// --- QR ordering is one switch (ADR-0160 decision 5) -------------------------------------------

/// Gives `store` a tree whose history publishes each document on the Store layer in turn, at the
/// instant beside it, as a run of console publishes would. Each version's id is minted at its
/// instant, which is what a rollback reads to tell a version from before the switch.
fn seed_versions(
    trees: &FakeConfigTrees,
    tenant_id: TenantId,
    store: StoreId,
    versions: &[(i64, serde_json::Value)],
) {
    let mut tree = ConfigTree::new(store, CapabilityValidator);
    for (n, (at_ms, document)) in (1_u128..).zip(versions) {
        let at = u64::try_from(*at_ms).expect("an instant after the epoch");
        tree.publish(
            ConfigLevel::Store,
            document.clone(),
            ConfigVersionId::new(Ulid::from_parts(at, n)),
            Vec::new(),
        )
        .expect("a document the validator accepts");
    }
    let version = trees.mint();
    trees
        .rows
        .lock()
        .expect("lock")
        .insert((tenant_id, store), Versioned::new(tree.state(), version));
}

/// The document a store runs: its newest version's, which its edge holds and the guest intake reads.
fn running_document(
    trees: &FakeConfigTrees,
    tenant_id: TenantId,
    store: StoreId,
) -> serde_json::Value {
    trees
        .rows
        .lock()
        .expect("lock")
        .get(&(tenant_id, store))
        .and_then(|row| {
            row.record
                .history
                .last()
                .map(|version| version.effective.clone())
        })
        .unwrap_or(serde_json::Value::Null)
}

/// How many versions a store's tree holds.
fn version_count(trees: &FakeConfigTrees, tenant_id: TenantId, store: StoreId) -> usize {
    trees
        .rows
        .lock()
        .expect("lock")
        .get(&(tenant_id, store))
        .map_or(0, |row| row.record.history.len())
}

/// A guest's order is refused until the store's published configuration turns the switch on.
/// `qr.enabled`, which the intake read before, decides nothing now: a store that publishes it alone
/// has not turned QR ordering on.
#[tokio::test]
async fn a_guest_qr_order_is_refused_until_the_store_turns_its_switch_on() {
    let secret = TableTokenSecret::new("qr-endpoint-secret");
    let token = mint_table_token(&secret, tenant(), order_store(), qr_table());
    let (known, _price) = known_menu_item();

    let refused = |router: axum::Router, reference: &'static str| {
        let body = qr_body(&token, reference, known);
        async move {
            let response = router
                .oneshot(post_json("/v1/qr/orders", &body))
                .await
                .expect("route the QR order");
            (response.status(), json_body(response).await)
        }
    };

    // Nothing published: the switch's default, which is off.
    let (status, body) = refused(
        qr_router(secret.clone(), FakeIntake::new(), EmptyConfigTrees, clock()),
        "qr-none",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(
        body["error"]["message"], "QR ordering is not enabled for this store",
        "{body}"
    );

    for (reference, document) in [
        (
            "qr-node-only",
            serde_json::json!({ "qr": { "enabled": true } }),
        ),
        (
            "qr-switched-off",
            serde_json::json!({ "qr_ordering_enabled": false }),
        ),
    ] {
        let trees = FakeConfigTrees::default();
        seed_versions(
            &trees,
            tenant(),
            order_store(),
            &[(NOW_MS, document.clone())],
        );
        let (status, body) = refused(
            qr_router(secret.clone(), FakeIntake::new(), trees, clock()),
            reference,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{document}: {body}");
    }
}

/// The capability and channel publishes over one tree, so a test can turn the switch and see what
/// follows it.
fn qr_switch_app(admin: FakeAdmin, trees: FakeConfigTrees) -> axum::Router {
    config_capabilities_app(admin.clone(), trees.clone()).merge(http::config_channels_router(
        trees,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// The Store layer of [`store_id`]'s tree, as the last publish left it.
fn qr_store_layer(trees: &FakeConfigTrees) -> serde_json::Value {
    trees
        .rows
        .lock()
        .expect("lock")
        .get(&(tenant(), store_id()))
        .map_or(serde_json::Value::Null, |row| row.record.layers[2].clone())
}

/// Publishes one body to one route as the console would, and expects it accepted.
async fn publish_ok(router: &axum::Router, cookie: &str, uri: &str, mut body: serde_json::Value) {
    if let serde_json::Value::Object(map) = &mut body {
        map.insert(
            "tenant_id".to_owned(),
            serde_json::json!(tenant().as_ulid().to_string()),
        );
        map.insert(
            "store_id".to_owned(),
            serde_json::json!(store_id().as_ulid().to_string()),
        );
    }
    let response = router
        .clone()
        .oneshot(put_with_cookie(uri, &body, cookie))
        .await
        .expect("route the publish");
    let status = response.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "{uri}: {}",
        json_body(response).await
    );
}

/// Turning the switch publishes `qr.enabled` and the QR channel with it, so an edge on an earlier
/// release, which reads only the channel list, and anything still reading `qr.enabled` agree.
#[tokio::test]
async fn turning_the_qr_switch_on_and_off_carries_qr_enabled_and_the_qr_channel_with_it() {
    let admin = provisioned_admin();
    let trees = FakeConfigTrees::default();
    let router = qr_switch_app(admin, trees.clone());
    let cookie = admin_cookie(&router).await;

    // A list without the switch: the switch is off, so the list keeps QR out and `qr.enabled`,
    // which would read as on while absent, is written off.
    publish_ok(
        &router,
        &cookie,
        "/admin/config/channels",
        serde_json::json!({ "enabled": ["SALES_CHANNEL_DINE_IN"] }),
    )
    .await;
    let layer = qr_store_layer(&trees);
    assert_eq!(
        layer["channels"]["enabled"],
        serde_json::json!(["SALES_CHANNEL_DINE_IN"])
    );
    assert_eq!(layer["qr"], serde_json::json!({ "enabled": false }));
    assert!(
        layer.get("qr_ordering_enabled").is_none(),
        "the switch was not written"
    );

    publish_ok(
        &router,
        &cookie,
        "/admin/config/capabilities",
        serde_json::json!({ "flags": { "qr_ordering_enabled": true } }),
    )
    .await;
    let layer = qr_store_layer(&trees);
    assert_eq!(layer["qr_ordering_enabled"], true);
    assert_eq!(layer["qr"]["enabled"], true);
    assert_eq!(
        layer["channels"]["enabled"],
        serde_json::json!(["SALES_CHANNEL_DINE_IN", "SALES_CHANNEL_QR"]),
        "the QR channel joins the store's own list"
    );

    publish_ok(
        &router,
        &cookie,
        "/admin/config/capabilities",
        serde_json::json!({ "flags": { "qr_ordering_enabled": false } }),
    )
    .await;
    let layer = qr_store_layer(&trees);
    assert_eq!(layer["qr_ordering_enabled"], false);
    assert_eq!(layer["qr"]["enabled"], false);
    assert_eq!(
        layer["channels"]["enabled"],
        serde_json::json!(["SALES_CHANNEL_DINE_IN"])
    );
}

/// A `qr` publish without `enabled` takes the store's switch, and one with it — an older console's
/// checkbox, or a cohort copy — sets the switch, with what follows it.
#[tokio::test]
async fn a_qr_publish_takes_the_switch_and_one_naming_enabled_sets_it() {
    let admin = provisioned_admin();
    let trees = FakeConfigTrees::default();
    let router = qr_switch_app(admin, trees.clone());
    let cookie = admin_cookie(&router).await;
    let guardrails = serde_json::json!({
        "staff_confirmation_required": false,
        "per_table_limit": 5,
        "rate_window_secs": 60,
    });

    publish_ok(
        &router,
        &cookie,
        "/admin/config/channels",
        serde_json::json!({ "enabled": ["SALES_CHANNEL_TAKEAWAY"] }),
    )
    .await;
    publish_ok(&router, &cookie, "/admin/config/qr", guardrails.clone()).await;
    let layer = qr_store_layer(&trees);
    assert_eq!(
        layer["qr"],
        serde_json::json!({
            "staff_confirmation_required": false,
            "per_table_limit": 5,
            "rate_window_secs": 60,
            "enabled": false,
        }),
        "the guardrails, and `enabled` following the switch, which is off"
    );
    assert!(layer.get("qr_ordering_enabled").is_none());

    let mut named = guardrails;
    if let serde_json::Value::Object(map) = &mut named {
        map.insert("enabled".to_owned(), serde_json::json!(true));
    }
    publish_ok(&router, &cookie, "/admin/config/qr", named).await;
    let layer = qr_store_layer(&trees);
    assert_eq!(
        layer["qr_ordering_enabled"], true,
        "`enabled` is the switch"
    );
    assert_eq!(layer["qr"]["enabled"], true);
    assert_eq!(
        layer["channels"]["enabled"],
        serde_json::json!(["SALES_CHANNEL_TAKEAWAY", "SALES_CHANNEL_QR"])
    );
}

/// A channel list the console publishes has its QR channel set by the switch, and keeps what a
/// list means: an empty list takes no channel and stays empty, and a list of QR alone keeps it.
#[tokio::test]
async fn a_published_channel_list_follows_the_switch_and_keeps_what_a_list_means() {
    let admin = provisioned_admin();
    let trees = FakeConfigTrees::default();
    let router = qr_switch_app(admin, trees.clone());
    let cookie = admin_cookie(&router).await;
    let list = |trees: &FakeConfigTrees| qr_store_layer(trees)["channels"]["enabled"].clone();

    publish_ok(
        &router,
        &cookie,
        "/admin/config/capabilities",
        serde_json::json!({ "flags": { "qr_ordering_enabled": true } }),
    )
    .await;
    publish_ok(
        &router,
        &cookie,
        "/admin/config/channels",
        serde_json::json!({ "enabled": ["SALES_CHANNEL_DELIVERY"] }),
    )
    .await;
    assert_eq!(
        list(&trees),
        serde_json::json!(["SALES_CHANNEL_DELIVERY", "SALES_CHANNEL_QR"]),
        "the switch is on, so the list carries QR"
    );

    publish_ok(
        &router,
        &cookie,
        "/admin/config/channels",
        serde_json::json!({ "enabled": [] }),
    )
    .await;
    assert_eq!(
        list(&trees),
        serde_json::json!([]),
        "an empty list takes no channel"
    );

    publish_ok(
        &router,
        &cookie,
        "/admin/config/capabilities",
        serde_json::json!({ "flags": { "qr_ordering_enabled": false } }),
    )
    .await;
    assert_eq!(list(&trees), serde_json::json!([]), "and stays empty");

    publish_ok(
        &router,
        &cookie,
        "/admin/config/channels",
        serde_json::json!({ "enabled": ["SALES_CHANNEL_QR"] }),
    )
    .await;
    assert_eq!(
        list(&trees),
        serde_json::json!(["SALES_CHANNEL_QR"]),
        "taking QR out of a list of QR alone would empty it and refuse every order"
    );
}

/// The preview of a switch publish shows what follows the switch, as the publish would write it.
#[tokio::test]
async fn a_preview_of_the_qr_switch_shows_what_follows_it() {
    let admin = provisioned_admin();
    let trees = FakeConfigTrees::default();
    seed_versions(
        &trees,
        tenant(),
        store_id(),
        &[(
            NOW_MS,
            serde_json::json!({
                "qr_ordering_enabled": true,
                "channels": { "enabled": ["SALES_CHANNEL_DINE_IN", "SALES_CHANNEL_QR"] },
            }),
        )],
    );
    let router = people_preview_app(admin, FakePeople::default(), trees);
    let cookie = admin_cookie(&router).await;
    let previewed = router
        .oneshot(post_with_cookie(
            "/admin/config/preview",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id().as_ulid().to_string(),
                "node": "capabilities",
                "arguments": { "flags": { "qr_ordering_enabled": false } },
            }),
            &cookie,
        ))
        .await
        .expect("route the preview");
    assert_eq!(previewed.status(), StatusCode::OK);
    let preview = json_body(previewed).await;
    assert_eq!(
        preview["diff"],
        serde_json::json!({
            "qr_ordering_enabled": false,
            "qr": { "enabled": false },
            "channels": { "enabled": ["SALES_CHANNEL_DINE_IN"] },
        }),
        "{preview}"
    );
}

/// A tenant with one store per case, in a registry the one-time setting reads.
async fn registry_with(stores: &[(StoreId, bool)]) -> FakeRegistry {
    let registry = FakeRegistry::default();
    registry
        .create_tenant(&TenantRecord {
            tenant_id: tenant(),
            name: "Pizza 4P's".to_owned(),
            status: EntityStatus::Active,
        })
        .await
        .expect("seed the tenant");
    for (store, active) in stores {
        seed_store(&registry, tenant(), *store, *active);
    }
    registry
}

fn numbered_store(n: u128) -> StoreId {
    StoreId::new(Ulid::from_u128(0x0E5_0000 + n))
}

/// The stores of [`the_one_time_setting_gives_every_store_the_switch_it_ran_and_runs_once`], one
/// per way a store could stand before the switch.
const TOOK_QR: u128 = 1;
const INTAKE_OFF: u128 = 2;
const DEAD_FLAG: u128 = 3;
const QR_BESIDE_DINE_IN: u128 = 4;
const EMPTY_LIST: u128 = 5;
const NEVER_PUBLISHED: u128 = 6;
const ARCHIVED_WITHOUT_QR: u128 = 7;
const LAYERS_ONLY: u128 = 8;

/// A registry and trees holding those stores, each as a console from before the switch left it.
async fn a_fleet_from_before_the_switch() -> (FakeRegistry, FakeConfigTrees) {
    let registry = registry_with(&[
        (numbered_store(TOOK_QR), true),
        (numbered_store(INTAKE_OFF), true),
        (numbered_store(DEAD_FLAG), true),
        (numbered_store(QR_BESIDE_DINE_IN), true),
        (numbered_store(EMPTY_LIST), true),
        (numbered_store(NEVER_PUBLISHED), true),
        (numbered_store(ARCHIVED_WITHOUT_QR), false),
        (numbered_store(LAYERS_ONLY), true),
    ])
    .await;
    let trees = FakeConfigTrees::default();
    for (store, document) in [
        // No flag, `qr.enabled` absent and no list: the guest intake and the edge both took it.
        (TOOK_QR, serde_json::json!({ "tips_enabled": true })),
        // The guest intake refused it; nothing else needs saying, as the switch is off unset.
        (
            INTAKE_OFF,
            serde_json::json!({ "qr": { "enabled": false } }),
        ),
        // The console's capability form wrote `false` into the flag while nothing read it.
        (
            DEAD_FLAG,
            serde_json::json!({ "qr_ordering_enabled": false, "qr": { "per_table_limit": 5 } }),
        ),
        // The guest page was off, and the list still named QR beside dine-in.
        (
            QR_BESIDE_DINE_IN,
            serde_json::json!({
                "qr": { "enabled": false },
                "channels": { "enabled": ["SALES_CHANNEL_QR", "SALES_CHANNEL_DINE_IN"] },
            }),
        ),
        // A dead flag left on beside an empty list: the intake took it and the edge refused it,
        // as it will under the switch.
        (
            EMPTY_LIST,
            serde_json::json!({ "qr_ordering_enabled": true, "channels": { "enabled": [] } }),
        ),
        // The edge refused it: the list leaves QR out.
        (
            ARCHIVED_WITHOUT_QR,
            serde_json::json!({ "channels": { "enabled": ["SALES_CHANNEL_DINE_IN"] } }),
        ),
    ] {
        seed_versions(
            &trees,
            tenant(),
            numbered_store(store),
            &[(NOW_MS - 60_000, document)],
        );
    }
    // Layers and no history: nothing was ever published, so nothing reached the edge.
    trees.seed_store_layer(
        tenant(),
        numbered_store(LAYERS_ONLY),
        serde_json::json!({ "qr": { "enabled": true } }),
    );
    (registry, trees)
}

/// Every store's switch is set from what it did before the switch, by publishing a new version,
/// and a store whose switch, `qr.enabled` and channel list agree is left alone. The run is
/// recorded, and a second one publishes nothing.
#[tokio::test]
async fn the_one_time_setting_gives_every_store_the_switch_it_ran_and_runs_once() {
    let (registry, trees) = a_fleet_from_before_the_switch().await;
    let took_qr = numbered_store(TOOK_QR);
    let dead_flag = numbered_store(DEAD_FLAG);
    let qr_beside_dine_in = numbered_store(QR_BESIDE_DINE_IN);
    let archived_without_qr = numbered_store(ARCHIVED_WITHOUT_QR);
    let record = InMemoryDataMigrations::new();
    let now = Timestamp::from_milliseconds_since_epoch(NOW_MS).expect("an instant");

    let first = qr_ordering::run_once(&registry, &trees, &record, &clock())
        .await
        .expect("the run completes");
    assert_eq!(first.since, now, "the change began when the run did");
    assert_eq!(
        first.report,
        Some(qr_ordering::MigrationReport {
            published: 4,
            unchanged: 2,
            unconfigured: 2,
            failed: Vec::new(),
        })
    );
    assert_eq!(
        record.recorded_time(qr_ordering::MIGRATION).await,
        Ok(Some(now)),
        "the run is recorded"
    );

    assert_eq!(
        running_document(&trees, tenant(), took_qr),
        serde_json::json!({ "tips_enabled": true, "qr_ordering_enabled": true }),
        "a store taking QR orders with no flag keeps taking them"
    );
    assert_eq!(
        running_document(&trees, tenant(), dead_flag),
        serde_json::json!({ "qr_ordering_enabled": true, "qr": { "per_table_limit": 5 } }),
        "the flag nothing read is replaced by what the store did"
    );
    assert_eq!(
        running_document(&trees, tenant(), qr_beside_dine_in),
        serde_json::json!({
            "qr": { "enabled": false },
            "channels": { "enabled": ["SALES_CHANNEL_DINE_IN"] },
            "qr_ordering_enabled": false,
        }),
        "off, and the list follows it"
    );
    assert_eq!(
        running_document(&trees, tenant(), archived_without_qr),
        serde_json::json!({
            "channels": { "enabled": ["SALES_CHANNEL_DINE_IN"] },
            "qr_ordering_enabled": false,
            "qr": { "enabled": false },
        }),
        "an archived store is set too, and `qr.enabled` follows its switch"
    );
    for (store, versions) in [
        (INTAKE_OFF, 1),
        (EMPTY_LIST, 1),
        (NEVER_PUBLISHED, 0),
        (LAYERS_ONLY, 0),
    ] {
        assert_eq!(
            version_count(&trees, tenant(), numbered_store(store)),
            versions,
            "left alone: store {store}"
        );
    }

    let second = qr_ordering::run_once(&registry, &trees, &record, &clock())
        .await
        .expect("the second run completes");
    assert_eq!(second.since, now, "the first run's time is kept");
    assert_eq!(second.report, None, "a recorded run does not run again");
    assert_eq!(
        version_count(&trees, tenant(), took_qr),
        2,
        "nothing more is published"
    );
}

/// One store the cloud cannot publish to does not stop the others, and is reported by id. The run
/// is still recorded: run again, it would read stores published to since as stores from before the
/// switch.
#[tokio::test]
async fn a_store_the_one_time_setting_cannot_publish_to_is_reported_and_the_rest_are_set() {
    let settable = numbered_store(11);
    let unpublishable = numbered_store(12);
    let registry = registry_with(&[(settable, true), (unpublishable, true)]).await;
    let trees = FakeConfigTrees::default();
    seed_versions(
        &trees,
        tenant(),
        settable,
        &[(NOW_MS - 1_000, serde_json::json!({}))],
    );
    // A version the validator would refuse today, so any publish composed on it is refused.
    let invalid = serde_json::json!({ "pay_first_enabled": true, "tables_enabled": true });
    trees.rows.lock().expect("lock").insert(
        (tenant(), unpublishable),
        Versioned::new(
            ConfigTreeState {
                layers: [
                    serde_json::json!({}),
                    serde_json::json!({}),
                    invalid.clone(),
                    serde_json::json!({}),
                ],
                history: vec![PublishedVersion {
                    id: ConfigVersionId::new(Ulid::from_parts(1_000, 1)),
                    effective: invalid,
                    nodes: Vec::new(),
                }],
                k: 8,
            },
            Version::new("seed"),
        ),
    );
    let record = InMemoryDataMigrations::new();

    let run = qr_ordering::run_once(&registry, &trees, &record, &clock())
        .await
        .expect("the run completes");
    let report = run.report.expect("the run ran");
    assert_eq!(report.published, 1);
    assert_eq!(report.failed, vec![(tenant(), unpublishable)]);
    assert_eq!(
        running_document(&trees, tenant(), settable)["qr_ordering_enabled"],
        true
    );
    assert!(
        record
            .recorded_time(qr_ordering::MIGRATION)
            .await
            .expect("the record reads")
            .is_some(),
        "recorded, and the failed store is left to an operator"
    );
}

/// A rollback to a version from before the switch gives it the switch it ran, while a version
/// published since is restored as it was, even one that never set the switch.
#[tokio::test]
async fn a_rollback_to_a_version_from_before_the_switch_gives_it_the_switch_it_ran() {
    let since = Timestamp::from_milliseconds_since_epoch(NOW_MS).expect("an instant");
    let trees = FakeConfigTrees::default();
    seed_versions(
        &trees,
        tenant(),
        store_id(),
        &[
            // Before the switch: the dead flag written off, and the store taking QR orders.
            (
                NOW_MS - 10_000,
                serde_json::json!({ "qr_ordering_enabled": false, "qr": { "per_table_limit": 3 } }),
            ),
            // Since: a store that never set its switch, so QR ordering is off.
            (NOW_MS + 10_000, serde_json::json!({ "tips_enabled": true })),
            (
                NOW_MS + 20_000,
                serde_json::json!({ "tips_enabled": false }),
            ),
        ],
    );
    let ids: Vec<String> = trees
        .rows
        .lock()
        .expect("lock")
        .get(&(tenant(), store_id()))
        .map(|row| {
            row.record
                .history
                .iter()
                .map(|version| version.id.to_string())
                .collect()
        })
        .unwrap_or_default();
    let router = http::router(
        app_full(
            Cloud::new(FakeStore::new()),
            FakeRollups::default(),
            FakeKeys::default(),
            provisioned_admin(),
            trees.clone(),
        )
        .with_qr_switch_since(since),
    );
    let cookie = admin_cookie(&router).await;
    let base = format!("/admin/stores/{}/config", store_id().as_ulid());
    let tenant_ulid = tenant().as_ulid().to_string();
    let roll_back_to = |version: String, current: String| {
        let router = router.clone();
        let cookie = cookie.clone();
        let uri = format!("{base}/rollback?tenant_id={tenant_ulid}");
        async move {
            let response = router
                .oneshot(post_config_with_etag(
                    &uri,
                    &serde_json::json!({ "version_id": version }),
                    &cookie,
                    &current,
                ))
                .await
                .expect("route the rollback");
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await["config_version_id"]
                .as_str()
                .expect("a new version id")
                .to_owned()
        }
    };

    let restored = roll_back_to(ids[0].clone(), ids[2].clone()).await;
    assert_eq!(
        running_document(&trees, tenant(), store_id()),
        serde_json::json!({ "qr_ordering_enabled": true, "qr": { "per_table_limit": 3 } }),
        "the version from before the switch comes back with the switch it ran"
    );

    roll_back_to(ids[1].clone(), restored).await;
    assert_eq!(
        running_document(&trees, tenant(), store_id()),
        serde_json::json!({ "tips_enabled": true }),
        "a version published since is restored as it was, QR ordering off"
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
        _outbox_depth: Option<u64>,
        _lease_generation: Option<u64>,
        _print_agents: Option<Vec<PrintAgentStanding>>,
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
    let relay_token = issue_store_key(&keys, tenant(), order_store(), &[Scope::RelayOrders]);
    let (known, _price) = known_menu_item();
    let queue = FakeOrderQueue::new();
    // One wake for both halves, as `main` wires it: the submit park listens on the same instance
    // the ack leg signals. Two would each signal an audience the other is not listening to, and
    // the park would only ever resolve on its fallback timer.
    let relay_wake = SharedWake::new();
    let app = || {
        orders_router(
            OrderRelay::new(
                FakeDirectory {
                    owner: Some(tenant()),
                },
                EmptyConfigTrees,
                queue.clone(),
                clock(),
                relay_wake.clone(),
            ),
            keys.clone(),
            clock(),
            FakeDirectory {
                owner: Some(tenant()),
            },
            generous_intake_limit(),
        )
        .merge(orders_sync_router_with_cap(
            queue.clone(),
            keys.clone(),
            clock(),
            relay_wake.clone(),
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

/// The outcome a store reports for an order it accepted, as its relay client posts it.
fn accepted_ack() -> serde_json::Value {
    serde_json::json!({
        "outcome": "accepted",
        "order_id": order_store().as_ulid().to_string(),
        "created": true,
        "total": { "currency_code": "VND", "amount_minor": 150_000 },
        "repriced": false,
        "awaiting_staff_confirmation": false,
    })
}

/// The ack is what resolves a parked `submit`, not the fallback timer
/// ([ADR-0062](../../../docs/adr/0062-the-relay-wake.md)).
///
/// Time is paused, so the figures are exact rather than flaky. The park runs to the default 3000 ms
/// deadline and its fallback interval is `min(5s, wait / 2)` = **1500 ms**, so a submit that returns
/// far inside 1500 ms can only have been woken. Delete the `wake.reported(..)` call in `ack_order`
/// and this test measures 1500 ms instead — which is the whole point of asserting on the clock
/// rather than only on the status code.
#[tokio::test(start_paused = true)]
async fn an_ack_wakes_the_parked_submit_rather_than_its_fallback_timer() {
    let keys = FakeKeys::default();
    let place = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let relay_token = issue_store_key(&keys, tenant(), order_store(), &[Scope::RelayOrders]);
    let (known, _price) = known_menu_item();
    let queue = FakeOrderQueue::new();
    // One wake, shared by both halves exactly as `main` wires it.
    let relay_wake = SharedWake::new();
    let app = || {
        orders_router(
            OrderRelay::new(
                FakeDirectory {
                    owner: Some(tenant()),
                },
                EmptyConfigTrees,
                queue.clone(),
                clock(),
                relay_wake.clone(),
            ),
            keys.clone(),
            clock(),
            FakeDirectory {
                owner: Some(tenant()),
            },
            generous_intake_limit(),
        )
        .merge(orders_sync_router_with_cap(
            queue.clone(),
            keys.clone(),
            clock(),
            relay_wake.clone(),
            std::time::Duration::ZERO,
        ))
    };

    let store = order_store().to_string();
    let started = tokio::time::Instant::now();

    // The caller submits and parks. Concurrently the store collects the order and reports the
    // acceptance it decided locally — through the ack route, which signals the wake once the write
    // has committed.
    let submitting = app().oneshot(post_json_bearer(
        "/v1/orders",
        &order_body("wake-1", known, None),
        &place,
    ));
    let store_side = async {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let pulled = app()
                .oneshot(get(
                    &format!("/sync/stores/{store}/orders"),
                    Some(&relay_token),
                ))
                .await
                .expect("route pull");
            let body = json_body(pulled).await;
            let Some(queued_id) = body[0]["queued_id"].as_str().map(str::to_owned) else {
                continue;
            };
            let acked = app()
                .oneshot(post_json_bearer(
                    &format!("/sync/stores/{store}/orders/{queued_id}/ack"),
                    &accepted_ack(),
                    &relay_token,
                ))
                .await
                .expect("route ack");
            assert_eq!(acked.status(), StatusCode::NO_CONTENT);
            return;
        }
    };
    let (submitted, ()) = tokio::join!(submitting, store_side);
    let elapsed = started.elapsed();

    let submitted = submitted.expect("route submit");
    assert_eq!(
        submitted.status(),
        StatusCode::CREATED,
        "the park resolved with the store's acceptance rather than timing out"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(1500),
        "the ack woke the park; on the fallback timer alone this would be 1500 ms, not {elapsed:?}"
    );
}

/// A signal that never arrives makes the park **slow, not wrong**
/// ([ADR-0062](../../../docs/adr/0062-the-relay-wake.md)).
///
/// The row in the queue stays the only source of truth, so this writes the outcome straight into the
/// store without going through the ack route — which is what a lost, dropped or cross-process signal
/// looks like from the park's side. The submit still resolves, on the fallback interval, which is
/// the safety net `fallback_for` exists to guarantee cannot be arithmetic'd away.
#[tokio::test(start_paused = true)]
async fn a_lost_signal_still_resolves_the_park_on_the_fallback_timer() {
    let keys = FakeKeys::default();
    let place = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let (known, _price) = known_menu_item();
    let queue = FakeOrderQueue::new();
    let app = orders_router(
        OrderRelay::new(
            FakeDirectory {
                owner: Some(tenant()),
            },
            EmptyConfigTrees,
            queue.clone(),
            clock(),
            SharedWake::new(),
        ),
        keys.clone(),
        clock(),
        FakeDirectory {
            owner: Some(tenant()),
        },
        generous_intake_limit(),
    );

    let started = tokio::time::Instant::now();
    let submitting = app.oneshot(post_json_bearer(
        "/v1/orders",
        &order_body("wake-2", known, None),
        &place,
    ));
    let silent_store = async {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let pending = queue
                .pull_pending(tenant(), order_store(), 8)
                .await
                .expect("pull pending");
            if let Some(order) = pending.first() {
                let outcome: StoreOutcome =
                    serde_json::from_value(accepted_ack()).expect("an ack body");
                queue
                    .record_outcome(tenant(), order_store(), order.queued_id, &outcome)
                    .await
                    .expect("record the outcome");
                // Deliberately no `wake.reported(..)`: this is the lost-signal case.
                return;
            }
        }
    };
    let (submitted, ()) = tokio::join!(submitting, silent_store);
    let elapsed = started.elapsed();

    assert_eq!(
        submitted.expect("route submit").status(),
        StatusCode::CREATED,
        "the fallback re-read found the outcome the wake never announced"
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(1500),
        "this resolved on the 1500 ms fallback slice, not a wake — got {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(3000),
        "and well inside the 3000 ms deadline, so the caller never saw a 503 — got {elapsed:?}"
    );
}

/// An enqueue wakes the store's long-poll, so an idle store is not re-reading the queue on a timer
/// ([ADR-0062](../../../docs/adr/0062-the-relay-wake.md)).
///
/// This is the finding the ADR was written for. Under ADR-0061 this leg re-read PostgreSQL every
/// 100 ms for the whole 20-second hold — about ten queries a second per store, whether it sold
/// anything or not. The hold is now the real 20-second cap and the fallback interval is 5 s, so a
/// pull that answers far inside 5 s was woken; on the fallback alone it would answer at 5 s, and on
/// nothing at all at 20.
#[tokio::test(start_paused = true)]
async fn a_queued_order_wakes_the_stores_long_poll_rather_than_holding_to_the_cap() {
    let keys = FakeKeys::default();
    let place = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let relay_token = issue_store_key(&keys, tenant(), order_store(), &[Scope::RelayOrders]);
    let (known, _price) = known_menu_item();
    let queue = FakeOrderQueue::new();
    let relay_wake = SharedWake::new();
    let app = || {
        orders_router(
            OrderRelay::new(
                FakeDirectory {
                    owner: Some(tenant()),
                },
                EmptyConfigTrees,
                queue.clone(),
                clock(),
                relay_wake.clone(),
            ),
            keys.clone(),
            clock(),
            FakeDirectory {
                owner: Some(tenant()),
            },
            generous_intake_limit(),
        )
        .merge(orders_sync_router_with_cap(
            queue.clone(),
            keys.clone(),
            clock(),
            relay_wake.clone(),
            // The shipped hold, not the zero the other relay tests use — the point here is that the
            // pull answers long before it.
            std::time::Duration::from_secs(20),
        ))
    };

    let store = order_store().to_string();
    // The store is already holding its pull open with nothing to collect.
    let pulling = async {
        let started = tokio::time::Instant::now();
        let pulled = app()
            .oneshot(get(
                &format!("/sync/stores/{store}/orders"),
                Some(&relay_token),
            ))
            .await
            .expect("route pull");
        (pulled, started.elapsed())
    };
    // A caller submits ten milliseconds later. Nobody acks, so the submit itself parks out and 503s
    // — which is the ADR-0061 contract and not what this test is about.
    let submitting = async {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        app()
            .oneshot(post_json_bearer(
                "/v1/orders",
                &order_body("wake-3", known, None),
                &place,
            ))
            .await
            .expect("route submit")
    };
    let ((pulled, elapsed), submitted) = tokio::join!(pulling, submitting);

    assert_eq!(pulled.status(), StatusCode::OK);
    assert_eq!(
        json_body(pulled).await.as_array().map(Vec::len),
        Some(1),
        "the hold ended with the order, not with an empty answer at the cap"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "the enqueue woke the hold; on the fallback interval this would be 5 s, not {elapsed:?}"
    );
    assert_eq!(submitted.status(), StatusCode::SERVICE_UNAVAILABLE);
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
        SharedWake::new(),
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
    registry_app_over_people(admin, registry, FakePeople::default(), audit)
}

/// As [`registry_app_with_audit`], over a people store the test keeps a handle on, so it can read
/// the roles a new tenant starts with (ADR-0158).
fn registry_app_over_people(
    admin: FakeAdmin,
    registry: FakeRegistry,
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
    http::router(app).merge(http::registry_router(
        registry,
        admin,
        clock(),
        audit,
        Arc::new(RoleSeeder::new(people)),
    ))
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
    fleet_app_with_config(admin, fleet, FakeConfigTrees::default())
}

/// The same, with a config-tree store a test can publish a `locale` node into — the country the
/// single-store read compares a region against
/// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
fn fleet_app_with_config(
    admin: FakeAdmin,
    fleet: FakeFleet,
    config_trees: FakeConfigTrees,
) -> axum::Router {
    fleet_app_with_audit(admin, fleet, config_trees, Arc::new(NoopAuditRecorder))
}

/// The same again, with a real audit recorder — for the acknowledge route, whose whole point is the
/// entry it writes ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// The acknowledgement store is the same `FakeConfigTrees` handle the tree comes from, mirroring
/// production, where both are the one `PostgresConfigTrees` over one pool.
fn fleet_app_with_audit(
    admin: FakeAdmin,
    fleet: FakeFleet,
    config_trees: FakeConfigTrees,
    audit: Arc<dyn AuditRecorder>,
) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::fleet_router(
        fleet,
        admin,
        clock(),
        config_trees.clone(),
        config_trees,
        audit,
    ))
}

/// One fleet row in a chosen handover state, so the four wire answers can be asserted side by side.
fn store_in_handover(
    store_id: StoreId,
    authoritative: Option<u64>,
    superseded: Option<u64>,
    retired_at: Option<Timestamp>,
) -> FleetRow {
    FleetRow {
        store_id,
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
        // Reporting the authoritative generation, empty, from one beat — the shape that reads as
        // settled once a handover has actually happened.
        outbox_depth: Some(0),
        outbox_reported_at: Some(seen_ago(1_000)),
        lease_generation_held: authoritative,
        lease_reported_at: Some(seen_ago(1_000)),
        lease_generation_authoritative: authoritative,
        edge_placement: StorePlacement::NeverIssued,
        region: None,
        superseded_generation: superseded,
        retired_at,
        retired_by: retired_at.map(|_| "01ADMINADMINADMINADMINADMI".to_owned()),
        print_agents: None,
        print_agents_reported_at: None,
    }
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

/// Creates a tenant through the route, the way the console does, and returns its id.
async fn create_tenant(router: &axum::Router, cookie: &str, body: &serde_json::Value) -> TenantId {
    let created = router
        .clone()
        .oneshot(post_with_cookie("/admin/tenants", body, cookie))
        .await
        .expect("route create tenant");
    assert_eq!(created.status(), StatusCode::CREATED);
    json_body(created).await["tenant_id"]
        .as_str()
        .expect("a tenant id")
        .parse::<Ulid>()
        .map(TenantId::new)
        .expect("a ULID")
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
    assert_eq!(
        headers
            .get("strict-transport-security")
            .expect("strict-transport-security header")
            .to_str()
            .expect("ascii"),
        "max-age=31536000; includeSubDomains"
    );
    assert_eq!(
        headers
            .get("permissions-policy")
            .expect("permissions-policy header")
            .to_str()
            .expect("ascii"),
        "camera=(), microphone=(), geolocation=()"
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
    courses: Arc<Mutex<Vec<Versioned<Course>>>>,
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

    async fn create_course(&self, course: &Course) -> Result<Version, CatalogStoreError> {
        let version = self.mint();
        self.courses
            .lock()
            .expect("lock")
            .push(Versioned::new(course.clone(), version.clone()));
        Ok(version)
    }

    async fn list_courses(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<Course>>, CatalogStoreError> {
        // Sorted as the real store sorts it: the order is the entity's meaning, and a fake that
        // handed courses back in insertion order would let a test pass here and fail on Postgres.
        let mut rows: Vec<Versioned<Course>> = self
            .courses
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.record.tenant_id == tenant_id)
            .cloned()
            .collect();
        rows.sort_by_key(|row| (row.record.sort, row.record.course_id));
        Ok(rows)
    }

    async fn update_course(
        &self,
        course: &Course,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let version = self.mint();
        let mut rows = self.courses.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|row| {
            row.record.course_id == course.course_id && row.record.tenant_id == course.tenant_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record = course.clone();
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
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
            .map(|(_owner, entry)| entry.clone())
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
        rows.extend(entries.iter().map(|entry| (tenant_id, entry.clone())));
        Ok(UpdateOutcome::Updated(version))
    }
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
        FakeKeys::default(),
        Some(internal_secret()),
    ))
}

/// The report router with a key store a test can issue a store token against — what the `/sync`
/// route authenticates. Returns the router and the keys, so a caller can mint the token it needs.
fn store_report_app(reports: FakeOtaReports) -> (axum::Router, FakeKeys) {
    let keys = FakeKeys::default();
    let app = app_full(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        keys.clone(),
        provisioned_admin(),
        FakeConfigTrees::default(),
    );
    let router = http::router(app).merge(http::ota_report_router(
        reports,
        clock(),
        keys.clone(),
        Some(internal_secret()),
    ));
    (router, keys)
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

/// The router with the OTA levers merged in ([ADR-0052](../../../docs/adr/0052-ota-rollout-config.md),
/// ADR-0078) — the rollout publish/halt/read and the placement publish/read.
fn ota_app(admin: FakeAdmin, config_trees: FakeConfigTrees) -> axum::Router {
    ota_app_hosting(admin, config_trees, &["1.4.0"])
}

/// The OTA levers, with `hosted` releases already in the registry.
///
/// A rollout naming a version the cloud does not host is refused ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
/// Amendment 2), so a test about *publishing* has to host its version first. Pass an empty slice to
/// exercise the refusal itself.
fn ota_app_hosting(
    admin: FakeAdmin,
    config_trees: FakeConfigTrees,
    hosted: &[&str],
) -> axum::Router {
    let app = app_full(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
    );
    let releases = FakeOtaArtifacts::default();
    for release in hosted {
        releases
            .artifacts
            .lock()
            .expect("the artifact list is not poisoned")
            .push(pos_cloud::ota::ReleaseArtifact {
                release: (*release).to_owned(),
                target: pos_cloud::ota::TargetTriple::parse(TEST_TARGET).expect("a triple"),
                size_bytes: 4,
                sha256: "00".repeat(32),
                recorded_at: Timestamp::from_milliseconds_since_epoch(1_777_000_000_000)
                    .expect("a valid timestamp"),
            });
    }
    http::router(app).merge(http::ota_config_router(
        config_trees,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
        releases,
    ))
}

/// The OTA levers with a real trail behind them, for the tests that assert what a bump *recorded*
/// rather than what it wrote. `ota_app` uses a no-op recorder, which is fine for the routes whose
/// evidence is the config tree and useless for the ones whose evidence is the audit entry.
fn ota_app_with_audit(
    admin: FakeAdmin,
    config_trees: FakeConfigTrees,
    audit: Arc<dyn AuditRecorder>,
) -> axum::Router {
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
        audit,
        FakeOtaArtifacts::default(),
    ))
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
            return Err(EmployeeStoreError::CodeInUse);
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

    async fn list_page(
        &self,
        tenant: TenantId,
        page: PageRequest,
        filter: &EmployeeListFilter,
    ) -> Result<Page<Versioned<Employee>>, EmployeeStoreError> {
        // Filter, then order, then window — the order the adapter's `WHERE … ORDER BY … LIMIT …
        // OFFSET` applies, and each step has to happen where it does. Windowing before ordering
        // would leave every page holding the rows it already held; filtering after the window would
        // make `total` count the roster instead of the match.
        let needle = filter.search.as_ref().map(|text| text.to_lowercase());
        let mut matched: Vec<Versioned<Employee>> = self
            .list(tenant)
            .await?
            .into_iter()
            .filter(|row| {
                needle.as_ref().is_none_or(|needle| {
                    row.record.name.to_lowercase().contains(needle)
                        || row.record.code.to_lowercase().contains(needle)
                })
            })
            .collect();
        // `list` already hands them back newest-first, so `Newest` is a no-op here. Every order ends
        // in the employee id, matching the adapter's tiebreaker: without it two rows sharing a name
        // could land on two pages or on neither.
        match filter.sort {
            EmployeeSort::Newest => {}
            EmployeeSort::Name => matched.sort_by(|a, b| {
                a.record
                    .name
                    .cmp(&b.record.name)
                    .then_with(|| a.record.employee_id.cmp(&b.record.employee_id))
            }),
            EmployeeSort::Code => matched.sort_by(|a, b| {
                a.record
                    .code
                    .cmp(&b.record.code)
                    .then_with(|| a.record.employee_id.cmp(&b.record.employee_id))
            }),
        }
        if filter.descending {
            matched.reverse();
        }
        let total = u32::try_from(matched.len()).unwrap_or(u32::MAX);
        let items = matched
            .into_iter()
            .skip(page.offset() as usize)
            .take(page.limit() as usize)
            .collect();
        Ok(Page::new(items, total))
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

#[tokio::test]
async fn the_employee_page_partitions_the_roster_and_every_page_reports_the_whole_headcount() {
    let store = FakeEmployees::default();
    let mine = tenant();
    let other = TenantId::new(Ulid::from_u128(0xB0B));
    for index in 1..=5_u128 {
        store
            .create(&NewEmployee {
                employee_id: EmployeeId::new(Ulid::from_u128(index)),
                tenant_id: mine,
                code: format!("A{index:02}"),
                name: format!("Person {index}"),
            })
            .await
            .expect("create in my tenant");
    }
    // One in another tenant, so a headcount that ignored the scope would read 6 instead of 5.
    store
        .create(&NewEmployee {
            employee_id: EmployeeId::new(Ulid::from_u128(0xFF)),
            tenant_id: other,
            code: "A01".to_owned(),
            name: "Elsewhere".to_owned(),
        })
        .await
        .expect("create in the other tenant");

    let roster = store.list(mine).await.expect("the unpaged roster");
    assert_eq!(roster.len(), 5, "the unpaged read is unchanged by paging");

    // Three pages of two stitch back into the roster, in order and without a repeat or a gap — the
    // property the total order exists for (ADR-0098 decision 9).
    let mut stitched = Vec::new();
    for offset in [0, 2, 4] {
        let page = store
            .list_page(
                mine,
                PageRequest::new(2, offset).expect("bounds"),
                &EmployeeListFilter::default(),
            )
            .await
            .expect("a page");
        assert_eq!(
            page.total, 5,
            "every page reports the tenant's headcount, not the window's size"
        );
        stitched.extend(page.items);
    }
    assert_eq!(
        stitched
            .iter()
            .map(|row| row.record.employee_id)
            .collect::<Vec<_>>(),
        roster
            .iter()
            .map(|row| row.record.employee_id)
            .collect::<Vec<_>>(),
        "the pages partition the roster the unpaged read returns"
    );

    // A page past the end is empty and still counts the roster — not an error, and not a zero total.
    let beyond = store
        .list_page(
            mine,
            PageRequest::new(10, 50).expect("bounds"),
            &EmployeeListFilter::default(),
        )
        .await
        .expect("a page past the end");
    assert!(beyond.items.is_empty());
    assert_eq!(beyond.total, 5);

    // No page carries a PIN hash, the same as the unpaged read: paging changes how much of the
    // roster crosses the wire, never which fields do (ADR-0070).
    let page = store
        .list_page(
            mine,
            PageRequest::new(5, 0).expect("bounds"),
            &EmployeeListFilter::default(),
        )
        .await
        .expect("the whole roster as one page");
    assert!(
        page.items.iter().all(|row| !row.record.has_pin),
        "the view carries whether a PIN exists, never the hash"
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
                late_after_seconds: station.late_after_seconds,
                ticket_language: station
                    .ticket_language
                    .map(|language| language.as_wire().to_owned()),
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
        row.record.late_after_seconds = update.late_after_seconds;
        row.record.ticket_language = update
            .ticket_language
            .map(|language| language.as_wire().to_owned());
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
#[expect(
    clippy::too_many_lines,
    reason = "one store's stations and routing rules through create, update, list and remove, \
              against the same fake"
)]
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
            late_after_seconds: Some(240),
            ticket_language: None,
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
                late_after_seconds: None,
                ticket_language: None,
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
    assert_eq!(oven_view.record.late_after_seconds, None);

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
                permissions_with_approval: template.permissions_with_approval.clone(),
                discount_ceiling_minor: template.discount_ceiling_minor,
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
        row.record
            .permissions_with_approval
            .clone_from(&template.permissions_with_approval);
        row.record.discount_ceiling_minor = template.discount_ceiling_minor;
        row.record.status = template.status;
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }
}

/// The assignment store as an in-memory list — the same seam the binary implements. An assignment is
/// removed (offboarding), not archived.
///
/// A group assignment reaches the stores `groups` holds when it is read, as the adapter's join on
/// `store_group_members` does, so a test that hands the same [`FakeStoreGroups`] to the group routes
/// ([`FakePeople::over`]) sees a membership change move who works where.
#[derive(Clone, Default)]
struct FakeAssignments {
    rows: Arc<Mutex<Vec<Assignment>>>,
    groups: FakeStoreGroups,
}

impl FakeAssignments {
    /// The rows of `tenant` that `keep` selects, in the order they were written.
    fn select(&self, tenant: TenantId, keep: impl Fn(&Assignment) -> bool) -> Vec<Assignment> {
        self.rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant && keep(row))
            .cloned()
            .collect()
    }
}

impl AssignmentStore for FakeAssignments {
    async fn assign(&self, assignment: &NewAssignment) -> Result<(), AssignmentStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        // One per person and store, per person and group, and tenant-wide: the adapter's unique
        // indexes, one per scope.
        if rows.iter().any(|row| {
            row.tenant_id == assignment.tenant_id
                && row.employee_id == assignment.employee_id
                && row.scope == assignment.scope
        }) {
            return Err(AssignmentStoreError::AlreadyAssigned(
                "the employee is already assigned there".to_owned(),
            ));
        }
        rows.push(Assignment {
            assignment_id: assignment.assignment_id,
            tenant_id: assignment.tenant_id,
            employee_id: assignment.employee_id,
            scope: assignment.scope,
            role_template_id: assignment.role_template_id,
            // Stored unresolved: this fake holds only assignments, so it has no roster to join
            // against — exactly the state the adapter's `LEFT JOIN` reports when no employee row
            // matches. `FakePeople`, which holds all three seams, is where the resolution happens.
            employee_name: None,
            employee_code: None,
        });
        Ok(())
    }

    async fn list_for_store(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        let member_of: Vec<StoreGroupId> = self
            .groups
            .members
            .lock()
            .expect("lock")
            .iter()
            .filter(|((owner, _), stores)| *owner == tenant && stores.contains(&store_id))
            .map(|((_, group_id), _)| *group_id)
            .collect();
        Ok(self.select(tenant, |row| match row.scope {
            AssignmentScope::Store(at) => at == store_id,
            AssignmentScope::StoreGroup(group_id) => member_of.contains(&group_id),
            AssignmentScope::Tenant => true,
        }))
    }

    async fn list_for_group(
        &self,
        tenant: TenantId,
        store_group_id: StoreGroupId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        Ok(self.select(tenant, |row| {
            row.scope == AssignmentScope::StoreGroup(store_group_id)
        }))
    }

    async fn list_tenant_wide(
        &self,
        tenant: TenantId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        Ok(self.select(tenant, |row| row.scope == AssignmentScope::Tenant))
    }

    async fn list_for_employee(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        Ok(self.select(tenant, |row| row.employee_id == employee_id))
    }

    async fn list_for_role(
        &self,
        tenant: TenantId,
        role_template_id: RoleTemplateId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        Ok(self.select(tenant, |row| row.role_template_id == role_template_id))
    }

    async fn get(
        &self,
        tenant: TenantId,
        assignment_id: AssignmentId,
    ) -> Result<Option<Assignment>, AssignmentStoreError> {
        Ok(self
            .select(tenant, |row| row.assignment_id == assignment_id)
            .into_iter()
            .next())
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
            permissions_with_approval: Vec::new(),
            discount_ceiling_minor: None,
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
            permissions_with_approval: Vec::new(),
            discount_ceiling_minor: None,
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
                permissions_with_approval: Vec::new(),
                discount_ceiling_minor: None,
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
                    permissions_with_approval: Vec::new(),
                    discount_ceiling_minor: None,
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

/// The in-memory assignment store holds the seam's contract, the one `PostgresPeople` is held to in
/// `tests/assignment_store_postgres.rs`: every scope, reach through a group's membership as it
/// changes, one assignment per person and place, and nothing across a tenant.
#[tokio::test]
async fn the_in_memory_assignment_store_holds_the_contract() {
    let groups = FakeStoreGroups::default();
    let assignments = FakeAssignments {
        groups: groups.clone(),
        ..FakeAssignments::default()
    };
    assignment_contract::holds(&assignments, &groups, 0).await;
}

/// The in-memory employee store holds the seam's rule for staff codes, the one `PostgresPeople` is
/// held to in `tests/employee_store_postgres.rs`: unique within a tenant, free across tenants, and a
/// second one refused as a duplicate that writes nothing.
#[tokio::test]
async fn the_in_memory_employee_store_holds_the_contract() {
    employee_contract::holds(&FakeEmployees::default(), 0).await;
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
    async fn list_page(
        &self,
        tenant: TenantId,
        page: PageRequest,
        filter: &EmployeeListFilter,
    ) -> Result<Page<Versioned<Employee>>, EmployeeStoreError> {
        self.employees.list_page(tenant, page, filter).await
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

impl FakePeople {
    /// A roster whose group assignments reach the stores `groups` holds — the store-group fake the
    /// same test's group routes write, so a membership change there moves who works where.
    fn over(groups: &FakeStoreGroups) -> Self {
        Self {
            assignments: FakeAssignments {
                groups: groups.clone(),
                ..FakeAssignments::default()
            },
            ..Self::default()
        }
    }

    /// The store groups this roster's group assignments reach through.
    fn groups(&self) -> FakeStoreGroups {
        self.assignments.groups.clone()
    }

    /// Fills in each assignment's `employee_name`/`employee_code` from the roster this fake also
    /// holds — the in-memory stand-in for the adapter's `LEFT JOIN`.
    ///
    /// It lives here and not in `FakeAssignments` for the same reason the join lives in the people
    /// module and not in a standalone assignments table: resolving the name needs both seams, and
    /// `FakePeople` is the type that has both. An employee the roster does not know stays `None`,
    /// which is what the left join yields for an assignment whose employee row is gone.
    async fn resolved(
        &self,
        tenant: TenantId,
        rows: Vec<Assignment>,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        let roster = self
            .employees
            .list(tenant)
            .await
            .map_err(|error| AssignmentStoreError::new(error.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let found = roster
                    .iter()
                    .find(|employee| employee.record.employee_id == row.employee_id);
                Assignment {
                    employee_name: found.map(|employee| employee.record.name.clone()),
                    employee_code: found.map(|employee| employee.record.code.clone()),
                    ..row
                }
            })
            .collect())
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
        let rows = self.assignments.list_for_store(tenant, store_id).await?;
        self.resolved(tenant, rows).await
    }
    async fn list_for_group(
        &self,
        tenant: TenantId,
        store_group_id: StoreGroupId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        let rows = self
            .assignments
            .list_for_group(tenant, store_group_id)
            .await?;
        self.resolved(tenant, rows).await
    }
    async fn list_tenant_wide(
        &self,
        tenant: TenantId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        let rows = self.assignments.list_tenant_wide(tenant).await?;
        self.resolved(tenant, rows).await
    }
    async fn list_for_employee(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        let rows = self
            .assignments
            .list_for_employee(tenant, employee_id)
            .await?;
        self.resolved(tenant, rows).await
    }
    async fn list_for_role(
        &self,
        tenant: TenantId,
        role_template_id: RoleTemplateId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        let rows = self
            .assignments
            .list_for_role(tenant, role_template_id)
            .await?;
        self.resolved(tenant, rows).await
    }
    async fn get(
        &self,
        tenant: TenantId,
        assignment_id: AssignmentId,
    ) -> Result<Option<Assignment>, AssignmentStoreError> {
        let Some(row) = self.assignments.get(tenant, assignment_id).await? else {
            return Ok(None);
        };
        Ok(self.resolved(tenant, vec![row]).await?.into_iter().next())
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
/// assert the audit trail (ADR-0070). The registry holds the one store these tests assign people
/// to, active, so an assignment to it passes the route's checks.
fn people_app_with_audit(
    admin: FakeAdmin,
    people: FakePeople,
    audit: Arc<dyn AuditRecorder>,
) -> axum::Router {
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant(), store_id(), true);
    people_app(admin, people, registry, FakeConfigTrees::default(), audit)
}

/// The people routes and the people publish over one roster, one registry and one config-tree fake:
/// a test seeds the stores an assignment may name, and reads back the `permissions` node that a
/// removal or an archive publishes at once
/// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decision 7).
fn people_app(
    admin: FakeAdmin,
    people: FakePeople,
    registry: FakeRegistry,
    config: FakeConfigTrees,
    audit: Arc<dyn AuditRecorder>,
) -> axum::Router {
    people_app_with_settings(
        admin,
        people,
        registry,
        config,
        FakeSettings::default(),
        audit,
    )
}

/// [`people_app`] over the tenant settings a test chooses, which the PIN route reads for the
/// fewest digits a PIN may have
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)).
fn people_app_with_settings<S>(
    admin: FakeAdmin,
    people: FakePeople,
    registry: FakeRegistry,
    config: FakeConfigTrees,
    settings: S,
    audit: Arc<dyn AuditRecorder>,
) -> axum::Router
where
    S: pos_cloud::settings::SettingsStore + Send + Sync + 'static,
{
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config.clone(),
        FakeWebhooks::default(),
    );
    http::router(app)
        .merge(http::people_router(
            people.clone(),
            registry,
            people.groups(),
            settings,
            config.clone(),
            admin.clone(),
            clock(),
            Arc::clone(&audit),
        ))
        .merge(http::people_publish_router(
            people,
            config,
            admin,
            clock(),
            audit,
        ))
}

/// Seeds one store's people directly through the seam: Alice (`C01`) with a PIN hash, a Cashier
/// role granting `billing.discount.apply`, and Alice assigned to `store` as a Cashier. Returns the
/// two ids, for the tests that address either one.
async fn seed_a_cashier(people: &FakePeople, store: StoreId) -> (EmployeeId, RoleTemplateId) {
    let mine = tenant();
    let alice = EmployeeId::new(Ulid::from_u128(0xA11));
    let cashier = RoleTemplateId::new(Ulid::from_u128(0xCA5));
    EmployeeStore::create(
        people,
        &NewEmployee {
            employee_id: alice,
            tenant_id: mine,
            code: "C01".to_owned(),
            name: "Alice".to_owned(),
        },
    )
    .await
    .expect("create employee");
    EmployeeStore::set_pin(people, mine, alice, "argon2id$phc$alice")
        .await
        .expect("set pin");
    RoleTemplateStore::create(
        people,
        &NewRoleTemplate {
            role_template_id: cashier,
            tenant_id: mine,
            name: "Cashier".to_owned(),
            permissions: vec!["billing.discount.apply".to_owned()],
            permissions_with_approval: Vec::new(),
            discount_ceiling_minor: None,
        },
    )
    .await
    .expect("create role");
    AssignmentStore::assign(
        people,
        &NewAssignment {
            assignment_id: AssignmentId::new(Ulid::from_u128(1)),
            tenant_id: mine,
            employee_id: alice,
            scope: AssignmentScope::Store(store),
            role_template_id: cashier,
        },
    )
    .await
    .expect("assign");
    (alice, cashier)
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

// --- ADR-0158 decision 7: what the console authors is what the store enforces -------------------

/// The staff a store's `permissions` node lists on its Store layer now — none without a tree.
async fn staff_at(config: &FakeConfigTrees, store: StoreId) -> Vec<serde_json::Value> {
    config
        .load(tenant(), store)
        .await
        .expect("load the tree")
        .and_then(|tree| {
            tree.record.layers[2]["permissions"]["staff"]
                .as_array()
                .cloned()
        })
        .unwrap_or_default()
}

/// How many versions a store's tree has been published at.
async fn versions_at(config: &FakeConfigTrees, store: StoreId) -> usize {
    config
        .load(tenant(), store)
        .await
        .expect("load the tree")
        .map_or(0, |tree| tree.record.history.len())
}

/// Publishes a store's people the way the console's button does.
async fn publish_people(router: &axum::Router, cookie: &str, store: StoreId) {
    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/people/publish",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store.as_ulid().to_string(),
            }),
            cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
}

/// The `(store, outcome)` pairs a write that publishes at once answers with.
async fn publish_report(response: axum::response::Response) -> Vec<(String, String)> {
    json_body(response).await["stores"]
        .as_array()
        .expect("a stores array")
        .iter()
        .map(|row| {
            (
                row["store_id"].as_str().expect("a store id").to_owned(),
                row["outcome"].as_str().expect("an outcome").to_owned(),
            )
        })
        .collect()
}

/// A store as the report names one it published to.
fn applied(store: StoreId) -> (String, String) {
    (
        store.as_ulid().to_string(),
        "PERMISSIONS_PUBLISH_APPLIED".to_owned(),
    )
}

/// A `POST /admin/assignments` body for `employee` with `role`, at the target `fields` name.
fn assignment_body(
    employee: EmployeeId,
    role: RoleTemplateId,
    fields: &[(&str, String)],
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "employee_id": employee.as_ulid().to_string(),
        "role_template_id": role.as_ulid().to_string(),
    });
    for (field, value) in fields {
        body[*field] = serde_json::json!(value);
    }
    body
}

/// Assigns through the route, as the console does.
async fn post_assignment(
    router: &axum::Router,
    cookie: &str,
    body: &serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(post_with_cookie("/admin/assignments", body, cookie))
        .await
        .expect("route the assignment")
}

// --- Remote device retirement (`POST /admin/stores/{id}/devices/revoke`, ADR-0118 §6) -----------

/// The main app merged with the device-revoke router, sharing one config-tree fake so the test can
/// read back the deny-list the publish appended to.
fn device_revoke_app(admin: FakeAdmin, config: FakeConfigTrees) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config.clone(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::device_revoke_router(
        config,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// The `revoked_devices` deny-list on a store's Store layer, as the tree currently holds it.
///
/// Empty when the store has no tree at all, which is the state a refused publish leaves it in — so
/// this answers "what is on the list" rather than asserting a publish happened.
async fn published_deny_list(
    config: &FakeConfigTrees,
    tenant: TenantId,
    store: StoreId,
) -> Vec<String> {
    let Some(state) = config
        .load(tenant, store)
        .await
        .expect("read the tree")
        .map(|held| held.record)
    else {
        return Vec::new();
    };
    state
        .layer(ConfigLevel::Store)
        .get("revoked_devices")
        .and_then(|node| node.get("device_ids"))
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
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

/// A mutual-exclusion refusal names **every** field of the set — the one case where that is right.
///
/// Every other multi-field refusal in Q3b was narrowed to name only what was wrong. This one is the
/// deliberate exception, and the distinction is worth a test because it looks like the same bug:
/// `/admin/assignments` wants exactly one of `store_id`, `employee_id`, `store_group_id` or
/// `scope_kind`, so when the caller sends none or several, *the set* is the mistake and no field
/// alone is at fault.
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
        body["error"]["message"],
        "name exactly one of store_id, employee_id, store_group_id or scope_kind",
        "got {body}"
    );
    assert_eq!(body["error"]["details"][0]["field"], "store_id");
    assert_eq!(body["error"]["details"][0]["reason"], "MUTUALLY_EXCLUSIVE");
    assert_eq!(body["error"]["details"][1]["field"], "employee_id");
    assert_eq!(body["error"]["details"][2]["field"], "store_group_id");
    assert_eq!(body["error"]["details"][3]["field"], "scope_kind");
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
    /// Which reads the routes actually made, in order.
    ///
    /// The read-one seam is invisible to every behavioural assertion in this file:
    /// `get_ingredient` and `list_ingredients().find(…)` return the same record, so the route tests
    /// passed before it existed and would pass again if a handler regressed to scanning the list.
    /// What differs is the *work* — the list read carries the tenant's whole catalogue of that kind
    /// out of the database on every read, edit and delete of one row. This log is what makes the
    /// difference assertable rather than a claim in a commit message.
    reads: Arc<Mutex<Vec<&'static str>>>,
}

impl FakeInventory {
    /// The fake's stand-in for `xmin` (ADR-0094): a token that changes on every successful write.
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }

    /// Records that a route made this read.
    fn note(&self, read: &'static str) {
        self.reads.lock().expect("lock").push(read);
    }

    /// Empties the log and returns what it held, so one test can check several routes.
    fn taken_reads(&self) -> Vec<&'static str> {
        core::mem::take(&mut *self.reads.lock().expect("lock"))
    }
}

impl InventoryStore for FakeInventory {
    async fn list_ingredients(
        &self,
        _tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedIngredient>>, InventoryStoreError> {
        self.note("list_ingredients");
        Ok(self.ingredients.lock().expect("lock").clone())
    }

    async fn get_ingredient(
        &self,
        _tenant_id: TenantId,
        ingredient_id: IngredientId,
    ) -> Result<Option<Versioned<PublishedIngredient>>, InventoryStoreError> {
        self.note("get_ingredient");
        Ok(self
            .ingredients
            .lock()
            .expect("lock")
            .iter()
            .find(|row| row.record.id == ingredient_id)
            .cloned())
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
        self.note("list_recipes");
        Ok(self.recipes.lock().expect("lock").clone())
    }

    async fn get_recipe(
        &self,
        _tenant_id: TenantId,
        item: MenuItemId,
    ) -> Result<Option<Versioned<PublishedRecipe>>, InventoryStoreError> {
        self.note("get_recipe");
        Ok(self
            .recipes
            .lock()
            .expect("lock")
            .iter()
            .find(|row| row.record.item == item)
            .cloned())
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
        self.note("list_suppliers");
        Ok(self.suppliers.lock().expect("lock").clone())
    }

    async fn get_supplier(
        &self,
        _tenant_id: TenantId,
        supplier_id: SupplierId,
    ) -> Result<Option<Versioned<PublishedSupplier>>, InventoryStoreError> {
        self.note("get_supplier");
        Ok(self
            .suppliers
            .lock()
            .expect("lock")
            .iter()
            .find(|row| row.record.id == supplier_id)
            .cloned())
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

// --- OTA artifact fetch (`POST /sync/stores/{store_id}/artifact`) --------------------------------

/// An object store held in memory, so the route can be exercised without Garage.
#[derive(Clone, Default)]
struct FakeBlobs {
    objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl FakeBlobs {
    fn put_now(&self, key: &str, body: &[u8]) {
        self.objects
            .lock()
            .expect("the blob map is not poisoned")
            .insert(key.to_owned(), body.to_vec());
    }
}

impl pos_ports::blob_store::BlobStore for FakeBlobs {
    async fn put(
        &self,
        key: &pos_ports::blob_store::BlobKey,
        body: &[u8],
    ) -> Result<(), PortError> {
        self.put_now(key.as_str(), body);
        Ok(())
    }

    async fn get(
        &self,
        key: &pos_ports::blob_store::BlobKey,
    ) -> Result<Option<Vec<u8>>, PortError> {
        Ok(self
            .objects
            .lock()
            .expect("the blob map is not poisoned")
            .get(key.as_str())
            .cloned())
    }

    async fn delete(&self, key: &pos_ports::blob_store::BlobKey) -> Result<(), PortError> {
        self.objects
            .lock()
            .expect("the blob map is not poisoned")
            .remove(key.as_str());
        Ok(())
    }

    async fn list(
        &self,
        prefix: &pos_ports::blob_store::BlobKey,
    ) -> Result<Vec<pos_ports::blob_store::BlobKey>, PortError> {
        let objects = self.objects.lock().expect("the blob map is not poisoned");
        let mut keys: Vec<_> = objects
            .keys()
            .filter(|key| key.starts_with(prefix.as_str()))
            .filter_map(|key| pos_ports::blob_store::BlobKey::parse(key).ok())
            .collect();
        keys.sort();
        Ok(keys)
    }
}

/// A release registry holding whatever a test recorded.
#[derive(Clone, Default)]
struct FakeOtaArtifacts {
    artifacts: Arc<Mutex<Vec<pos_cloud::ota::ReleaseArtifact>>>,
}

impl pos_cloud::ota::ArtifactStore for FakeOtaArtifacts {
    async fn record_artifact(
        &self,
        artifact: &pos_cloud::ota::ReleaseArtifact,
    ) -> Result<pos_cloud::ota::RecordOutcome, pos_cloud::ota::ArtifactStoreError> {
        let mut artifacts = self
            .artifacts
            .lock()
            .expect("the artifact list is not poisoned");
        let stored = artifacts
            .iter()
            .find(|held| held.release == artifact.release && held.target == artifact.target)
            .map(|held| held.sha256.clone());
        // The fake consults the same pure rule the adapter does, as the port's doc asks: otherwise
        // "re-uploading different bytes is refused" would be a claim only the SQL could break.
        let outcome = pos_cloud::ota::admit_artifact(stored.as_deref(), artifact)?;
        if outcome == pos_cloud::ota::RecordOutcome::Recorded {
            artifacts.push(artifact.clone());
        }
        Ok(outcome)
    }

    async fn find_artifact(
        &self,
        release: &str,
        target: &pos_cloud::ota::TargetTriple,
    ) -> Result<Option<pos_cloud::ota::ReleaseArtifact>, pos_cloud::ota::ArtifactStoreError> {
        Ok(self
            .artifacts
            .lock()
            .expect("the artifact list is not poisoned")
            .iter()
            .find(|artifact| artifact.release == release && &artifact.target == target)
            .cloned())
    }

    async fn list_artifacts(
        &self,
        release: &str,
    ) -> Result<Vec<pos_cloud::ota::ReleaseArtifact>, pos_cloud::ota::ArtifactStoreError> {
        Ok(self
            .artifacts
            .lock()
            .expect("the artifact list is not poisoned")
            .iter()
            .filter(|artifact| artifact.release == release)
            .cloned()
            .collect())
    }
}

/// The target every artifact test uses.
const TEST_TARGET: &str = "x86_64-unknown-linux-gnu";

/// Records a release whose bytes are `body`, and stores both blobs, so the route has something to
/// serve. Returns the router under test and the token a store presents.
fn artifact_fixture(
    body: &[u8],
    signature: &[u8],
    scopes: &[Scope],
) -> (axum::Router, String, FakeBlobs) {
    use sha2::Digest as _;
    let keys = FakeKeys::default();
    let token = issue_store_key(&keys, tenant(), store_id(), scopes);
    let target = pos_cloud::ota::TargetTriple::parse(TEST_TARGET).expect("a valid triple");
    let blobs = FakeBlobs::default();
    blobs.put_now(
        pos_cloud::ota::artifact_key("1.4.0", &target, pos_cloud::ota::ArtifactKind::Binary)
            .expect("a valid key")
            .as_str(),
        body,
    );
    blobs.put_now(
        pos_cloud::ota::artifact_key("1.4.0", &target, pos_cloud::ota::ArtifactKind::Signature)
            .expect("a valid key")
            .as_str(),
        signature,
    );
    let releases = FakeOtaArtifacts::default();
    releases
        .artifacts
        .lock()
        .expect("the artifact list is not poisoned")
        .push(pos_cloud::ota::ReleaseArtifact {
            release: "1.4.0".to_owned(),
            target,
            size_bytes: i64::try_from(body.len()).expect("a representable size"),
            sha256: Sha256::digest(body)
                .iter()
                .fold(String::new(), |mut hex, byte| {
                    use core::fmt::Write as _;
                    let _ = write!(hex, "{byte:02x}");
                    hex
                }),
            recorded_at: Timestamp::from_milliseconds_since_epoch(1_777_000_000_000)
                .expect("a representable instant"),
        });
    let router = http::ota_artifact_router(keys, clock(), blobs.clone(), releases);
    (router, token, blobs)
}

/// The body a store posts: a release tag and the architecture it runs.
fn artifact_body(release: &str, arch: &str) -> serde_json::Value {
    serde_json::json!({ "release": release, "arch": arch })
}

// --- Reason-code authoring admin routes (ADR-0115, B2.2 slice 3) --------------------------------

/// An in-memory `ReasonCodeStore` for the route tests.
///
/// Tenant-scoped and version-carrying like the real thing, because that is what the routes under
/// test depend on: a create refuses a taken id, an update applies only at the version a read handed
/// back, and the list is where the console obtains it.
/// One stored entry: which tenant authored it, and the record with the version it stands at.
type ReasonCodeRow = (TenantId, Versioned<PublishedReasonCode>);

#[derive(Default, Clone)]
struct FakeReasonCodes {
    rows: Arc<Mutex<Vec<ReasonCodeRow>>>,
    next_version: Arc<Mutex<u64>>,
}

impl FakeReasonCodes {
    /// The fake's stand-in for `xmin` (ADR-0094): a token that changes on every successful write.
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }
}

impl ReasonCodeStore for FakeReasonCodes {
    async fn list(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedReasonCode>>, ReasonCodeStoreError> {
        let rows = self.rows.lock().expect("lock");
        let mut mine: Vec<Versioned<PublishedReasonCode>> = rows
            .iter()
            .filter(|(owner, _)| *owner == tenant_id)
            .map(|(_, row)| row.clone())
            .collect();
        mine.sort_by_key(|row| row.record.id.as_ulid().to_u128());
        Ok(mine)
    }

    async fn get(
        &self,
        tenant_id: TenantId,
        reason_code_id: ReasonCodeId,
    ) -> Result<Option<Versioned<PublishedReasonCode>>, ReasonCodeStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|(owner, row)| *owner == tenant_id && row.record.id == reason_code_id)
            .map(|(_, row)| row.clone()))
    }

    async fn create(
        &self,
        tenant_id: TenantId,
        reason_code: &PublishedReasonCode,
    ) -> Result<CreateOutcome, ReasonCodeStoreError> {
        let version = self.mint();
        let mut rows = self.rows.lock().expect("lock");
        if rows
            .iter()
            .any(|(owner, row)| *owner == tenant_id && row.record.id == reason_code.id)
        {
            return Ok(CreateOutcome::AlreadyExists);
        }
        rows.push((
            tenant_id,
            Versioned::edited(reason_code.clone(), version.clone(), NOW_MS),
        ));
        Ok(CreateOutcome::Created(version))
    }

    async fn update(
        &self,
        tenant_id: TenantId,
        reason_code: &PublishedReasonCode,
        expected: &Version,
    ) -> Result<UpdateOutcome, ReasonCodeStoreError> {
        let version = self.mint();
        let mut rows = self.rows.lock().expect("lock");
        let Some((_, row)) = rows
            .iter_mut()
            .find(|(owner, row)| *owner == tenant_id && row.record.id == reason_code.id)
        else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record = reason_code.clone();
        row.etag = version.clone();
        // The real adapter's `updated_at` moves on every write; a fake that stamped only on create
        // would let a read-path bug that pins the create instant pass.
        row.updated_at_ms = Some(NOW_MS);
        Ok(UpdateOutcome::Updated(version))
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        reason_code_id: ReasonCodeId,
    ) -> Result<(), ReasonCodeStoreError> {
        self.rows
            .lock()
            .expect("lock")
            .retain(|(owner, row)| !(*owner == tenant_id && row.record.id == reason_code_id));
        Ok(())
    }
}

/// The most recent trail entry for `action`, or `None`.
async fn last_trail_entry(audit: &FakeAudit, action: &str) -> Option<AuditEntry> {
    audit
        .list(None, 50)
        .await
        .expect("read the trail")
        .into_iter()
        .find(|entry| entry.action == action)
}

// --- Store groups and the batch publish (ADR-0122) ---------------------------------------------

/// An in-memory `StoreGroupStore` for the batch-publish route tests.
///
/// Version-carrying like the real thing, because the membership write is conditional (ADR-0094),
/// and it keeps the batch tables as flat lists — which is enough to prove the property the record
/// actually promises: **every member gets a row**, whatever happened at it.
/// One stored group: the tenant that owns it, and the record with the version it stands at.
type StoreGroupRow = (TenantId, Versioned<StoreGroup>);

/// A cohort's membership, keyed as `store_group_members` is.
type StoreGroupMembers = HashMap<(TenantId, StoreGroupId), Vec<StoreId>>;

/// One recorded per-store outcome, keyed as `config_batch_results` is.
type BatchResultRow = (TenantId, ConfigBatchId, BatchResult);

#[derive(Clone, Default)]
struct FakeStoreGroups {
    groups: Arc<Mutex<Vec<StoreGroupRow>>>,
    members: Arc<Mutex<StoreGroupMembers>>,
    batches: Arc<Mutex<Vec<ConfigBatch>>>,
    results: Arc<Mutex<Vec<BatchResultRow>>>,
    next_version: Arc<Mutex<u64>>,
}

impl FakeStoreGroups {
    /// The fake's stand-in for `xmin` (ADR-0094).
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }

    /// Seeds a group and its membership without going through the routes.
    fn seed(&self, group: StoreGroup, members: &[StoreId]) {
        let version = self.mint();
        let key = (group.tenant_id, group.group_id);
        self.groups
            .lock()
            .expect("lock")
            .push((group.tenant_id, Versioned::new(group, version)));
        self.members
            .lock()
            .expect("lock")
            .insert(key, members.to_vec());
    }
}

impl StoreGroupStore for FakeStoreGroups {
    async fn create_group(&self, group: &StoreGroup) -> Result<Version, StoreGroupStoreError> {
        let version = self.mint();
        self.groups.lock().expect("lock").push((
            group.tenant_id,
            Versioned::new(group.clone(), version.clone()),
        ));
        Ok(version)
    }

    async fn list_groups(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<StoreGroup>>, StoreGroupStoreError> {
        Ok(self
            .groups
            .lock()
            .expect("lock")
            .iter()
            .filter(|(owner, _)| *owner == tenant_id)
            .map(|(_, row)| row.clone())
            .collect())
    }

    async fn update_group(
        &self,
        group: &StoreGroup,
        expected: &Version,
    ) -> Result<UpdateOutcome, StoreGroupStoreError> {
        let version = self.mint();
        let mut rows = self.groups.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|(owner, row)| {
            *owner == group.tenant_id && row.record.group_id == group.group_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if row.1.etag != *expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.1 = Versioned::new(group.clone(), version.clone());
        Ok(UpdateOutcome::Updated(version))
    }

    async fn list_members(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
    ) -> Result<Vec<StoreId>, StoreGroupStoreError> {
        Ok(self
            .members
            .lock()
            .expect("lock")
            .get(&(tenant_id, group_id))
            .cloned()
            .unwrap_or_default())
    }

    async fn set_members(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
        members: &[StoreId],
        expected: &Version,
    ) -> Result<UpdateOutcome, StoreGroupStoreError> {
        let version = self.mint();
        let mut rows = self.groups.lock().expect("lock");
        let Some(row) = rows
            .iter_mut()
            .find(|(owner, row)| *owner == tenant_id && row.record.group_id == group_id)
        else {
            return Ok(UpdateOutcome::NotFound);
        };
        if row.1.etag != *expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.1 = Versioned::new(row.1.record.clone(), version.clone());
        self.members
            .lock()
            .expect("lock")
            .insert((tenant_id, group_id), members.to_vec());
        Ok(UpdateOutcome::Updated(version))
    }

    async fn start_batch(&self, batch: &ConfigBatch) -> Result<(), StoreGroupStoreError> {
        self.batches.lock().expect("lock").push(batch.clone());
        Ok(())
    }

    async fn record_result(
        &self,
        tenant_id: TenantId,
        batch_id: ConfigBatchId,
        result: &BatchResult,
    ) -> Result<(), StoreGroupStoreError> {
        self.results
            .lock()
            .expect("lock")
            .push((tenant_id, batch_id, result.clone()));
        Ok(())
    }

    async fn finish_batch(
        &self,
        tenant_id: TenantId,
        batch_id: ConfigBatchId,
        finished_at_ms: i64,
    ) -> Result<(), StoreGroupStoreError> {
        if let Some(batch) = self
            .batches
            .lock()
            .expect("lock")
            .iter_mut()
            .find(|batch| batch.tenant_id == tenant_id && batch.batch_id == batch_id)
        {
            batch.finished_at_ms = Some(finished_at_ms);
        }
        Ok(())
    }

    async fn list_batches(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
        limit: u32,
    ) -> Result<Vec<ConfigBatch>, StoreGroupStoreError> {
        let batches = self.batches.lock().expect("lock");
        let mut mine: Vec<ConfigBatch> = batches
            .iter()
            .filter(|batch| batch.tenant_id == tenant_id && batch.group_id == group_id)
            .cloned()
            .collect();
        mine.sort_by(|left, right| right.started_at_ms.cmp(&left.started_at_ms));
        mine.truncate(limit as usize);
        Ok(mine)
    }

    async fn batch_results(
        &self,
        tenant_id: TenantId,
        batch_id: ConfigBatchId,
    ) -> Result<Vec<BatchResult>, StoreGroupStoreError> {
        Ok(self
            .results
            .lock()
            .expect("lock")
            .iter()
            .filter(|(owner, batch, _)| *owner == tenant_id && *batch == batch_id)
            .map(|(_, _, result)| result.clone())
            .collect())
    }
}

/// A `CampaignStore` that holds nothing — the batch registry needs all seven authoring seams, and
/// these tests exercise the `menu` and `tax` nodes.
#[derive(Clone, Default)]
struct FakeCampaigns;

impl CampaignStore for FakeCampaigns {
    async fn list_campaigns(
        &self,
        _tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedCampaign>>, CampaignStoreError> {
        Ok(Vec::new())
    }

    async fn get_campaign(
        &self,
        _tenant_id: TenantId,
        _campaign_id: CampaignId,
    ) -> Result<Option<Versioned<PublishedCampaign>>, CampaignStoreError> {
        Ok(None)
    }

    async fn create_campaign(
        &self,
        _tenant_id: TenantId,
        _campaign: &PublishedCampaign,
    ) -> Result<CreateOutcome, CampaignStoreError> {
        Ok(CreateOutcome::Created(Version::new("1")))
    }

    async fn update_campaign(
        &self,
        _tenant_id: TenantId,
        _campaign: &PublishedCampaign,
        _expected: &Version,
    ) -> Result<UpdateOutcome, CampaignStoreError> {
        Ok(UpdateOutcome::NotFound)
    }

    async fn delete_campaign(
        &self,
        _tenant_id: TenantId,
        _campaign_id: CampaignId,
    ) -> Result<(), CampaignStoreError> {
        Ok(())
    }
}

/// The console surface a batch test needs: the catalog to author a menu with, the group routes,
/// and the batch registry over the same seams `main.rs` builds it from.
///
/// The generic preview (F11) is merged here rather than in a helper of its own, and for the reason
/// it exists: it compiles through the *same* node table the batch does, so a test that publishes
/// through the batch and then previews the same node is asking the one question worth asking — do
/// the two agree?
fn store_group_app(
    admin: FakeAdmin,
    groups: FakeStoreGroups,
    registry: FakeRegistry,
    config_trees: FakeConfigTrees,
    catalog: FakeCatalog,
) -> axum::Router {
    store_group_app_with_fees(
        admin,
        groups,
        registry,
        config_trees,
        catalog,
        InMemoryFeeRules::new(),
        Arc::new(NoopAuditRecorder),
    )
}

/// [`store_group_app`] over a tenant's fee rules, which a menu batch brings to each member, with
/// the group routes recording to `audit`.
fn store_group_app_with_fees(
    admin: FakeAdmin,
    groups: FakeStoreGroups,
    registry: FakeRegistry,
    config_trees: FakeConfigTrees,
    catalog: FakeCatalog,
    fee_rules: InMemoryFeeRules,
    audit: Arc<dyn AuditRecorder>,
) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    );
    let people = FakePeople::over(&groups);
    http::router(app)
        .merge(http::catalog_router(
            catalog.clone(),
            admin.clone(),
            clock(),
            Arc::new(NoopAuditRecorder),
        ))
        .merge(http::store_group_router(
            groups,
            registry.clone(),
            config_trees.clone(),
            http::batch_nodes(
                catalog.clone(),
                FakeTaxRates::default(),
                FakeCampaigns,
                FakeInventory::default(),
                FakeReasonCodes::default(),
                people.clone(),
                FakeFloor::default(),
                fee_rules.clone(),
                registry.clone(),
            ),
            people,
            admin.clone(),
            clock(),
            audit,
        ))
        .merge(http::config_preview_router(
            config_trees,
            http::batch_nodes(
                catalog,
                FakeTaxRates::default(),
                FakeCampaigns,
                FakeInventory::default(),
                FakeReasonCodes::default(),
                FakePeople::default(),
                FakeFloor::default(),
                fee_rules,
                registry,
            ),
            admin,
            clock(),
        ))
}

/// Seeds a store into the registry at `status`, so the batch's protective check has something to
/// read. A membership row for a store the registry never heard of is the third case, and the
/// fake's absence of a row is exactly how it arises in production.
fn seed_store(registry: &FakeRegistry, tenant_id: TenantId, store_id: StoreId, active: bool) {
    let version = Version::new(format!("{}", store_id.as_ulid().to_u128()));
    registry.stores.lock().expect("lock").push(Versioned::new(
        StoreRecord {
            store_id,
            tenant_id,
            brand_id: None,
            name: format!("Store {}", store_id.as_ulid()),
            status: if active {
                EntityStatus::Active
            } else {
                EntityStatus::Archived
            },
        },
        version,
    ));
}

/// Gives one store the `tax` and `locale` nodes a `menu` batch requires (ADR-0122 §7), by writing
/// the Store layer directly — the point under test is the batch, not how the prerequisites got
/// there.
fn seed_menu_prerequisites(config_trees: &FakeConfigTrees, tenant_id: TenantId, store_id: StoreId) {
    config_trees.rows.lock().expect("lock").insert(
        (tenant_id, store_id),
        Versioned::new(
            ConfigTreeState {
                layers: [
                    serde_json::json!({}),
                    serde_json::json!({}),
                    serde_json::json!({ "tax": { "rates": [] }, "locale": { "currency_code": "VND" } }),
                    serde_json::json!({}),
                ],
                history: Vec::new(),
                k: 8,
            },
            Version::new("seed"),
        ),
    );
}

/// Authors one menu through the catalog routes and returns its id.
async fn author_a_menu(router: &axum::Router, cookie: &str, tenant: &str) -> String {
    let menu = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/menus",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard" }),
            cookie,
        ))
        .await
        .expect("route create menu");
    assert_eq!(menu.status(), StatusCode::CREATED);
    json_body(menu).await["menu_id"]
        .as_str()
        .expect("a menu id")
        .to_owned()
}

/// The people publish and both previews — the generic one and the campaigns one — over one roster
/// and one config-tree fake, so a test can publish a store's `permissions` node, change a person,
/// and ask what a publish would do next.
fn people_preview_app(
    admin: FakeAdmin,
    people: FakePeople,
    config: FakeConfigTrees,
) -> axum::Router {
    people_publish_app(
        admin.clone(),
        people.clone(),
        config.clone(),
        Arc::new(NoopAuditRecorder),
    )
    .merge(http::config_preview_router(
        config.clone(),
        http::batch_nodes(
            FakeCatalog::default(),
            FakeTaxRates::default(),
            FakeCampaigns,
            FakeInventory::default(),
            FakeReasonCodes::default(),
            people,
            FakeFloor::default(),
            InMemoryFeeRules::new(),
            FakeRegistry::default(),
        ),
        admin.clone(),
        clock(),
    ))
    .merge(http::config_campaigns_router(
        FakeCampaigns,
        config,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

// ---------------------------------------------------------------------------------------------
// Config releases (ADR-0125): one decision, many writes
// ---------------------------------------------------------------------------------------------

/// The release identity and roll-up, in memory.
#[derive(Clone, Default)]
struct FakeReleases {
    rows: Arc<Mutex<Vec<pos_cloud::releases::Release>>>,
}

impl pos_cloud::releases::ReleaseStore for FakeReleases {
    async fn create(
        &self,
        release: &pos_cloud::releases::NewRelease,
    ) -> Result<(), pos_cloud::releases::ReleaseStoreError> {
        self.rows
            .lock()
            .expect("lock")
            .push(pos_cloud::releases::Release {
                id: release.id.clone(),
                tenant_id: release.tenant_id,
                name: release.name.clone(),
                status: pos_cloud::releases::ReleaseStatus::Draft,
                target_group_id: release.target_group_id.clone(),
                moment: release.moment.clone(),
                created_by: release.created_by.clone(),
                created_at_ms: 0,
                updated_at_ms: 0,
            });
        Ok(())
    }

    async fn list_for_tenant(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<pos_cloud::releases::Release>, pos_cloud::releases::ReleaseStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn fetch_one(
        &self,
        tenant_id: TenantId,
        id: &str,
    ) -> Result<Option<pos_cloud::releases::Release>, pos_cloud::releases::ReleaseStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|row| row.tenant_id == tenant_id && row.id == id)
            .cloned())
    }

    async fn set_status(
        &self,
        tenant_id: TenantId,
        id: &str,
        status: pos_cloud::releases::ReleaseStatus,
    ) -> Result<bool, pos_cloud::releases::ReleaseStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        let Some(row) = rows
            .iter_mut()
            .find(|row| row.tenant_id == tenant_id && row.id == id)
        else {
            return Ok(false);
        };
        row.status = status;
        Ok(true)
    }

    async fn in_flight(
        &self,
    ) -> Result<Vec<pos_cloud::releases::Release>, pos_cloud::releases::ReleaseStoreError> {
        Ok(Vec::new())
    }
}

/// The pair rows, in memory.
#[derive(Clone, Default)]
struct FakeScheduledPublishes {
    rows: Arc<Mutex<Vec<pos_cloud::scheduling::ScheduledPublish>>>,
}

impl pos_cloud::scheduling::ScheduledPublishStore for FakeScheduledPublishes {
    async fn schedule(
        &self,
        publish: &pos_cloud::scheduling::NewScheduledPublish,
    ) -> Result<(), pos_cloud::scheduling::ScheduledPublishError> {
        self.rows
            .lock()
            .expect("lock")
            .push(pos_cloud::scheduling::ScheduledPublish {
                id: publish.id.clone(),
                tenant_id: publish.tenant_id,
                store_id: publish.store_id,
                node_key: publish.node_key.clone(),
                node_value: publish.node_value.clone(),
                effective_at_ms: publish.effective_at_ms,
                status: pos_cloud::scheduling::ScheduledPublishStatus::Pending,
                created_at_ms: 0,
                applied_version_id: None,
                release_id: publish.release_id.clone(),
                failure: None,
                resolved_timezone: publish.resolved_timezone.clone(),
            });
        Ok(())
    }

    async fn due(
        &self,
        _now_ms: i64,
    ) -> Result<
        Vec<pos_cloud::scheduling::ScheduledPublish>,
        pos_cloud::scheduling::ScheduledPublishError,
    > {
        Ok(Vec::new())
    }

    async fn list_for_store(
        &self,
        _tenant_id: TenantId,
        _store_id: StoreId,
    ) -> Result<
        Vec<pos_cloud::scheduling::ScheduledPublish>,
        pos_cloud::scheduling::ScheduledPublishError,
    > {
        Ok(Vec::new())
    }

    async fn cancel(
        &self,
        tenant_id: TenantId,
        id: &str,
    ) -> Result<bool, pos_cloud::scheduling::ScheduledPublishError> {
        let mut rows = self.rows.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|row| {
            row.tenant_id == tenant_id
                && row.id == id
                && row.status == pos_cloud::scheduling::ScheduledPublishStatus::Pending
        }) else {
            return Ok(false);
        };
        row.status = pos_cloud::scheduling::ScheduledPublishStatus::Cancelled;
        Ok(true)
    }

    async fn mark_applied(
        &self,
        _id: &str,
        _version_id: &str,
    ) -> Result<(), pos_cloud::scheduling::ScheduledPublishError> {
        Ok(())
    }

    async fn mark_failed(
        &self,
        _id: &str,
        _failure: &str,
    ) -> Result<(), pos_cloud::scheduling::ScheduledPublishError> {
        Ok(())
    }

    async fn list_for_release(
        &self,
        tenant_id: TenantId,
        release_id: &str,
    ) -> Result<
        Vec<pos_cloud::scheduling::ScheduledPublish>,
        pos_cloud::scheduling::ScheduledPublishError,
    > {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| {
                row.tenant_id == tenant_id && row.release_id.as_deref() == Some(release_id)
            })
            .cloned()
            .collect())
    }
}

/// The console surface a release test needs, over the same node table `main.rs` builds.
fn config_release_app(
    admin: FakeAdmin,
    releases: FakeReleases,
    scheduled: FakeScheduledPublishes,
    groups: FakeStoreGroups,
    registry: FakeRegistry,
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
    http::router(app).merge(http::config_release_router(
        releases,
        scheduled,
        groups,
        registry.clone(),
        config_trees,
        http::batch_nodes(
            FakeCatalog::default(),
            FakeTaxRates::default(),
            FakeCampaigns,
            FakeInventory::default(),
            FakeReasonCodes::default(),
            FakePeople::default(),
            FakeFloor::default(),
            InMemoryFeeRules::new(),
            registry,
        ),
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// Creates a draft aimed at `group_id` with a Monday-04:00 wall-clock moment, and returns its id.
async fn draft_a_release(router: &axum::Router, cookie: &str, tenant: &str, group: &str) -> String {
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config-releases",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Tết 2027",
                "target_group_id": group,
                "wall_clock_date": "2027-02-08",
                "wall_clock_time": "04:00",
            }),
            cookie,
        ))
        .await
        .expect("route the create");
    assert_eq!(created.status(), StatusCode::CREATED);
    let body = json_body(created).await;
    assert_eq!(
        body["status"], "draft",
        "a new release is not yet scheduled"
    );
    body["release_id"].as_str().expect("release_id").to_owned()
}

// --- Revocations, the kill switch and the fleet row after a rollback ------------------------------

/// The two devices a store had retired before its rollback.
fn retired_before_the_rollback() -> [String; 2] {
    [
        Ulid::from_u128(0x21).to_string(),
        Ulid::from_u128(0x22).to_string(),
    ]
}

/// The rollout a store ran before its rollback, as `PUT /admin/config/ota` writes one.
fn restored_rollout() -> serde_json::Value {
    serde_json::json!({
        "target_version": "1.4.0",
        "min_ring": "fleet",
        "rollout_percent": 40,
        "signing_key_id": "a1a1a1a1a1a1a1a1",
        "revoked_key_ids": [],
    })
}

/// Gives a store a tree as a rollback leaves it: a deny-list of [`retired_before_the_rollback`], a
/// rollout and a published country on the Store layer, a placement on the Device layer, then a
/// publish that dropped them, rolled back ([`ConfigTree::restore`], the rollback route's own
/// restore). What the store runs is on the Tenant layer; the Store and Device layers are empty.
fn seed_rolled_back_with_retirements(
    config_trees: &FakeConfigTrees,
    tenant_id: TenantId,
    store_id: StoreId,
) {
    let version = |n: u128| ConfigVersionId::new(Ulid::from_u128(n));
    let mut tree = ConfigTree::new(store_id, CapabilityValidator);
    tree.publish(
        ConfigLevel::Store,
        serde_json::json!({
            "revoked_devices": { "device_ids": retired_before_the_rollback() },
            "fleet_update": restored_rollout(),
            "locale": { "country_code": "VN", "currency_code": "VND" },
        }),
        version(1),
        Vec::new(),
    )
    .expect("the store's publish");
    tree.publish(
        ConfigLevel::Device,
        serde_json::json!({ "device_ota": { "ring": "fleet", "canary_bucket": 10 } }),
        version(2),
        Vec::new(),
    )
    .expect("the placement");
    tree.publish(
        ConfigLevel::Store,
        serde_json::json!({}),
        version(3),
        Vec::new(),
    )
    .expect("the publish rolled back");
    tree.restore(version(2), version(4)).expect("the rollback");
    let state = tree.state();
    assert_eq!(
        (&state.layers[2], &state.layers[3]),
        (&serde_json::json!({}), &serde_json::json!({})),
        "a rollback empties the Store and Device layers"
    );
    let version = config_trees.mint();
    config_trees
        .rows
        .lock()
        .expect("lock")
        .insert((tenant_id, store_id), Versioned::new(state, version));
}

/// A revocation after a rollback keeps every device the store had retired: the store runs the
/// restored list with the new device on it, rather than a list of the new device alone.
#[tokio::test]
async fn after_a_rollback_a_revocation_keeps_every_device_the_store_had_retired() {
    let config = FakeConfigTrees::default();
    seed_rolled_back_with_retirements(&config, tenant(), store_id());
    let router = device_revoke_app(provisioned_admin(), config.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let uri = format!("/admin/stores/{}/devices/revoke", store_id().as_ulid());
    let revoke = |device: String| {
        router.clone().oneshot(post_with_cookie(
            &uri,
            &serde_json::json!({ "tenant_id": tenant_ulid, "local_device_id": device }),
            &cookie,
        ))
    };
    let [first, second] = retired_before_the_rollback();
    let third = Ulid::from_u128(0x23).to_string();
    let every_one = vec![first.clone(), second, third.clone()];

    let response = revoke(third).await.expect("route the revoke");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        running_document(&config, tenant(), store_id())["revoked_devices"]["device_ids"],
        serde_json::json!(every_one),
        "the two retired before the rollback stay retired"
    );
    assert_eq!(
        published_deny_list(&config, tenant(), store_id()).await,
        every_one,
        "written on the Store layer, as before, holding the whole list"
    );

    // Idempotent as before: a device the rollback restored adds nothing.
    let response = revoke(first).await.expect("route the revoke");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        published_deny_list(&config, tenant(), store_id()).await,
        every_one
    );
}

/// After a rollback the OTA reads answer the rollout and the placement the store runs, and the kill
/// switch halts that rollout, keeping the rest of it.
#[tokio::test]
async fn after_a_rollback_the_kill_switch_halts_the_rollout_the_store_runs() {
    let config = FakeConfigTrees::default();
    seed_rolled_back_with_retirements(&config, tenant(), store_id());
    let router = ota_app(provisioned_admin(), config.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let read = |path: &str| {
        router.clone().oneshot(get_with_cookie(
            &format!("{path}?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
    };

    let rollout = read("/admin/config/ota")
        .await
        .expect("route the rollout read");
    assert_eq!(json_body(rollout).await, restored_rollout());
    let placement = read("/admin/config/ota/placement")
        .await
        .expect("route the placement read");
    assert_eq!(
        json_body(placement).await,
        serde_json::json!({ "ring": "fleet", "canary_bucket": 10 })
    );

    let halted = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config/ota/halt",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "halted": true,
            }),
            &cookie,
        ))
        .await
        .expect("route the halt");
    assert_eq!(halted.status(), StatusCode::OK);
    let mut expected = restored_rollout();
    expected["halted"] = serde_json::json!(true);
    assert_eq!(
        running_document(&config, tenant(), store_id())["fleet_update"],
        expected,
        "the restored rollout, halted, the rest of it kept"
    );
}

/// The kill switch on a store never rolled back halts its published rollout and resumes it, keeping
/// the rest of the rollout either way.
#[tokio::test]
async fn the_kill_switch_halts_and_resumes_a_published_rollout() {
    let config = FakeConfigTrees::default();
    let router = ota_app(provisioned_admin(), config.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let mut body = restored_rollout();
    body["tenant_id"] = serde_json::json!(tenant_ulid);
    body["store_id"] = serde_json::json!(store_ulid);
    let published = router
        .clone()
        .oneshot(put_with_cookie("/admin/config/ota", &body, &cookie))
        .await
        .expect("route the rollout publish");
    assert_eq!(published.status(), StatusCode::OK);

    for halted in [true, false] {
        let response = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/config/ota/halt",
                &serde_json::json!({
                    "tenant_id": tenant_ulid,
                    "store_id": store_ulid,
                    "halted": halted,
                }),
                &cookie,
            ))
            .await
            .expect("route the halt");
        assert_eq!(response.status(), StatusCode::OK);
        let mut expected = restored_rollout();
        expected["halted"] = serde_json::json!(halted);
        let tree = config
            .load(tenant(), store_id())
            .await
            .expect("load")
            .expect("a tree");
        assert_eq!(tree.record.layers[2]["fleet_update"], expected, "{halted}");
    }
}

/// The kill switch halts the rollout the store runs when the halt is written, not the one it read
/// first: a rollout another operator publishes between the halt's read and its write is the one
/// halted, and the halt answers and is recorded as any halt is.
///
/// The halt used to read the tree, then publish a node built from that read through a write that read
/// the tree again: the newer rollout was overwritten by the older one, halted, and the store would
/// have run the target nobody had asked for any more.
#[tokio::test]
async fn the_kill_switch_halts_a_rollout_published_between_its_read_and_its_write() {
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let mut tree = ConfigTree::new(store_id(), CapabilityValidator);
    tree.publish(
        ConfigLevel::Store,
        serde_json::json!({ "fleet_update": restored_rollout() }),
        ConfigVersionId::new(Ulid::from_u128(1)),
        Vec::new(),
    )
    .expect("the first rollout");
    let version = config.mint();
    config.rows.lock().expect("lock").insert(
        (tenant(), store_id()),
        Versioned::new(tree.state(), version),
    );
    let router = ota_app_with_audit(
        provisioned_admin(),
        config.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;

    // Another operator's rollout lands after the halt reads the tree and before it writes.
    let newer = serde_json::json!({
        "target_version": "1.5.0",
        "min_ring": "fleet",
        "rollout_percent": 60,
        "signing_key_id": "b2b2b2b2b2b2b2b2",
        "revoked_key_ids": [],
    });
    *config.interpose.lock().expect("lock") = Some(("fleet_update".to_owned(), newer.clone()));

    let halted = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config/ota/halt",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id().as_ulid().to_string(),
                "halted": true,
            }),
            &cookie,
        ))
        .await
        .expect("route the halt");
    assert_eq!(halted.status(), StatusCode::OK);
    assert!(
        json_body(halted).await["config_version_id"].is_string(),
        "the version the halt published, as before"
    );
    let mut expected = newer;
    expected["halted"] = serde_json::json!(true);
    assert_eq!(
        running_document(&config, tenant(), store_id())["fleet_update"],
        expected,
        "the rollout published meanwhile, halted, and not the one the halt read first"
    );
    let recorded: Vec<_> = audit
        .list(None, 100)
        .await
        .expect("list audit")
        .into_iter()
        .filter(|entry| entry.action == "config.ota.halt")
        .map(|entry| (entry.entity_id, entry.after))
        .collect();
    assert_eq!(
        recorded,
        vec![(
            store_id().to_string(),
            Some(serde_json::json!({ "halted": true }))
        )],
        "recorded once, as every halt is"
    );
}

/// The kill switch on a store with no published rollout is refused `400`, as it always was. The
/// refusal is decided on the tree the halt would write over, so nothing is published and nothing is
/// recorded.
#[tokio::test]
async fn the_kill_switch_on_a_store_with_no_rollout_is_refused_and_records_nothing() {
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let router = ota_app_with_audit(
        provisioned_admin(),
        config.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;
    let recorded = audit.list(None, 100).await.expect("list audit").len();

    let refused = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config/ota/halt",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id().as_ulid().to_string(),
                "halted": true,
            }),
            &cookie,
        ))
        .await
        .expect("route the halt");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let error = json_body(refused).await;
    assert_eq!(
        error["error"]["message"], "the store has no published rollout to halt",
        "{error}"
    );
    assert_eq!(
        version_count(&config, tenant(), store_id()),
        0,
        "nothing published"
    );
    assert_eq!(
        audit.list(None, 100).await.expect("list audit").len(),
        recorded,
        "nothing recorded"
    );
}

/// After a rollback the fleet row compares the store's region against the country the store runs,
/// which the rollback restored, rather than reporting no country.
#[tokio::test]
async fn after_a_rollback_the_fleet_row_shows_the_restored_country() {
    let store = store_id();
    let mut row = store_in_handover(store, Some(1), None, None);
    row.region = Region::stored("SG", "ap-southeast-1");
    let fleet = FakeFleet::default().with_row(tenant(), row);
    let config = FakeConfigTrees::default();
    seed_rolled_back_with_retirements(&config, tenant(), store);
    let router = fleet_app_with_config(provisioned_admin(), fleet, config);
    let cookie = admin_cookie(&router).await;

    let response = router
        .oneshot(get_with_cookie(
            &format!("/admin/fleet/{store}?tenant_id={}", tenant().as_ulid()),
            &cookie,
        ))
        .await
        .expect("route the single-store read");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["profile_country"], "VN");
    assert_eq!(body["region_agreement"], "differs");
}

// --- Settings (ADR-0160 decision 3) -----------------------------------------------------------

/// A tenant-scoped in-memory `SettingsStore`: a write replaces the value in its slot.
#[derive(Default, Clone)]
struct FakeSettings {
    rows: Arc<Mutex<Vec<(TenantId, pos_cloud::settings::SettingValue)>>>,
}

impl pos_cloud::settings::SettingsStore for FakeSettings {
    async fn list(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<pos_cloud::settings::SettingValue>, pos_cloud::settings::SettingsStoreError>
    {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|(owner, _)| *owner == tenant_id)
            .map(|(_, value)| value.clone())
            .collect())
    }

    async fn put(
        &self,
        tenant_id: TenantId,
        value: &pos_cloud::settings::SettingValue,
    ) -> Result<(), pos_cloud::settings::SettingsStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        rows.retain(|(owner, held)| {
            !(*owner == tenant_id
                && held.setting_key == value.setting_key
                && held.scope == value.scope
                && held.scope_id == value.scope_id)
        });
        rows.push((tenant_id, value.clone()));
        Ok(())
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        setting_key: &str,
        scope: pos_proto::settings::SettingScope,
        scope_id: &str,
    ) -> Result<bool, pos_cloud::settings::SettingsStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        let before = rows.len();
        rows.retain(|(owner, held)| {
            !(*owner == tenant_id
                && held.setting_key == setting_key
                && held.scope.known() == scope
                && held.scope_id == scope_id)
        });
        Ok(rows.len() < before)
    }
}

const NO_SHIFT_SELLING: &str = "shift.no_shift_selling";

fn settings_brand() -> pos_cloud::registry::BrandId {
    pos_cloud::registry::BrandId::new(Ulid::from_u128(0xB4A0D))
}

/// Three stores of one tenant: two of a brand, one without; a group holding the first and the
/// third; and a second group, archived.
fn settings_stores() -> [StoreId; 3] {
    [
        StoreId::new(Ulid::from_u128(0x5A01)),
        StoreId::new(Ulid::from_u128(0x5A02)),
        StoreId::new(Ulid::from_u128(0x5A03)),
    ]
}

fn settings_group() -> StoreGroupId {
    StoreGroupId::new(Ulid::from_u128(0x6A01))
}

fn archived_group() -> StoreGroupId {
    StoreGroupId::new(Ulid::from_u128(0x6A02))
}

/// The settings routes over fakes, with the layout above seeded.
fn settings_app() -> (axum::Router, FakeConfigTrees, FakeStoreGroups) {
    settings_app_with(Arc::new(NoopAuditRecorder))
}

/// [`settings_app`], with the settings routes recording to `audit`.
fn settings_app_with(
    audit: Arc<dyn AuditRecorder>,
) -> (axum::Router, FakeConfigTrees, FakeStoreGroups) {
    let admin = provisioned_admin();
    let config_trees = FakeConfigTrees::default();
    let registry = FakeRegistry::default();
    let [first, second, third] = settings_stores();
    registry.brands.lock().expect("lock").push(Versioned::new(
        BrandRecord {
            brand_id: settings_brand(),
            tenant_id: tenant(),
            name: "Brand".to_owned(),
            status: EntityStatus::Active,
        },
        Version::new("1"),
    ));
    for (store_id, brand_id) in [
        (first, Some(settings_brand())),
        (second, Some(settings_brand())),
        (third, None),
    ] {
        registry.stores.lock().expect("lock").push(Versioned::new(
            StoreRecord {
                store_id,
                tenant_id: tenant(),
                brand_id,
                name: format!("Store {}", store_id.as_ulid()),
                status: EntityStatus::Active,
            },
            Version::new("1"),
        ));
    }
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id: settings_group(),
            tenant_id: tenant(),
            name: "North".to_owned(),
            status: EntityStatus::Active,
        },
        &[first, third],
    );
    groups.seed(
        StoreGroup {
            group_id: archived_group(),
            tenant_id: tenant(),
            name: "Closed".to_owned(),
            status: EntityStatus::Archived,
        },
        &[second],
    );
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    );
    let router = http::router(app)
        .merge(http::settings_router(
            FakeSettings::default(),
            registry,
            groups.clone(),
            config_trees.clone(),
            admin.clone(),
            clock(),
            audit,
        ))
        // The tender publish, whose node the tip keys share (ADR-0160).
        .merge(http::config_channels_router(
            config_trees.clone(),
            admin,
            clock(),
            Arc::new(NoopAuditRecorder),
        ));
    (router, config_trees, groups)
}

/// Each store a write reached and its outcome, in the order the response lists them.
fn outcomes(body: &serde_json::Value) -> Vec<(String, String)> {
    body["stores"]
        .as_array()
        .expect("a list of stores")
        .iter()
        .map(|store| {
            (
                store["store_id"].as_str().unwrap_or_default().to_owned(),
                store["outcome"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

const LOCKOUT_ATTEMPTS: &str = "session.lockout_attempts";

/// A write of `value` for the whole-number setting `setting_key`, for every store of the tenant.
fn put_number(setting_key: &str, value: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "setting_key": setting_key,
        "scope": "SETTING_SCOPE_TENANT",
        "scope_id": tenant().as_ulid().to_string(),
        "value": value,
    })
}

/// The `session` node a store's Tenant layer carries, if any.
async fn tenant_layer_session(
    config_trees: &FakeConfigTrees,
    store_id: StoreId,
) -> Option<serde_json::Value> {
    tenant_layer_node(config_trees, store_id, "session").await
}

/// What a store is sent: its four layers merged, as the sync path composes them (ADR-0033).
fn sent_to(tree: &ConfigTreeState) -> serde_json::Value {
    pos_cloud::config_tree::merge::merge_layers(&[
        &tree.layers[0],
        &tree.layers[1],
        &tree.layers[2],
        &tree.layers[3],
    ])
}

/// The `node` a store's Tenant layer carries, if any.
async fn tenant_layer_node(
    config_trees: &FakeConfigTrees,
    store_id: StoreId,
    node: &str,
) -> Option<serde_json::Value> {
    let tree = config_trees.load(tenant(), store_id).await.expect("load")?;
    tree.record.layers[0].get(node).cloned()
}
