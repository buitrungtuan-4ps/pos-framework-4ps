// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A kiosk signs in as its own staff record, and a person leaves it only with `admin.kiosk.exit`
//! and their PIN (ADR-0224 decision 4), over the shipped routes.

use core::future::Future;
use core::pin::Pin;
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
use pos_ports::{PortError, PortName};
use pos_proto::ClockSource;
use pos_proto::SalesChannel;
use pos_proto::devices::{DeviceClass, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StoreId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use serde_json::{Value, json};
use tower::ServiceExt;

const SALT: &[u8] = b"a-fixed-test-slt";

const KIOSK_TERMINAL: u128 = 0x7A01;
const COUNTER_TERMINAL: u128 = 0x7A02;
/// A kiosk's record is its terminal's own id.
const KIOSK_RECORD: u128 = KIOSK_TERMINAL;
const MANAGER: u128 = 24;
const CASHIER: u128 = 21;
const MANAGER_PIN: &str = "4417";
const CASHIER_PIN: &str = "1357";
const ITEM: u128 = 0x1734;

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

/// The store's people: a manager who may leave a kiosk and a cashier who may not. The kiosk's own
/// record is not among them: the store derives it from the published `devices` node.
fn staff() -> StaffRoster {
    let mut staff = StaffRoster::new();
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
    edge: Arc<Edge<FakeStore>>,
    log: FakeStore,
    kiosk: String,
    counter: String,
    unbound: String,
}

/// The published `devices` node: the kiosk terminal, of `class`, and an ordinary counter.
fn devices_node(class: Option<DeviceClass>) -> Value {
    json!({
        "devices": serde_json::to_value(PublishedDevices::new(vec![
            terminal(KIOSK_TERMINAL, "Kiosk", class),
            terminal(COUNTER_TERMINAL, "Counter", Some(DeviceClass::Terminal)),
        ]))
        .expect("the node encodes"),
    })
}

/// The store's session: a pizza on the menu, taxed, and its people, with the published devices node
/// that makes the kiosk terminal a kiosk.
fn a_session() -> EdgeSession {
    let mut session = EdgeSession {
        ..EdgeSession::bootstrap()
            .with_menu(MenuCatalog::new().with(MenuEntry::new(
                MenuItemId::new(Ulid::from_u128(ITEM)),
                DisplayName::new("Margherita"),
                Money::new(CurrencyCode::VND, 150_000),
                EdgeSession::standard_tax_class(),
            )))
            .with_tax_rates(
                TaxRateTable::new()
                    .with(
                        EdgeSession::standard_tax_class(),
                        SalesChannel::Takeaway,
                        TaxRate::from_percent(10),
                    )
                    .with(
                        EdgeSession::standard_tax_class(),
                        SalesChannel::DineIn,
                        TaxRate::from_percent(10),
                    ),
            )
    };
    session.staff = staff();
    // The kiosk's record comes only from the node the cloud publishes.
    pos_edge::config_client::session_from_config(&session, &devices_node(Some(DeviceClass::Kiosk)))
}

async fn store() -> Store {
    store_reading_bindings(true).await
}

/// A binding record that cannot be read.
struct Unreadable;

impl AgentDispatch for Unreadable {
    fn enqueue<'a>(
        &'a self,
        _agent: DeviceId,
        _printer: DeviceId,
        _job: pos_ports::printer::PrintJob,
    ) -> Pin<Box<dyn Future<Output = pos_edge::printing::PrintOutcome> + Send + 'a>> {
        Box::pin(async { pos_edge::printing::PrintOutcome::AgentUnavailable })
    }

    fn terminal_of<'a>(
        &'a self,
        _device: DeviceId,
    ) -> Pin<Box<dyn Future<Output = Result<Option<DeviceId>, PortError>> + Send + 'a>> {
        Box::pin(async {
            Err(PortError::unavailable(
                PortName::EventStore,
                "the binding cannot be read",
            ))
        })
    }
}

/// [`store`], whose printers read the bindings it records or, when `readable` is false, cannot read
/// any binding at all.
async fn store_reading_bindings(readable: bool) -> Store {
    let log = FakeStore::new();
    let session = a_session();
    let edge = Arc::new(
        Edge::new(
            log.clone(),
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
    let dispatch: Arc<dyn AgentDispatch> = if readable {
        dispatch
    } else {
        Arc::new(Unreadable)
    };
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
    let devices = {
        let edge = Arc::clone(&edge);
        pos_edge::state::DevicesReader(Arc::new(move || edge.session().devices.clone()))
    };
    let config = pos_edge::EdgeConfig::new(
        "127.0.0.1:0".parse().expect("an address"),
        StoreId::new(Ulid::from_u128(1)),
    );
    let live = pos_edge::http::router(
        pos_edge::AppState::new(config)
            .with_pairing(Arc::clone(&pairing))
            .with_devices(devices),
    );
    let app = pos_edge::http::domain_router(
        Arc::clone(&edge),
        InMemoryQueueNumbers::new(),
        agents,
        queue,
        wake,
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    )
    .merge(live)
    .layer(axum::Extension(printers));
    Store {
        app,
        edge,
        log,
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
        Some(json!({ "code": format!("KIOSK-{}", id(KIOSK_TERMINAL)), "pin": "" })),
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

/// Rings a pizza on `order` from the device holding `token`, answering the line.
async fn ring(app: &Router, token: &str, order: &str) -> String {
    let (status, _, line) = send(
        app,
        token,
        "POST",
        &format!("/api/orders/{order}/lines"),
        Some(json!({
            "menu_item_id": MenuItemId::new(Ulid::from_u128(ITEM)),
            "quantity": Quantity::ONE,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{line}");
    line["order_line_id"]
        .as_str()
        .expect("a line id")
        .to_owned()
}

/// Opens an order over the routes from the device holding `token` and rings a pizza on it,
/// answering the order and the line.
async fn an_order_with_a_pizza(app: &Router, token: &str) -> (String, String) {
    let (status, _, opened) = send(app, token, "POST", "/api/orders", Some(json!({}))).await;
    assert_eq!(status, StatusCode::CREATED, "{opened}");
    assert!(
        opened["queue_number"].as_u64().is_some(),
        "the guest's number: {opened}"
    );
    let order = opened["order_id"].as_str().expect("an order id").to_owned();
    let line = ring(app, token, &order).await;
    (order, line)
}

/// Each request, from the kiosk, is refused `KIOSK_NOT_ALLOWED`.
async fn refused(store: &Store, requests: Vec<(&str, String, Value)>) {
    for (method, uri, body) in requests {
        let body = (!body.is_null()).then_some(body);
        let (status, reason, _) = send(&store.app, &store.kiosk, method, &uri, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        assert_eq!(
            reason.as_deref(),
            Some("KIOSK_NOT_ALLOWED"),
            "{method} {uri}"
        );
    }
}

#[tokio::test]
async fn a_kiosk_works_its_own_order_and_is_refused_everything_else() {
    let store = store().await;
    let (status, _, _) = send(&store.app, &store.kiosk, "POST", "/api/session/kiosk", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &store.app,
        &store.counter,
        "POST",
        "/api/session/sign-in",
        Some(json!({ "code": "MGR-1", "pin": MANAGER_PIN })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The kiosk's own order: rung, read, a line changed and voided, then sent to be paid.
    let (mine, my_line) = an_order_with_a_pizza(&store.app, &store.kiosk).await;
    let extra = ring(&store.app, &store.kiosk, &mine).await;
    let (status, _, body) = send(
        &store.app,
        &store.kiosk,
        "POST",
        &format!("/api/lines/{extra}/void"),
        // The framework's `ABANDONED`, which a kiosk cites (ADR-0224 decision 5).
        Some(json!({ "reason_code_id": Ulid::from_u128(13).to_string() })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a kiosk voids its own unsent line: {body}"
    );
    let (status, _, _) = send(
        &store.app,
        &store.kiosk,
        "GET",
        &format!("/api/orders/{mine}/check"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, bill) = send(
        &store.app,
        &store.kiosk,
        "POST",
        &format!("/api/orders/{mine}/bill"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "sent to be paid: {bill}");
    let bill = bill["bill_id"].as_str().expect("a bill").to_owned();

    // Money, the drawer, and the store's lists are not a kiosk's.
    refused(
        &store,
        vec![
            (
                "POST",
                format!("/api/bills/{bill}/settle"),
                json!({ "payments": [] }),
            ),
            ("POST", format!("/api/bills/{bill}/payments"), json!({})),
            ("POST", format!("/api/bills/{bill}/discount"), json!({})),
            ("POST", format!("/api/bills/{bill}/comp"), json!({})),
            ("POST", format!("/api/bills/{bill}/void"), json!({})),
            ("POST", format!("/api/lines/{my_line}/price"), json!({})),
            ("POST", "/api/drawer/open".to_owned(), json!({})),
            ("POST", "/api/shifts".to_owned(), json!({})),
            ("GET", "/api/orders/open".to_owned(), Value::Null),
            ("GET", "/api/orders/live".to_owned(), Value::Null),
            ("GET", "/api/floor".to_owned(), Value::Null),
            ("GET", "/api/reports/takings".to_owned(), Value::Null),
        ],
    )
    .await;

    // The manager's order at the counter is out of the kiosk's reach, however it asks.
    let (theirs, their_line) = an_order_with_a_pizza(&store.app, &store.counter).await;
    refused(
        &store,
        vec![
            ("GET", format!("/api/orders/{theirs}/check"), Value::Null),
            ("POST", format!("/api/orders/{theirs}/lines"), json!({})),
            ("POST", format!("/api/orders/{theirs}/bill"), json!({})),
            ("POST", format!("/api/orders/{theirs}/close"), json!({})),
            ("POST", format!("/api/lines/{their_line}/void"), json!({})),
            (
                "POST",
                format!("/api/lines/{their_line}/quantity"),
                json!({}),
            ),
        ],
    )
    .await;
    // And the manager, a person, is not held to the kiosk's routes.
    let (status, _, _) = send(&store.app, &store.counter, "GET", "/api/orders/open", None).await;
    assert_eq!(status, StatusCode::OK);
}

/// A kiosk never opens the store's event stream, which carries every till's orders (ADR-0224
/// decision 4); an ordinary till's device is not refused for it.
#[tokio::test]
async fn a_kiosk_may_not_open_the_stores_event_stream() {
    let store = store().await;
    let upgrade = |token: &str| {
        Request::builder()
            .uri("/ws")
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .expect("request builds")
    };
    let kiosk = store
        .app
        .clone()
        .oneshot(upgrade(&store.kiosk))
        .await
        .expect("router responds");
    assert_eq!(kiosk.status(), StatusCode::FORBIDDEN);
    let counter = store
        .app
        .clone()
        .oneshot(upgrade(&store.counter))
        .await
        .expect("router responds");
    assert_ne!(
        counter.status(),
        StatusCode::FORBIDDEN,
        "only the handshake's own rules apply"
    );
    assert!(store.edge.session().devices.devices().len() >= 2);
}

/// Whether a device is a kiosk is read from its binding; a binding that cannot be read, on a store
/// with a kiosk terminal, opens no event stream for any device, and signs no kiosk in (phase 9
/// security review, finding K1).
#[tokio::test]
async fn an_unreadable_binding_opens_no_event_stream() {
    let store = store_reading_bindings(false).await;
    for token in [&store.kiosk, &store.counter] {
        let response = store
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ws")
                    .header("connection", "Upgrade")
                    .header("upgrade", "websocket")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    let (status, _, _) = send(&store.app, &store.kiosk, "POST", "/api/session/kiosk", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Only the published `devices` node makes a terminal a kiosk: a session that carries the class
/// any other way holds no kiosk record. And once the node stops making the terminal a kiosk, its
/// record is gone and the device cannot sign in as it.
#[tokio::test]
async fn a_kiosk_record_comes_only_from_the_published_node_and_goes_with_it() {
    let store = store().await;
    let (status, _, _) = send(&store.app, &store.kiosk, "POST", "/api/session/kiosk", None).await;
    assert_eq!(status, StatusCode::OK);
    let record = EmployeeId::new(Ulid::from_u128(KIOSK_RECORD));

    // Set by hand, the class makes no record: only a node applied through the config path does.
    let mut by_hand = store.edge.session().as_ref().clone();
    by_hand.staff = staff();
    store.edge.apply_session(by_hand);
    assert!(!store.edge.session().staff.is_kiosk(record));
    let (status, _, _) = send(&store.app, &store.kiosk, "POST", "/api/session/kiosk", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Published again as a kiosk, then retired from the class: the record goes with it.
    for (class, signs_in) in [
        (Some(DeviceClass::Kiosk), StatusCode::OK),
        (None, StatusCode::FORBIDDEN),
    ] {
        let next = pos_edge::config_client::session_from_config(
            &store.edge.session(),
            &devices_node(class),
        );
        store.edge.apply_session(next);
        let (status, _, _) =
            send(&store.app, &store.kiosk, "POST", "/api/session/kiosk", None).await;
        assert_eq!(status, signs_in, "{class:?}");
    }
}

/// What a kiosk wrote names its terminal for good: the record goes with the kiosk class, and the
/// order it opened still names the terminal's own id, which no person's record can carry.
#[tokio::test]
async fn a_kiosks_history_outlives_its_record() {
    use pos_ports::event_store::{EventQuery, EventStore as _};

    let store = store().await;
    let (status, _, _) = send(&store.app, &store.kiosk, "POST", "/api/session/kiosk", None).await;
    assert_eq!(status, StatusCode::OK);
    an_order_with_a_pizza(&store.app, &store.kiosk).await;
    let retired =
        pos_edge::config_client::session_from_config(&store.edge.session(), &devices_node(None));
    store.edge.apply_session(retired);
    let record = EmployeeId::new(Ulid::from_u128(KIOSK_RECORD));
    assert!(!store.edge.session().staff.is_kiosk(record));

    let query = EventQuery::first(
        StoreId::new(Ulid::from_u128(1)),
        std::num::NonZeroU32::new(100).expect("a positive limit"),
    );
    let written = store.log.read(&query).await.expect("the log reads");
    let opened = written
        .iter()
        .find(|envelope| envelope.event_type.as_str() == "sales.order.opened")
        .expect("the kiosk opened an order");
    assert_eq!(opened.employee_id, Some(record));
    assert_eq!(record.as_ulid(), id(KIOSK_TERMINAL).as_ulid());
}
