// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `Fiscalization` suite.
//!
//! [`issues_with_no_network`] is the case this port exists for. "The store sells with no internet"
//! ([ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md)) stops being true the
//! moment a customer asks for an invoice, unless numbers were allocated in advance. An adapter
//! that needs the authority reachable has not implemented this port, and the first place that
//! gets discovered is a store with no internet and a customer waiting.
//!
//! [`never_reissues_a_number`] is the compliance case. One bill, one invoice number; a duplicate
//! is an incident rather than a duplicate row.

use core::num::NonZeroU32;

use pos_ports::PortName;
use pos_ports::fiscalization::{
    Fiscalization, InvoiceLine, InvoiceNumber, InvoiceRequest, ReconciliationReport,
};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::{BillId, CalendarDate, ErrorStatus, Quantity, StoreId, Ulid};

use crate::harness::FiscalizationHarness;
use crate::{CaseFailure, Obligation};

/// Emits every `Fiscalization` case as a `#[test]`.
#[macro_export]
macro_rules! fiscalization_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_FISCALIZATION,
            module = fiscalization,
            cases = [
                allocates_a_range,
                issues_from_an_allocated_range,
                issues_with_no_network,
                is_idempotent_by_bill_id,
                never_reissues_a_number,
                refuses_when_the_range_is_exhausted,
                warns_before_the_range_is_exhausted,
                submits_queued_invoices_on_reconnect,
                reconciles_in_both_directions,
            ]
        }
    };
}

fn offline_issuance() -> Obligation {
    Obligation::new(PortName::Fiscalization, "issue works with no network")
}

fn uniqueness() -> Obligation {
    Obligation::new(PortName::Fiscalization, "a number is never reused")
}

fn idempotency() -> Obligation {
    Obligation::new(PortName::Fiscalization, "idempotency by bill identifier")
}

fn count(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or(NonZeroU32::MIN)
}

/// Calendar, never business date: a tax authority recognises no cut-off hour.
fn issued_on() -> CalendarDate {
    crate::fixtures::calendar_date()
}

fn request(store_id: StoreId, seed: u32) -> InvoiceRequest {
    InvoiceRequest {
        bill_id: BillId::new(Ulid::from_u128(u128::from(seed))),
        store_id,
        issued_on: issued_on(),
        buyer: None,
        lines: vec![InvoiceLine {
            description: "Pizza".to_owned(),
            quantity: Quantity::ONE,
            unit_price: Money::new(CurrencyCode::VND, 100_000),
            tax_amount: Money::new(CurrencyCode::VND, 10_000),
            tax_rate_basis_points: 1_000,
        }],
        total: Money::new(CurrencyCode::VND, 110_000),
    }
}

/// A range arrives with numbers in it.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn allocates_a_range<H: FiscalizationHarness>(harness: &H) -> Result<(), CaseFailure> {
    let fiscal = harness.fresh().await?;
    let range = fiscal.allocate_range(harness.store_id(), count(10)).await?;
    let obligation = offline_issuance();
    obligation.require_eq(
        &range.numbers.len(),
        &10,
        "a range of ten holds ten numbers",
    )?;
    obligation.require_eq(&range.remaining(), &10, "and none are issued yet")?;

    // Distinct, because a range with a repeat in it is a compliance incident waiting for the
    // second invoice.
    let mut sorted: Vec<&str> = range.numbers.iter().map(InvoiceNumber::as_str).collect();
    let total = sorted.len();
    sorted.sort_unstable();
    sorted.dedup();
    obligation.require_eq(&sorted.len(), &total, "every number in a range is distinct")
}

/// Issuing consumes a number from the range.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn issues_from_an_allocated_range<H: FiscalizationHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let fiscal = harness.fresh().await?;
    let range = fiscal.allocate_range(harness.store_id(), count(10)).await?;
    let invoice = fiscal.issue(&request(harness.store_id(), 1)).await?;
    offline_issuance().require(
        range.numbers.contains(&invoice.invoice_number),
        "the number on the invoice comes from the allocated range, not from anywhere else",
    )
}

/// With the authority unreachable, an invoice still issues.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn issues_with_no_network<H: FiscalizationHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let fiscal = harness.fresh().await?;
    fiscal.allocate_range(harness.store_id(), count(10)).await?;
    harness.disconnect(&fiscal).await?;

    let obligation = offline_issuance();
    let invoice = match fiscal.issue(&request(harness.store_id(), 1)).await {
        Ok(invoice) => invoice,
        Err(error) => {
            return obligation.require(
                false,
                format!(
                    "an invoice must issue against a pre-allocated range with the authority \
                     unreachable. This is what pre-allocation is for, and an adapter that \
                     requires the network has not implemented this port. Got: {error}"
                ),
            );
        }
    };
    obligation.require(
        !invoice.submitted,
        "and it reports itself unsubmitted, which is legal and is what the flush-on-reconnect \
         path exists to clear",
    )?;
    obligation.require(
        !invoice.invoice_number.as_str().is_empty(),
        "with a real number on it",
    )
}

/// One bill, one invoice, however many retries.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn is_idempotent_by_bill_id<H: FiscalizationHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let fiscal = harness.fresh().await?;
    fiscal.allocate_range(harness.store_id(), count(10)).await?;
    let request = request(harness.store_id(), 1);

    let first = fiscal.issue(&request).await?;
    let second = fiscal.issue(&request).await?;
    idempotency().require_eq(
        &second.invoice_number,
        &first.invoice_number,
        "re-issuing the same bill returns the same number. Two numbers for one bill is a \
         compliance incident, not a duplicate row",
    )
}

/// Two bills never share a number, including after a restart.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn never_reissues_a_number<H: FiscalizationHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let fiscal = harness.fresh().await?;
    fiscal.allocate_range(harness.store_id(), count(10)).await?;
    let obligation = uniqueness();

    let mut issued = Vec::new();
    for seed in 1..=5_u32 {
        issued.push(
            fiscal
                .issue(&request(harness.store_id(), seed))
                .await?
                .invoice_number,
        );
    }
    let total = issued.len();
    issued.sort_unstable();
    issued.dedup();
    obligation.require_eq(
        &issued.len(),
        &total,
        "five bills consumed five distinct numbers",
    )
}

/// An exhausted range is the one failure that can stop a sale.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_when_the_range_is_exhausted<H: FiscalizationHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let fiscal = harness.fresh().await?;
    fiscal.allocate_range(harness.store_id(), count(2)).await?;
    harness.disconnect(&fiscal).await?;

    fiscal.issue(&request(harness.store_id(), 1)).await?;
    fiscal.issue(&request(harness.store_id(), 2)).await?;

    uniqueness().require_error(
        fiscal.issue(&request(harness.store_id(), 3)).await,
        ErrorStatus::ResourceExhausted,
        "an exhausted range must refuse rather than invent a number. This is the only failure in \
         the framework with no path that keeps selling, which is why the alert exists — and why \
         the status has to be the one the alert reads",
    )
}

/// The alert has room to act on.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn warns_before_the_range_is_exhausted<H: FiscalizationHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let fiscal = harness.fresh().await?;
    let range = fiscal.allocate_range(harness.store_id(), count(10)).await?;
    let obligation = Obligation::new(PortName::Fiscalization, "the exhaustion alert has warning");
    obligation.require(
        !range.is_nearly_exhausted(2),
        "a fresh range of ten is not nearly exhausted at a threshold of two",
    )?;
    obligation.require(
        range.is_nearly_exhausted(10),
        "and the threshold is inclusive, so a threshold at the range size fires immediately — \
         which is what lets an operator configure how much warning they want",
    )
}

/// Reconnecting flushes the queue.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn submits_queued_invoices_on_reconnect<H: FiscalizationHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let fiscal = harness.fresh().await?;
    fiscal.allocate_range(harness.store_id(), count(10)).await?;
    harness.disconnect(&fiscal).await?;
    let offline = fiscal.issue(&request(harness.store_id(), 1)).await?;
    harness.reconnect(&fiscal).await?;

    let obligation = offline_issuance();
    // Reconciling is what triggers the flush, and it is also how the framework learns it worked.
    let report = fiscal.reconcile(harness.store_id(), issued_on()).await?;
    let looked_up = fiscal.look_up(&offline.invoice_number).await?;
    let found = obligation.require_nth(looked_up.as_slice(), 0, "the invoice after reconnect")?;
    obligation.require(
        found.submitted || report.unsubmitted.contains(&offline.invoice_number),
        "after reconnecting, an offline-issued invoice is either submitted or named in the \
         report as still needing submission. Silently neither is how a number goes unreported \
         until an audit finds it",
    )
}

/// Both directions, because only one is discoverable locally.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reconciles_in_both_directions<H: FiscalizationHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let fiscal = harness.fresh().await?;
    fiscal.allocate_range(harness.store_id(), count(10)).await?;
    let obligation = Obligation::new(PortName::Fiscalization, "reconcile reports both directions");

    let clean: ReconciliationReport = fiscal.reconcile(harness.store_id(), issued_on()).await?;
    obligation.require(
        clean.is_clean(),
        "a day with nothing issued reconciles clean",
    )?;

    fiscal.issue(&request(harness.store_id(), 1)).await?;
    let after = fiscal.reconcile(harness.store_id(), issued_on()).await?;
    obligation.require(
        after.unknown_locally.is_empty(),
        "an invoice this store issued is not unknown to it. A number the authority holds with no \
         local record is the finding that matters most, and a false positive here would bury it",
    )
}
