// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! How the cloud numbers and records a correction of an issued invoice
//! ([ADR-0241](../../../../docs/adr/0241-the-cloud-submits-each-issued-invoice-from-the-log-and-keeps-its-state.md)
//! decision 5, [ADR-0245](../../../../docs/adr/0245-an-invoiced-bill-is-corrected-by-the-route-the-store-chooses.md)).
//!
//! One path for both callers: the console's `POST /admin/einvoices/{invoice_id}/correct`, and the
//! feed when a store voids a bill whose invoice was issued. A correction is a new invoice under the
//! tenant's correction symbol (ADR-0240 decision 6), the next number of that symbol's gapless
//! sequence, recorded `PENDING`, and it never takes an invoice below zero. Which errors need which
//! kind is to be confirmed by legal or the accountant.

use std::sync::Arc;

use pos_country::CountryModule;
use pos_proto::einvoice::{EInvoiceKind, PublishedEInvoice};
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::locale::CountryCode;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use store_postgres::SubmissionRow;

use super::feed::{FeedFuture, STATE_PENDING, SubmissionStore, kind_token};
use super::symbols::{SymbolLedger, allocate};
use crate::config_tree::ConfigTreeStore;
use crate::http::config_nodes::utc_year;

/// Why a correction was not recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionError {
    /// The adjustment would take the invoice below zero.
    BelowZero,
    /// The store's country has no rule to allocate the tenant's correction symbol from.
    NoRule,
    /// The named store could not be read or written.
    Unavailable(&'static str),
}

/// A correction to record: its kind, and for an adjustment the change (negative for down), for a
/// replacement the new total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Correction {
    /// `ADJUSTMENT` or `REPLACEMENT`.
    pub kind: EInvoiceKind,
    /// The change or the new total, in the minor unit.
    pub total_minor: i64,
}

/// What the invoice stands at after its corrections so far: its total, replaced by each replacement
/// and moved by each adjustment, in the order they were recorded.
#[must_use]
pub fn standing_total(original: &SubmissionRow, earlier: &[SubmissionRow]) -> i64 {
    earlier.iter().fold(original.total_minor, |total, row| {
        if row.kind == "REPLACEMENT" {
            row.total_minor
        } else {
            total.saturating_add(row.total_minor)
        }
    })
}

/// The correction a void of the whole bill needs under `kind`: an adjustment of the standing total
/// down to zero, or a replacement of zero. `None` once the invoice already stands at zero or below,
/// so a void read twice is corrected once.
#[must_use]
pub fn void_correction(kind: EInvoiceKind, standing: i64) -> Option<Correction> {
    (standing > 0).then(|| match kind {
        EInvoiceKind::Replacement => Correction {
            kind: EInvoiceKind::Replacement,
            total_minor: 0,
        },
        EInvoiceKind::Unspecified | EInvoiceKind::Original | EInvoiceKind::Adjustment => {
            Correction {
                kind: EInvoiceKind::Adjustment,
                total_minor: standing.saturating_neg(),
            }
        }
    })
}

/// Numbers `correction` of `original` under the tenant's correction symbol and records it as
/// `invoice_id`, `PENDING` and due at once. The symbol is the ledger's for this year, or allocated
/// from `country`'s rule.
///
/// # Errors
///
/// [`CorrectionError::BelowZero`] for an adjustment past zero; [`CorrectionError::NoRule`] when no
/// symbol can be allocated; [`CorrectionError::Unavailable`] when a store cannot be reached.
pub async fn record_correction(
    submissions: &dyn SubmissionStore,
    ledger: &dyn SymbolLedger,
    country: Option<&dyn CountryModule>,
    original: &SubmissionRow,
    correction: Correction,
    invoice_id: Ulid,
    now: Timestamp,
) -> Result<SubmissionRow, CorrectionError> {
    let tenant = original
        .tenant_id
        .parse::<Ulid>()
        .map(TenantId::new)
        .map_err(|_| CorrectionError::Unavailable("e-invoice submissions"))?;
    let earlier = submissions
        .corrections_of(tenant, original.invoice_number.clone())
        .await
        .map_err(|_| CorrectionError::Unavailable("e-invoice submissions"))?;
    let standing = standing_total(original, &earlier);
    if correction.kind == EInvoiceKind::Adjustment
        && standing.saturating_add(correction.total_minor) < 0
    {
        return Err(CorrectionError::BelowZero);
    }
    let symbol = correction_symbol(ledger, country, tenant, now).await?;
    let last = submissions
        .last_number(tenant, symbol.clone())
        .await
        .map_err(|_| CorrectionError::Unavailable("e-invoice submissions"))?;
    let sequence = last
        .and_then(|number| {
            number
                .rsplit('/')
                .next()
                .and_then(|n| n.parse::<u32>().ok())
        })
        .unwrap_or(0)
        .saturating_add(1);
    let now_ms = now.as_milliseconds_since_epoch();
    let row = SubmissionRow {
        invoice_id: invoice_id.to_string(),
        invoice_number: format!("{symbol}/{sequence:08}"),
        kind: kind_token(correction.kind).to_owned(),
        issued_on: utc_date(now_ms),
        total_minor: correction.total_minor,
        buyer_subject_id: None,
        corrects_invoice_number: Some(original.invoice_number.clone()),
        state: STATE_PENDING.to_owned(),
        failed_attempts: 0,
        next_attempt_ms: now_ms,
        authority_reference: None,
        refusal: None,
        recorded_at_ms: now_ms,
        updated_at_ms: now_ms,
        ..original.clone()
    };
    submissions
        .record(row.clone())
        .await
        .map_err(|_| CorrectionError::Unavailable("e-invoice submissions"))?;
    Ok(row)
}

/// The tenant's correction symbol for this year: the ledger's, or a new one allocated from
/// `country`'s rule (ADR-0240 decision 6).
async fn correction_symbol(
    ledger: &dyn SymbolLedger,
    country: Option<&dyn CountryModule>,
    tenant: TenantId,
    now: Timestamp,
) -> Result<String, CorrectionError> {
    let year = utc_year(now);
    let year_digits = format!("{:02}", year.rem_euclid(100));
    let held = ledger
        .list(tenant)
        .await
        .map_err(|_| CorrectionError::Unavailable("e-invoice symbol ledger"))?;
    if let Some(record) = held.iter().find(|record| {
        record.store_id.is_none() && record.symbol.get(2..4) == Some(year_digits.as_str())
    }) {
        return Ok(record.symbol.clone());
    }
    allocate(ledger, country, tenant, None, None, year, now)
        .await
        .map(|record| record.symbol)
        .map_err(|_| CorrectionError::NoRule)
}

/// What a correction reads of a store's configuration: its effective `einvoice` node and its
/// country. Object-safe, so the feed holds it beside the ledger.
pub trait StoreEInvoice: Send + Sync {
    /// The store's effective `einvoice` node (the default where none is published) and the country
    /// its `locale` node names.
    fn read(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> FeedFuture<'_, (PublishedEInvoice, Option<CountryCode>)>;
}

/// [`StoreEInvoice`] over the configuration trees.
#[derive(Debug, Clone)]
pub struct Trees<Cfg>(pub Cfg);

impl<Cfg: ConfigTreeStore + Send + Sync> StoreEInvoice for Trees<Cfg> {
    fn read(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> FeedFuture<'_, (PublishedEInvoice, Option<CountryCode>)> {
        Box::pin(async move {
            let Some(tree) = self
                .0
                .load(tenant, store)
                .await
                .map_err(|e| e.to_string())?
            else {
                return Ok((PublishedEInvoice::default(), None));
            };
            let node = tree
                .record
                .effective()
                .get(PublishedEInvoice::NODE)
                .and_then(|node| serde_json::from_value(node.clone()).ok())
                .unwrap_or_default();
            Ok((
                node,
                crate::http::locale_node::published_country(&tree.record),
            ))
        })
    }
}

/// What the feed corrects a voided bill with: the stores' settings and the symbol ledger, and the
/// queue it records a store's correction requests in (ADR-0250).
#[derive(Clone)]
pub struct Corrector {
    /// The stores' `einvoice` nodes and countries.
    pub settings: Arc<dyn StoreEInvoice>,
    /// The tenants' symbols.
    pub ledger: Arc<dyn SymbolLedger>,
    /// The correction requests waiting for HQ.
    pub requests: Arc<dyn super::requests::CorrectionRequests>,
}

impl core::fmt::Debug for Corrector {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Corrector")
    }
}

/// Corrects the invoice of a bill the store voided at the till (ADR-0245): an
/// `einvoice.correction_kind` correction to zero, recorded as the void event's id so a void read
/// twice is recorded once. `false` when the bill has no invoice or it already stands at zero.
///
/// # Errors
///
/// A store could not be reached; the feed keeps its cursor and reads the void again. A country with
/// no correction symbol rule is logged and skipped, and the invoice stays for HQ to correct.
pub async fn correct_voided_bill(
    corrector: &Corrector,
    submissions: &dyn SubmissionStore,
    tenant: TenantId,
    store: StoreId,
    bill_id: String,
    void_event: Ulid,
    now: Timestamp,
) -> Result<bool, String> {
    let Some(original) = submissions
        .original_for_bill(tenant, store, bill_id)
        .await?
    else {
        return Ok(false);
    };
    let earlier = submissions
        .corrections_of(tenant, original.invoice_number.clone())
        .await?;
    let (node, country) = corrector.settings.read(tenant, store).await?;
    let Some(correction) =
        void_correction(node.correction_kind(), standing_total(&original, &earlier))
    else {
        return Ok(false);
    };
    let countries = crate::countries::registry();
    let country = country.and_then(|code| countries.get(code));
    match record_correction(
        submissions,
        corrector.ledger.as_ref(),
        country,
        &original,
        correction,
        void_event,
        now,
    )
    .await
    {
        Ok(_) => Ok(true),
        Err(CorrectionError::Unavailable(what)) => Err(format!("{what} unreachable")),
        Err(error) => {
            tracing::error!(?error, store = %store, "a voided bill's invoice could not be corrected; HQ corrects it in the console");
            Ok(false)
        }
    }
}

/// `YYYY-MM-DD` of `ms` in UTC.
#[must_use]
pub fn utc_date(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests;
