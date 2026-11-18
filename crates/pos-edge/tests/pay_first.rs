// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A counter that takes the money first
//! ([ADR-0222](../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md)
//! decision 6): settling a tableless order on a pay-first store sends its unsent lines to the
//! kitchen with the payment, and nowhere else does a settle fire anything.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::capability::{Capability, CapabilityContext};
use pos_core::decision::Actor;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, OrderLineChoice, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::floor::{KitchenStation, RoutingRule, StationPlan};
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StationId, StoreId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{Open, PaymentMethod, SalesChannel};

fn cashier() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn dish() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

/// A counter store at 10 % selling one 50,000 dish that a kitchen station makes, with `capabilities`.
fn session(capabilities: CapabilityContext) -> EdgeSession {
    let class = EdgeSession::standard_tax_class();
    let station = StationId::new(Ulid::from_u128(700));
    let mut session = EdgeSession::bootstrap()
        .with_menu(MenuCatalog::new().with(MenuEntry::new(
            dish(),
            DisplayName::new("Fake bun"),
            vnd(50_000),
            class,
        )))
        .with_tax_rates(TaxRateTable::new().with(
            class,
            SalesChannel::Takeaway,
            TaxRate::from_percent(10),
        ))
        .with_stations(
            StationPlan::new()
                .with_station(KitchenStation {
                    station_id: station,
                    name: DisplayName::new("Grill"),
                    backup_station_id: None,
                    late_after_seconds: None,
                    ticket_language: Open::default(),
                })
                .with_rule(RoutingRule {
                    station_id: station,
                    menu_item_id: Some(dish()),
                    course_id: None,
                }),
        );
    session.capabilities = capabilities;
    session
}

/// Sells two of the dish on a takeaway counter order and settles it in cash; the event types the
/// store then holds, in order.
async fn sell_and_settle(capabilities: CapabilityContext) -> Vec<String> {
    let store = FakeStore::default();
    let edge = Edge::new(
        store.clone(),
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session(capabilities),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator");
    let order = edge
        .open_counter_order(cashier(), SalesChannel::Takeaway)
        .await
        .expect("opens")
        .order_id;
    edge.add_line_to_order(
        cashier(),
        order,
        OrderLineChoice {
            menu_item_id: dish(),
            quantity: Quantity::from_milli(2_000),
            modifier_menu_item_ids: Vec::new(),
            seat: None,
            course_id: None,
            note_present: false,
            typed_unit_price: None,
        },
    )
    .await
    .expect("adds");
    let bill = edge
        .open_bill_for_order(cashier(), order)
        .await
        .expect("bills")
        .bill_id;
    edge.settle_bill(
        cashier(),
        bill,
        vec![Payment {
            method: PaymentMethod::Cash,
            tendered: vnd(110_000),
            applied_to_bill: vnd(110_000),
            tip: vnd(0),
        }],
        None,
    )
    .await
    .expect("settles");
    let query = EventQuery::first(
        StoreId::new(Ulid::from_u128(1)),
        NonZeroU32::new(100).expect("a positive limit"),
    );
    store
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .map(|envelope| envelope.event_type.as_str().to_owned())
        .collect()
}

#[test]
fn a_pay_first_counter_sends_its_unsent_lines_with_the_payment() {
    run_ready(async {
        let events = sell_and_settle(CapabilityContext::fast_food()).await;
        let tail: Vec<&str> = events[events.len() - 3..]
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(
            tail,
            [
                "sales.order_line.fired",
                "billing.payment.captured",
                "billing.bill.settled"
            ]
        );
    });
}

#[test]
fn a_store_that_does_not_pay_first_fires_nothing_at_settle() {
    run_ready(async {
        let events = sell_and_settle(
            CapabilityContext::NONE
                .with(Capability::QueueNumber)
                .with(Capability::Kds),
        )
        .await;
        assert!(
            !events.contains(&"sales.order_line.fired".to_owned()),
            "{events:?}"
        );
    });
}
