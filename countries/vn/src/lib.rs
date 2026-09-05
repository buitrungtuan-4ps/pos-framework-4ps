// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Vietnam: what its law and its coinage fix.
//!
//! Written against [ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md), which is the
//! claim this file is evidence for — a country is **supplied**, and everything below is a constant.
//! `README.md` beside this file is the boundary between what belongs here and what belongs in the
//! configuration tree; [ADR-0027](../../../docs/adr/0027-country-modules.md) is the record.
//!
//! # What is not here
//!
//! A provider. Vietnam's e-invoicing is a licensed provider's API, and this pack ships the offline
//! allocator every country shares ([`pos_country::offline`]) rather than pretending to a connection
//! it does not have. That allocator satisfies the whole `Fiscalization` contract, so wiring a
//! provider replaces the submission and keeps everything else — including the offline path, which is
//! the one that matters when the line drops.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

use pos_country::CountryModule;
use pos_country::offline::OfflineFiscalization;
use pos_country::tax_class;
use pos_proto::SalesChannel;
use pos_proto::locale::{CountryCode, LocalePack, NumberFormat, TaxRate, TaxRateTable};
use pos_proto::money::CurrencyCode;
use pos_proto::text::TranslationKey;

/// The standing VAT rate. `docs/pos-spec.md` §5.
///
/// A 2-point relief has been in force for eligible goods and services since Resolution 43/2022 and
/// has been extended more than once, so a great many Vietnamese restaurants charge **8 %** today.
/// This pack publishes the statutory 10 % and leaves the relief to the store's `tax` node, because a
/// dated, repeatedly-extended concession is precisely what configuration is for: a store that has to
/// wait for a release to follow a decree is a store the framework has failed.
const VAT_STANDARD: TaxRate = TaxRate::from_percent(10);

/// The smallest note anybody carries, in đồng. Coins have not circulated in practice since 2011.
const CASH_ROUNDING: i64 = 1_000;

/// The notes a guest hands over, ascending, in đồng.
const NOTES: [i64; 6] = [10_000, 20_000, 50_000, 100_000, 200_000, 500_000];

/// Vietnam.
#[derive(Debug, Clone, Copy, Default)]
pub struct Vietnam;

impl CountryModule for Vietnam {
    fn country_code(&self) -> CountryCode {
        CountryCode::VN
    }

    fn locale_pack(&self) -> LocalePack {
        LocalePack {
            country_code: CountryCode::VN,
            currency_code: CurrencyCode::VND,
            tax_rate_table: rate_table(),
            // 1.234.567,89 — the group separator is the full stop and the decimal is the comma,
            // which is the opposite of the default and the reason this field exists.
            number_format: NumberFormat {
                decimal_separator: ',',
                group_separator: '.',
                digits_per_group: 3,
            },
            default_language: TranslationKey::new("vi"),
            // A default, not a determination. Vietnam's PDPD (Decree 13/2023) puts the duty on the
            // operator as data controller (`docs/pos-spec.md` §11); this is a starting value somebody
            // must confirm against their own lawful basis and retention policy.
            default_retention_days: 365,
            // Vietnamese menus quote **before** VAT and service charge — the familiar `++`. This is
            // the posture ADR-0104 calls exclusive, and it is the one the framework has always
            // computed, so a Vietnamese bill is unchanged byte for byte by any of this.
            prices_include_tax: false,
            cash_rounding_increment: Some(CASH_ROUNDING),
            cash_denominations: NOTES.to_vec(),
        }
    }

    fn display_name(&self) -> &'static str {
        "Vietnam"
    }

    fn is_valid_tax_code(&self, tax_code: &str) -> bool {
        is_valid_mst(tax_code)
    }
}

/// The default table: one rate, every channel, and alcohol stated rather than assumed.
///
/// Every channel is listed even though they share a rate, because
/// [`TaxRateTable::rate_for`](pos_proto::locale::TaxRateTable::rate_for) answers `None` for a missing
/// row and a caller treating `None` as zero would sell untaxed. The redundancy is the point.
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
            .with(tax_class::food(), channel, VAT_STANDARD)
            // Alcohol carries the same VAT. Special consumption tax (thuế tiêu thụ đặc biệt) is
            // levied on the producer or importer and is already inside the price a restaurant pays,
            // so it is not a line on a guest's bill and does not belong in this table.
            .with(tax_class::alcohol(), channel, VAT_STANDARD)
            .with(tax_class::exempt(), channel, TaxRate::ZERO);
    }
    table
}

/// Whether `tax_code` is a well-formed **mã số thuế**: ten digits, optionally `-` and three more.
///
/// Format only, never registration — checking that a code exists is a call to the tax authority and
/// belongs behind `Fiscalization`. Keeping them apart is what lets a cashier take a corporate
/// customer's tax code with the line down.
///
/// The three-digit suffix is a branch (đơn vị trực thuộc), so `0101243150-001` is the first branch of
/// `0101243150` and both are valid.
fn is_valid_mst(tax_code: &str) -> bool {
    let (head, branch) = match tax_code.split_once('-') {
        Some((head, branch)) => (head, Some(branch)),
        None => (tax_code, None),
    };
    let digits = |text: &str, count: usize| {
        text.len() == count && text.bytes().all(|byte| byte.is_ascii_digit())
    };
    digits(head, 10) && branch.is_none_or(|branch| digits(branch, 3))
}

/// `Fiscalization` for Vietnam, over a locally allocated range.
///
/// Vietnam's e-invoice number is a form-and-serial pair issued with the provider's registration
/// followed by an eight-digit sequence — `1C26TAA/00000001`. The serial belongs to the provider; the
/// sequence belongs to the store. Until a provider is configured this writes the same **shape** from
/// the range's own series, so a receipt rendered in a pilot carries a Vietnamese-looking number and
/// nothing downstream has to change when a real serial arrives.
#[must_use]
pub fn fiscalization() -> OfflineFiscalization {
    OfflineFiscalization::new("vn", |series, index| format!("1C26T{series:02}/{index:08}"))
}

#[cfg(test)]
mod tests {
    use super::{NOTES, VAT_STANDARD, Vietnam, fiscalization, is_valid_mst};
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
    fn the_pack_names_its_own_country_and_prices_in_dong() {
        let pack = Vietnam.locale_pack();
        assert_eq!(pack.country_code, CountryCode::VN);
        assert_eq!(pack.currency_code, CurrencyCode::VND);
        assert_eq!(Vietnam.country_code(), CountryCode::VN);
    }

    #[test]
    fn every_class_has_a_rate_on_every_channel() {
        // The trap: `rate_for` answers None for a missing row, and a caller treating None as zero
        // sells untaxed. A row missing here would be found by an audit rather than by a test.
        let pack = Vietnam.locale_pack();
        for class in [tax_class::food(), tax_class::alcohol(), tax_class::exempt()] {
            for channel in CHANNELS {
                assert!(
                    pack.rate_for(class, channel).is_some(),
                    "{class} on {channel:?} has no rate"
                );
            }
        }
    }

    #[test]
    fn vietnam_charges_one_rate_whatever_the_channel() {
        // Unlike Japan. Asserted rather than assumed, because a copied table is how a channel-varying
        // rate gets into a country that does not have one.
        let pack = Vietnam.locale_pack();
        for channel in CHANNELS {
            assert_eq!(
                pack.rate_for(tax_class::food(), channel),
                Some(VAT_STANDARD)
            );
            assert_eq!(
                pack.rate_for(tax_class::alcohol(), channel),
                Some(VAT_STANDARD),
                "special consumption tax is the producer's, not a line on a guest's bill"
            );
        }
    }

    #[test]
    fn prices_are_quoted_before_tax_and_cash_rounds_to_the_thousand() {
        let pack = Vietnam.locale_pack();
        assert!(!pack.prices_include_tax, "Vietnamese menus quote ++");
        assert_eq!(pack.cash_rounding_increment, Some(1_000));
        assert_eq!(pack.cash_denominations, NOTES.to_vec());
        assert!(
            pack.cash_denominations
                .windows(2)
                .all(|pair| matches!(pair, [smaller, larger] if smaller < larger)),
            "the till lays its keys out in the order this list is in"
        );
    }

    #[test]
    fn numbers_are_written_the_vietnamese_way_round() {
        let pack = Vietnam.locale_pack();
        assert_eq!(pack.number_format.decimal_separator, ',');
        assert_eq!(pack.number_format.group_separator, '.');
    }

    #[test]
    fn a_tax_code_is_ten_digits_or_ten_and_a_branch() {
        assert!(is_valid_mst("0101243150"));
        assert!(is_valid_mst("0101243150-001"));
        assert!(Vietnam.is_valid_tax_code("0101243150"));

        assert!(!is_valid_mst(""));
        assert!(!is_valid_mst("010124315"), "nine digits");
        assert!(!is_valid_mst("01012431501"), "eleven digits");
        assert!(!is_valid_mst("0101243150-01"), "a branch is three digits");
        assert!(!is_valid_mst("0101243150 001"), "the separator is a hyphen");
        assert!(!is_valid_mst("A101243150"));

        // And it says nothing about registration: a well-formed code no authority has heard of
        // passes here on purpose, so a cashier can take one with the line down.
        assert!(is_valid_mst("0000000000"));
    }

    #[test]
    fn two_ranges_never_share_a_number() {
        // The obligation this pack's *format* carries, as opposed to the ones `pos_country::offline`
        // carries for it: a format that drops the series hands the same number out twice after a
        // second allocation, and the port's never-reuse guard would then be all that stood between a
        // store and a duplicated invoice.
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
