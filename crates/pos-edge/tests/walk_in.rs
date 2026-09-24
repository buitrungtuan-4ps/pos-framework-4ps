// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A counter store starts its own orders
//! ([ADR-0146](../../../docs/adr/0146-a-counter-store-starts-its-own-orders.md), finding F12).
//!
//! The properties the record promises: an order opens with no table on a channel the store
//! accepts; a line added by order is priced by the edge **at that order's channel**, not at the
//! menu the till copied; nothing joins an order whose bill is open; and the whole of it comes back
//! after a restart.

use std::collections::BTreeSet;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_edge::{AppError, Edge, EdgeSession, InMemoryReceipts, OrderLineChoice, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::SalesChannel;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StoreId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

fn actor() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn item() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

/// Dine-in at 10%, takeaway at 8%: different enough that a line priced at the wrong channel shows.
fn session() -> EdgeSession {
    let class = EdgeSession::standard_tax_class();
    let menu = MenuCatalog::new().with(MenuEntry::new(
        item(),
        DisplayName::new("Bánh mì"),
        vnd(50_000),
        class,
    ));
    let rates = TaxRateTable::new()
        .with(class, SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(class, SalesChannel::Takeaway, TaxRate::from_percent(8));
    EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates)
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

fn one_of(menu_item_id: MenuItemId) -> OrderLineChoice {
    OrderLineChoice {
        menu_item_id,
        quantity: Quantity::ONE,
        modifier_menu_item_ids: Vec::new(),
        seat: None,
        course_id: None,
        note_present: false,
    }
}

/// A takeaway walk-in is priced and taxed as takeaway: 50,000 at 8% owes 54,000, not the 55,000 a
/// line copied from the dine-in menu would have carried.
#[test]
fn a_walk_in_line_is_priced_at_the_order_s_own_channel() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session());
        let opened = edge
            .open_counter_order(actor(), SalesChannel::Takeaway)
            .await
            .expect("the counter opens an order");
        edge.add_line_to_order(actor(), opened.order_id, one_of(item()))
            .await
            .expect("a line joins it");

        let totals = edge.order_totals(opened.order_id).expect("totals");
        assert_eq!(totals.total_due, vnd(54_000));
        let live = edge.live_orders();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].table_id, None, "a walk-in sits on no table");
    });
}

/// Nothing joins an order whose bill is open: that bill would not cover it.
#[test]
fn a_line_cannot_join_an_order_whose_bill_is_open() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session());
        let opened = edge
            .open_counter_order(actor(), SalesChannel::Takeaway)
            .await
            .expect("opens");
        edge.add_line_to_order(actor(), opened.order_id, one_of(item()))
            .await
            .expect("adds");
        edge.open_bill_for_order(actor(), opened.order_id)
            .await
            .expect("bills");

        let late = edge
            .add_line_to_order(actor(), opened.order_id, one_of(item()))
            .await;
        assert!(
            matches!(late, Err(AppError::BillAlreadyOpen)),
            "got {late:?}"
        );
    });
}

/// A channel the store does not take, an item it does not sell, and an order it never opened are
/// each refused, and none of them writes anything.
#[test]
fn what_the_store_cannot_sell_is_refused() {
    run_ready(async {
        let mut dine_in_only = session();
        dine_in_only.enabled_channels = Some(BTreeSet::from([SalesChannel::DineIn]));
        let edge = edge_over(FakeStore::default(), dine_in_only);
        let refused = edge
            .open_counter_order(actor(), SalesChannel::Takeaway)
            .await;
        assert!(
            matches!(refused, Err(AppError::ChannelNotAccepted)),
            "got {refused:?}"
        );

        let opened = edge
            .open_counter_order(actor(), SalesChannel::DineIn)
            .await
            .expect("an accepted channel opens");
        let unknown = edge
            .add_line_to_order(
                actor(),
                opened.order_id,
                one_of(MenuItemId::new(Ulid::from_u128(999))),
            )
            .await;
        assert!(
            matches!(unknown, Err(AppError::ItemNotSellable)),
            "got {unknown:?}"
        );

        let nowhere = edge
            .add_line_to_order(
                actor(),
                pos_proto::ids::OrderId::new(Ulid::from_u128(0xDEAD)),
                one_of(item()),
            )
            .await;
        assert!(
            matches!(nowhere, Err(AppError::UnknownOrder)),
            "got {nowhere:?}"
        );
    });
}

/// A walk-in comes back after a restart, still on its channel: the order, its line and the rate
/// it is taxed at.
#[test]
fn a_walk_in_survives_a_restart() {
    run_ready(async {
        let store = FakeStore::default();
        let order_id = {
            let edge = edge_over(store.clone(), session());
            let opened = edge
                .open_counter_order(actor(), SalesChannel::Takeaway)
                .await
                .expect("opens");
            edge.add_line_to_order(actor(), opened.order_id, one_of(item()))
                .await
                .expect("adds");
            opened.order_id
        };

        let edge = edge_over(store, session());
        edge.rebuild().await.expect("rebuilds from the log");
        let live = edge.live_orders();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].order_id, order_id);
        assert_eq!(
            edge.order_totals(order_id).expect("totals").total_due,
            vnd(54_000),
            "still taxed as takeaway"
        );
    });
}
