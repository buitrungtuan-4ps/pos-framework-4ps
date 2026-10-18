// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The integration routes (ADR-0153): the vendor catalogue, connections and their sealed secrets,
//! and the integrations publish.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_cloud::audit::{AuditRecorder, AuditSink, NoopAuditRecorder};
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::config_tree::ConfigTreeStore;
use pos_cloud::registry::{EntityStatus, StoreRecord};
use pos_cloud::version::{CreateOutcome, UpdateOutcome, Version, Versioned};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::TenantId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeAudit, FakeConfigTrees, FakeKeys, FakeRegistry, FakeRollups, FakeWebhooks,
    NOW_MS, admin_cookie, app_all, clock, etag_of, get_with_cookie, json_body, last_trail_entry,
    post_with_cookie, provisioned_admin, put_with_cookie, put_with_etag, role_session_cookie,
    store_id, tenant, ulid_text,
};

/// The main app merged with the provider-catalogue router (ADR-0153).
fn provider_catalogue_app(admin: FakeAdmin) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    let providers = pos_cloud::providers::registry().expect("the compiled-in catalogue is valid");
    http::router(app).merge(http::provider_catalogue_router(
        Arc::new(providers),
        admin,
        clock(),
    ))
}

#[tokio::test]
async fn the_provider_catalogue_serves_each_vendors_form_to_any_admin() {
    let admin = provisioned_admin();
    let router = provider_catalogue_app(admin.clone());

    // No session, no catalogue: it names the vendors a tenant could connect, which is not public.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/integrations/providers")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("route the catalogue");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Every console role may read it — a Viewer included — because it describes the platform.
    let viewer = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let response = router
        .oneshot(get_with_cookie("/admin/integrations/providers", &viewer))
        .await
        .expect("route the catalogue");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let providers = body["providers"].as_array().expect("a providers list");
    let sap = providers
        .iter()
        .find(|provider| provider["provider_id"] == "erp.sap")
        .expect("the ERP adapter is listed");
    assert_eq!(sap["family"], "INTEGRATION_FAMILY_ERP");
    assert_eq!(
        sap["countries"],
        serde_json::json!([]),
        "SAP serves every country"
    );
    let keys: Vec<&str> = sap["fields"]
        .as_array()
        .expect("a schema")
        .iter()
        .filter_map(|field| field["key"].as_str())
        .collect();
    assert_eq!(keys, vec!["base_url", "timeout_seconds"]);
}

// --- Vendor connections (ADR-0153) ----------------------------------------------------------------

/// A version-carrying, tenant-scoped in-memory `ConnectionStore`, like [`FakeReasonCodes`], that
/// also lets a test read back exactly what reached the store — which is where "sealed before it is
/// stored" is proved.
/// One stored connection: the tenant that owns it, and the record at its version.
type ConnectionRow = (TenantId, Versioned<pos_cloud::connections::Connection>);

#[derive(Default, Clone)]
struct FakeConnections {
    rows: Arc<Mutex<Vec<ConnectionRow>>>,
    next_version: Arc<Mutex<u64>>,
}

impl FakeConnections {
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }

    /// The stored record, sealed values included.
    fn stored(&self, connection_id: &str) -> Option<pos_cloud::connections::Connection> {
        self.rows
            .lock()
            .expect("lock")
            .iter()
            .find(|(_, row)| row.record.connection_id == connection_id)
            .map(|(_, row)| row.record.clone())
    }
}

impl pos_cloud::connections::ConnectionStore for FakeConnections {
    async fn list(
        &self,
        tenant_id: TenantId,
    ) -> Result<
        Vec<Versioned<pos_cloud::connections::Connection>>,
        pos_cloud::connections::ConnectionStoreError,
    > {
        let mut mine: Vec<_> = self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|(owner, _)| *owner == tenant_id)
            .map(|(_, row)| row.clone())
            .collect();
        mine.sort_by(|a, b| a.record.connection_id.cmp(&b.record.connection_id));
        Ok(mine)
    }

    async fn get(
        &self,
        tenant_id: TenantId,
        connection_id: &str,
    ) -> Result<
        Option<Versioned<pos_cloud::connections::Connection>>,
        pos_cloud::connections::ConnectionStoreError,
    > {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|(owner, row)| *owner == tenant_id && row.record.connection_id == connection_id)
            .map(|(_, row)| row.clone()))
    }

    async fn create(
        &self,
        tenant_id: TenantId,
        connection: &pos_cloud::connections::Connection,
    ) -> Result<CreateOutcome, pos_cloud::connections::ConnectionStoreError> {
        let version = self.mint();
        let mut rows = self.rows.lock().expect("lock");
        if rows.iter().any(|(owner, row)| {
            *owner == tenant_id && row.record.connection_id == connection.connection_id
        }) {
            return Ok(CreateOutcome::AlreadyExists);
        }
        rows.push((
            tenant_id,
            Versioned::edited(connection.clone(), version.clone(), NOW_MS),
        ));
        Ok(CreateOutcome::Created(version))
    }

    async fn update(
        &self,
        tenant_id: TenantId,
        connection: &pos_cloud::connections::Connection,
        expected: &Version,
    ) -> Result<UpdateOutcome, pos_cloud::connections::ConnectionStoreError> {
        let version = self.mint();
        let mut rows = self.rows.lock().expect("lock");
        let Some((_, row)) = rows.iter_mut().find(|(owner, row)| {
            *owner == tenant_id && row.record.connection_id == connection.connection_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record = connection.clone();
        row.etag = version.clone();
        row.updated_at_ms = Some(NOW_MS);
        Ok(UpdateOutcome::Updated(version))
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        connection_id: &str,
    ) -> Result<(), pos_cloud::connections::ConnectionStoreError> {
        self.rows.lock().expect("lock").retain(|(owner, row)| {
            !(*owner == tenant_id && row.record.connection_id == connection_id)
        });
        Ok(())
    }
}

/// A gateway whose schema has a required secret, and a ledger — a family a scope may have only one
/// of. Test descriptors, so the rules are proved without depending on which real vendors exist.
static TEST_GATEWAY: pos_providers::ProviderDescriptor = pos_providers::ProviderDescriptor {
    provider_id: "qr.test_gateway",
    family: pos_providers::Family::QrPayment,
    name_key: "provider.qr.test_gateway",
    countries: &[],
    fields: &[
        pos_providers::fields::BASE_URL,
        pos_providers::SettingField {
            key: "api_key",
            label_key: "provider.field.api_key",
            kind: pos_providers::FieldKind::Secret { max_len: 64 },
            required: true,
        },
    ],
    capabilities: &[],
    sandbox: true,
};
static TEST_LEDGER: pos_providers::ProviderDescriptor = pos_providers::ProviderDescriptor {
    provider_id: "erp.test_ledger",
    family: pos_providers::Family::Erp,
    name_key: "provider.erp.test_ledger",
    countries: &[],
    fields: &[],
    capabilities: &[],
    sandbox: true,
};

/// The sealing key the tests configure.
fn test_integration_secret() -> pos_cloud::connections::ConnectionSecret {
    pos_cloud::connections::ConnectionSecret::new("ab".repeat(32))
}

fn connection_app(
    admin: FakeAdmin,
    connections: FakeConnections,
    secret: Option<pos_cloud::connections::ConnectionSecret>,
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
    let providers =
        pos_providers::Registry::new([&TEST_GATEWAY, &TEST_LEDGER]).expect("valid test catalogue");
    http::router(app).merge(http::connection_router(
        connections,
        Arc::new(providers),
        secret,
        admin,
        clock(),
        audit,
    ))
}

fn gateway_body(tenant: &str, api_key: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "tenant_id": tenant,
        "provider_id": "qr.test_gateway",
        "scope_level": "CONNECTION_SCOPE_TENANT",
        "display_name": "Test gateway",
        "settings": { "base_url": "https://gateway.example" },
    });
    if let Some(api_key) = api_key {
        body["secrets"] = serde_json::json!({ "api_key": api_key });
    }
    body
}

/// A credential is sealed before it reaches the store, answers only as "set", and is not in the
/// audit trail — and it opens again, for the connector, only under the configured key.
#[tokio::test]
async fn a_connection_seals_its_secret_and_no_read_or_trail_carries_it() {
    let audit = FakeAudit::default();
    let store = FakeConnections::default();
    let router = connection_app(
        provisioned_admin(),
        store.clone(),
        Some(test_integration_secret()),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let plaintext = "live-key-do-not-leak";

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/integrations/connections",
            &gateway_body(&tenant, Some(plaintext)),
            &cookie,
        ))
        .await
        .expect("route the create");
    assert_eq!(created.status(), StatusCode::CREATED);
    let body = json_body(created).await;
    assert_eq!(body["secrets_set"], serde_json::json!(["api_key"]));
    assert!(
        !body.to_string().contains(plaintext),
        "the create answers no secret: {body}"
    );
    let id = body["connection_id"].as_str().expect("an id").to_owned();

    let stored = store.stored(&id).expect("stored");
    let sealed = stored.sealed_secrets.get("api_key").expect("sealed");
    assert!(
        !sealed.contains(plaintext),
        "what reached the store is ciphertext"
    );
    let opened = pos_cloud::connections::open(
        &test_integration_secret(),
        pos_cloud::connections::SealedFor {
            tenant_id: TenantId::new(Ulid::from_u128(1)),
            connection_id: &id,
            field_key: "api_key",
        },
        sealed,
    )
    .expect("opens under the configured key");
    assert_eq!(opened.as_str(), plaintext);

    let listed = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/integrations/connections?tenant_id={tenant}"),
                &cookie,
            ))
            .await
            .expect("route the list"),
    )
    .await;
    assert!(!listed.to_string().contains(plaintext));
    assert!(listed[0].get("sealed_secrets").is_none(), "{listed}");

    let entry = last_trail_entry(&audit, "integrations.connection.create")
        .await
        .expect("the create is in the trail");
    let after = entry.after.expect("a summary");
    assert_eq!(after["secrets_set"], serde_json::json!(["api_key"]));
    assert!(
        !after.to_string().contains(plaintext) && !after.to_string().contains(sealed.as_str()),
        "the trail names which secret is set, and holds neither its value nor its ciphertext"
    );
}

/// An edit that does not resend the secret keeps it; switching vendor drops it, because one
/// vendor's key is never another's.
#[tokio::test]
async fn an_edit_keeps_the_stored_secret_and_switching_vendor_drops_it() {
    let store = FakeConnections::default();
    let router = connection_app(
        provisioned_admin(),
        store.clone(),
        Some(test_integration_secret()),
        Arc::new(AuditSink::new(FakeAudit::default())),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/integrations/connections",
            &gateway_body(&tenant, Some("k1")),
            &cookie,
        ))
        .await
        .expect("route the create");
    let version = etag_of(&created);
    let id = json_body(created).await["connection_id"]
        .as_str()
        .expect("an id")
        .to_owned();
    let row = format!("/admin/integrations/connections/{id}");
    let sealed_before = store.stored(&id).expect("stored").sealed_secrets;

    let mut renamed = gateway_body(&tenant, None);
    renamed["display_name"] = serde_json::json!("Renamed gateway");
    let edited = router
        .clone()
        .oneshot(put_with_etag(&row, &renamed, &cookie, &version))
        .await
        .expect("route the edit");
    assert_eq!(edited.status(), StatusCode::OK);
    let version = etag_of(&edited);
    assert_eq!(
        json_body(edited).await["secrets_set"],
        serde_json::json!(["api_key"])
    );
    assert_eq!(
        store.stored(&id).expect("stored").sealed_secrets,
        sealed_before,
        "the sealed value is kept byte for byte"
    );

    let mut switched = renamed.clone();
    switched["provider_id"] = serde_json::json!("erp.test_ledger");
    switched["settings"] = serde_json::json!({});
    let moved = router
        .clone()
        .oneshot(put_with_etag(&row, &switched, &cookie, &version))
        .await
        .expect("route the switch");
    assert_eq!(moved.status(), StatusCode::OK);
    assert_eq!(json_body(moved).await["secrets_set"], serde_json::json!([]));
    assert!(store.stored(&id).expect("stored").sealed_secrets.is_empty());
}

/// Every problem with the settings is named at once; a secret with nowhere to be sealed is
/// refused rather than stored; an unknown vendor is refused by name.
#[tokio::test]
async fn a_connection_the_schema_or_the_key_cannot_accept_is_refused() {
    let without_key = connection_app(
        provisioned_admin(),
        FakeConnections::default(),
        None,
        Arc::new(AuditSink::new(FakeAudit::default())),
    );
    let cookie = admin_cookie(&without_key).await;
    let tenant = ulid_text(1);

    let unsealable = without_key
        .clone()
        .oneshot(post_with_cookie(
            "/admin/integrations/connections",
            &gateway_body(&tenant, Some("k1")),
            &cookie,
        ))
        .await
        .expect("route");
    assert_eq!(unsealable.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        json_body(unsealable).await["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("integration_secret"))
    );

    let mut broken = gateway_body(&tenant, None);
    broken["settings"] = serde_json::json!({ "base_url": "http://plain.example", "colour": "red" });
    let refused = without_key
        .clone()
        .oneshot(post_with_cookie(
            "/admin/integrations/connections",
            &broken,
            &cookie,
        ))
        .await
        .expect("route");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let details = json_body(refused).await["error"]["details"].clone();
    let reasons: Vec<(String, String)> = details
        .as_array()
        .expect("details")
        .iter()
        .map(|detail| {
            (
                detail["field"].as_str().unwrap_or_default().to_owned(),
                detail["reason"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        reasons,
        vec![
            ("settings.colour".to_owned(), "SETTING_UNKNOWN".to_owned()),
            ("settings.base_url".to_owned(), "SETTING_INVALID".to_owned()),
            ("settings.api_key".to_owned(), "SETTING_MISSING".to_owned()),
        ]
    );

    let mut unknown = gateway_body(&tenant, None);
    unknown["provider_id"] = serde_json::json!("qr.nobody");
    let refused = without_key
        .oneshot(post_with_cookie(
            "/admin/integrations/connections",
            &unknown,
            &cookie,
        ))
        .await
        .expect("route");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(refused).await["error"]["details"][0]["reason"],
        "PROVIDER_UNKNOWN"
    );
}

/// A scope has one enabled ledger at most; a second is refused until the first is disabled, and a
/// Viewer may read connections but not write one.
#[tokio::test]
async fn a_scope_has_one_ledger_and_only_a_manager_writes() {
    let admin = provisioned_admin();
    let router = connection_app(
        admin.clone(),
        FakeConnections::default(),
        Some(test_integration_secret()),
        Arc::new(AuditSink::new(FakeAudit::default())),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let ledger = |name: &str, enabled: bool| {
        serde_json::json!({
            "tenant_id": tenant,
            "provider_id": "erp.test_ledger",
            "scope_level": "CONNECTION_SCOPE_TENANT",
            "display_name": name,
            "enabled": enabled,
        })
    };

    let first = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/integrations/connections",
            &ledger("Books", true),
            &cookie,
        ))
        .await
        .expect("route");
    assert_eq!(first.status(), StatusCode::CREATED);
    let second = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/integrations/connections",
            &ledger("Second books", true),
            &cookie,
        ))
        .await
        .expect("route");
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(second).await["error"]["details"][0]["reason"],
        "CONNECTION_SCOPE_TAKEN"
    );
    let parked = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/integrations/connections",
            &ledger("Second books, parked", false),
            &cookie,
        ))
        .await
        .expect("route");
    assert_eq!(
        parked.status(),
        StatusCode::CREATED,
        "a disabled one is kept for a switch back"
    );

    let viewer = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/integrations/connections?tenant_id={tenant}"),
            &viewer,
        ))
        .await
        .expect("route");
    assert_eq!(read.status(), StatusCode::OK);
    let write = router
        .oneshot(post_with_cookie(
            "/admin/integrations/connections",
            &ledger("A viewer's books", false),
            &viewer,
        ))
        .await
        .expect("route");
    assert_eq!(write.status(), StatusCode::FORBIDDEN);
}

// --- Integrations publish (ADR-0153 decision 5) --------------------------------------------------

/// A store connected to a card terminal of its own, sitting under a brand with a ledger, in a tenant
/// with a tenant-wide ledger and courier: the publish sends it the terminal (with its settings), the
/// brand's ledger alone (the tenant's is shadowed), and the courier (without its settings) — and no
/// secret, which the node cannot carry.
/// The publish router over a seeded tenant: a store under a brand, a tenant-wide ledger and courier,
/// the brand's own ledger, and the store's own card terminal — each with a setting and a sealed
/// secret, so what the node leaves out is as visible as what it keeps.
fn seeded_integrations_publish() -> (axum::Router, FakeConfigTrees) {
    use pos_cloud::connections::{Connection, ScopeLevel};

    static TERMINAL: pos_providers::ProviderDescriptor = pos_providers::ProviderDescriptor {
        provider_id: "card.test_terminal",
        family: pos_providers::Family::CardTerminal,
        name_key: "provider.card.test_terminal",
        countries: &[],
        fields: &[],
        capabilities: &[],
        sandbox: true,
    };
    static COURIER: pos_providers::ProviderDescriptor = pos_providers::ProviderDescriptor {
        provider_id: "courier.test_courier",
        family: pos_providers::Family::Courier,
        ..TERMINAL
    };

    let brand = pos_cloud::registry::BrandId::new(Ulid::from_u128(0xB1));
    let registry = FakeRegistry::default();
    registry.stores.lock().expect("lock").push(Versioned::new(
        StoreRecord {
            store_id: store_id(),
            tenant_id: tenant(),
            brand_id: Some(brand),
            name: "Store".to_owned(),
            status: EntityStatus::Active,
        },
        Version::new("1"),
    ));
    let connection = |id: &str, provider: &str, scope: (ScopeLevel, Option<String>)| Connection {
        connection_id: id.to_owned(),
        provider_id: provider.to_owned(),
        scope_level: scope.0,
        scope_id: scope.1,
        display_name: format!("Connection {id}"),
        enabled: true,
        settings: BTreeMap::from([(
            "address".to_owned(),
            pos_providers::SettingValue::Text("192.0.2.40".to_owned()),
        )]),
        sealed_secrets: BTreeMap::from([("api_key".to_owned(), "c1phertext".to_owned())]),
    };
    let connections = FakeConnections::default();
    for row in [
        connection("01A", "erp.test_ledger", (ScopeLevel::Tenant, None)),
        connection(
            "01B",
            "erp.test_ledger",
            (ScopeLevel::Brand, Some(brand.to_string())),
        ),
        connection("01C", "courier.test_courier", (ScopeLevel::Tenant, None)),
        connection(
            "01D",
            "card.test_terminal",
            (ScopeLevel::Store, Some(store_id().to_string())),
        ),
    ] {
        connections
            .rows
            .lock()
            .expect("lock")
            .push((tenant(), Versioned::new(row, Version::new("1"))));
    }

    let admin = provisioned_admin();
    let config_trees = FakeConfigTrees::default();
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    );
    let providers = pos_providers::Registry::new([&TEST_LEDGER, &COURIER, &TERMINAL])
        .expect("valid test catalogue");
    let router = http::router(app).merge(http::config_integrations_router(
        connections,
        Arc::new(providers),
        registry,
        config_trees.clone(),
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ));
    (router, config_trees)
}

#[tokio::test]
async fn publishing_integrations_sends_a_store_what_serves_it_and_no_secret() {
    let (router, config_trees) = seeded_integrations_publish();
    let cookie = admin_cookie(&router).await;

    let published = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/integrations",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id().as_ulid().to_string(),
            }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    assert_eq!(json_body(published).await["integrations"], 3);

    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    let node = &state.record.layers[2]["integrations"];
    let ids: Vec<&str> = node["integrations"]
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|integration| integration["connection_id"].as_str())
        .collect();
    assert_eq!(ids, vec!["01D", "01B", "01C"]);
    assert_eq!(
        node["integrations"][0]["family"],
        "INTEGRATION_FAMILY_CARD_TERMINAL"
    );
    assert_eq!(node["integrations"][0]["settings"]["address"], "192.0.2.40");
    assert!(
        node["integrations"][2].get("settings").is_none(),
        "a courier is the cloud's to drive; its settings stay there"
    );
    assert!(
        !node.to_string().contains("c1phertext"),
        "no secret, sealed or not: {node}"
    );

    // A store the tenant does not have is refused, not published to.
    let unknown = router
        .oneshot(put_with_cookie(
            "/admin/config/integrations",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": Ulid::from_u128(0xDEAD).to_string(),
            }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}
