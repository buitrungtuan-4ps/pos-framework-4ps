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

use pos_core::menu::{PricedLine, RepriceError, RequestedLine, reprice_line};
use pos_ports::event_store::EventStore;
use pos_ports::intake_ledger::{IntakeLedger, IntakeRecord};
use pos_ports::order_in::{ExternalReference, InboundOrder, OrderAcceptance, OrderIn};
use pos_ports::{PortError, PortName};
use pos_proto::error::ErrorStatus;
use pos_proto::ids::DeviceId;
use pos_proto::money::Money;
use pos_proto::wire_enum::Open;
use pos_proto::{SalesChannel, StoreId};

use crate::app::{AppError, Edge, IntakeIntent};
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
        let queue_number = if record.awaiting_staff_confirmation {
            None
        } else {
            let number = self
                .queue
                .allocate_queue_number(store_id, record.business_date, record.order_id)
                .await?;
            Some(u32::try_from(number).unwrap_or(u32::MAX))
        };
        Ok(OrderAcceptance {
            order_id: record.order_id,
            created,
            queue_number,
            total: record.total,
            repriced: record.repriced,
            awaiting_staff_confirmation: record.awaiting_staff_confirmation,
        })
    }
}

impl<S, Q> OrderIn for EdgeOrderIn<S, Q>
where
    S: EventStore + IntakeLedger + Send + Sync,
    Q: QueueNumberAuthority,
{
    async fn submit(&self, order: &InboundOrder) -> Result<OrderAcceptance, PortError> {
        if order.lines.is_empty() {
            return Err(PortError::invalid_argument(
                PortName::OrderIn,
                "an order must have at least one line",
            ));
        }
        if order.store_id != self.edge.store_id() {
            return Err(PortError::failed_precondition(
                PortName::OrderIn,
                "the order is addressed to another store",
            ));
        }

        let channel_token = order.sales_channel.as_wire();
        let reference = order.external_reference.as_str();

        // A repeat — the caller's retry, or the relay's at-least-once delivery — returns the recorded
        // acceptance and reports it did not create a second order.
        if let Some(record) = self
            .edge
            .look_up_intake(channel_token, reference)
            .await
            .map_err(port_error_from_app)?
        {
            return self
                .acceptance_from_record(order.store_id, &record, false)
                .await;
        }

        let session = self.edge.session();
        let channel = order.sales_channel.known();
        let mut priced_lines: Vec<(PricedLine, bool)> = Vec::with_capacity(order.lines.len());
        let mut total = Money::zero(session.currency);
        let mut repriced = false;
        for line in &order.lines {
            let requested = RequestedLine {
                menu_item_id: line.menu_item_id,
                quantity: line.quantity,
                modifier_menu_item_ids: line.modifier_menu_item_ids.clone(),
                quoted_unit_price: line.quoted_unit_price,
            };
            let priced = reprice_line(&session.menu, &session.tax_rates, channel, &requested)
                .map_err(|error| port_error_from_reprice(&error))?;
            total = total.checked_add(priced.line_total).map_err(|_ignored| {
                PortError::internal(PortName::OrderIn, "the order total overflowed")
            })?;
            repriced |= priced.repriced;
            priced_lines.push((priced, line.note.is_some()));
        }

        // Open the order and record the idempotency row in ONE transaction: `sales.order.opened` +
        // a line per priced line + the ledger row (ADR-0064). A concurrent second order on the same
        // key loses the race at commit (`already_exists`) — resolve it by returning the winner.
        let intent = IntakeIntent {
            sales_channel: channel_token,
            external_reference: reference,
            total,
            repriced,
        };
        let (order_id, business_date) = match self
            .edge
            .open_inbound_order(
                self.device_id,
                order.sales_channel.clone(),
                order.table_id,
                &priced_lines,
                Some(intent),
            )
            .await
        {
            Ok(opened) => opened,
            Err(AppError::Port(error)) if error.status() == ErrorStatus::AlreadyExists => {
                // Another delivery of the same reference won the race and its record is now durable.
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
                return self
                    .acceptance_from_record(order.store_id, &record, false)
                    .await;
            }
            Err(other) => return Err(port_error_from_app(other)),
        };

        // A tableless order (takeaway / delivery / public API) is called back by a daily queue
        // number; a QR order names a table and is served there, so it gets none. The authority is
        // durable and idempotent by order (ADR-0064).
        let queue_number = if order.table_id.is_none() {
            let number = self
                .queue
                .allocate_queue_number(order.store_id, business_date, order_id)
                .await?;
            Some(u32::try_from(number).unwrap_or(u32::MAX))
        } else {
            None
        };

        Ok(OrderAcceptance {
            order_id,
            created: true,
            queue_number,
            total,
            repriced,
            // A QR order (one that names a table) waits for staff before the kitchen sees it
            // (ADR-0057); a delivery or public-API order is already committed by its channel.
            awaiting_staff_confirmation: order.table_id.is_some(),
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
        other => PortError::internal(PortName::OrderIn, other.to_string()),
    }
}
