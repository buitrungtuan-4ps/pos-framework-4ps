// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A line's new quantity reaches the product mix, through the projector the cloud runs.

use std::sync::Mutex;

use pos_contract_tests::fixtures;
use pos_fakes::FakeStore;
use pos_ports::event_store::EventStore as _;
use pos_ports::{Transactional as _, TxContext as _};
use pos_proto::BusinessDate;
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::EventType;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;
use serde_json::json;

use crate::dashboard::projection::{RollupError, RollupStore, StoredRollups, project};
use crate::types::ItemMix;

const ORDER: u128 = 0x0D;
const ICED_TEA: u128 = 0x7EA;
const MARGHERITA: u128 = 0x9A;

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0x5707))
}

fn ulid(n: u128) -> String {
    Ulid::from_u128(n).to_string()
}

fn vnd(minor: i64) -> serde_json::Value {
    json!({ "currency_code": "VND", "amount_minor": minor })
}

/// An event on 2026-10-31 whose id sorts by `seed`.
fn event(
    seed: u32,
    event_type: EventType,
    payload: &serde_json::Value,
) -> EventEnvelope<RawPayload> {
    let mut base = fixtures::activations(store_id(), seed, 1).remove(0);
    base.business_date = BusinessDate::from_ymd(2026, 10, 31).expect("a valid date");
    base.event_type = EventTypeRef::from_known(event_type);
    base.data = RawPayload::encode(payload).expect("encode the payload");
    base
}

fn added(seed: u32, line: u128, item: u128, qty: i64, total: i64) -> EventEnvelope<RawPayload> {
    event(
        seed,
        EventType::SalesOrderLineAdded,
        &json!({
            "order_id": ulid(ORDER), "order_line_id": ulid(line), "menu_item_id": ulid(item),
            "display_name": "Iced tea (demo)", "quantity": { "milli": qty * 1000 },
            "unit_price": vnd(total / qty), "line_total": vnd(total),
            "tax_class_id": ulid(5), "tax_rate": { "numerator": 8, "denominator": 100 },
            "seat": null, "course_id": null, "note_present": false,
        }),
    )
}

fn updated(seed: u32, line: u128, qty: i64, total: i64) -> EventEnvelope<RawPayload> {
    event(
        seed,
        EventType::SalesOrderLineUpdated,
        &json!({
            "order_id": ulid(ORDER), "order_line_id": ulid(line),
            "quantity": { "milli": qty * 1000 }, "line_total": vnd(total),
        }),
    )
}

#[derive(Default)]
struct OneRollup(Mutex<StoredRollups>);

impl RollupStore for OneRollup {
    async fn load(&self, _: TenantId, _: StoreId) -> Result<StoredRollups, RollupError> {
        Ok(self.0.lock().expect("lock").clone())
    }

    async fn save(
        &self,
        _: TenantId,
        _: StoreId,
        rollups: &StoredRollups,
    ) -> Result<(), RollupError> {
        *self.0.lock().expect("lock") = rollups.clone();
        Ok(())
    }
}

/// Projects `log`, in two passes split at `split`, and returns the day's mix of `item`.
async fn mix_after(log: &[EventEnvelope<RawPayload>], split: usize, item: u128) -> ItemMix {
    let events = FakeStore::new();
    let rollups = OneRollup::default();
    let tenant = TenantId::new(Ulid::from_u128(0x7E));
    for part in [&log[..split], &log[split..]] {
        let mut tx = events.begin().await.expect("begin");
        events.append(&mut tx, part).await.expect("append");
        tx.commit().await.expect("commit");
        project(&events, &rollups, tenant, store_id())
            .await
            .expect("project");
    }
    let stored = rollups.0.lock().expect("lock").clone();
    stored.revenue["2026-10-31"].by_item[&ulid(item)].clone()
}

/// The pilot's iced tea: added at one for 39,000, raised to two for 78,000. The mix says two and
/// 78,000, across a projector pass between the two events as within one, and the pizza beside it
/// is untouched.
#[tokio::test]
async fn a_lines_new_quantity_and_total_replace_what_it_added_to_the_mix() {
    let log = [
        added(1, 0x11, ICED_TEA, 1, 39_000),
        added(2, 0x12, MARGHERITA, 1, 229_000),
        updated(3, 0x11, 2, 78_000),
    ];
    for split in [3, 2] {
        let tea = mix_after(&log, split, ICED_TEA).await;
        assert_eq!(
            (tea.ordered_qty_milli, tea.ordered_value),
            (2_000, 78_000),
            "split at {split}"
        );
        let pizza = mix_after(&log, split, MARGHERITA).await;
        assert_eq!(
            (pizza.ordered_qty_milli, pizza.ordered_value),
            (1_000, 229_000)
        );
    }

    // Lowered again, the mix follows the line down, not the sum of every quantity it held.
    let lowered = [
        added(1, 0x11, ICED_TEA, 1, 39_000),
        updated(2, 0x11, 3, 117_000),
        updated(3, 0x11, 2, 78_000),
    ];
    let tea = mix_after(&lowered, 3, ICED_TEA).await;
    assert_eq!((tea.ordered_qty_milli, tea.ordered_value), (2_000, 78_000));
}

/// A closed order's lines are let go, so what is kept is bounded by the open orders.
#[tokio::test]
async fn a_closed_orders_lines_are_not_kept() {
    let closed = event(
        3,
        EventType::SalesOrderClosed,
        &json!({ "order_id": ulid(ORDER) }),
    );
    let events = FakeStore::new();
    let rollups = OneRollup::default();
    let mut tx = events.begin().await.expect("begin");
    events
        .append(&mut tx, &[added(1, 0x11, ICED_TEA, 1, 39_000), closed])
        .await
        .expect("append");
    tx.commit().await.expect("commit");
    project(
        &events,
        &rollups,
        TenantId::new(Ulid::from_u128(0x7E)),
        store_id(),
    )
    .await
    .expect("project");
    assert!(rollups.0.lock().expect("lock").lines.orders.is_empty());
}
