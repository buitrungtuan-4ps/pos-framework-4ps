// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The order connector (ADR-0083 decision 3, roadmap-v3 B9.3), over the sandbox demo vendor and
//! fake data only: a vendor's signed webhook becomes a delivery order on the same relay `/v1/orders`
//! uses, and the store pulls it; everything a vendor or a prober could get wrong is refused before
//! the intake sees it.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_cloud::connections::{
    Connection, ConnectionSecret, ConnectionStore as _, ScopeLevel, SealedFor, seal,
};
use pos_cloud::connectors::answer::DeliveryOrders;
use pos_cloud::connectors::demo::{DemoConnector, SIGNATURE_HEADER, sign};
use pos_cloud::connectors::recipient::{KeptRecipient, RecipientStore, recipient_sync_router};
use pos_cloud::connectors::route::connector_router;
use pos_cloud::relay::{OrderRelay, orders_sync_router_with_cap};
use pos_cloud::wake::SharedWake;
use pos_ports::PortError;
use pos_proto::ids::{MenuItemId, StoreId, SubjectId, TenantId};
use pos_proto::integrations::SettingValue;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::integrations::FakeConnections;
use crate::{
    EmptyConfigTrees, FakeDirectory, FakeKeys, FakeOrderQueue, clock, get, issue_store_key,
    json_body, order_store, tenant,
};

const CONNECTION: &str = "01J0000000000000000000CNN1";
const SECRET: &str = "demo-webhook-secret";

fn key() -> ConnectionSecret {
    ConnectionSecret::new("a1".repeat(32))
}

fn pizza() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(0x0910))
}

/// A demo connection serving `scope`, holding the webhook secret sealed and a one-item map.
async fn connection(
    store: &FakeConnections,
    id: &str,
    provider: &str,
    scope: (ScopeLevel, Option<String>),
    enabled: bool,
) {
    // Sealed in the format before tenant keys, version 0, which the router still opens under
    // integration_secret (ADR-0217 decision 2).
    let sealed = seal(
        &key(),
        SealedFor {
            tenant_id: tenant(),
            connection_id: id,
            field_key: "webhook_secret",
        },
        SECRET,
    )
    .expect("seal");
    let record = Connection {
        connection_id: id.to_owned(),
        provider_id: provider.to_owned(),
        scope_level: scope.0,
        scope_id: scope.1,
        display_name: "Demo delivery".to_owned(),
        enabled,
        settings: [
            (
                "item_map".to_owned(),
                SettingValue::Text(format!("PIZZA-M={}", pizza())),
            ),
            (
                "answer_url".to_owned(),
                SettingValue::Text("https://vendor.example/answers".to_owned()),
            ),
        ]
        .into(),
        sealed_secrets: [("webhook_secret".to_owned(), sealed)].into(),
        processing_country: None,
    };
    store.create(tenant(), &record).await.expect("seed");
}

fn store_scope() -> (ScopeLevel, Option<String>) {
    (ScopeLevel::Store, Some(order_store().to_string()))
}

fn body(reference: &str, sku: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "order_ref": reference,
        "placed_at_ms": 1_800_000_000_000_i64,
        "items": [{ "sku": sku, "quantity": 2, "note": "extra napkins" }],
    }))
    .expect("json")
}

fn webhook(
    tenant_id: TenantId,
    connection_id: &str,
    body: Vec<u8>,
    signature: &str,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/connectors/{tenant_id}/{connection_id}/orders"))
        .header("content-type", "application/json")
        .header(SIGNATURE_HEADER, signature)
        .body(Body::from(body))
        .expect("a request")
}

fn signed(tenant_id: TenantId, connection_id: &str, body: Vec<u8>) -> Request<Body> {
    let signature = sign(&body, SECRET).expect("signs");
    webhook(tenant_id, connection_id, body, &signature)
}

/// The store's personal-data retention the fake recipients report, in days.
const STORE_RETENTION_DAYS: u16 = 30;

/// Kept recipients in memory, by tenant and store, under a store retention of
/// [`STORE_RETENTION_DAYS`]; a subject already kept stays as it is.
#[derive(Clone, Default)]
struct FakeRecipients {
    kept: Arc<Mutex<Vec<(TenantId, StoreId, KeptRecipient)>>>,
}

impl RecipientStore for FakeRecipients {
    async fn retention_days(
        &self,
        _tenant: TenantId,
        _store_id: StoreId,
    ) -> Result<Option<u16>, PortError> {
        Ok(Some(STORE_RETENTION_DAYS))
    }

    async fn keep(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        recipient: &KeptRecipient,
    ) -> Result<(), PortError> {
        let mut kept = self.kept.lock().expect("lock");
        if !kept
            .iter()
            .any(|(_, _, row)| row.subject_id == recipient.subject_id)
        {
            kept.push((tenant, store_id, recipient.clone()));
        }
        Ok(())
    }

    async fn fetch(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        subject_id: SubjectId,
    ) -> Result<Option<KeptRecipient>, PortError> {
        Ok(self
            .kept
            .lock()
            .expect("lock")
            .iter()
            .find(|(t, s, row)| *t == tenant && *s == store_id && row.subject_id == subject_id)
            .map(|(_, _, row)| row.clone()))
    }
}

/// The connector over a real relay and an in-memory queue, merged with the store's pull, as the
/// binary wires them: one wake for both halves.
fn app(connections: FakeConnections, queue: FakeOrderQueue, keys: FakeKeys) -> axum::Router {
    app_keeping(connections, queue, keys, FakeRecipients::default())
}

/// [`app`] keeping recipients in `recipients`, with the store's read of them merged.
fn app_keeping(
    connections: FakeConnections,
    queue: FakeOrderQueue,
    keys: FakeKeys,
    recipients: FakeRecipients,
) -> axum::Router {
    let wake = SharedWake::new();
    connector_router(
        (
            connections,
            OrderRelay::new(
                FakeDirectory {
                    owner: Some(tenant()),
                },
                EmptyConfigTrees,
                queue.clone(),
                clock(),
                wake.clone(),
            ),
            recipients.clone(),
        ),
        clock(),
        vec![Arc::new(DemoConnector)],
        Some(crate::sealing_keys::file_keys("connectors", &key())),
    )
    .merge(recipient_sync_router(recipients, keys.clone(), clock()))
    .merge(orders_sync_router_with_cap(
        queue,
        keys,
        clock(),
        wake,
        std::time::Duration::ZERO,
    ))
}

/// A demo order naming who it goes to, with fake data only.
fn delivered(reference: &str, address: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "order_ref": reference,
        "placed_at_ms": 1_800_000_000_000_i64,
        "items": [{ "sku": "PIZZA-M", "quantity": 1 }],
        "deliver_to": { "name": "Test Guest", "phone": "000-0000", "address": address },
    }))
    .expect("json")
}

/// A vendor order's recipient is kept apart as a subject record for its store, under the store's
/// retention: the queued order carries only its `subject_id`, a retry keeps no second copy, and
/// only the store's own relay credential reads it back.
#[tokio::test(start_paused = true)]
async fn a_delivery_recipient_is_kept_apart_and_only_its_store_reads_it() {
    let connections = FakeConnections::default();
    connection(
        &connections,
        CONNECTION,
        "delivery.demo",
        store_scope(),
        true,
    )
    .await;
    let queue = FakeOrderQueue::new();
    let keys = FakeKeys::default();
    let scope = [pos_cloud::auth::apikey::Scope::RelayOrders];
    let relay = issue_store_key(&keys, tenant(), order_store(), &scope);
    let elsewhere = issue_store_key(
        &keys,
        tenant(),
        StoreId::new(Ulid::from_u128(0xE15E)),
        &scope,
    );
    let recipients = FakeRecipients::default();
    let app = app_keeping(connections, queue, keys, recipients.clone());

    for _attempt in 0..2 {
        let submitted = app
            .clone()
            .oneshot(signed(
                tenant(),
                CONNECTION,
                delivered("D-7", "1 Example Street"),
            ))
            .await
            .expect("route the webhook");
        assert_eq!(
            submitted.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "queued"
        );
    }
    let kept = recipients.kept.lock().expect("lock").clone();
    assert_eq!(kept.len(), 1, "the vendor's retry kept no second copy");
    let (kept_tenant, kept_store, record) = &kept[0];
    assert_eq!((*kept_tenant, *kept_store), (tenant(), order_store()));
    assert_eq!(
        record
            .retain_until
            .map(|until| until.as_milliseconds_since_epoch()
                - record.collected_at.as_milliseconds_since_epoch()),
        Some(i64::from(STORE_RETENTION_DAYS) * 24 * 60 * 60 * 1_000),
        "kept for the store's retention from when the cloud kept it"
    );

    let pulled = app
        .clone()
        .oneshot(get(
            &format!("/sync/stores/{}/orders", order_store()),
            Some(&relay),
        ))
        .await
        .expect("route the pull");
    let pulled = json_body(pulled).await;
    assert_eq!(
        pulled[0]["order"]["subject_id"],
        record.subject_id.to_string()
    );
    let text = pulled.to_string();
    for value in ["Test Guest", "000-0000", "Example Street"] {
        assert!(!text.contains(value), "the order carries no personal value");
    }

    let path = format!(
        "/sync/stores/{}/subjects/{}",
        order_store(),
        record.subject_id
    );
    let read = app
        .clone()
        .oneshot(get(&path, Some(&relay)))
        .await
        .expect("route the read");
    assert_eq!(read.status(), StatusCode::OK);
    let read = json_body(read).await;
    assert_eq!(read["fields"]["address"], "1 Example Street");
    assert_eq!(read["fields"]["name"], "Test Guest");
    assert!(read["retain_until_time"].is_i64());

    only_its_store_reads(app, &path, &relay, &elsewhere).await;
}

/// Another store's credential is refused, an unknown subject is not found, and no credential is
/// unauthenticated.
async fn only_its_store_reads(app: axum::Router, path: &str, relay: &str, elsewhere: &str) {
    let theirs = app
        .clone()
        .oneshot(get(path, Some(elsewhere)))
        .await
        .expect("route");
    assert_eq!(
        theirs.status(),
        StatusCode::FORBIDDEN,
        "another store's credential"
    );
    let unknown = format!(
        "/sync/stores/{}/subjects/{}",
        order_store(),
        SubjectId::new(Ulid::from_u128(77))
    );
    let unknown = app
        .clone()
        .oneshot(get(&unknown, Some(relay)))
        .await
        .expect("route");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let anonymous = app.oneshot(get(path, None)).await.expect("route");
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
}

/// A recipient with a field missing or too long refuses the order, naming the field and never its
/// value, and nothing is kept or queued.
#[tokio::test(start_paused = true)]
async fn a_bad_recipient_refuses_the_order_and_keeps_nothing() {
    let connections = FakeConnections::default();
    connection(
        &connections,
        CONNECTION,
        "delivery.demo",
        store_scope(),
        true,
    )
    .await;
    let queue = FakeOrderQueue::new();
    let recipients = FakeRecipients::default();
    let app = app_keeping(
        connections,
        queue.clone(),
        FakeKeys::default(),
        recipients.clone(),
    );
    let refused = app
        .oneshot(signed(tenant(), CONNECTION, delivered("D-8", "   ")))
        .await
        .expect("route");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert!(json_body(refused).await.to_string().contains("address"));
    assert!(recipients.kept.lock().expect("lock").is_empty());
    assert!(queue.entries.lock().expect("lock").is_empty());
}

/// A demo vendor's signed order reaches the store as a delivery order: the store pulls it with its
/// mapped item and the reference keyed by the connection, and the vendor's retry queues nothing new.
#[tokio::test(start_paused = true)]
async fn a_signed_vendor_order_reaches_the_store_through_the_cloud() {
    let connections = FakeConnections::default();
    connection(
        &connections,
        CONNECTION,
        "delivery.demo",
        store_scope(),
        true,
    )
    .await;
    let queue = FakeOrderQueue::new();
    let keys = FakeKeys::default();
    let relay = issue_store_key(
        &keys,
        tenant(),
        order_store(),
        &[pos_cloud::auth::apikey::Scope::RelayOrders],
    );
    let app = app(connections, queue, keys);

    // The store is silent, so the relay answers that the order is queued for it.
    let submitted = app
        .clone()
        .oneshot(signed(tenant(), CONNECTION, body("D-1001", "PIZZA-M")))
        .await
        .expect("route the webhook");
    assert_eq!(submitted.status(), StatusCode::SERVICE_UNAVAILABLE);
    let retried = app
        .clone()
        .oneshot(signed(tenant(), CONNECTION, body("D-1001", "PIZZA-M")))
        .await
        .expect("route the retry");
    assert_eq!(retried.status(), StatusCode::SERVICE_UNAVAILABLE);

    let pulled = app
        .oneshot(get(
            &format!("/sync/stores/{}/orders", order_store()),
            Some(&relay),
        ))
        .await
        .expect("route the pull");
    assert_eq!(pulled.status(), StatusCode::OK);
    let pulled = json_body(pulled).await;
    let orders = pulled.as_array().expect("a list");
    assert_eq!(
        orders.len(),
        1,
        "the vendor's retry queued nothing new: {pulled}"
    );
    let order = &orders[0]["order"];
    assert_eq!(order["sales_channel"], "SALES_CHANNEL_DELIVERY");
    assert_eq!(order["external_reference"], format!("{CONNECTION}:D-1001"));
    assert_eq!(order["lines"][0]["menu_item_id"], pizza().to_string());
    assert_eq!(order["lines"][0]["quantity_milli"], 2_000);
}

/// What is refused before the intake sees anything: a bad or missing signature (`401`); a
/// connection that is unknown, another tenant's, disabled, not store-scoped, or of a provider with
/// no connector (one `404`); a body that is not the vendor's order (`400`); an item the map does
/// not hold (`422`, naming it); and a body over 64 KiB (`413`).
#[tokio::test(start_paused = true)]
async fn what_the_connector_refuses_before_the_intake() {
    let connections = FakeConnections::default();
    connection(
        &connections,
        CONNECTION,
        "delivery.demo",
        store_scope(),
        true,
    )
    .await;
    connection(
        &connections,
        "01J0000000000000000000CNN2",
        "delivery.demo",
        store_scope(),
        false,
    )
    .await;
    connection(
        &connections,
        "01J0000000000000000000CNN3",
        "delivery.demo",
        (ScopeLevel::Tenant, None),
        true,
    )
    .await;
    connection(
        &connections,
        "01J0000000000000000000CNN4",
        "courier.ahamove",
        store_scope(),
        true,
    )
    .await;
    let queue = FakeOrderQueue::new();
    let app = app(connections, queue.clone(), FakeKeys::default());
    let status = |request: Request<Body>| {
        let app = app.clone();
        async move { app.oneshot(request).await.expect("route").status() }
    };

    let good = || body("D-2", "PIZZA-M");
    assert_eq!(
        status(webhook(tenant(), CONNECTION, good(), "sha256=00")).await,
        StatusCode::UNAUTHORIZED
    );
    let other_secret = sign(&good(), "not-the-secret").expect("signs");
    assert_eq!(
        status(webhook(tenant(), CONNECTION, good(), &other_secret)).await,
        StatusCode::UNAUTHORIZED
    );
    for (tenant_id, id) in [
        (tenant(), "01J0000000000000000000CNN9"),
        (TenantId::new(Ulid::from_u128(0xBAD)), CONNECTION),
        (tenant(), "01J0000000000000000000CNN2"),
        (tenant(), "01J0000000000000000000CNN3"),
        (tenant(), "01J0000000000000000000CNN4"),
    ] {
        assert_eq!(
            status(signed(tenant_id, id, good())).await,
            StatusCode::NOT_FOUND,
            "{id}"
        );
    }
    assert_eq!(
        status(signed(tenant(), CONNECTION, b"{\"order_ref\":1}".to_vec())).await,
        StatusCode::BAD_REQUEST
    );
    let unmapped = app
        .clone()
        .oneshot(signed(tenant(), CONNECTION, body("D-3", "SUSHI")))
        .await
        .expect("route");
    assert_eq!(unmapped.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(json_body(unmapped).await.to_string().contains("SUSHI"));
    let large = vec![b' '; 65 * 1024];
    assert_eq!(
        status(signed(tenant(), CONNECTION, large)).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert!(
        queue.entries.lock().expect("lock").is_empty(),
        "nothing reached the intake"
    );
}

/// A vendor endpoint that records what it is sent and answers `status`.
struct Vendor {
    sent: Mutex<Vec<pos_cloud::gateway::call::OutboundRequest>>,
    status: u16,
}

impl pos_cloud::gateway::call::GatewayTransport for Vendor {
    async fn send(
        &self,
        request: &pos_cloud::gateway::call::OutboundRequest,
    ) -> Result<pos_cloud::gateway::call::TargetAnswer, String> {
        self.sent
            .lock()
            .expect("lock")
            .push(pos_cloud::gateway::call::OutboundRequest {
                method: request.method,
                url: request.url.clone(),
                headers: request.headers.clone(),
                body: request.body.clone(),
            });
        Ok(pos_cloud::gateway::call::TargetAnswer {
            status: self.status,
            body: Vec::new(),
        })
    }
}

/// ADR-0215 decision 4: a store answers its vendor through the cloud. The answer reaches the
/// connection's `answer_url` signed as the vendor's webhooks are, under the vendor's own reference,
/// with the same idempotency key on a retry; a malformed answer, a vendor that is not this store's
/// and a key without `relay_orders` are refused before anything is sent.
#[tokio::test]
async fn a_store_answers_its_delivery_vendor_through_the_cloud() {
    use pos_cloud::connectors::answer::delivery_answer_router;

    let connections = FakeConnections::default();
    connection(
        &connections,
        CONNECTION,
        "delivery.demo",
        store_scope(),
        true,
    )
    .await;
    let keys = FakeKeys::default();
    let relay = issue_store_key(
        &keys,
        tenant(),
        order_store(),
        &[pos_cloud::auth::apikey::Scope::RelayOrders],
    );
    let other = issue_store_key(&keys, tenant(), order_store(), &[]);
    let vendor = Arc::new(Vendor {
        sent: Mutex::new(Vec::new()),
        status: 200,
    });
    let app = delivery_answer_router(
        (connections, FakeOrders),
        keys,
        clock(),
        Arc::clone(&vendor),
        vec![Arc::new(DemoConnector)],
        Some(crate::sealing_keys::file_keys("connectors", &key())),
    );
    let uri = format!("/sync/stores/{}/delivery/answers", order_store());
    let accept = serde_json::json!({ "vendor_id": CONNECTION,
        "external_reference": format!("{CONNECTION}:D-1001"), "answer": "DELIVERY_ANSWER_ACCEPT", "prep_minutes": 15 });
    for _ in 0..2 {
        let answered = app
            .clone()
            .oneshot(crate::post_json_bearer(&uri, &accept, &relay))
            .await
            .expect("answers");
        assert_eq!(answered.status(), StatusCode::NO_CONTENT);
    }
    let sent = std::mem::take(&mut *vendor.sent.lock().expect("lock"));
    assert_eq!(sent.len(), 2);
    let first = sent.first().expect("sent");
    assert_eq!(first.url, "https://vendor.example/answers");
    let body = first.body.clone().expect("a body");
    let read: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(read["order_ref"], "D-1001", "the vendor's own reference");
    assert_eq!(read["answer"], "accept");
    let header = |request: &pos_cloud::gateway::call::OutboundRequest, name: &str| {
        request
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(header(first, SIGNATURE_HEADER), sign(&body, SECRET));
    assert_eq!(
        header(first, "Idempotency-Key"),
        header(sent.get(1).expect("retried"), "Idempotency-Key")
    );

    for (request, token, status) in [
        (
            serde_json::json!({ "vendor_id": CONNECTION, "external_reference": "x", "answer": "DELIVERY_ANSWER_REJECT" }),
            &relay,
            StatusCode::BAD_REQUEST,
        ),
        (
            serde_json::json!({ "vendor_id": CONNECTION, "answer": "DELIVERY_ANSWER_READY" }),
            &relay,
            StatusCode::BAD_REQUEST,
        ),
        (
            serde_json::json!({ "vendor_id": Ulid::from_u128(9).to_string(), "external_reference": "x", "answer": "DELIVERY_ANSWER_READY" }),
            &relay,
            StatusCode::NOT_FOUND,
        ),
        (accept.clone(), &other, StatusCode::FORBIDDEN),
    ] {
        let answered = app
            .clone()
            .oneshot(crate::post_json_bearer(&uri, &request, token))
            .await
            .expect("answers");
        assert_eq!(answered.status(), status, "{request}");
    }
    assert!(
        vendor.sent.lock().expect("lock").is_empty(),
        "nothing refused was sent"
    );

    answers_by_order_id(app, &uri, &relay, &vendor).await;
}

/// A store that names the order by the id it gave it: the cloud finds the vendor's reference from
/// the acceptance the store reported, and refuses an order it reported none of.
async fn answers_by_order_id(app: axum::Router, uri: &str, relay: &str, vendor: &Vendor) {
    let by_order = |order_id: Ulid| {
        serde_json::json!({ "vendor_id": CONNECTION, "order_id": order_id.to_string(),
            "answer": "DELIVERY_ANSWER_REJECT", "reason_code_id": Ulid::from_u128(3).to_string() })
    };
    let answered = app
        .clone()
        .oneshot(crate::post_json_bearer(uri, &by_order(TAKEN_ORDER), relay))
        .await
        .expect("answers");
    assert_eq!(answered.status(), StatusCode::NO_CONTENT);
    let sent = std::mem::take(&mut *vendor.sent.lock().expect("lock"));
    let read: serde_json::Value =
        serde_json::from_slice(&sent[0].body.clone().expect("a body")).expect("json");
    assert_eq!(read["order_ref"], "D-1001");
    assert_eq!(read["answer"], "reject");
    let unknown = app
        .oneshot(crate::post_json_bearer(
            uri,
            &by_order(Ulid::from_u128(77)),
            relay,
        ))
        .await
        .expect("answers");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

/// The one order the fake store took, as the id it gave it.
const TAKEN_ORDER: Ulid = Ulid::from_u128(0x0D1);

/// The orders a store took: [`TAKEN_ORDER`] at this connection's `D-1001`, for [`order_store`].
#[derive(Clone)]
struct FakeOrders;

impl DeliveryOrders for FakeOrders {
    async fn reference_of(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        order_id: pos_proto::ids::OrderId,
    ) -> Result<Option<String>, PortError> {
        Ok((tenant_id == tenant()
            && store_id == order_store()
            && order_id.as_ulid() == TAKEN_ORDER)
            .then(|| format!("{CONNECTION}:D-1001")))
    }
}
