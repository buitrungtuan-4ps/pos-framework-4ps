// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The half of `Fiscalization` that is the same in every country.
//!
//! # Why this is framework code and not country code
//!
//! Every country's fiscalization splits the same way. One half is **the country's**: what an
//! invoice number looks like, which authority acknowledges it, what a submission carries. The other
//! half is the port's contract, and it is identical everywhere:
//!
//! - a number comes from a **pre-allocated range**, so a store issues one with no internet
//!   ([ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md));
//! - a number is **never reused**, tracked apart from what is available so a second allocation
//!   cannot resurrect a consumed one;
//! - **one bill has one number**, so a retried submission is not a compliance incident;
//! - **exhaustion refuses** rather than inventing a number.
//!
//! Writing that four times would be four chances to get never-reuse subtly wrong, in the one place
//! where being wrong is a conversation with an auditor. So it is written once, here, and a country
//! pack supplies the one thing only it knows: [`InvoiceNumberFormat`].
//!
//! # What a real provider does with it
//!
//! Wraps it. A country whose authority issues numbers online keeps this for the offline path — which
//! is the path that matters, because the alternative is a till that stops selling when the line
//! drops — and flushes to the authority on reconnect, setting `submitted` as the acknowledgement
//! arrives. [ADR-0027](../../../docs/adr/0027-country-modules.md) is the boundary; this is the part
//! of it a fork does not have to write.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use pos_ports::fiscalization::{
    Fiscalization, InvoiceNumber, InvoiceRange, InvoiceRequest, IssuedInvoice, ReconciliationReport,
};
use pos_ports::{PortError, PortName};
use pos_proto::{BillId, CalendarDate, StoreId, Timestamp};

/// How a country writes an invoice number, given the range's series and the number's index in it.
///
/// A function pointer rather than a trait: there is exactly one method, every implementation is a
/// `format!`, and a `fn` keeps [`OfflineFiscalization`] `Clone` and `Debug` without a type parameter
/// spreading through every pack's harness.
///
/// The obligation is uniqueness across ranges, which is why the series is an argument: a format that
/// ignores it hands the same number out twice after a second allocation, and the port's never-reuse
/// guard would then be the only thing standing between a store and a duplicated invoice.
pub type InvoiceNumberFormat = fn(series: u32, index: u32) -> String;

/// Everything this fiscalization has allocated and issued.
#[derive(Debug, Default)]
struct FiscalState {
    /// Numbers allocated and not yet consumed, in issue order.
    available: Vec<InvoiceNumber>,
    /// Issued invoices by bill. One bill, one number.
    issued: BTreeMap<BillId, IssuedInvoice>,
    /// Every number ever consumed, so reuse is impossible even across range allocations.
    consumed: Vec<InvoiceNumber>,
    /// Which range to allocate next, so two ranges never overlap.
    next_series: u32,
}

/// `Fiscalization` over a locally allocated range, in a country's own number format.
///
/// Passes the full `Fiscalization` contract suite. A country pack constructs one with its prefix and
/// format and is done; a country with a real authority wraps one and adds the submission.
#[derive(Debug, Clone)]
pub struct OfflineFiscalization {
    state: Arc<Mutex<FiscalState>>,
    /// Names the range in the ledger. The country's code, ordinarily.
    prefix: &'static str,
    format: InvoiceNumberFormat,
}

impl OfflineFiscalization {
    /// A fiscalization with no ranges allocated, writing numbers in this country's format.
    ///
    /// `prefix` names the *range* in the ledger and the reconciliation report; `format` writes the
    /// numbers themselves. They are separate because a country's number format frequently carries no
    /// stable token an operator could use to name the range it came from.
    #[must_use]
    pub fn new(prefix: &'static str, format: InvoiceNumberFormat) -> Self {
        Self {
            state: Arc::new(Mutex::new(FiscalState::default())),
            prefix,
            format,
        }
    }

    /// Locks the state, recovering from poisoning rather than panicking.
    ///
    /// A poisoned mutex means another thread panicked while holding it. Propagating that as a panic
    /// here would turn one failure into a cascade, and the backbone lints forbid the `unwrap` that
    /// would do it.
    fn lock(&self) -> std::sync::MutexGuard<'_, FiscalState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Fiscalization for OfflineFiscalization {
    async fn allocate_range(
        &self,
        store_id: StoreId,
        count: core::num::NonZeroU32,
    ) -> Result<InvoiceRange, PortError> {
        let mut state = self.lock();
        let series = state.next_series;
        state.next_series = state.next_series.saturating_add(1);

        // The series is what keeps two ranges from overlapping. A real provider is told its prefix by
        // the authority; the property is the same either way — a number is unique for the deployment,
        // not merely within one range.
        let numbers: Vec<InvoiceNumber> = (0..count.get())
            .map(|index| InvoiceNumber::new((self.format)(series, index)))
            .collect();
        state.available.clone_from(&numbers);

        Ok(InvoiceRange {
            store_id,
            range_id: format!("{}-range-{series}", self.prefix).into(),
            numbers,
            issued: 0,
        })
    }

    async fn issue(&self, request: &InvoiceRequest) -> Result<IssuedInvoice, PortError> {
        let mut state = self.lock();

        if let Some(existing) = state.issued.get(&request.bill_id) {
            // One bill, one number, however many times a submission is retried. Two numbers for one
            // bill is a compliance incident rather than a duplicate row.
            return Ok(existing.clone());
        }
        if state.available.is_empty() {
            return Err(PortError::resource_exhausted(
                PortName::Fiscalization,
                "no invoice numbers remain in the allocated range",
            ));
        }

        let invoice_number = state.available.remove(0);
        state.consumed.push(invoice_number.clone());
        let invoice = IssuedInvoice {
            bill_id: request.bill_id,
            invoice_number,
            // A real module stamps the instant it issued. `EPOCH` here rather than a clock reading,
            // because a country module has no `ClockSource` and reading the system clock is what
            // `AGENTS.md` §2 forbids.
            issued_at: Timestamp::EPOCH,
            // Never submitted, because nothing here contacts an authority — and `false` is legal,
            // which is what the flush-on-reconnect path exists to clear.
            submitted: false,
            authority_reference: None,
        };
        state.issued.insert(request.bill_id, invoice.clone());
        Ok(invoice)
    }

    async fn look_up(
        &self,
        invoice_number: &InvoiceNumber,
    ) -> Result<Option<IssuedInvoice>, PortError> {
        let state = self.lock();
        Ok(state
            .issued
            .values()
            .find(|invoice| &invoice.invoice_number == invoice_number)
            .cloned())
    }

    async fn reconcile(
        &self,
        _store_id: StoreId,
        _on: CalendarDate,
    ) -> Result<ReconciliationReport, PortError> {
        let state = self.lock();
        Ok(ReconciliationReport {
            unsubmitted: state
                .issued
                .values()
                .filter(|invoice| !invoice.submitted)
                .map(|invoice| invoice.invoice_number.clone())
                .collect(),
            // Always empty here: no authority is holding numbers this store does not know about. A
            // real module fills this from the authority's records, and it is the direction that
            // matters most — a number consumed with no local record is a gap nobody can explain.
            unknown_locally: Vec::new(),
        })
    }
}
