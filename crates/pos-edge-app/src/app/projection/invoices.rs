// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The e-invoices a store has issued and the ones it is waiting to issue
//! ([ADR-0225](../../../../../docs/adr/0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md),
//! [ADR-0229](../../../../../docs/adr/0229-the-e-invoice-issue-moment-is-a-schedule.md)): the last
//! number under each symbol, which the next invoice continues from; the bills with one, which a void
//! after payment refuses and a receipt's copy prints again; and the deferred invoices in due order.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use pos_core::decision::Actor;
use pos_proto::einvoice::{DecidedCorrection, InvoiceSymbol, VendorNumber};
use pos_proto::events::{FiscalInvoiceIssued, FiscalInvoiceScheduled};
use pos_proto::ids::{BillId, EventId, SubjectId};
use pos_proto::time::Timestamp;

use crate::app::IssuedInvoiceRef;

/// The most deferred invoices a store holds at once (AGENTS.md §2). A settle that finds the queue
/// full issues at once rather than refuse the sale or lose the invoice (ADR-0229 decision 4).
pub(in crate::app) const PENDING_LIMIT: usize = 10_000;

/// The most bills a store remembers asking HQ to correct (ADR-0250). Past it the oldest bill is
/// forgotten, and a decision on it is not recorded again; the till still honours it.
pub(in crate::app) const CORRECTIONS_LIMIT: usize = 10_000;

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
    /// The bills this store asked HQ to correct, folded from `fiscal.correction.requested`, each
    /// with the `decided_time` of the decision `fiscal.correction.decided` last recorded for it
    /// ([ADR-0250](../../../../../docs/adr/0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)
    /// decision 3). At most [`CORRECTIONS_LIMIT`].
    corrections: BTreeMap<BillId, Option<Timestamp>>,
    /// The bills whose invoice was issued "number pending" and whose number the vendor has not yet
    /// given (ADR-0251), at most [`PENDING_LIMIT`]. While any waits, the store takes no number of
    /// its own, so it never numbers over one the vendor is about to give (decision 5).
    awaiting_number: BTreeSet<BillId>,
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
        let reference = if issued.invoice.number_pending() {
            if self.awaiting_number.len() < PENDING_LIMIT {
                self.awaiting_number.insert(issued.bill_id);
            }
            IssuedInvoiceRef::pending(&invoice_id.to_string())
        } else {
            let last = self
                .last
                .entry(issued.invoice.invoice_symbol.clone())
                .or_insert(0);
            *last = (*last).max(issued.invoice.invoice_sequence);
            IssuedInvoiceRef::new(
                issued.invoice.invoice_symbol.as_str(),
                issued.invoice.invoice_sequence,
                &invoice_id.to_string(),
            )
        };
        self.invoiced.insert(
            issued.bill_id,
            (
                reference,
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
    /// Folds the vendor's number for a bill whose invoice was issued "number pending" (ADR-0251
    /// decision 4): the bill's reference takes it, the bill no longer waits, and a number under a
    /// symbol counts as that symbol's, so a store switched back to the POS numbers after it
    /// (decision 5).
    pub(in crate::app) fn fold_numbered(&mut self, numbered: &VendorNumber) {
        self.awaiting_number.remove(&numbered.bill_id);
        if let Some((reference, _)) = self.invoiced.get_mut(&numbered.bill_id) {
            reference.number = Some(numbered.invoice_number.as_str().to_owned());
        }
        let parsed =
            numbered
                .invoice_number
                .as_str()
                .rsplit_once('/')
                .and_then(|(symbol, sequence)| {
                    Some((InvoiceSymbol::parse(symbol)?, sequence.parse::<u32>().ok()?))
                });
        if let Some((symbol, sequence)) = parsed {
            let last = self.last.entry(symbol).or_insert(0);
            *last = (*last).max(sequence);
        }
    }

    /// The published numbers for bills still waiting for one, newest per bill.
    pub(in crate::app) fn unnumbered(&self, published: &[VendorNumber]) -> Vec<VendorNumber> {
        let mut newest: BTreeMap<BillId, &VendorNumber> = BTreeMap::new();
        for numbered in published {
            newest.insert(numbered.bill_id, numbered);
        }
        newest
            .into_values()
            .filter(|numbered| self.awaiting_number.contains(&numbered.bill_id))
            .cloned()
            .collect()
    }

    /// Whether any invoice issued "number pending" still waits for the vendor's number.
    pub(in crate::app) fn awaits_vendor_numbers(&self) -> bool {
        !self.awaiting_number.is_empty()
    }

    pub(in crate::app) fn issued(&self, bill_id: BillId) -> Option<&IssuedInvoiceRef> {
        self.invoiced.get(&bill_id).map(|(reference, _)| reference)
    }

    /// When the invoice issued for `bill_id` was issued, in Unix milliseconds, if one was.
    pub(in crate::app) fn issued_ms(&self, bill_id: BillId) -> Option<u64> {
        self.invoiced.get(&bill_id).map(|(_, issued_ms)| *issued_ms)
    }

    /// Remembers that this store asked HQ to correct `bill_id`'s invoice. Past
    /// [`CORRECTIONS_LIMIT`] the oldest bill (the smallest id, which a ULID orders by time) goes.
    pub(in crate::app) fn fold_requested(&mut self, bill_id: BillId) {
        if self.corrections.contains_key(&bill_id) {
            return;
        }
        if self.corrections.len() >= CORRECTIONS_LIMIT {
            self.corrections.pop_first();
        }
        self.corrections.insert(bill_id, None);
    }

    /// Remembers that `decided` was recorded, for a bill this store asked about.
    pub(in crate::app) fn fold_decided(&mut self, decided: &DecidedCorrection) {
        if let Some(recorded) = self.corrections.get_mut(&decided.bill_id) {
            *recorded = Some(decided.decided_time);
        }
    }

    /// The decisions of `published` on bills this store asked about that it has not recorded yet:
    /// each bill's newest entry, unless its `decided_time` is the one last recorded.
    pub(in crate::app) fn unrecorded(
        &self,
        published: &[DecidedCorrection],
    ) -> Vec<DecidedCorrection> {
        let mut newest: BTreeMap<BillId, &DecidedCorrection> = BTreeMap::new();
        for decided in published {
            newest.insert(decided.bill_id, decided);
        }
        newest
            .into_values()
            .filter(|decided| {
                self.corrections
                    .get(&decided.bill_id)
                    .is_some_and(|recorded| *recorded != Some(decided.decided_time))
            })
            .cloned()
            .collect()
    }
}
