// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The gateway endpoint routes (ADR-0198 decisions 2 to 5): a tenant declares the `https` targets a
//! store's lookups name by id. The URL is vetted by the webhook guard, the fields and the deadline
//! are checked, and every secret header is sealed before it reaches the store and never answered.
//! IP-literal URLs throughout, so the guard needs no DNS.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use pos_cloud::audit::{AuditRecorder, AuditSink};
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::gateway::endpoints::{
    GatewayEndpoint, GatewayEndpointStore, GatewayEndpointStoreError,
};
use pos_cloud::version::{CreateOutcome, UpdateOutcome, Version, Versioned};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::TenantId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeAudit, FakeConfigTrees, FakeKeys, FakeRollups, FakeWebhooks, NOW_MS,
    admin_cookie, app_all, clock, delete_with_cookie, etag_of, get_with_cookie, json_body,
    last_trail_entry, post_with_cookie, provisioned_admin, put_with_etag, role_session_cookie,
    ulid_text,
};

type Row = (TenantId, Versioned<GatewayEndpoint>);

/// A version-carrying, tenant-scoped in-memory store that lets a test read back exactly what
/// reached it: where "sealed before it is stored" is proved.
#[derive(Default, Clone)]
pub(crate) struct FakeEndpoints {
    rows: Arc<Mutex<Vec<Row>>>,
    next_version: Arc<Mutex<u64>>,
}

impl FakeEndpoints {
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }

    fn stored(&self, id: &str) -> Option<GatewayEndpoint> {
        self.rows
            .lock()
            .expect("lock")
            .iter()
            .find(|(_, row)| row.record.gateway_endpoint_id == id)
            .map(|(_, row)| row.record.clone())
    }
}

impl GatewayEndpointStore for FakeEndpoints {
    async fn list(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<GatewayEndpoint>>, GatewayEndpointStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|(owner, _)| *owner == tenant_id)
            .map(|(_, row)| row.clone())
            .collect())
    }

    async fn get(
        &self,
        tenant_id: TenantId,
        id: &str,
    ) -> Result<Option<Versioned<GatewayEndpoint>>, GatewayEndpointStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|(owner, row)| *owner == tenant_id && row.record.gateway_endpoint_id == id)
            .map(|(_, row)| row.clone()))
    }

    async fn create(
        &self,
        tenant_id: TenantId,
        endpoint: &GatewayEndpoint,
    ) -> Result<CreateOutcome, GatewayEndpointStoreError> {
        let version = self.mint();
        self.rows.lock().expect("lock").push((
            tenant_id,
            Versioned::edited(endpoint.clone(), version.clone(), NOW_MS),
        ));
        Ok(CreateOutcome::Created(version))
    }

    async fn update(
        &self,
        tenant_id: TenantId,
        endpoint: &GatewayEndpoint,
        expected: &Version,
    ) -> Result<UpdateOutcome, GatewayEndpointStoreError> {
        let version = self.mint();
        let mut rows = self.rows.lock().expect("lock");
        let Some((_, row)) = rows.iter_mut().find(|(owner, row)| {
            *owner == tenant_id && row.record.gateway_endpoint_id == endpoint.gateway_endpoint_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record = endpoint.clone();
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }

    async fn delete(&self, tenant_id: TenantId, id: &str) -> Result<(), GatewayEndpointStoreError> {
        self.rows
            .lock()
            .expect("lock")
            .retain(|(owner, row)| !(*owner == tenant_id && row.record.gateway_endpoint_id == id));
        Ok(())
    }
}

fn key() -> pos_cloud::connections::ConnectionSecret {
    pos_cloud::connections::ConnectionSecret::new("cd".repeat(32))
}

fn endpoint_app(
    admin: FakeAdmin,
    endpoints: FakeEndpoints,
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
    http::router(app).merge(http::gateway_endpoint_router(
        endpoints,
        secret,
        admin,
        clock(),
        audit,
    ))
}

const PLAINTEXT: &str = "demo-key-do-not-leak";

fn body(tenant: &str) -> serde_json::Value {
    serde_json::json!({
        "tenant_id": tenant,
        "display_name": "Demo loyalty",
        "url": "https://93.184.216.34/balance?token=in-the-query",
        "method": "GATEWAY_METHOD_POST",
        "request_fields": [{ "name": "card_code", "kind": "GATEWAY_FIELD_KIND_STRING", "required": true }],
        "response_fields": [{ "name": "points", "kind": "GATEWAY_FIELD_KIND_INTEGER" }],
        "idempotent": true,
        "secret_headers": { "X-Api-Key": PLAINTEXT },
    })
}

/// A secret header is sealed before it reaches the store, answers only as set, is not in the
/// trail, and opens again only under the configured key; the deadline defaults to three seconds;
/// the trail keeps the URL without its query.
#[tokio::test]
async fn an_endpoint_seals_its_secret_headers_and_no_read_or_trail_carries_them() {
    let audit = FakeAudit::default();
    let store = FakeEndpoints::default();
    let router = endpoint_app(
        provisioned_admin(),
        store.clone(),
        Some(key()),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/integrations/gateway-endpoints",
            &body(&tenant),
            &cookie,
        ))
        .await
        .expect("route the create");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(
        created["secret_headers_set"],
        serde_json::json!(["x-api-key"])
    );
    assert_eq!(created["deadline_ms"], 3_000);
    assert!(!created.to_string().contains(PLAINTEXT), "{created}");
    let id = created["gateway_endpoint_id"]
        .as_str()
        .expect("an id")
        .to_owned();

    let stored = store.stored(&id).expect("stored");
    let sealed = stored.sealed_headers.get("x-api-key").expect("sealed");
    assert!(
        !sealed.contains(PLAINTEXT),
        "what reached the store is ciphertext"
    );
    let opened = pos_cloud::connections::open(
        &key(),
        pos_cloud::connections::SealedFor {
            tenant_id: TenantId::new(Ulid::from_u128(1)),
            connection_id: &id,
            field_key: "header:x-api-key",
        },
        sealed,
    )
    .expect("opens under the configured key");
    assert_eq!(opened.as_str(), PLAINTEXT);

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/integrations/gateway-endpoints?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the list");
    let listed = json_body(listed).await;
    assert!(!listed.to_string().contains(PLAINTEXT));
    assert!(listed[0].get("sealed_headers").is_none(), "{listed}");

    let entry = last_trail_entry(&audit, "integrations.gateway_endpoint.create")
        .await
        .expect("the create is in the trail");
    let trail = entry.after.expect("a summary").to_string();
    assert!(!trail.contains(PLAINTEXT), "{trail}");
    assert!(
        !trail.contains("in-the-query"),
        "no query in the trail: {trail}"
    );
    assert!(trail.contains("x-api-key"));
}

/// What the guard and the checks refuse, each named on its field; and a secret header sent to a
/// cloud with no key to seal it is a 503 that stores nothing.
#[tokio::test]
async fn an_unsafe_url_a_reserved_header_a_bad_field_or_deadline_is_refused() {
    let store = FakeEndpoints::default();
    let router = endpoint_app(
        provisioned_admin(),
        store.clone(),
        Some(key()),
        Arc::new(pos_cloud::audit::NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let refused = |change: serde_json::Value| {
        let mut sent = body(&tenant);
        for (field, value) in change.as_object().expect("an object") {
            sent[field] = value.clone();
        }
        sent
    };
    for (change, field) in [
        (
            serde_json::json!({ "url": "http://93.184.216.34/x" }),
            "url",
        ),
        (serde_json::json!({ "url": "https://127.0.0.1/x" }), "url"),
        (serde_json::json!({ "url": "https://10.0.0.8/x" }), "url"),
        (
            serde_json::json!({ "secret_headers": { "Host": "x" } }),
            "secret_headers",
        ),
        (
            serde_json::json!({ "secret_headers": { "x-k": "a\r\nb: c" } }),
            "secret_headers",
        ),
        (serde_json::json!({ "deadline_ms": 20_000 }), "deadline_ms"),
        (
            serde_json::json!({ "request_fields": [{ "name": "Card", "kind": "GATEWAY_FIELD_KIND_STRING" }] }),
            "request_fields",
        ),
        (serde_json::json!({ "display_name": "  " }), "display_name"),
    ] {
        let response = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/integrations/gateway-endpoints",
                &refused(change.clone()),
                &cookie,
            ))
            .await
            .expect("route the create");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{change}");
        let answer = json_body(response).await.to_string();
        assert!(answer.contains(field), "{change}: {answer}");
    }
    assert!(
        store.rows.lock().expect("lock").is_empty(),
        "nothing stored"
    );

    let keyless = endpoint_app(
        provisioned_admin(),
        store.clone(),
        None,
        Arc::new(pos_cloud::audit::NoopAuditRecorder),
    );
    let cookie = admin_cookie(&keyless).await;
    let response = keyless
        .oneshot(post_with_cookie(
            "/admin/integrations/gateway-endpoints",
            &body(&tenant),
            &cookie,
        ))
        .await
        .expect("route the create");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(store.rows.lock().expect("lock").is_empty());
}

/// An update keeps a secret header it does not send, clears one it names, and is refused at a stale
/// version; a Viewer reads but cannot write; a delete removes it.
#[tokio::test]
async fn an_update_keeps_or_clears_headers_at_its_version_and_a_viewer_only_reads() {
    let store = FakeEndpoints::default();
    let admin = provisioned_admin();
    let router = endpoint_app(
        admin.clone(),
        store.clone(),
        Some(key()),
        Arc::new(pos_cloud::audit::NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/integrations/gateway-endpoints",
            &body(&tenant),
            &cookie,
        ))
        .await
        .expect("route the create");
    let first = etag_of(&created);
    let id = json_body(created).await["gateway_endpoint_id"]
        .as_str()
        .expect("an id")
        .to_owned();
    let uri = format!("/admin/integrations/gateway-endpoints/{id}");

    let mut renamed = body(&tenant);
    renamed["display_name"] = serde_json::json!("Renamed");
    renamed["secret_headers"] = serde_json::json!({});
    let updated = router
        .clone()
        .oneshot(put_with_etag(&uri, &renamed, &cookie, &first))
        .await
        .expect("route the update");
    assert_eq!(updated.status(), StatusCode::OK);
    let second = etag_of(&updated);
    assert_eq!(
        json_body(updated).await["secret_headers_set"],
        serde_json::json!(["x-api-key"]),
        "kept, because it was not sent"
    );

    let stale = router
        .clone()
        .oneshot(put_with_etag(&uri, &renamed, &cookie, &first))
        .await
        .expect("route the stale update");
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);

    renamed["clear_secret_headers"] = serde_json::json!(["X-API-KEY"]);
    let cleared = router
        .clone()
        .oneshot(put_with_etag(&uri, &renamed, &cookie, &second))
        .await
        .expect("route the clearing update");
    assert_eq!(
        json_body(cleared).await["secret_headers_set"],
        serde_json::json!([])
    );
    assert!(store.stored(&id).expect("stored").sealed_headers.is_empty());

    let viewer = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{uri}?tenant_id={tenant}"),
            &viewer,
        ))
        .await
        .expect("route the read");
    assert_eq!(read.status(), StatusCode::OK);
    let write = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/integrations/gateway-endpoints",
            &body(&tenant),
            &viewer,
        ))
        .await
        .expect("route the viewer's create");
    assert_eq!(write.status(), StatusCode::FORBIDDEN);

    let deleted = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("{uri}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the delete");
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let gone = router
        .oneshot(get_with_cookie(
            &format!("{uri}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
}
