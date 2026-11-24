// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store issues its deferred e-invoices on its own clock
//! ([ADR-0229](../../../docs/adr/0229-the-e-invoice-issue-moment-is-a-schedule.md) decision 4).
//!
//! Every minute the edge issues whatever has fallen due, each taking the next number of its symbol
//! then. Nothing here needs the cloud: a store offline at its batch window issues on time, and the
//! cloud submits when the events arrive. A tick that fails is logged and tried at the next one.

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
                _instant = ticks.tick() => match edge.issue_due_invoices().await {
                    // A count only: no bill or buyer is named in a log.
                    Ok(0) => {}
                    Ok(issued) => tracing::info!(issued, "issued deferred e-invoices"),
                    Err(error) => tracing::warn!(%error, "issuing deferred e-invoices failed; retrying next tick"),
                },
            }
        }
    });
}
