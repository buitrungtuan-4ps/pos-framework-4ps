// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `EdgeOrderIn` against the shared `OrderIn` contract suite ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)).
//!
//! The suite is the specification of what a caller may rely on ([ADR-0026](../../../docs/adr/0026-port-shapes.md)
//! §5): a first submit creates the order and its total comes from the store's own menu; a retry on
//! the same `(channel, reference)` returns the same order with `created: false`; the same reference
//! on two channels is two orders; a quoted price that differs is repriced, not honoured; an unknown
//! item and an empty order are refused. Running it here proves the edge's implementation honours all
//! of that, against the in-memory store and ledger.

use std::sync::Arc;

use pos_contract_tests::harness::{OrderInHarness, Setup};
use pos_edge::{
    Edge, EdgeOrderIn, EdgeSession, InMemoryIntakeLedger, InMemoryQueueNumbers, InMemoryReceipts,
    StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::SalesChannel;
use pos_proto::ids::{DeviceId, MenuItemId, StoreId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

fn store() -> StoreId {
    StoreId::new(Ulid::from_u128(7))
}

fn price() -> Money {
    Money::new(CurrencyCode::VND, 120_000)
}

/// The catalog and rate table a fresh edge is seeded with: one item the store sells at [`price`], on
/// the standard class, taxed on every channel the suite exercises.
fn seeded_session() -> EdgeSession {
    let (item, unit_price) = (MenuItemId::new(Ulid::from_u128(500)), price());
    let class = EdgeSession::standard_tax_class();
    let menu = MenuCatalog::new().with(MenuEntry::new(
        item,
        DisplayName::new("Margherita"),
        unit_price,
        class,
    ));
    let rates = TaxRateTable::new()
        .with(class, SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(class, SalesChannel::Takeaway, TaxRate::from_percent(10))
        .with(class, SalesChannel::Delivery, TaxRate::from_percent(10));
    EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates)
}

/// Supplies a fresh `EdgeOrderIn` — a new edge over the in-memory store and a new idempotency
/// ledger — plus the known/unknown menu items and the store id the cases use.
struct EdgeIntakeHarness;

impl OrderInHarness for EdgeIntakeHarness {
    type Intake = EdgeOrderIn<FakeStore, InMemoryIntakeLedger, InMemoryQueueNumbers>;

    async fn fresh(&self) -> Setup<Self::Intake> {
        let edge = Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(store()),
            seeded_session(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator");
        Ok(EdgeOrderIn::new(
            Arc::new(edge),
            InMemoryIntakeLedger::new(),
            InMemoryQueueNumbers::new(),
            DeviceId::new(Ulid::from_u128(20)),
        ))
    }

    fn known_menu_item(&self) -> (MenuItemId, Money) {
        (MenuItemId::new(Ulid::from_u128(500)), price())
    }

    fn unknown_menu_item(&self) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(u128::MAX))
    }

    fn store_id(&self) -> StoreId {
        store()
    }
}

mod order_in {
    use super::{EdgeIntakeHarness, run_ready};
    pos_contract_tests::order_in_suite!(EdgeIntakeHarness, run_ready);
}
