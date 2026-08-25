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
use pos_cloud::auth::SuperAdminCredential;
use pos_cloud::auth::admin::{AdminCredential, AdminStore, AdminStoreError};
use pos_cloud::auth::apikey::{
    ApiKeyAdminStore, ApiKeyId, ApiKeyStore, ApiKeyStoreError, ApiKeySummary, Scope, StoredApiKey,
    issue,
};
use pos_cloud::auth::password::hash_password;
use pos_cloud::auth::totp::{DIGITS, TotpSecret, code_at};
use pos_cloud::catalog::{
    CatalogItem, CatalogStore, CatalogStoreError, ItemCategory, ItemSubcategory, Menu, MenuId,
    MenuPlacement, TaxClass,
};
use pos_cloud::config_tree::{ConfigStoreError, ConfigTreeState, ConfigTreeStore};
use pos_cloud::dashboard::{RollupError, RollupStore, StoredRollups, project};
use pos_cloud::devices::{
    DeviceKind, DeviceProposalError, DeviceProposalId, DeviceProposalStatus, DeviceProposalStore,
    DeviceProposalSummary, PersistedDeviceProposal,
};
use pos_cloud::http::CloudApp;
use pos_cloud::orders::{StoreDirectory, orders_router};
use pos_cloud::reconcile::{ReconcileError, ReconcileStore};
use pos_cloud::registry::{
    BrandRecord, DeviceRecord, RegistryStore, RegistryStoreError, StoreRecord, TenantRecord,
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
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{DeviceId, EventId, MenuItemId, StoreId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

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
#[derive(Clone, Default)]
struct FakeAdmin {
    credential: Arc<Mutex<Option<SuperAdminCredential>>>,
    last_used_totp_step: Arc<Mutex<Option<u64>>>,
    sessions: Arc<Mutex<HashMap<[u8; 32], Timestamp>>>,
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

    async fn record_totp_step(&self, step: u64) -> Result<(), AdminStoreError> {
        let mut last = self.last_used_totp_step.lock().expect("lock");
        if last.is_none_or(|current| step > current) {
            *last = Some(step);
        }
        Ok(())
    }

    async fn create_session(
        &self,
        token_hash: [u8; 32],
        expires_at: Timestamp,
    ) -> Result<(), AdminStoreError> {
        self.sessions
            .lock()
            .expect("lock")
            .insert(token_hash, expires_at);
        Ok(())
    }

    async fn session_is_valid(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<bool, AdminStoreError> {
        Ok(self
            .sessions
            .lock()
            .expect("lock")
            .get(&token_hash)
            .is_some_and(|expires_at| *expires_at > now))
    }

    async fn revoke_session(&self, token_hash: [u8; 32]) -> Result<(), AdminStoreError> {
        self.sessions.lock().expect("lock").remove(&token_hash);
        Ok(())
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

/// The config-tree store, keyed by `(tenant, store)` exactly as the real table.
#[derive(Clone, Default)]
struct FakeConfigTrees {
    rows: Arc<Mutex<HashMap<(TenantId, StoreId), ConfigTreeState>>>,
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
    http::router(app).merge(http::device_router(devices, admin, keys, clock()))
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
    http::router(app).merge(http::translation_router(translations, admin, clock()))
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
/// production's `merge`, in a test.
fn registry_app(admin: FakeAdmin, registry: FakeRegistry) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::registry_router(registry, admin, clock()))
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

// --- Catalog authoring admin routes (ADR-0066) --------------------------------------------------

#[derive(Default, Clone)]
struct FakeCatalog {
    items: Arc<Mutex<Vec<CatalogItem>>>,
    tax_classes: Arc<Mutex<Vec<TaxClass>>>,
    categories: Arc<Mutex<Vec<ItemCategory>>>,
    subcategories: Arc<Mutex<Vec<ItemSubcategory>>>,
    menus: Arc<Mutex<Vec<Menu>>>,
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
    http::router(app).merge(http::catalog_router(catalog, admin, clock()))
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
        ))
        .merge(http::catalog_publish_router(
            catalog,
            config_trees,
            admin,
            clock(),
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
