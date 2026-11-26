// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The e-invoice a settle issues
//! ([ADR-0225](../../../../../docs/adr/0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md),
//! [ADR-0229](../../../../../docs/adr/0229-the-e-invoice-issue-moment-is-a-schedule.md)): numbered
//! under the store's symbol in the sale's own transaction, or recorded as due and numbered when it
//! falls due, written as `fiscal.invoice.issued`, and submitted later by the cloud.

use pos_core::business_date::{CutoffHour, derive_business_date};
use pos_core::decision::{Actor, DecisionCtx};
use pos_core::einvoice::{IssueSchedule, due_time};
use pos_ports::event_store::EventStore;
use pos_proto::ClockSource;
use pos_proto::einvoice::{
    EInvoiceKind, EInvoiceNumberSource, FiscalInvoice, InvoiceSchedule, InvoiceSymbol,
};
use pos_proto::events::{BillingBillSettled, FiscalInvoiceIssued, FiscalInvoiceScheduled};
use pos_proto::ids::{BillId, EventId, SubjectId};
use pos_proto::integrations::IntegrationFamily;
use pos_proto::money::Money;
use pos_proto::time::{CalendarDate, Timestamp};
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use crate::app::{AppError, Edge, IssuedInvoiceRef};

/// How many due invoices one tick issues, so a long queue never holds the lock for long.
const ISSUED_PER_TICK: usize = 100;

/// What a settle does about its e-invoice (ADR-0229 decision 1).
pub(in crate::app) enum InvoicePlan {
    /// Nothing: the store issues none for this bill.
    None,
    /// Issue it in the settle's own transaction, under this symbol.
    Now(InvoiceSymbol),
    /// Record it as due later; it takes its number when it is issued (decision 3).
    Later,
}

/// The invoice record a settle writes in its own transaction.
pub(in crate::app) enum SettleInvoice {
    /// Issued now.
    Issued(FiscalInvoiceIssued),
    /// Due later.
    Scheduled(FiscalInvoiceScheduled),
}

impl<S: EventStore> Edge<S> {
    /// The invoice record `plan` asks of the settle of `settled`, if any.
    ///
    /// # Errors
    ///
    /// As [`Self::issued_invoice`] and [`Self::scheduled_invoice`].
    pub(in crate::app) fn settle_invoice(
        &self,
        ctx: &DecisionCtx,
        plan: InvoicePlan,
        settled: &BillingBillSettled,
    ) -> Result<Option<SettleInvoice>, AppError> {
        let buyer = settled.buyer_subject_id;
        Ok(match plan {
            InvoicePlan::None => None,
            InvoicePlan::Now(symbol) => Some(SettleInvoice::Issued(self.issued_invoice(
                ctx,
                symbol,
                (settled.bill_id, settled.total_due, buyer),
            )?)),
            InvoicePlan::Later => self
                .scheduled_invoice(ctx, settled.bill_id, buyer)?
                .map(SettleInvoice::Scheduled),
        })
    }

    /// What a settle does about its invoice under the store's `einvoice` node.
    ///
    /// Asked before the receipt number is taken, so a refused settle consumes neither. A deferred
    /// mode whose queue is full issues now rather than refuse the sale (decision 4).
    ///
    /// # Errors
    ///
    /// [`AppError::EInvoiceSymbolMissing`] when the bill must have an invoice and the cloud has
    /// published no symbol: the sale is not taken, deferred or not.
    pub(in crate::app) fn invoice_plan(&self, has_buyer: bool) -> Result<InvoicePlan, AppError> {
        let einvoice = &self.session().einvoice;
        let moment = einvoice.issue_moment();
        if !moment.issues(has_buyer) {
            return Ok(InvoicePlan::None);
        }
        let symbol = einvoice.symbol().ok_or(AppError::EInvoiceSymbolMissing)?;
        if moment.is_deferred() && self.lock_projection().invoices.has_room() {
            Ok(InvoicePlan::Later)
        } else {
            Ok(InvoicePlan::Now(symbol))
        }
    }

    /// The deferred record for a bill settled at `ctx.now`, due by the store's schedule and never
    /// after the country's legal limit (decision 2). `None` if the schedule names no later moment.
    ///
    /// # Errors
    ///
    /// [`AppError::Clock`] if the due time cannot be worked out in the store's zone.
    pub(in crate::app) fn scheduled_invoice(
        &self,
        ctx: &DecisionCtx,
        bill_id: BillId,
        buyer_subject_id: Option<SubjectId>,
    ) -> Result<Option<FiscalInvoiceScheduled>, AppError> {
        let session = self.session();
        let schedule = IssueSchedule {
            moment: session.einvoice.issue_moment(),
            delay_minutes: session.einvoice.delay_minutes(),
            batch_minute: session.einvoice.batch_minute(),
        };
        let limit = self.issue_limit.read().ok().and_then(|limit| *limit);
        let due = due_time(schedule, ctx.now, &session.timezone, limit)
            .map_err(|_clock| AppError::Clock)?;
        Ok(due.map(|due_time| FiscalInvoiceScheduled {
            bill_id,
            schedule: InvoiceSchedule {
                due_time,
                buyer_subject_id,
            },
        }))
    }

    /// The invoice for `bill_id`, numbered one after the last under `symbol` and dated `ctx.now`.
    ///
    /// The caller holds [`Edge::invoicing`] from here until the invoice is folded, so no second
    /// invoice reads the same last number.
    ///
    /// # Errors
    ///
    /// [`AppError::Clock`] if the instant has no calendar date in the store's zone, and
    /// [`AppError::EInvoiceSymbolMissing`] if the symbol has no number left.
    pub(in crate::app) fn issued_invoice(
        &self,
        ctx: &DecisionCtx,
        symbol: InvoiceSymbol,
        (bill_id, total, buyer_subject_id): (BillId, Money, Option<SubjectId>),
    ) -> Result<FiscalInvoiceIssued, AppError> {
        let session = self.session();
        let source = self.number_source_now(session.einvoice.number_source());
        let sequence = if source == EInvoiceNumberSource::Pos {
            self.lock_projection()
                .invoices
                .next_under(&symbol)
                .ok_or(AppError::EInvoiceSymbolMissing)?
        } else {
            // Issued "number pending": the store takes no number, so its sequence never moves.
            0
        };
        Ok(FiscalInvoiceIssued {
            bill_id,
            invoice: FiscalInvoice {
                kind: Open::from_known(EInvoiceKind::Original),
                invoice_symbol: symbol,
                invoice_sequence: sequence,
                issued_on: calendar_date(ctx.now, &session.timezone)?,
                total,
                buyer_subject_id,
                // The connection serving the store now: the one the cloud submits it through, so a
                // switch of provider reaches only later invoices (decision 5).
                connection_id: session
                    .integrations
                    .in_family(IntegrationFamily::EInvoice)
                    .next()
                    .and_then(|connection| connection.connection_id.parse::<Ulid>().ok()),
                number_source: Open::from_known(source),
            },
        })
    }

    /// Who numbers an invoice issued now under the store's `configured` source (ADR-0251 decisions 5
    /// and 6). A store switched back to the POS keeps issuing "number pending" while any invoice
    /// still waits for the vendor's number, so it never numbers over one the vendor is about to
    /// give. `HYBRID` without an offline symbol is issued "number pending", as `VENDOR` is.
    fn number_source_now(&self, configured: EInvoiceNumberSource) -> EInvoiceNumberSource {
        match configured {
            EInvoiceNumberSource::Pos | EInvoiceNumberSource::Unspecified => {
                if self.lock_projection().invoices.awaits_vendor_numbers() {
                    EInvoiceNumberSource::Vendor
                } else {
                    EInvoiceNumberSource::Pos
                }
            }
            source @ (EInvoiceNumberSource::Vendor | EInvoiceNumberSource::Hybrid) => source,
        }
    }

    /// Issues every deferred invoice due by now, in due order, each in its own transaction and
    /// each taking the next number then (ADR-0229 decision 3); answers how many it issued.
    ///
    /// A bill voided while it waited left the queue and is not issued. With no symbol published,
    /// nothing is issued and the queue waits for one. The edge's own loop calls this every minute.
    ///
    /// # Errors
    ///
    /// The first store or clock failure; the invoices before it stay issued.
    pub async fn issue_due_invoices(&self) -> Result<usize, AppError> {
        let _invoicing = self.invoicing.lock().await;
        let due = self
            .lock_projection()
            .invoices
            .due_at(self.clock.now(), ISSUED_PER_TICK);
        let mut issued_count = 0;
        for (bill_id, waiting) in due {
            let Some(symbol) = self.session().einvoice.symbol() else {
                break;
            };
            let total = self
                .lock_projection()
                .settled
                .get(&bill_id)
                .map(|receipt| receipt.total_due);
            let Some(total) = total else {
                self.lock_projection().invoices.drop_pending(bill_id);
                continue;
            };
            let ctx = self.decision_ctx(waiting.settled_by)?;
            let issued =
                self.issued_invoice(&ctx, symbol, (bill_id, total, waiting.buyer_subject_id))?;
            let (envelope, message) = self.prepare_on(&ctx, &issued, None)?;
            let invoice_id = envelope.event_id;
            self.append_and_publish(vec![envelope], vec![message])
                .await?;
            self.lock_projection()
                .invoices
                .fold(&issued, invoice_id, ctx.now);
            issued_count += 1;
        }
        Ok(issued_count)
    }
}

impl<S: EventStore> Edge<S> {
    /// Folds the invoice a settle committed, recorded by the event `invoice_id`, and answers the
    /// number and lookup code of one issued there and then (ADR-0242 decisions 1 to 3).
    pub(in crate::app) fn fold_settle_invoice(
        &self,
        record: &SettleInvoice,
        invoice_id: EventId,
        settled_by: Actor,
        settled_time: Timestamp,
    ) -> Option<IssuedInvoiceRef> {
        let mut projection = self.lock_projection();
        match record {
            SettleInvoice::Issued(issued) => {
                projection.invoices.fold(issued, invoice_id, settled_time);
                projection.invoices.issued(issued.bill_id).cloned()
            }
            SettleInvoice::Scheduled(scheduled) => {
                projection.invoices.fold_scheduled(scheduled, settled_by);
                None
            }
        }
    }
}

/// The calendar date of `instant` in the store's zone: midnight, never the store's cut-off.
fn calendar_date(
    instant: Timestamp,
    zone: &pos_core::business_date::StoreTimeZone,
) -> Result<CalendarDate, AppError> {
    let civil = CutoffHour::new(0)
        .and_then(|midnight| derive_business_date(instant, zone, midnight))
        .map_err(|_clock| AppError::Clock)?;
    CalendarDate::from_ymd(civil.year(), civil.month(), civil.day())
        .map_err(|_clock| AppError::Clock)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod vendor_tests;
