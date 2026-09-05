// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! India: what its law and its coinage fix.
//!
//! Written against [ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md). India is the
//! country [ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md) exists for: a tax
//! invoice for an intra-state sale must print **CGST and SGST on separate lines**, because the two
//! halves go to different governments, and printing their sum is not a terser rendering of the same
//! fact — it is not a valid invoice.
//!
//! # The three things that make this pack different from the others
//!
//! - **The rate is composed.** 5 % restaurant GST is published as CGST 2.5 % + SGST 2.5 %, and the
//!   framework allocates the tax across those parts so they sum to it exactly.
//! - **Prices are inclusive.** MRP is inclusive by definition, so the tax is extracted from the
//!   quoted price rather than added to it, and the guest pays what the label says.
//! - **Alcohol has no row, deliberately.** See [`rate_table`].
//!
//! # What is not here
//!
//! A GSTIN, and the IRP. The GSTIN is issued to a **business** in a **state** and is a per-store
//! value in the configuration tree; [`India::is_valid_tax_code`] checks its shape and nothing else.
//! The Invoice Registration Portal — which returns an IRN and a signed QR for businesses above the
//! e-invoicing threshold — is a provider behind the `Fiscalization` port, and this pack ships the
//! offline allocator every country shares ([`pos_country::offline`]) rather than pretending to a
//! connection it does not have.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

use pos_country::CountryModule;
use pos_country::offline::OfflineFiscalization;
use pos_country::tax_class;
use pos_proto::SalesChannel;
use pos_proto::locale::{
    CountryCode, LocalePack, NumberFormat, TaxComponent, TaxRate, TaxRateTable,
};
use pos_proto::money::CurrencyCode;
use pos_proto::text::TranslationKey;

/// Restaurant service GST: 5 %, without input tax credit, under Notification 11/2017 as amended.
///
/// A restaurant inside a hotel whose declared room tariff exceeds ₹7,500 charges 18 % instead. That
/// is a fact about the *premises*, not about the country, so it is a store's `tax` node rather than a
/// second table here.
const GST_RESTAURANT: TaxRate = TaxRate::from_basis_points(500);

/// Each half of it. The parts must sum to the whole, and
/// [`TaxRateTable::unbalanced_rows`](pos_proto::locale::TaxRateTable::unbalanced_rows) is what
/// checks that they do.
const GST_HALF: TaxRate = TaxRate::from_basis_points(250);

/// Section 170 of the CGST Act rounds an invoice to the nearest rupee, and no smaller coin
/// circulates to settle the difference. ₹1 is 100 paise.
const CASH_ROUNDING: i64 = 100;

/// The notes a guest hands over, ascending, in paise.
const NOTES: [i64; 6] = [1_000, 2_000, 5_000, 10_000, 20_000, 50_000];

/// India.
#[derive(Debug, Clone, Copy, Default)]
pub struct India;

impl CountryModule for India {
    fn country_code(&self) -> CountryCode {
        CountryCode::IN
    }

    fn locale_pack(&self) -> LocalePack {
        LocalePack {
            country_code: CountryCode::IN,
            currency_code: CurrencyCode::INR,
            tax_rate_table: rate_table(),
            // 1,234,567 — and India actually writes 12,34,567, grouping by two above the first three
            // (the lakh–crore system). `digits_per_group` is a single number and cannot say that, so
            // this renders the Western way: wrong-looking rather than wrong. ADR-0105 records why the
            // fix is a group *pattern* and a separate change rather than a constant here.
            number_format: NumberFormat::default(),
            // English, which is what an Indian tax invoice is written in and what the console and the
            // receipt default to. A store serving guests in Hindi, Tamil or Kannada publishes that as
            // its display language; `en` is the fallback the catalogue always carries.
            default_language: TranslationKey::new("en"),
            // A default, not a determination. India's DPDP Act 2023 puts the duty on the operator as
            // data fiduciary (`docs/pos-spec.md` §11); somebody must confirm this against their own
            // notice and consent record before a deployment relies on it.
            default_retention_days: 365,
            // MRP is inclusive by definition, and a printed price that the guest is then charged more
            // than is an offence under the Legal Metrology (Packaged Commodities) Rules. So the tax
            // is extracted from the quoted price rather than added to it (ADR-0104).
            prices_include_tax: true,
            cash_rounding_increment: Some(CASH_ROUNDING),
            cash_denominations: NOTES.to_vec(),
        }
    }

    fn display_name(&self) -> &'static str {
        "India"
    }

    fn is_valid_tax_code(&self, tax_code: &str) -> bool {
        is_valid_gstin(tax_code)
    }
}

/// The default table: food composed into CGST and SGST, and **no row for alcohol**.
///
/// # Why alcohol is missing
///
/// Alcoholic liquor for human consumption is outside GST altogether. It attracts **state excise and
/// state VAT**, at rates each state sets for itself — so there is no national number this pack could
/// publish, and a plausible-looking one would be wrong in every state but the one it was copied from.
///
/// The absence is load-bearing rather than an omission.
/// [`TaxRateTable::rate_for`](pos_proto::locale::TaxRateTable::rate_for) answers `None` for a missing
/// row and `pos_core::billing` turns that into `TaxRateNotConfigured`, refusing the sale. So an
/// Indian store that sells alcohol **must** publish its state's rate before it can ring one up, and
/// the failure is a refusal at the till on the first attempt rather than a year of untaxed liquor
/// discovered by an assessment. That is the difference between `None` and a zero, and it is the whole
/// reason the table answers `None`.
///
/// # Inter-state sales
///
/// A sale to a buyer in another state is **IGST at the full 5 %**, one component rather than two. That
/// depends on the buyer's state relative to the seller's, which is a fact about *this bill* and not
/// about the store's rate table (ADR-0104), so it is not a row here. The shape carries it: one
/// component named `IGST` at the full rate.
fn rate_table() -> TaxRateTable {
    let mut table = TaxRateTable::new();
    for channel in [
        SalesChannel::DineIn,
        SalesChannel::Takeaway,
        SalesChannel::Delivery,
        SalesChannel::Qr,
        SalesChannel::Api,
    ] {
        table = table
            .with_components(
                tax_class::food(),
                channel,
                GST_RESTAURANT,
                vec![
                    TaxComponent::new("CGST", GST_HALF),
                    TaxComponent::new("SGST", GST_HALF),
                ],
            )
            .with(tax_class::exempt(), channel, TaxRate::ZERO);
    }
    table
}

/// Whether `tax_code` is a well-formed GSTIN.
///
/// Fifteen characters: two digits of state code, a ten-character PAN, one entity digit, the literal
/// `Z`, and one alphanumeric checksum. The PAN itself is five letters, four digits and a letter.
///
/// Format only, never registration — the GST portal's lookup needs a network, and a cashier must be
/// able to take a corporate customer's GSTIN with the line down. The checksum digit is **not**
/// verified here for the same reason the format is checked at all: this is a typo guard, and a
/// wrongly-rejected valid number would be worse than a wrongly-accepted invalid one, which the portal
/// catches at filing.
fn is_valid_gstin(tax_code: &str) -> bool {
    let bytes = tax_code.as_bytes();
    let Ok(code) = <&[u8; 15]>::try_from(bytes) else {
        return false;
    };
    let digits = |range: core::ops::Range<usize>| {
        code.get(range)
            .is_some_and(|slice| !slice.is_empty() && slice.iter().all(u8::is_ascii_digit))
    };
    let letters = |range: core::ops::Range<usize>| {
        code.get(range)
            .is_some_and(|slice| !slice.is_empty() && slice.iter().all(u8::is_ascii_uppercase))
    };

    digits(0..2)                       // state code
        && letters(2..7)               // PAN: five letters
        && digits(7..11)               // PAN: four digits
        && letters(11..12)             // PAN: the check letter
        && code[12].is_ascii_alphanumeric() // entity number for this PAN in this state
        && code[13] == b'Z'            // fixed
        && code[14].is_ascii_alphanumeric() // checksum
}

/// `Fiscalization` for India, over a locally allocated range.
///
/// Rule 46(b) lets the **seller** choose the invoice number: up to sixteen characters, alphanumeric
/// with `/` and `-`, unique within a financial year. So a locally allocated range is the correct
/// answer below the e-invoicing threshold, and the offline half of the answer above it — a business
/// over the threshold registers the invoice with the IRP and prints the IRN and signed QR it returns,
/// which is a provider wrapping this rather than replacing it.
#[must_use]
pub fn fiscalization() -> OfflineFiscalization {
    OfflineFiscalization::new("in", |series, index| format!("INV/{series:02}/{index:06}"))
}

#[cfg(test)]
mod tests {
    use super::{
        GST_HALF, GST_RESTAURANT, India, NOTES, fiscalization, is_valid_gstin, rate_table,
    };
    use core::num::NonZeroU32;

    use pos_country::{CountryModule, tax_class};
    use pos_fakes::executor::run_ready;
    use pos_ports::fiscalization::Fiscalization;
    use pos_proto::locale::CountryCode;
    use pos_proto::money::CurrencyCode;
    use pos_proto::{SalesChannel, StoreId, Ulid};

    const CHANNELS: [SalesChannel; 5] = [
        SalesChannel::DineIn,
        SalesChannel::Takeaway,
        SalesChannel::Delivery,
        SalesChannel::Qr,
        SalesChannel::Api,
    ];

    #[test]
    fn the_pack_names_its_own_country_and_prices_in_rupees() {
        let pack = India.locale_pack();
        assert_eq!(pack.country_code, CountryCode::IN);
        assert_eq!(pack.currency_code, CurrencyCode::INR);
        assert_eq!(India.country_code(), CountryCode::IN);
    }

    #[test]
    fn food_is_five_percent_split_into_two_named_halves() {
        // The invoice must print CGST and SGST separately; the halves go to different governments.
        let pack = India.locale_pack();
        for channel in CHANNELS {
            assert_eq!(
                pack.rate_for(tax_class::food(), channel),
                Some(GST_RESTAURANT)
            );
            let components = pack
                .tax_rate_table
                .components_for(tax_class::food(), channel);
            let names: Vec<&str> = components
                .iter()
                .map(|component| component.name.as_str())
                .collect();
            assert_eq!(names, ["CGST", "SGST"]);
            assert!(
                components
                    .iter()
                    .all(|component| component.rate == GST_HALF)
            );
        }
    }

    #[test]
    fn the_components_sum_to_the_rate_they_belong_to() {
        // ADR-0104's invariant, on the one pack that uses it. A table that fails this prints an
        // invoice whose parts do not add up to the tax charged.
        let table = rate_table();
        let unbalanced = table.unbalanced_rows();
        assert!(
            unbalanced.is_empty(),
            "{} row(s) publish components that miss their own rate",
            unbalanced.len()
        );
    }

    #[test]
    fn alcohol_has_no_rate_so_a_store_must_publish_its_states() {
        // Not an omission. Liquor is outside GST — state excise and state VAT, at rates each state
        // sets — so there is no national number to publish. `rate_for` answering None makes
        // `pos_core::billing` refuse the sale, which turns "nobody configured the liquor rate" into a
        // refusal at the till on the first attempt rather than a year of untaxed sales found by an
        // assessment.
        let pack = India.locale_pack();
        for channel in CHANNELS {
            assert!(
                pack.rate_for(tax_class::alcohol(), channel).is_none(),
                "a national alcohol rate would be wrong in every state but one"
            );
        }
    }

    #[test]
    fn food_and_exempt_are_priced_on_every_channel() {
        let pack = India.locale_pack();
        for class in [tax_class::food(), tax_class::exempt()] {
            for channel in CHANNELS {
                assert!(pack.rate_for(class, channel).is_some());
            }
        }
    }

    #[test]
    fn prices_already_contain_their_tax_and_cash_rounds_to_the_rupee() {
        let pack = India.locale_pack();
        assert!(pack.prices_include_tax, "MRP is inclusive by definition");
        assert_eq!(pack.cash_rounding_increment, Some(100), "₹1 in paise");
        assert_eq!(pack.cash_denominations, NOTES.to_vec());
    }

    #[test]
    fn a_gstin_is_fifteen_characters_in_the_shape_the_portal_issues() {
        assert!(is_valid_gstin("29ABCDE1234F1Z5"));
        assert!(India.is_valid_tax_code("29ABCDE1234F1Z5"));

        assert!(!is_valid_gstin(""));
        assert!(!is_valid_gstin("29ABCDE1234F1Z"), "fourteen");
        assert!(!is_valid_gstin("29ABCDE1234F1Z55"), "sixteen");
        assert!(!is_valid_gstin("2AABCDE1234F1Z5"), "state code is digits");
        assert!(
            !is_valid_gstin("29ABCD11234F1Z5"),
            "PAN starts with letters"
        );
        assert!(!is_valid_gstin("29ABCDE123AF1Z5"), "PAN's middle is digits");
        assert!(!is_valid_gstin("29ABCDE1234F1Y5"), "the fourteenth is Z");
        assert!(!is_valid_gstin("29abcde1234f1z5"), "upper case");

        // And it says nothing about registration: the portal's lookup needs a network, and a till
        // must not need one to take a corporate customer's number.
        assert!(is_valid_gstin("00AAAAA0000A0Z0"));
    }

    #[test]
    fn two_ranges_never_share_a_number() {
        let fiscal = fiscalization();
        let store = StoreId::new(Ulid::from_u128(1));
        let count = NonZeroU32::new(4).expect("4 is not zero");
        let first = run_ready(fiscal.allocate_range(store, count)).expect("a range");
        let second = run_ready(fiscal.allocate_range(store, count)).expect("a second range");
        for number in &first.numbers {
            assert!(
                !second.numbers.contains(number),
                "{number:?} was handed out by both ranges"
            );
        }
    }
}
