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
use crate::money::{CurrencyCode, Rounding};
use crate::wire_enum;
use crate::wire_enum::{Open, is_absent};

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
    /// India, the worked example for multi-component tax in
    /// [ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md).
    pub const IN: Self = Self([b'I', b'N']);
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

/// One named part of a tax rate, for a country whose invoice must print the parts.
///
/// India is the case this exists for: an intra-state sale charged 18 % GST prints **CGST 9 % and
/// SGST 9 % on separate lines**, because the two halves go to different governments. Printing the
/// sum is not a terser rendering of the same fact — it is not a valid invoice
/// ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)).
///
/// The name is free text carried straight to the invoice, not an enum: the framework cannot know
/// every jurisdiction's label, and a closed set would make each new country a code change, which is
/// the thing country packs exist to avoid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TaxComponent {
    /// What the invoice calls it — `CGST`, `SGST`, `IGST`, `TVA`.
    pub name: String,
    /// This part's share of the row's rate. The parts must sum to the row's `rate`.
    pub rate: TaxRate,
}

impl TaxComponent {
    /// A named component at a rate.
    #[must_use]
    pub fn new(name: impl Into<String>, rate: TaxRate) -> Self {
        Self {
            name: name.into(),
            rate,
        }
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
    /// The rate in force. Stays the authority on what the guest pays, whatever the components say.
    pub rate: TaxRate,
    /// How that rate is broken out on the invoice, when a country requires it.
    ///
    /// Empty for Vietnam and Japan, which print one rate and always did — so a country pays for this
    /// only if it needs it. `#[serde(default)]` is what makes the field additive on the wire in both
    /// directions: an older edge reading a newer publish ignores the key and still charges `rate`,
    /// which is the correct total, and a newer edge reading an older publish sees an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<TaxComponent>,
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
            components: Vec::new(),
        });
        self
    }

    /// Adds a row whose rate prints as named components.
    ///
    /// The components are not validated here — a builder that refused would have to return a
    /// `Result` and every existing call site is infallible. [`Self::unbalanced_rows`] is the check,
    /// run where a table is authored or applied, so a bad table is refused with a message naming the
    /// row rather than rejected one call at a time.
    #[must_use]
    pub fn with_components(
        mut self,
        tax_class_id: TaxClassId,
        sales_channel: SalesChannel,
        rate: TaxRate,
        components: Vec<TaxComponent>,
    ) -> Self {
        self.rows.push(TaxRateRow {
            tax_class_id,
            sales_channel: Open::from_known(sales_channel),
            rate,
            components,
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

    /// The components for a class on a channel, empty when the row prints one rate.
    ///
    /// Separate from [`Self::rate_for`] rather than returned beside it, because the two answer
    /// different questions and only one of them can stop a sale: a missing *rate* is a refusal, a
    /// missing *breakdown* is the ordinary case in most of the world.
    #[must_use]
    pub fn components_for(
        &self,
        tax_class_id: TaxClassId,
        sales_channel: SalesChannel,
    ) -> &[TaxComponent] {
        self.rows
            .iter()
            .find(|row| {
                row.tax_class_id == tax_class_id
                    && row.sales_channel.known() == sales_channel
                    && !row.sales_channel.is_unrecognised()
            })
            .map_or(&[], |row| row.components.as_slice())
    }

    /// Rows whose components do not sum to their own rate.
    ///
    /// The invariant ADR-0104 rests on. A row that fails it would print an invoice whose parts do
    /// not add up to the tax charged — which is the one way this feature can produce a document an
    /// auditor rejects, so it is checked rather than assumed. An empty component list always passes:
    /// it is "no breakdown", not "a breakdown summing to zero".
    ///
    /// Returns the offending rows so a refusal can name them; empty means the table is sound.
    #[must_use]
    pub fn unbalanced_rows(&self) -> Vec<&TaxRateRow> {
        self.rows
            .iter()
            .filter(|row| {
                !row.components.is_empty()
                    && row
                        .components
                        .iter()
                        .map(|component| u64::from(component.rate.basis_points()))
                        .sum::<u64>()
                        != u64::from(row.rate.basis_points())
            })
            .collect()
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
    /// How many decimal places that currency has: `0` for the đồng and the yen, `2` for the paisa
    /// and the cent ([ADR-0134](../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md)).
    ///
    /// Money is an integer in the minor unit and floating point is banned at every layer
    /// (`docs/naming-and-api.md` §4), so this is the one fact that turns that integer back into
    /// something a person reads — and the one fact nothing in this tree used to hold. It was a
    /// three-row table in the front end ending in `?? 0`, which is right for the đồng and the yen
    /// and silently wrong for the rupee: ₹261.45 drew as `INR 26,145`, a cashier typing a discount
    /// was out by a hundred, and the receipt printed raw paise.
    ///
    /// **Required, not defaulted.** A default here would be the same defect one layer down: a
    /// country that forgot to say would be given a zero that is indistinguishable from a real
    /// answer. Required means a pack that omits it does not compile, which is a stronger gate than
    /// any check and needs no check at all. The *wire* copy is optional, because a cloud that
    /// predates the field must still publish a locale node an edge can apply — a different
    /// obligation, in a different type.
    pub currency_exponent: u8,
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
    /// Whether this country's menu prices already contain their tax
    /// ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)).
    ///
    /// Japan quotes 税込 and India quotes MRP; Vietnam quotes exclusive. The store's `locale` node
    /// still overrides it — this is what a fresh store in the country is correct with, so nobody
    /// provisioning the fortieth Japanese shop has to remember the box.
    #[serde(default)]
    pub prices_include_tax: bool,
    /// What the grand total is rounded to in cash, in minor units, or `None` for no rounding.
    ///
    /// A fact about the country's **coinage**, not about its tax: India rounds to the rupee because
    /// no smaller coin settles the difference, Vietnam to the thousand đồng because that is the
    /// smallest note, and Japan not at all because the 1-yen coin circulates. Applied once to the
    /// grand total and materialised as [`crate::Money`] on an explicit receipt line, so the bill
    /// still reconciles.
    #[serde(default)]
    pub cash_rounding_increment: Option<i64>,
    /// The notes a guest hands over, ascending, in minor units — the till's quick-cash keys.
    ///
    /// Empty is a legitimate answer and means "offer the exact amount only": an unhelpful key on a
    /// till is worse than no key, and a country nobody has filled in should not have amounts guessed
    /// for it. Notes rather than coins, because a coin is not a key a cashier presses.
    #[serde(default)]
    pub cash_denominations: Vec<i64>,
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

wire_enum! {
    /// How a store rounds each tax amount and each fee to the currency's minor unit
    /// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// decision 2, [ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 2).
    ///
    /// It rounds the tax of each tax class, the tax on a charge taxed at a class no line is in, and
    /// each fee's amount, whether prices include their tax or not; a tax line's named parts are
    /// shares of its rounded tax, so they follow it. It does not round the cash rounding of the
    /// grand total, which is a fact about the country's coins and stays half-up, nor a line's price,
    /// a campaign or a recipe's consumption, which keep their own rounding.
    TaxRounding, prefix = "TAX_ROUNDING";
    /// Half a minor unit or more rounds away from zero, and less rounds toward it: what every store
    /// did before the setting existed.
    HalfUp = "HALF_UP",
    /// The fraction of a minor unit is dropped, toward zero: how many Japanese businesses round
    /// consumption tax (切り捨て).
    Down = "DOWN",
}

impl TaxRounding {
    /// The rounding the bill's arithmetic takes for this mode: [`Rounding::HalfUp`] for
    /// [`Self::HalfUp`], and [`Rounding::TowardZero`] for [`Self::Down`].
    /// `TAX_ROUNDING_UNSPECIFIED` is half-up, the default.
    #[must_use]
    pub const fn rounding(self) -> Rounding {
        match self {
            Self::Unspecified | Self::HalfUp => Rounding::HalfUp,
            Self::Down => Rounding::TowardZero,
        }
    }
}

/// The published `locale` node, as a store applies it: the store's currency, timezone, business-date
/// cutoff, tax posture, cash rounding and number format, which the cloud's locale publish writes
/// from the store's country pack ([ADR-0074](../../../docs/adr/0074-localization-and-tax.md),
/// Track M4).
///
/// **One of two views of the node.** This is the country's view, and [`LocaleSettings`] is the
/// register's, the settings the cloud writes beside these fields. They stay two types:
/// `currency_code`, `timezone` and `cutoff_hour` are required here, so a node without them, such as
/// a `locale` node carrying only settings, does not parse as this type, while [`LocaleSettings`]
/// reads any node. That refusal is load-bearing. The edge applies `prices_include_tax`,
/// `cash_rounding_increment`, `cash_denominations` and `country_language` as the node states them,
/// absent included, so a settings-only node that parsed would put a store's tax posture and cash
/// rounding back to their defaults.
///
/// Deliberately permissive: the fields are strings and plain numbers rather than domain types, so a
/// value the edge cannot use, such as a currency code that is not one, costs that field alone
/// rather than the whole node. A value of the wrong JSON type still fails the whole node. No
/// `deny_unknown_fields`, as for every published node: the cloud's own fields, `country_code` and
/// `display_language`, ride the same node, and a node carrying a field from a newer release still
/// applies. An absent optional field stays off the wire; `prices_include_tax` and
/// `cash_denominations` are written as they read, as the cloud writes them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedLocale {
    /// The store's currency, as an ISO 4217 code: a string, narrowed to a [`CurrencyCode`] where it
    /// is applied, so a code that does not parse costs the currency alone.
    pub currency_code: String,
    /// How many decimal places the currency has (ADR-0134). `#[serde(default)]` because a cloud
    /// that predates the field must still publish a locale node an edge can apply, and `None` then
    /// means "leave what the session has" — not zero, which would be the very silent default that
    /// record is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency_exponent: Option<u8>,
    /// The store's IANA timezone, such as `Asia/Ho_Chi_Minh`: a string, checked against the timezone
    /// database where it is applied, so a name that is not one costs the timezone alone.
    pub timezone: String,
    /// The hour of the day the store's business date turns over, checked where it is applied.
    pub cutoff_hour: u8,
    /// Whether this store quotes tax-inclusive prices (ADR-0104). `#[serde(default)]` so a locale
    /// node published before this field existed still applies, as the exclusive posture it meant.
    #[serde(default)]
    pub prices_include_tax: bool,
    /// What the grand total is rounded to in cash, in minor units (ADR-0105). Absent means no
    /// rounding, which is what every store did before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cash_rounding_increment: Option<i64>,
    /// The notes the till offers as quick-cash keys, in minor units. Absent means the exact amount
    /// only, which is the front end's own fallback, so an older publish changes nothing.
    #[serde(default)]
    pub cash_denominations: Vec<i64>,
    /// How many days the store keeps a personal record before its own sweep scrubs it (ADR-0107).
    /// Absent leaves the session's current figure — the country pack's default — rather than zero,
    /// which would scrub a buyer the moment the invoice was printed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_retention_days: Option<u16>,
    /// How this store's country writes a number
    /// ([ADR-0136](../../../docs/adr/0136-a-store-publishes-how-it-writes-numbers.md)). Absent for a
    /// cloud that predates the field, which leaves the session's own — the `en-US`-shaped default
    /// every surface used before the node could say otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_format: Option<PublishedNumberFormat>,
    /// The language of the store's country, from the cloud's pack for it (ADR-0105), which a receipt
    /// set to `RECEIPT_LANGUAGE_COUNTRY` prints in (ADR-0160). Absent for a cloud that predates the
    /// field, and such a receipt prints in the display language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country_language: Option<String>,
}

/// A published number format, in the permissive shape the rest of [`PublishedLocale`] uses.
///
/// Separators are `String` rather than `char` and are narrowed on the way in, for the reason
/// `currency_code` is a `String` there and a [`CurrencyCode`] after: a value [`PublishedLocale`]
/// cannot deserialize takes the **whole** locale node down with it, and a store that loses its
/// currency and its timezone because somebody published a two-character group separator is a worse
/// outcome than one that keeps the format it had.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedNumberFormat {
    /// Between the integer and fractional parts, as [`NumberFormat::decimal_separator`].
    pub decimal_separator: String,
    /// Between groups of digits, as [`NumberFormat::group_separator`].
    pub group_separator: String,
    /// Digits per group, as [`NumberFormat::digits_per_group`].
    pub digits_per_group: u8,
}

impl PublishedNumberFormat {
    /// The domain form, or `None` when the publish does not describe one.
    ///
    /// A separator has to be exactly one character — not zero, which would run the digits together,
    /// and not two, which no formatter here can place. A group of zero digits would loop forever in
    /// any grouping routine that trusted it.
    #[must_use]
    pub fn validate(&self) -> Option<NumberFormat> {
        let mut decimal = self.decimal_separator.chars();
        let mut group = self.group_separator.chars();
        let (decimal_separator, group_separator) = (decimal.next()?, group.next()?);
        if decimal.next().is_some() || group.next().is_some() || self.digits_per_group == 0 {
            return None;
        }
        Some(NumberFormat {
            decimal_separator,
            group_separator,
            digits_per_group: self.digits_per_group,
        })
    }
}

/// The settings on the published `locale` node, as far as the register reads it (ADR-0160).
///
/// The node is older than this type: the cloud's locale publish writes the store's currency,
/// timezone, cutoff, tax posture and cash rounding onto it from the store's country pack, and the
/// edge reads those through [`PublishedLocale`], the node's other view. This type carries only the
/// fields that are settings in [`crate::settings`], which the cloud writes beside them, and leaves
/// the rest to that view. It has no `deny_unknown_fields`, so a node carrying the country's fields
/// parses, and so does one carrying a field from a newer release.
///
/// Every default is what the edge did before the field existed, so a node without a field runs a
/// store as it ran before.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LocaleSettings {
    /// How the store rounds each tax amount and each fee. Read it through
    /// [`LocaleSettings::tax_rounding`], which gives an absent or unknown value its default. Left
    /// off the wire while absent.
    #[serde(default, skip_serializing_if = "is_absent")]
    pub tax_rounding: Open<TaxRounding>,
}

impl LocaleSettings {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "locale";

    /// How the store rounds each tax amount and each fee.
    ///
    /// An absent value, `TAX_ROUNDING_UNSPECIFIED`, and a value this release does not know all read
    /// as [`TaxRounding::HalfUp`], the default and what every store did before the setting existed.
    /// A value from a newer release is never offered to a store that cannot honour it (ADR-0160
    /// decision 5), so the default is what such a store was running anyway.
    #[must_use]
    pub fn tax_rounding(&self) -> TaxRounding {
        match self.tax_rounding.known() {
            TaxRounding::Unspecified | TaxRounding::HalfUp => TaxRounding::HalfUp,
            TaxRounding::Down => TaxRounding::Down,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CountryCode, CountryCodeError, LocalePack, LocaleSettings, NumberFormat, PublishedLocale,
        PublishedNumberFormat, TaxComponent, TaxRate, TaxRateTable, TaxRounding,
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
            currency_exponent: 0,
            default_retention_days: 365,
            prices_include_tax: false,
            cash_rounding_increment: Some(1_000),
            cash_denominations: vec![50_000, 100_000, 200_000],
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
        let down: LocaleSettings =
            serde_json::from_str(r#"{ "tax_rounding": "TAX_ROUNDING_DOWN" }"#)
                .expect("the node parses");
        assert_eq!(down.tax_rounding(), TaxRounding::Down);
        assert_eq!(down.tax_rounding().rounding(), Rounding::TowardZero);

        let newer = r#"{"tax_rounding":"TAX_ROUNDING_HALF_EVEN"}"#;
        let settings: LocaleSettings =
            serde_json::from_str(newer).expect("an unknown token parses");
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
        let locale: PublishedLocale =
            serde_json::from_value(node.clone()).expect("the node parses");
        assert_eq!(locale.currency_code, "VND");
        assert_eq!(locale.currency_exponent, Some(0));
        assert_eq!(locale.cash_rounding_increment, Some(1000));
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
}
