// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Japan: what its law and its coinage fix.
//!
//! Written against [ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md). Japan is the
//! reason two of the framework's tax dimensions exist at all:
//!
//! - **The channel dimension** ([ADR-0074](../../../docs/adr/0074-localization-and-tax.md), and
//!   `docs/pos-spec.md` §5's worked example): the same onigiri is 8 % carried out and 10 % eaten in,
//!   because the reduced rate covers food *for takeaway* and a dine-in sale is a service.
//! - **The inclusive posture** ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)):
//!   総額表示 has been compulsory since April 2021, so a Japanese menu price is 税込 — it already
//!   contains its tax, and adding 10 % on top would overcharge every guest.
//!
//! Alcohol is the case that makes the class dimension load-bearing too: it is excluded from the
//! reduced rate, so takeaway beer is 10 % while the takeaway food beside it is 8 %.
//!
//! # What is not here
//!
//! A registration number. A qualified invoice (適格請求書) must carry the seller's 登録番号 —
//! `T` plus thirteen digits — and that number is issued to a **business**, by application, after
//! this pack has done everything it can. It is a per-store value in the configuration tree, next to
//! the store's name and address, and [`Japan::is_valid_tax_code`] checks its shape and nothing else.
//!
//! Japan's authority allocates no invoice numbers, so the offline allocator every pack shares
//! ([`pos_country::offline`]) is not a stand-in here — a seller-chosen serial *is* the correct
//! answer.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

use pos_country::CountryModule;
use pos_country::offline::OfflineFiscalization;
use pos_country::tax_class;
use pos_proto::locale::{CountryCode, LocalePack, NumberFormat, TaxRate, TaxRateTable};
use pos_proto::money::CurrencyCode;
use pos_proto::text::TranslationKey;
use pos_proto::{SalesChannel, TaxClassId};

/// 消費税, the standard rate: dine-in food, all alcohol, everything not eligible for the relief.
const CONSUMPTION_STANDARD: TaxRate = TaxRate::from_percent(10);

/// 軽減税率, the reduced rate: food and non-alcoholic drink taken away.
const CONSUMPTION_REDUCED: TaxRate = TaxRate::from_percent(8);

/// The notes a guest hands over, ascending, in yen.
///
/// Coins run to ¥500 and are not keys a cashier presses; the note series begins at ¥1,000.
const NOTES: [i64; 3] = [1_000, 5_000, 10_000];

/// Japan.
#[derive(Debug, Clone, Copy, Default)]
pub struct Japan;

impl CountryModule for Japan {
    fn country_code(&self) -> CountryCode {
        CountryCode::JP
    }

    fn locale_pack(&self) -> LocalePack {
        LocalePack {
            country_code: CountryCode::JP,
            currency_code: CurrencyCode::JPY,
            tax_rate_table: rate_table(),
            // 1,234,567 — the ordinary Western arrangement. Japanese prose also groups by 万 (ten
            // thousand), but a price on a receipt is written this way.
            number_format: NumberFormat::default(),
            default_language: TranslationKey::new("ja"),
            // A default, not a determination. Japan's APPI puts the duty on the operator as the
            // business handling personal information (`docs/pos-spec.md` §11); somebody must confirm
            // this against their own purpose and retention policy.
            default_retention_days: 365,
            // 総額表示: the price on the menu is what the guest pays. Compulsory since April 2021.
            // Under this posture the tax is *extracted* from the quoted price rather than added, so a
            // ¥1,100 dish stays ¥1,100 and reports ¥100 of tax (ADR-0104).
            prices_include_tax: true,
            // The 1-yen coin circulates, so there is nothing to round to. `None` rather than `Some(1)`
            // because they are not the same statement: one says "no rounding", the other says
            // "rounding, to a unit that changes nothing", and only the first survives a currency
            // change.
            cash_rounding_increment: None,
            cash_denominations: NOTES.to_vec(),
        }
    }

    fn display_name(&self) -> &'static str {
        "Japan"
    }

    fn is_valid_tax_code(&self, tax_code: &str) -> bool {
        is_valid_registration_number(tax_code)
    }
}

/// The default table: the reduced rate where the law puts it, and nowhere else.
///
/// The rule is *what is being sold and how it leaves*, which is exactly the pair this table is keyed
/// on:
///
/// | | dine-in · QR | takeaway · delivery · API |
/// |---|---|---|
/// | food, soft drink | 10 % | **8 %** |
/// | alcohol | 10 % | 10 % |
/// | exempt | 0 % | 0 % |
///
/// QR ordering is a guest ordering at a table, so it is dine-in. The API channel is the marketplace
/// intake — Uber Eats, 出前館 — which is delivery, so it takes the reduced rate. Both are defaults a
/// store overrides on its `tax` node if it uses the channel for something else.
fn rate_table() -> TaxRateTable {
    let eat_in = [SalesChannel::DineIn, SalesChannel::Qr];
    let carried_out = [
        SalesChannel::Takeaway,
        SalesChannel::Delivery,
        SalesChannel::Api,
    ];
    let mut table = TaxRateTable::new();
    for channel in eat_in {
        table = table.with(tax_class::food(), channel, CONSUMPTION_STANDARD);
    }
    for channel in carried_out {
        table = table.with(tax_class::food(), channel, CONSUMPTION_REDUCED);
    }
    for channel in eat_in.into_iter().chain(carried_out) {
        table = table
            // Alcohol is excluded from the relief however it leaves the shop, which is why one rate
            // per channel could not express Japan and why the class dimension is load-bearing here.
            .with(tax_class::alcohol(), channel, CONSUMPTION_STANDARD)
            .with(tax_class::exempt(), channel, TaxRate::ZERO);
    }
    table
}

/// Whether `tax_code` is a well-formed 登録番号: `T` followed by thirteen digits.
///
/// The thirteen digits are the holder's corporate number (法人番号) for a company, or a number
/// issued to the individual for a sole trader. Format only, never registration: the National Tax
/// Agency publishes a lookup and calling it needs a network, which is the thing a till must not
/// require to take a corporate customer's number.
fn is_valid_registration_number(tax_code: &str) -> bool {
    let Some(digits) = tax_code.strip_prefix('T') else {
        return false;
    };
    digits.len() == 13 && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// The tax class an item takes when nothing else classifies it.
///
/// Exported because "which class is this" is the question a Japanese menu import has to answer for
/// every row, and answering it with a hard-coded `Ulid::from_u128(1)` in an importer is how the
/// convention in [`pos_country::tax_class`] quietly acquires a second definition.
#[must_use]
pub fn default_tax_class() -> TaxClassId {
    tax_class::food()
}

/// `Fiscalization` for Japan, over a locally allocated range.
///
/// Japan's authority allocates nothing: a qualified invoice needs the seller's registration number
/// and a serial the **seller** chooses, so a locally allocated range is not a stand-in for a
/// provider — it is the whole answer. What a Japanese deployment still owes is printing the
/// registration number and the per-rate breakdown on the document, which is the receipt template's
/// work rather than this port's.
#[must_use]
pub fn fiscalization() -> OfflineFiscalization {
    OfflineFiscalization::new("jp", |series, index| format!("{series:04}-{index:06}"))
}

#[cfg(test)]
mod tests {
    use super::{
        CONSUMPTION_REDUCED, CONSUMPTION_STANDARD, Japan, NOTES, default_tax_class, fiscalization,
        is_valid_registration_number,
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
    fn the_pack_names_its_own_country_and_prices_in_yen() {
        let pack = Japan.locale_pack();
        assert_eq!(pack.country_code, CountryCode::JP);
        assert_eq!(pack.currency_code, CurrencyCode::JPY);
        assert_eq!(Japan.country_code(), CountryCode::JP);
    }

    #[test]
    fn every_class_has_a_rate_on_every_channel() {
        let pack = Japan.locale_pack();
        for class in [tax_class::food(), tax_class::alcohol(), tax_class::exempt()] {
            for channel in CHANNELS {
                assert!(
                    pack.rate_for(class, channel).is_some(),
                    "{class} on {channel:?} has no rate, so an item on it would be untaxed"
                );
            }
        }
    }

    #[test]
    fn the_same_food_is_eight_percent_carried_out_and_ten_percent_eaten_in() {
        // `docs/pos-spec.md` §5's worked example, asserted. This is the whole reason the rate table
        // has a channel dimension.
        let pack = Japan.locale_pack();
        let food = tax_class::food();
        assert_eq!(
            pack.rate_for(food, SalesChannel::Takeaway),
            Some(CONSUMPTION_REDUCED)
        );
        assert_eq!(
            pack.rate_for(food, SalesChannel::DineIn),
            Some(CONSUMPTION_STANDARD)
        );
        assert_eq!(
            pack.rate_for(food, SalesChannel::Qr),
            Some(CONSUMPTION_STANDARD),
            "QR ordering is a guest at a table, which is dine-in"
        );
    }

    #[test]
    fn alcohol_never_takes_the_reduced_rate() {
        // The case that makes the *class* dimension load-bearing as well as the channel one: takeaway
        // beer is 10 % while the takeaway food beside it is 8 %.
        let pack = Japan.locale_pack();
        for channel in CHANNELS {
            assert_eq!(
                pack.rate_for(tax_class::alcohol(), channel),
                Some(CONSUMPTION_STANDARD),
                "alcohol is excluded from the relief on {channel:?}"
            );
        }
        assert_ne!(
            pack.rate_for(tax_class::alcohol(), SalesChannel::Takeaway),
            pack.rate_for(tax_class::food(), SalesChannel::Takeaway)
        );
    }

    #[test]
    fn prices_already_contain_their_tax_and_nothing_is_rounded() {
        let pack = Japan.locale_pack();
        assert!(pack.prices_include_tax, "総額表示, compulsory since 2021");
        assert_eq!(
            pack.cash_rounding_increment, None,
            "the 1-yen coin circulates, so there is nothing to round to"
        );
        assert_eq!(pack.cash_denominations, NOTES.to_vec());
    }

    #[test]
    fn a_registration_number_is_t_and_thirteen_digits() {
        assert!(is_valid_registration_number("T1234567890123"));
        assert!(Japan.is_valid_tax_code("T1234567890123"));

        assert!(!is_valid_registration_number(""));
        assert!(!is_valid_registration_number("1234567890123"), "no T");
        assert!(
            !is_valid_registration_number("t1234567890123"),
            "lower case"
        );
        assert!(!is_valid_registration_number("T123456789012"), "twelve");
        assert!(!is_valid_registration_number("T12345678901234"), "fourteen");
        assert!(!is_valid_registration_number("T123456789012A"));

        // And it says nothing about registration: the National Tax Agency's lookup needs a network,
        // and a till must not need one to take a corporate customer's number.
        assert!(is_valid_registration_number("T0000000000000"));
    }

    #[test]
    fn the_default_class_is_the_one_the_reduced_rate_applies_to() {
        assert_eq!(default_tax_class(), tax_class::food());
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
