// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The §4 stress-test models — [`capacity-and-reliability.md`](../../../docs/capacity-and-reliability.md) §4.
//!
//! Three of the published stress tests are behavioural properties, not throughput measurements, so they
//! can be made executable now without the hardware: the **offline drain** (how long a day of buffered
//! events takes to catch up, and whether that fits the ingest ceiling), the **webhook backpressure**
//! (a dead endpoint's cursor falls behind, but nothing grows in memory), and the **nightly
//! reconciliation** (the missing-id diff a store re-pushes). Each is integer and deterministic; the
//! real sustained soak that measures the throughput itself stays a hardware/ops handoff.

use std::collections::BTreeSet;

use crate::capacity::EVENTS_PER_BILL;

/// A conservative sustained ingest rate, events/second — the low end of the §2 ceiling (2,000–5,000/s).
/// Used to check that a drain deadline is feasible without claiming the upper end.
pub const SUSTAINED_INGEST_PER_SECOND: i64 = 2_000;

/// Integer ceiling division; `0` when the divisor is not positive, so the model is panic-free.
#[must_use]
fn ceil_div(numerator: i64, divisor: i64) -> i64 {
    if divisor <= 0 {
        0
    } else {
        (numerator + divisor - 1) / divisor
    }
}

/// Events buffered in store outboxes while `stores` are offline for `days`, at `bills_per_store` bills
/// a day — `stores × bills_per_store × `[`EVENTS_PER_BILL`]` × days`.
#[must_use]
pub const fn buffered_events(stores: i64, bills_per_store: i64, days: i64) -> i64 {
    stores * bills_per_store * EVENTS_PER_BILL * days
}

/// How long, in seconds, `buffered` events take to drain at `drain_rate` events/second.
#[must_use]
pub fn drain_seconds(buffered: i64, drain_rate_per_second: i64) -> i64 {
    ceil_div(buffered, drain_rate_per_second)
}

/// The sustained rate, events/second, needed to drain `buffered` events within `deadline_seconds`.
#[must_use]
pub fn required_drain_rate(buffered: i64, deadline_seconds: i64) -> i64 {
    ceil_div(buffered, deadline_seconds)
}

/// A webhook endpoint that has fallen behind — the model of ADR-0032's cursor-over-the-log delivery.
///
/// The point of the cursor design is that a dead endpoint costs a growing *lag* but a bounded
/// *footprint*: delivery re-reads the durable log from the cursor a batch at a time, so nothing
/// accumulates in memory no matter how far behind it falls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebhookBacklog {
    /// Events the tenant produces per hour.
    pub events_per_hour: i64,
    /// The delivery batch size — the most the sender ever holds at once.
    pub batch_size: i64,
}

impl WebhookBacklog {
    /// How far the cursor has fallen behind after `dead_hours` — grows linearly with the outage.
    #[must_use]
    pub const fn cursor_lag(&self, dead_hours: i64) -> i64 {
        self.events_per_hour * dead_hours
    }

    /// The in-memory footprint — one batch, whatever the lag. This is the property that matters:
    /// independent of `dead_hours`, so a 24-hour outage costs no more memory than a one-hour one.
    #[must_use]
    pub const fn in_memory_events(&self) -> i64 {
        self.batch_size
    }
}

/// The nightly reconciliation diff: the ids a store holds that the cloud has not received, which the
/// store is asked to re-push (`docs/roadmap.md` P7/P12). A sorted set difference `store − cloud`.
#[must_use]
pub fn missing_ids<T: Ord + Copy>(store_ids: &[T], cloud_ids: &[T]) -> Vec<T> {
    let store: BTreeSet<T> = store_ids.iter().copied().collect();
    let cloud: BTreeSet<T> = cloud_ids.iter().copied().collect();
    store.difference(&cloud).copied().collect()
}

#[cfg(test)]
mod tests {
    use super::{
        SUSTAINED_INGEST_PER_SECOND, WebhookBacklog, buffered_events, drain_seconds, missing_ids,
        required_drain_rate,
    };

    #[test]
    fn two_hundred_stores_offline_a_day_buffer_the_published_eight_hundred_thousand() {
        // Reproduces §4's "800k events" from the fleet model rather than restating it: 200 stores at
        // scenario B's 500 bills, one day.
        assert_eq!(buffered_events(200, 500, 1), 800_000);
    }

    #[test]
    fn the_nine_minute_drain_is_feasible_within_the_ingest_ceiling() {
        // §4 states 800k drains in ~9 minutes. The rate that implies (≈1,482/s) sits under the
        // conservative ingest ceiling, so the claim is feasible — and at the ceiling the same backlog
        // clears faster than 9 minutes, so the published figure is conservative.
        let buffered = buffered_events(200, 500, 1);
        let nine_minutes = 9 * 60;
        assert!(
            required_drain_rate(buffered, nine_minutes) <= SUSTAINED_INGEST_PER_SECOND,
            "the rate needed to hit 9 minutes must be within the ingest ceiling"
        );
        assert!(
            drain_seconds(buffered, SUSTAINED_INGEST_PER_SECOND) <= nine_minutes,
            "at the ceiling the backlog clears within the published 9 minutes"
        );
    }

    #[test]
    fn a_dead_webhook_falls_behind_without_growing_in_memory() {
        // Scenario B produces ~4M events/day ≈ 166,667/hour; deliver in batches of 1,000.
        let backlog = WebhookBacklog {
            events_per_hour: 166_667,
            batch_size: 1_000,
        };
        // The lag grows with the outage...
        assert!(backlog.cursor_lag(1) < backlog.cursor_lag(24));
        assert!(backlog.cursor_lag(24) < backlog.cursor_lag(168));
        // ...but the footprint does not: one batch, whatever the outage length.
        for dead_hours in [1_i64, 24, 168] {
            assert_eq!(
                backlog.in_memory_events(),
                1_000,
                "in-memory footprint must not grow with a {dead_hours}-hour outage"
            );
        }
    }

    #[test]
    fn reconciliation_returns_exactly_the_ids_the_cloud_is_missing() {
        let store: Vec<u64> = (1..=10).collect();
        let cloud: Vec<u64> = vec![1, 2, 4, 5, 7, 8, 10];
        assert_eq!(missing_ids(&store, &cloud), vec![3, 6, 9]);
    }

    #[test]
    fn reconciliation_is_empty_when_the_cloud_has_everything() {
        let store: Vec<u64> = (1..=10).collect();
        assert!(missing_ids(&store, &store).is_empty());
    }
}
