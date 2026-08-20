// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The one fold that turns events into a per-trading-day rollup
//! ([ADR-0036](../../../docs/adr/0036-materialised-rollups.md)).
//!
//! Both the from-log computation ([`Cloud::daily_rollups`](crate::cloud::Cloud::daily_rollups)) and
//! the incrementally-**materialised** projection ([`super::projection`]) run *this* function over
//! their events, so the two paths cannot drift: whatever a full re-scan would compute is exactly what
//! the maintained rollup holds. The fold uses only envelope fields (`business_date`, `event_type`),
//! so it needs no per-event-type decoding.

use std::collections::BTreeMap;

use pos_proto::envelope::{EventEnvelope, RawPayload};

use crate::cloud::DailyRollup;

/// Folds one event into `days`, creating the trading day's rollup if absent and counting the event
/// against its total and its type.
pub fn fold_event(days: &mut BTreeMap<String, DailyRollup>, event: &EventEnvelope<RawPayload>) {
    let day = days
        .entry(event.business_date.to_string())
        .or_insert_with(|| DailyRollup {
            business_date: event.business_date.to_string(),
            total_events: 0,
            by_type: BTreeMap::new(),
        });
    day.total_events = day.total_events.saturating_add(1);
    let type_count = day
        .by_type
        .entry(event.event_type.as_str().to_owned())
        .or_insert(0);
    *type_count = type_count.saturating_add(1);
}

/// Renders a materialised day map as the dashboard's list, oldest trading day first.
#[must_use]
pub fn render(days: BTreeMap<String, DailyRollup>) -> Vec<DailyRollup> {
    days.into_values().collect()
}
