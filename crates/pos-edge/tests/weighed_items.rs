// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A weighed item sells by the grams a scale's label carries
//! ([ADR-0223](../../../docs/adr/0223-a-weighed-item-is-priced-by-its-weight.md)): its price is per
//! kilogram, its line counts net grams, the line records the label as its weight's source, a
//! weight below the store's least is refused, and the item is never sold by the piece.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::weighing::WeighError;
use pos_edge::app::{AppError, LineDraft};
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::SalesChannel;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, OrderId, StoreId};
use pos_proto::item_codes::{ItemCodes, VariableMeasureLayout};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::weighing::{PublishedWeighing, SoldBy};
use pos_proto::wire_enum::Open;

fn actor() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn rye() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(600))
}

/// Fake rye at 120,000 a kilogram with PLU 9, sold by weight, untaxed, on a store reading weight
/// labels and refusing a line under `min_grams`.
fn edge(min_grams: i64) -> Edge<FakeStore> {
    let class = EdgeSession::standard_tax_class();
    let rye = MenuEntry {
        codes: ItemCodes {
            barcodes: vec!["8930000000019".to_owned()],
            plu: Some("9".to_owned()),
            sold_by: Open::from_known(SoldBy::Weight),
            ..ItemCodes::default()
        },
        ..MenuEntry::new(rye(), DisplayName::new("Fake rye"), vnd(120_000), class)
    };
    let mut session = EdgeSession::bootstrap()
        .with_menu(MenuCatalog::new().with(rye))
        .with_tax_rates(TaxRateTable::new().with(
            class,
            SalesChannel::Takeaway,
            TaxRate::from_percent(0),
        ));
    session.scan.variable_measure_layout = Open::from_known(VariableMeasureLayout::Weight5);
    session.weighing = PublishedWeighing {
        min_grams: Some(min_grams),
        ..PublishedWeighing::default()
    };
    Edge::new(
        FakeStore::default(),
        StoreIdentity::for_store(store_id()),
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

/// Every `sales.order_line.added` payload on the store's log.
async fn lines_added(edge: &Edge<FakeStore>) -> Vec<serde_json::Value> {
    let query = EventQuery::first(store_id(), NonZeroU32::new(64).expect("non-zero"));
    edge.store()
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .filter(|envelope| envelope.event_type.as_str() == "sales.order_line.added")
        .map(|envelope| serde_json::from_str(envelope.data.as_json()).expect("a payload"))
        .collect()
}

#[test]
fn a_weight_label_sells_its_grams_at_the_price_per_kilogram_and_records_the_label() {
    run_ready(async {
        let edge = edge(1);
        let order_id = order(&edge).await;
        // 20 00009 00375 and its check digit: 375 g of rye, 45,000 at 120,000 a kilogram.
        edge.add_scanned_line(actor(), order_id, "2000009003758")
            .await
            .expect("a weighed item takes its label");
        assert_eq!(
            edge.order_totals(order_id).expect("totals").total_due,
            vnd(45_000)
        );
        let lines = lines_added(&edge).await;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["quantity"]["milli"], 375);
        assert_eq!(lines[0]["unit_price"]["amount_minor"], 120_000);
        assert_eq!(lines[0]["weight_source"], "WEIGHT_SOURCE_LABEL");
    });
}

#[test]
fn a_label_under_the_least_weight_is_refused() {
    run_ready(async {
        let edge = edge(50);
        let order_id = order(&edge).await;
        // 20 grams of rye, under the store's 50.
        let light = edge
            .add_scanned_line(actor(), order_id, "2000009000207")
            .await;
        assert!(
            matches!(
                light,
                Err(AppError::Weighing(WeighError::BelowMinimum {
                    min_grams: 50
                }))
            ),
            "{light:?}"
        );
        assert!(lines_added(&edge).await.is_empty());
    });
}

#[test]
fn a_weighed_item_is_never_sold_by_the_piece() {
    run_ready(async {
        let edge = edge(1);
        let order_id = order(&edge).await;
        for code in ["8930000000019", "9"] {
            let piece = edge.add_scanned_line(actor(), order_id, code).await;
            assert!(
                matches!(piece, Err(AppError::Weighing(WeighError::Required))),
                "{code}: {piece:?}"
            );
        }
        let table = edge
            .add_line(
                actor(),
                pos_proto::ids::TableId::new(Ulid::from_u128(30)),
                LineDraft {
                    menu_item_id: rye(),
                    display_name: DisplayName::new("Fake rye"),
                    quantity: pos_proto::Quantity::ONE,
                    unit_price: vnd(120_000),
                    line_total: vnd(120_000),
                    tax_class_id: EdgeSession::standard_tax_class(),
                    tax_rate: TaxRate::from_percent(0).as_ratio(),
                    seat: None,
                    course_id: None,
                    modifier_menu_item_ids: Vec::new(),
                    note_present: false,
                },
            )
            .await;
        assert!(
            matches!(table, Err(AppError::Weighing(WeighError::Required))),
            "{table:?}"
        );
        assert!(lines_added(&edge).await.is_empty());
    });
}
