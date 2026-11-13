// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! When a delivery order's slip prints
//! ([ADR-0215](../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decision 3): once,
//! when the store takes the order. The relay tells [`DeliverySlips`] of each new order the store
//! takes without waiting for staff; the confirm route prints it when staff confirm one that waited.
//! An order with no recipient, or one already masked, prints nothing. The log names the order and
//! what came of the paper, never the person.

use std::sync::Arc;

use pos_core::business_date::local_time;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::ClockSource;
use pos_proto::ids::OrderId;

use crate::app::Edge;
use crate::clock::SystemClock;
use crate::printing::{PrintOutcome, Printers};
use crate::relay_client::TakenOrders;

/// Prints the slips of the orders the relay takes.
#[derive(Debug)]
pub struct DeliverySlips<S> {
    edge: Arc<Edge<S>>,
    printers: Arc<Printers>,
}

impl<S> DeliverySlips<S> {
    /// Slips for `edge`'s orders, on `printers`.
    pub const fn new(edge: Arc<Edge<S>>, printers: Arc<Printers>) -> Self {
        Self { edge, printers }
    }
}

impl<S> TakenOrders for DeliverySlips<S>
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    async fn taken(&self, order_id: OrderId) {
        let _outcome = print_delivery_slip(&self.printers, &self.edge, order_id).await;
    }
}

/// Prints `order_id`'s delivery slip, stamped with the store's local time, and logs what came of
/// it. `None` when the order has no recipient to print.
pub async fn print_delivery_slip<S>(
    printers: &Printers,
    edge: &Edge<S>,
    order_id: OrderId,
) -> Option<PrintOutcome>
where
    S: EventStore + SubjectStore,
{
    let now = SystemClock.now();
    let printed_at = local_time(now, &edge.session().timezone)
        .map_or_else(|_| now.to_string(), |local| local.to_string());
    let outcome = printers
        .print_delivery_slip(edge, order_id, &printed_at)
        .await?;
    tracing::info!(%order_id, print = outcome.as_wire(), "a delivery slip was sent to print");
    Some(outcome)
}
