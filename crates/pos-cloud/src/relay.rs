// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud→store order relay ([ADR-0061](../../../docs/adr/0061-order-relay.md)).
//!
//! [ADR-0056](../../../docs/adr/0056-public-order-intake.md) built the public intake and said the
//! binary's `OrderIn` is "the cloud→store relay"; this is it. The store's edge is the real
//! [`OrderIn`] implementor (it reprices, routes to the kitchen, and accepts offline); the cloud can
//! only get the order to the store and relay what the store decided, and it must do so **without**
//! pushing into the store — `docs/architecture.md` §3 keeps the store outbound-only so a 4G/CGNAT
//! box needs no port-forward.
//!
//! So the relay is a **durable per-store queue the store pulls**:
//!
//! * [`OrderRelay`] implements [`OrderIn`]. `submit` enqueues idempotently on
//!   `(tenant, store, channel, reference)` and then **parks** up to the store's configured deadline
//!   waiting for the store to report an acceptance — returning it (a `201`/`200`) if it arrives, or
//!   [`PortError::unavailable`] (a `503`) on timeout **with the order still queued**. `look_up` reads
//!   the recorded acceptance, which is the port's stated resolution path for a timed-out caller.
//! * The store, over its own outbound sync channel, pulls the pending batch
//!   (`GET /sync/stores/{id}/orders`, a bounded long-poll) and reports each outcome back
//!   (`POST /sync/stores/{id}/orders/{queued_id}/ack`) — [`orders_sync_router`], behind the
//!   deny-by-default [`Scope::RelayOrders`].
//!
//! The per-store `store.order_relay.{enabled,wait_ms}` knobs are read from the config tree
//! ([ADR-0033](../../../docs/adr/0033-config-tree.md)), so an operator turns intake on/off or tunes
//! the wait for one store from the dashboard, with no deploy.

use core::future::Future;
use core::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use pos_ports::PortError;
use pos_ports::order_in::{
    ExternalReference, InboundOrder, InboundOrderLine, OrderAcceptance, OrderIn,
};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{OrderId, StoreId, TenantId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::wire_enum::Open;
use pos_proto::{SalesChannel, Ulid};

use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope};
use crate::config_tree::{CapabilityValidator, ConfigTree, ConfigTreeStore};
use crate::http::{api_error, api_error_with_details};
use crate::orders::StoreDirectory;

/// The default the relay parks for when a store publishes no `store.order_relay.wait_ms`.
const DEFAULT_WAIT_MS: u64 = 3000;
/// How often `submit`'s park re-reads the queue while waiting for the store's ack.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long the store-facing long-poll holds an empty pull open before answering `[]`.
const LONGPOLL_CAP: Duration = Duration::from_secs(20);
/// How many pending orders a single pull returns.
const PULL_BATCH: u32 = 32;

// ---------------------------------------------------------------------------------------------
// The stored shapes
// ---------------------------------------------------------------------------------------------

/// The internal id of a queued order — the handle the store's ack path names. Distinct from the
/// caller's `(channel, reference)`, which stays the idempotency and look-up key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderQueueId(Ulid);

impl OrderQueueId {
    /// Wraps a ULID as a queued-order id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl core::fmt::Display for OrderQueueId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A queued order's payload, in a serializable wire form — what the queue stores and the store pulls.
/// The domain [`InboundOrder`] is not `Serialize` (its `GuestNote` cannot enter the event log), so
/// the relay carries this explicit shape; the guest note rides along to the store here (this is order
/// delivery, not the log) and never touches the event stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MoneyPayload {
    /// ISO 4217 currency code.
    pub currency_code: String,
    /// The amount in the currency's minor unit.
    pub amount_minor: i64,
}

impl QueuedOrderPayload {
    /// The wire form of a domain [`InboundOrder`], for enqueueing.
    #[must_use]
    pub fn from_inbound(order: &InboundOrder) -> Self {
        Self {
            external_reference: order.external_reference.as_str().to_owned(),
            sales_channel: order.sales_channel.as_wire().to_owned(),
            store_id: order.store_id.to_string(),
            table_id: order.table_id.map(|id| id.to_string()),
            subject_id: order.subject_id.map(|id| id.to_string()),
            lines: order
                .lines
                .iter()
                .map(QueuedOrderLine::from_inbound)
                .collect(),
            placed_at_ms: order.placed_at.as_milliseconds_since_epoch(),
        }
    }
}

impl QueuedOrderLine {
    fn from_inbound(line: &InboundOrderLine) -> Self {
        Self {
            menu_item_id: line.menu_item_id.to_string(),
            quantity_milli: line.quantity.as_milli(),
            modifier_menu_item_ids: line
                .modifier_menu_item_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
            quoted_unit_price: line.quoted_unit_price.map(|money| MoneyPayload {
                currency_code: money.currency_code.as_str().to_owned(),
                amount_minor: money.amount_minor,
            }),
            note: line.note.as_ref().map(|note| note.as_str().to_owned()),
        }
    }
}

/// A serializable [`OrderAcceptance`] — what the store reports and the relay records and replays.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// The record of a domain [`OrderAcceptance`].
    #[must_use]
    pub fn from_acceptance(acceptance: &OrderAcceptance) -> Self {
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

    /// Back to a domain [`OrderAcceptance`], or `None` if a stored field will not parse (a corrupt
    /// row — treated as no acceptance rather than a panic).
    #[must_use]
    pub fn to_acceptance(&self) -> Option<OrderAcceptance> {
        Some(OrderAcceptance {
            order_id: self.order_id.parse::<OrderId>().ok()?,
            created: self.created,
            queue_number: self.queue_number,
            total: Money {
                currency_code: CurrencyCode::parse(&self.total.currency_code).ok()?,
                amount_minor: self.total.amount_minor,
            },
            repriced: self.repriced,
            awaiting_staff_confirmation: self.awaiting_staff_confirmation,
        })
    }
}

/// What a store reported for a pulled order: an acceptance, or a refusal it decided locally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum StoreOutcome {
    /// The store accepted (or idempotently matched) the order.
    Accepted(AcceptanceRecord),
    /// The store refused it — an unknown item, a closed store, a rate limit. `status` is the
    /// [`pos_proto::error::ErrorStatus`] wire name so the caller sees the same class the port defines.
    Rejected {
        /// The refusal class (`invalid_argument`, `failed_precondition`, …).
        status: String,
        /// A human-readable reason.
        message: String,
    },
}

/// A queued order's current state, as the store persists it.
#[derive(Debug, Clone, PartialEq)]
pub enum OrderStatus {
    /// Enqueued, not yet reported by the store.
    Pending,
    /// The store reported an outcome.
    Reported(StoreOutcome),
}

/// A row of the order queue.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderRecord {
    /// The internal id (the store's ack handle).
    pub queued_id: OrderQueueId,
    /// The current state.
    pub status: OrderStatus,
}

/// One pending order handed to a pulling store.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingOrder {
    /// The ack handle.
    pub queued_id: OrderQueueId,
    /// The order to make.
    pub payload: QueuedOrderPayload,
}

// ---------------------------------------------------------------------------------------------
// The persistence seam
// ---------------------------------------------------------------------------------------------

/// The durable per-store order queue ([ADR-0061](../../../docs/adr/0061-order-relay.md)). One row per
/// `(tenant, store, sales_channel, external_reference)` — the port's idempotency key — so a resubmit
/// converges on one order. Backed by `store-postgres` (RLS-isolated by tenant) in the binary and an
/// in-memory fake in tests.
pub trait OrderQueueStore: Send + Sync {
    /// Inserts a pending order if `(tenant, store, channel, reference)` is new, else returns the
    /// existing row unchanged. Idempotent: the caller's retry, and the queue's own at-least-once
    /// delivery, both converge here.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the queue could not be read or written.
    fn enqueue(
        &self,
        tenant: TenantId,
        queued_id: OrderQueueId,
        payload: &QueuedOrderPayload,
    ) -> impl Future<Output = Result<OrderRecord, PortError>> + Send;

    /// The current row for `(tenant, store, channel, reference)`, or `None` if nothing was enqueued.
    /// Backs `submit`'s park poll and `look_up`.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the queue could not be read.
    fn outcome(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        sales_channel: &str,
        external_reference: &str,
    ) -> impl Future<Output = Result<Option<OrderRecord>, PortError>> + Send;

    /// Up to `limit` pending orders for a store, oldest first — the store's pull.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the queue could not be read.
    fn pull_pending(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<PendingOrder>, PortError>> + Send;

    /// Records a store's outcome for a pending order, by its `queued_id`. Returns whether a pending
    /// row was updated — `false` for an unknown id or one already reported (acking twice is a no-op).
    ///
    /// # Errors
    ///
    /// [`PortError`] if the queue could not be written.
    fn record_outcome(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        queued_id: OrderQueueId,
        outcome: &StoreOutcome,
    ) -> impl Future<Output = Result<bool, PortError>> + Send;
}

// ---------------------------------------------------------------------------------------------
// The relay: an OrderIn over the queue
// ---------------------------------------------------------------------------------------------

/// The cloud→store relay. Implements [`OrderIn`] over the durable queue: `submit` enqueues and parks;
/// `look_up` reads the recorded acceptance. Resolves the owning tenant and the per-store config from
/// the config tree itself, so it satisfies the tenant-agnostic port signature.
#[derive(Clone)]
pub struct OrderRelay<D, T, Q, C> {
    directory: D,
    config_trees: T,
    queue: Q,
    clock: C,
}

impl<D, T, Q, C> core::fmt::Debug for OrderRelay<D, T, Q, C> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("OrderRelay").finish_non_exhaustive()
    }
}

/// The per-store relay behaviour read from config.
struct RelayConfig {
    enabled: bool,
    wait: Duration,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            wait: Duration::from_millis(DEFAULT_WAIT_MS),
        }
    }
}

impl<D, T, Q, C> OrderRelay<D, T, Q, C>
where
    D: StoreDirectory + Clone,
    T: ConfigTreeStore + Clone,
    Q: OrderQueueStore + Clone,
    C: ClockSource + Clone,
{
    /// Builds a relay over the store→tenant directory, the config tree, the order queue, and a clock
    /// (which stamps the time half of a queued order's id).
    pub const fn new(directory: D, config_trees: T, queue: Q, clock: C) -> Self {
        Self {
            directory,
            config_trees,
            queue,
            clock,
        }
    }

    /// The owning tenant of a store, or a `PortError` when it is unknown to the relay (no config has
    /// been published to it) or the directory is unreadable.
    async fn tenant_of(&self, store_id: StoreId) -> Result<TenantId, PortError> {
        match self.directory.tenant_of(store_id).await {
            Ok(Some(tenant)) => Ok(tenant),
            Ok(None) => Err(PortError::failed_precondition(
                pos_ports::PortName::OrderIn,
                "the store is not configured for order intake",
            )),
            Err(error) => Err(error),
        }
    }

    /// Reads `store.order_relay.{enabled,wait_ms}` from the store's effective config, tolerating any
    /// shape: an absent or malformed value falls back to the default rather than failing intake.
    async fn config_for(&self, tenant: TenantId, store_id: StoreId) -> RelayConfig {
        let Ok(Some(state)) = self.config_trees.load(tenant, store_id).await else {
            return RelayConfig::default();
        };
        let tree = ConfigTree::from_state(store_id, CapabilityValidator, state);
        let Some(effective) = tree.current_effective() else {
            return RelayConfig::default();
        };
        let node = effective.get("order_relay");
        let default = RelayConfig::default();
        RelayConfig {
            enabled: node
                .and_then(|relay| relay.get("enabled"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(default.enabled),
            wait: node
                .and_then(|relay| relay.get("wait_ms"))
                .and_then(serde_json::Value::as_u64)
                .map_or(default.wait, Duration::from_millis),
        }
    }
}

impl<D, T, Q, C> OrderIn for OrderRelay<D, T, Q, C>
where
    D: StoreDirectory + Clone + Send + Sync,
    T: ConfigTreeStore + Clone + Send + Sync,
    Q: OrderQueueStore + Clone + Send + Sync,
    C: ClockSource + Clone + Send + Sync,
{
    async fn submit(&self, order: &InboundOrder) -> Result<OrderAcceptance, PortError> {
        let tenant = self.tenant_of(order.store_id).await?;
        let config = self.config_for(tenant, order.store_id).await;
        if !config.enabled {
            return Err(PortError::failed_precondition(
                pos_ports::PortName::OrderIn,
                "order intake is disabled for this store",
            ));
        }

        let channel = order.sales_channel.as_wire();
        let reference = order.external_reference.as_str();

        // Enqueue idempotently. A row that already carries an outcome (a repeat, or a store that
        // reported between our reads) short-circuits the park.
        let queued_id =
            OrderQueueId::new(mint_ulid(self.clock.now().as_milliseconds_since_epoch())?);
        let payload = QueuedOrderPayload::from_inbound(order);
        let record = self.queue.enqueue(tenant, queued_id, &payload).await?;
        if let Some(result) = outcome_to_result(&record.status) {
            return result;
        }

        // Park: re-read until the store reports or the deadline passes. The order stays queued either
        // way, so a timeout is `Unavailable` (a 503) and the caller resolves via `look_up`.
        let deadline_polls = config.wait.as_millis() / POLL_INTERVAL.as_millis().max(1);
        for _ in 0..deadline_polls {
            tokio::time::sleep(POLL_INTERVAL).await;
            if let Some(found) = self
                .queue
                .outcome(tenant, order.store_id, channel, reference)
                .await?
                && let Some(result) = outcome_to_result(&found.status)
            {
                return result;
            }
        }
        Err(PortError::unavailable(
            pos_ports::PortName::OrderIn,
            "the store has not yet confirmed the order; look it up to resolve",
        ))
    }

    async fn look_up(
        &self,
        store_id: StoreId,
        sales_channel: Open<SalesChannel>,
        external_reference: &ExternalReference,
    ) -> Result<Option<OrderAcceptance>, PortError> {
        let tenant = self.tenant_of(store_id).await?;
        let found = self
            .queue
            .outcome(
                tenant,
                store_id,
                sales_channel.as_wire(),
                external_reference.as_str(),
            )
            .await?;
        match found.map(|record| record.status) {
            Some(OrderStatus::Reported(StoreOutcome::Accepted(record))) => {
                Ok(record.to_acceptance())
            }
            // Pending, rejected, or unknown all read as "no acceptance to hand back" — a rejected
            // order was never accepted, and the caller learns that from the original submit's error.
            _ => Ok(None),
        }
    }
}

/// Maps a stored status to the `submit`/park result: `Some(Ok)` accepted, `Some(Err)` rejected,
/// `None` still pending (keep waiting).
fn outcome_to_result(status: &OrderStatus) -> Option<Result<OrderAcceptance, PortError>> {
    match status {
        OrderStatus::Pending => None,
        OrderStatus::Reported(StoreOutcome::Accepted(record)) => match record.to_acceptance() {
            Some(acceptance) => Some(Ok(acceptance)),
            None => Some(Err(PortError::internal(
                pos_ports::PortName::OrderIn,
                "the stored acceptance could not be read back",
            ))),
        },
        OrderStatus::Reported(StoreOutcome::Rejected { status, message }) => {
            Some(Err(port_error_from_wire(status, message)))
        }
    }
}

/// Rebuilds a [`PortError`] from a store's reported status/message so the caller sees the same class.
fn port_error_from_wire(status: &str, message: &str) -> PortError {
    let port = pos_ports::PortName::OrderIn;
    let message = message.to_owned();
    match status {
        "invalid_argument" => PortError::invalid_argument(port, message),
        "already_exists" => PortError::already_exists(port, message),
        "resource_exhausted" => PortError::resource_exhausted(port, message),
        "not_found" => PortError::not_found(port, message),
        // A store that refused for any other reason maps to failed_precondition (a 409) — a refusal
        // is the store's decision, not a server fault.
        _ => PortError::failed_precondition(port, message),
    }
}

/// Mints a ULID (time half from `now_ms`, random half from OS entropy), or
/// [`PortError::unavailable`] if the entropy source failed. `Ulid::from_parts` masks the randomness
/// to the low 80 bits the format defines.
fn mint_ulid(now_ms: i64) -> Result<Ulid, PortError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_error| {
        PortError::unavailable(pos_ports::PortName::OrderIn, "could not read OS entropy")
    })?;
    let ms = u64::try_from(now_ms.max(0)).unwrap_or(0);
    Ok(Ulid::from_parts(ms, u128::from_le_bytes(bytes)))
}

// ---------------------------------------------------------------------------------------------
// The store-facing sync surface: pull + ack
// ---------------------------------------------------------------------------------------------

/// The collaborators the store-facing order routes compose.
struct SyncState<Q, K, C> {
    queue: Q,
    keys: K,
    clock: C,
    /// The long-poll hold cap — a field so tests can drive it to zero.
    longpoll_cap: Duration,
}

impl<Q: Clone, K: Clone, C: Clone> Clone for SyncState<Q, K, C> {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
            keys: self.keys.clone(),
            clock: self.clock.clone(),
            longpoll_cap: self.longpoll_cap,
        }
    }
}

/// Builds the store-facing order sync router: pull pending orders, ack an outcome. Store-initiated
/// and scoped by [`Scope::RelayOrders`]; merged into the app router, so the `CloudApp` generics do not
/// grow.
pub fn orders_sync_router<Q, K, C>(queue: Q, keys: K, clock: C) -> Router
where
    Q: OrderQueueStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    orders_sync_router_with_cap(queue, keys, clock, LONGPOLL_CAP)
}

/// [`orders_sync_router`] with an explicit long-poll cap (tests pass zero for an immediate answer).
pub fn orders_sync_router_with_cap<Q, K, C>(
    queue: Q,
    keys: K,
    clock: C,
    longpoll_cap: Duration,
) -> Router
where
    Q: OrderQueueStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/orders",
            get(pull_orders::<Q, K, C>),
        )
        .route(
            "/sync/stores/{store_id}/orders/{queued_id}/ack",
            post(ack_order::<Q, K, C>),
        )
        .with_state(SyncState {
            queue,
            keys,
            clock,
            longpoll_cap,
        })
}

/// One pending order on the wire, for the pull response.
#[derive(Debug, Clone, Serialize)]
struct PendingOrderDto {
    queued_id: String,
    order: QueuedOrderPayload,
}

/// `GET /sync/stores/{store_id}/orders` — the store pulls its pending batch (bounded long-poll).
async fn pull_orders<Q, K, C>(
    State(state): State<SyncState<Q, K, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
) -> Response
where
    Q: OrderQueueStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::RelayOrders) {
        return forbidden.into_response();
    }
    let Ok(store_id) = store_id.parse::<StoreId>() else {
        return api_error_with_details(
            pos_proto::error::ErrorStatus::InvalidArgument,
            "store_id is not a ULID",
            &[("store_id", "NOT_A_ULID")],
        );
    };

    // Answer immediately if anything is pending; otherwise hold the request open, re-checking, until
    // the cap. The store still initiated the connection — this is its own outbound wait, not a push.
    let mut waited = Duration::ZERO;
    loop {
        match state
            .queue
            .pull_pending(grant.tenant(), store_id, PULL_BATCH)
            .await
        {
            Ok(pending) if !pending.is_empty() || waited >= state.longpoll_cap => {
                let body: Vec<PendingOrderDto> = pending
                    .into_iter()
                    .map(|order| PendingOrderDto {
                        queued_id: order.queued_id.to_string(),
                        order: order.payload,
                    })
                    .collect();
                return (StatusCode::OK, Json(body)).into_response();
            }
            Ok(_) => {
                tokio::time::sleep(POLL_INTERVAL).await;
                waited += POLL_INTERVAL;
            }
            Err(error) => return relay_error(&error),
        }
    }
}

/// `POST /sync/stores/{store_id}/orders/{queued_id}/ack` — the store reports an outcome.
async fn ack_order<Q, K, C>(
    State(state): State<SyncState<Q, K, C>>,
    headers: HeaderMap,
    Path((store_id, queued_id)): Path<(String, String)>,
    Json(outcome): Json<StoreOutcome>,
) -> Response
where
    Q: OrderQueueStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::RelayOrders) {
        return forbidden.into_response();
    }
    let (Ok(store_id), Ok(queued_id)) = (
        store_id.parse::<StoreId>(),
        queued_id.parse::<Ulid>().map(OrderQueueId::new),
    ) else {
        return api_error_with_details(
            pos_proto::error::ErrorStatus::InvalidArgument,
            "store_id or queued_id is not a ULID",
            &[("store_id", "NOT_A_ULID"), ("queued_id", "NOT_A_ULID")],
        );
    };

    match state
        .queue
        .record_outcome(grant.tenant(), store_id, queued_id, &outcome)
        .await
    {
        // `204` whether or not it updated a pending row: acking an unknown or already-acked id is an
        // idempotent no-op, and telling the store which it was is a needless signal.
        Ok(_updated) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => relay_error(&error),
    }
}

/// The tenant a look-up names, and the caller's own idempotency key — the `GET /v1/orders` query.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LookUpQuery {
    pub(crate) store_id: String,
    pub(crate) sales_channel: String,
    pub(crate) external_reference: String,
}

/// Maps a queue [`PortError`] to a response for the store-facing routes.
///
/// This was the **second** hand-written copy of the `ErrorStatus` -> HTTP code map that
/// `ErrorStatus::http_code` owns; `orders.rs` held the first, deleted in the previous slice.
/// Finding it twice is what makes it a pattern rather than an oversight — the shape was being
/// copied from one route module to the next — so both are gone and the map has one home again.
fn relay_error(error: &PortError) -> Response {
    api_error(error.status(), error.to_string())
}

/// Converts a wire look-up query to the typed parts, or the reason it is a `400`.
pub(crate) fn parse_look_up(
    query: &LookUpQuery,
) -> Result<(StoreId, Open<SalesChannel>, ExternalReference), &'static str> {
    let store_id = query
        .store_id
        .parse::<StoreId>()
        .map_err(|_ignored| "store_id must be a ULID")?;
    let sales_channel = Open::<SalesChannel>::parse(&query.sales_channel);
    let external_reference = ExternalReference::parse(&query.external_reference)
        .map_err(|_ignored| "external_reference must be non-empty and at most 128 bytes")?;
    Ok((store_id, sales_channel, external_reference))
}
