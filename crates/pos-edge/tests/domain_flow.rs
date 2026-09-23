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
use pos_edge::pairing::Minter;
use pos_edge::printing::{Printers, TransportFactory};
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StaffAuth,
    StaffRoster, StoreIdentity, SystemClock,
};
use pos_fakes::FakeStore;
use pos_ports::PortError;
use pos_proto::ClockSource;
use pos_proto::CurrencyCode;
use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::ids::DeviceId;
use pos_proto::ids::{EmployeeId, MenuItemId, StationId, StoreId, TableId};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use printer_escpos::{Transport, TransportStatus, Unreachable};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A fixed 16-byte salt, so a hashed fixture is deterministic. Never used in production.
const SALT: &[u8] = b"a-fixed-test-slt";

/// The badge code + PIN seeded into the store's roster, and the employee they sign in as.
const STAFF_CODE: &str = "C01";
const STAFF_PIN: &str = "2468";

/// A real Argon2id PHC hash of `pin`, computed with a fixed salt so the test needs no RNG — the same
/// recipe the offline-auth unit tests use.
fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), SALT)
        .expect("hash")
        .to_string()
}

/// The domain router plus a bearer token for a device that is paired *and signed in* — every command
/// route now requires both (S0b, ADR-0084). The store is seeded with one staff member so the device
/// can sign in over the real route.
async fn app() -> (Router, String) {
    app_with(None).await
}

/// The same router, with a print dispatcher layered in and the given devices published — the shape
/// `serve` composes ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)).
async fn app_with(printing: Option<(Arc<Printers>, PublishedDevices)>) -> (Router, String) {
    let identity = StoreIdentity::for_store(StoreId::new(Ulid::from_u128(7)));
    let mut roster = StaffRoster::new();
    roster.insert(
        STAFF_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::default(),
            discount_ceiling: None,
            pin_phc: Some(hash_of(STAFF_PIN)),
        },
    );
    let devices = printing
        .as_ref()
        .map(|(_, devices)| devices.clone())
        .unwrap_or_default();
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            identity,
            EdgeSession {
                devices,
                ..EdgeSession::bootstrap().with_staff(roster)
            },
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let (code, _) = pairing
        .mint(now, Minter::Boot)
        .expect("mint a pairing code");
    let token = pairing
        .redeem(&code, now)
        .await
        .expect("redeem")
        .token()
        .expect("a fresh code pairs a device")
        .as_str()
        .to_owned();
    let mut service = pos_edge::http::domain_router(
        edge,
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    );
    if let Some((printers, _)) = printing {
        service = service.layer(axum::Extension(printers));
    }
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

/// [`send`], keeping the `pos-error-reason` header a refusal carries (ADR-0137).
async fn send_for_reason(
    app: Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Option<String>) {
    let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(body)
        .expect("request builds");
    let response = app.oneshot(request).await.expect("router responds");
    let reason = response
        .headers()
        .get("pos-error-reason")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    (response.status(), reason)
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
    // No dispatcher is layered into this router, so the honest answer is that there was nothing to
    // print on — not the silence the till used to render "Printing receipt…" over (ADR-0100).
    assert_eq!(settled["receipt_print"], "NO_PRINTER");

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

/// A transport that records what a printer would have received.
#[derive(Debug, Default)]
struct Recorder {
    written: std::sync::Mutex<Vec<Vec<u8>>>,
}

#[derive(Debug)]
struct Recorders(Arc<Recorder>);

#[derive(Debug)]
struct RecordingTransport(Arc<Recorder>);

impl Transport for RecordingTransport {
    fn write(&self, bytes: &[u8]) -> Result<(), Unreachable> {
        self.0
            .written
            .lock()
            .map_err(|_| Unreachable)?
            .push(bytes.to_vec());
        Ok(())
    }

    fn probe(&self) -> Result<TransportStatus, Unreachable> {
        Ok(TransportStatus::default())
    }
}

impl TransportFactory for Recorders {
    fn open(&self, _device: &PublishedDevice) -> Result<Box<dyn Transport>, PortError> {
        Ok(Box::new(RecordingTransport(Arc::clone(&self.0))))
    }
}

#[tokio::test]
async fn settling_a_bill_puts_the_receipt_on_the_stores_published_printer() {
    // The whole point of C2: `print_receipt` has been true on every settle since P5 and nothing ever
    // constructed a job. This drives the shipped route and asserts paper (ADR-0100).
    let recorder = Arc::new(Recorder::default());
    let printers = Arc::new(Printers::over(Arc::new(Recorders(Arc::clone(&recorder)))));
    let counter_printer = PublishedDevice {
        device_id: DeviceId::new(Ulid::from_u128(0x9100)),
        kind: DeviceKind::Printer.into(),
        connection: DeviceConnection::Network.into(),
        address: "192.0.2.10:9100".to_owned(),
        name: DisplayName::new("Counter"),
        // No station: this is the receipt printer.
        station_id: None,
        // No agent: the edge opens the address itself, which is what this test drives.
        agent_device_id: None,
    };
    let (app, token) = app_with(Some((
        printers,
        PublishedDevices::new(vec![counter_printer]),
    )))
    .await;

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

    let (status, settled) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/bills/{bill_id}/settle"),
        Some(json!({
            "payments": [{
                "method": "PAYMENT_METHOD_CASH",
                "tendered": vnd(165_000),
                "applied_to_bill": vnd(165_000),
            }],
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(settled["receipt_print"], "PRINTED");
    let written = recorder.written.lock().expect("the recorder");
    assert_eq!(written.len(), 1, "one settle, one receipt");
    let bytes = written.first().expect("the receipt");
    assert!(
        bytes.windows(2).any(|window| window == b"#1"),
        "the gapless receipt number is on the paper"
    );
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

#[tokio::test]
async fn the_open_shift_is_readable_by_a_device_that_reloads() {
    let (app, token) = app().await;

    // Nothing open is the ordinary morning state, and it is an answer, not an error.
    let (status, none) = send(app.clone(), &token, "GET", "/api/shifts/current", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(none, Value::Null);

    let (_, opened) = send(
        app.clone(),
        &token,
        "POST",
        "/api/shifts",
        Some(json!({ "opening_float": vnd(500_000) })),
    )
    .await;
    let shift_id = opened["shift_id"].as_str().expect("a shift id").to_owned();

    // A reload reads the shift another request opened: same id, same state, and blind.
    let (status, current) = send(app.clone(), &token, "GET", "/api/shifts/current", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(current["shift_id"], shift_id.as_str());
    assert_eq!(current["state"], "SHIFT_STATE_OPEN");
    assert!(
        current.get("expected_amount").is_none(),
        "the read is blind"
    );

    let (_, _) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/shifts/{shift_id}/count"),
        Some(json!({ "counted_minor": 480_000 })),
    )
    .await;
    let (_, current) = send(app.clone(), &token, "GET", "/api/shifts/current", None).await;
    assert_eq!(current["state"], "SHIFT_STATE_COUNTED");
    assert_eq!(current["counted_amount"], json!(vnd(480_000)));
    assert!(
        current.get("expected_amount").is_none() && current.get("variance").is_none(),
        "a counted shift is still blind until it closes"
    );

    let (_, _) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/shifts/{shift_id}/close"),
        None,
    )
    .await;
    let (_, after) = send(app, &token, "GET", "/api/shifts/current", None).await;
    assert_eq!(after, Value::Null, "a closed shift is not the current one");
}

#[tokio::test]
async fn a_refusal_names_itself_in_a_header_a_till_can_translate() {
    let (app, token) = app().await;
    let open = || Some(json!({ "opening_float": vnd(500_000) }));

    let (status, reason) =
        send_for_reason(app.clone(), &token, "POST", "/api/shifts", open()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reason, None, "a success carries no reason");

    let (status, reason) =
        send_for_reason(app.clone(), &token, "POST", "/api/shifts", open()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(reason.as_deref(), Some("SHIFT_ALREADY_OPEN"));

    // A path segment that is not a ULID is the caller's mistake, and says so the same way.
    let (status, reason) =
        send_for_reason(app, &token, "POST", "/api/shifts/not-a-ulid/close", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(reason.as_deref(), Some("INVALID_ARGUMENT"));
}

#[tokio::test]
async fn the_status_bar_reads_the_outbox_and_the_cloud_link() {
    let (app, token) = app().await;
    let table = TableId::new(Ulid::from_u128(701));
    let (status, _) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, sync) = send(app, &token, "GET", "/api/sync", None).await;
    assert_eq!(status, StatusCode::OK);
    // This store has no cloud, so every event it committed is still waiting — and that is a
    // warning at most, never a refusal (ADR-0137).
    assert!(sync["outbox_depth"].as_u64().expect("a depth") >= 1);
    assert_eq!(sync["outbox_planned_depth"], 100_000);
    assert_eq!(sync["outbox_level"], "OUTBOX_LEVEL_NORMAL");
    // Nothing has tried to reach a cloud, which is not the same as having failed to.
    assert_eq!(sync["cloud_link"], "CLOUD_LINK_UNSPECIFIED");
    assert!(sync.get("last_sync_time").is_none());
}

#[tokio::test]
async fn a_fired_line_tells_a_reloaded_board_which_station_it_went_to() {
    let (app, token) = app().await;
    let table = TableId::new(Ulid::from_u128(702));
    let station = StationId::new(Ulid::from_u128(9));
    send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    let (_, added) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/lines"),
        Some(a_line_body()),
    )
    .await;
    let line_id = added["order_line_id"]
        .as_str()
        .expect("a line id")
        .to_owned();

    // Still on the pad: no station yet, and the field is omitted rather than null.
    let (_, live) = send(app.clone(), &token, "GET", "/api/orders/live", None).await;
    assert!(live[0]["lines"][0].get("station_id").is_none());

    let (status, _) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/lines/{line_id}/fire"),
        Some(json!({ "station_id": station })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, live) = send(app, &token, "GET", "/api/orders/live", None).await;
    assert_eq!(live[0]["lines"][0]["station_id"], json!(station));
}
