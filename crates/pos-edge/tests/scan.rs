// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A scanned code becomes a line the edge prices
//! ([ADR-0222](../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md)
//! decisions 3 and 5): a barcode or a PLU sells one at the menu's price; a price label read by the
//! store's layout sells an open-price item at its price within the item's bounds, and is refused
//! for a fixed-price item or outside the bounds; a weight label is refused for an item sold by the
//! piece (`weighed_items.rs` sells a weighed one); a code no item carries is refused.

use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_edge::app::AppError;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::SalesChannel;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, OrderId, StoreId};
use pos_proto::item_codes::{ItemCodes, OpenPrice, VariableMeasureLayout};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

fn actor() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

/// A fake loaf at 30,000 with a barcode and PLU 101, and fake cheese at an open price from 10,000
/// to 90,000 with PLU 7, untaxed, on a store reading labels by `layout`.
fn edge(layout: VariableMeasureLayout) -> Edge<FakeStore> {
    let class = EdgeSession::standard_tax_class();
    let loaf = MenuEntry {
        codes: ItemCodes {
            barcodes: vec!["8930000000019".to_owned()],
            plu: Some("101".to_owned()),
            ..ItemCodes::default()
        },
        ..MenuEntry::new(
            MenuItemId::new(Ulid::from_u128(500)),
            DisplayName::new("Fake loaf"),
            vnd(30_000),
            class,
        )
    };
    let cheese = MenuEntry {
        codes: ItemCodes {
            plu: Some("7".to_owned()),
            open_price: Some(OpenPrice {
                minimum: vnd(10_000),
                maximum: vnd(90_000),
            }),
            ..ItemCodes::default()
        },
        ..MenuEntry::new(
            MenuItemId::new(Ulid::from_u128(501)),
            DisplayName::new("Fake cheese"),
            vnd(1),
            class,
        )
    };
    let mut session = EdgeSession::bootstrap()
        .with_menu(MenuCatalog::new().with(loaf).with(cheese))
        .with_tax_rates(TaxRateTable::new().with(
            class,
            SalesChannel::Takeaway,
            TaxRate::from_percent(0),
        ));
    session.scan.variable_measure_layout = Open::from_known(layout);
    Edge::new(
        FakeStore::default(),
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

async fn order(edge: &Edge<FakeStore>) -> OrderId {
    edge.open_counter_order(actor(), SalesChannel::Takeaway)
        .await
        .expect("opens")
        .order_id
}

#[test]
fn a_barcode_or_a_plu_sells_one_at_the_menu_price() {
    run_ready(async {
        let edge = edge(VariableMeasureLayout::None);
        let order_id = order(&edge).await;
        for code in ["8930000000019", "101"] {
            edge.add_scanned_line(actor(), order_id, code)
                .await
                .expect("adds");
        }
        let due = edge.order_totals(order_id).expect("totals").total_due;
        assert_eq!(due, vnd(60_000));
    });
}

#[test]
fn a_price_label_sells_an_open_price_item_within_its_bounds_and_nothing_else() {
    run_ready(async {
        let edge = edge(VariableMeasureLayout::Price5);
        let order_id = order(&edge).await;
        // 20 00007 45000 and its check digit: cheese at 45,000.
        edge.add_scanned_line(actor(), order_id, "2000007450004")
            .await
            .expect("a label within the bounds");
        assert_eq!(
            edge.order_totals(order_id).expect("totals").total_due,
            vnd(45_000)
        );
        // 95,000 is above the cheese's most.
        let above = edge
            .add_scanned_line(actor(), order_id, "2000007950009")
            .await;
        assert!(
            matches!(
                above,
                Err(AppError::Domain(DomainError::PriceOutOfRange { .. }))
            ),
            "{above:?}"
        );
        // A label for the loaf, a fixed-price item.
        let fixed = edge
            .add_scanned_line(actor(), order_id, "2000101450009")
            .await;
        assert!(
            matches!(fixed, Err(AppError::PriceLabelNotOpenPrice)),
            "{fixed:?}"
        );
        let unknown = edge.add_scanned_line(actor(), order_id, "123").await;
        assert!(matches!(unknown, Err(AppError::UnknownCode)), "{unknown:?}");
    });
}

#[test]
fn a_weight_label_on_a_piece_item_is_refused_and_a_store_without_a_layout_reads_a_label_as_a_plain_code()
 {
    run_ready(async {
        let weighed = edge(VariableMeasureLayout::Weight5);
        let order_id = order(&weighed).await;
        let refused = weighed
            .add_scanned_line(actor(), order_id, "2000007450004")
            .await;
        assert!(
            matches!(refused, Err(AppError::WeightLabelNotSold)),
            "{refused:?}"
        );

        let plain = edge(VariableMeasureLayout::None);
        let order_id = order(&plain).await;
        let refused = plain
            .add_scanned_line(actor(), order_id, "2000007450004")
            .await;
        assert!(matches!(refused, Err(AppError::UnknownCode)), "{refused:?}");
    });
}
