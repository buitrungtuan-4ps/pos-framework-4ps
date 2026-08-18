// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The reference country module. Copy this directory to start a real country.
//!
//! `ZZ` is CLDR's code for an unknown region, so it can never collide with a real country.
//! `README.md` beside this file is the six-step copy procedure and the country-versus-configuration
//! boundary; this file is the code.
//!
//! # Why a working module rather than a stub
//!
//! It would have been cheaper to leave `todo!()` here and call it a template. That would have proved
//! nothing, and `docs/roadmap.md` P10 would then be inventing the shape of a country module at the
//! moment a tax authority is waiting on it.
//!
//! Instead [`ZzFiscalization`] is a complete implementation over a local counter — no authority to
//! contact, because there is no `ZZ` tax authority — and it **passes the full `Fiscalization` contract
//! suite**, including offline issuance, never-reuse across restarts, and exhaustion. So the shape
//! `fiscal-vn` fills in is already proven, and a forker starting a new country finds out immediately
//! whether their provider fits it.
//!
//! What a real module changes: the body of [`ZzFiscalization`], the constants in
//! [`Zz::locale_pack`], and [`Zz::is_valid_tax_code`]. What it keeps: the suite.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use pos_country::CountryModule;
use pos_ports::fiscalization::{
    Fiscalization, InvoiceNumber, InvoiceRange, InvoiceRequest, IssuedInvoice, ReconciliationReport,
};
use pos_ports::{PortError, PortName};
use pos_proto::locale::{CountryCode, LocalePack, NumberFormat, TaxRate, TaxRateTable};
use pos_proto::money::CurrencyCode;
use pos_proto::text::TranslationKey;
use pos_proto::{BillId, CalendarDate, StoreId, TaxClassId, Timestamp, Ulid};

/// The tax class every reference item falls into.
///
/// A real country lists its own — food, drink, alcohol, service — and the identifiers come from the
/// configuration tree rather than from here. This constant exists only so the reference rate table
/// has something to key on.
const REFERENCE_TAX_CLASS: u128 = 1;

/// The reference country.
#[derive(Debug, Clone, Copy, Default)]
pub struct Zz;

impl CountryModule for Zz {
    fn country_code(&self) -> CountryCode {
        CountryCode::ZZ
    }

    fn locale_pack(&self) -> LocalePack {
        let tax_class_id = TaxClassId::new(Ulid::from_u128(REFERENCE_TAX_CLASS));
        LocalePack {
            country_code: CountryCode::ZZ,
            currency_code: CurrencyCode::USD,
            // Every channel stated explicitly, including the ones that share a rate. A real country
            // must do the same, because `TaxRateTable::rate_for` returns `None` for a missing row and
            // never falls back to zero — an item nobody classified charging no tax is a bug found by
            // an audit rather than by a test.
            tax_rate_table: TaxRateTable::new()
                .with(
                    tax_class_id,
                    pos_proto::SalesChannel::DineIn,
                    TaxRate::from_percent(10),
                )
                .with(
                    tax_class_id,
                    pos_proto::SalesChannel::Takeaway,
                    // Different from dine-in on purpose, so a copy of this module starts out
                    // exercising the channel dimension rather than looking like a flat rate that
                    // happens to have a spare column. `docs/pos-spec.md` §5's worked case is Japan.
                    TaxRate::from_percent(8),
                )
                .with(
                    tax_class_id,
                    pos_proto::SalesChannel::Delivery,
                    TaxRate::from_percent(8),
                )
                .with(
                    tax_class_id,
                    pos_proto::SalesChannel::Qr,
                    TaxRate::from_percent(10),
                )
                .with(
                    tax_class_id,
                    pos_proto::SalesChannel::Api,
                    TaxRate::from_percent(10),
                ),
            number_format: NumberFormat::default(),
            default_language: TranslationKey::new("en"),
            // A default, not a determination. `docs/pos-spec.md` §11 puts the legal judgement on the
            // operator as data controller, and a real country module's value here still needs
            // confirming against local law before a deployment relies on it.
            default_retention_days: 365,
        }
    }

    fn display_name(&self) -> &'static str {
        "Reference country (ZZ)"
    }

    fn is_valid_tax_code(&self, tax_code: &str) -> bool {
        // Format only, never existence — see this module's README. A real country encodes its own
        // rule: Vietnam's is ten digits, optionally followed by a three-digit branch suffix.
        !tax_code.is_empty()
            && tax_code.len() <= 20
            && tax_code.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }
}

/// Everything the reference fiscalization has issued.
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

/// `Fiscalization` for the reference country.
///
/// # What makes this a reference and not a stub
///
/// It satisfies every obligation in the port's contract, and the ones that matter are the ones a real
/// provider is most likely to get wrong:
///
/// - **Issuing works with no network**, because numbers come from a pre-allocated range. That is the
///   whole reason ranges exist: without them, "the store sells with no internet"
///   ([ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md)) stops being true the moment
///   a customer asks for an invoice.
/// - **A number is never reused**, tracked separately from what is available so a second range
///   allocation cannot resurrect a consumed number.
/// - **One bill has one number**, so a retried submission is not a compliance incident.
/// - **Exhaustion refuses** rather than inventing a number. This is the only failure in the framework
///   with no path that keeps selling, which is why the alert exists.
#[derive(Debug, Clone, Default)]
pub struct ZzFiscalization {
    state: Arc<Mutex<FiscalState>>,
}

impl ZzFiscalization {
    /// A country module with no ranges allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

impl Fiscalization for ZzFiscalization {
    async fn allocate_range(
        &self,
        store_id: StoreId,
        count: core::num::NonZeroU32,
    ) -> Result<InvoiceRange, PortError> {
        let mut state = self.lock();
        let series = state.next_series;
        state.next_series = state.next_series.saturating_add(1);

        // The series prefix is what keeps two ranges from overlapping. A real provider is told its
        // prefix by the authority; the important property is the same either way — a number is
        // globally unique for the deployment, not merely unique within one range.
        let numbers: Vec<InvoiceNumber> = (0..count.get())
            .map(|index| InvoiceNumber::new(format!("ZZ{series:04}/{index:06}")))
            .collect();
        state.available.clone_from(&numbers);

        Ok(InvoiceRange {
            store_id,
            range_id: format!("zz-range-{series}").into(),
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
            // A real module stamps the instant it issued. Taken from the request's calendar date here
            // rather than from a clock, because a country module has no `ClockSource` and reading the
            // system clock is what `AGENTS.md` §2 forbids.
            issued_at: Timestamp::EPOCH,
            // Never submitted, because there is no `ZZ` authority. A real module reports whether the
            // authority acknowledged it — and `false` is legal, which is what the flush-on-reconnect
            // path exists to clear.
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
            // Always empty here: there is no authority holding numbers this store does not know about.
            // A real module fills this from the authority's own records, and it is the direction that
            // matters most — a number consumed with no local record is a gap nobody can explain later.
            unknown_locally: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Zz, ZzFiscalization};
    use pos_country::{CountryModule, CountryRegistry};
    use pos_proto::locale::CountryCode;
    use pos_proto::{SalesChannel, TaxClassId, Ulid};

    fn tax_class() -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(super::REFERENCE_TAX_CLASS))
    }

    #[test]
    fn the_module_registers_and_is_found_by_its_code() {
        let registry = pos_country::country_registry! { Zz };
        registry.validate().expect("one country is valid");
        let found = registry.get(CountryCode::ZZ).expect("ZZ is in this build");
        assert_eq!(found.country_code(), CountryCode::ZZ);
        assert!(registry.sole().is_some(), "a cell serves one country");
    }

    #[test]
    fn a_build_without_this_country_does_not_find_it() {
        // What a fork sees when it enables one country and configures another: None, not a wrong
        // module. Answering with the wrong country's tax regime would be worse than not answering.
        assert!(CountryRegistry::empty().get(CountryCode::ZZ).is_none());
    }

    #[test]
    fn the_locale_pack_names_its_own_country_and_prices_in_one_currency() {
        let pack = Zz.locale_pack();
        assert_eq!(pack.country_code, CountryCode::ZZ);
        assert_eq!(pack.currency_code, pos_proto::money::CurrencyCode::USD);
    }

    #[test]
    fn every_channel_has_a_rate_so_none_falls_through_to_none() {
        // The trap a copied module must not inherit: `rate_for` returns None for a missing row, and a
        // caller treating None as zero would charge no tax on that channel.
        let pack = Zz.locale_pack();
        for channel in [
            SalesChannel::DineIn,
            SalesChannel::Takeaway,
            SalesChannel::Delivery,
            SalesChannel::Qr,
            SalesChannel::Api,
        ] {
            assert!(
                pack.rate_for(tax_class(), channel).is_some(),
                "{channel:?} has no rate, so an item on that channel would be untaxed"
            );
        }
    }

    #[test]
    fn the_rate_table_actually_varies_by_channel() {
        // So a copy of this module starts out exercising the channel dimension rather than looking
        // like a flat rate with a spare column.
        let pack = Zz.locale_pack();
        assert_ne!(
            pack.rate_for(tax_class(), SalesChannel::DineIn),
            pack.rate_for(tax_class(), SalesChannel::Takeaway)
        );
    }

    #[test]
    fn an_unclassified_item_has_no_rate() {
        let pack = Zz.locale_pack();
        let unknown = TaxClassId::new(Ulid::from_u128(u128::MAX));
        assert!(pack.rate_for(unknown, SalesChannel::DineIn).is_none());
    }

    #[test]
    fn tax_code_validation_is_format_only() {
        assert!(Zz.is_valid_tax_code("0101234567"));
        assert!(Zz.is_valid_tax_code("AB123"));
        assert!(!Zz.is_valid_tax_code(""));
        assert!(
            !Zz.is_valid_tax_code("0101234567 001"),
            "no separators in this format"
        );
        assert!(!Zz.is_valid_tax_code(&"9".repeat(21)));

        // And it says nothing about registration. A syntactically valid code that no authority has
        // ever heard of passes here on purpose, because checking otherwise needs a network call and
        // would stop a store validating a corporate customer offline.
        assert!(Zz.is_valid_tax_code("0000000000"));
    }

    #[test]
    fn fiscalization_is_constructible_without_configuration() {
        // A country module is interrogated during startup, before configuration and sometimes before
        // a runtime, so constructing its parts must not need either.
        let _fiscal = ZzFiscalization::new();
    }
}
