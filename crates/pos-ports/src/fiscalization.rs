// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Legal invoicing, per country.
//!
//! # Four operations, and the first one is what makes offline selling legal
//!
//! `docs/roadmap.md` P10 fixes the surface: allocate a range, issue, look up, reconcile.
//! Pre-allocated number ranges are the whole design — a store holding a block of numbers can
//! issue a legally-numbered invoice with no internet, queue the submission, and flush it on
//! reconnect. Without pre-allocation, "sells with no internet"
//! ([ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md)) would stop being true
//! the moment a customer asked for an invoice.
//!
//! # An invoice number is not a receipt number
//!
//! `docs/pos-spec.md` §5 keeps them apart and the distinction is legal, not stylistic. The
//! receipt number is a gapless per-store counter the framework owns. The invoice number comes
//! from a range the tax authority allocated, and duplicating one is a compliance incident. A
//! machine replacement therefore hands the new machine a **fresh range** rather than resuming
//! the old one, so even an overlapping window cannot reissue a number.
//!
//! # Calendar date, never business date
//!
//! A business date runs to the store's cut-off hour, which may be 04:00. The tax authority
//! recognises no such thing. So [`InvoiceRequest::issued_on`] is a
//! [`CalendarDate`](pos_proto::CalendarDate), and `pos-proto` makes the two types mutually
//! unconvertible so this cannot be got wrong by assignment.
//!
//! # Buyer details are personal data
//!
//! `buyer_name`, `buyer_tax_code` and `buyer_email` are what a corporate invoice needs, and
//! they are exactly what may not enter the immutable event log. So [`InvoiceBuyer`] lives
//! here, crosses this port, and is stored in the personal-data side table keyed by
//! `subject_id` — never in an event payload. Under Vietnam's PDPD (Decree 13/2023) that
//! record has a lawful basis, a retention period, and a masking job; `docs/roadmap.md` A6
//! tracks all three.

use core::fmt;
use core::future::Future;
use core::num::NonZeroU32;

use pos_proto::money::Money;
use pos_proto::{BillId, CalendarDate, StoreId, SubjectId, Timestamp};

use crate::error::PortError;

/// A legal invoice number, as the authority defines it.
///
/// Text rather than an integer, because the format is the authority's: a series prefix, a
/// template code, a year, and a sequence, in a combination that varies by country and by
/// provider. Parsing it into parts would be inventing a schema on the authority's behalf.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InvoiceNumber(Box<str>);

impl InvoiceNumber {
    /// Wraps an invoice number.
    #[must_use]
    pub fn new(number: impl Into<Box<str>>) -> Self {
        Self(number.into())
    }

    /// The number as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InvoiceNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for InvoiceNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InvoiceNumber({})", self.0)
    }
}

/// A block of numbers a store may issue from, offline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvoiceRange {
    /// Which store holds it. A range is never shared between stores or between machines.
    pub store_id: StoreId,
    /// The authority's identifier for this range, quoted when reporting or extending it.
    pub range_id: Box<str>,
    /// Every number in the block, in issue order.
    ///
    /// Materialised rather than expressed as a first-and-count, because the format is the
    /// authority's and "the next number after this one" is not something the framework may
    /// compute. A range is thousands of entries, allocated rarely.
    pub numbers: Vec<InvoiceNumber>,
    /// How many have been issued so far.
    pub issued: u32,
}

impl InvoiceRange {
    /// How many numbers are left.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.numbers.len().saturating_sub(self.issued as usize)
    }

    /// Whether the range is at or below `threshold` remaining.
    ///
    /// This is the trigger for the alert `docs/pos-spec.md` §18 lists among the six that
    /// matter, and it is worth understanding why it is the only one that can eventually stop
    /// a sale: every other degraded state in this system has a path that keeps selling, and
    /// an exhausted invoice range does not, because issuing an invoice without a number is
    /// not a thing that can be done later.
    #[must_use]
    pub fn is_nearly_exhausted(&self, threshold: usize) -> bool {
        self.remaining() <= threshold
    }
}

/// Who the invoice is for.
///
/// Personal data. Never in an event payload; stored in the side table keyed by
/// [`Self::subject_id`], which is the identifier the event log carries instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvoiceBuyer {
    /// The key the event log uses to refer to this person without holding their details.
    pub subject_id: SubjectId,
    /// The buyer's name, as it must appear on the invoice.
    pub buyer_name: String,
    /// Their tax code, for a corporate invoice.
    pub buyer_tax_code: Option<String>,
    /// Where to send it.
    pub buyer_email: Option<String>,
    /// Their address, where the authority requires it.
    pub buyer_address: Option<String>,
}

/// Deliberately hand-written: personal data must not reach a log through `{:?}`.
///
/// `AGENTS.md` §2 forbids personal data in logs, and a derived `Debug` on this type would put
/// a buyer's name and address into any span that carried it.
impl fmt::Debug for InvoiceBuyerRedacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InvoiceBuyer")
            .field("subject_id", &self.0.subject_id)
            .finish_non_exhaustive()
    }
}

/// A redacting view of an [`InvoiceBuyer`], for logging.
///
/// The buyer type itself derives `Debug` so tests can compare and print it; this wrapper is
/// what production code logs. Making the redaction a separate, named type is the honest
/// arrangement: a redacting `Debug` on the buyer would make a test that *wants* to see the
/// value impossible to write, and people then reach for `{:#?}` on the containing struct
/// instead, which defeats it.
pub struct InvoiceBuyerRedacted<'a>(pub &'a InvoiceBuyer);

impl InvoiceBuyer {
    /// A view of this buyer that is safe to log.
    #[must_use]
    pub const fn redacted(&self) -> InvoiceBuyerRedacted<'_> {
        InvoiceBuyerRedacted(self)
    }
}

/// One line as the authority wants it.
///
/// Deliberately not the domain's order line: an invoice line carries the tax treatment and the
/// authority's own item description, and `docs/pos-spec.md` §5's tax model is per item class
/// keyed by sales channel, so the rate is stated here rather than recomputed by the adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvoiceLine {
    /// What was sold, in the words the invoice shows.
    pub description: String,
    /// How many, in thousandths.
    pub quantity: pos_proto::Quantity,
    /// Price per unit, before tax.
    pub unit_price: Money,
    /// Tax on this line.
    pub tax_amount: Money,
    /// The tax rate applied, in basis points, so 10% is 1000 and 8% is 800.
    ///
    /// Integer basis points because there is no floating point in this workspace, and because
    /// a tax rate rendered as `0.09999999` on a legal document is a conversation with an
    /// auditor.
    pub tax_rate_basis_points: u32,
}

/// A request to issue an invoice.
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceRequest {
    /// The bill being invoiced. The idempotency key: one bill, one invoice, however many
    /// times the submission is retried.
    pub bill_id: BillId,
    /// Which store.
    pub store_id: StoreId,
    /// The **calendar** date, not the business date. See this module's documentation.
    pub issued_on: CalendarDate,
    /// Who it is for, when the customer asked for one in their name.
    pub buyer: Option<InvoiceBuyer>,
    /// The lines.
    pub lines: Vec<InvoiceLine>,
    /// The total, tax included.
    pub total: Money,
}

/// An invoice that has a number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedInvoice {
    /// The bill it belongs to.
    pub bill_id: BillId,
    /// The number taken from the pre-allocated range.
    pub invoice_number: InvoiceNumber,
    /// When it was issued locally.
    pub issued_at: Timestamp,
    /// Whether the authority has acknowledged it.
    ///
    /// `false` for an invoice issued offline and still queued — which is legal, and is the
    /// state the flush-on-reconnect path exists to clear.
    pub submitted: bool,
    /// The authority's own reference, once it has one.
    pub authority_reference: Option<Box<str>>,
}

/// What a reconciliation found.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ReconciliationReport {
    /// Invoices the store issued that the authority has not acknowledged.
    pub unsubmitted: Vec<InvoiceNumber>,
    /// Invoices the authority holds that the store has no record of.
    ///
    /// The direction that matters most: a number consumed without a local record is a gap
    /// nobody can explain later, and finding it the next day is far better than finding it in
    /// an audit.
    pub unknown_locally: Vec<InvoiceNumber>,
}

impl ReconciliationReport {
    /// Whether both sides agree.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.unsubmitted.is_empty() && self.unknown_locally.is_empty()
    }
}

/// Issues legal invoices for one country.
///
/// # Contract
///
/// 1. **`issue` is idempotent by [`InvoiceRequest::bill_id`].** Re-issuing returns the same
///    number. One bill has exactly one invoice number, and a second one is a compliance
///    incident rather than a duplicate row.
/// 2. **`issue` works with no network**, consuming from an allocated range and returning
///    `submitted: false`. An adapter that requires the authority to be reachable has not
///    implemented this port.
/// 3. **A number is never reused**, across restarts and across machine replacement. The
///    replacement path allocates a fresh range rather than resuming.
/// 4. **`reconcile` reports both directions**, because only one of them is discoverable from
///    local data.
pub trait Fiscalization: Send + Sync {
    /// Asks the authority for a block of numbers.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the authority cannot be reached — the ordinary case
    /// offline, and the reason ranges are allocated well ahead of need;
    /// [`PortError::permission_denied`] if the deployment's registration does not permit it;
    /// [`PortError::resource_exhausted`] if the authority will not allocate more today.
    fn allocate_range(
        &self,
        store_id: StoreId,
        count: NonZeroU32,
    ) -> impl Future<Output = Result<InvoiceRange, PortError>> + Send;

    /// Issues an invoice, offline if necessary.
    ///
    /// # Errors
    ///
    /// [`PortError::resource_exhausted`] if no numbers remain — the one failure in this
    /// framework that can stop a sale, which is why
    /// [`InvoiceRange::is_nearly_exhausted`] exists; [`PortError::invalid_argument`] if the
    /// request is not something the authority accepts, such as a line total that does not sum.
    fn issue(
        &self,
        request: &InvoiceRequest,
    ) -> impl Future<Output = Result<IssuedInvoice, PortError>> + Send;

    /// Looks up an invoice by number.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the authority cannot be reached. An unknown number is
    /// `Ok(None)`, because "the authority has never heard of this" is a finding the
    /// reconciliation needs rather than an exception.
    fn look_up(
        &self,
        invoice_number: &InvoiceNumber,
    ) -> impl Future<Output = Result<Option<IssuedInvoice>, PortError>> + Send;

    /// Compares the store's record against the authority's for a calendar day.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the authority cannot be reached.
    fn reconcile(
        &self,
        store_id: StoreId,
        on: CalendarDate,
    ) -> impl Future<Output = Result<ReconciliationReport, PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{InvoiceBuyer, InvoiceNumber, InvoiceRange, ReconciliationReport};
    use pos_proto::{StoreId, SubjectId, Ulid};

    fn range(count: usize, issued: u32) -> InvoiceRange {
        InvoiceRange {
            store_id: StoreId::new(Ulid::from_u128(1)),
            range_id: "range-1".into(),
            numbers: (0..count)
                .map(|index| InvoiceNumber::new(format!("1C25TAA/{index:06}")))
                .collect(),
            issued,
        }
    }

    #[test]
    fn the_exhaustion_alert_fires_before_the_last_number() {
        // This is the only alert in the system whose failure mode is "cannot sell", so it
        // has to fire with room to act on it.
        let plenty = range(1_000, 100);
        assert_eq!(plenty.remaining(), 900);
        assert!(!plenty.is_nearly_exhausted(100));

        let nearly = range(1_000, 950);
        assert_eq!(nearly.remaining(), 50);
        assert!(nearly.is_nearly_exhausted(100));
    }

    #[test]
    fn a_range_issued_past_its_end_reports_zero_rather_than_underflowing() {
        // An `issued` count above the block size should not be possible, and if it happens
        // the answer must be "none left" rather than a panic or a huge number — both of
        // which would be worse than the bug that caused it.
        let overrun = range(10, 25);
        assert_eq!(overrun.remaining(), 0);
        assert!(overrun.is_nearly_exhausted(0));
    }

    #[test]
    fn a_buyer_does_not_reach_a_log_through_the_redacting_view() {
        let buyer = InvoiceBuyer {
            subject_id: SubjectId::new(Ulid::from_u128(42)),
            buyer_name: "Cong ty ABC".to_owned(),
            buyer_tax_code: Some("0101234567".to_owned()),
            buyer_email: Some("ke.toan@example.com".to_owned()),
            buyer_address: Some("1 Nguyen Hue".to_owned()),
        };
        let logged = format!("{:?}", buyer.redacted());
        assert!(logged.contains("subject_id"));
        for personal in [
            "Cong ty ABC",
            "0101234567",
            "ke.toan@example.com",
            "Nguyen Hue",
        ] {
            assert!(
                !logged.contains(personal),
                "{personal} reached a log: {logged}"
            );
        }
    }

    #[test]
    fn a_clean_reconciliation_is_empty_in_both_directions() {
        assert!(ReconciliationReport::default().is_clean());

        let authority_has_extra = ReconciliationReport {
            unsubmitted: Vec::new(),
            unknown_locally: vec![InvoiceNumber::new("1C25TAA/000042")],
        };
        assert!(
            !authority_has_extra.is_clean(),
            "a number consumed with no local record is the finding that matters most"
        );
    }
}
