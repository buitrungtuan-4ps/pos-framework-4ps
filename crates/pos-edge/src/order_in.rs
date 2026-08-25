// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store-side [`OrderIn`] ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)).
//!
//! [ADR-0026](../../../docs/adr/0026-port-shapes.md) §5 makes the edge the real `OrderIn`: it reprices
//! from its own menu, opens the order in its local log, and accepts **offline**. This is that
//! implementor. It is a thin driving-port adapter over [`Edge`]: `submit` reprices each line against
//! the session's [`MenuCatalog`](pos_proto::menu::MenuCatalog) (ADR-0063), opens the order through
//! [`Edge::open_inbound_order`], and records the acceptance in an idempotency ledger keyed by the
//! caller's `(sales_channel, external_reference)` — so a marketplace's retry, or the relay's
//! at-least-once delivery, converge on one order in the kitchen.
//!
//! The relay client ([ADR-0061](../../../docs/adr/0061-order-relay.md)) is the production caller; a
//! guest QR order and a `POST /v1/orders` reach the same path.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use pos_core::menu::{PricedLine, RepriceError, RequestedLine, reprice_line};
use pos_ports::event_store::EventStore;
use pos_ports::order_in::{ExternalReference, InboundOrder, OrderAcceptance, OrderIn};
use pos_ports::{PortError, PortName};
use pos_proto::ids::DeviceId;
use pos_proto::money::Money;
use pos_proto::wire_enum::Open;
use pos_proto::{SalesChannel, StoreId};

use crate::app::{AppError, Edge};
use crate::queue::QueueNumberAuthority;

/// The durable, per-store record of what a caller's reference produced — the idempotency source of
/// truth ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)). Keyed by the channel's wire token and
/// the caller's reference, exactly as the cloud relay keys its queue. Backed by `store-sqlite` in the
/// binary (the follow-up commit) and an in-memory map in tests and the example — the same split as
/// [`ReceiptAuthority`](crate::receipt::ReceiptAuthority).
pub trait IntakeLedger: Send + Sync {
    /// The acceptance a reference already produced, or `None` if this is the first time.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the ledger could not be read.
    fn lookup(
        &self,
        sales_channel: &str,
        external_reference: &str,
    ) -> impl Future<Output = Result<Option<OrderAcceptance>, PortError>> + Send;

    /// Records the acceptance for a reference. Insert-if-absent: a racing second writer keeps the
    /// first record rather than overwriting it.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the ledger could not be written.
    fn record(
        &self,
        sales_channel: &str,
        external_reference: &str,
        acceptance: OrderAcceptance,
    ) -> impl Future<Output = Result<(), PortError>> + Send;
}

/// An in-memory [`IntakeLedger`] — the tests-and-example implementation. Not durable across a
/// restart; the SQLite implementation (written in the order's own transaction) is the production one.
#[derive(Debug, Clone, Default)]
pub struct InMemoryIntakeLedger {
    inner: Arc<Mutex<BTreeMap<(String, String), OrderAcceptance>>>,
}

impl InMemoryIntakeLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl IntakeLedger for InMemoryIntakeLedger {
    async fn lookup(
        &self,
        sales_channel: &str,
        external_reference: &str,
    ) -> Result<Option<OrderAcceptance>, PortError> {
        let guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(guard
            .get(&(sales_channel.to_owned(), external_reference.to_owned()))
            .cloned())
    }

    async fn record(
        &self,
        sales_channel: &str,
        external_reference: &str,
        acceptance: OrderAcceptance,
    ) -> Result<(), PortError> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        guard
            .entry((sales_channel.to_owned(), external_reference.to_owned()))
            .or_insert(acceptance);
        Ok(())
    }
}

/// The edge's [`OrderIn`]: reprice from the store's menu, open the order in the local log, dedupe on
/// the caller's reference, and hand a tableless order its daily queue number. Generic over the store
/// `S`, the ledger `L`, and the queue authority `Q` — static dispatch, no `dyn`
/// ([ADR-0013](../../../docs/adr/0013-async-strategy.md)).
#[derive(Debug)]
pub struct EdgeOrderIn<S, L, Q> {
    edge: Arc<Edge<S>>,
    ledger: L,
    queue: Q,
    device_id: DeviceId,
}

impl<S, L, Q> EdgeOrderIn<S, L, Q> {
    /// Builds the intake over an edge, an idempotency ledger, a queue-number authority, and the box's
    /// own device id (the events an inbound order writes carry it, since there is no signed-in
    /// employee). In the field the ledger and the authority are both the one
    /// [`SqliteStore`](store_sqlite::SqliteStore); the tests and the example pass the in-memory pair.
    pub const fn new(edge: Arc<Edge<S>>, ledger: L, queue: Q, device_id: DeviceId) -> Self {
        Self {
            edge,
            ledger,
            queue,
            device_id,
        }
    }
}

impl<S, L, Q> OrderIn for EdgeOrderIn<S, L, Q>
where
    S: EventStore + Send + Sync,
    L: IntakeLedger,
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
        if let Some(existing) = self.ledger.lookup(channel_token, reference).await? {
            return Ok(OrderAcceptance {
                created: false,
                ..existing
            });
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

        // Open the order in one transaction: `sales.order.opened` + a line per priced line. The
        // business date it was stamped with keys the queue number below.
        let (order_id, business_date) = self
            .edge
            .open_inbound_order(
                self.device_id,
                order.sales_channel.clone(),
                order.table_id,
                &priced_lines,
            )
            .await
            .map_err(port_error_from_app)?;

        // A tableless order (takeaway / delivery / public API) is called back by a daily queue
        // number; a QR order names a table and is served there, so it gets none. The authority is
        // durable and idempotent by order, so a retry that got past the ledger still yields one
        // number, not two (ADR-0064).
        let queue_number = if order.table_id.is_none() {
            let number = self
                .queue
                .allocate_queue_number(order.store_id, business_date, order_id)
                .await?;
            Some(u32::try_from(number).unwrap_or(u32::MAX))
        } else {
            None
        };

        let acceptance = OrderAcceptance {
            order_id,
            created: true,
            queue_number,
            total,
            repriced,
            // A QR order (one that names a table) waits for staff before the kitchen sees it
            // (ADR-0057); a delivery or public-API order is already committed by its channel.
            awaiting_staff_confirmation: order.table_id.is_some(),
        };
        self.ledger
            .record(channel_token, reference, acceptance.clone())
            .await?;
        Ok(acceptance)
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
        self.ledger
            .lookup(sales_channel.as_wire(), external_reference.as_str())
            .await
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
