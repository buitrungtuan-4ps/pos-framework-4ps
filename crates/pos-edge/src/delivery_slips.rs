// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a store does when it takes a delivery order
//! ([ADR-0215](../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decisions 3 to 5): it
//! prints the delivery slip, once, and accepts the order with its vendor. The relay tells
//! [`DeliverySlips`] of each new order the store takes without waiting for staff; the confirm route
//! does both when staff confirm one that waited ([`confirmed`]). Refusing an order rejects it with
//! its vendor ([`refused`]), and the bump that makes its last line says it is ready ([`made`]).
//! An order with no recipient, or one already masked, prints nothing; an order from no vendor in
//! the `vendors` node answers no one. The log names the order and what came of the paper, never
//! the person.

use std::sync::Arc;

use pos_core::business_date::local_time;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::ClockSource;
use pos_proto::ids::{OrderId, ReasonCodeId};
use pos_proto::vendors::PublishedVendor;

use crate::app::Edge;
use crate::clock::SystemClock;
use crate::delivery_answers::{Answer, AnswerQueue, DeliveryAnswers};
use crate::printing::{PrintOutcome, Printers};
use crate::relay_client::TakenOrders;

/// Where a store's delivery effects go: its printers, and the queue its answers to vendors wait on.
#[derive(Debug)]
pub struct Delivery {
    /// The printers a slip prints on.
    pub printers: Arc<Printers>,
    /// Where an answer is handed over.
    pub answers: DeliveryAnswers,
    /// The queue the cloud loop drains.
    pub queue: AnswerQueue,
}

impl Delivery {
    /// The effects over `printers` and one answer queue's two ends.
    #[must_use]
    pub const fn new(
        printers: Arc<Printers>,
        answers: DeliveryAnswers,
        queue: AnswerQueue,
    ) -> Self {
        Self {
            printers,
            answers,
            queue,
        }
    }
}

/// Prints the slips of the orders the relay takes, and accepts them with their vendors.
#[derive(Debug)]
pub struct DeliverySlips<S> {
    edge: Arc<Edge<S>>,
    printers: Arc<Printers>,
    answers: DeliveryAnswers,
}

impl<S> DeliverySlips<S> {
    /// Slips for `edge`'s orders, on `printers`, and answers through `answers`.
    pub const fn new(
        edge: Arc<Edge<S>>,
        printers: Arc<Printers>,
        answers: DeliveryAnswers,
    ) -> Self {
        Self {
            edge,
            printers,
            answers,
        }
    }
}

impl<S> TakenOrders for DeliverySlips<S>
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    async fn taken(&self, order_id: OrderId) {
        confirmed(
            Some(&self.printers),
            Some(&self.answers),
            &self.edge,
            order_id,
        )
        .await;
    }
}

/// The store has taken `order_id`, at once or because staff confirmed it: print its slip and
/// accept it with its vendor, where it has either.
pub async fn confirmed<S>(
    printers: Option<&Printers>,
    answers: Option<&DeliveryAnswers>,
    edge: &Edge<S>,
    order_id: OrderId,
) where
    S: EventStore + SubjectStore,
{
    if let Some(printers) = printers {
        let _outcome = print_delivery_slip(printers, edge, order_id).await;
    }
    answer(answers, edge, order_id, Answer::Accept);
}

/// Staff refused `order_id`, citing `reason`: reject it with `vendor`, the order's vendor as it was
/// read before the refusal closed the order.
pub fn refused(
    answers: Option<&DeliveryAnswers>,
    vendor: Option<&PublishedVendor>,
    order_id: OrderId,
    reason: ReasonCodeId,
) {
    if let (Some(answers), Some(vendor)) = (answers, vendor) {
        answers.send(vendor, order_id, Answer::Reject(reason));
    }
}

/// A bump has made the last line of `order_id`: tell its vendor it is ready.
pub fn made<S: EventStore>(answers: Option<&DeliveryAnswers>, edge: &Edge<S>, order_id: OrderId) {
    answer(answers, edge, order_id, Answer::Ready);
}

/// Hands `kind` about `order_id` to the queue, when the order came through a vendor the store
/// takes.
fn answer<S: EventStore>(
    answers: Option<&DeliveryAnswers>,
    edge: &Edge<S>,
    order_id: OrderId,
    kind: Answer,
) {
    if let (Some(answers), Some(vendor)) = (answers, edge.order_vendor(order_id)) {
        answers.send(&vendor, order_id, kind);
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
