// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A kiosk signs in as its own staff record, and a person leaves it only with `admin.kiosk.exit`
//! and their PIN (ADR-0224 decision 4), over the shipped routes.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::pairing::Minter;
use pos_edge::printing::{AgentDispatch, AgentLane, Printers, TcpTransports};
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StaffAuth,
    StaffRoster, StoreIdentity, SystemClock,
};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::devices::{DeviceClass, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::ids::{DeviceId, EmployeeId, StoreId};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use serde_json::{Value, json};
use tower::ServiceExt;

const SALT: &[u8] = b"a-fixed-test-slt";

const KIOSK_TERMINAL: u128 = 0x7A01;
const COUNTER_TERMINAL: u128 = 0x7A02;
const KIOSK_RECORD: u128 = 0x0C1;
const MANAGER: u128 = 24;
const CASHIER: u128 = 21;
const MANAGER_PIN: &str = "4417";
const CASHIER_PIN: &str = "1357";

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), SALT)
        .expect("hash")
        .to_string()
}

fn id(seed: u128) -> DeviceId {
    DeviceId::new(Ulid::from_u128(seed))
}

fn terminal(seed: u128, name: &str, class: Option<DeviceClass>) -> PublishedDevice {
    PublishedDevice {
        device_id: id(seed),
        kind: DeviceKind::Terminal.into(),
        connection: Open::default(),
        address: String::new(),
        name: DisplayName::new(name),
        station_id: None,
        agent_device_id: None,
        drawer_attached: false,
        paper_width: Open::default(),
        cuts_paper: None,
        receipt_printer_id: None,
        receipt_language: Open::default(),
        receipt_second_language: Open::default(),
        opening_float_minor: None,
        device_class: class.map_or_else(Open::default, Open::from_known),
    }
}

/// The store's staff: the kiosk's own record, a manager who may leave a kiosk and a cashier who
/// may not. The kiosk record is published with every permission, which it must not keep.
fn staff() -> StaffRoster {
    let mut staff = StaffRoster::new();
    staff.insert_kiosk(
        "KIOSK-1",
        EmployeeId::new(Ulid::from_u128(KIOSK_RECORD)),
        id(KIOSK_TERMINAL),
    );
    let till = PermissionSet::EMPTY
        .with(Permission::AddLine)
        .with(Permission::OpenBill)
        .with(Permission::TakePayment);
    for (code, seed, permissions, pin) in [
        (
            "MGR-1",
            MANAGER,
            till.with(Permission::ExitKiosk),
            MANAGER_PIN,
        ),
        ("CSH-1", CASHIER, till, CASHIER_PIN),
    ] {
        staff.insert(
            code,
            StaffAuth {
                employee_id: Some(EmployeeId::new(Ulid::from_u128(seed))),
                permissions,
                permissions_with_approval: PermissionSet::EMPTY,
                discount_ceiling: None,
                pin_phc: Some(hash_of(pin)),
            },
        );
    }
    staff
}

/// The store over the shipped routes, and three paired devices: one bound to the kiosk terminal,
/// one bound to an ordinary counter terminal, and one bound to none.
struct Store {
    app: Router,
    kiosk: String,
    counter: String,
    unbound: String,
}

async fn store() -> Store {
    let mut session = EdgeSession {
        devices: PublishedDevices::new(vec![
            terminal(KIOSK_TERMINAL, "Kiosk", Some(DeviceClass::Kiosk)),
            terminal(COUNTER_TERMINAL, "Counter", Some(DeviceClass::Terminal)),
        ]),
        ..EdgeSession::bootstrap()
    };
    session.staff = staff();
    let edge = Arc::new(
        Edge::new(
            FakeStore::new(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator"),
    );
    let agents = Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new());
    let queue = Arc::new(pos_edge::print_queue::InMemoryPrintQueue::new());
    let wake = pos_edge::print_wake::SharedPrintWake::new();
    let dispatch: Arc<dyn AgentDispatch> = Arc::new(AgentLane::new(
        Arc::clone(&agents),
        Arc::clone(&queue),
        wake.clone(),
    ));
    let printers = Arc::new(Printers::over(Arc::new(TcpTransports)).with_agents(dispatch));
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let mut paired = Vec::new();
    for _ in 0..3 {
        let (code, _) = pairing.mint(now, Minter::Boot).expect("mint a code");
        let token = pairing
            .redeem(&code, now)
            .await
            .expect("redeem")
            .token()
            .expect("a fresh code pairs a device");
        let device = pairing.device_for(&token).expect("the token resolves");
        paired.push((device, token.as_str().to_owned()));
    }
    let (_, unbound) = paired.pop().expect("a device bound to nothing");
    let (counter_device, counter) = paired.pop().expect("the counter's device");
    let (kiosk_device, kiosk) = paired.pop().expect("the kiosk's device");
    for (terminal, device) in [
        (KIOSK_TERMINAL, kiosk_device),
        (COUNTER_TERMINAL, counter_device),
    ] {
        use pos_edge::print_agent::PrintAgents as _;
        agents
            .claim(id(terminal), device, now.as_milliseconds_since_epoch())
            .await
            .expect("the binding is recorded");
    }
    let app = pos_edge::http::domain_router(
        edge,
        InMemoryQueueNumbers::new(),
        agents,
        queue,
        wake,
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    )
    .layer(axum::Extension(printers));
    Store {
        app,
        kiosk,
        counter,
        unbound,
    }
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
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("router responds");
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

async fn who(app: &Router, token: &str) -> Value {
    let (status, _, body) = send(app, token, "GET", "/api/session", None).await;
    assert_eq!(status, StatusCode::OK);
    body
}

#[tokio::test]
async fn a_kiosk_signs_in_as_its_own_record_and_holds_only_the_kiosk_set() {
    let store = store().await;
    let (status, _, body) =
        send(&store.app, &store.kiosk, "POST", "/api/session/kiosk", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let record = EmployeeId::new(Ulid::from_u128(KIOSK_RECORD)).to_string();
    assert_eq!(body["employee_id"], record.as_str());
    let session = who(&store.app, &store.kiosk).await;
    assert_eq!(session["employee_id"], record.as_str());
    assert_eq!(
        session["permissions"],
        json!(["billing.bill.open", "sales.line.add"])
    );
    assert_eq!(session["permissions_with_approval"], json!([]));
    assert_eq!(session["device_class"], "DEVICE_CLASS_KIOSK");
    assert_eq!(session["kiosk_abandon_seconds"], 120);
}

#[tokio::test]
async fn only_a_device_bound_to_a_kiosk_terminal_signs_in_as_a_kiosk() {
    let store = store().await;
    for token in [&store.counter, &store.unbound] {
        let (status, _, _) = send(&store.app, token, "POST", "/api/session/kiosk", None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(who(&store.app, token).await["signed_in"], false);
    }
    // Nor does its record's code sign in anywhere: it has no PIN.
    let (status, _, _) = send(
        &store.app,
        &store.counter,
        "POST",
        "/api/session/sign-in",
        Some(json!({ "code": "KIOSK-1", "pin": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_person_leaves_a_kiosk_only_with_the_permission_and_their_pin() {
    let store = store().await;
    let (status, _, _) = send(&store.app, &store.kiosk, "POST", "/api/session/kiosk", None).await;
    assert_eq!(status, StatusCode::OK);
    let record = EmployeeId::new(Ulid::from_u128(KIOSK_RECORD)).to_string();

    // A cashier's PIN verifies, but a cashier may not leave the kiosk: it stays signed in as itself.
    let (status, reason, _) = send(
        &store.app,
        &store.kiosk,
        "POST",
        "/api/session/sign-in",
        Some(json!({ "code": "CSH-1", "pin": CASHIER_PIN })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(reason.as_deref(), Some("KIOSK_EXIT_NOT_ALLOWED"));
    assert_eq!(
        who(&store.app, &store.kiosk).await["employee_id"],
        record.as_str()
    );

    // A wrong PIN is a wrong PIN, under the sign-in's own lockout.
    let (status, _, body) = send(
        &store.app,
        &store.kiosk,
        "POST",
        "/api/session/sign-in",
        Some(json!({ "code": "MGR-1", "pin": "0000" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["outcome"], "wrong");

    // The manager holds `admin.kiosk.exit`, and leaves it.
    let (status, _, _) = send(
        &store.app,
        &store.kiosk,
        "POST",
        "/api/session/sign-in",
        Some(json!({ "code": "MGR-1", "pin": MANAGER_PIN })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let manager = EmployeeId::new(Ulid::from_u128(MANAGER)).to_string();
    assert_eq!(
        who(&store.app, &store.kiosk).await["employee_id"],
        manager.as_str()
    );

    // At an ordinary till the cashier signs in as ever.
    let (status, _, _) = send(
        &store.app,
        &store.counter,
        "POST",
        "/api/session/sign-in",
        Some(json!({ "code": "CSH-1", "pin": CASHIER_PIN })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
