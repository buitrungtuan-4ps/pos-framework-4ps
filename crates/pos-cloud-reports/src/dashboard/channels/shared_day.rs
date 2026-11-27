// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The console's half of the edge-to-console match
//! ([ADR-0259](../../../../../docs/adr/0259-a-sale-is-reported-by-its-channel-and-its-order-type.md)):
//! the shared store day in `pos-proto/testdata/sale_channel_day.json`, folded by the rollup, gives
//! the figures the file expects. `pos-edge-app` folds the same file through the till's X and asserts
//! the same figures, so the two sides agree without either crate depending on the other.

use std::collections::BTreeMap;

use pos_proto::envelope::{EventEnvelope, RawPayload};
use serde::Deserialize;

use crate::dashboard::channels::{KeptChannels, fold_channels};
use crate::dashboard::rollup::fold_revenue;
use crate::dashboard::tenders::fold_tenders;
use crate::dashboard::voids::{KeptBills, fold_voids};
use crate::types::ChannelSales;

const DAY: &str = include_str!("../../../../pos-proto/testdata/sale_channel_day.json");

#[derive(Deserialize)]
struct Day {
    events: Vec<EventEnvelope<RawPayload>>,
    expected: Expected,
}

#[derive(Deserialize)]
struct Expected {
    business_date: String,
    bills: u64,
    sales: i64,
    by_channel: Vec<ChannelSales>,
    by_app_tender: BTreeMap<String, i64>,
    by_tender: BTreeMap<String, i64>,
}

#[test]
fn the_shared_store_day_folds_to_the_figures_the_till_shows_too() {
    let day: Day = serde_json::from_str(DAY).expect("the shared day parses");
    let (mut revenue, mut cash) = (BTreeMap::new(), BTreeMap::new());
    let (mut voids, mut channels) = (KeptBills::default(), KeptChannels::default());
    for event in &day.events {
        fold_revenue(&mut revenue, event);
        fold_tenders(&mut revenue, event);
        fold_voids(&mut voids, &mut revenue, &mut cash, event);
        fold_channels(&mut channels, &mut revenue, event);
    }
    let expected = day.expected;
    let folded = &revenue[&expected.business_date];
    assert_eq!((folded.bills, folded.net), (expected.bills, expected.sales));
    assert_eq!(folded.by_channel, expected.by_channel);
    assert_eq!(folded.by_app_tender, expected.by_app_tender);
    assert_eq!(folded.by_tender, expected.by_tender);
}
