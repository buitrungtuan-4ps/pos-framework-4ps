// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The receipt-number authority: where the bill flow gets its gapless per-store number.
//!
//! A receipt number is allocated inside the settle transaction and is **gapless per store** — the
//! promise a customer, an auditor and a country's tax code all rely on ([ADR-0025](../../../docs/adr/0025-receipt-number-authority.md)).
//! [`crate::app::Edge`] is generic over its event store `S`, so it cannot reach into a concrete
//! store's inherent methods; the authority is injected instead. In the field that is
//! [`store_sqlite::SqliteStore`], whose single writer thread serialises every allocation; the
//! fakes-backed example and the engine tests use [`InMemoryReceipts`], which is the same gapless,
//! bill-idempotent contract without a database.
//!
//! This is the store's own receipt number and **never** a legal invoice number: the latter is the
//! country module's, issued from a pre-allocated range, and conflating the two is forbidden
//! ([ADR-0025](../../../docs/adr/0025-receipt-number-authority.md), `docs/pos-spec.md` §14.4).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use pos_ports::PortError;
use pos_proto::ids::{BillId, StoreId};

use store_sqlite::SqliteStore;

/// Allocates the gapless per-store receipt number a bill settles with (ADR-0025).
///
/// Injected into [`crate::app::Edge`] rather than derived from its store type, so the one
/// application loop runs unchanged over the real SQLite store and over the fakes. The returned
/// future borrows `self`, which is what lets the SQLite implementation forward straight to its
/// writer thread without boxing state.
pub trait ReceiptAuthority: Send + Sync + std::fmt::Debug {
    /// Allocates the next receipt number for `bill_id` at `store_id`.
    ///
    /// **Idempotent by bill**: allocating twice for one bill returns the same number and does not
    /// advance the counter, so a crash between allocating and appending `billing.bill.settled`
    /// reuses the number rather than skipping one — the property that keeps the sequence gapless.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the authority cannot be reached or the allocation fails.
    fn allocate_receipt<'a>(
        &'a self,
        store_id: StoreId,
        bill_id: BillId,
    ) -> Pin<Box<dyn Future<Output = Result<u64, PortError>> + Send + 'a>>;
}

impl ReceiptAuthority for SqliteStore {
    /// Forwards to the store's single-writer counter (ADR-0025), which is gapless while this one
    /// store authority is reachable because every allocation serialises through the writer thread.
    fn allocate_receipt<'a>(
        &'a self,
        store_id: StoreId,
        bill_id: BillId,
    ) -> Pin<Box<dyn Future<Output = Result<u64, PortError>> + Send + 'a>> {
        Box::pin(self.allocate_receipt_number(store_id, bill_id))
    }
}

/// An in-memory receipt authority: a per-store gapless counter with bill-keyed idempotency.
///
/// What the fakes-backed example and the engine tests settle against. Not durable — a restart
/// forgets its counters — which is exactly why the field uses [`store_sqlite::SqliteStore`]; the
/// contract it honours (gapless, idempotent by bill) is identical, so a flow proven here behaves the
/// same in production.
#[derive(Debug, Default)]
pub struct InMemoryReceipts {
    inner: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// The next number to hand out, per store. Absent means "start at 1".
    next: HashMap<StoreId, u64>,
    /// The number already handed to a bill, so a repeat is idempotent.
    allocated: HashMap<BillId, u64>,
}

impl InMemoryReceipts {
    /// A fresh authority with every counter at its start.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ReceiptAuthority for InMemoryReceipts {
    fn allocate_receipt<'a>(
        &'a self,
        store_id: StoreId,
        bill_id: BillId,
    ) -> Pin<Box<dyn Future<Output = Result<u64, PortError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(existing) = state.allocated.get(&bill_id) {
                return Ok(*existing);
            }
            // The number for this store, defaulting the sequence to 1, then advanced for next time.
            let counter = state.next.entry(store_id).or_insert(1);
            let number = *counter;
            *counter = counter.saturating_add(1);
            state.allocated.insert(bill_id, number);
            Ok(number)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{InMemoryReceipts, ReceiptAuthority};
    use pos_proto::ids::{BillId, StoreId};
    use pos_proto::ulid::Ulid;

    fn store() -> StoreId {
        StoreId::new(Ulid::from_u128(1))
    }

    #[test]
    fn numbers_are_gapless_from_one() {
        pos_fakes::executor::run_ready(async {
            let receipts = InMemoryReceipts::new();
            let mut seen = Vec::new();
            for bill in 1..=5 {
                let number = receipts
                    .allocate_receipt(store(), BillId::new(Ulid::from_u128(bill)))
                    .await
                    .expect("allocates");
                seen.push(number);
            }
            assert_eq!(seen, vec![1, 2, 3, 4, 5], "gapless, starting at one");
        });
    }

    #[test]
    fn allocating_twice_for_one_bill_returns_the_same_number() {
        pos_fakes::executor::run_ready(async {
            let receipts = InMemoryReceipts::new();
            let bill = BillId::new(Ulid::from_u128(42));
            let first = receipts
                .allocate_receipt(store(), bill)
                .await
                .expect("allocates");
            let again = receipts
                .allocate_receipt(store(), bill)
                .await
                .expect("allocates");
            assert_eq!(first, again, "idempotent by bill, so no gap opens on retry");

            // A different bill still advances past the reused number.
            let other = receipts
                .allocate_receipt(store(), BillId::new(Ulid::from_u128(43)))
                .await
                .expect("allocates");
            assert_eq!(other, first + 1);
        });
    }

    #[test]
    fn counters_are_independent_per_store() {
        pos_fakes::executor::run_ready(async {
            let receipts = InMemoryReceipts::new();
            let store_a = StoreId::new(Ulid::from_u128(10));
            let store_b = StoreId::new(Ulid::from_u128(20));
            let a = receipts
                .allocate_receipt(store_a, BillId::new(Ulid::from_u128(1)))
                .await
                .expect("allocates");
            let b = receipts
                .allocate_receipt(store_b, BillId::new(Ulid::from_u128(2)))
                .await
                .expect("allocates");
            assert_eq!((a, b), (1, 1), "each store starts its own sequence at one");
        });
    }
}
