// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store issues its deferred e-invoices on its own clock
//! ([ADR-0229](../../../docs/adr/0229-the-e-invoice-issue-moment-is-a-schedule.md) decision 4).
//!
//! Every minute the edge issues whatever has fallen due, each taking the next number of its symbol
//! then. Nothing here needs the cloud: a store offline at its batch window issues on time, and the
//! cloud submits when the events arrive. A tick that fails is logged and tried at the next one. The
//! same tick records each correction decision HQ published for this store (ADR-0250).

use std::sync::Arc;
use std::time::Duration;

use pos_ports::event_store::EventStore;

use crate::app::Edge;

/// How often the store looks for invoices that have fallen due.
pub const ISSUE_TICK: Duration = Duration::from_secs(60);

/// Spawns the loop until `shutdown_rx` says stop.
pub fn spawn_invoice_issuing<S>(
    edge: &Arc<Edge<S>>,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) where
    S: EventStore + Send + Sync + 'static,
{
    let edge = Arc::clone(edge);
    let mut shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(ISSUE_TICK);
        // The first tick fires at once, so a box that was off past its window issues at boot.
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _instant = ticks.tick() => tick(&edge).await,
            }
        }
    });
}

/// One tick: the deferred invoices that fell due, the vendor's numbers for invoices issued "number
/// pending" (ADR-0251), then HQ's correction decisions (ADR-0250), each
/// recorded once whether it arrived before a restart or after. Counts only: no bill or buyer is
/// named in a log.
async fn tick<S: EventStore>(edge: &Edge<S>) {
    match edge.issue_due_invoices().await {
        Ok(0) => {}
        Ok(issued) => tracing::info!(issued, "issued deferred e-invoices"),
        Err(error) => {
            tracing::warn!(%error, "issuing deferred e-invoices failed; retrying next tick");
        }
    }
    match edge.record_vendor_numbers().await {
        Ok(0) => {}
        Ok(recorded) => tracing::info!(recorded, "recorded the vendor's e-invoice numbers"),
        Err(error) => {
            tracing::warn!(%error, "recording the vendor's e-invoice numbers failed; retrying next tick");
        }
    }
    match edge.record_correction_decisions().await {
        Ok(0) => {}
        Ok(recorded) => tracing::info!(recorded, "recorded HQ's correction decisions"),
        Err(error) => {
            tracing::warn!(%error, "recording HQ's correction decisions failed; retrying next tick");
        }
    }
}
