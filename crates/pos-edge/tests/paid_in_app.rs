// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An order paid for in a delivery vendor's app is settled "Paid via <vendor>" when the store takes
//! it ([ADR-0248](../../../docs/adr/0248-a-delivery-order-is-priced-paid-and-reconciled-as-the-app-sold-it.md)
//! decision 2), and one paid at the store is left for the till. Fake data only.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_edge::{
    Edge, EdgeOrderIn, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::order_in::{ExternalReference, InboundOrder, InboundOrderLine, OrderIn};
use pos_proto::events::{BillingBillSettled, BillingPaymentCaptured};
use pos_proto::ids::{DeviceId, MenuItemId, OrderId, StoreId, VendorId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::vendors::{PublishedVendor, PublishedVendors};
use pos_proto::wire_enum::Open;
use pos_proto::{PaymentMethod, SalesChannel};

fn store() -> StoreId {
    StoreId::new(Ulid::from_u128(7))
}

fn vendor_id() -> VendorId {
    VendorId::new(Ulid::from_u128(0x0EE1))
}

/// An edge selling a 120,000 Margherita at 10% tax, taking orders from one vendor paid as `payment`
/// says, and the log it writes to.
fn edge_paid(payment: &str) -> (Arc<Edge<FakeStore>>, FakeStore) {
    let class = EdgeSession::standard_tax_class();
    let menu = MenuCatalog::new().with(MenuEntry::new(
        MenuItemId::new(Ulid::from_u128(500)),
        DisplayName::new("Margherita"),
        Money::new(CurrencyCode::VND, 120_000),
        class,
    ));
    let rates = TaxRateTable::new().with(class, SalesChannel::Delivery, TaxRate::from_percent(10));
    let mut session = EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates);
    session.vendors = PublishedVendors::new(vec![PublishedVendor {
        vendor_id: vendor_id(),
        display_name: DisplayName::new("Sandbox delivery"),
        staff_confirm: true,
        prep_minutes: 20,
        kitchen_release: Open::default(),
        price_source: Open::default(),
        price_difference_bps: 0,
        commission_bps: 0,
        payment: Open::parse(payment),
    }]);
    let log = FakeStore::default();
    let edge = Edge::new(
        log.clone(),
        StoreIdentity::for_store(store()),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator");
    (Arc::new(edge), log)
}

/// Takes one of the vendor's orders and returns its id.
fn take_order(edge: &Arc<Edge<FakeStore>>) -> OrderId {
    let intake = EdgeOrderIn::new(
        Arc::clone(edge),
        InMemoryQueueNumbers::new(),
        DeviceId::new(Ulid::from_u128(20)),
    );
    let order = InboundOrder {
        external_reference: ExternalReference::parse("01J0000000000000000000CN01:A1")
            .expect("a reference"),
        sales_channel: Open::from_known(SalesChannel::Delivery),
        store_id: store(),
        table_id: None,
        subject_id: None,
        vendor_id: Some(vendor_id()),
        lines: vec![InboundOrderLine {
            menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
            quantity: Quantity::ONE,
            modifier_menu_item_ids: Vec::new(),
            quoted_unit_price: None,
            note: None,
        }],
        placed_at: pos_contract_tests::fixtures::instant(),
    };
    run_ready(intake.submit(&order)).expect("accepted").order_id
}

/// The payments and settles in `log`: each payment's method and amount, and each settle's total.
fn money_in(log: &FakeStore) -> (Vec<(PaymentMethod, i64)>, Vec<i64>) {
    let query = EventQuery::first(store(), NonZeroU32::new(100).expect("a positive limit"));
    let events = run_ready(log.read(&query)).expect("read the log");
    let payments = events
        .iter()
        .filter(|envelope| envelope.event_type.as_str() == "billing.payment.captured")
        .map(|envelope| {
            let captured: BillingPaymentCaptured = envelope.data.decode().expect("decodes");
            (
                captured.method.known(),
                captured.applied_to_bill.amount_minor,
            )
        })
        .collect();
    let settles = events
        .iter()
        .filter(|envelope| envelope.event_type.as_str() == "billing.bill.settled")
        .map(|envelope| {
            let settled: BillingBillSettled = envelope.data.decode().expect("decodes");
            settled.total_due.amount_minor
        })
        .collect();
    (payments, settles)
}

#[test]
fn an_order_paid_in_the_app_is_settled_via_its_vendor_when_the_store_takes_it() {
    let (edge, log) = edge_paid("");
    let order_id = take_order(&edge);
    assert_eq!(
        money_in(&log),
        (vec![], vec![]),
        "nothing settles before it is taken"
    );

    run_ready(pos_edge::delivery_slips::confirmed(
        None, None, &edge, order_id,
    ));
    assert_eq!(
        money_in(&log),
        (vec![(PaymentMethod::Vendor, 132_000)], vec![132_000]),
        "the whole bill, tax included, paid via the vendor"
    );
    // Taking it again settles nothing more.
    assert_eq!(
        run_ready(edge.settle_paid_in_app(order_id)).expect("again"),
        None
    );
    assert_eq!(money_in(&log).0.len(), 1);
}

#[test]
fn an_order_paid_at_the_store_is_left_for_the_till() {
    let (edge, log) = edge_paid("VENDOR_PAYMENT_AT_STORE");
    let order_id = take_order(&edge);
    run_ready(pos_edge::delivery_slips::confirmed(
        None, None, &edge, order_id,
    ));
    assert_eq!(money_in(&log), (vec![], vec![]));
}

/// ADR-0259 decisions 4 and 5: on the till's X the app's order is its own line, the app named and
/// delivered, and its in-app payment is named by the app instead of "other"; a rebuild from the log
/// reads the same.
#[test]
fn the_x_names_the_apps_order_and_its_in_app_payment() {
    use pos_core::decision::Actor;
    use pos_proto::ids::EmployeeId;
    use pos_proto::sale_channel::{OrderType, SaleSource, SaleToken};

    let actor = Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    };
    let (edge, log) = edge_paid("");
    let shift = run_ready(edge.open_shift(actor, Money::new(CurrencyCode::VND, 0)))
        .expect("opens a shift")
        .shift_id;
    let order_id = take_order(&edge);
    run_ready(pos_edge::delivery_slips::confirmed(
        None, None, &edge, order_id,
    ));
    let read = |edge: &Edge<FakeStore>| {
        let figures = edge.shift_x(actor, shift).expect("reads").figures;
        let lines: Vec<_> = figures
            .channels
            .iter()
            .map(|line| {
                (
                    line.channel.source.clone(),
                    line.channel.vendor_id,
                    line.channel.order_type.clone(),
                    line.name.as_ref().map(|name| name.as_str().to_owned()),
                    line.bills,
                    line.sales.amount_minor,
                )
            })
            .collect();
        let apps: Vec<_> = figures
            .tenders
            .in_app
            .iter()
            .map(|app| (app.vendor_id, app.amount.amount_minor))
            .collect();
        (lines, apps, figures.tenders.other.amount_minor)
    };
    let expected = (
        vec![(
            SaleToken::of(SaleSource::App),
            Some(vendor_id()),
            SaleToken::of(OrderType::Delivery),
            Some("Sandbox delivery".to_owned()),
            1,
            132_000,
        )],
        vec![(vendor_id(), 132_000)],
        132_000,
    );
    assert_eq!(read(&edge), expected);
    let rebuilt = Edge::new(
        log,
        StoreIdentity::for_store(store()),
        edge.session().as_ref().clone(),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator");
    run_ready(rebuilt.rebuild()).expect("rebuilds");
    assert_eq!(read(&rebuilt), expected, "the same after a rebuild");
}

/// ADR-0260 decision 4: an app's order takes the type the store lists for apps of its base kind,
/// and the X names it as the store does, live and after a rebuild. Fake configuration only.
#[test]
fn an_apps_order_takes_the_type_the_store_maps_apps_to() {
    use pos_core::decision::Actor;
    use pos_proto::ids::EmployeeId;

    let actor = Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    };
    let (edge, log) = edge_paid("");
    let mut session = edge.session().as_ref().clone();
    session.order_channels = serde_json::from_value(serde_json::json!({
        "order_types": [
            { "order_type": "ORDER_TYPE_DELIVERY", "base_order_type": "ORDER_TYPE_DELIVERY" },
            { "order_type": "ORDER_TYPE_APP_DELIVERY", "base_order_type": "ORDER_TYPE_DELIVERY",
              "names": { "en": "App delivery", "vi": "Giao qua ứng dụng" },
              "applies_to_sources": ["SALE_SOURCE_APP"] }
        ]
    }))
    .expect("the node reads");
    edge.apply_session(session);
    let shift = run_ready(edge.open_shift(actor, Money::new(CurrencyCode::VND, 0)))
        .expect("opens a shift")
        .shift_id;
    let order_id = take_order(&edge);
    run_ready(pos_edge::delivery_slips::confirmed(
        None, None, &edge, order_id,
    ));
    let read = |edge: &Edge<FakeStore>| {
        let figures = edge.shift_x(actor, shift).expect("reads").figures;
        figures
            .channels
            .iter()
            .map(|line| {
                (
                    line.channel.source.as_str().to_owned(),
                    line.channel.order_type.as_str().to_owned(),
                    line.order_type_names
                        .get("vi")
                        .map(|name| name.as_str().to_owned()),
                    line.bills,
                )
            })
            .collect::<Vec<_>>()
    };
    let expected = vec![(
        "SALE_SOURCE_APP".to_owned(),
        "ORDER_TYPE_APP_DELIVERY".to_owned(),
        Some("Giao qua ứng dụng".to_owned()),
        1,
    )];
    assert_eq!(read(&edge), expected);
    let rebuilt = Edge::new(
        log,
        StoreIdentity::for_store(store()),
        edge.session().as_ref().clone(),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator");
    run_ready(rebuilt.rebuild()).expect("rebuilds");
    assert_eq!(read(&rebuilt), expected, "the same after a rebuild");
}
