// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The inbound-order idempotency ledger ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)).
//!
//! An inbound order arrives with a caller's own reference — a marketplace order id, a public-API
//! idempotency key — that is **not** in the event log (the log carries the order's identity, not the
//! caller's). So the store keeps a small side record: `(sales_channel, external_reference) →` what
//! the order became. A retry, or the relay's at-least-once redelivery, finds the record and returns
//! the same acceptance rather than opening a second order.
//!
//! # Why this is a [`Transactional`] port, not a standalone store
//!
//! The record must be written **in the same transaction as `sales.order.opened`** — either the order
//! and its ledger row both land, or neither. Anything less reopens the exact duplicate-order window
//! the ledger exists to close: a crash between opening the order and recording it would let a retry
//! open a second one. So, exactly like [`crate::ConfigStore`], this port shares
//! [`Transactional::Tx`] with [`crate::EventStore`] — `record` buffers into the caller's transaction
//! and commits with it. The key is a *plain* insert, not insert-or-ignore: a second order racing in
//! on the same key fails its commit (the single writer serialises them) and rolls back rather than
//! duplicating, and the caller resolves the race with [`Self::look_up`].

use core::future::Future;

use pos_proto::ids::{OrderId, StoreId};
use pos_proto::money::Money;
use pos_proto::time::BusinessDate;
use serde::{Deserialize, Serialize};

use crate::error::PortError;
use crate::tx::Transactional;

/// What a caller's `(sales_channel, external_reference)` produced — enough to rebuild the acceptance
/// a repeat is owed, without re-reading the order.
///
/// The `queue_number` is deliberately **not** stored: it is reconstructed on a repeat from the
/// (durable, order-keyed, idempotent) queue authority, so a crash between opening the order and
/// allocating its number still yields exactly one number. `business_date` is stored precisely so
/// that reconstruction keys the queue on the day the order opened, not on "now".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct IntakeRecord {
    /// The order the caller's reference opened.
    pub order_id: OrderId,
    /// The trading day the order opened on — the key its queue number is reconstructed under.
    pub business_date: BusinessDate,
    /// The accepted total (tax-inclusive), the store's own menu total.
    pub total: Money,
    /// Whether any line's caller-quoted price differed from the store's (reported, never charged).
    pub repriced: bool,
    /// Whether the order waits for a member of staff before the kitchen sees it (a QR order does).
    pub awaiting_staff_confirmation: bool,
}

/// The store's inbound-order idempotency ledger.
///
/// # Contract
///
/// 1. **`record` buffers into the caller's transaction**, so the ledger row commits atomically with
///    the `sales.order.opened` events — either both land or neither.
/// 2. **A repeat key is refused, not ignored.** Committing a transaction whose key already exists
///    fails with [`PortError::already_exists`] and rolls the whole transaction back — so a second
///    order on the same key never lands. The caller then resolves the repeat with [`Self::look_up`].
/// 3. **`look_up` returns the record a key already produced**, or `None` if it is the first time.
pub trait IntakeLedger: Transactional {
    /// Buffers the ledger row for `(sales_channel, external_reference)` into the caller's
    /// transaction. The write is realised — and its uniqueness enforced — at
    /// [`crate::TxContext::commit`].
    ///
    /// # Errors
    ///
    /// [`PortError::internal`] if the record cannot be encoded for storage.
    fn record(
        &self,
        tx: &mut <Self as Transactional>::Tx,
        store_id: StoreId,
        sales_channel: &str,
        external_reference: &str,
        record: &IntakeRecord,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// The record a `(sales_channel, external_reference)` already produced, or `None`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached, or [`PortError::internal`] if a
    /// stored record cannot be decoded.
    fn look_up(
        &self,
        store_id: StoreId,
        sales_channel: &str,
        external_reference: &str,
    ) -> impl Future<Output = Result<Option<IntakeRecord>, PortError>> + Send;
}
