// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Retention over the real store: what the edge forgets, and what it must not
//! ([ADR-0145](../../../docs/adr/0145-the-edge-keeps-events-until-synced-and-n-days-old.md)).
//!
//! The store's own rules (unsynced stays, the head stays) are tested in `store-sqlite`. These cases
//! are the edge's half: an order still open keeps every event from the moment it began, the
//! store's own retention is a floor, and a box restarted over a pruned log rebuilds exactly the
//! trading it had. The edge's clock is the real one, so "later" is a `now` passed to the sweep.

use core::num::NonZeroU32;
use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity, SystemClock};
use pos_ports::event_store::{EventQuery, EventStore, OutboxPosition};
use pos_proto::chain::ChainStatus;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StoreId, TableId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::{ClockSource as _, CurrencyCode, PaymentMethod, SalesChannel, TableState};
use store_sqlite::SqliteStore;

const DAY_MS: i64 = 86_400_000;

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

fn actor() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn table() -> TableId {
    TableId::new(Ulid::from_u128(42))
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn session() -> EdgeSession {
    let item = MenuItemId::new(Ulid::from_u128(500));
    let class = EdgeSession::standard_tax_class();
    let menu = MenuCatalog::new().with(MenuEntry::new(
        item,
        DisplayName::new("Margherita"),
        vnd(150_000),
        class,
    ));
    let rates = TaxRateTable::new().with(class, SalesChannel::DineIn, TaxRate::from_percent(10));
    EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates)
}

fn a_line() -> pos_edge::LineDraft {
    pos_edge::LineDraft {
        menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
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

fn edge_over(store: &SqliteStore) -> Edge<SqliteStore> {
    Edge::new(
        store.clone(),
        StoreIdentity::for_store(store_id()),
        session(),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

/// `days` from now.
fn in_days(days: i64) -> Timestamp {
    let now = SystemClock.now().as_milliseconds_since_epoch();
    Timestamp::from_milliseconds_since_epoch(now + days * DAY_MS).expect("instant")
}

/// The link acknowledges everything the store has written.
async fn sync_everything(store: &SqliteStore) {
    let batch = store
        .outbox_batch(
            store_id(),
            OutboxPosition::START,
            NonZeroU32::new(10_000).expect("positive"),
        )
        .await
        .expect("read the outbox");
    let through = batch.last().expect("the day wrote something").position;
    store
        .acknowledge_outbox(store_id(), through)
        .await
        .expect("acknowledge");
}

async fn kept(store: &SqliteStore) -> usize {
    let query = EventQuery::first(store_id(), NonZeroU32::new(10_000).expect("positive"));
    store.read(&query).await.expect("read").len()
}

/// A table seated, served, paid and cleaned: a finished day, with nothing left open.
async fn a_finished_table(edge: &Edge<SqliteStore>) {
    edge.seat_table(actor(), table(), None)
        .await
        .expect("seats");
    edge.add_line(actor(), table(), a_line())
        .await
        .expect("adds");
    let bill = edge.open_bill(actor(), table()).await.expect("opens");
    edge.settle_bill(
        actor(),
        bill.bill_id,
        vec![Payment {
            method: PaymentMethod::Cash,
            tendered: vnd(165_000),
            applied_to_bill: vnd(165_000),
            tip: Money::zero(CurrencyCode::VND),
        }],
        None,
    )
    .await
    .expect("settles");
    edge.clean_table(actor(), table()).await.expect("cleans");
}

/// A finished, synced day is forgotten once it is old, down to the chain head, and a box
/// restarted over what is left rebuilds the same (empty) trading and a chain that verifies.
#[tokio::test]
async fn a_finished_synced_day_is_forgotten_once_old() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = SqliteStore::open(dir.path().join("store.db")).expect("open");
    let edge = edge_over(&store);
    a_finished_table(&edge).await;
    sync_everything(&store).await;
    let written = kept(&store).await;

    let deleted = pos_edge::retention::sweep_once(&edge, in_days(100))
        .await
        .expect("sweeps");
    assert_eq!(usize::try_from(deleted).expect("small"), written - 1);
    assert_eq!(kept(&store).await, 1, "only the chain head stays");
    assert!(matches!(
        store.verify_chain(store_id()).await.expect("verify"),
        ChainStatus::Intact { checked: 1, .. }
    ));

    let rebuilt = edge_over(&store);
    rebuilt.rebuild().await.expect("rebuilds");
    assert!(rebuilt.live_orders().is_empty());
    assert_eq!(rebuilt.table_state(table()), TableState::Free);
}

/// An order still open keeps every event from the moment it began, however old and however
/// synced: a restart must still find the table, the order and its lines.
#[tokio::test]
async fn an_open_order_keeps_everything_since_it_began() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = SqliteStore::open(dir.path().join("store.db")).expect("open");
    let edge = edge_over(&store);
    edge.seat_table(actor(), table(), None)
        .await
        .expect("seats");
    edge.add_line(actor(), table(), a_line())
        .await
        .expect("adds");
    sync_everything(&store).await;

    let deleted = pos_edge::retention::sweep_once(&edge, in_days(400))
        .await
        .expect("sweeps");
    assert_eq!(deleted, 0);

    let rebuilt = edge_over(&store);
    rebuilt.rebuild().await.expect("rebuilds");
    assert_eq!(rebuilt.table_state(table()), TableState::Occupied);
    let live = rebuilt.live_orders();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].lines.len(), 1);
}

/// The store's retention is a floor: a synced, finished day younger than it stays.
#[tokio::test]
async fn nothing_goes_inside_the_store_s_retention() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = SqliteStore::open(dir.path().join("store.db")).expect("open");
    let edge = edge_over(&store);
    a_finished_table(&edge).await;
    sync_everything(&store).await;

    // The default is 90 days, so a month on, nothing is old enough.
    let deleted = pos_edge::retention::sweep_once(&edge, in_days(30))
        .await
        .expect("sweeps");
    assert_eq!(deleted, 0);
}
