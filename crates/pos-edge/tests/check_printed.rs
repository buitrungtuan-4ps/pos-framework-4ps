// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A printed check over HTTP ([ADR-0218](../../../docs/adr/0218-a-printed-check-is-an-event-every-till-sees.md)):
//! the print records it, every check read shows it, a change marks it stale, and a store that
//! requires one refuses payment without a fresh one, naming `CHECK_REQUIRED`.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::permission::PermissionSet;
use pos_edge::pairing::Minter;
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StaffAuth,
    StaffRoster, StoreIdentity, SystemClock,
};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::billing::PublishedBilling;
use pos_proto::ids::{EmployeeId, MenuItemId, StoreId, TableId};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::ulid::Ulid;
use serde_json::{Value, json};
use tower::ServiceExt;

const CODE: &str = "C01";
const PIN: &str = "2468";

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), b"a-fixed-test-slt")
        .expect("hash")
        .to_string()
}

/// A signed-in till on a store whose `billing` node is `billing`.
async fn till(billing: PublishedBilling) -> (Router, String) {
    let mut roster = StaffRoster::new();
    roster.insert(
        CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::default(),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(PIN)),
        },
    );
    let edge = Edge::new(
        FakeStore::default(),
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(7))),
        EdgeSession {
            billing,
            ..EdgeSession::bootstrap().with_staff(roster)
        },
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed");
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let (code, _) = pairing.mint(now, Minter::Boot).expect("mint");
    let token = pairing
        .redeem(&code, now)
        .await
        .expect("redeem")
        .token()
        .expect("pairs")
        .as_str()
        .to_owned();
    let app = pos_edge::http::domain_router(
        Arc::new(edge),
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    );
    let (status, _, _) = send(
        &app,
        &token,
        "POST",
        "/api/session/sign-in",
        Some(json!({ "code": CODE, "pin": PIN })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    (app, token)
}

async fn send(
    app: &Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Option<String>, Value) {
    let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(body)
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("answers");
    let status = response.status();
    let reason = response
        .headers()
        .get("pos-error-reason")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        reason,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

/// One 150,000₫ line at 10 %, as the till captures it.
fn line() -> Value {
    json!({
        "menu_item_id": MenuItemId::new(Ulid::from_u128(500)),
        "display_name": "Margherita",
        "quantity": Quantity::ONE,
        "unit_price": vnd(150_000),
        "line_total": vnd(150_000),
        "tax_class_id": EdgeSession::standard_tax_class(),
        "tax_rate": Ratio::basis_points(1_000).expect("a rate"),
        "note_present": false,
    })
}

#[tokio::test]
async fn a_printed_check_shows_on_every_read_and_turns_stale_when_the_order_changes() {
    let (app, token) = till(PublishedBilling::default()).await;
    let table = TableId::new(Ulid::from_u128(700));
    send(
        &app,
        &token,
        "POST",
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    send(
        &app,
        &token,
        "POST",
        &format!("/api/tables/{table}/lines"),
        Some(line()),
    )
    .await;
    let check = format!("/api/tables/{table}/check");
    let (_, _, before) = send(&app, &token, "GET", &check, None).await;
    assert!(before.get("check_printed").is_none(), "{before}");

    let (status, _, _) = send(
        &app,
        &token,
        "POST",
        &format!("/api/tables/{table}/check/print"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, printed) = send(&app, &token, "GET", &check, None).await;
    assert_eq!(printed["check_printed"]["copy"], 1, "{printed}");
    assert_eq!(printed["check_printed"]["stale"], false);
    assert_eq!(printed["check_printed"]["total_due"], json!(vnd(165_000)));

    send(
        &app,
        &token,
        "POST",
        &format!("/api/tables/{table}/lines"),
        Some(line()),
    )
    .await;
    let (_, _, changed) = send(&app, &token, "GET", &check, None).await;
    assert_eq!(changed["check_printed"]["stale"], true, "{changed}");
}

#[tokio::test]
async fn a_store_that_requires_a_check_refuses_payment_without_a_fresh_one() {
    let (app, token) = till(PublishedBilling {
        check_before_payment: Some(true),
        ..PublishedBilling::default()
    })
    .await;
    let table = TableId::new(Ulid::from_u128(701));
    send(
        &app,
        &token,
        "POST",
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    send(
        &app,
        &token,
        "POST",
        &format!("/api/tables/{table}/lines"),
        Some(line()),
    )
    .await;
    let (_, _, bill) = send(
        &app,
        &token,
        "POST",
        &format!("/api/tables/{table}/bill"),
        None,
    )
    .await;
    let bill_id = bill["bill_id"].as_str().expect("a bill").to_owned();
    let pay = json!({ "payments": [{
        "method": "PAYMENT_METHOD_CASH",
        "tendered": vnd(165_000),
        "applied_to_bill": vnd(165_000),
    }] });
    let settle = format!("/api/bills/{bill_id}/settle");
    let (status, reason, _) = send(&app, &token, "POST", &settle, Some(pay.clone())).await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("CHECK_REQUIRED"))
    );

    send(
        &app,
        &token,
        "POST",
        &format!("/api/bills/{bill_id}/check/print"),
        None,
    )
    .await;
    let (status, _, settled) = send(&app, &token, "POST", &settle, Some(pay)).await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["receipt_number"], 1);
}
