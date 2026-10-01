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
use pos_proto::Open;
use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::fees::PublishedFees;
use pos_proto::ids::DeviceId;
use pos_proto::ids::{EmployeeId, MenuItemId, StationId, StoreId, TableId};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::shift::{NoShiftSelling, PublishedShift};
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
    app_with_permissions(printing, PermissionSet::default()).await
}

/// [`app_with`], signing in somebody granted `permissions` — a manager, for the routes that need one.
async fn app_with_permissions(
    printing: Option<(Arc<Printers>, PublishedDevices)>,
    permissions: PermissionSet,
) -> (Router, String) {
    app_with_shift(printing, permissions, PublishedShift::default()).await
}

/// [`app_with_permissions`], at a store whose published `shift` node is `shift` (ADR-0160).
async fn app_with_shift(
    printing: Option<(Arc<Printers>, PublishedDevices)>,
    permissions: PermissionSet,
    shift: PublishedShift,
) -> (Router, String) {
    app_with_config(printing, permissions, shift, PublishedFees::default()).await
}

/// [`app_with_shift`], at a store whose published `fees` node is `fees` as well (ADR-0159).
async fn app_with_config(
    printing: Option<(Arc<Printers>, PublishedDevices)>,
    permissions: PermissionSet,
    shift: PublishedShift,
    fees: PublishedFees,
) -> (Router, String) {
    let identity = StoreIdentity::for_store(StoreId::new(Ulid::from_u128(7)));
    let mut roster = StaffRoster::new();
    roster.insert(
        STAFF_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions,
            permissions_with_approval: PermissionSet::EMPTY,
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
                shift,
                fees,
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

/// The check reads name each fee a bill charges (ADR-0159 decision 4): before the bill opens, as the
/// rules in force would charge it, and once it is open, as the bill froze them. The settle takes the
/// figure the check quoted.
#[tokio::test]
async fn a_check_shows_each_fee_and_the_settle_takes_its_total() {
    let service: PublishedFees = serde_json::from_str(
        &json!({ "fees": [{
            "fee_id": "01JQ0000000000000000000003",
            "code": "SERVICE",
            "display_name": "Service charge",
            "kind": "FEE_KIND_PERCENT",
            "rate": { "numerator": 5, "denominator": 100 },
        }] })
        .to_string(),
    )
    .expect("a fees node");
    let (app, token) = app_with_config(
        None,
        PermissionSet::default(),
        PublishedShift::default(),
        service,
    )
    .await;
    let table = TableId::new(Ulid::from_u128(701));
    send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    let (status, _) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/lines"),
        Some(a_line_body()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 5% of 150,000, and the 10% tax on 157,500, of which 750 is the fee's.
    let fee_line = json!([{
        "fee_id": "01JQ0000000000000000000003",
        "code": "SERVICE",
        "display_name": "Service charge",
        "amount": vnd(7_500),
        "tax": vnd(750),
    }]);
    let (status, check) = send(
        app.clone(),
        &token,
        "GET",
        &format!("/api/tables/{table}/check"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(check["fee_lines"], fee_line, "{check}");
    assert_eq!(check["total_due"], json!(vnd(173_250)));

    let (_, bill) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/bill"),
        None,
    )
    .await;
    let bill_id = bill["bill_id"].as_str().expect("a bill id").to_owned();
    for uri in [
        format!("/api/bills/{bill_id}/check"),
        format!("/api/tables/{table}/check"),
    ] {
        let (status, check) = send(app.clone(), &token, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(check["fee_lines"], fee_line, "{uri}: {check}");
        assert_eq!(check["total_due"], json!(vnd(173_250)), "{uri}");
    }

    let (status, settled) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/bills/{bill_id}/settle"),
        Some(json!({
            "payments": [{
                "method": "PAYMENT_METHOD_CASH",
                "tendered": vnd(173_250),
                "applied_to_bill": vnd(173_250),
            }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["state"], "BILL_STATE_SETTLED");
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
        drawer_attached: false,
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

/// Seats a table, puts one line on it, opens its bill and settles it with one tender of `method`,
/// returning the settle's response body.
async fn settle_a_table_with(app: &Router, token: &str, table: TableId, method: &str) -> Value {
    send(
        app.clone(),
        token,
        "POST",
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    send(
        app.clone(),
        token,
        "POST",
        &format!("/api/tables/{table}/lines"),
        Some(a_line_body()),
    )
    .await;
    let (_, bill) = send(
        app.clone(),
        token,
        "POST",
        &format!("/api/tables/{table}/bill"),
        None,
    )
    .await;
    let bill_id = bill["bill_id"].as_str().expect("a bill id").to_owned();
    let (status, settled) = send(
        app.clone(),
        token,
        "POST",
        &format!("/api/bills/{bill_id}/settle"),
        Some(json!({
            "payments": [{
                "method": method,
                "tendered": vnd(165_000),
                "applied_to_bill": vnd(165_000),
            }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    settled
}

#[tokio::test]
async fn a_cash_settle_opens_the_marked_drawer_and_a_card_settle_does_not() {
    // ADR-0165 decision 4, over the shipped route: the drawer opens for cash, and only for cash, and
    // it opens before the receipt prints, because the cashier needs the change first.
    let recorder = Arc::new(Recorder::default());
    let printers = Arc::new(Printers::over(Arc::new(Recorders(Arc::clone(&recorder)))));
    let with_drawer = PublishedDevice {
        connection: DeviceConnection::Usb.into(),
        address: "/dev/usb/lp0".to_owned(),
        drawer_attached: true,
        ..counter_printer()
    };
    let (app, token) = app_with(Some((printers, PublishedDevices::new(vec![with_drawer])))).await;

    let cash = settle_a_table_with(
        &app,
        &token,
        TableId::new(Ulid::from_u128(705)),
        "PAYMENT_METHOD_CASH",
    )
    .await;
    assert_eq!(cash["drawer_open"], "OPENED");
    assert_eq!(cash["receipt_print"], "PRINTED");
    {
        let written = recorder.written.lock().expect("the recorder");
        assert_eq!(written.len(), 2, "a kick and a receipt");
        assert_eq!(
            written.first(),
            Some(&printer_escpos::escpos::DRAWER_KICK.to_vec()),
            "the drawer opens before the receipt prints"
        );
    }

    let card = settle_a_table_with(
        &app,
        &token,
        TableId::new(Ulid::from_u128(706)),
        "PAYMENT_METHOD_CARD",
    )
    .await;
    assert!(
        card.get("drawer_open").is_none(),
        "a card payment asks nothing of the drawer: {card}"
    );
    assert_eq!(card["receipt_print"], "PRINTED");
    assert_eq!(
        recorder.written.lock().expect("the recorder").len(),
        3,
        "the card settle printed its receipt and kicked nothing"
    );
}

#[tokio::test]
async fn cash_paid_out_over_http_opens_the_drawer_and_the_shift_keeps_it() {
    // ADR-0165 decisions 1 and 4 over the shipped routes: a paid out is recorded against the shift,
    // the drawer opens for the cash to come out, and a reload still shows it.
    let recorder = Arc::new(Recorder::default());
    let printers = Arc::new(Printers::over(Arc::new(Recorders(Arc::clone(&recorder)))));
    let with_drawer = PublishedDevice {
        connection: DeviceConnection::Usb.into(),
        address: "/dev/usb/lp0".to_owned(),
        drawer_attached: true,
        ..counter_printer()
    };
    let (app, token) = app_with(Some((printers, PublishedDevices::new(vec![with_drawer])))).await;

    // A reason the store lists for a paid out: the framework's default list has "Making change".
    let (_, listed) = send(app.clone(), &token, "GET", "/api/reason-codes", None).await;
    let reason = listed["reasons"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|reason| {
            reason["applies_to"]
                .as_array()
                .is_some_and(|acts| acts.iter().any(|act| act == "REASON_ACTION_CASH_PAID_OUT"))
        })
        .expect("a reason for a paid out")["reason_code_id"]
        .as_str()
        .expect("an id")
        .to_owned();

    let (status, shift) = send(
        app.clone(),
        &token,
        "POST",
        "/api/shifts",
        Some(json!({ "opening_float": vnd(500_000) })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{shift}");
    let shift_id = shift["shift_id"].as_str().expect("a shift id").to_owned();
    let paid_out = format!("/api/shifts/{shift_id}/paid-out");

    let (status, zero) = send(
        app.clone(),
        &token,
        "POST",
        &paid_out,
        Some(json!({ "amount_minor": 0, "reason_code_id": reason })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "nothing is paid out: {zero}"
    );

    let (status, paid) = send(
        app.clone(),
        &token,
        "POST",
        &paid_out,
        Some(json!({ "amount_minor": 50_000, "reason_code_id": reason })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{paid}");
    assert_eq!(paid["drawer_open"], "OPENED");
    assert_eq!(paid["paid_out_amount"]["amount_minor"], 50_000);
    assert!(
        paid.get("expected_amount").is_none(),
        "a paid out reveals nothing the close keeps blind: {paid}"
    );
    assert_eq!(
        *recorder.written.lock().expect("the recorder"),
        vec![printer_escpos::escpos::DRAWER_KICK.to_vec()],
        "the drawer opened for the cash to come out"
    );

    let (_, current) = send(app.clone(), &token, "GET", "/api/shifts/current", None).await;
    assert_eq!(
        current["paid_out_amount"]["amount_minor"], 50_000,
        "a reload keeps it"
    );

    let (status, no_pin) = send(
        app.clone(),
        &token,
        "POST",
        "/api/drawer/open",
        Some(json!({ "reason_code_id": reason })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a drawer opened without a sale needs a manager: {no_pin}"
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

/// A line rung onto a table after its bill opened is refused, and says why in the header a till
/// translates: the bill named its lines when it opened, so the new one would be on no bill and
/// leave with the table unpaid.
#[tokio::test]
async fn a_line_after_the_bill_is_refused_and_the_bill_is_unchanged() {
    let (app, token) = app().await;
    let table = TableId::new(Ulid::from_u128(701));
    let lines = format!("/api/tables/{table}/lines");
    let (status, _) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(app.clone(), &token, "POST", &lines, Some(a_line_body())).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/bill"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, reason) =
        send_for_reason(app.clone(), &token, "POST", &lines, Some(a_line_body())).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(reason.as_deref(), Some("BILL_ALREADY_OPEN"));

    let (status, check) = send(
        app,
        &token,
        "GET",
        &format!("/api/tables/{table}/check"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        check["total_due"]["amount_minor"], 165_000,
        "one pizza and its tax, as when the bill opened"
    );
}

/// A table seated by mistake goes back to the floor over HTTP, and the two refusals around it name
/// themselves in the header a till translates (ADR-0163): a bill on nothing is `NOTHING_TO_BILL`,
/// and releasing a table with a dish on it is `ORDER_NOT_EMPTY`.
#[tokio::test]
async fn a_table_seated_by_mistake_is_released_over_http() {
    let (app, token) = app().await;
    let table = TableId::new(Ulid::from_u128(702));
    let seat = format!("/api/tables/{table}/seat");
    let release = format!("/api/tables/{table}/release");
    let (status, _) = send(app.clone(), &token, "POST", &seat, None).await;
    assert_eq!(status, StatusCode::OK);

    let bill = format!("/api/tables/{table}/bill");
    let (status, reason) = send_for_reason(app.clone(), &token, "POST", &bill, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(reason.as_deref(), Some("NOTHING_TO_BILL"));

    let (status, released) = send(app.clone(), &token, "POST", &release, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(released["state"], "TABLE_STATE_FREE");
    let (status, reason) = send_for_reason(app.clone(), &token, "POST", &release, None).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a free table has nobody to send away"
    );
    assert_eq!(reason.as_deref(), Some("TRANSITION_REFUSED"));

    let (status, _) = send(app.clone(), &token, "POST", &seat, None).await;
    assert_eq!(status, StatusCode::OK);
    let lines = format!("/api/tables/{table}/lines");
    let (status, _) = send(app.clone(), &token, "POST", &lines, Some(a_line_body())).await;
    assert_eq!(status, StatusCode::OK);
    let (status, reason) = send_for_reason(app, &token, "POST", &release, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(reason.as_deref(), Some("ORDER_NOT_EMPTY"));
}

/// A settled bill's receipt is printed again over HTTP, on the store's receipt printer, as a copy
/// marked COPY under its own number (ADR-0164). Today's list names the bill and counts its copies;
/// a bill still open has no receipt to copy, and says so in the header a till translates.
#[tokio::test]
async fn a_receipt_is_copied_over_http_from_today_s_bills() {
    let recorder = Arc::new(Recorder::default());
    let printers = Arc::new(Printers::over(Arc::new(Recorders(Arc::clone(&recorder)))));
    let (app, token) = app_with(Some((
        printers,
        PublishedDevices::new(vec![counter_printer()]),
    )))
    .await;
    let table = TableId::new(Ulid::from_u128(703));
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
    let reprint = format!("/api/bills/{bill_id}/receipt/reprint");

    let (status, reason) = send_for_reason(app.clone(), &token, "POST", &reprint, None).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an open bill has no receipt yet"
    );
    assert_eq!(reason.as_deref(), Some("NOT_SETTLED"));

    let (status, _) = send(
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

    let (status, listed) = send(app.clone(), &token, "GET", "/api/bills/settled", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed[0]["bill_id"], json!(bill_id));
    assert_eq!(listed[0]["table_id"], json!(table));
    assert_eq!(listed[0]["receipt_number"], 1);
    assert_eq!(listed[0]["total_due"], json!(vnd(165_000)));
    assert_eq!(listed[0]["copies"], 0);
    assert!(
        listed[0].get("queue_number").is_none(),
        "a table's bill is named by its table"
    );

    let (status, copy) = send(app.clone(), &token, "POST", &reprint, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        copy["receipt_number"], 1,
        "a copy keeps the receipt's number"
    );
    assert_eq!(copy["copy_number"], 1);
    assert_eq!(copy["receipt_print"], "PRINTED");
    {
        let written = recorder.written.lock().expect("the recorder");
        assert_eq!(written.len(), 2, "the receipt, then its copy");
        let paper = written.last().expect("the copy");
        assert!(
            paper.windows(4).any(|window| window == b"COPY"),
            "the copy says so"
        );
        assert!(
            paper.windows(2).any(|window| window == b"#1"),
            "under the original's number"
        );
    }

    let (_, again) = send(app.clone(), &token, "POST", &reprint, None).await;
    assert_eq!(again["copy_number"], 2);
    let (_, listed) = send(app, &token, "GET", "/api/bills/settled", None).await;
    assert_eq!(listed[0]["copies"], 2);
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
async fn a_store_that_refuses_selling_without_a_shift_says_so_until_one_opens() {
    let refusing = PublishedShift {
        no_shift_selling: Open::from_known(NoShiftSelling::Refuse),
    };
    let (app, token) = app_with_shift(None, PermissionSet::default(), refusing).await;
    let seat = format!("/api/tables/{}/seat", TableId::new(Ulid::from_u128(780)));

    let (status, reason) = send_for_reason(app.clone(), &token, "POST", &seat, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(reason.as_deref(), Some("OPEN_SHIFT_REQUIRED"));
    let counter = Some(json!({}));
    let (status, reason) =
        send_for_reason(app.clone(), &token, "POST", "/api/orders", counter).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(reason.as_deref(), Some("OPEN_SHIFT_REQUIRED"));

    let open = Some(json!({ "opening_float": vnd(500_000) }));
    let (status, _) = send(app.clone(), &token, "POST", "/api/shifts", open).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(app, &token, "POST", &seat, None).await;
    assert_eq!(status, StatusCode::OK, "a table seats once a shift is open");
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

fn counter_printer() -> PublishedDevice {
    PublishedDevice {
        device_id: DeviceId::new(Ulid::from_u128(0x9100)),
        kind: DeviceKind::Printer.into(),
        connection: DeviceConnection::Network.into(),
        address: "192.0.2.10:9100".to_owned(),
        name: DisplayName::new("Counter"),
        station_id: None,
        agent_device_id: None,
        drawer_attached: false,
    }
}

#[tokio::test]
async fn a_manager_prints_a_test_page_on_a_published_printer() {
    let recorder = Arc::new(Recorder::default());
    let printers = Arc::new(Printers::over(Arc::new(Recorders(Arc::clone(&recorder)))));
    let (app, token) = app_with_permissions(
        Some((printers, PublishedDevices::new(vec![counter_printer()]))),
        PermissionSet::default().with(pos_core::permission::Permission::ManageDevices),
    )
    .await;

    let (status, listed) = send(app.clone(), &token, "GET", "/api/printers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed[0]["name"], "Counter");
    let printer = listed[0]["device_id"].as_str().expect("an id").to_owned();

    let (status, outcome) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/printers/{printer}/test"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(outcome["print"], "PRINTED");
    assert!(
        !recorder.written.lock().expect("recorder").is_empty(),
        "bytes reached the printer's transport"
    );

    // A printer this store never published is not guessed at.
    let unknown = DeviceId::new(Ulid::from_u128(0x9999));
    let (_, missing) = send(
        app,
        &token,
        "POST",
        &format!("/api/printers/{unknown}/test"),
        None,
    )
    .await;
    assert_eq!(missing["print"], "NO_PRINTER");
}

#[tokio::test]
async fn a_test_page_needs_a_manager() {
    let recorder = Arc::new(Recorder::default());
    let printers = Arc::new(Printers::over(Arc::new(Recorders(Arc::clone(&recorder)))));
    let (app, token) = app_with(Some((
        printers,
        PublishedDevices::new(vec![counter_printer()]),
    )))
    .await;
    let printer = counter_printer().device_id;
    let (status, reason) = send_for_reason(
        app,
        &token,
        "POST",
        &format!("/api/printers/{printer}/test"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(reason.as_deref(), Some("PERMISSION_DENIED"));
    assert!(
        recorder.written.lock().expect("recorder").is_empty(),
        "no paper for a refusal"
    );
}

/// A table asks to see its bill before paying: the pre-bill reaches the receipt printer, says it is
/// not a receipt, and takes no receipt number — the settle that follows is still receipt one.
#[tokio::test]
async fn a_table_prints_its_pre_bill_and_the_receipt_number_is_untouched() {
    let recorder = Arc::new(Recorder::default());
    let printers = Arc::new(Printers::over(Arc::new(Recorders(Arc::clone(&recorder)))));
    let (app, token) = app_with(Some((
        printers,
        PublishedDevices::new(vec![counter_printer()]),
    )))
    .await;
    let table = TableId::new(Ulid::from_u128(703));
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

    let (status, printed) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/check/print"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(printed["prints"], json!(["PRINTED"]));
    {
        let written = recorder.written.lock().expect("the recorder");
        assert_eq!(written.len(), 1, "one pre-bill");
        let bytes = written.first().expect("the pre-bill");
        let contains = |needle: &[u8]| bytes.windows(needle.len()).any(|window| window == needle);
        assert!(contains(b"PRE-BILL"), "it says what it is");
        assert!(contains(b"Not a receipt"), "and what it is not");
        assert!(!contains(b"#1"), "it carries no receipt number");
    }

    // Asking again prints again: a guest who lost the first copy gets a second.
    let (_, again) = send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/bills/{bill_id}/check/print"),
        None,
    )
    .await;
    assert_eq!(again["prints"], json!(["PRINTED"]));
    assert_eq!(recorder.written.lock().expect("the recorder").len(), 2);

    let (_, settled) = send(
        app,
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
    assert_eq!(settled["receipt_number"], 1, "a pre-bill used no number");
}

/// A table with nothing on it has nothing to print, and a till is told so rather than being handed
/// an empty success it would read as paper on the way.
#[tokio::test]
async fn a_pre_bill_for_a_free_table_is_nothing_to_print() {
    let (app, token) = app().await;
    let table = TableId::new(Ulid::from_u128(704));
    let (status, reason) = send_for_reason(
        app.clone(),
        &token,
        "POST",
        &format!("/api/tables/{table}/check/print"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(reason.as_deref(), Some("NOTHING_TO_PRINT"));

    // Seated with a line and no dispatcher: the pre-bill is honest about the missing printer.
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
    let (status, printed) = send(
        app,
        &token,
        "POST",
        &format!("/api/tables/{table}/check/print"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(printed["prints"], json!(["NO_PRINTER"]));
}

/// Closing a shift puts its report on the receipt printer, and says so in the close response.
#[tokio::test]
async fn closing_a_shift_prints_its_report() {
    let recorder = Arc::new(Recorder::default());
    let printers = Arc::new(Printers::over(Arc::new(Recorders(Arc::clone(&recorder)))));
    let (app, token) = app_with(Some((
        printers,
        PublishedDevices::new(vec![counter_printer()]),
    )))
    .await;
    let (_, opened) = send(
        app.clone(),
        &token,
        "POST",
        "/api/shifts",
        Some(json!({ "opening_float": vnd(500_000) })),
    )
    .await;
    let shift_id = opened["shift_id"].as_str().expect("a shift id").to_owned();
    assert!(
        opened.get("shift_report_print").is_none(),
        "only a close prints"
    );
    send(
        app.clone(),
        &token,
        "POST",
        &format!("/api/shifts/{shift_id}/count"),
        Some(json!({ "counted_minor": 480_000 })),
    )
    .await;

    let (status, closed) = send(
        app,
        &token,
        "POST",
        &format!("/api/shifts/{shift_id}/close"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(closed["shift_report_print"], "PRINTED");
    let written = recorder.written.lock().expect("the recorder");
    assert_eq!(written.len(), 1, "one close, one report");
    let bytes = written.first().expect("the report");
    let contains = |needle: &[u8]| bytes.windows(needle.len()).any(|window| window == needle);
    assert!(contains(b"SHIFT REPORT"));
    assert!(contains(b"Short"), "20k under the float reads as short");
}

/// A store the cloud has published no connection to lists none, which the Devices screen reads as
/// "every family on its offline path" rather than an error.
#[tokio::test]
async fn a_store_with_no_published_connection_lists_none() {
    let (app, token) = app().await;
    let (status, listed) = send(app, &token, "GET", "/api/integrations", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed, json!([]));
}
