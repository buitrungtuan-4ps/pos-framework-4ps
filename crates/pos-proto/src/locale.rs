// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Locale packs: what a country's law fixes.
//!
//! # Why these types are here and not in `pos-country`
//!
//! Two reasons, and either alone would be enough.
//!
//! `pos-core` computes tax, so it needs to read a rate table — and `pos-core` must not depend on
//! `pos-ports` or on anything downstream of it
//! ([ADR-0013](../../../docs/adr/0013-async-strategy.md)). `pos-proto` is the only crate both
//! siblings share.
//!
//! And a locale pack crosses the wire: the cloud publishes it to stores inside the configuration
//! tree, so it needs the same forward-compatible serialisation as everything else here.
//!
//! # Country module or configuration?
//!
//! [ADR-0027](../../../docs/adr/0027-country-modules.md) draws the line: **the country module ships
//! what the law says, and configuration overrides it.** A [`LocalePack`] is the default a fresh
//! store is correct with before anybody has typed a rate table. `store.tax.tax_class_rates`
//! overrides it, because a store may sit in a special economic zone, and because a legislative
//! change can land before a release ships — an operator must be able to correct a rate without
//! waiting for a build.
//!
//! Note what is deliberately **absent**: the store's timezone. Indonesia spans three and the United
//! States spans six, so a country-level timezone would be wrong exactly where it mattered.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::enums::SalesChannel;
use crate::ids::TaxClassId;
use crate::money::CurrencyCode;
use crate::wire_enum::Open;

/// An ISO 3166-1 alpha-2 country code, upper-case.
///
/// Two bytes rather than a string: it is a fixed-width code, it appears in a hostname
/// ([ADR-0011](../../../docs/adr/0011-country-in-hostname.md)), and validating it here means no
/// later stage has to wonder whether `"Vietnam"` or `"vnm"` might turn up.
///
/// `ZZ` is CLDR's unknown region and is used by the reference country module, so it can never
/// collide with a real country.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CountryCode([u8; 2]);

/// Why a country code was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountryCodeError {
    /// Not exactly two bytes.
    Length,
    /// Contained something other than an ASCII letter.
    NotAlphabetic,
}

impl fmt::Display for CountryCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Length => "a country code is two letters",
            Self::NotAlphabetic => "a country code is ASCII letters only",
        })
    }
}

impl core::error::Error for CountryCodeError {}

impl CountryCode {
    /// Vietnam, the first country deployed.
    pub const VN: Self = Self([b'V', b'N']);
    /// Japan, the worked example for channel-keyed tax in `docs/pos-spec.md` §5.
    pub const JP: Self = Self([b'J', b'P']);
    /// CLDR's unknown region, used by the reference country module.
    pub const ZZ: Self = Self([b'Z', b'Z']);

    /// Validates and wraps a code, accepting either case and storing upper-case.
    ///
    /// Case-insensitive on input because `countries/vn/` is lower-case on disk while the code is
    /// upper-case by the standard, and a framework that made a forker care about that difference
    /// would be creating work rather than removing it.
    ///
    /// # Errors
    ///
    /// [`CountryCodeError`] if the input is not two ASCII letters.
    pub fn parse(code: &str) -> Result<Self, CountryCodeError> {
        let bytes = code.as_bytes();
        let [first, second] = bytes else {
            return Err(CountryCodeError::Length);
        };
        if !first.is_ascii_alphabetic() || !second.is_ascii_alphabetic() {
            return Err(CountryCodeError::NotAlphabetic);
        }
        Ok(Self([
            first.to_ascii_uppercase(),
            second.to_ascii_uppercase(),
        ]))
    }

    /// The code as upper-case text, for a hostname label or a log field.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // The bytes are ASCII letters by construction, so this cannot fail. Written as a fallible
        // conversion with a fallback rather than an `expect`, because the backbone crates are
        // compiled with `-F clippy::expect_used` and a total function is better than an exemption.
        core::str::from_utf8(&self.0).unwrap_or("ZZ")
    }

    /// The code as lower-case, which is how it appears on disk and in a hostname.
    #[must_use]
    pub fn as_directory(&self) -> String {
        self.as_str().to_ascii_lowercase()
    }
}

impl fmt::Display for CountryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for CountryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CountryCode({})", self.as_str())
    }
}

impl Serialize for CountryCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CountryCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'_, str>>::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// A tax rate in basis points: one hundredth of one percent.
///
/// Integer, because `clippy.toml` bans floating point workspace-wide and because a rate rendered as
/// `0.09999999` on a legal document is a conversation with an auditor. 10% is `1000`, Japan's
/// reduced 8% is `800`, and a tenth of a percent is expressible.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaxRate {
    basis_points: u32,
}

impl TaxRate {
    /// No tax.
    pub const ZERO: Self = Self { basis_points: 0 };

    /// A rate from basis points.
    #[must_use]
    pub const fn from_basis_points(basis_points: u32) -> Self {
        Self { basis_points }
    }

    /// A whole-percent rate, for the common case.
    #[must_use]
    pub const fn from_percent(percent: u32) -> Self {
        Self {
            basis_points: percent.saturating_mul(100),
        }
    }

    /// The rate in basis points.
    #[must_use]
    pub const fn basis_points(self) -> u32 {
        self.basis_points
    }

    /// The rate as a ratio, for arithmetic against [`crate::Money`].
    ///
    /// Returned as a [`Ratio`](crate::money::Ratio) rather than applied here, so that all money
    /// arithmetic keeps going through the one rounding primitive in `pos_proto::money` instead of a
    /// second implementation growing in this module.
    #[must_use]
    pub const fn as_ratio(self) -> crate::money::Ratio {
        // 10_000 is never zero, so the NonZeroI64 construction below cannot fail. Written with a
        // fallback rather than an `expect` for the reason given on `CountryCode::as_str`.
        match core::num::NonZeroI64::new(10_000) {
            Some(denominator) => crate::money::Ratio::new(self.basis_points as i64, denominator),
            None => crate::money::Ratio::new(0, core::num::NonZeroI64::MIN),
        }
    }
}

impl fmt::Display for TaxRate {
    /// Renders as a percentage with two decimal places, without floating point.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{:02}%",
            self.basis_points / 100,
            self.basis_points % 100
        )
    }
}

impl fmt::Debug for TaxRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TaxRate({self})")
    }
}

/// One row of a tax rate table.
///
/// A list of rows rather than a nested map, because it has to survive JSON round-tripping in the
/// configuration tree, and because `docs/adr/0010-naming-standard.md` wants a shape a person can
/// read in a diff. A map keyed by a composite would serialise as a stringified tuple.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TaxRateRow {
    /// Which class of item.
    pub tax_class_id: TaxClassId,
    /// Which channel. `Open`, so a rate table published by a newer cloud that has learned a channel
    /// this build has not does not fail to deserialise.
    pub sales_channel: Open<SalesChannel>,
    /// The rate in force.
    pub rate: TaxRate,
}

/// Tax rates, keyed by item class and sales channel.
///
/// The channel dimension is why this is a table rather than a rate. `docs/pos-spec.md` §5's worked
/// example is Japan: the same item is 8% takeaway and 10% dine-in. Vietnam v1 populates one class at
/// one rate, which is a *special case* of this table rather than a different model — and having both
/// dimensions from day one is what avoids a migration across every order line ever written.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaxRateTable {
    rows: Vec<TaxRateRow>,
}

impl TaxRateTable {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self { rows: Vec::new() }
    }

    /// A table from rows.
    #[must_use]
    pub const fn from_rows(rows: Vec<TaxRateRow>) -> Self {
        Self { rows }
    }

    /// Adds a row.
    #[must_use]
    pub fn with(
        mut self,
        tax_class_id: TaxClassId,
        sales_channel: SalesChannel,
        rate: TaxRate,
    ) -> Self {
        self.rows.push(TaxRateRow {
            tax_class_id,
            sales_channel: Open::from_known(sales_channel),
            rate,
        });
        self
    }

    /// Every row.
    #[must_use]
    pub fn rows(&self) -> &[TaxRateRow] {
        &self.rows
    }

    /// The rate for a class on a channel.
    ///
    /// Returns `None` rather than falling back to zero when there is no row. That is deliberate and
    /// it is the important decision in this type: a missing rate is a **configuration error**, and
    /// silently charging no tax on an item nobody classified is the kind of bug that is discovered
    /// by a tax audit rather than by a test. The caller decides — refuse the sale, or use a
    /// documented default — and either way it is a visible choice.
    #[must_use]
    pub fn rate_for(
        &self,
        tax_class_id: TaxClassId,
        sales_channel: SalesChannel,
    ) -> Option<TaxRate> {
        self.rows
            .iter()
            .find(|row| {
                row.tax_class_id == tax_class_id
                    && row.sales_channel.known() == sales_channel
                    && !row.sales_channel.is_unrecognised()
            })
            .map(|row| row.rate)
    }

    /// Whether the table says anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// How a country writes numbers.
///
/// Held as separators rather than as a format string, because a format string is a small language
/// and every small language eventually needs an escape rule. Grouping is *digits per group* so that
/// India's 2-2-3 lakh grouping is expressible later by widening this field rather than by replacing
/// the type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct NumberFormat {
    /// Between the integer and fractional parts. `.` in Vietnam and Japan, `,` in much of Europe.
    pub decimal_separator: char,
    /// Between groups of digits. `.` in Vietnam, `,` in Japan.
    pub group_separator: char,
    /// Digits per group, counting from the decimal separator.
    pub digits_per_group: u8,
}

impl Default for NumberFormat {
    fn default() -> Self {
        Self {
            decimal_separator: '.',
            group_separator: ',',
            digits_per_group: 3,
        }
    }
}

/// Everything a country's law and locale fix, as a default a fresh store is correct with.
///
/// Published to stores inside the configuration tree, so it is versioned and overridable — see
/// [ADR-0027](../../../docs/adr/0027-country-modules.md) for which half of each pair belongs here
/// and which belongs to configuration.
///
/// No `deny_unknown_fields`, deliberately, and for the same reason as the event envelope: a store
/// running an older build must apply a locale pack carrying a field it does not understand rather
/// than refusing it, because a store that will not accept configuration is a store that has stopped
/// being manageable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LocalePack {
    /// Which country.
    pub country_code: CountryCode,
    /// The currency its law denominates in.
    pub currency_code: CurrencyCode,
    /// The default tax rate table. Overridable per store by `store.tax.tax_class_rates`.
    pub tax_rate_table: TaxRateTable,
    /// How numbers are written.
    pub number_format: NumberFormat,
    /// The `en`-relative language a fresh store starts in, as a BCP 47 tag.
    ///
    /// `en` is always present as a fallback (`docs/pos-spec.md` §9), so this names the *preferred*
    /// language rather than the only one.
    pub default_language: crate::text::TranslationKey,
    /// How long personal data is kept by default, in days.
    ///
    /// A default and not a determination: `docs/pos-spec.md` §11 is explicit that the framework
    /// makes no legal judgement and the operator is the data controller. Vietnam's PDPD
    /// (Decree 13/2023) and the GDPR both put that duty on the operator, so this is a starting value
    /// somebody must confirm — not a compliance claim the framework is making on their behalf.
    pub default_retention_days: u16,
}

impl LocalePack {
    /// The rate for a class on a channel, from this pack's default table.
    ///
    /// A thin forward to [`TaxRateTable::rate_for`], present so a caller holding a pack does not
    /// have to reach through two fields and so the `None`-means-unconfigured rule has one place to
    /// be documented.
    #[must_use]
    pub fn rate_for(
        &self,
        tax_class_id: TaxClassId,
        sales_channel: SalesChannel,
    ) -> Option<TaxRate> {
        self.tax_rate_table.rate_for(tax_class_id, sales_channel)
    }
}

#[cfg(test)]
mod tests {
    use super::{CountryCode, CountryCodeError, LocalePack, NumberFormat, TaxRate, TaxRateTable};
    use crate::enums::SalesChannel;
    use crate::ids::TaxClassId;
    use crate::money::{CurrencyCode, Money, Rounding};
    use crate::text::TranslationKey;
    use crate::ulid::Ulid;

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
        let table =
            TaxRateTable::new().with(food(), SalesChannel::DineIn, TaxRate::from_percent(10));
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
        let table: TaxRateTable =
            serde_json::from_str(&json).expect("an unknown channel deserialises");
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
            default_retention_days: 365,
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
    fn the_default_number_format_is_the_common_one_not_the_vietnamese_one() {
        // Vietnam writes 120.000,50 and the default here is 120,000.50. That is deliberate: a default
        // should be the least surprising to a reader of the code, and every country module states its
        // own format explicitly rather than inheriting one.
        let default = NumberFormat::default();
        assert_eq!(default.group_separator, ',');
        assert_eq!(default.decimal_separator, '.');
        assert_eq!(default.digits_per_group, 3);
    }
}
