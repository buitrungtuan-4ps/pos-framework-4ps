// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The order connector (ADR-0083 decision 3, roadmap-v3 B9.3), over the sandbox demo vendor and
//! fake data only: a vendor's signed webhook becomes a delivery order on the same relay `/v1/orders`
//! uses, and the store pulls it; everything a vendor or a prober could get wrong is refused before
//! the intake sees it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_cloud::connections::{
    Connection, ConnectionSecret, ConnectionStore as _, ScopeLevel, SealedFor, seal,
};
use pos_cloud::connectors::demo::{DemoConnector, SIGNATURE_HEADER, sign};
use pos_cloud::connectors::route::connector_router;
use pos_cloud::relay::{OrderRelay, orders_sync_router_with_cap};
use pos_cloud::wake::SharedWake;
use pos_proto::ids::{MenuItemId, TenantId};
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
        settings: [(
            "item_map".to_owned(),
            SettingValue::Text(format!("PIZZA-M={}", pizza())),
        )]
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

/// The connector over a real relay and an in-memory queue, merged with the store's pull, as the
/// binary wires them: one wake for both halves.
fn app(connections: FakeConnections, queue: FakeOrderQueue, keys: FakeKeys) -> axum::Router {
    let wake = SharedWake::new();
    connector_router(
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
        clock(),
        vec![Arc::new(DemoConnector)],
        Some(key()),
    )
    .merge(orders_sync_router_with_cap(
        queue,
        keys,
        clock(),
        wake,
        std::time::Duration::ZERO,
    ))
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
