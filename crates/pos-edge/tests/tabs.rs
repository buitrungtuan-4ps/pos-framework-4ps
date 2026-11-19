// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A bar tab
//! ([ADR-0221](../../../docs/adr/0221-a-bar-tab-is-an-order-without-a-table-that-stays-open.md)):
//! a store that keeps tabs opens one with the day's next number, every till lists the open ones by
//! number, a settled tab leaves the list, and a restart finds the same tabs; a store without tabs
//! is refused. A tab nothing was sold on closes with a reason the store lists for closing.

use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::capability::{Capability, CapabilityContext};
use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_edge::app::AppError;
use pos_edge::queue::InMemoryQueueNumbers;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, OrderLineChoice, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, OrderId, StoreId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::ReasonAction;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{PaymentMethod, SalesChannel};

fn bartender() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn beer() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

/// A store at 10 % selling a 40,000 beer, with `capabilities`.
fn edge_over(store: FakeStore, capabilities: CapabilityContext) -> Edge<FakeStore> {
    let class = EdgeSession::standard_tax_class();
    let mut session = EdgeSession::bootstrap()
        .with_menu(MenuCatalog::new().with(MenuEntry::new(
            beer(),
            DisplayName::new("Fake lager"),
            vnd(40_000),
            class,
        )))
        .with_tax_rates(TaxRateTable::new().with(
            class,
            SalesChannel::Takeaway,
            TaxRate::from_percent(10),
        ));
    session.capabilities = capabilities;
    Edge::new(
        store,
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

fn bar() -> CapabilityContext {
    CapabilityContext::NONE.with(Capability::Tabs)
}

async fn a_round(edge: &Edge<FakeStore>, order_id: OrderId) {
    edge.add_line_to_order(
        bartender(),
        order_id,
        OrderLineChoice {
            menu_item_id: beer(),
            quantity: Quantity::from_milli(2_000),
            modifier_menu_item_ids: Vec::new(),
            seat: None,
            course_id: None,
            note_present: false,
            typed_unit_price: None,
            weight_source: None,
        },
    )
    .await
    .expect("adds a round");
}

#[test]
fn a_store_without_tabs_opens_none() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), CapabilityContext::NONE);
        let refused = edge
            .open_tab(
                bartender(),
                SalesChannel::Takeaway,
                &InMemoryQueueNumbers::new(),
            )
            .await;
        assert!(
            matches!(
                refused,
                Err(AppError::Domain(DomainError::CapabilityDisabled {
                    capability: "tabs_enabled"
                }))
            ),
            "{refused:?}"
        );
        assert!(edge.open_tabs().expect("lists").is_empty());
    });
}

#[test]
fn tabs_are_listed_by_number_until_settled_and_a_restart_finds_them() {
    run_ready(async {
        let store = FakeStore::default();
        let numbers = InMemoryQueueNumbers::new();
        let edge = edge_over(store.clone(), bar());
        let first = edge
            .open_tab(bartender(), SalesChannel::Takeaway, &numbers)
            .await
            .expect("opens a tab")
            .order_id;
        let second = edge
            .open_tab(bartender(), SalesChannel::Takeaway, &numbers)
            .await
            .expect("opens another")
            .order_id;
        a_round(&edge, first).await;
        a_round(&edge, second).await;

        let listed = edge.open_tabs().expect("lists");
        let numbered: Vec<(OrderId, u64)> = listed
            .iter()
            .map(|tab| (tab.order_id, tab.tab_number))
            .collect();
        assert_eq!(numbered, [(first, 1), (second, 2)]);
        assert_eq!(listed[0].total_due, vnd(88_000), "two at 40,000 and 10 %");

        let bill = edge
            .open_bill_for_order(bartender(), first)
            .await
            .expect("bills")
            .bill_id;
        edge.settle_bill(
            bartender(),
            bill,
            vec![Payment {
                method: PaymentMethod::Cash,
                tendered: vnd(88_000),
                applied_to_bill: vnd(88_000),
                tip: vnd(0),
            }],
            None,
        )
        .await
        .expect("settles");
        let still_open = |edge: &Edge<FakeStore>| -> Vec<(OrderId, u64)> {
            edge.open_tabs()
                .expect("lists")
                .iter()
                .map(|tab| (tab.order_id, tab.tab_number))
                .collect()
        };
        assert_eq!(still_open(&edge), [(second, 2)], "a settled tab leaves");

        let restarted = edge_over(store, bar());
        restarted.rebuild().await.expect("rebuilds from the log");
        assert_eq!(
            still_open(&restarted),
            [(second, 2)],
            "the number rides the event"
        );
    });
}

#[test]
fn an_empty_tab_closes_with_a_listed_reason_and_one_with_a_round_does_not() {
    run_ready(async {
        let store = FakeStore::default();
        let numbers = InMemoryQueueNumbers::new();
        let edge = edge_over(store.clone(), bar());
        let empty = edge
            .open_tab(bartender(), SalesChannel::Takeaway, &numbers)
            .await
            .expect("opens a tab")
            .order_id;
        let drinking = edge
            .open_tab(bartender(), SalesChannel::Takeaway, &numbers)
            .await
            .expect("opens another")
            .order_id;
        a_round(&edge, drinking).await;
        let reason = edge
            .session()
            .reason_codes
            .for_action(ReasonAction::CloseOrder)
            .next()
            .expect("the framework's list gives a reason for closing")
            .id;
        let void_only = edge
            .session()
            .reason_codes
            .codes()
            .iter()
            .find(|code| !code.is_valid_for(ReasonAction::CloseOrder))
            .expect("a reason for something else")
            .id;

        let unreasoned = edge.close_empty_order(bartender(), empty, None).await;
        assert!(
            matches!(unreasoned, Err(AppError::CloseReasonNotValid)),
            "{unreasoned:?}"
        );
        let wrong = edge
            .close_empty_order(bartender(), empty, Some(void_only))
            .await;
        assert!(
            matches!(wrong, Err(AppError::CloseReasonNotValid)),
            "{wrong:?}"
        );
        let sold = edge
            .close_empty_order(bartender(), drinking, Some(reason))
            .await;
        assert!(matches!(sold, Err(AppError::OrderNotEmpty)), "{sold:?}");

        edge.close_empty_order(bartender(), empty, Some(reason))
            .await
            .expect("closes the empty tab");
        let numbers_open = |edge: &Edge<FakeStore>| -> Vec<u64> {
            edge.open_tabs()
                .expect("lists")
                .iter()
                .map(|tab| tab.tab_number)
                .collect()
        };
        assert_eq!(numbers_open(&edge), [2], "the closed tab leaves the list");
        let again = edge
            .close_empty_order(bartender(), empty, Some(reason))
            .await;
        assert!(matches!(again, Err(AppError::UnknownOrder)), "{again:?}");

        let restarted = edge_over(store, bar());
        restarted.rebuild().await.expect("rebuilds from the log");
        assert_eq!(numbers_open(&restarted), [2], "the close is folded back");
    });
}
