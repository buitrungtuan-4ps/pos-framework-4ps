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
