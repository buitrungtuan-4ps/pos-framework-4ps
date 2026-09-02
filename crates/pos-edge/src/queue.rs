// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The queue-number authority: where a tableless order gets the number the counter shouts out.
//!
//! A takeaway order — a marketplace order, the public API, a takeaway guest — has no table to be
//! served at, so the store hands it a **daily-resetting** queue number ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)).
//! Unlike the receipt number ([`ReceiptAuthority`](crate::receipt::ReceiptAuthority)), which is
//! gapless-forever per store, this counter resets each business date: the first takeaway of a new
//! trading day is `#1` again, with no midnight job — a business date the counter has never seen
//! simply starts at 1.
//!
//! [`crate::app::Edge`] is generic over its store `S`, so it cannot reach a concrete store's
//! inherent counter; the authority is injected into [`EdgeOrderIn`](crate::EdgeOrderIn) instead. In
//! the field that is [`store_sqlite::SqliteStore`], whose single writer thread serialises every
//! allocation into one durable counter — so a restart mid-day does **not** reissue `#1`. The
//! fakes-backed example and the intake tests use [`InMemoryQueueNumbers`], the same contract
//! (per-date reset, idempotent by order) without a database.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;

use pos_ports::PortError;
use pos_proto::ids::{OrderId, StoreId};
use pos_proto::time::BusinessDate;

use store_sqlite::SqliteStore;

/// Allocates the daily queue number a tableless order is called back by (ADR-0064).
///
/// Injected into [`EdgeOrderIn`](crate::EdgeOrderIn) rather than derived from the store type, so the
/// one intake path runs unchanged over the real SQLite store and over the fakes.
pub trait QueueNumberAuthority: Send + Sync {
    /// Allocates the next queue number for `order_id` at `store_id` on `business_date`.
    ///
    /// **Idempotent by order**: allocating twice for one order returns the same number and does not
    /// advance the counter, so a retry after a crash shouts the same number rather than burning a
    /// second one. **Resets per business date**: a date the counter has not seen starts at 1.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the authority cannot be reached or the allocation fails.
    fn allocate_queue_number(
        &self,
        store_id: StoreId,
        business_date: BusinessDate,
        order_id: OrderId,
    ) -> impl Future<Output = Result<u64, PortError>> + Send;

    /// The number an order was **already** given, or `None` if it never had one.
    ///
    /// A read, for the counter screen that lists open takeaway orders: it shows the number staff
    /// shouted, not a new one. [`Self::allocate_queue_number`] would answer too, being idempotent
    /// by order — and would mint a number for an order that has none, which is a write on a read
    /// path and would put a number on a floor order that should never have had one.
    ///
    /// No business date, deliberately. The allocation is recorded per `(store, order)`, so an order
    /// still unpaid after the day's cutoff is found the next morning rather than looked for under a
    /// date it was never allocated on.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the authority cannot be reached or the read fails.
    fn queue_number_for(
        &self,
        store_id: StoreId,
        order_id: OrderId,
    ) -> impl Future<Output = Result<Option<u64>, PortError>> + Send;
}

/// One authority, shared. The trait returns `impl Future`, so it is not dyn-compatible and
/// `Arc<dyn QueueNumberAuthority>` cannot exist; this is what lets the intake path and the
/// counter's read route hold the *same* authority instead of two that would disagree.
impl<T: QueueNumberAuthority + ?Sized> QueueNumberAuthority for std::sync::Arc<T> {
    fn allocate_queue_number(
        &self,
        store_id: StoreId,
        business_date: BusinessDate,
        order_id: OrderId,
    ) -> impl Future<Output = Result<u64, PortError>> + Send {
        (**self).allocate_queue_number(store_id, business_date, order_id)
    }

    fn queue_number_for(
        &self,
        store_id: StoreId,
        order_id: OrderId,
    ) -> impl Future<Output = Result<Option<u64>, PortError>> + Send {
        (**self).queue_number_for(store_id, order_id)
    }
}

impl QueueNumberAuthority for SqliteStore {
    /// Forwards to the store's single-writer counter, keyed by `(store, business_date)` so the
    /// sequence resets daily and survives a restart.
    async fn allocate_queue_number(
        &self,
        store_id: StoreId,
        business_date: BusinessDate,
        order_id: OrderId,
    ) -> Result<u64, PortError> {
        self.allocate_daily_queue_number(store_id, business_date, order_id)
            .await
    }

    /// Forwards to the store's read of `queue_allocations`, which is keyed by order and carries no
    /// date — so the lookup needs none.
    async fn queue_number_for(
        &self,
        store_id: StoreId,
        order_id: OrderId,
    ) -> Result<Option<u64>, PortError> {
        self.daily_queue_number_for(store_id, order_id).await
    }
}

/// An in-memory queue-number authority: a per-`(store, business_date)` counter with order-keyed
/// idempotency.
///
/// What the fakes-backed example and the intake tests allocate against. Not durable — a restart
/// forgets its counters, which is exactly why the field uses [`store_sqlite::SqliteStore`]; the
/// contract it honours (per-date reset, idempotent by order) is identical, so a flow proven here
/// behaves the same in production.
#[derive(Debug, Default)]
pub struct InMemoryQueueNumbers {
    inner: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// The next number to hand out, per `(store, business_date)`. Absent means "start at 1", which
    /// is the daily reset.
    next: HashMap<(StoreId, BusinessDate), u64>,
    /// The number already handed to an order, so a repeat is idempotent.
    allocated: HashMap<OrderId, u64>,
}

impl InMemoryQueueNumbers {
    /// A fresh authority with every counter at its start.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl QueueNumberAuthority for InMemoryQueueNumbers {
    async fn allocate_queue_number(
        &self,
        store_id: StoreId,
        business_date: BusinessDate,
        order_id: OrderId,
    ) -> Result<u64, PortError> {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = state.allocated.get(&order_id) {
            return Ok(*existing);
        }
        // The number for this store on this date, defaulting the sequence to 1, then advanced.
        let counter = state.next.entry((store_id, business_date)).or_insert(1);
        let number = *counter;
        *counter = counter.saturating_add(1);
        state.allocated.insert(order_id, number);
        Ok(number)
    }

    async fn queue_number_for(
        &self,
        _store_id: StoreId,
        order_id: OrderId,
    ) -> Result<Option<u64>, PortError> {
        // `allocated` is keyed by order alone, matching the durable table: an order id already
        // names its store, so there is nothing to scope by.
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(state.allocated.get(&order_id).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::{InMemoryQueueNumbers, QueueNumberAuthority};
    use pos_proto::ids::{OrderId, StoreId};
    use pos_proto::time::BusinessDate;
    use pos_proto::ulid::Ulid;

    fn store() -> StoreId {
        StoreId::new(Ulid::from_u128(1))
    }

    fn order(seed: u128) -> OrderId {
        OrderId::new(Ulid::from_u128(seed))
    }

    fn date(day: u8) -> BusinessDate {
        BusinessDate::from_ymd(2026, 8, day).expect("a real date")
    }

    #[test]
    fn numbers_are_sequential_from_one_within_a_day() {
        pos_fakes::executor::run_ready(async {
            let queue = InMemoryQueueNumbers::new();
            let mut seen = Vec::new();
            for n in 1..=4 {
                let number = queue
                    .allocate_queue_number(store(), date(24), order(n))
                    .await
                    .expect("allocates");
                seen.push(number);
            }
            assert_eq!(seen, vec![1, 2, 3, 4], "sequential, starting at one");
        });
    }

    #[test]
    fn the_counter_resets_on_a_new_business_date() {
        pos_fakes::executor::run_ready(async {
            let queue = InMemoryQueueNumbers::new();
            let first_day = queue
                .allocate_queue_number(store(), date(24), order(1))
                .await
                .expect("allocates");
            let _second = queue
                .allocate_queue_number(store(), date(24), order(2))
                .await
                .expect("allocates");
            // A new business date has never been seen, so it starts its own sequence at 1 again.
            let next_day = queue
                .allocate_queue_number(store(), date(25), order(3))
                .await
                .expect("allocates");
            assert_eq!((first_day, next_day), (1, 1), "each day restarts at one");
        });
    }

    #[test]
    fn allocating_twice_for_one_order_returns_the_same_number() {
        pos_fakes::executor::run_ready(async {
            let queue = InMemoryQueueNumbers::new();
            let repeated = order(42);
            let first = queue
                .allocate_queue_number(store(), date(24), repeated)
                .await
                .expect("allocates");
            let again = queue
                .allocate_queue_number(store(), date(24), repeated)
                .await
                .expect("allocates");
            assert_eq!(
                first, again,
                "idempotent by order, so a retry shouts the same number"
            );

            // A different order still advances past the reused number.
            let other = queue
                .allocate_queue_number(store(), date(24), order(43))
                .await
                .expect("allocates");
            assert_eq!(other, first + 1);
        });
    }

    #[test]
    fn counters_are_independent_per_store() {
        pos_fakes::executor::run_ready(async {
            let queue = InMemoryQueueNumbers::new();
            let store_a = StoreId::new(Ulid::from_u128(10));
            let store_b = StoreId::new(Ulid::from_u128(20));
            let a = queue
                .allocate_queue_number(store_a, date(24), order(1))
                .await
                .expect("allocates");
            let b = queue
                .allocate_queue_number(store_b, date(24), order(2))
                .await
                .expect("allocates");
            assert_eq!((a, b), (1, 1), "each store starts its own sequence at one");
        });
    }
}
