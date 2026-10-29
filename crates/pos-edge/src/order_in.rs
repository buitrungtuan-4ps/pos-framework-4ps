// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store-side [`OrderIn`] ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)).
//!
//! [ADR-0026](../../../docs/adr/0026-port-shapes.md) §5 makes the edge the real `OrderIn`: it reprices
//! from its own menu, opens the order in its local log, and accepts **offline**. This is that
//! implementor. It is a thin driving-port adapter over [`Edge`]: `submit` reprices each line against
//! the session's [`MenuCatalog`](pos_proto::menu::MenuCatalog) (ADR-0063), opens the order through
//! [`Edge::open_inbound_order`], and — in that order's **own transaction** — records the acceptance
//! in the durable [`IntakeLedger`] keyed by the caller's `(sales_channel, external_reference)`. So a
//! marketplace's retry, the relay's at-least-once redelivery, or a crash mid-open all converge on
//! one order in the kitchen.
//!
//! The relay client ([ADR-0061](../../../docs/adr/0061-order-relay.md)) is the production caller; a
//! guest QR order and a `POST /v1/orders` reach the same path.

use std::sync::Arc;

use pos_core::capability::Capability;
use pos_core::error::DomainError;
use pos_core::menu::{PricedLine, RepriceError, RequestedLine, reprice_line};
use pos_ports::event_store::EventStore;
use pos_ports::intake_ledger::{IntakeLedger, IntakeRecord};
use pos_ports::order_in::{ExternalReference, InboundOrder, OrderAcceptance, OrderIn};
use pos_ports::{PortError, PortName};
use pos_proto::error::ErrorStatus;
use pos_proto::ids::{DeviceId, OrderId};
use pos_proto::money::Money;
use pos_proto::time::BusinessDate;
use pos_proto::wire_enum::Open;
use pos_proto::{SalesChannel, StoreId};

use crate::app::{AppError, Edge, EdgeSession, InboundOrderOpened, IntakeIntent};
use crate::line_notes::NoteText;
use crate::queue::QueueNumberAuthority;

/// The edge's [`OrderIn`]: reprice from the store's menu, open the order in the local log, dedupe on
/// the caller's reference through the store's durable ledger, and hand a tableless order its daily
/// queue number. Generic over the store `S` (which supplies both the event log and the idempotency
/// ledger, so the two share one transaction) and the queue authority `Q` — static dispatch, no `dyn`
/// ([ADR-0013](../../../docs/adr/0013-async-strategy.md)).
#[derive(Debug)]
pub struct EdgeOrderIn<S, Q> {
    edge: Arc<Edge<S>>,
    queue: Q,
    device_id: DeviceId,
}

impl<S, Q> EdgeOrderIn<S, Q> {
    /// Builds the intake over an edge, a queue-number authority, and the box's own device id (the
    /// events an inbound order writes carry it, since there is no signed-in employee). In the field
    /// the store and the authority are both the one [`SqliteStore`](store_sqlite::SqliteStore) — so
    /// the ledger row lands in the order's transaction and the queue number survives a restart; the
    /// tests and the example pass the fake store and the in-memory queue authority.
    pub const fn new(edge: Arc<Edge<S>>, queue: Q, device_id: DeviceId) -> Self {
        Self {
            edge,
            queue,
            device_id,
        }
    }
}

impl<S, Q> EdgeOrderIn<S, Q>
where
    S: EventStore + IntakeLedger + Send + Sync,
    Q: QueueNumberAuthority,
{
    /// Rebuilds the acceptance a repeat is owed from its stored record. The queue number is
    /// reconstructed here rather than stored: the authority is idempotent by order, so a tableless
    /// order gets the same number back (and a crash that opened the order but never numbered it gets
    /// one now), while a QR order gets none.
    async fn acceptance_from_record(
        &self,
        store_id: StoreId,
        record: &IntakeRecord,
        created: bool,
    ) -> Result<OrderAcceptance, PortError> {
        // Ask the authority what this order WAS given, rather than inferring it from the hold.
        //
        // This used to read `if record.awaiting_staff_confirmation { None } else { allocate() }`,
        // which conflated two different questions and only worked by accident: the stored flag was
        // `table_id.is_some()`, so it happened to answer "does this order get a number" correctly.
        // The moment that flag became the real hold — false for a table order at a store that
        // switched the hold off — the else-branch would have ALLOCATED, putting a queue number on
        // a floor order that must never have one. `allocate_queue_number` is idempotent by order,
        // so it would not have failed; it would have quietly minted one. `queue_number_for`'s own
        // doc comment names this exact hazard, and this is the read path it was built for.
        let queue_number = self
            .queue
            .queue_number_for(store_id, record.order_id)
            .await?
            .map(|number| u32::try_from(number).unwrap_or(u32::MAX));
        Ok(OrderAcceptance {
            order_id: record.order_id,
            created,
            queue_number,
            total: record.total,
            repriced: record.repriced,
            awaiting_staff_confirmation: record.awaiting_staff_confirmation,
        })
    }
    /// Reprices each line against the store's menu for `channel`, refusing an item staff marked
    /// sold out, and sums what the order owes.
    fn price_lines(
        &self,
        session: &EdgeSession,
        channel: SalesChannel,
        order: &InboundOrder,
    ) -> Result<PricedOrder, PortError> {
        let mut priced_lines: Vec<(PricedLine, Option<NoteText>)> =
            Vec::with_capacity(order.lines.len());
        let mut total = Money::zero(session.currency);
        let mut repriced = false;
        for line in &order.lines {
            let requested = RequestedLine {
                menu_item_id: line.menu_item_id,
                quantity: line.quantity,
                modifier_menu_item_ids: line.modifier_menu_item_ids.clone(),
                quoted_unit_price: line.quoted_unit_price,
            };
            let priced = reprice_line(
                session.menu_for(channel),
                &session.tax_rates,
                channel,
                &requested,
            )
            .map_err(|error| port_error_from_reprice(&error))?;
            // An item staff marked sold out at the till is refused exactly as one the console
            // withdrew is: the menu the guest or the marketplace ordered from was published before
            // the kitchen ran out, and does not know.
            if let Some(sold_out) = self
                .edge
                .first_sold_out(line.menu_item_id, &line.modifier_menu_item_ids)
            {
                return Err(port_error_from_reprice(&RepriceError::Unavailable(
                    sold_out,
                )));
            }
            total = total.checked_add(priced.line_total).map_err(|_ignored| {
                PortError::internal(PortName::OrderIn, "the order total overflowed")
            })?;
            repriced |= priced.repriced;
            // The note is kept for the kitchen, in memory only (ADR-0157). It used to be dropped
            // here, so a delivery order's "no peanuts" reached the log as "a note existed" and the
            // kitchen never read it.
            let note = line
                .note
                .as_ref()
                .and_then(|note| NoteText::lenient(note.as_str()));
            priced_lines.push((priced, note));
        }
        Ok(PricedOrder {
            lines: priced_lines,
            total,
            repriced,
        })
    }

    /// The acceptance a repeat of `order`'s reference is owed, if one was already accepted.
    async fn recorded_acceptance(
        &self,
        order: &InboundOrder,
    ) -> Result<Option<OrderAcceptance>, PortError> {
        let channel_token = order.sales_channel.as_wire();
        let reference = order.external_reference.as_str();
        if let Some(record) = self
            .edge
            .look_up_intake(channel_token, reference)
            .await
            .map_err(port_error_from_app)?
        {
            return self
                .acceptance_from_record(order.store_id, &record, false)
                .await
                .map(Some);
        }
        Ok(None)
    }

    /// Opens the order and records the idempotency row in ONE transaction: `sales.order.opened` +
    /// a line per priced line + the ledger row (ADR-0064).
    async fn open(
        &self,
        order: &InboundOrder,
        priced: &PricedOrder,
    ) -> Result<InboundOrderOpened, AppError> {
        let intent = IntakeIntent {
            sales_channel: order.sales_channel.as_wire(),
            external_reference: order.external_reference.as_str(),
            total: priced.total,
            repriced: priced.repriced,
        };
        self.edge
            .open_inbound_order(
                self.device_id,
                order.sales_channel.clone(),
                order.table_id,
                &priced.lines,
                Some(intent),
            )
            .await
    }

    /// The acceptance another delivery of the same reference won the race with, whose record is
    /// now durable.
    async fn race_winner(&self, order: &InboundOrder) -> Result<OrderAcceptance, PortError> {
        let channel_token = order.sales_channel.as_wire();
        let reference = order.external_reference.as_str();
        let record = self
            .edge
            .look_up_intake(channel_token, reference)
            .await
            .map_err(port_error_from_app)?
            .ok_or_else(|| {
                PortError::internal(
                    PortName::OrderIn,
                    "an order exists for this reference but its record is missing",
                )
            })?;
        self.acceptance_from_record(order.store_id, &record, false)
            .await
    }

    /// A tableless order (takeaway / delivery / public API) is called back by a daily queue
    /// number; a QR order names a table and is served there, so it gets none. The authority is
    /// durable and idempotent by order (ADR-0064).
    async fn queue_number(
        &self,
        order: &InboundOrder,
        business_date: BusinessDate,
        order_id: OrderId,
    ) -> Result<Option<u32>, PortError> {
        if order.table_id.is_none() {
            let number = self
                .queue
                .allocate_queue_number(order.store_id, business_date, order_id)
                .await?;
            Ok(Some(u32::try_from(number).unwrap_or(u32::MAX)))
        } else {
            Ok(None)
        }
    }
}

impl<S, Q> OrderIn for EdgeOrderIn<S, Q>
where
    S: EventStore + IntakeLedger + Send + Sync,
    Q: QueueNumberAuthority,
{
    async fn submit(&self, order: &InboundOrder) -> Result<OrderAcceptance, PortError> {
        refuse_misaddressed(order, self.edge.store_id())?;
        // A repeat — the caller's retry, or the relay's at-least-once delivery — returns the recorded
        // acceptance and reports it did not create a second order.
        if let Some(repeat) = self.recorded_acceptance(order).await? {
            return Ok(repeat);
        }

        let session = self.edge.session();
        let channel = order.sales_channel.known();
        refuse_closed_channel(&session, channel)?;
        let priced = self.price_lines(&session, channel, order)?;

        // A concurrent second order on the same key loses the race at commit (`already_exists`) —
        // resolve it by returning the winner.
        let InboundOrderOpened {
            order_id,
            business_date,
            awaiting_staff_confirmation,
        } = match self.open(order, &priced).await {
            Ok(opened) => opened,
            Err(AppError::Port(error)) if error.status() == ErrorStatus::AlreadyExists => {
                return self.race_winner(order).await;
            }
            Err(other) => return Err(port_error_from_app(other)),
        };
        let queue_number = self.queue_number(order, business_date, order_id).await?;

        Ok(OrderAcceptance {
            order_id,
            created: true,
            queue_number,
            total: priced.total,
            repriced: priced.repriced,
            // Whether a QR order waits for staff (ADR-0057, and ADR-0080 M7 for the store's own
            // switch) was decided by `open_inbound_order` against the session snapshot this order
            // was priced on, and written to the ledger row in the same transaction. Taken from
            // there, not recomputed here: the two must be the same value, and asking twice is how
            // they came to differ.
            awaiting_staff_confirmation,
        })
    }

    async fn look_up(
        &self,
        store_id: StoreId,
        sales_channel: Open<SalesChannel>,
        external_reference: &ExternalReference,
    ) -> Result<Option<OrderAcceptance>, PortError> {
        if store_id != self.edge.store_id() {
            return Ok(None);
        }
        match self
            .edge
            .look_up_intake(sales_channel.as_wire(), external_reference.as_str())
            .await
            .map_err(port_error_from_app)?
        {
            Some(record) => Ok(Some(
                self.acceptance_from_record(store_id, &record, false)
                    .await?,
            )),
            None => Ok(None),
        }
    }
}

/// An inbound order's lines, repriced, with what the order owes and whether any price moved.
struct PricedOrder {
    lines: Vec<(PricedLine, Option<NoteText>)>,
    total: Money,
    repriced: bool,
}

/// Refuses an order with no lines, or one addressed to another store.
fn refuse_misaddressed(order: &InboundOrder, store_id: StoreId) -> Result<(), PortError> {
    if order.lines.is_empty() {
        return Err(PortError::invalid_argument(
            PortName::OrderIn,
            "an order must have at least one line",
        ));
    }
    if order.store_id != store_id {
        return Err(PortError::failed_precondition(
            PortName::OrderIn,
            "the order is addressed to another store",
        ));
    }
    Ok(())
}

/// Refuses an order on a channel the store does not take.
fn refuse_closed_channel(session: &EdgeSession, channel: SalesChannel) -> Result<(), PortError> {
    // The store must accept this sales channel (ADR-0080, M7). A store with no `channels` node
    // published has no restriction; once one is published, an order on a channel it does not list
    // is refused up front rather than priced and opened.
    if !session.channel_enabled(channel) {
        return Err(PortError::failed_precondition(
            PortName::OrderIn,
            "this store does not accept orders on that sales channel",
        ));
    }
    // QR ordering is one switch, the `qr_ordering_enabled` capability (ADR-0160 decision 5): a
    // QR order is refused where the store's configuration switches it off, whether the guest
    // page relayed it or a keyed caller sent it. A configuration that does not carry the switch
    // decides by the channel list above alone, as this did before the switch, so an edge that
    // updates before its cloud refuses nothing it took (`EdgeSession::qr_ordering_switched_off`).
    if channel == SalesChannel::Qr && session.qr_ordering_switched_off() {
        return Err(port_error_from_app(AppError::Domain(
            DomainError::CapabilityDisabled {
                capability: Capability::QrOrdering.meta().key,
            },
        )));
    }
    Ok(())
}

/// Maps a [`RepriceError`] to the `OrderIn` error class the contract fixes for it
/// ([ADR-0026](../../../docs/adr/0026-port-shapes.md) §5).
fn port_error_from_reprice(error: &RepriceError) -> PortError {
    match error {
        // Rule 3: an item the store does not sell is refused, never substituted.
        RepriceError::UnknownItem(id) => PortError::invalid_argument(
            PortName::OrderIn,
            format!("the store does not sell menu item {id}"),
        ),
        RepriceError::Unavailable(id) => PortError::failed_precondition(
            PortName::OrderIn,
            format!("menu item {id} is not available right now"),
        ),
        RepriceError::MissingRate { .. } => PortError::failed_precondition(
            PortName::OrderIn,
            "no tax rate is configured for that item on this channel",
        ),
        RepriceError::Money(_) => {
            PortError::internal(PortName::OrderIn, "the order line could not be priced")
        }
        // `RepriceError` is non-exhaustive; a variant this build has not learned is a server fault.
        _ => PortError::internal(PortName::OrderIn, "the order line could not be priced"),
    }
}

/// Maps an [`AppError`] from opening the order to a `PortError`.
fn port_error_from_app(error: AppError) -> PortError {
    match error {
        AppError::Port(port) => port,
        AppError::Domain(domain) => {
            PortError::failed_precondition(PortName::OrderIn, domain.to_string())
        }
        // A replaced machine leaves a channel order **queued for the replacement** (ADR-0123).
        // `unavailable` is the vocabulary for "not from here, ask again"; the catch-all below would
        // have made it `internal`, which reads as a bug in this store and invites the caller to
        // give up on an order a guest has already paid for.
        replaced @ AppError::Superseded => {
            PortError::unavailable(PortName::OrderIn, replaced.to_string())
        }
        other => PortError::internal(PortName::OrderIn, other.to_string()),
    }
}
