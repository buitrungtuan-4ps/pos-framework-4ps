// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A drawer per till
//! ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decisions 2, 3, 5, 6
//! and 8).
//!
//! Where a store keeps a drawer per till, each till's drawer runs a shift of its own: started,
//! counted and closed on its own, with its own float, cash, movements, expectation and over/short.
//! Cash goes into the drawer of the till it is taken at and springs that drawer alone, and a device
//! that is no till takes any other tender but no cash. Another till's drawer is managed from any
//! paired device by somebody holding `cash.shift.manage_other_till`, or approved for one act, and
//! only somebody holding it alone reads what that drawer should hold. A change of model waits until
//! no drawer is open, and the log rebuilds the same drawers. Where a store keeps one drawer nothing
//! changes, and its events name no till.
//!
//! Drawers closed together are each asked the reason a close over or short by more than the store's
//! `shift.variance_reason_minor` gives (decision 12), and close every one or none.

use std::future::Future;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::billing::Payment;
use pos_core::business_date::StoreTimeZone;
use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::pairing::Minter;
use pos_edge::print_agent::PrintAgents;
use pos_edge::printing::{AgentDispatch, AgentLane, PrintOutcome, Printers, TransportFactory};
use pos_edge::{
    AppError, Approval, CashMovement, DrawerFigures, Edge, EdgeSession, InMemoryQueueNumbers,
    InMemoryReceipts, LineDraft, Pairing, Sessions, StaffAuth, StaffRoster, StoreIdentity,
    SystemClock, TillScope,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::printer::PrintJob;
use pos_ports::{PortError, PortName};
use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{CashDrawerOpened, CashShiftClosed};
use pos_proto::ids::{BillId, DeviceId, EmployeeId, MenuItemId, ReasonCodeId, StoreId, TableId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::printing::PublishedPrinting;
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::{
    PublishedReasonCode, PublishedReasonCodes, ReasonAction, ReasonCode,
};
use pos_proto::shift::{CloseReport, DrawerDayEnd, DrawerModel, NoShiftSelling, PublishedShift};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use pos_proto::{ClockSource, PaymentMethod, SalesChannel, ShiftState};
use printer_escpos::{Transport, TransportStatus, Unreachable};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A fixed 16-byte salt, so a hashed fixture is deterministic. Never used in production.
const SALT: &[u8] = b"a-fixed-test-slt";

/// The bar's till and the counter's, two `TERMINAL` entries.
const BAR: u128 = 0x7111;
const COUNTER: u128 = 0x7112;

/// The printers, each on USB and serving the bill: the store's receipt printer, with a drawer no
/// till names; the bar's, with the drawer the bar's till names; and the counter till's own, with no
/// drawer marked.
const STORE_PRINTER: u128 = 0x9100;
const BAR_PRINTER: u128 = 0x9130;
const COUNTER_PRINTER: u128 = 0x9140;
const STORE_DRAWER: &str = "/dev/usb/lp0";
const BAR_DRAWER: &str = "/dev/usb/lp1";
const COUNTER_PAPER: &str = "/dev/usb/lp2";

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

fn pizza() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(5))
}

fn a_pizza() -> LineDraft {
    LineDraft {
        menu_item_id: pizza(),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: vnd(150_000),
        line_total: vnd(150_000),
        tax_class_id: EdgeSession::standard_tax_class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

/// A `TERMINAL` entry, with a float of its own or none, naming `printer` as its receipt printer.
fn terminal(seed: u128, name: &str, float: Option<i64>, printer: Option<u128>) -> PublishedDevice {
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
        receipt_printer_id: printer.map(|seed| DeviceId::new(Ulid::from_u128(seed))),
        receipt_language: Open::default(),
        receipt_second_language: Open::default(),
        opening_float_minor: float,
    }
}

/// A USB printer that serves the bill at `address`, with a cash drawer marked where `drawer`.
fn printer(seed: u128, address: &str, drawer: bool) -> PublishedDevice {
    PublishedDevice {
        device_id: DeviceId::new(Ulid::from_u128(seed)),
        kind: DeviceKind::Printer.into(),
        connection: DeviceConnection::Usb.into(),
        address: address.to_owned(),
        name: DisplayName::new("Printer"),
        station_id: None,
        agent_device_id: None,
        drawer_attached: drawer,
        paper_width: Open::default(),
        cuts_paper: None,
        receipt_printer_id: None,
        receipt_language: Open::default(),
        receipt_second_language: Open::default(),
        opening_float_minor: None,
    }
}

fn staff() -> StaffRoster {
    let shifts = PermissionSet::EMPTY
        .with(Permission::OpenShift)
        .with(Permission::CloseShift)
        .with(Permission::RecordCashMovement)
        .with(Permission::ManageTables)
        .with(Permission::AddLine)
        .with(Permission::OpenBill)
        .with(Permission::TakePayment);
    let manage = PermissionSet::EMPTY.with(Permission::ManageOtherTill);
    let managing = shifts
        .with(Permission::ManageOtherTill)
        .with(Permission::OpenDrawerNoSale);
    let mut staff = StaffRoster::new();
    for (code, seed, direct, approved, pin) in [
        ("CSH-1", CASHIER, shifts, PermissionSet::EMPTY, None),
        ("ASK-1", ASKING, shifts, manage, None),
        ("SUP-1", SUPERVISOR, managing, PermissionSet::EMPTY, None),
        (
            MANAGER_CODE,
            MANAGER,
            managing,
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

/// The store: two tills, the bar's naming the bar's printer and the counter's, with a float of its
/// own, naming a printer with no drawer; the store's receipt printer, with a drawer no till names;
/// a pizza on the menu; and no receipt printed on a settle, so a printer is written only to open
/// its drawer. Under `model`, each person's own permissions enforced where `enforced`.
fn session(model: DrawerModel, enforced: bool) -> EdgeSession {
    let class = EdgeSession::standard_tax_class();
    let menu = MenuCatalog::new().with(MenuEntry::new(
        pizza(),
        DisplayName::new("Margherita"),
        vnd(150_000),
        class,
    ));
    let rates = TaxRateTable::new().with(class, SalesChannel::DineIn, TaxRate::from_percent(10));
    let mut session = EdgeSession {
        devices: PublishedDevices::new(vec![
            printer(STORE_PRINTER, STORE_DRAWER, true),
            terminal(BAR, "Bar", None, Some(BAR_PRINTER)),
            printer(BAR_PRINTER, BAR_DRAWER, true),
            terminal(COUNTER, "Counter", Some(300_000), Some(COUNTER_PRINTER)),
            printer(COUNTER_PRINTER, COUNTER_PAPER, false),
        ]),
        shift: PublishedShift {
            drawer_model: Open::from_known(model),
            opening_float_minor: Some(500_000),
            ..PublishedShift::default()
        },
        printing: PublishedPrinting {
            receipt_printed_on_settle: Some(false),
            ..PublishedPrinting::default()
        },
        ..EdgeSession::bootstrap()
            .with_menu(menu)
            .with_tax_rates(rates)
    };
    session.staff = staff();
    session.permissions_enforced = enforced;
    session.reason_codes = PublishedReasonCodes::from_parts(vec![PublishedReasonCode::new(
        making_change(),
        ReasonCode::new("MAKING_CHANGE"),
        DisplayName::new("Making change"),
        vec![
            ReasonAction::DrawerOpen,
            ReasonAction::CashPaidIn,
            ReasonAction::CashPaidOut,
        ],
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

/// Rings a pizza on table `table` and opens its bill, answering the bill and what it owes.
async fn a_bill(edge: &Edge<FakeStore>, by: Actor, table: u128) -> (BillId, Money) {
    let table = TableId::new(Ulid::from_u128(table));
    edge.seat_table(by, table, None).await.expect("seats");
    edge.add_line(by, table, a_pizza())
        .await
        .expect("rings a pizza");
    let bill = edge
        .open_bill(by, table)
        .await
        .expect("opens the bill")
        .bill_id;
    let due = edge.check_totals(table).expect("the check reads").total_due;
    (bill, due)
}

/// One tender of `due` by `method`.
fn paid(method: PaymentMethod, due: Money) -> Vec<Payment> {
    vec![Payment {
        method,
        tendered: due,
        applied_to_bill: due,
        tip: vnd(0),
    }]
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

        edge.record_cash_movement_at(
            person(CASHIER, 0xB1),
            at(BAR),
            bar.shift_id,
            CashMovement::PaidIn,
            vnd(100_000),
            making_change(),
        )
        .await
        .expect("paid in at the bar");
        edge.record_cash_movement_at(
            person(CASHIER, 0xC1),
            at(COUNTER),
            counter.shift_id,
            CashMovement::PaidOut,
            vnd(50_000),
            making_change(),
        )
        .await
        .expect("paid out at the counter");

        // Cash at the bar goes into the bar's drawer; a card at the counter goes into none.
        let (bill, due) = a_bill(&edge, person(CASHIER, 0xB1), 41).await;
        assert_eq!(due, vnd(165_000));
        let settled = edge
            .settle_bill_at(
                person(CASHIER, 0xB1),
                at(BAR),
                bill,
                paid(PaymentMethod::Cash, due),
                None,
            )
            .await
            .expect("cash at the bar");
        assert_eq!(
            (settled.open_drawer, settled.drawer_till),
            (true, Some(till(BAR)))
        );
        let (bill, due) = a_bill(&edge, person(CASHIER, 0xC1), 42).await;
        let settled = edge
            .settle_bill_at(
                person(CASHIER, 0xC1),
                at(COUNTER),
                bill,
                paid(PaymentMethod::Card, due),
                None,
            )
            .await
            .expect("a card at the counter");
        assert!(!settled.open_drawer, "a card opens no drawer");

        for (device, own, shift, counted, expected) in [
            (0xB1, BAR, bar.shift_id, 755_000, 765_000),
            (0xC1, COUNTER, counter.shift_id, 250_000, 250_000),
        ] {
            edge.count_shift_at(person(CASHIER, device), at(own), shift, counted, None)
                .await
                .expect("counts its own");
            let closed = edge
                .close_shift_at(person(CASHIER, device), at(own), shift, None, None)
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
            .close_shift_at(person(CASHIER, 0xB1), at(BAR), counter.shift_id, None, None)
            .await;
        assert!(
            matches!(
                close_refused,
                Err(AppError::Domain(DomainError::PermissionDenied { .. }))
            ),
            "{close_refused:?}"
        );
        let closed = edge
            .close_shift_at(
                person(CASHIER, 0xC1),
                at(COUNTER),
                counter.shift_id,
                None,
                None,
            )
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
        let waiting = edge.drawers(person(MANAGER, 0xB1), TillScope::default());
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
        let applied = edge.drawers(person(MANAGER, 0xB1), TillScope::default());
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
fn a_restart_rebuilds_every_tills_drawer_with_its_own_cash() {
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
            edge.record_cash_movement_at(
                person(CASHIER, 0xB1),
                at(BAR),
                bar,
                CashMovement::PaidIn,
                vnd(100_000),
                making_change(),
            )
            .await
            .expect("paid in");
            let (bill, due) = a_bill(&edge, person(CASHIER, 0xB1), 41).await;
            edge.settle_bill_at(
                person(CASHIER, 0xB1),
                at(BAR),
                bill,
                paid(PaymentMethod::Cash, due),
                None,
            )
            .await
            .expect("cash at the bar");
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
            .close_shift_at(person(CASHIER, 0xC1), at(COUNTER), counter, None, None)
            .await
            .expect("closes after the restart");
        assert_eq!(closed.expected_amount, Some(vnd(300_000)));
        restarted
            .count_shift_at(person(CASHIER, 0xB1), at(BAR), bar, 765_000, None)
            .await
            .expect("counted after the restart");
        let closed = restarted
            .close_shift_at(person(CASHIER, 0xB1), at(BAR), bar, None, None)
            .await
            .expect("closes after the restart");
        assert_eq!(
            closed.expected_amount,
            Some(vnd(765_000)),
            "the float, the paid in and the bar's cash, put back by the payment's own shift"
        );
    });
}

#[test]
fn a_log_of_one_drawer_names_no_till_and_rebuilds_as_the_stores_one_drawer() {
    run_ready(async {
        let one = FakeStore::new();
        let shift = {
            let edge = edge_over(one.clone(), session(DrawerModel::PerStore, false));
            let shift = edge
                .open_shift(person(MANAGER, 0xB1), vnd(200_000))
                .await
                .expect("opens")
                .shift_id;
            let (bill, due) = a_bill(&edge, person(MANAGER, 0xB1), 41).await;
            let settled = edge
                .settle_bill(
                    person(MANAGER, 0xB1),
                    bill,
                    paid(PaymentMethod::Cash, due),
                    None,
                )
                .await
                .expect("cash into the store's drawer");
            assert_eq!((settled.open_drawer, settled.drawer_till), (true, None));
            shift
        };
        let restarted = edge_over(one.clone(), session(DrawerModel::PerStore, false));
        restarted.rebuild().await.expect("replays the log");
        let view = restarted.drawers(person(MANAGER, 0xB1), TillScope::default());
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
        restarted
            .count_shift(person(MANAGER, 0xB1), shift, 365_000)
            .await
            .expect("counts");
        let closed = restarted
            .close_shift(person(MANAGER, 0xB1), shift)
            .await
            .expect("closes");
        assert_eq!(closed.expected_amount, Some(vnd(365_000)));
    });
}

#[test]
fn a_device_that_is_no_till_takes_any_tender_but_cash_and_a_till_moves_only_its_own_drawer() {
    run_ready(async {
        let edge = edge_over(FakeStore::new(), session(DrawerModel::PerTerminal, true));
        let bar = edge
            .open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None)
            .await
            .expect("the bar's drawer opens")
            .shift_id;

        // A device that is no till: a card, and nothing that moves cash.
        let elsewhere = person(SUPERVISOR, 0xD1);
        let (bill, due) = a_bill(&edge, elsewhere, 41).await;
        let cash = edge
            .settle_bill_at(
                elsewhere,
                TillScope::default(),
                bill,
                paid(PaymentMethod::Cash, due),
                None,
            )
            .await;
        assert!(matches!(cash, Err(AppError::NotATill)), "{cash:?}");
        let card = edge
            .settle_bill_at(
                elsewhere,
                TillScope::default(),
                bill,
                paid(PaymentMethod::Card, due),
                None,
            )
            .await
            .expect("a card is taken anywhere");
        assert_eq!((card.open_drawer, card.drawer_till), (false, None));
        let paid_in = edge
            .record_cash_movement_at(
                elsewhere,
                TillScope::default(),
                bar,
                CashMovement::PaidIn,
                vnd(1_000),
                making_change(),
            )
            .await;
        assert!(matches!(paid_in, Err(AppError::NotATill)), "{paid_in:?}");
        let no_sale = edge
            .open_drawer_no_sale_at(elsewhere, TillScope::default(), making_change(), None)
            .await;
        assert!(matches!(no_sale, Err(AppError::NotATill)), "{no_sale:?}");

        // A till moves cash in its own drawer, never another till's.
        edge.open_shift_at(person(CASHIER, 0xC1), at(COUNTER), vnd(300_000), None)
            .await
            .expect("the counter's drawer opens");
        let crossed = edge
            .record_cash_movement_at(
                person(CASHIER, 0xC1),
                at(COUNTER),
                bar,
                CashMovement::PaidIn,
                vnd(1_000),
                making_change(),
            )
            .await;
        assert!(
            matches!(crossed, Err(AppError::ShiftNotOpen)),
            "{crossed:?}"
        );
    });
}

#[test]
fn a_tills_payment_and_no_sale_opening_name_its_drawers_shift() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), session(DrawerModel::PerTerminal, true));
        let bar = edge
            .open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None)
            .await
            .expect("the bar's drawer opens")
            .shift_id;
        let counter = edge
            .open_shift_at(person(CASHIER, 0xC1), at(COUNTER), vnd(300_000), None)
            .await
            .expect("the counter's drawer opens")
            .shift_id;
        let (bill, due) = a_bill(&edge, person(SUPERVISOR, 0xD1), 41).await;
        edge.settle_bill_at(
            person(SUPERVISOR, 0xD1),
            TillScope::default(),
            bill,
            paid(PaymentMethod::Card, due),
            None,
        )
        .await
        .expect("a card at a device that is no till");
        let opened = edge
            .open_drawer_no_sale_at(person(SUPERVISOR, 0xB1), at(BAR), making_change(), None)
            .await
            .expect("a supervisor opens the bar's drawer");
        assert_eq!(opened, Some(till(BAR)));
        let (bill, due) = a_bill(&edge, person(CASHIER, 0xC1), 42).await;
        edge.settle_bill_at(
            person(CASHIER, 0xC1),
            at(COUNTER),
            bill,
            paid(PaymentMethod::Cash, due),
            None,
        )
        .await
        .expect("cash at the counter");
        let log = logged(&store).await;
        let opening = log
            .iter()
            .find(|envelope| envelope.event_type.as_str() == "cash.drawer.opened")
            .expect("the opening is logged");
        let event: CashDrawerOpened = opening.data.decode().expect("decodes");
        assert_eq!(
            (event.terminal_device_id, opening.shift_id),
            (Some(till(BAR)), Some(bar))
        );
        let payments: Vec<_> = log
            .iter()
            .filter(|envelope| envelope.event_type.as_str() == "billing.payment.captured")
            .map(|envelope| envelope.shift_id)
            .collect();
        assert_eq!(
            payments,
            [None, Some(counter)],
            "the card was no till's, and the counter's cash names the counter's shift"
        );
    });
}

#[test]
fn a_store_that_sells_only_in_a_shift_takes_cash_only_into_an_open_drawer() {
    run_ready(async {
        let mut refusing = session(DrawerModel::PerTerminal, true);
        refusing.shift.no_shift_selling = Open::from_known(NoShiftSelling::Refuse);
        let edge = edge_over(FakeStore::new(), refusing);
        let closed = edge
            .seat_table(
                person(CASHIER, 0xC1),
                TableId::new(Ulid::from_u128(41)),
                None,
            )
            .await;
        assert!(
            matches!(closed, Err(AppError::OpenShiftRequired)),
            "no drawer is open: {closed:?}"
        );

        edge.open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None)
            .await
            .expect("the bar's drawer opens");
        // Some drawer is open, so a sale starts and a card is taken at the counter too.
        let (bill, due) = a_bill(&edge, person(CASHIER, 0xC1), 41).await;
        let cash = edge
            .settle_bill_at(
                person(CASHIER, 0xC1),
                at(COUNTER),
                bill,
                paid(PaymentMethod::Cash, due),
                None,
            )
            .await;
        assert!(
            matches!(cash, Err(AppError::OpenShiftRequired)),
            "the counter's own drawer is not open: {cash:?}"
        );
        edge.settle_bill_at(
            person(CASHIER, 0xC1),
            at(COUNTER),
            bill,
            paid(PaymentMethod::Card, due),
            None,
        )
        .await
        .expect("a card needs some drawer open");
        let (bill, due) = a_bill(&edge, person(CASHIER, 0xB1), 42).await;
        edge.settle_bill_at(
            person(CASHIER, 0xB1),
            at(BAR),
            bill,
            paid(PaymentMethod::Cash, due),
            None,
        )
        .await
        .expect("cash into the bar's open drawer");
    });
}

#[test]
fn another_tills_expectation_shows_only_to_whoever_may_count_it_alone() {
    run_ready(async {
        let mut counted_open = session(DrawerModel::PerTerminal, true);
        counted_open.shift.blind_close = Some(false);
        let edge = edge_over(FakeStore::new(), counted_open);
        edge.open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None)
            .await
            .expect("the bar's");
        edge.open_shift_at(person(CASHIER, 0xC1), at(COUNTER), vnd(300_000), None)
            .await
            .expect("the counter's");
        let expected = |viewer: Actor, scope: TillScope| -> Vec<Option<Money>> {
            edge.drawers(viewer, scope)
                .drawers
                .iter()
                .map(|drawer| {
                    drawer
                        .shift
                        .as_ref()
                        .and_then(|shift| shift.expected_amount)
                })
                .collect()
        };
        assert_eq!(
            expected(person(CASHIER, 0xB1), at(BAR)),
            [Some(vnd(500_000)), None],
            "a cashier at the bar reads the bar's drawer and not the counter's"
        );
        assert_eq!(
            expected(person(ASKING, 0xB1), at(BAR)),
            [Some(vnd(500_000)), None],
            "managing another till's drawer only with approval reads none of the others"
        );
        assert_eq!(
            expected(person(SUPERVISOR, 0xB1), at(BAR)),
            [Some(vnd(500_000)), Some(vnd(300_000))],
            "whoever may count any drawer alone reads every one"
        );
        assert_eq!(
            expected(person(CASHIER, 0xD1), TillScope::default()),
            [None, None],
            "at a device that is no till, every drawer is another till's"
        );

        // Counted blind, nobody reads any before the close.
        edge.apply_session(session(DrawerModel::PerTerminal, true));
        assert_eq!(expected(person(SUPERVISOR, 0xB1), at(BAR)), [None, None]);
    });
}

/// The store of [`session`], printing `report` at a close.
fn reporting(model: DrawerModel, enforced: bool, report: CloseReport) -> EdgeSession {
    let mut session = session(model, enforced);
    session.shift.close_report = Open::from_known(report);
    session
}

#[test]
fn a_drawer_closed_on_its_own_prints_its_report_unless_the_store_prints_none() {
    run_ready(async {
        for (report, printed) in [
            (CloseReport::PerDrawer, true),
            (CloseReport::Combined, true),
            (CloseReport::None, false),
        ] {
            let edge = edge_over(
                FakeStore::new(),
                reporting(DrawerModel::PerStore, false, report),
            );
            let shift = edge
                .open_shift(person(MANAGER, 0xB1), vnd(500_000))
                .await
                .expect("opens")
                .shift_id;
            edge.count_shift(person(MANAGER, 0xB1), shift, 500_000)
                .await
                .expect("counts");
            let closed = edge
                .close_shift(person(MANAGER, 0xB1), shift)
                .await
                .expect("closes");
            assert_eq!(closed.print_shift_report, printed, "{report:?}");
            assert!(
                closed.report.is_some(),
                "the close answers with the figures whatever prints: {report:?}"
            );
        }
    });
}

#[test]
fn a_combined_close_report_is_the_sum_of_the_drawers_closed_together() {
    run_ready(async {
        let edge = edge_over(
            FakeStore::new(),
            reporting(DrawerModel::PerTerminal, true, CloseReport::Combined),
        );
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
        let (bill, due) = a_bill(&edge, person(CASHIER, 0xB1), 41).await;
        edge.settle_bill_at(
            person(CASHIER, 0xB1),
            at(BAR),
            bill,
            paid(PaymentMethod::Cash, due),
            None,
        )
        .await
        .expect("cash at the bar");
        for (device, own, shift, movement, amount) in [
            (0xB1, BAR, bar, CashMovement::PaidIn, 100_000),
            (0xC1, COUNTER, counter, CashMovement::PaidOut, 50_000),
        ] {
            edge.record_cash_movement_at(
                person(CASHIER, device),
                at(own),
                shift,
                movement,
                vnd(amount),
                making_change(),
            )
            .await
            .expect("moved at its own till");
        }
        for (device, own, shift, counted) in
            [(0xB1, BAR, bar, 760_000), (0xC1, COUNTER, counter, 255_000)]
        {
            edge.count_shift_at(person(CASHIER, device), at(own), shift, counted, None)
                .await
                .expect("counted");
        }

        let closed = edge
            .close_shifts_at(
                person(SUPERVISOR, 0xB1),
                at(BAR),
                &[bar, counter],
                None,
                &[],
            )
            .await
            .expect("both close at once");
        assert!(
            closed
                .shifts
                .iter()
                .all(|view| view.state == ShiftState::Closed && !view.print_shift_report),
            "the one slip stands for each drawer's report"
        );
        let combined = closed.combined.expect("one slip for both");
        let names: Vec<Option<&str>> = combined
            .drawers
            .iter()
            .map(|(name, _)| name.as_ref().map(DisplayName::as_str))
            .collect();
        assert_eq!(names, [Some("Bar"), Some("Counter")]);
        assert_eq!(
            combined.totals,
            DrawerFigures {
                opening_float: vnd(800_000),
                cash_collected: vnd(165_000),
                paid_in: vnd(100_000),
                paid_out: vnd(50_000),
                expected_amount: vnd(1_015_000),
                counted_amount: vnd(1_015_000),
                variance: vnd(0),
            },
            "each figure is the sum of the two drawers' own: 765k expected at the bar, 250k at \
             the counter, 5k short and 5k over"
        );
    });
}

#[test]
fn a_batch_close_closes_every_drawer_or_none_and_another_tills_takes_the_permission() {
    run_ready(async {
        let store = FakeStore::new();
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
        edge.count_shift_at(person(CASHIER, 0xB1), at(BAR), bar, 500_000, None)
            .await
            .expect("the bar's is counted");

        let uncounted = edge
            .close_shifts_at(
                person(SUPERVISOR, 0xB1),
                at(BAR),
                &[bar, counter],
                None,
                &[],
            )
            .await;
        assert!(
            matches!(uncounted, Err(AppError::Domain(_))),
            "{uncounted:?}"
        );
        let closings = |log: &[EventEnvelope<RawPayload>]| {
            log.iter()
                .filter(|envelope| envelope.event_type.as_str() == "cash.shift.closed")
                .count()
        };
        assert_eq!(closings(&logged(&store).await), 0, "nothing closed");
        assert!(
            edge.current_shift_at(at(BAR)).is_some(),
            "the bar's is open"
        );

        edge.count_shift_at(person(CASHIER, 0xC1), at(COUNTER), counter, 300_000, None)
            .await
            .expect("the counter's is counted");
        let refused = edge
            .close_shifts_at(person(CASHIER, 0xB1), at(BAR), &[bar, counter], None, &[])
            .await;
        assert!(
            matches!(
                refused,
                Err(AppError::Domain(DomainError::PermissionDenied { permission }))
                    if permission == "cash.shift.manage_other_till"
            ),
            "{refused:?}"
        );
        let unapproved = edge
            .close_shifts_at(person(ASKING, 0xB1), at(BAR), &[bar, counter], None, &[])
            .await;
        assert!(
            matches!(unapproved, Err(AppError::ApprovalRequired)),
            "{unapproved:?}"
        );
        let closed = edge
            .close_shifts_at(
                person(ASKING, 0xB1),
                at(BAR),
                &[bar, counter, bar],
                Some(&manager()),
                &[],
            )
            .await
            .expect("one approval covers the batch");
        assert_eq!(closed.shifts.len(), 2, "a shift named twice closes once");
        assert!(
            closed.shifts.iter().all(|view| view.print_shift_report),
            "each drawer prints its own report"
        );
        assert!(closed.combined.is_none());
        let log = logged(&store).await;
        assert_eq!(closings(&log), 2);
        let overrides: Vec<_> = log
            .iter()
            .filter(|envelope| envelope.event_type.as_str() == "security.permission.overridden")
            .map(|envelope| envelope.shift_id)
            .collect();
        assert_eq!(
            overrides,
            [Some(counter)],
            "the approval is recorded on the other till's drawer it covered"
        );
    });
}

/// Twelve hours behind UTC and fourteen ahead: the business day a drawer opens on behind is always
/// earlier than the one the same instant falls on ahead, so moving a store from one to the other
/// moves its day on.
const BEHIND: &str = "Etc/GMT+12";
const AHEAD: &str = "Etc/GMT-14";

/// The store of [`session`], keeping a drawer per till in the time zone `zone`, and handling a
/// drawer left open past its day as `day_end` says.
fn zoned(zone: &str, day_end: DrawerDayEnd, enforced: bool) -> EdgeSession {
    let mut session = session(DrawerModel::PerTerminal, enforced);
    session.timezone = StoreTimeZone::from_iana_name(zone).expect("a real zone");
    session.shift.drawer_day_end = Open::from_known(day_end);
    session
}

#[test]
fn a_drawer_left_open_past_its_day_is_flagged_and_by_default_refused_nothing() {
    run_ready(async {
        let edge = edge_over(FakeStore::new(), zoned(BEHIND, DrawerDayEnd::Flag, true));
        let bar = edge
            .open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None)
            .await
            .expect("the bar's")
            .shift_id;
        let opened_today = edge.current_shift_at(at(BAR)).expect("open");
        assert!(!opened_today.day_ended, "a drawer opened today");

        edge.apply_session(zoned(AHEAD, DrawerDayEnd::Flag, true));
        edge.open_shift_at(person(CASHIER, 0xC1), at(COUNTER), vnd(300_000), None)
            .await
            .expect("the counter's opens today");
        let ended = |own| {
            edge.current_shift_at(at(own))
                .is_some_and(|shift| shift.day_ended)
        };
        assert!(ended(BAR), "the bar's opened on an earlier business day");
        assert!(!ended(COUNTER), "the counter's opened today");
        let listed: Vec<bool> = edge
            .drawers(person(MANAGER, 0xB1), TillScope::default())
            .drawers
            .iter()
            .map(|drawer| drawer.shift.as_ref().is_some_and(|shift| shift.day_ended))
            .collect();
        assert_eq!(listed, [true, false]);

        let (bill, due) = a_bill(&edge, person(CASHIER, 0xB1), 41).await;
        edge.settle_bill_at(
            person(CASHIER, 0xB1),
            at(BAR),
            bill,
            paid(PaymentMethod::Cash, due),
            None,
        )
        .await
        .expect("flagged, it still takes cash");
        edge.record_cash_movement_at(
            person(CASHIER, 0xB1),
            at(BAR),
            bar,
            CashMovement::PaidIn,
            vnd(1_000),
            making_change(),
        )
        .await
        .expect("and a paid in");
    });
}

#[test]
fn a_store_that_requires_it_takes_no_cash_into_a_drawer_past_its_day_until_it_closes() {
    run_ready(async {
        let store = FakeStore::new();
        let bar = edge_over(
            store.clone(),
            zoned(BEHIND, DrawerDayEnd::RequireClose, true),
        )
        .open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None)
        .await
        .expect("the bar's")
        .shift_id;
        // A day on, and a restart: the log says which day the drawer opened on.
        let edge = edge_over(
            store.clone(),
            zoned(AHEAD, DrawerDayEnd::RequireClose, true),
        );
        edge.rebuild().await.expect("replays the log");
        edge.open_shift_at(person(CASHIER, 0xC1), at(COUNTER), vnd(300_000), None)
            .await
            .expect("the counter's opens today");

        let (bill, due) = a_bill(&edge, person(CASHIER, 0xB1), 41).await;
        let cash = edge
            .settle_bill_at(
                person(CASHIER, 0xB1),
                at(BAR),
                bill,
                paid(PaymentMethod::Cash, due),
                None,
            )
            .await;
        assert!(matches!(cash, Err(AppError::DrawerDayEnded)), "{cash:?}");
        for movement in [CashMovement::PaidIn, CashMovement::PaidOut] {
            let moved = edge
                .record_cash_movement_at(
                    person(CASHIER, 0xB1),
                    at(BAR),
                    bar,
                    movement,
                    vnd(1_000),
                    making_change(),
                )
                .await;
            assert!(matches!(moved, Err(AppError::DrawerDayEnded)), "{moved:?}");
        }
        edge.settle_bill_at(
            person(CASHIER, 0xB1),
            at(BAR),
            bill,
            paid(PaymentMethod::Card, due),
            None,
        )
        .await
        .expect("a card still goes through");
        edge.open_drawer_no_sale_at(person(SUPERVISOR, 0xB1), at(BAR), making_change(), None)
            .await
            .expect("the drawer opens without a sale, to be counted");
        let (bill, due) = a_bill(&edge, person(CASHIER, 0xC1), 42).await;
        edge.settle_bill_at(
            person(CASHIER, 0xC1),
            at(COUNTER),
            bill,
            paid(PaymentMethod::Cash, due),
            None,
        )
        .await
        .expect("a drawer opened today takes cash");

        edge.count_shift_at(person(CASHIER, 0xB1), at(BAR), bar, 500_000, None)
            .await
            .expect("counted");
        let closed = edge
            .close_shift_at(person(CASHIER, 0xB1), at(BAR), bar, None, None)
            .await
            .expect("closing is the way out");
        assert_eq!(
            closed.variance,
            Some(vnd(0)),
            "no cash went in after its day"
        );
    });
}

/// A reason for a drawer's over or short, as the framework's own "Counting difference" is.
fn miscounted() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_102))
}

/// The store of [`session`], keeping a drawer per till, asking a reason of a close over or short by
/// more than 20,000, and listing [`miscounted`] for it.
fn varying(enforced: bool) -> EdgeSession {
    let mut session = session(DrawerModel::PerTerminal, enforced);
    session.shift.variance_reason_minor = Some(20_000);
    let mut codes = session.reason_codes.codes().to_vec();
    codes.push(PublishedReasonCode::new(
        miscounted(),
        ReasonCode::new("COUNT_DIFFERENCE"),
        DisplayName::new("Counting difference"),
        vec![ReasonAction::CashVariance],
    ));
    session.reason_codes = PublishedReasonCodes::from_parts(codes);
    session
}

#[test]
fn drawers_closed_together_close_with_a_reason_for_each_one_owed_or_not_at_all() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), varying(true));
        // The bar's is 50k short, beyond the limit; the counter's is 10k over, within it.
        let mut shifts = Vec::new();
        for (device, own, float, counted) in [
            (0xB1, BAR, 500_000, 450_000),
            (0xC1, COUNTER, 300_000, 310_000),
        ] {
            let shift = edge
                .open_shift_at(person(CASHIER, device), at(own), vnd(float), None)
                .await
                .expect("starts")
                .shift_id;
            edge.count_shift_at(person(CASHIER, device), at(own), shift, counted, None)
                .await
                .expect("counted");
            shifts.push(shift);
        }
        let (bar, counter) = (shifts[0], shifts[1]);
        let supervisor = person(SUPERVISOR, 0xB1);

        for given in [vec![], vec![(counter, miscounted())]] {
            let asked = edge
                .close_shifts_at(supervisor, at(BAR), &shifts, None, &given)
                .await;
            let Err(AppError::VarianceReasonRequired(owing)) = asked else {
                panic!("the bar's is owed a reason: {asked:?}");
            };
            let named: Vec<_> = owing
                .iter()
                .map(|view| (view.shift_id, view.till, view.variance))
                .collect();
            assert_eq!(named, [(bar, Some(till(BAR)), Some(vnd(-50_000)))]);
        }
        let wrong = edge
            .close_shifts_at(
                supervisor,
                at(BAR),
                &shifts,
                None,
                &[(bar, making_change())],
            )
            .await;
        assert!(
            matches!(wrong, Err(AppError::CashReasonNotValid)),
            "{wrong:?}"
        );
        assert!(
            edge.current_shift_at(at(BAR)).is_some()
                && edge.current_shift_at(at(COUNTER)).is_some(),
            "every drawer or none"
        );

        let closed = edge
            .close_shifts_at(supervisor, at(BAR), &shifts, None, &[(bar, miscounted())])
            .await
            .expect("each drawer owed a reason has one");
        assert_eq!(closed.shifts.len(), 2);
        let reasons: Vec<_> = logged(&store)
            .await
            .iter()
            .filter(|envelope| envelope.event_type.as_str() == "cash.shift.closed")
            .map(|envelope| {
                let event: CashShiftClosed = envelope.data.decode().expect("decodes");
                (event.closed_shift_id, event.reason_code_id)
            })
            .collect();
        assert_eq!(
            reasons,
            [(bar, Some(miscounted())), (counter, None)],
            "the counter's is within the limit and records none"
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

/// What each printer was sent, by address, in order: with no receipt printed on a settle, a
/// drawer's kick and a shift's report are all a printer is sent.
#[derive(Debug, Default)]
struct Written(std::sync::Mutex<Vec<(String, Vec<u8>)>>);

impl Written {
    /// The addresses written to, one per job, in order.
    fn at(&self) -> Vec<String> {
        let sent = self.0.lock().expect("the printers");
        sent.iter().map(|(address, _)| address.clone()).collect()
    }

    /// Whether the last job sent to `address` says `needle`.
    fn last_says(&self, address: &str, needle: &str) -> bool {
        let sent = self.0.lock().expect("the printers");
        sent.iter()
            .rev()
            .find(|(at, _)| at == address)
            .is_some_and(|(_, bytes)| {
                bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes())
            })
    }
}

#[derive(Debug)]
struct WrittenAt(Arc<Written>);

#[derive(Debug)]
struct WriteTransport(Arc<Written>, String);

impl Transport for WriteTransport {
    fn write(&self, bytes: &[u8]) -> Result<(), Unreachable> {
        self.0
            .0
            .lock()
            .map_err(|_| Unreachable)?
            .push((self.1.clone(), bytes.to_vec()));
        Ok(())
    }

    fn probe(&self) -> Result<TransportStatus, Unreachable> {
        Ok(TransportStatus::default())
    }
}

impl TransportFactory for WrittenAt {
    fn open(&self, device: &PublishedDevice) -> Result<Box<dyn Transport>, PortError> {
        Ok(Box::new(WriteTransport(
            Arc::clone(&self.0),
            device.address.clone(),
        )))
    }
}

/// The store over the shipped routes, and the devices signed in on it.
struct Tills {
    app: Router,
    edge: Arc<Edge<FakeStore>>,
    written: Arc<Written>,
    /// A device bound to the bar's till, one bound to the counter's, and one bound to none.
    bar: String,
    counter: String,
    stranger: String,
}

/// The store over the shipped routes, keeping a drawer per till: a device bound to the bar's till,
/// one bound to the counter's, and one bound to none, each signed in by the manager. The binding is
/// the print agents' record, as `serve` composes it, or one that cannot be read where `unreadable`.
async fn drawers_app(unreadable: bool) -> Tills {
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
    let written = Arc::new(Written::default());
    let printers =
        Arc::new(Printers::over(Arc::new(WrittenAt(Arc::clone(&written)))).with_agents(dispatch));
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let mut paired = Vec::new();
    for _ in 0..3 {
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
    let (counter_device, counter) = paired.pop().expect("the counter's device");
    let (bar_device, bar) = paired.pop().expect("the bar's device");
    for (terminal, device) in [(BAR, bar_device), (COUNTER, counter_device)] {
        agents
            .claim(till(terminal), device, now.as_milliseconds_since_epoch())
            .await
            .expect("the manager's binding is recorded");
    }
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
    for token in [&bar, &counter, &stranger] {
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
    Tills {
        app,
        edge,
        written,
        bar,
        counter,
        stranger,
    }
}

/// Settles a fresh bill for a pizza on table `table`, over the route from the device holding
/// `token`, in one tender of `method`.
async fn settle_over(
    tills: &Tills,
    token: &str,
    table: u128,
    method: &str,
) -> (StatusCode, Option<String>, Value) {
    let (bill, due) = a_bill(&tills.edge, person(MANAGER, 0xB1), table).await;
    let body = json!({
        "payments": [{ "method": method, "tendered": due, "applied_to_bill": due }],
    });
    send(
        tills.app.clone(),
        token,
        "POST",
        &format!("/api/bills/{bill}/settle"),
        Some(body),
    )
    .await
}

#[tokio::test]
async fn the_drawers_read_lists_each_till_and_a_bound_device_opens_its_own() {
    let tills = drawers_app(false).await;
    let (app, edge, bar, stranger) = (tills.app, tills.edge, tills.bar, tills.stranger);
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

/// The read names the till the device asking is, whose drawer its Shift screen shows first, and
/// tells a device that is no till it is none.
#[tokio::test]
async fn the_drawers_read_names_the_till_the_device_asking_is() {
    let tills = drawers_app(false).await;
    let (app, bar, stranger) = (tills.app, tills.bar, tills.stranger);
    let (_, _, read) = send(app.clone(), &bar, "GET", "/api/shifts", None).await;
    assert_eq!(read["terminal_device_id"], till(BAR).to_string(), "{read}");
    let (_, _, read) = send(app.clone(), &stranger, "GET", "/api/shifts", None).await;
    assert!(
        read.get("terminal_device_id").is_none(),
        "a device that is no till is told so: {read}"
    );
}

/// A drawer whose shift is open says when it opened on the shop's wall clock, which a till's own
/// clock may not keep, so the list of every till's drawer can show it as it is.
#[tokio::test]
async fn an_open_drawer_says_when_it_opened_on_the_shops_wall_clock() {
    let tills = drawers_app(false).await;
    let (app, bar, stranger) = (tills.app, tills.bar, tills.stranger);
    let (_, _, read) = send(app.clone(), &stranger, "GET", "/api/shifts", None).await;
    assert!(
        read["drawers"][0].get("opened_clock").is_none(),
        "a closed drawer opened at no time: {read}"
    );
    let open = json!({ "opening_float": vnd(500_000) });
    let (status, _, _) = send(app.clone(), &bar, "POST", "/api/shifts", Some(open)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, read) = send(app.clone(), &stranger, "GET", "/api/shifts", None).await;
    let clock = read["drawers"][0]["opened_clock"]
        .as_str()
        .unwrap_or_default();
    assert!(
        clock.len() == 5 && clock.chars().nth(2) == Some(':'),
        "the shop's wall clock, HH:MM: {read}"
    );
}

#[tokio::test]
async fn a_binding_that_cannot_be_read_refuses_rather_than_guesses_the_till() {
    let tills = drawers_app(true).await;
    let (status, reason, _) = settle_over(&tills, &tills.bar, 41, "PAYMENT_METHOD_CASH").await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("STORE_UNAVAILABLE")),
        "cash is refused rather than put in a drawer that may be the wrong one"
    );
    let (status, _, card) = settle_over(&tills, &tills.bar, 42, "PAYMENT_METHOD_CARD").await;
    assert_eq!(status, StatusCode::OK, "a card goes into no drawer: {card}");
    let Tills { app, edge, bar, .. } = tills;
    let open = json!({ "opening_float": vnd(500_000) });
    let (status, reason, _) =
        send(app.clone(), &bar, "POST", "/api/shifts", Some(open.clone())).await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("STORE_UNAVAILABLE"))
    );
    let (status, reason, _) = send(app.clone(), &bar, "GET", "/api/shifts", None).await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("STORE_UNAVAILABLE")),
        "which drawer is the device's own decides what the list shows"
    );

    // Where the store keeps one drawer it is every device's, and nothing asks which till this is.
    edge.apply_session(session(DrawerModel::PerStore, false));
    let (status, _, opened) = send(app, &bar, "POST", "/api/shifts", Some(open)).await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert!(opened.get("terminal_device_id").is_none(), "{opened}");
}

#[tokio::test]
async fn cash_springs_only_the_drawer_of_the_till_it_is_taken_at() {
    let tills = drawers_app(false).await;
    let mut shifts = Vec::new();
    for (token, own) in [(&tills.bar, BAR), (&tills.counter, COUNTER)] {
        let open = json!({ "opening_float": vnd(500_000) });
        let (status, _, opened) =
            send(tills.app.clone(), token, "POST", "/api/shifts", Some(open)).await;
        assert_eq!(status, StatusCode::OK, "{opened}");
        assert_eq!(opened["terminal_device_id"], till(own).to_string());
        shifts.push(opened["shift_id"].as_str().unwrap_or_default().to_owned());
    }

    let (status, _, settled) = settle_over(&tills, &tills.bar, 41, "PAYMENT_METHOD_CASH").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["drawer_open"], "OPENED");
    assert_eq!(
        tills.written.at(),
        [BAR_DRAWER],
        "the bar's drawer and no other"
    );

    // The counter's till names a printer with no drawer: none springs, the store's included.
    let (status, _, settled) = settle_over(&tills, &tills.counter, 42, "PAYMENT_METHOD_CASH").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["drawer_open"], "NO_DRAWER");
    assert_eq!(tills.written.at(), [BAR_DRAWER]);

    // A device that is no till takes a card, and no cash.
    let (status, reason, _) = settle_over(&tills, &tills.stranger, 43, "PAYMENT_METHOD_CASH").await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("NOT_A_TILL"))
    );
    let (status, _, settled) =
        settle_over(&tills, &tills.stranger, 44, "PAYMENT_METHOD_CARD").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert!(settled.get("drawer_open").is_none(), "{settled}");

    // A paid in springs the drawer it goes into, and is made at that drawer's own till.
    let movement = json!({ "amount_minor": 100_000, "reason_code_id": making_change() });
    let paid_in = format!("/api/shifts/{}/paid-in", shifts[0]);
    let (status, _, moved) = send(
        tills.app.clone(),
        &tills.bar,
        "POST",
        &paid_in,
        Some(movement.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["drawer_open"], "OPENED");
    for (token, refusal) in [
        (&tills.counter, "SHIFT_NOT_OPEN"),
        (&tills.stranger, "NOT_A_TILL"),
    ] {
        let (status, reason, _) = send(
            tills.app.clone(),
            token,
            "POST",
            &paid_in,
            Some(movement.clone()),
        )
        .await;
        assert_eq!(
            (status, reason.as_deref()),
            (StatusCode::CONFLICT, Some(refusal))
        );
    }

    // A no-sale opening opens the drawer of the till it is made at.
    let no_sale = json!({
        "reason_code_id": making_change(),
        "approver_code": MANAGER_CODE,
        "approver_pin": MANAGER_PIN,
    });
    let (status, _, opened) = send(
        tills.app.clone(),
        &tills.counter,
        "POST",
        "/api/drawer/open",
        Some(no_sale.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert_eq!(opened["drawer_open"], "NO_DRAWER");
    let (status, reason, _) = send(
        tills.app.clone(),
        &tills.stranger,
        "POST",
        "/api/drawer/open",
        Some(no_sale),
    )
    .await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("NOT_A_TILL"))
    );
    assert_eq!(
        tills.written.at(),
        [BAR_DRAWER, BAR_DRAWER],
        "the store's drawer never sprang for a till's cash"
    );
}

/// Opens a drawer over the route from the device holding `token`, its own till's, answering its
/// shift.
async fn open_over(tills: &Tills, token: &str) -> String {
    let open = json!({ "opening_float": vnd(500_000) });
    let (status, _, opened) =
        send(tills.app.clone(), token, "POST", "/api/shifts", Some(open)).await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    opened["shift_id"].as_str().unwrap_or_default().to_owned()
}

/// Counts `shift` at 500,000 over the route from the device holding `token`.
async fn count_over(tills: &Tills, token: &str, shift: &str) {
    let count = json!({ "counted_minor": 500_000 });
    let uri = format!("/api/shifts/{shift}/count");
    let (status, _, counted) = send(tills.app.clone(), token, "POST", &uri, Some(count)).await;
    assert_eq!(status, StatusCode::OK, "{counted}");
}

/// The manager's code and PIN, which approve their own act where the store does not enforce.
fn approver() -> Value {
    json!({ "approver_code": MANAGER_CODE, "approver_pin": MANAGER_PIN })
}

#[tokio::test]
async fn a_drawers_report_prints_at_the_till_it_is_closed_at_or_not_at_all() {
    let tills = drawers_app(false).await;
    let bar = open_over(&tills, &tills.bar).await;
    let counter = open_over(&tills, &tills.counter).await;
    count_over(&tills, &tills.bar, &bar).await;
    count_over(&tills, &tills.counter, &counter).await;

    let (status, _, closed) = send(
        tills.app.clone(),
        &tills.bar,
        "POST",
        &format!("/api/shifts/{bar}/close"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(closed["shift_report_print"], "PRINTED");
    assert_eq!(tills.written.at(), [BAR_DRAWER], "at the bar's own printer");
    assert!(tills.written.last_says(BAR_DRAWER, "SHIFT REPORT"));

    // Closed from a device that is no till, the report prints at the store's receipt printer.
    let (status, _, closed) = send(
        tills.app.clone(),
        &tills.stranger,
        "POST",
        &format!("/api/shifts/{counter}/close"),
        Some(approver()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(tills.written.at(), [BAR_DRAWER, STORE_DRAWER]);

    // A store that prints no report at a close prints none.
    tills.edge.apply_session(reporting(
        DrawerModel::PerTerminal,
        false,
        CloseReport::None,
    ));
    let bar = open_over(&tills, &tills.bar).await;
    count_over(&tills, &tills.bar, &bar).await;
    let (status, _, closed) = send(
        tills.app.clone(),
        &tills.bar,
        "POST",
        &format!("/api/shifts/{bar}/close"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert!(closed.get("shift_report_print").is_none(), "{closed}");
    assert_eq!(
        closed["variance"],
        json!(vnd(0)),
        "the figures still answer"
    );
    assert_eq!(tills.written.at(), [BAR_DRAWER, STORE_DRAWER]);
}

#[tokio::test]
async fn drawers_closed_together_print_one_slip_where_they_are_closed() {
    let tills = drawers_app(false).await;
    tills.edge.apply_session(reporting(
        DrawerModel::PerTerminal,
        false,
        CloseReport::Combined,
    ));
    let bar = open_over(&tills, &tills.bar).await;
    let counter = open_over(&tills, &tills.counter).await;
    count_over(&tills, &tills.bar, &bar).await;

    let mut batch = approver();
    batch["shift_ids"] = json!([counter, bar]);
    let (status, reason, _) = send(
        tills.app.clone(),
        &tills.counter,
        "POST",
        "/api/shifts:batch_close",
        Some(batch.clone()),
    )
    .await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("TRANSITION_REFUSED")),
        "the counter's drawer is not counted, so neither closes"
    );
    count_over(&tills, &tills.counter, &counter).await;

    let (status, _, closed) = send(
        tills.app.clone(),
        &tills.counter,
        "POST",
        "/api/shifts:batch_close",
        Some(batch),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(closed["shift_report_print"], "PRINTED");
    assert_eq!(closed["shifts"][0]["shift_id"], counter.as_str());
    assert_eq!(closed["shifts"][1]["state"], "SHIFT_STATE_CLOSED");
    assert!(closed["shifts"][1].get("shift_report_print").is_none());
    assert_eq!(
        tills.written.at(),
        [COUNTER_PAPER],
        "one slip, at the counter's own printer"
    );
    for words in ["STORE TOTALS", "Bar", "Counter"] {
        assert!(tills.written.last_says(COUNTER_PAPER, words), "{words}");
    }
}

#[tokio::test]
async fn the_shift_reads_say_a_drawers_day_has_ended_and_its_cash_is_refused_by_name() {
    let tills = drawers_app(false).await;
    tills
        .edge
        .apply_session(zoned(BEHIND, DrawerDayEnd::RequireClose, false));
    let bar = open_over(&tills, &tills.bar).await;
    let (_, _, current) = send(
        tills.app.clone(),
        &tills.bar,
        "GET",
        "/api/shifts/current",
        None,
    )
    .await;
    assert!(current.get("day_ended").is_none(), "{current}");

    tills
        .edge
        .apply_session(zoned(AHEAD, DrawerDayEnd::RequireClose, false));
    let (_, _, current) = send(
        tills.app.clone(),
        &tills.bar,
        "GET",
        "/api/shifts/current",
        None,
    )
    .await;
    assert_eq!(current["shift_id"], bar.as_str());
    assert_eq!(current["day_ended"], true);
    let (_, _, listed) = send(tills.app.clone(), &tills.bar, "GET", "/api/shifts", None).await;
    assert_eq!(listed["drawers"][0]["shift"]["day_ended"], true, "{listed}");
    let (status, reason, _) = settle_over(&tills, &tills.bar, 41, "PAYMENT_METHOD_CASH").await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("DRAWER_DAY_ENDED"))
    );
}

/// `body` posted to `uri` from the device holding `token`.
async fn post(
    tills: &Tills,
    token: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Option<String>, Value) {
    send(tills.app.clone(), token, "POST", uri, Some(body)).await
}

/// Counts `shift` at `counted` over the route from the device holding `token`.
async fn count_at(tills: &Tills, token: &str, shift: &str, counted: i64) {
    let uri = format!("/api/shifts/{shift}/count");
    let (status, _, answered) = post(tills, token, &uri, json!({ "counted_minor": counted })).await;
    assert_eq!(status, StatusCode::OK, "{answered}");
}

#[tokio::test]
async fn a_close_owed_a_reason_answers_the_figures_to_show_and_closes_when_given_one() {
    let tills = drawers_app(false).await;
    tills.edge.apply_session(varying(false));
    let owed = (StatusCode::CONFLICT, Some("VARIANCE_REASON_REQUIRED"));

    // The bar's drawer, counted 50k short of the 500k float it opened on.
    let bar = open_over(&tills, &tills.bar).await;
    count_at(&tills, &tills.bar, &bar, 450_000).await;
    let close = format!("/api/shifts/{bar}/close");
    let (status, reason, asked) = post(&tills, &tills.bar, &close, json!({})).await;
    assert_eq!((status, reason.as_deref()), owed);
    assert!(asked["message"].is_string(), "{asked}");
    let owing = &asked["shifts"][0];
    assert_eq!(owing["shift_id"], bar.as_str());
    assert_eq!(owing["terminal_device_id"], till(BAR).to_string());
    assert_eq!(owing["state"], "SHIFT_STATE_COUNTED");
    assert_eq!(owing["expected_amount"], json!(vnd(500_000)));
    assert_eq!(owing["counted_amount"], json!(vnd(450_000)));
    assert_eq!(owing["variance"], json!(vnd(-50_000)));
    let wrong = json!({ "reason_code_id": making_change() });
    let (status, reason, _) = post(&tills, &tills.bar, &close, wrong).await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("CASH_REASON_NOT_VALID"))
    );
    let given = json!({ "reason_code_id": miscounted() });
    let (status, _, closed) = post(&tills, &tills.bar, &close, given).await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(closed["variance"], json!(vnd(-50_000)));

    // Both drawers 30k short, closed together from the counter: the refusal names each, and one
    // reason apiece closes them.
    let bar = open_over(&tills, &tills.bar).await;
    let counter = open_over(&tills, &tills.counter).await;
    count_at(&tills, &tills.bar, &bar, 470_000).await;
    count_at(&tills, &tills.counter, &counter, 470_000).await;
    let mut batch = approver();
    batch["shift_ids"] = json!([bar, counter]);
    let uri = "/api/shifts:batch_close";
    let (status, reason, asked) = post(&tills, &tills.counter, uri, batch.clone()).await;
    assert_eq!((status, reason.as_deref()), owed);
    let named: Vec<&Value> = asked["shifts"]
        .as_array()
        .map(|shifts| shifts.iter().map(|shift| &shift["shift_id"]).collect())
        .unwrap_or_default();
    assert_eq!(named, [&json!(bar), &json!(counter)], "{asked}");
    batch["variance_reasons"] = json!([
        { "shift_id": bar, "reason_code_id": miscounted() },
        { "shift_id": counter, "reason_code_id": miscounted() },
    ]);
    let (status, _, closed) = post(&tills, &tills.counter, uri, batch).await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(closed["shifts"][1]["state"], "SHIFT_STATE_CLOSED");
}
