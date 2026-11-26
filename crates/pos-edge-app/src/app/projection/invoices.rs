// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The e-invoices a store has issued and the ones it is waiting to issue
//! ([ADR-0225](../../../../../docs/adr/0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md),
//! [ADR-0229](../../../../../docs/adr/0229-the-e-invoice-issue-moment-is-a-schedule.md)): the last
//! number under each symbol, which the next invoice continues from; the bills with one, which a void
//! after payment refuses and a receipt's copy prints again; and the deferred invoices in due order.

use std::collections::{BTreeMap, HashMap};

use pos_core::decision::Actor;
use pos_proto::einvoice::InvoiceSymbol;
use pos_proto::events::{FiscalInvoiceIssued, FiscalInvoiceScheduled};
use pos_proto::ids::{BillId, EventId, SubjectId};
use pos_proto::time::Timestamp;

use crate::app::IssuedInvoiceRef;

/// The most deferred invoices a store holds at once (AGENTS.md §2). A settle that finds the queue
/// full issues at once rather than refuse the sale or lose the invoice (ADR-0229 decision 4).
pub(in crate::app) const PENDING_LIMIT: usize = 10_000;

/// A deferred invoice: the buyer it will name, and who settled the bill, whom its issue is
/// recorded against as the settle's own follow-on.
#[derive(Debug, Clone, Copy)]
pub(in crate::app) struct Waiting {
    /// The buyer, when the guest gave one.
    pub(in crate::app) buyer_subject_id: Option<SubjectId>,
    /// Who settled the bill, and on which device.
    pub(in crate::app) settled_by: Actor,
}

/// Folded from `fiscal.invoice.issued` and `fiscal.invoice.scheduled`, so a restart continues each
/// sequence where it stopped and still issues every invoice that was waiting.
#[derive(Debug, Default)]
pub(in crate::app) struct Invoices {
    last: HashMap<InvoiceSymbol, u32>,
    /// Each invoiced bill's invoice number and lookup code, which a copy of its receipt prints
    /// again ([ADR-0242](../../../../../docs/adr/0242-a-receipt-prints-its-invoice-number-and-lookup-code.md)
    /// decision 3).
    /// With each, when its invoice was issued, in Unix milliseconds, as its event was stamped (ADR-0245's
    /// cancel window).
    invoiced: HashMap<BillId, (IssuedInvoiceRef, u64)>,
    /// Waiting bills by due time then bill.
    pending: BTreeMap<(Timestamp, BillId), Waiting>,
    /// Each waiting bill's due time, to find it in `pending`.
    due: HashMap<BillId, Timestamp>,
}

impl Invoices {
    /// Records an issued invoice, recorded by the event `invoice_id`. The highest number seen wins,
    /// so a replay in any order ends where the live box did. An invoice that was waiting leaves the
    /// queue.
    pub(in crate::app) fn fold(
        &mut self,
        issued: &FiscalInvoiceIssued,
        invoice_id: EventId,
        issued_time: Timestamp,
    ) {
        let last = self
            .last
            .entry(issued.invoice.invoice_symbol.clone())
            .or_insert(0);
        *last = (*last).max(issued.invoice.invoice_sequence);
        self.invoiced.insert(
            issued.bill_id,
            (
                IssuedInvoiceRef::new(
                    issued.invoice.invoice_symbol.as_str(),
                    issued.invoice.invoice_sequence,
                    &invoice_id.to_string(),
                ),
                u64::try_from(issued_time.as_milliseconds_since_epoch()).unwrap_or(0),
            ),
        );
        self.drop_pending(issued.bill_id);
    }

    /// Records a deferred invoice, unless its bill already has one.
    pub(in crate::app) fn fold_scheduled(
        &mut self,
        scheduled: &FiscalInvoiceScheduled,
        settled_by: Actor,
    ) {
        if self.invoiced.contains_key(&scheduled.bill_id) || self.pending.len() >= PENDING_LIMIT {
            return;
        }
        self.drop_pending(scheduled.bill_id);
        let due_time = scheduled.schedule.due_time;
        self.pending.insert(
            (due_time, scheduled.bill_id),
            Waiting {
                buyer_subject_id: scheduled.schedule.buyer_subject_id,
                settled_by,
            },
        );
        self.due.insert(scheduled.bill_id, due_time);
    }

    /// Forgets a waiting invoice, as a void after payment does: it never takes a number, so the
    /// sequence keeps no gap (ADR-0229 decision 3).
    pub(in crate::app) fn drop_pending(&mut self, bill_id: BillId) {
        if let Some(due_time) = self.due.remove(&bill_id) {
            self.pending.remove(&(due_time, bill_id));
        }
    }

    /// Whether the queue can take another deferred invoice.
    pub(in crate::app) fn has_room(&self) -> bool {
        self.pending.len() < PENDING_LIMIT
    }

    /// The waiting invoices due at `now`, oldest first, at most `limit`.
    pub(in crate::app) fn due_at(&self, now: Timestamp, limit: usize) -> Vec<(BillId, Waiting)> {
        self.pending
            .iter()
            .take_while(|((due_time, _), _)| *due_time <= now)
            .take(limit)
            .map(|((_, bill_id), waiting)| (*bill_id, *waiting))
            .collect()
    }

    /// The number the next invoice under `symbol` takes: one after the last, from 1. `None` past
    /// `u32::MAX`, which no country's sequence reaches.
    pub(in crate::app) fn next_under(&self, symbol: &InvoiceSymbol) -> Option<u32> {
        self.last
            .get(symbol)
            .map_or(Some(1), |last| last.checked_add(1))
    }

    /// The number and lookup code of the invoice issued for `bill_id`, if one was.
    pub(in crate::app) fn issued(&self, bill_id: BillId) -> Option<&IssuedInvoiceRef> {
        self.invoiced.get(&bill_id).map(|(reference, _)| reference)
    }

    /// When the invoice issued for `bill_id` was issued, in Unix milliseconds, if one was.
    pub(in crate::app) fn issued_ms(&self, bill_id: BillId) -> Option<u64> {
        self.invoiced.get(&bill_id).map(|(_, issued_ms)| *issued_ms)
    }
}
