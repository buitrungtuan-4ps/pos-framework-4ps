// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Retention of the event log
//! ([ADR-0145](../../../docs/adr/0145-the-edge-keeps-events-until-synced-and-n-days-old.md)).
//!
//! An event goes only when three things are true: the link has acknowledged it, it is older than
//! the store's `event_log_days`, and nothing still open began before it. The projection answers the
//! third ([`Edge::retention_horizon`]); the store answers the first and keeps its chain head
//! ([`SqliteStore::prune_events`]). This module only puts the two together, once an hour.

use core::time::Duration;
use std::sync::Arc;

use pos_ports::PortError;
use pos_proto::ClockSource as _;
use pos_proto::time::Timestamp;
use store_sqlite::SqliteStore;

use crate::app::Edge;
use crate::clock::SystemClock;

/// How often the sweep runs. An hour is far finer than a retention measured in days, and cheap:
/// a sweep with nothing to delete reads one outbox row and one chunk of the log.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// How long after boot the first sweep waits. The start-up chain walk runs beside the first
/// minutes of trading, chunk by chunk, and a delete landing between two of its chunks would look
/// to it like a record torn out of the middle of the log. Ten minutes is long past a year of log.
const FIRST_SWEEP_DELAY: Duration = Duration::from_secs(10 * 60);

/// One sweep: deletes what the store may forget as of `now`, and returns how many events went.
///
/// # Errors
///
/// [`PortError`] if the store cannot be reached. What was deleted before the failure stays
/// deleted, with its checkpoint, and the next sweep carries on from there.
pub async fn sweep_once(edge: &Edge<SqliteStore>, now: Timestamp) -> Result<u64, PortError> {
    let horizon = edge.retention_horizon(now);
    edge.store().prune_events(edge.store_id(), horizon).await
}

/// Runs [`sweep_once`] every hour, the first time ten minutes after boot, for as long as the
/// process lives. Logged, never fatal: a sweep that fails leaves the log as it was, which is the
/// safe direction.
pub fn spawn(edge: Arc<Edge<SqliteStore>>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_SWEEP_DELAY).await;
        let mut ticks = tokio::time::interval(SWEEP_INTERVAL);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            match sweep_once(&edge, SystemClock.now()).await {
                Ok(0) => tracing::debug!("event retention: nothing to delete"),
                Ok(deleted) => tracing::info!(deleted, "event retention deleted old synced events"),
                Err(error) => tracing::warn!(%error, "event retention could not run"),
            }
        }
    })
}
