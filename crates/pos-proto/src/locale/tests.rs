// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The locale module's tests, kept beside it so the module stays within its size budget.

use super::{
    CountryCode, CountryCodeError, LocalePack, LocaleSettings, NumberFormat, PublishedLocale,
    PublishedNumberFormat, TaxComponent, TaxComponentName, TaxComponentNameError, TaxRate,
    TaxRateTable, TaxRounding,
};
use crate::enums::SalesChannel;
use crate::ids::TaxClassId;
use crate::money::{CurrencyCode, Money, Rounding};
use crate::text::TranslationKey;
use crate::ulid::Ulid;
use crate::wire_enum::WireEnum;

fn food() -> TaxClassId {
    TaxClassId::new(Ulid::from_u128(1))
}

fn alcohol() -> TaxClassId {
    TaxClassId::new(Ulid::from_u128(2))
}

#[test]
fn a_country_code_normalises_case_and_refuses_anything_else() {
    assert_eq!(CountryCode::parse("vn"), Ok(CountryCode::VN));
    assert_eq!(CountryCode::parse("VN"), Ok(CountryCode::VN));
    assert_eq!(CountryCode::VN.as_str(), "VN");
    assert_eq!(
        CountryCode::VN.as_directory(),
        "vn",
        "lower-case on disk and in a hostname"
    );

    assert_eq!(CountryCode::parse("VNM"), Err(CountryCodeError::Length));
    assert_eq!(CountryCode::parse("V"), Err(CountryCodeError::Length));
    assert_eq!(CountryCode::parse(""), Err(CountryCodeError::Length));
    assert_eq!(
        CountryCode::parse("V1"),
        Err(CountryCodeError::NotAlphabetic)
    );
    assert_eq!(
        CountryCode::parse("Vietnam"),
        Err(CountryCodeError::Length),
        "a country name is not a country code, and accepting one would let it reach a hostname"
    );
}

#[test]
fn a_country_code_round_trips_as_upper_case_text() {
    let json = serde_json::to_string(&CountryCode::VN).expect("serialise");
    assert_eq!(json, r#""VN""#);
    let back: CountryCode = serde_json::from_str(r#""vn""#).expect("deserialise");
    assert_eq!(back, CountryCode::VN, "and accepts either case on the wire");
    assert!(serde_json::from_str::<CountryCode>(r#""VNM""#).is_err());
}

#[test]
fn a_rate_renders_without_floating_point() {
    assert_eq!(TaxRate::from_percent(10).to_string(), "10.00%");
    assert_eq!(TaxRate::from_percent(8).to_string(), "8.00%");
    assert_eq!(TaxRate::from_basis_points(1_050).to_string(), "10.50%");
    assert_eq!(
        TaxRate::from_basis_points(1).to_string(),
        "0.01%",
        "a hundredth of a percent is expressible, which is why basis points"
    );
    assert_eq!(TaxRate::ZERO.to_string(), "0.00%");
}

#[test]
fn a_rate_applies_through_the_one_money_primitive() {
    // The point of `as_ratio`: tax arithmetic goes through pos_proto::money rather than growing a
    // second rounding implementation in this module.
    let price = Money::new(CurrencyCode::VND, 100_000);
    let tax = price
        .mul_ratio(TaxRate::from_percent(10).as_ratio(), Rounding::HalfUp)
        .expect("in range");
    assert_eq!(tax, Money::new(CurrencyCode::VND, 10_000));

    let reduced = price
        .mul_ratio(TaxRate::from_percent(8).as_ratio(), Rounding::HalfUp)
        .expect("in range");
    assert_eq!(reduced, Money::new(CurrencyCode::VND, 8_000));
}

#[test]
fn a_rate_whose_components_sum_to_it_is_balanced() {
    // India's intra-state 18%: CGST 9 + SGST 9. The invariant ADR-0104 rests on.
    let table = TaxRateTable::new().with_components(
        TaxClassId::new(Ulid::from_u128(1)),
        SalesChannel::DineIn,
        TaxRate::from_percent(18),
        vec![
            TaxComponent::new("CGST", TaxRate::from_percent(9)),
            TaxComponent::new("SGST", TaxRate::from_percent(9)),
        ],
    );
    assert!(table.unbalanced_rows().is_empty());
}

#[test]
fn components_that_miss_their_rate_are_reported_rather_than_charged() {
    // 9 + 8 is not 18. The money is still 18% — `rate` is the authority — but the invoice would
    // print parts that do not add up, which is the one document an auditor rejects.
    let table = TaxRateTable::new().with_components(
        TaxClassId::new(Ulid::from_u128(1)),
        SalesChannel::DineIn,
        TaxRate::from_percent(18),
        vec![
            TaxComponent::new("CGST", TaxRate::from_percent(9)),
            TaxComponent::new("SGST", TaxRate::from_percent(8)),
        ],
    );
    assert_eq!(table.unbalanced_rows().len(), 1);
}

#[test]
fn a_row_with_no_components_is_balanced_not_empty() {
    // Vietnam and Japan publish no breakdown at all. "No components" must never read as
    // "components summing to zero", or every existing table would be reported as broken.
    let table = TaxRateTable::new().with(
        TaxClassId::new(Ulid::from_u128(1)),
        SalesChannel::DineIn,
        TaxRate::from_percent(10),
    );
    assert!(table.unbalanced_rows().is_empty());
    assert!(
        table
            .components_for(TaxClassId::new(Ulid::from_u128(1)), SalesChannel::DineIn)
            .is_empty()
    );
}

#[test]
fn a_component_name_is_a_short_upper_case_token() {
    // ADR-0168 decision 1. `ST` and `COMPCESS` are the bounds: two characters and eight.
    for name in [
        "CGST", "SGST", "UTGST", "IGST", "CESS", "GST1", "ST", "COMPCESS",
    ] {
        let token = TaxComponentName::new(name).expect("a token");
        assert_eq!(token.as_str(), name);
        assert_eq!(token.to_string(), name);
    }
    for refused in [
        "",
        "C",
        "cgst",
        "Cgst",
        "1GST",
        "CGST-X",
        "COMPCESS1",
        "C GST",
        " CGST",
        "Central GST",
        "GST@1",
        "ÇGST",
        "ＣＧＳＴ",
    ] {
        assert_eq!(
            TaxComponentName::new(refused),
            Err(TaxComponentNameError),
            "{refused:?}"
        );
    }
}

#[test]
fn a_component_name_is_a_string_on_the_wire_and_reads_only_as_a_token() {
    let token = TaxComponentName::new("UTGST").expect("a token");
    let json = serde_json::to_string(&token).expect("serialise");
    assert_eq!(json, r#""UTGST""#);
    let back: TaxComponentName = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, token);
    let owned: TaxComponentName =
        serde_json::from_value(serde_json::json!("SGST")).expect("an owned string reads too");
    assert_eq!(owned.as_str(), "SGST");
    for refused in [
        r#""cgst""#,
        r#""Central GST""#,
        r#""""#,
        r#""C""#,
        "9",
        "null",
    ] {
        assert!(
            serde_json::from_str::<TaxComponentName>(refused).is_err(),
            "{refused}"
        );
    }
}

#[test]
fn a_row_whose_component_is_named_in_free_text_is_reported() {
    // ADR-0168 decision 2. The row still balances and is still charged at its rate; the name is
    // what a bill could not record, so the check names the row.
    let table = TaxRateTable::new()
        .with_components(
            food(),
            SalesChannel::DineIn,
            TaxRate::from_percent(18),
            vec![
                TaxComponent::new("Central GST", TaxRate::from_percent(9)),
                TaxComponent::new("SGST", TaxRate::from_percent(9)),
            ],
        )
        .with_components(
            alcohol(),
            SalesChannel::DineIn,
            TaxRate::from_percent(18),
            vec![
                TaxComponent::new("CGST", TaxRate::from_percent(9)),
                TaxComponent::new("SGST", TaxRate::from_percent(9)),
            ],
        );
    let reported = table.rows_with_untokened_components();
    assert_eq!(reported.len(), 1);
    assert_eq!(reported.first().map(|row| row.tax_class_id), Some(food()));
    assert!(table.unbalanced_rows().is_empty(), "a separate check");
    assert_eq!(
        table.rate_for(food(), SalesChannel::DineIn),
        Some(TaxRate::from_percent(18))
    );
}

#[test]
fn rows_without_components_have_no_names_to_report() {
    // Vietnam and Japan publish no components, so every table they have today passes.
    let table = TaxRateTable::new()
        .with(food(), SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(food(), SalesChannel::Takeaway, TaxRate::from_percent(8))
        .with(alcohol(), SalesChannel::DineIn, TaxRate::from_percent(10));
    assert!(table.rows_with_untokened_components().is_empty());
    assert!(
        TaxRateTable::new()
            .rows_with_untokened_components()
            .is_empty()
    );
}

#[test]
fn a_row_published_before_components_existed_still_parses() {
    // The additive-on-the-wire claim, checked rather than asserted: the `tax` node is a bare
    // array of rows, and a document written by a cloud that predates ADR-0104 has no
    // `components` key at all.
    let legacy = r#"[{"tax_class_id":"00000000000000000000000001",
        "sales_channel":"DINE_IN","rate":1000}]"#;
    let table: TaxRateTable = serde_json::from_str(legacy).expect("legacy rows still parse");
    let row = table.rows().first().expect("one row");
    assert_eq!(table.rows().len(), 1);
    assert!(row.components.is_empty());
    assert_eq!(row.rate, TaxRate::from_percent(10));
}

#[test]
fn the_japanese_example_from_the_specification_resolves_both_ways() {
    // pos-spec.md §5's worked case, and the reason this is a table rather than a rate: the same
    // item is taxed differently takeaway and dine-in.
    let table = TaxRateTable::new()
        .with(food(), SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(food(), SalesChannel::Takeaway, TaxRate::from_percent(8));

    assert_eq!(
        table.rate_for(food(), SalesChannel::DineIn),
        Some(TaxRate::from_percent(10))
    );
    assert_eq!(
        table.rate_for(food(), SalesChannel::Takeaway),
        Some(TaxRate::from_percent(8))
    );
}

#[test]
fn a_missing_rate_is_none_and_never_zero() {
    // The important decision in this type. Falling back to zero would charge no tax on an item
    // nobody classified, and that is discovered by an audit rather than by a test.
    let table = TaxRateTable::new().with(food(), SalesChannel::DineIn, TaxRate::from_percent(10));
    assert_eq!(table.rate_for(alcohol(), SalesChannel::DineIn), None);
    assert_eq!(table.rate_for(food(), SalesChannel::Delivery), None);
    assert_eq!(
        TaxRateTable::new().rate_for(food(), SalesChannel::DineIn),
        None
    );
}

#[test]
fn a_flat_rate_is_the_same_model_as_a_table() {
    // Vietnam v1: one class, every channel. Stated as a test because the specification calls it a
    // special case rather than a different model, and a reader should be able to see that.
    let mut vietnam = TaxRateTable::new();
    for channel in [
        SalesChannel::DineIn,
        SalesChannel::Takeaway,
        SalesChannel::Delivery,
        SalesChannel::Qr,
        SalesChannel::Api,
    ] {
        vietnam = vietnam.with(food(), channel, TaxRate::from_percent(10));
    }
    for channel in [SalesChannel::DineIn, SalesChannel::Api] {
        assert_eq!(
            vietnam.rate_for(food(), channel),
            Some(TaxRate::from_percent(10))
        );
    }
}

#[test]
fn a_row_for_a_channel_this_build_does_not_know_matches_nothing() {
    // Forward compatibility without a wrong answer: an unrecognised channel deserialises rather
    // than failing, but it must not silently serve as the rate for DINE_IN, which is what
    // `Open::known()` reporting `Unspecified` would otherwise cause.
    let json = format!(
        r#"[{{"tax_class_id":"{}","sales_channel":"SALES_CHANNEL_DRIVE_THROUGH","rate":1000}}]"#,
        food()
    );
    let table: TaxRateTable = serde_json::from_str(&json).expect("an unknown channel deserialises");
    assert_eq!(table.rows().len(), 1);
    let row = table.rows().first().expect("one row");
    assert!(row.sales_channel.is_unrecognised());
    assert_eq!(
        table.rate_for(food(), SalesChannel::DineIn),
        None,
        "an unrecognised channel must not answer for a known one"
    );
}

#[test]
fn a_locale_pack_round_trips_and_tolerates_a_field_from_the_future() {
    let pack = LocalePack {
        country_code: CountryCode::VN,
        currency_code: CurrencyCode::VND,
        tax_rate_table: TaxRateTable::new().with(
            food(),
            SalesChannel::DineIn,
            TaxRate::from_percent(10),
        ),
        number_format: NumberFormat {
            decimal_separator: ',',
            group_separator: '.',
            digits_per_group: 3,
        },
        default_language: TranslationKey::new("vi"),
        currency_exponent: 0,
        default_retention_days: 365,
        prices_include_tax: false,
        cash_rounding_increment: Some(1_000),
        cash_denominations: vec![50_000, 100_000, 200_000],
        cash_count_denominations: vec![1_000, 2_000, 5_000, 10_000],
    };
    let json = serde_json::to_string(&pack).expect("serialise");
    let back: LocalePack = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, pack);

    // A newer cloud adds a field. An older store must still apply the pack, or it stops being
    // manageable — the same rule the event envelope follows.
    let extended = json.replace('{', r#"{"a_field_from_the_future":true,"#);
    assert!(
        serde_json::from_str::<LocalePack>(&extended).is_ok(),
        "an unknown field must not make a locale pack unusable"
    );
}

#[test]
fn a_locale_node_that_sets_no_tax_rounding_rounds_half_up_as_before() {
    for node in [
        "{}",
        r#"{ "tax_rounding": "TAX_ROUNDING_UNSPECIFIED" }"#,
        r#"{ "tax_rounding": "TAX_ROUNDING_HALF_UP" }"#,
        // A country pack's own fields beside it do not stop the setting being read.
        r#"{ "currency_code": "JPY", "timezone": "Asia/Tokyo", "cutoff_hour": 4,
             "prices_include_tax": true, "cash_rounding_increment": 10 }"#,
    ] {
        let settings: LocaleSettings = serde_json::from_str(node).expect("the node parses");
        assert_eq!(settings.tax_rounding(), TaxRounding::HalfUp, "{node}");
        assert_eq!(
            settings.tax_rounding().rounding(),
            Rounding::HalfUp,
            "{node}"
        );
    }
    assert_eq!(
        LocaleSettings::default().tax_rounding(),
        TaxRounding::HalfUp
    );
}

#[test]
fn a_store_set_down_drops_the_fraction_and_a_mode_from_a_newer_release_reads_as_half_up() {
    let down: LocaleSettings = serde_json::from_str(r#"{ "tax_rounding": "TAX_ROUNDING_DOWN" }"#)
        .expect("the node parses");
    assert_eq!(down.tax_rounding(), TaxRounding::Down);
    assert_eq!(down.tax_rounding().rounding(), Rounding::TowardZero);

    let newer = r#"{"tax_rounding":"TAX_ROUNDING_HALF_EVEN"}"#;
    let settings: LocaleSettings = serde_json::from_str(newer).expect("an unknown token parses");
    assert!(settings.tax_rounding.is_unrecognised());
    assert_eq!(settings.tax_rounding(), TaxRounding::HalfUp);
    assert_eq!(
        serde_json::to_string(&settings).expect("serialise"),
        newer,
        "a mode this release does not know goes back out as it came"
    );
    assert_eq!(
        serde_json::to_string(&LocaleSettings::default()).expect("serialise"),
        "{}",
        "a node that sets nothing is written as nothing"
    );
    assert_eq!(TaxRounding::Down.as_wire(), "TAX_ROUNDING_DOWN");
    assert_eq!(TaxRounding::HalfUp.as_wire(), "TAX_ROUNDING_HALF_UP");
}

#[test]
fn the_default_number_format_is_the_common_one_not_the_vietnamese_one() {
    // Vietnam writes 120.000,50 and the default here is 120,000.50. That is deliberate: a default
    // should be the least surprising to a reader of the code, and every country module states its
    // own format explicitly rather than inheriting one.
    let default = NumberFormat::default();
    assert_eq!(default.group_separator, ',');
    assert_eq!(default.decimal_separator, '.');
    assert_eq!(default.digits_per_group, 3);
}

/// A store's `locale` node as it reaches the edge: the country's fields the locale publish
/// writes, the cloud's own beside them, and a setting from the Tenant layer.
fn a_published_locale_node() -> serde_json::Value {
    serde_json::json!({
        "country_code": "VN",
        "currency_code": "VND",
        "currency_exponent": 0,
        "timezone": "Asia/Ho_Chi_Minh",
        "cutoff_hour": 4,
        "prices_include_tax": false,
        "cash_rounding_increment": 1000,
        "cash_denominations": [10_000, 20_000, 50_000],
        "cash_count_denominations": [1_000, 2_000, 5_000, 10_000],
        "default_retention_days": 365,
        "number_format": {
            "decimal_separator": ",",
            "group_separator": ".",
            "digits_per_group": 3,
        },
        "country_language": "vi",
        "display_language": "vi",
        "tax_rounding": "TAX_ROUNDING_DOWN",
    })
}

#[test]
fn a_published_locale_round_trips_and_its_settings_read_from_the_same_node() {
    let node = a_published_locale_node();
    let locale: PublishedLocale = serde_json::from_value(node.clone()).expect("the node parses");
    assert_eq!(locale.currency_code, "VND");
    assert_eq!(locale.currency_exponent, Some(0));
    assert_eq!(locale.cash_rounding_increment, Some(1000));
    assert_eq!(
        locale.cash_count_denominations,
        [1_000, 2_000, 5_000, 10_000]
    );
    assert_eq!(
        locale
            .number_format
            .as_ref()
            .and_then(PublishedNumberFormat::validate),
        Some(NumberFormat {
            decimal_separator: ',',
            group_separator: '.',
            digits_per_group: 3,
        })
    );
    assert_eq!(locale.country_language.as_deref(), Some("vi"));

    // Every field the type reads comes back as it was written; the others belong to the cloud
    // and to the register's view.
    let mut read_back = node.clone();
    for not_this_view in ["country_code", "display_language", "tax_rounding"] {
        read_back
            .as_object_mut()
            .expect("an object")
            .remove(not_this_view);
    }
    assert_eq!(serde_json::to_value(&locale).expect("serialise"), read_back);

    let settings: LocaleSettings = serde_json::from_value(node).expect("the settings parse");
    assert_eq!(settings.tax_rounding(), TaxRounding::Down);
}

#[test]
fn an_absent_field_stays_absent_and_an_unknown_one_is_ignored() {
    let locale: PublishedLocale = serde_json::from_value(serde_json::json!({
        "currency_code": "JPY",
        "timezone": "Asia/Tokyo",
        "cutoff_hour": 5,
        "from_a_newer_release": { "nested": true },
    }))
    .expect("the required fields are enough");
    assert_eq!(
        locale,
        PublishedLocale {
            currency_code: "JPY".to_owned(),
            currency_exponent: None,
            timezone: "Asia/Tokyo".to_owned(),
            cutoff_hour: 5,
            prices_include_tax: false,
            cash_rounding_increment: None,
            cash_denominations: Vec::new(),
            cash_count_denominations: Vec::new(),
            default_retention_days: None,
            number_format: None,
            country_language: None,
        }
    );
    assert_eq!(
        serde_json::to_value(&locale).expect("serialise"),
        serde_json::json!({
            "currency_code": "JPY",
            "timezone": "Asia/Tokyo",
            "cutoff_hour": 5,
            "prices_include_tax": false,
            "cash_denominations": [],
        })
    );
}

#[test]
fn a_node_without_the_country_s_required_fields_is_not_a_published_locale_but_is_settings() {
    // The two views stay two types for this: a settings-only node must not read as a country's
    // locale, or the edge would put the store's tax posture and cash rounding back to defaults.
    for required in ["currency_code", "timezone", "cutoff_hour"] {
        let mut node = a_published_locale_node();
        node.as_object_mut().expect("an object").remove(required);
        assert!(
            serde_json::from_value::<PublishedLocale>(node.clone()).is_err(),
            "{required}"
        );
        let settings: LocaleSettings =
            serde_json::from_value(node).expect("the settings read any node");
        assert_eq!(settings.tax_rounding(), TaxRounding::Down, "{required}");
    }

    let settings_only = serde_json::json!({ "tax_rounding": "TAX_ROUNDING_DOWN" });
    assert!(serde_json::from_value::<PublishedLocale>(settings_only.clone()).is_err());
    let settings: LocaleSettings =
        serde_json::from_value(settings_only).expect("the settings read any node");
    assert_eq!(settings.tax_rounding(), TaxRounding::Down);
}

#[test]
fn a_published_number_format_takes_one_character_each_side_and_a_group_of_digits() {
    let format = |decimal: &str, group: &str, digits_per_group: u8| PublishedNumberFormat {
        decimal_separator: decimal.to_owned(),
        group_separator: group.to_owned(),
        digits_per_group,
    };
    assert_eq!(
        format(",", "\u{a0}", 3).validate(),
        Some(NumberFormat {
            decimal_separator: ',',
            group_separator: '\u{a0}',
            digits_per_group: 3,
        })
    );
    for refused in [
        format("", ".", 3),
        format(",", "..", 3),
        format(",", ".", 0),
    ] {
        assert_eq!(refused.validate(), None, "{refused:?}");
    }
}
