// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The printed check (pre-bill) of each open order and part, and whether it still shows what the
//! order comes to ([ADR-0218](../../../../../docs/adr/0218-a-printed-check-is-an-event-every-till-sees.md)
//! decision 2): a marker, never a state, so nothing is refused because of it but the settle of a
//! store that requires a fresh check.

use pos_proto::OrderLineState;
use pos_proto::ids::{BillId, OrderId, OrderLineId};
use pos_proto::money::Money;
use pos_proto::time::Timestamp;

use super::super::Projection;

/// The last check printed for an order, or for one part of its split bill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) struct CheckMark {
    /// When it printed.
    pub(in crate::app) printed_time: Timestamp,
    /// What it showed as owed.
    pub(in crate::app) total_due: Money,
    /// Which check of the order or part it was, from 1.
    pub(in crate::app) copy: u32,
    /// What it was printed against, so a later change shows it stale.
    basis: CheckBasis,
}

/// What a check shows, in the terms that change it: each live line's id, quantity and total, and
/// what has been taken off or waived. Compared whole, so any change to any of them makes the check
/// stale, and a bill opening over the same lines does not.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckBasis {
    lines: Vec<(OrderLineId, i64, i64)>,
    reduced_minor: i64,
    waived: usize,
}

impl Projection {
    /// Records a check printed for `order_id`, or for its part `bill_id`, against what they come
    /// to now. Live and replayed alike.
    pub(in crate::app) fn record_check(
        &mut self,
        order_id: OrderId,
        bill_id: Option<BillId>,
        printed_time: Timestamp,
        total_due: Money,
        copy: u32,
    ) {
        let basis = self.check_basis(order_id, bill_id);
        self.checks.insert(
            (order_id, bill_id),
            CheckMark {
                printed_time,
                total_due,
                copy,
                basis,
            },
        );
    }

    /// The last check printed for the order or part, and whether the order has changed since.
    pub(in crate::app) fn check_mark(
        &self,
        order_id: OrderId,
        bill_id: Option<BillId>,
    ) -> Option<(CheckMark, bool)> {
        let mark = self.checks.get(&(order_id, bill_id))?;
        let stale = mark.basis != self.check_basis(order_id, bill_id);
        Some((mark.clone(), stale))
    }

    /// The copy the next check of the order or part is.
    pub(in crate::app) fn next_check_copy(
        &self,
        order_id: OrderId,
        bill_id: Option<BillId>,
    ) -> u32 {
        self.checks
            .get(&(order_id, bill_id))
            .map_or(1, |mark| mark.copy.saturating_add(1))
    }

    /// Forgets every check of an order that owes nothing any more: closed, or paid in full.
    pub(in crate::app) fn forget_checks(&mut self, order_id: OrderId) {
        self.checks.retain(|(order, _), _| *order != order_id);
    }

    /// The order's lines as a check covers them, and what is taken off them: every live line of the
    /// order for a whole-order check, or the part's own lines and reductions.
    fn check_basis(&self, order_id: OrderId, bill_id: Option<BillId>) -> CheckBasis {
        let bills: Vec<BillId> = match bill_id {
            Some(bill_id) => vec![bill_id],
            None => self
                .order_bill_ids
                .get(&order_id)
                .cloned()
                .unwrap_or_default(),
        };
        let line_ids: Vec<OrderLineId> = match bill_id.and_then(|id| self.bills.get(&id)) {
            Some(bill) => bill.order_line_ids.clone(),
            None => self.line_ids_for_order(order_id).to_vec(),
        };
        let mut lines: Vec<(OrderLineId, i64, i64)> = line_ids
            .iter()
            .filter_map(|id| self.lines.get(id).map(|line| (*id, line)))
            .filter(|(_, line)| line.state != OrderLineState::Voided)
            .map(|(id, line)| (id, line.quantity.milli, line.line_total.amount_minor))
            .collect();
        lines.sort_unstable();
        let mut reduced_minor: i64 = 0;
        let mut waived = 0;
        for bill in bills.iter().filter_map(|id| self.bills.get(id)) {
            for taken in [bill.discount, bill.comp].into_iter().flatten() {
                reduced_minor = reduced_minor.saturating_add(taken.amount_minor);
            }
            waived += bill.waived_fee_ids.len();
        }
        CheckBasis {
            lines,
            reduced_minor,
            waived,
        }
    }
}
