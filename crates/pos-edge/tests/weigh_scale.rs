// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A weighed item's line from the scale beside the till, or a weight typed by hand
//! ([ADR-0223](../../../docs/adr/0223-a-weighed-item-is-priced-by-its-weight.md) decision 2): the
//! scale's settled reading less the item's tare is the line's net grams; a moving, overloaded or
//! absent scale is refused and never read as a number; a typed weight is taken only where the
//! store's `weighing.manual_entry` allows it; each line records where its weight came from.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::weighing::WeighError;
use pos_edge::app::{AppError, WeighFrom};
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::executor::run_ready;
use pos_fakes::{FakeScale, FakeStore};
use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::scale::ScaleReading;
use pos_proto::SalesChannel;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, OrderId, StoreId};
use pos_proto::item_codes::ItemCodes;
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

fn salad() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(700))
}

fn loaf() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(701))
}

/// Fake salad at 200,000 a kilogram in a 25 g bowl, sold by weight, and a fake loaf sold by the
/// piece, untaxed, on a store that does or does not take a typed weight, reading `scale`.
fn edge(scale: &FakeScale, manual_entry: bool) -> Edge<FakeStore> {
    let class = EdgeSession::standard_tax_class();
    let salad = MenuEntry {
        codes: ItemCodes {
            sold_by: Open::from_known(SoldBy::Weight),
            tare_grams: Some(25),
            ..ItemCodes::default()
        },
        ..MenuEntry::new(salad(), DisplayName::new("Fake salad"), vnd(200_000), class)
    };
    let loaf = MenuEntry::new(loaf(), DisplayName::new("Fake loaf"), vnd(30_000), class);
    let mut session = EdgeSession::bootstrap()
        .with_menu(MenuCatalog::new().with(salad).with(loaf))
        .with_tax_rates(TaxRateTable::new().with(
            class,
            SalesChannel::Takeaway,
            TaxRate::from_percent(0),
        ));
    session.weighing = PublishedWeighing {
        manual_entry: Some(manual_entry),
        ..PublishedWeighing::default()
    };
    Edge::new(
        FakeStore::default(),
        StoreIdentity::for_store(store_id()),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
    .with_scale(scale.clone())
}

async fn order(edge: &Edge<FakeStore>) -> OrderId {
    edge.open_counter_order(actor(), SalesChannel::Takeaway)
        .await
        .expect("opens")
        .order_id
}

/// The `weight_source` and net grams of every line added, in order.
async fn weighed(edge: &Edge<FakeStore>) -> Vec<(serde_json::Value, serde_json::Value)> {
    let query = EventQuery::first(store_id(), NonZeroU32::new(64).expect("non-zero"));
    edge.store()
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .filter(|envelope| envelope.event_type.as_str() == "sales.order_line.added")
        .map(|envelope| {
            let line: serde_json::Value =
                serde_json::from_str(envelope.data.as_json()).expect("a payload");
            (
                line["weight_source"].clone(),
                line["quantity"]["milli"].clone(),
            )
        })
        .collect()
}

#[test]
fn the_scales_settled_weight_less_the_tare_is_the_line() {
    run_ready(async {
        let scale = FakeScale::new();
        let edge = edge(&scale, false);
        let order_id = order(&edge).await;
        // 400 g on the scale, 25 g of it the bowl: 375 g at 200,000 a kilogram is 75,000.
        scale.settle(400);
        edge.add_weighed_line(actor(), order_id, salad(), WeighFrom::Scale)
            .await
            .expect("a settled weight");
        assert_eq!(
            edge.order_totals(order_id).expect("totals").total_due,
            vnd(75_000)
        );
        assert_eq!(
            weighed(&edge).await,
            vec![(
                serde_json::json!("WEIGHT_SOURCE_SCALE"),
                serde_json::json!(375)
            )]
        );
    });
}

#[test]
fn a_scale_that_has_not_settled_or_does_not_answer_is_refused() {
    run_ready(async {
        let scale = FakeScale::new();
        let edge = edge(&scale, false);
        let order_id = order(&edge).await;
        for reading in [
            ScaleReading::Unstable,
            ScaleReading::Overloaded,
            ScaleReading::UnderZero,
        ] {
            scale.set(reading);
            let refused = edge
                .add_weighed_line(actor(), order_id, salad(), WeighFrom::Scale)
                .await;
            assert!(
                matches!(refused, Err(AppError::ScaleNotSettled)),
                "{reading:?}: {refused:?}"
            );
        }
        // Only the bowl on the scale: nothing to sell.
        scale.settle(25);
        let empty = edge
            .add_weighed_line(actor(), order_id, salad(), WeighFrom::Scale)
            .await;
        assert!(
            matches!(
                empty,
                Err(AppError::Weighing(WeighError::BelowMinimum { .. }))
            ),
            "{empty:?}"
        );
        scale.unplug();
        let absent = edge
            .add_weighed_line(actor(), order_id, salad(), WeighFrom::Scale)
            .await;
        assert!(matches!(absent, Err(AppError::ScaleAbsent)), "{absent:?}");
        assert!(weighed(&edge).await.is_empty());
    });
}

#[test]
fn a_typed_weight_is_taken_only_where_the_store_allows_it() {
    run_ready(async {
        let scale = FakeScale::new();
        let typed = WeighFrom::Manual { net_grams: 250 };
        let refusing = edge(&scale, false);
        let order_id = order(&refusing).await;
        let refused = refusing
            .add_weighed_line(actor(), order_id, salad(), typed)
            .await;
        assert!(
            matches!(refused, Err(AppError::ManualWeightNotAllowed)),
            "{refused:?}"
        );

        let allowing = edge(&scale, true);
        let order_id = order(&allowing).await;
        allowing
            .add_weighed_line(actor(), order_id, salad(), typed)
            .await
            .expect("a typed weight, net, where the store takes one");
        assert_eq!(
            weighed(&allowing).await,
            vec![(
                serde_json::json!("WEIGHT_SOURCE_MANUAL"),
                serde_json::json!(250)
            )]
        );
        let piece = allowing
            .add_weighed_line(actor(), order_id, loaf(), typed)
            .await;
        assert!(
            matches!(piece, Err(AppError::Weighing(WeighError::NotSoldByWeight))),
            "{piece:?}"
        );
    });
}

#[test]
fn a_country_that_forbids_a_typed_weight_refuses_one_whatever_the_setting_says() {
    // ADR-0223 decision 3: the pack's law holds even where the store's setting is on, as a value
    // inherited from before its country was known would be.
    run_ready(async {
        let scale = FakeScale::new();
        let edge = edge(&scale, true);
        assert!(edge.manual_weight_allowed());
        edge.forbid_manual_weight();
        assert!(!edge.manual_weight_allowed());
        let order_id = order(&edge).await;
        let refused = edge
            .add_weighed_line(
                actor(),
                order_id,
                salad(),
                WeighFrom::Manual { net_grams: 250 },
            )
            .await;
        assert!(
            matches!(refused, Err(AppError::ManualWeightNotAllowed)),
            "{refused:?}"
        );
        scale.settle(400);
        edge.add_weighed_line(actor(), order_id, salad(), WeighFrom::Scale)
            .await
            .expect("the scale still weighs");
    });
}
