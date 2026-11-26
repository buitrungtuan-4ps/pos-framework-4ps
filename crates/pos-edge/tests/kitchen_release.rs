// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! How a guest's QR order reaches the kitchen
//! ([ADR-0247](../../../docs/adr/0247-a-guest-or-vendor-order-reaches-the-kitchen-as-the-store-chooses.md)).
//!
//! `KITCHEN_RELEASE_CONFIRM_THEN_SEND`, the default, is two steps: confirming releases the order and
//! sends nothing. `_CONFIRM_FIRES` sends every line in the confirmation's own transaction.
//! `_AUTO_ACCEPT_AND_FIRE` holds nothing and sends every line in the order's own transaction, under
//! the box rather than a person, and a rebuilt edge reads the lines as sent.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::menu::PricedLine;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, InboundOrderOpened, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::floor::{KitchenStation, StationPlan};
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StationId, StoreId, TableId};
use pos_proto::kitchen_release::KitchenRelease;
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::qr::PublishedQr;
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{Open, OrderLineState, SalesChannel};

fn waiter() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

fn pizza() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

fn price() -> Money {
    Money::new(CurrencyCode::VND, 160_000)
}

fn a_guest_line() -> PricedLine {
    PricedLine {
        menu_item_id: pizza(),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: price(),
        line_total: price(),
        tax_class_id: EdgeSession::standard_tax_class(),
        tax_rate: Ratio::basis_points(800).expect("a valid rate"),
        modifier_menu_item_ids: Vec::new(),
        modifier_display_names: Vec::new(),
        repriced: false,
    }
}

/// A store that holds guest orders for staff and releases them to the kitchen as `release` says.
fn session(release: KitchenRelease) -> EdgeSession {
    let menu = MenuCatalog::new().with(MenuEntry::new(
        pizza(),
        DisplayName::new("Margherita"),
        price(),
        EdgeSession::standard_tax_class(),
    ));
    let station = StationId::new(Ulid::from_u128(900));
    let mut session = EdgeSession::bootstrap().with_menu(menu).with_stations(
        StationPlan::new()
            .with_station(KitchenStation {
                station_id: station,
                name: DisplayName::new("Pizza"),
                backup_station_id: None,
                late_after_seconds: None,
                ticket_language: Open::default(),
            })
            .with_default(station),
    );
    session.qr = PublishedQr {
        kitchen_release: Open::from_known(release),
        ..PublishedQr::default()
    };
    session.qr_staff_confirmation_required = true;
    session
}

fn edge_over(store: FakeStore, release: KitchenRelease) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(store_id()),
        session(release),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

async fn a_guest_order(edge: &Edge<FakeStore>) -> InboundOrderOpened {
    edge.open_inbound_order(
        waiter().device_id,
        Open::from_known(SalesChannel::Qr),
        Some(TableId::new(Ulid::from_u128(42))),
        &[(a_guest_line(), None)],
        None,
    )
    .await
    .expect("a guest's order is taken")
}

async fn logged_types(store: &FakeStore) -> Vec<String> {
    let query = EventQuery::first(store_id(), NonZeroU32::new(64).expect("a positive limit"));
    store
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .map(|envelope| envelope.event_type.as_str().to_owned())
        .collect()
}

#[test]
fn by_default_confirming_releases_the_order_and_sends_nothing() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), KitchenRelease::ConfirmThenSend);
        let opened = a_guest_order(&edge).await;
        assert!(opened.awaiting_staff_confirmation);
        assert!(!edge.confirm_fires(opened.order_id));
        let fired = edge
            .confirm_inbound_order(waiter(), opened.order_id)
            .await
            .expect("confirms");
        assert!(fired.is_empty(), "two steps: nothing is sent yet");
        let line = edge.order_line_ids(opened.order_id)[0];
        assert_eq!(edge.line_state(line), Some(OrderLineState::Added));
        edge.fire_order(waiter(), opened.order_id, None)
            .await
            .expect("then sent as any order is");
        assert_eq!(edge.line_state(line), Some(OrderLineState::Fired));
    });
}

#[test]
fn confirm_fires_sends_every_line_in_the_confirmations_transaction() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), KitchenRelease::ConfirmFires);
        let opened = a_guest_order(&edge).await;
        assert!(opened.awaiting_staff_confirmation);
        assert!(edge.confirm_fires(opened.order_id));
        let fired = edge
            .confirm_inbound_order(waiter(), opened.order_id)
            .await
            .expect("confirms and sends");
        assert_eq!(fired.len(), 1);
        let line = edge.order_line_ids(opened.order_id)[0];
        assert_eq!(edge.line_state(line), Some(OrderLineState::Fired));
        let types = logged_types(&store).await;
        let tail: Vec<&str> = types
            .iter()
            .rev()
            .take(2)
            .rev()
            .map(String::as_str)
            .collect();
        assert_eq!(
            tail,
            ["sales.order.confirmed_by_staff", "sales.order_line.fired"]
        );
    });
}

#[test]
fn auto_accept_and_fire_holds_nothing_and_sends_with_the_order() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), KitchenRelease::AutoAcceptAndFire);
        let opened = a_guest_order(&edge).await;
        assert!(!opened.awaiting_staff_confirmation, "nobody is asked");
        assert!(edge.orders_awaiting_staff_confirmation().is_empty());
        let line = edge.order_line_ids(opened.order_id)[0];
        assert_eq!(edge.line_state(line), Some(OrderLineState::Fired));
        assert_eq!(
            logged_types(&store).await,
            [
                "sales.order.opened",
                "sales.order_line.added",
                "sales.order_line.fired"
            ]
        );
        let query = EventQuery::first(store_id(), NonZeroU32::new(64).expect("a positive limit"));
        let fired = store.read(&query).await.expect("read the log");
        assert!(
            fired.iter().all(|envelope| envelope.employee_id.is_none()),
            "the box sent it, not a person"
        );

        let rebuilt = edge_over(store, KitchenRelease::AutoAcceptAndFire);
        rebuilt.rebuild().await.expect("rebuilds");
        assert_eq!(rebuilt.line_state(line), Some(OrderLineState::Fired));
    });
}
