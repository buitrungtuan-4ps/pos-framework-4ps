// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The till's half of the edge-to-console match
//! ([ADR-0259](../../../../../docs/adr/0259-a-sale-is-reported-by-its-channel-and-its-order-type.md)):
//! the shared store day in `pos-proto/testdata/sale_channel_day.json`, rebuilt from the log, gives
//! the X the file expects. `pos-cloud-reports` folds the same file through the console's rollup and
//! asserts the same figures, so the two sides agree without either crate depending on the other.

use std::collections::BTreeMap;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_fakes::FakeStore;
use pos_ports::event_store::EventStore as _;
use pos_ports::{Transactional as _, TxContext as _};
use pos_proto::WireEnum as _;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{DeviceId, EmployeeId, ShiftId, StoreId};
use pos_proto::ulid::Ulid;
use serde::Deserialize;

use crate::app::{Edge, EdgeSession, StoreIdentity};
use crate::receipt::InMemoryReceipts;

const DAY: &str = include_str!("../../../../pos-proto/testdata/sale_channel_day.json");

#[derive(Deserialize)]
struct Day {
    store_id: StoreId,
    shift_id: ShiftId,
    events: Vec<EventEnvelope<RawPayload>>,
    expected: Expected,
}

#[derive(Deserialize)]
struct Expected {
    bills: u32,
    sales: i64,
    by_channel: Vec<Line>,
    by_app_tender: BTreeMap<String, i64>,
    by_tender: BTreeMap<String, i64>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct Line {
    source: String,
    #[serde(default)]
    vendor_id: Option<String>,
    order_type: String,
    bills: u32,
    sales: i64,
}

#[test]
fn the_shared_store_day_rebuilds_to_the_figures_the_console_shows_too() {
    pos_fakes::executor::run_ready(async {
        let day: Day = serde_json::from_str(DAY).expect("the shared day parses");
        let store = FakeStore::default();
        let mut tx = store.begin().await.expect("begin");
        store.append(&mut tx, &day.events).await.expect("append");
        tx.commit().await.expect("commit");
        let edge = Edge::new(
            store,
            StoreIdentity::for_store(day.store_id),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        edge.rebuild().await.expect("rebuilds");
        let actor = Actor {
            employee_id: EmployeeId::new(Ulid::from_u128(1)),
            device_id: DeviceId::new(Ulid::from_u128(2)),
        };
        let x = edge.shift_x(actor, day.shift_id).expect("reads").figures;

        let expected = day.expected;
        assert_eq!(
            (x.bills, x.sales.amount_minor),
            (expected.bills, expected.sales)
        );
        let lines: Vec<Line> = x
            .channels
            .iter()
            .map(|line| Line {
                source: line.channel.source.as_wire().to_owned(),
                vendor_id: line.channel.vendor_id.map(|app| app.to_string()),
                order_type: line.channel.order_type.as_wire().to_owned(),
                bills: line.bills,
                sales: line.sales.amount_minor,
            })
            .collect();
        assert_eq!(
            lines, expected.by_channel,
            "the console's lines, in its order"
        );
        let apps: BTreeMap<String, i64> = x
            .tenders
            .in_app
            .iter()
            .map(|app| (app.vendor_id.to_string(), app.amount.amount_minor))
            .collect();
        assert_eq!(apps, expected.by_app_tender);
        let tender = |token: &str| expected.by_tender.get(token).copied().unwrap_or(0);
        assert_eq!(
            (
                x.tenders.cash.amount_minor,
                x.tenders.card.amount_minor,
                x.tenders.other.amount_minor,
            ),
            (
                tender("PAYMENT_METHOD_CASH"),
                tender("PAYMENT_METHOD_CARD"),
                tender("PAYMENT_METHOD_VENDOR"),
            )
        );
    });
}
