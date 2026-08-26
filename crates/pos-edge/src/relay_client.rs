// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store-side order relay client — pull the cloud queue, make each order, ack the outcome
//! ([ADR-0061](../../../docs/adr/0061-order-relay.md)).
//!
//! The cloud parks a guest QR order, a marketplace order, or a `POST /v1/orders` on a durable
//! per-store queue and waits for the store to report what became of it. This is the store half: a
//! thin loop that long-polls `GET /sync/stores/{id}/orders`, feeds each pulled order through the
//! local [`OrderIn`] (which reprices, opens it in the store's log, and dedupes on the caller's
//! reference — ADR-0064), and posts the outcome back to
//! `POST /sync/stores/{id}/orders/{queued_id}/ack`. The queue is at-least-once and intake is
//! idempotent, so a redelivered order converges on one order in the kitchen.
//!
//! The HTTP itself is a seam ([`RelayTransport`]): the loop is pure control flow over "pull a batch"
//! and "ack one", so every branch — a malformed payload, a store refusal, a transport error — is a
//! test with no socket. The wire shapes mirror the cloud's ([`crate::relay_client`] re-declares them
//! rather than depend on `pos-cloud`, which the edge must not); a round-trip test pins them to the
//! cloud's JSON.

use core::future::Future;
use core::time::Duration;

use serde::{Deserialize, Serialize};

use pos_ports::PortError;
use pos_ports::order_in::{
    ExternalReference, InboundOrder, InboundOrderLine, OrderAcceptance, OrderIn,
};
use pos_proto::error::ErrorStatus;
use pos_proto::ids::{MenuItemId, StoreId, SubjectId, TableId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::text::GuestNote;
use pos_proto::wire_enum::Open;
use pos_proto::{Quantity, SalesChannel, Timestamp};

// ---------------------------------------------------------------------------------------------
// Wire shapes — mirror `pos_cloud::relay` exactly (the edge must not depend on the cloud crate).
// ---------------------------------------------------------------------------------------------

/// One pending order in the pull response: the ack handle and the order to make.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PendingOrderDto {
    /// The opaque ack handle the store echoes back on ack.
    pub queued_id: String,
    /// The order payload.
    pub order: QueuedOrderPayload,
}

/// A queued order's payload, the store pulls. Mirrors `pos_cloud::relay::QueuedOrderPayload`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct QueuedOrderPayload {
    /// The caller's reference — the idempotency key, scoped by channel.
    pub external_reference: String,
    /// The channel the order arrived on (an open enum's wire token).
    pub sales_channel: String,
    /// The store to make it.
    pub store_id: String,
    /// The table, for a QR order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_id: Option<String>,
    /// The recipient side-table id, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    /// The requested lines.
    pub lines: Vec<QueuedOrderLine>,
    /// When the caller says it was placed (ms since the Unix epoch).
    pub placed_at_ms: i64,
}

/// One line of a [`QueuedOrderPayload`].
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct QueuedOrderLine {
    /// The menu item ordered.
    pub menu_item_id: String,
    /// How many, in thousandths.
    pub quantity_milli: i64,
    /// Chosen modifiers.
    #[serde(default)]
    pub modifier_menu_item_ids: Vec<String>,
    /// The price the caller quoted, advisory only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quoted_unit_price: Option<MoneyPayload>,
    /// The guest's free-text note; stays at the store, never the event log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A money value on the wire.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct MoneyPayload {
    /// ISO 4217 currency code.
    pub currency_code: String,
    /// The amount in the currency's minor unit.
    pub amount_minor: i64,
}

/// A serializable [`OrderAcceptance`] — what the store reports on ack.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AcceptanceRecord {
    /// The store's order id.
    pub order_id: String,
    /// Whether the store's call created it (vs an idempotent repeat).
    pub created: bool,
    /// The queue number, for a channel that issues one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_number: Option<u32>,
    /// The store's authoritative total.
    pub total: MoneyPayload,
    /// Whether the store's price differed from the caller's quote.
    pub repriced: bool,
    /// Whether staff must confirm before the kitchen sees it.
    pub awaiting_staff_confirmation: bool,
}

impl AcceptanceRecord {
    /// The wire record of a domain [`OrderAcceptance`].
    fn from_acceptance(acceptance: &OrderAcceptance) -> Self {
        Self {
            order_id: acceptance.order_id.to_string(),
            created: acceptance.created,
            queue_number: acceptance.queue_number,
            total: MoneyPayload {
                currency_code: acceptance.total.currency_code.as_str().to_owned(),
                amount_minor: acceptance.total.amount_minor,
            },
            repriced: acceptance.repriced,
            awaiting_staff_confirmation: acceptance.awaiting_staff_confirmation,
        }
    }
}

/// What the store reports for a pulled order. Mirrors `pos_cloud::relay::StoreOutcome`, including its
/// `#[serde(tag = "outcome", rename_all = "snake_case")]` shape, so the cloud parses it unchanged.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum StoreOutcome {
    /// The store accepted (or idempotently matched) the order.
    Accepted(AcceptanceRecord),
    /// The store refused it. `status` is the [`ErrorStatus`] wire class the relay's own parser reads.
    Rejected {
        /// The refusal class (`invalid_argument`, `failed_precondition`, …).
        status: String,
        /// A human-readable reason.
        message: String,
    },
}

impl StoreOutcome {
    /// The outcome for a submit result: an acceptance, or a refusal carrying the port error's class.
    fn from_result(result: Result<OrderAcceptance, PortError>) -> Self {
        match result {
            Ok(acceptance) => Self::Accepted(AcceptanceRecord::from_acceptance(&acceptance)),
            Err(error) => Self::Rejected {
                status: status_wire(error.status()).to_owned(),
                message: error.to_string(),
            },
        }
    }
}

/// The lowercase `snake_case` class the relay's `port_error_from_wire` recognises. Classes it does
/// not distinguish collapse to `failed_precondition` — a store's refusal is its own decision, a `409`.
fn status_wire(status: ErrorStatus) -> &'static str {
    match status {
        ErrorStatus::InvalidArgument => "invalid_argument",
        ErrorStatus::AlreadyExists => "already_exists",
        ErrorStatus::ResourceExhausted => "resource_exhausted",
        ErrorStatus::NotFound => "not_found",
        _ => "failed_precondition",
    }
}

// ---------------------------------------------------------------------------------------------
// The transport seam
// ---------------------------------------------------------------------------------------------

/// A failure of the relay transport itself — the cloud is unreachable, or answered unparseably.
/// Distinct from a store's *refusal* of an order, which is an ack, not an error.
#[derive(Debug, thiserror::Error)]
#[error("the order relay transport failed: {0}")]
pub struct RelayTransportError(String);

impl RelayTransportError {
    /// Wraps a reason (for the store's log — never a guest's personal data).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The HTTP the relay client rides — a long-poll pull and an ack post, to the store's own cloud.
///
/// A seam so the client's loop is testable without a socket; the field implementation is an HTTPS
/// client authenticated with the store's own scoped API key ([ADR-0054](../../../docs/adr/0054-cloud-sync-http.md)).
pub trait RelayTransport: Send + Sync {
    /// Long-polls the store's pending batch, oldest first. An empty vector means "nothing yet".
    ///
    /// # Errors
    ///
    /// [`RelayTransportError`] if the cloud could not be reached or its answer did not parse.
    fn pull(
        &self,
    ) -> impl Future<Output = Result<Vec<PendingOrderDto>, RelayTransportError>> + Send;

    /// Acks one pulled order by its handle, reporting the outcome. Idempotent on the cloud side.
    ///
    /// # Errors
    ///
    /// [`RelayTransportError`] if the ack could not be delivered.
    fn ack(
        &self,
        queued_id: &str,
        outcome: &StoreOutcome,
    ) -> impl Future<Output = Result<(), RelayTransportError>> + Send;
}

// ---------------------------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------------------------

/// How long the loop waits after a transport error before trying again — a store offline from the
/// cloud's view still trades locally, so this is a background reconnect, not a hot spin.
const RETRY_BACKOFF: Duration = Duration::from_secs(5);

/// The relay client: a transport to the cloud queue and the local [`OrderIn`] each pulled order is
/// made through. Static dispatch, no `dyn` ([ADR-0013](../../../docs/adr/0013-async-strategy.md)).
#[derive(Debug)]
pub struct RelayClient<T, X> {
    transport: T,
    intake: X,
}

impl<T, X> RelayClient<T, X>
where
    T: RelayTransport,
    X: OrderIn,
{
    /// Builds a client over a transport and the store's intake.
    pub const fn new(transport: T, intake: X) -> Self {
        Self { transport, intake }
    }

    /// Pulls one batch, makes and acks each order, and returns how many were processed. A malformed
    /// payload is acked as an `invalid_argument` refusal (the store cannot make what it cannot read),
    /// never dropped, so the cloud stops re-parking it; a store refusal is acked as its class.
    ///
    /// # Errors
    ///
    /// [`RelayTransportError`] only for a transport failure (the pull, or an ack, could not reach the
    /// cloud). An order the store refuses is an ack, not an error, and does not fail the batch.
    pub async fn pump_once(&self) -> Result<usize, RelayTransportError> {
        let pending = self.transport.pull().await?;
        let mut processed = 0;
        for entry in &pending {
            let outcome = match to_inbound_order(&entry.order) {
                Ok(order) => StoreOutcome::from_result(self.intake.submit(&order).await),
                Err(reason) => StoreOutcome::Rejected {
                    status: "invalid_argument".to_owned(),
                    message: reason.to_owned(),
                },
            };
            self.transport.ack(&entry.queued_id, &outcome).await?;
            processed += 1;
        }
        Ok(processed)
    }

    /// Runs the pull→make→ack loop until `shutdown` resolves. A transport error is logged and retried
    /// after a backoff rather than propagated: the store keeps trading locally while the cloud link is
    /// down, and drains the queue when it returns.
    pub async fn run<F>(self, shutdown: F)
    where
        F: Future<Output = ()> + Send,
    {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                result = self.pump_once() => {
                    match result {
                        Ok(count) if count > 0 => {
                            tracing::debug!(count, "relay client processed a batch");
                        }
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(%error, "relay pull/ack failed; backing off");
                            tokio::select! {
                                () = &mut shutdown => break,
                                () = tokio::time::sleep(RETRY_BACKOFF) => {}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Maps a pulled payload to a domain [`InboundOrder`], or the reason its first bad field makes it
/// unmakeable (acked as an `invalid_argument` refusal rather than retried forever).
fn to_inbound_order(payload: &QueuedOrderPayload) -> Result<InboundOrder, &'static str> {
    let external_reference = ExternalReference::parse(&payload.external_reference)
        .map_err(|_ignored| "external_reference must be non-empty and at most 128 bytes")?;
    let store_id = payload
        .store_id
        .parse::<StoreId>()
        .map_err(|_ignored| "store_id must be a ULID")?;
    let table_id = match &payload.table_id {
        Some(text) => Some(
            text.parse::<TableId>()
                .map_err(|_ignored| "table_id must be a ULID")?,
        ),
        None => None,
    };
    let subject_id = match &payload.subject_id {
        Some(text) => Some(
            text.parse::<SubjectId>()
                .map_err(|_ignored| "subject_id must be a ULID")?,
        ),
        None => None,
    };
    let placed_at = Timestamp::from_milliseconds_since_epoch(payload.placed_at_ms)
        .map_err(|_ignored| "placed_at_ms is out of range")?;
    let mut lines = Vec::with_capacity(payload.lines.len());
    for line in &payload.lines {
        lines.push(to_inbound_line(line)?);
    }
    Ok(InboundOrder {
        external_reference,
        sales_channel: Open::<SalesChannel>::parse(&payload.sales_channel),
        store_id,
        table_id,
        subject_id,
        lines,
        placed_at,
    })
}

/// Maps one pulled line to a domain [`InboundOrderLine`], or the reason it is unmakeable.
fn to_inbound_line(line: &QueuedOrderLine) -> Result<InboundOrderLine, &'static str> {
    let menu_item_id = line
        .menu_item_id
        .parse::<MenuItemId>()
        .map_err(|_ignored| "menu_item_id must be a ULID")?;
    let mut modifier_menu_item_ids = Vec::with_capacity(line.modifier_menu_item_ids.len());
    for modifier in &line.modifier_menu_item_ids {
        modifier_menu_item_ids.push(
            modifier
                .parse::<MenuItemId>()
                .map_err(|_ignored| "a modifier id must be a ULID")?,
        );
    }
    let quoted_unit_price = match &line.quoted_unit_price {
        Some(money) => Some(Money {
            currency_code: CurrencyCode::parse(&money.currency_code)
                .map_err(|_ignored| "currency_code must be three uppercase letters")?,
            amount_minor: money.amount_minor,
        }),
        None => None,
    };
    Ok(InboundOrderLine {
        menu_item_id,
        quantity: Quantity::from_milli(line.quantity_milli),
        modifier_menu_item_ids,
        quoted_unit_price,
        note: line.note.clone().map(GuestNote::new),
    })
}
