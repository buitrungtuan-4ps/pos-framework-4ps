// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An open-price item sold at the price the cashier typed, within its published bounds, with
//! `sales.line.add` alone ([ADR-0222](../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md)
//! decision 4): on the counter's edge-priced path and on the table path alike.

use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_edge::{
    AppError, Edge, EdgeSession, InMemoryReceipts, LineDraft, OrderLineChoice, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::SalesChannel;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StoreId, TableId};
use pos_proto::item_codes::{ItemCodes, OpenPrice};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

fn actor() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn cake() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(700))
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

/// A store selling a fake cake at any price from 10,000 to 90,000, taxed at 10%.
fn edge() -> Edge<FakeStore> {
    let class = EdgeSession::standard_tax_class();
    let cake = MenuEntry {
        codes: ItemCodes {
            open_price: Some(OpenPrice {
                minimum: vnd(10_000),
                maximum: vnd(90_000),
            }),
            ..ItemCodes::default()
        },
        ..MenuEntry::new(cake(), DisplayName::new("Demo cake"), vnd(1), class)
    };
    let rates = TaxRateTable::new()
        .with(class, SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(class, SalesChannel::Takeaway, TaxRate::from_percent(10));
    let session = EdgeSession::bootstrap()
        .with_menu(MenuCatalog::new().with(cake))
        .with_tax_rates(rates);
    Edge::new(
        FakeStore::default(),
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

fn out_of_range<T: std::fmt::Debug>(result: &Result<T, AppError>) -> bool {
    matches!(
        result,
        Err(AppError::Domain(DomainError::PriceOutOfRange { .. }))
    )
}

#[test]
fn the_counter_sells_an_open_price_item_at_the_typed_price_within_its_bounds() {
    run_ready(async {
        let edge = edge();
        let order = edge
            .open_counter_order(actor(), SalesChannel::Takeaway)
            .await
            .expect("opens")
            .order_id;
        let typed = |minor: Option<i64>| OrderLineChoice {
            menu_item_id: cake(),
            quantity: Quantity::ONE,
            modifier_menu_item_ids: Vec::new(),
            seat: None,
            course_id: None,
            note_present: false,
            typed_unit_price: minor.map(vnd),
        };
        for refused in [None, Some(9_999), Some(90_001)] {
            let line = edge.add_line_to_order(actor(), order, typed(refused)).await;
            assert!(out_of_range(&line), "{refused:?}: {line:?}");
        }
        edge.add_line_to_order(actor(), order, typed(Some(45_000)))
            .await
            .expect("within its bounds");
        assert_eq!(
            edge.order_totals(order).expect("totals").subtotal,
            vnd(45_000)
        );
    });
}

#[test]
fn the_table_path_prices_an_open_price_line_only_within_its_bounds() {
    run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(42));
        edge.seat_table(actor(), table, None).await.expect("seats");
        let draft = |minor: i64| LineDraft {
            menu_item_id: cake(),
            display_name: DisplayName::new("Demo cake"),
            quantity: Quantity::ONE,
            unit_price: vnd(minor),
            line_total: vnd(minor),
            tax_class_id: EdgeSession::standard_tax_class(),
            tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
            seat: None,
            course_id: None,
            modifier_menu_item_ids: Vec::new(),
            note_present: false,
        };
        let refused = edge.add_line(actor(), table, draft(5_000)).await;
        assert!(out_of_range(&refused), "{refused:?}");
        edge.add_line(actor(), table, draft(30_000))
            .await
            .expect("within its bounds");
    });
}
