// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The order, bill and shift routes over HTTP, driven without a socket (P5).
//!
//! Proves a device can carry a table through the whole sell cycle over HTTP — seat, add a line, fire
//! it, open the bill, settle it for a gapless receipt, and cycle the table to clean — and that the
//! cash shift opens, counts blind, and closes with a variance, all through the same router the
//! shipped binary serves.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::permission::PermissionSet;
use pos_edge::{
    Edge, EdgeSession, InMemoryReceipts, Pairing, Sessions, StaffAuth, StaffRoster, StoreIdentity,
    SystemClock,
};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::CurrencyCode;
use pos_proto::ids::{EmployeeId, MenuItemId, StationId, StoreId, TableId};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::ulid::Ulid;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The badge code + PIN seeded into the store's roster, and the employee they sign in as.
const STAFF_CODE: &str = "C01";
const STAFF_PIN: &str = "2468";

/// A real Argon2id PHC hash of `pin`, computed with a fixed salt so the test needs no RNG — the same
/// recipe the offline-auth unit tests use.
fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHasher, SaltString};
    let salt = SaltString::encode_b64(b"fixed-test-salt!").expect("salt");
    Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .expect("hash")
        .to_string()
}

/// The domain router plus a bearer token for a device that is paired *and signed in* — every command
/// route now requires both (S0b, ADR-0084). The store is seeded with one staff member so the device
/// can sign in over the real route.
async fn app() -> (Router, String) {
    let identity = StoreIdentity::for_store(StoreId::new(Ulid::from_u128(7)));
    let mut roster = StaffRoster::new();
    roster.insert(
        STAFF_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::default(),
            pin_phc: Some(hash_of(STAFF_PIN)),
        },
    );
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            identity,
            EdgeSession::bootstrap().with_staff(roster),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let code = pairing.mint(now).expect("mint a pairing code");
    let token = pairing
        .redeem(&code, now)
        .await
        .expect("redeem")
        .expect("a fresh code pairs a device")
        .as_str()
        .to_owned();
    let service = pos_edge::http::domain_router(edge, pairing, Arc::new(Sessions::new()));
    // Sign the paired device in, so the command routes below run under a real employee.
    let (status, _) = send(
        service.clone(),
        &token,
        "POST",
        "/api/session/sign-in",
        Some(json!({ "code": STAFF_CODE, "pin": STAFF_PIN })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the seeded staff signs in");
    (service, token)
}

async fn send(
    app: Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(body)
        .expect("request builds");
    let response = app.oneshot(request).await.expect("router responds");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn a_line_body() -> Value {
    json!({
        "menu_item_id": MenuItemId::new(Ulid::from_u128(500)),
        "display_name": "Margherita",
        "quantity": Quantity::ONE,
        "unit_price": vnd(150_000),
        "line_total": vnd(150_000),
        "tax_class_id": EdgeSession::standard_tax_class(),
        "tax_rate": Ratio::basis_points(1_000).expect("a valid rate"),
        "note_present": false,
    })
}

#[tokio::test]
async fn a_table_sells_end_to_end_over_http() {
    let (app, token) = app().await;
    let table = TableId::new(Ulid::from_u128(700));

    // Seat: the table opens.
    let (status, view) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["state"], "TABLE_STATE_OCCUPIED");

    // Add a line, then fire it to a station.
    let (status, line) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/lines"),
        Some(a_line_body()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(line["state"], "ORDER_LINE_STATE_ADDED");
    let line_id = line["order_line_id"]
        .as_str()
        .expect("a line id")
        .to_owned();

    let station = StationId::new(Ulid::from_u128(9));
    let (status, fired) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/lines/{line_id}/fire"),
        Some(json!({ "station_id": station })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fired["state"], "ORDER_LINE_STATE_FIRED");

    // Open the bill: the table moves to awaiting payment.
    let (status, bill) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/bill"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bill["state"], "BILL_STATE_OPEN");
    assert_eq!(bill["table_state"], "TABLE_STATE_AWAITING_PAYMENT");
    let bill_id = bill["bill_id"].as_str().expect("a bill id").to_owned();

    // Settle 165k (150k + 10% tax) in cash: gapless receipt one, table needs cleaning, receipt prints.
    let settle_body = json!({
        "payments": [{
            "method": "PAYMENT_METHOD_CASH",
            "tendered": vnd(165_000),
            "applied_to_bill": vnd(165_000),
        }],
    });
    let (status, settled) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/bills/{bill_id}/settle"),
        Some(settle_body),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settled["state"], "BILL_STATE_SETTLED");
    assert_eq!(settled["receipt_number"], 1);
    assert_eq!(settled["table_state"], "TABLE_STATE_NEEDS_CLEANING");
    assert_eq!(settled["print_receipt"], true);

    // Clean it down: the table returns to free.
    let (status, cleaned) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/clean"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleaned["state"], "TABLE_STATE_FREE");
}

#[tokio::test]
async fn underpaying_a_bill_over_http_is_a_conflict() {
    let (app, token) = app().await;
    let table = TableId::new(Ulid::from_u128(701));
    send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/lines"),
        Some(a_line_body()),
    )
    .await;
    let (_, bill) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/bill"),
        None,
    )
    .await;
    let bill_id = bill["bill_id"].as_str().expect("a bill id").to_owned();

    // 150k applied against 165k owed does not sum to the total.
    let body = json!({
        "payments": [{
            "method": "PAYMENT_METHOD_CASH",
            "tendered": vnd(150_000),
            "applied_to_bill": vnd(150_000),
        }],
    });
    let (status, _) = send(
        app,
        &token,
        "POST",
        &format!("/api/bills/{bill_id}/settle"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn an_unknown_payment_method_is_a_bad_request() {
    let (app, token) = app().await;
    let table = TableId::new(Ulid::from_u128(702));
    send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/lines"),
        Some(a_line_body()),
    )
    .await;
    let (_, bill) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/bill"),
        None,
    )
    .await;
    let bill_id = bill["bill_id"].as_str().expect("a bill id").to_owned();

    // An unspecified method is not a real payment: the wire tolerates it, the domain boundary refuses.
    let body = json!({
        "payments": [{
            "method": "PAYMENT_METHOD_UNSPECIFIED",
            "tendered": vnd(165_000),
            "applied_to_bill": vnd(165_000),
        }],
    });
    let (status, _) = send(
        app,
        &token,
        "POST",
        &format!("/api/bills/{bill_id}/settle"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_shift_opens_counts_blind_and_closes_over_http() {
    let (app, token) = app().await;

    // Open with a 500k float.
    let (status, opened) = send(
        app.clone(),
        &token,
        "POST",
        "/api/shifts",
        Some(json!({ "opening_float": vnd(500_000) })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(opened["state"], "SHIFT_STATE_OPEN");
    let shift_id = opened["shift_id"].as_str().expect("a shift id").to_owned();

    // Count: blind, so the response reveals no expectation and no variance.
    let (status, counted) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/shifts/{shift_id}/count"),
        Some(json!({ "counted_minor": 500_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(counted["state"], "SHIFT_STATE_COUNTED");
    assert!(
        counted.get("expected_amount").is_none(),
        "the count is blind"
    );
    assert!(counted.get("variance").is_none(), "the count is blind");

    // Close: now the expected amount and the (zero) variance are revealed.
    let (status, closed) = send(
        app,
        &token,
        "POST",
        &format!("/api/shifts/{shift_id}/close"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(closed["state"], "SHIFT_STATE_CLOSED");
    assert_eq!(closed["expected_amount"], json!(vnd(500_000)));
    assert_eq!(closed["variance"], json!(vnd(0)));
    assert_eq!(closed["print_shift_report"], true);
}
