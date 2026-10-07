// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A drawer per till
//! ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decisions 2, 3, 5 and 8).
//!
//! Where a store keeps a drawer per till, each till's drawer runs a shift of its own: started,
//! counted and closed on its own, with its own float, movements, expectation and over/short. Another
//! till's drawer is managed from any device by somebody holding `cash.shift.manage_other_till`, or
//! approved for one act. A change of model waits until no drawer is open, and the log rebuilds the
//! same drawers. Where a store keeps one drawer nothing changes, and its events name no till.

use std::future::Future;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::pairing::Minter;
use pos_edge::print_agent::PrintAgents;
use pos_edge::printing::{AgentDispatch, AgentLane, PrintOutcome, Printers};
use pos_edge::{
    AppError, Approval, CashMovement, Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts,
    Pairing, Sessions, StaffAuth, StaffRoster, StoreIdentity, SystemClock, TillScope,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::printer::PrintJob;
use pos_ports::{PortError, PortName};
use pos_proto::devices::{DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::CashShiftClosed;
use pos_proto::ids::{DeviceId, EmployeeId, ReasonCodeId, StoreId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::reason_codes::{
    PublishedReasonCode, PublishedReasonCodes, ReasonAction, ReasonCode,
};
use pos_proto::shift::{DrawerModel, PublishedShift};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use pos_proto::{ClockSource, ShiftState};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A fixed 16-byte salt, so a hashed fixture is deterministic. Never used in production.
const SALT: &[u8] = b"a-fixed-test-slt";

/// The bar's till and the counter's, two `TERMINAL` entries.
const BAR: u128 = 0x7111;
const COUNTER: u128 = 0x7112;

/// Who works here: a cashier who keeps to their own till's drawer, one who may manage another's
/// with somebody's approval, a supervisor who may alone, and the manager who approves.
const CASHIER: u128 = 21;
const ASKING: u128 = 22;
const SUPERVISOR: u128 = 23;
const MANAGER: u128 = 24;
const MANAGER_CODE: &str = "MGR-1";
const MANAGER_PIN: &str = "4417";

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), SALT)
        .expect("hash")
        .to_string()
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn till(seed: u128) -> DeviceId {
    DeviceId::new(Ulid::from_u128(seed))
}

fn making_change() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_101))
}

/// A `TERMINAL` entry, with a float of its own or none.
fn terminal(seed: u128, name: &str, float: Option<i64>) -> PublishedDevice {
    PublishedDevice {
        device_id: till(seed),
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
        opening_float_minor: float,
    }
}

fn staff() -> StaffRoster {
    let shifts = PermissionSet::EMPTY
        .with(Permission::OpenShift)
        .with(Permission::CloseShift)
        .with(Permission::RecordCashMovement);
    let manage = PermissionSet::EMPTY.with(Permission::ManageOtherTill);
    let mut staff = StaffRoster::new();
    for (code, seed, direct, approved, pin) in [
        ("CSH-1", CASHIER, shifts, PermissionSet::EMPTY, None),
        ("ASK-1", ASKING, shifts, manage, None),
        (
            "SUP-1",
            SUPERVISOR,
            shifts.with(Permission::ManageOtherTill),
            PermissionSet::EMPTY,
            None,
        ),
        (
            MANAGER_CODE,
            MANAGER,
            shifts.with(Permission::ManageOtherTill),
            PermissionSet::EMPTY,
            Some(hash_of(MANAGER_PIN)),
        ),
    ] {
        staff.insert(
            code,
            StaffAuth {
                employee_id: Some(EmployeeId::new(Ulid::from_u128(seed))),
                permissions: direct,
                permissions_with_approval: approved,
                discount_ceiling: None,
                pin_phc: pin,
            },
        );
    }
    staff
}

/// The store: two tills, the counter's with a float of its own, under `model`, each person's own
/// permissions enforced where `enforced`.
fn session(model: DrawerModel, enforced: bool) -> EdgeSession {
    let mut session = EdgeSession {
        devices: PublishedDevices::new(vec![
            terminal(BAR, "Bar", None),
            terminal(COUNTER, "Counter", Some(300_000)),
        ]),
        shift: PublishedShift {
            drawer_model: Open::from_known(model),
            opening_float_minor: Some(500_000),
            ..PublishedShift::default()
        },
        ..EdgeSession::bootstrap()
    };
    session.staff = staff();
    session.permissions_enforced = enforced;
    session.reason_codes = PublishedReasonCodes::from_parts(vec![PublishedReasonCode::new(
        making_change(),
        ReasonCode::new("MAKING_CHANGE"),
        DisplayName::new("Making change"),
        vec![ReasonAction::CashPaidIn, ReasonAction::CashPaidOut],
    )]);
    session
}

fn edge_over(store: FakeStore, session: EdgeSession) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

/// `employee` acting on the device `device`.
fn person(employee: u128, device: u128) -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(employee)),
        device_id: DeviceId::new(Ulid::from_u128(device)),
    }
}

/// A request from a device that is `own`'s till, naming no other.
fn at(own: u128) -> TillScope {
    TillScope {
        own: Some(till(own)),
        named: None,
    }
}

/// A request from a device that is `own`'s till, or none, naming `named`'s drawer.
fn naming(own: Option<u128>, named: u128) -> TillScope {
    TillScope {
        own: own.map(till),
        named: Some(till(named)),
    }
}

fn manager() -> Approval {
    Approval {
        code: MANAGER_CODE.to_owned(),
        pin: MANAGER_PIN.to_owned(),
    }
}

async fn logged(store: &FakeStore) -> Vec<EventEnvelope<RawPayload>> {
    let query = EventQuery::first(
        StoreId::new(Ulid::from_u128(1)),
        NonZeroU32::new(200).expect("a positive limit"),
    );
    store.read(&query).await.expect("read the log")
}

#[test]
fn two_tills_each_count_and_close_their_own_drawer() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), session(DrawerModel::PerTerminal, true));
        let bar = edge
            .open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None)
            .await
            .expect("the bar's drawer opens");
        let counter = edge
            .open_shift_at(person(CASHIER, 0xC1), at(COUNTER), vnd(300_000), None)
            .await
            .expect("the counter's opens beside it");
        assert_eq!(
            (bar.till, counter.till),
            (Some(till(BAR)), Some(till(COUNTER)))
        );
        let again = edge
            .open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(1), None)
            .await;
        assert!(
            matches!(again, Err(AppError::ShiftAlreadyOpen)),
            "{again:?}"
        );

        edge.record_cash_movement(
            person(CASHIER, 0xB1),
            bar.shift_id,
            CashMovement::PaidIn,
            vnd(100_000),
            making_change(),
        )
        .await
        .expect("paid in at the bar");
        edge.record_cash_movement(
            person(CASHIER, 0xC1),
            counter.shift_id,
            CashMovement::PaidOut,
            vnd(50_000),
            making_change(),
        )
        .await
        .expect("paid out at the counter");

        for (device, own, shift, counted, expected) in [
            (0xB1, BAR, bar.shift_id, 590_000, 600_000),
            (0xC1, COUNTER, counter.shift_id, 250_000, 250_000),
        ] {
            edge.count_shift_at(person(CASHIER, device), at(own), shift, counted, None)
                .await
                .expect("counts its own");
            let closed = edge
                .close_shift_at(person(CASHIER, device), at(own), shift, None)
                .await
                .expect("closes its own");
            assert_eq!(closed.till, Some(till(own)));
            assert_eq!(closed.expected_amount, Some(vnd(expected)), "{own:x}");
            assert_eq!(closed.variance, Some(vnd(counted - expected)), "{own:x}");
        }

        let closed: Vec<Option<DeviceId>> = logged(&store)
            .await
            .iter()
            .filter(|envelope| envelope.event_type.as_str() == "cash.shift.closed")
            .map(|envelope| {
                envelope
                    .data
                    .decode::<CashShiftClosed>()
                    .expect("decodes")
                    .terminal_device_id
            })
            .collect();
        assert_eq!(closed, [Some(till(BAR)), Some(till(COUNTER))]);
    });
}

#[test]
fn a_device_that_is_no_till_opens_only_the_drawer_it_names() {
    run_ready(async {
        let edge = edge_over(FakeStore::new(), session(DrawerModel::PerTerminal, true));
        let refused = edge
            .open_shift_at(person(SUPERVISOR, 0xD1), TillScope::default(), vnd(1), None)
            .await;
        assert!(matches!(refused, Err(AppError::NotATill)), "{refused:?}");
        let unknown = edge
            .open_shift_at(person(SUPERVISOR, 0xD1), naming(None, 0x9999), vnd(1), None)
            .await;
        assert!(
            matches!(unknown, Err(AppError::NotATill)),
            "a till the store does not list: {unknown:?}"
        );
        let opened = edge
            .open_shift_at(person(SUPERVISOR, 0xD1), naming(None, BAR), vnd(1), None)
            .await
            .expect("a supervisor opens the bar's drawer from a device that is no till");
        assert_eq!(opened.till, Some(till(BAR)));
    });
}

#[test]
fn another_tills_drawer_takes_the_permission_held_directly_or_approved() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), session(DrawerModel::PerTerminal, true));

        let refused = edge
            .open_shift_at(
                person(CASHIER, 0xB1),
                naming(Some(BAR), COUNTER),
                vnd(300_000),
                None,
            )
            .await;
        assert!(
            matches!(
                refused,
                Err(AppError::Domain(DomainError::PermissionDenied { permission }))
                    if permission == "cash.shift.manage_other_till"
            ),
            "a cashier keeps to their own till's drawer: {refused:?}"
        );

        let unapproved = edge
            .open_shift_at(
                person(ASKING, 0xB1),
                naming(Some(BAR), COUNTER),
                vnd(300_000),
                None,
            )
            .await;
        assert!(
            matches!(unapproved, Err(AppError::ApprovalRequired)),
            "{unapproved:?}"
        );
        let counter = edge
            .open_shift_at(
                person(ASKING, 0xB1),
                naming(Some(BAR), COUNTER),
                vnd(300_000),
                Some(&manager()),
            )
            .await
            .expect("a manager approves opening the counter's drawer");
        assert!(
            logged(&store)
                .await
                .iter()
                .any(|envelope| envelope.event_type.as_str() == "security.permission.overridden"),
            "the approval is its own record"
        );

        // Their own till's drawer needs only the shift's own permission.
        edge.open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None)
            .await
            .expect("the cashier opens the bar's own");

        edge.count_shift_at(
            person(SUPERVISOR, 0xB1),
            at(BAR),
            counter.shift_id,
            300_000,
            None,
        )
        .await
        .expect("a supervisor counts another till's drawer alone");
        let close_refused = edge
            .close_shift_at(person(CASHIER, 0xB1), at(BAR), counter.shift_id, None)
            .await;
        assert!(
            matches!(
                close_refused,
                Err(AppError::Domain(DomainError::PermissionDenied { .. }))
            ),
            "{close_refused:?}"
        );
        let closed = edge
            .close_shift_at(person(CASHIER, 0xC1), at(COUNTER), counter.shift_id, None)
            .await
            .expect("the counter's own cashier closes it");
        assert_eq!(closed.variance, Some(vnd(0)));
    });
}

#[test]
fn a_change_of_model_waits_for_the_open_drawer_to_close() {
    run_ready(async {
        let edge = edge_over(FakeStore::new(), session(DrawerModel::PerStore, false));
        let open = edge
            .open_shift(person(MANAGER, 0xB1), vnd(500_000))
            .await
            .expect("the store's drawer opens");
        assert_eq!(open.till, None);

        edge.apply_session(session(DrawerModel::PerTerminal, false));
        assert_eq!(
            edge.drawer_model(),
            DrawerModel::PerStore,
            "the open drawer keeps its model"
        );
        let waiting = edge.drawers();
        assert_eq!(waiting.waiting, Some(DrawerModel::PerTerminal));
        assert_eq!(waiting.drawers.len(), 1, "still the store's one drawer");
        let refused = edge
            .open_shift_at(person(MANAGER, 0xB1), at(BAR), vnd(1), None)
            .await;
        assert!(
            matches!(refused, Err(AppError::ShiftAlreadyOpen)),
            "{refused:?}"
        );

        edge.count_shift(person(MANAGER, 0xB1), open.shift_id, 500_000)
            .await
            .expect("counts");
        edge.close_shift(person(MANAGER, 0xB1), open.shift_id)
            .await
            .expect("closes");
        assert_eq!(
            edge.drawer_model(),
            DrawerModel::PerTerminal,
            "at the boundary"
        );
        let applied = edge.drawers();
        assert_eq!(applied.waiting, None);
        let tills: Vec<(Option<DeviceId>, Money)> = applied
            .drawers
            .iter()
            .map(|drawer| (drawer.till, drawer.default_float))
            .collect();
        assert_eq!(
            tills,
            [
                (Some(till(BAR)), vnd(500_000)),
                (Some(till(COUNTER)), vnd(300_000)),
            ],
            "each till, with its own float where it sets one and the store's otherwise"
        );
    });
}

#[test]
fn a_restart_rebuilds_every_tills_drawer_and_one_drawers_log_rebuilds_one() {
    run_ready(async {
        let store = FakeStore::new();
        let (bar, counter) = {
            let edge = edge_over(store.clone(), session(DrawerModel::PerTerminal, true));
            let bar = edge
                .open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None)
                .await
                .expect("the bar's")
                .shift_id;
            let counter = edge
                .open_shift_at(person(CASHIER, 0xC1), at(COUNTER), vnd(300_000), None)
                .await
                .expect("the counter's")
                .shift_id;
            edge.record_cash_movement(
                person(CASHIER, 0xB1),
                bar,
                CashMovement::PaidIn,
                vnd(100_000),
                making_change(),
            )
            .await
            .expect("paid in");
            edge.count_shift_at(person(CASHIER, 0xC1), at(COUNTER), counter, 300_000, None)
                .await
                .expect("counted");
            (bar, counter)
        };

        let restarted = edge_over(store.clone(), session(DrawerModel::PerTerminal, true));
        restarted.rebuild().await.expect("replays the log");
        assert_eq!(restarted.drawer_model(), DrawerModel::PerTerminal);
        let at_bar = restarted
            .current_shift_at(at(BAR))
            .expect("the bar's is open");
        assert_eq!(
            (at_bar.shift_id, at_bar.state, at_bar.paid_in),
            (bar, ShiftState::Open, vnd(100_000))
        );
        let at_counter = restarted
            .current_shift_at(at(COUNTER))
            .expect("the counter's is open");
        assert_eq!(
            (at_counter.shift_id, at_counter.counted_amount),
            (counter, Some(vnd(300_000)))
        );
        let closed = restarted
            .close_shift_at(person(CASHIER, 0xC1), at(COUNTER), counter, None)
            .await
            .expect("closes after the restart");
        assert_eq!(closed.expected_amount, Some(vnd(300_000)));

        // A log of one drawer names no till, and rebuilds as the store's one drawer.
        let one = FakeStore::new();
        let shift = edge_over(one.clone(), session(DrawerModel::PerStore, false))
            .open_shift(person(MANAGER, 0xB1), vnd(200_000))
            .await
            .expect("opens")
            .shift_id;
        let restarted = edge_over(one.clone(), session(DrawerModel::PerStore, false));
        restarted.rebuild().await.expect("replays the log");
        let view = restarted.drawers();
        assert_eq!(view.model, DrawerModel::PerStore);
        let only: Vec<_> = view
            .drawers
            .iter()
            .map(|drawer| (drawer.till, drawer.shift.as_ref().map(|open| open.shift_id)))
            .collect();
        assert_eq!(only, [(None, Some(shift))]);
        assert!(
            logged(&one)
                .await
                .iter()
                .all(|envelope| !envelope.data.as_json().contains("terminal_device_id")),
            "one drawer's events name no till"
        );
    });
}

// --- Over the shipped routes ------------------------------------------------------------------

async fn send(
    app: Router,
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
    let response = app.oneshot(request).await.expect("router responds");
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

/// A binding record that cannot be read.
struct Unreadable;

impl AgentDispatch for Unreadable {
    fn enqueue<'a>(
        &'a self,
        _agent: DeviceId,
        _printer: DeviceId,
        _job: PrintJob,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>> {
        Box::pin(async { PrintOutcome::AgentUnavailable })
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

/// The store over the shipped routes, keeping a drawer per till: a device bound to the bar's till,
/// and one bound to none, both signed in by the manager. The binding is the print agents' record,
/// as `serve` composes it, or one that cannot be read where `unreadable`.
async fn drawers_app(unreadable: bool) -> (Router, Arc<Edge<FakeStore>>, String, String) {
    let edge = Arc::new(edge_over(
        FakeStore::new(),
        session(DrawerModel::PerTerminal, false),
    ));
    let agents = Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new());
    let queue = Arc::new(pos_edge::print_queue::InMemoryPrintQueue::new());
    let wake = pos_edge::print_wake::SharedPrintWake::new();
    let dispatch: Arc<dyn AgentDispatch> = if unreadable {
        Arc::new(Unreadable)
    } else {
        Arc::new(AgentLane::new(
            Arc::clone(&agents),
            Arc::clone(&queue),
            wake.clone(),
        ))
    };
    let printers = Arc::new(Printers::tcp().with_agents(dispatch));
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let mut paired = Vec::new();
    for _ in 0..2 {
        let (code, _) = pairing
            .mint(now, Minter::Boot)
            .expect("mint a pairing code");
        let token = pairing
            .redeem(&code, now)
            .await
            .expect("redeem")
            .token()
            .expect("a fresh code pairs a device");
        let device = pairing
            .device_for(&token)
            .expect("a freshly issued token resolves");
        paired.push((device, token.as_str().to_owned()));
    }
    let (_, stranger) = paired.pop().expect("a device that is no till");
    let (bar_device, bar) = paired.pop().expect("the bar's device");
    agents
        .claim(till(BAR), bar_device, now.as_milliseconds_since_epoch())
        .await
        .expect("the manager's binding is recorded");
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
    .layer(axum::Extension(printers));
    for token in [&bar, &stranger] {
        let (status, _, _) = send(
            app.clone(),
            token,
            "POST",
            "/api/session/sign-in",
            Some(json!({ "code": MANAGER_CODE, "pin": MANAGER_PIN })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "the manager signs in");
    }
    (app, edge, bar, stranger)
}

#[tokio::test]
async fn the_drawers_read_lists_each_till_and_a_bound_device_opens_its_own() {
    let (app, edge, bar, stranger) = drawers_app(false).await;
    let (status, _, read) = send(app.clone(), &bar, "GET", "/api/shifts", None).await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(read["drawer_model"], "DRAWER_MODEL_PER_TERMINAL");
    assert_eq!(
        read["drawers"][0]["terminal_device_id"],
        till(BAR).to_string()
    );
    assert_eq!(read["drawers"][0]["name"], "Bar");
    assert_eq!(read["drawers"][1]["default_float"], json!(vnd(300_000)));
    assert!(read["drawers"][0]["shift"].is_null(), "{read}");

    let (status, _, opened) = send(
        app.clone(),
        &bar,
        "POST",
        "/api/shifts",
        Some(json!({ "opening_float": vnd(500_000) })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert_eq!(opened["terminal_device_id"], till(BAR).to_string());
    let (_, _, current) = send(app.clone(), &bar, "GET", "/api/shifts/current", None).await;
    assert_eq!(current["shift_id"], opened["shift_id"]);

    let open = json!({ "opening_float": vnd(300_000) });
    let (status, reason, _) = send(app.clone(), &stranger, "POST", "/api/shifts", Some(open)).await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("NOT_A_TILL"))
    );
    let named = json!({ "opening_float": vnd(300_000), "terminal_device_id": till(COUNTER) });
    let (status, reason, _) =
        send(app.clone(), &stranger, "POST", "/api/shifts", Some(named)).await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::FORBIDDEN, Some("APPROVAL_REQUIRED")),
        "another till's drawer is PIN-flagged"
    );
    let approved = json!({
        "opening_float": vnd(300_000),
        "terminal_device_id": till(COUNTER),
        "approver_code": MANAGER_CODE,
        "approver_pin": MANAGER_PIN,
    });
    let (status, _, counter) = send(
        app.clone(),
        &stranger,
        "POST",
        "/api/shifts",
        Some(approved),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{counter}");
    assert_eq!(counter["terminal_device_id"], till(COUNTER).to_string());
    let (_, _, none) = send(app.clone(), &stranger, "GET", "/api/shifts/current", None).await;
    assert!(
        none.is_null(),
        "a device that is no till has no drawer: {none}"
    );

    // Counting and closing another till's drawer takes the approver in the request too.
    let count = format!(
        "/api/shifts/{}/count",
        counter["shift_id"].as_str().unwrap_or_default()
    );
    let (status, reason, _) = send(
        app.clone(),
        &bar,
        "POST",
        &count,
        Some(json!({ "counted_minor": 300_000 })),
    )
    .await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::FORBIDDEN, Some("APPROVAL_REQUIRED"))
    );
    let (status, _, _) = send(
        app.clone(),
        &bar,
        "POST",
        &count,
        Some(json!({
            "counted_minor": 300_000,
            "approver_code": MANAGER_CODE,
            "approver_pin": MANAGER_PIN,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, _, read) = send(app.clone(), &bar, "GET", "/api/shifts", None).await;
    assert!(read["drawers"][1]["opened_time"].is_string(), "{read}");
    assert_eq!(read["drawers"][1]["shift"]["shift_id"], counter["shift_id"]);
    assert!(
        read.get("waiting_drawer_model").is_none(),
        "nothing waits: {read}"
    );

    // The store publishes one drawer while two are open: it waits, and the read says so.
    edge.apply_session(session(DrawerModel::PerStore, false));
    let (_, _, read) = send(app.clone(), &bar, "GET", "/api/shifts", None).await;
    assert_eq!(read["drawer_model"], "DRAWER_MODEL_PER_TERMINAL");
    assert_eq!(read["waiting_drawer_model"], "DRAWER_MODEL_PER_STORE");
}

#[tokio::test]
async fn a_binding_that_cannot_be_read_refuses_rather_than_guesses_the_till() {
    let (app, edge, bar, _) = drawers_app(true).await;
    let open = json!({ "opening_float": vnd(500_000) });
    let (status, reason, _) =
        send(app.clone(), &bar, "POST", "/api/shifts", Some(open.clone())).await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("STORE_UNAVAILABLE"))
    );
    let (status, _, read) = send(app.clone(), &bar, "GET", "/api/shifts", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "listing the drawers reads no binding: {read}"
    );

    // Where the store keeps one drawer it is every device's, and nothing asks which till this is.
    edge.apply_session(session(DrawerModel::PerStore, false));
    let (status, _, opened) = send(app, &bar, "POST", "/api/shifts", Some(open)).await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert!(opened.get("terminal_device_id").is_none(), "{opened}");
}
