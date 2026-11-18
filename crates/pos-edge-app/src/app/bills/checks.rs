// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Recording a printed check and reading its marker
//! ([ADR-0218](../../../../../docs/adr/0218-a-printed-check-is-an-event-every-till-sees.md)).

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::events::BillingCheckPrinted;
use pos_proto::ids::{BillId, OrderId};
use pos_proto::money::Money;
use pos_proto::time::Timestamp;

use crate::app::{AppError, Edge, PreBill};

/// The last check printed for an order or a part, as every till reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckPrinted {
    /// When it printed.
    pub printed_time: Timestamp,
    /// What it showed as owed.
    pub total_due: Money,
    /// Which check of the order or part it was, from 1.
    pub copy: u32,
    /// Whether the order has changed since, so the paper no longer shows what is owed.
    pub stale: bool,
}

impl<S: EventStore> Edge<S> {
    /// Records one `billing.check.printed` per check about to print, in one transaction, before
    /// anything goes to a printer (ADR-0218 decision 1). A printer that fails afterwards leaves
    /// the record standing: the check was asked for and shown on screen.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] if the actor may not open a bill, the act a check belongs to; or
    /// [`AppError`] if the store cannot be written.
    pub async fn record_checks(
        &self,
        actor: Actor,
        order_id: OrderId,
        pre_bills: &[PreBill],
    ) -> Result<(), AppError> {
        let ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::OpenBill)?;
        let mut envelopes = Vec::with_capacity(pre_bills.len());
        let mut messages = Vec::with_capacity(pre_bills.len());
        let mut printed = Vec::with_capacity(pre_bills.len());
        for pre_bill in pre_bills {
            let copy = self
                .lock_projection()
                .next_check_copy(order_id, pre_bill.bill_id);
            let event = BillingCheckPrinted {
                order_id,
                bill_id: pre_bill.bill_id,
                total_due: pre_bill.totals.total_due,
                copy,
            };
            let (envelope, message) = self.prepare(&ctx, &event)?;
            envelopes.push(envelope);
            messages.push(message);
            printed.push(event);
        }
        self.append_and_publish(envelopes, messages).await?;
        let mut projection = self.lock_projection();
        for event in printed {
            projection.record_check(
                event.order_id,
                event.bill_id,
                ctx.now,
                event.total_due,
                event.copy,
            );
        }
        Ok(())
    }

    /// The last check printed for the order, or for its part `bill_id`, and whether it is stale.
    #[must_use]
    pub fn check_printed(
        &self,
        order_id: OrderId,
        bill_id: Option<BillId>,
    ) -> Option<CheckPrinted> {
        let (mark, stale) = self.lock_projection().check_mark(order_id, bill_id)?;
        Some(CheckPrinted {
            printed_time: mark.printed_time,
            total_due: mark.total_due,
            copy: mark.copy,
            stale,
        })
    }

    /// The check that stands for `bill_id`: its own, as a part of a split bill, or the whole
    /// order's where the bill is the order's only open one.
    #[must_use]
    pub fn check_for_bill(&self, order_id: OrderId, bill_id: BillId) -> Option<CheckPrinted> {
        self.check_printed(order_id, Some(bill_id)).or_else(|| {
            let whole = self.lock_projection().open_bills_for_order(order_id).len() <= 1;
            whole.then(|| self.check_printed(order_id, None)).flatten()
        })
    }

    /// Refuses to settle a bill with no fresh check, where the store requires one before payment
    /// (`billing.check_before_payment`, ADR-0218).
    pub(in crate::app) fn require_fresh_check(
        &self,
        order_id: OrderId,
        bill_id: BillId,
    ) -> Result<(), AppError> {
        match self.check_for_bill(order_id, bill_id) {
            Some(check) if !check.stale => Ok(()),
            _ => Err(AppError::CheckRequired),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pos_fakes::FakeStore;
    use pos_proto::billing::PublishedBilling;
    use pos_proto::ids::TableId;
    use pos_proto::ulid::Ulid;

    use crate::app::tests::{a_line, actor, cash, identity, vnd};
    use crate::app::{AppError, Edge, EdgeSession};
    use crate::receipt::InMemoryReceipts;

    fn edge_on(store: FakeStore, check_before_payment: bool) -> Edge<FakeStore> {
        let session = EdgeSession {
            billing: PublishedBilling {
                check_before_payment: Some(check_before_payment),
                ..PublishedBilling::default()
            },
            ..EdgeSession::bootstrap()
        };
        Edge::new(
            store,
            identity(),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds")
    }

    /// Prints the table's checks as the route does: recorded, then (here) no paper.
    async fn print(edge: &Edge<FakeStore>, table: TableId) {
        let order = edge.order_for_table(table).expect("an order");
        let pre_bills = edge.pre_bills_for_order(order).expect("pre-bills");
        edge.record_checks(actor(), order, &pre_bills)
            .await
            .expect("records");
    }

    /// ADR-0218 decision 2: every till sees the check, each print counts, and a change after it
    /// marks it stale until the next print.
    #[test]
    fn a_check_is_marked_on_the_order_and_turns_stale_when_the_order_changes() {
        pos_fakes::executor::run_ready(async {
            let edge = edge_on(FakeStore::default(), false);
            let table = TableId::new(Ulid::from_u128(900));
            edge.seat_table(actor(), table, None).await.expect("seats");
            edge.add_line(actor(), table, a_line()).await.expect("adds");
            let order = edge.order_for_table(table).expect("an order");
            assert_eq!(edge.check_printed(order, None), None);

            print(&edge, table).await;
            let first = edge.check_printed(order, None).expect("marked");
            assert_eq!((first.copy, first.stale), (1, false));
            assert_eq!(first.total_due, vnd(165_000));

            edge.add_line(actor(), table, a_line()).await.expect("adds");
            assert!(
                edge.check_printed(order, None)
                    .is_some_and(|check| check.stale)
            );

            print(&edge, table).await;
            let second = edge.check_printed(order, None).expect("marked");
            assert_eq!((second.copy, second.stale), (2, false));
        });
    }

    /// `billing.check_before_payment`: refused with no check and with a stale one, paid with a
    /// fresh one; and opening the bill over the same lines does not make the check stale.
    #[test]
    fn a_store_that_requires_a_check_takes_payment_only_against_a_fresh_one() {
        pos_fakes::executor::run_ready(async {
            let edge = edge_on(FakeStore::default(), true);
            let table = TableId::new(Ulid::from_u128(901));
            edge.seat_table(actor(), table, None).await.expect("seats");
            edge.add_line(actor(), table, a_line()).await.expect("adds");
            let bill = edge.open_bill(actor(), table).await.expect("opens").bill_id;
            let refused = edge
                .settle_bill(actor(), bill, vec![cash(165_000)], None)
                .await;
            assert!(
                matches!(refused, Err(AppError::CheckRequired)),
                "{refused:?}"
            );

            print(&edge, table).await;
            let settled = edge
                .settle_bill(actor(), bill, vec![cash(165_000)], None)
                .await
                .expect("a fresh check lets it be paid");
            assert_eq!(settled.receipt_number, Some(1));
            let order = edge.order_for_bill(bill).expect("an order");
            assert_eq!(
                edge.check_printed(order, None),
                None,
                "a paid order's check is done with"
            );
        });
    }

    /// The marker is the log's: a restart folds it back, copy count and all.
    #[test]
    fn a_restart_keeps_the_check_marker() {
        pos_fakes::executor::run_ready(async {
            let store = FakeStore::default();
            let table = TableId::new(Ulid::from_u128(902));
            let order = {
                let edge = edge_on(store.clone(), false);
                edge.seat_table(actor(), table, None).await.expect("seats");
                edge.add_line(actor(), table, a_line()).await.expect("adds");
                print(&edge, table).await;
                print(&edge, table).await;
                edge.order_for_table(table).expect("an order")
            };
            let edge = edge_on(store, false);
            edge.rebuild().await.expect("rebuilds");
            let check = edge.check_printed(order, None).expect("folded back");
            assert_eq!((check.copy, check.stale), (2, false));
        });
    }
}
