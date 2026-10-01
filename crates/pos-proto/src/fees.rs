// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `fees` config node: the charges a bill adds to what was sold — a service charge, a
//! packaging fee per box, a delivery fee
//! ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 1).
//!
//! # A fee is data
//!
//! A bill has one fee today, a service charge that is always zero: `pos_core::billing::assemble`
//! takes a single amount, and the edge passes zero. The owner decided that every fee is
//! configurable — a rate or an amount, taxed or not, by sales channel and by item — so a fee is a
//! rule in this node, as many as a store needs. Each has an id, a stable [`FeeCode`] for reports
//! and a name with per-locale translations, as reason codes have
//! ([ADR-0115](../../../docs/adr/0115-reason-codes-are-a-managed-list.md)). A rule names the items
//! it counts by [`MenuItemId`], not by category: ADR-0159 has the cloud compile categories and tags
//! into item ids per store, as it compiles the menu
//! ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md)), so the edge needs no category model.
//!
//! This module is the wire shape, the defaults it is read with, and a check of a rule's shape. It
//! computes nothing: `pos_core::billing::assemble` charges the rules, one fee line per rule applied
//! and folded into the tax and the total (ADR-0159 decision 2). Nothing authors the node yet, and
//! the edge does not install it.
//!
//! # Defaults, and refusing to guess
//!
//! Only a rule's id, code and name are required. Any other field may be absent, and absent reads as
//! the default the owner confirmed on 2026-10-01: a percentage is taken after discounts and comps
//! and net of tax, the fee is taxed the way its lines are ([`FeeTax::FollowLines`]), it is not
//! waivable, it applies on every channel and to every item, and a published rule is active.
//!
//! A token this build does not recognise is another matter. A fee is a price, and a guess is a
//! charge nobody authored, so the accessors refuse rather than guess: a rule of an unknown kind
//! applies to nothing ([`PublishedFee::kind()`]), an unknown item scope counts no item, and an
//! unknown channel matches no channel. The one exception is the tax treatment, which reads as
//! [`FeeTax::FollowLines`]: the fee is still the amount authored, and it is taxed the way
//! [ADR-0028](../../../docs/adr/0028-settlement-and-payment-invariant.md) has a fee taxed unless
//! configured otherwise. Every unrecognised token is kept ([`Open`]), so re-serialising a rule
//! reproduces it byte for byte.
//!
//! A fee rule is pricing configuration: ids, a code, a name, a rate or an amount. Nothing here is a
//! customer or employee identifier.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::enums::SalesChannel;
use crate::ids::{FeeId, MenuItemId, TaxClassId};
use crate::money::{Money, Ratio};
use crate::text::DisplayName;
use crate::wire_enum;
use crate::wire_enum::Open;

wire_enum! {
    /// How a fee is charged.
    ///
    /// Closed vocabulary: each kind names a calculation, so a new one is a release rather than a
    /// setting. The kinds ADR-0159 defers, a per-guest cover charge and a minimum spend topped up
    /// to a floor, join here when a store needs one. An edge on an older release reads such a rule
    /// as one of an unknown kind, which applies to nothing.
    FeeKind, prefix = "FEE_KIND";
    /// A rate of the base: the [`PublishedFee::rate`] share of the lines the rule counts, such as a
    /// 5 % service charge.
    Percent = "PERCENT",
    /// A fixed [`PublishedFee::amount`] once per bill, such as a delivery fee.
    AmountPerBill = "AMOUNT_PER_BILL",
    /// A fixed [`PublishedFee::amount`] per unit of each line the rule counts, such as a packaging
    /// fee per box.
    AmountPerUnit = "AMOUNT_PER_UNIT",
}

wire_enum! {
    /// How a fee is taxed.
    FeeTax, prefix = "FEE_TAX";
    /// Not taxed.
    NotTaxable = "NOT_TAXABLE",
    /// Spread across the base's tax classes in proportion to each one's share of the base, and
    /// taxed at each class's rate, the way a bill-level discount is spread. The default the owner
    /// confirmed, and what an absent or unknown treatment reads as.
    FollowLines = "FOLLOW_LINES",
    /// Taxed at the one class [`PublishedFee::tax_class_id`] names.
    TaxClass = "TAX_CLASS",
}

wire_enum! {
    /// Which lines a fee counts.
    FeeItems, prefix = "FEE_ITEMS";
    /// Every line. What an absent scope reads as.
    All = "ALL",
    /// Only the lines of an item [`PublishedFee::menu_item_ids`] lists.
    Include = "INCLUDE",
    /// Every line except those of an item [`PublishedFee::menu_item_ids`] lists.
    Exclude = "EXCLUDE",
}

/// A short, stable, language-independent handle for a fee, such as `"SERVICE"` or `"PACKAGING"`.
///
/// What a report groups fees by and a CSV export prints, so an operator reads a handle they chose
/// rather than a ULID. Like [`crate::reason_codes::ReasonCode`], it is deliberately not in
/// [`crate::text`], which holds the text admissible in an event payload, and no event carries it
/// yet. ADR-0159 decision 4 has a settled bill's fee lines name their code; admitting the code to a
/// payload is that change's decision.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeeCode(Box<str>);

impl FeeCode {
    /// Wraps a code.
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// The code as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for FeeCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One fee rule: what it is called, what it charges, on which channels and items, and how it is
/// taxed.
///
/// No `deny_unknown_fields`, as for every published node: an edge on an older release applies a
/// rule that carries a field it does not know, rather than refusing the whole node. Read the enum
/// fields through [`kind`](Self::kind()), [`item_scope`](Self::item_scope()) and
/// [`tax`](Self::tax()), which give an absent value its default and refuse an unknown one where a
/// guess would be a price. [`violations`](Self::violations) is the shape check ADR-0159 has the
/// cloud make before it publishes a rule.
#[expect(
    clippy::struct_excessive_bools,
    reason = "a published rule whose four flags are independent and named on the wire, each with \
              its own default; it is deserialised by field name and never built from positional \
              booleans, which is the confusion this lint guards against"
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedFee {
    /// Its stable id: the same fee across a rename, and the key ADR-0159 merges a tenant's, a
    /// brand's and a store's rules by.
    pub fee_id: FeeId,
    /// The short handle a report groups by.
    pub code: FeeCode,
    /// The name the bill prints the fee under. Always present, and the fallback for any locale
    /// [`display_name_translations`](Self::display_name_translations) does not carry
    /// (`docs/pos-spec.md` §12: English is always present and is the fallback).
    pub display_name: DisplayName,
    /// The name in each locale it is translated into, keyed by locale code (`"vi"`, `"ja"`, …), as
    /// [`crate::reason_codes::PublishedReasonCode::display_name_translations`] does for a reason.
    /// Read through [`localized_name`](Self::localized_name).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub display_name_translations: BTreeMap<String, DisplayName>,
    /// How the fee is charged. Read it through [`kind`](Self::kind()): an absent or unknown kind is
    /// no kind, and a rule of no kind applies to nothing. Absent parses, so one such rule does not
    /// stop a store applying the rest of the node.
    #[serde(default)]
    pub kind: Open<FeeKind>,
    /// For [`FeeKind::Percent`]: the share of the base, as an exact ratio. 5 % is
    /// `Ratio::percent(5)` and 2.5 % is `Ratio::basis_points(250)`; never a float. Ignored for the
    /// amount kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<Ratio>,
    /// For [`FeeKind::AmountPerBill`] and [`FeeKind::AmountPerUnit`]: the charge, an integer in the
    /// currency's minor unit. Ignored for a percentage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<Money>,
    /// The sales channels the fee applies on. Empty, which is what an absent field reads as, means
    /// every channel. Each is wrapped in [`Open`] so a channel from a newer cloud round-trips; one
    /// this build does not recognise matches no channel. Read through
    /// [`applies_on`](Self::applies_on).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<Open<SalesChannel>>,
    /// Which lines the fee counts, together with [`menu_item_ids`](Self::menu_item_ids). Read
    /// through [`item_scope`](Self::item_scope()) and [`counts_item`](Self::counts_item); absent
    /// means every line.
    #[serde(default)]
    pub item_scope: Open<FeeItems>,
    /// The items a [`FeeItems::Include`] or [`FeeItems::Exclude`] scope lists, by id rather than by
    /// category (the module documentation says why). Ignored for [`FeeItems::All`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub menu_item_ids: Vec<MenuItemId>,
    /// For a percentage: whether the base is discounted, that is taken after discounts and comps
    /// (`true`), or before them (`false`). Absent means `true`, as the owner confirmed.
    #[serde(default = "discounted_by_default")]
    pub base_discounted: bool,
    /// For a percentage: whether the base is tax-inclusive (`true`), for a market that quotes a
    /// charge on the tax-inclusive price, or net of tax (`false`). Absent means `false`, as the
    /// owner confirmed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub base_tax_inclusive: bool,
    /// How the fee is taxed. Read it through [`tax`](Self::tax()), which reads an absent or unknown
    /// treatment as [`FeeTax::FollowLines`].
    #[serde(default)]
    pub tax: Open<FeeTax>,
    /// For [`FeeTax::TaxClass`]: the class the fee is taxed at. Ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tax_class_id: Option<TaxClassId>,
    /// Whether staff may take the fee off one bill. Absent means `false`, as the owner confirmed.
    /// Waiving is an act on a bill, under its own permission and with a reason
    /// (ADR-0159 decision 5), never an edit of this rule.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub waivable: bool,
    /// Whether the rule applies at all. Absent means `true`: a rule the cloud publishes applies
    /// unless it is paused.
    #[serde(default = "active_by_default")]
    pub active: bool,
}

/// The default for [`PublishedFee::base_discounted`] when a document omits it: a percentage is
/// taken after discounts and comps.
const fn discounted_by_default() -> bool {
    true
}

/// The default for [`PublishedFee::active`] when a document omits it: a published rule applies.
const fn active_by_default() -> bool {
    true
}

impl PublishedFee {
    /// The name to print in `language`, falling back to [`display_name`](Self::display_name). Total
    /// and never blank.
    #[must_use]
    pub fn localized_name(&self, language: &str) -> &DisplayName {
        self.display_name_translations
            .get(language)
            .unwrap_or(&self.display_name)
    }

    /// How the fee is charged, or `None` when the kind is absent, `FEE_KIND_UNSPECIFIED` or a token
    /// this build does not know.
    ///
    /// `None` means the rule applies to nothing. Charging an unknown kind as though it were a known
    /// one would be a price nobody authored, so this never guesses.
    #[must_use]
    pub fn kind(&self) -> Option<FeeKind> {
        self.kind.require().ok()
    }

    /// Which lines the fee counts, or `None` for a scope token this build does not know.
    ///
    /// An absent scope and `FEE_ITEMS_UNSPECIFIED` read as [`FeeItems::All`]: a rule that names
    /// no scope counts every line. A token from a newer release is `None`, and
    /// [`counts_item`](Self::counts_item) then counts nothing, because this build cannot tell which
    /// lines that scope means and counting every one would be a guess.
    /// [`violations`](Self::violations) reports it as [`FeeViolation::UnknownItemScope`].
    #[must_use]
    pub fn item_scope(&self) -> Option<FeeItems> {
        if self.item_scope.is_unrecognised() {
            return None;
        }
        match self.item_scope.known() {
            FeeItems::Unspecified | FeeItems::All => Some(FeeItems::All),
            FeeItems::Include => Some(FeeItems::Include),
            FeeItems::Exclude => Some(FeeItems::Exclude),
        }
    }

    /// How the fee is taxed.
    ///
    /// An absent treatment, `FEE_TAX_UNSPECIFIED` and a token this build does not know all read as
    /// [`FeeTax::FollowLines`], the default the owner confirmed. Unlike an unknown kind this is not
    /// refused: the fee is still the amount authored, and following its lines taxes it the way
    /// ADR-0028 has a fee taxed unless configured otherwise.
    #[must_use]
    pub fn tax(&self) -> FeeTax {
        match self.tax.known() {
            FeeTax::Unspecified | FeeTax::FollowLines => FeeTax::FollowLines,
            FeeTax::NotTaxable => FeeTax::NotTaxable,
            FeeTax::TaxClass => FeeTax::TaxClass,
        }
    }

    /// Whether the fee applies on `channel`.
    ///
    /// An empty [`channels`](Self::channels) list means every channel; otherwise the channel must
    /// be listed. A listed token this build does not recognise reads as `SALES_CHANNEL_UNSPECIFIED`
    /// and matches no channel. [`SalesChannel::Unspecified`] is not a channel a sale is made on, so
    /// no rule applies on it, not even one that lists no channels. This says nothing about
    /// [`active`](Self::active), which [`PublishedFees::active_fees`] filters on.
    #[must_use]
    pub fn applies_on(&self, channel: SalesChannel) -> bool {
        channel != SalesChannel::Unspecified
            && (self.channels.is_empty()
                || self.channels.iter().any(|listed| listed.known() == channel))
    }

    /// Whether a line of `menu_item_id` counts toward the fee, following
    /// [`item_scope`](Self::item_scope()).
    ///
    /// Every line under [`FeeItems::All`], only a listed item's under [`FeeItems::Include`], every
    /// item's but a listed one's under [`FeeItems::Exclude`], and none under a scope this build
    /// does not know.
    #[must_use]
    pub fn counts_item(&self, menu_item_id: MenuItemId) -> bool {
        match self.item_scope() {
            Some(FeeItems::All) => true,
            Some(FeeItems::Include) => self.menu_item_ids.contains(&menu_item_id),
            Some(FeeItems::Exclude) => !self.menu_item_ids.contains(&menu_item_id),
            // `item_scope` reads `Unspecified` as `All`, so only an unknown token arrives here.
            Some(FeeItems::Unspecified) | None => false,
        }
    }

    /// What is wrong with the rule's shape, in a fixed order; empty when nothing is.
    ///
    /// Only what the rule alone decides: a percentage needs a rate from 0 % to 100 %, an amount
    /// kind needs an amount that is not negative, [`FeeTax::TaxClass`] needs a
    /// [`tax_class_id`](Self::tax_class_id), an include or exclude scope needs at least one item,
    /// and an unknown kind or an unknown item scope is refused: a rule of an unknown kind applies
    /// to nothing, and one with an unknown scope counts no line. An unknown tax treatment is not
    /// refused, because it reads as [`FeeTax::FollowLines`] ([`tax`](Self::tax())).
    ///
    /// This is the check ADR-0159 has the cloud make before it publishes a rule. Whether the listed
    /// items are on the store's menu needs the menu, and is not checked here.
    #[must_use]
    pub fn violations(&self) -> Vec<FeeViolation> {
        let charge = match self.kind.known() {
            FeeKind::Unspecified => Some(FeeViolation::UnknownKind),
            FeeKind::Percent => match self.rate {
                None => Some(FeeViolation::MissingRate),
                Some(rate) => {
                    (!is_zero_to_one_hundred_percent(rate)).then_some(FeeViolation::RateOutOfRange)
                }
            },
            FeeKind::AmountPerBill | FeeKind::AmountPerUnit => match self.amount {
                None => Some(FeeViolation::MissingAmount),
                Some(amount) => amount.is_negative().then_some(FeeViolation::NegativeAmount),
            },
        };
        let tax = (self.tax() == FeeTax::TaxClass && self.tax_class_id.is_none())
            .then_some(FeeViolation::MissingTaxClass);
        let items = match self.item_scope() {
            None => Some(FeeViolation::UnknownItemScope),
            Some(FeeItems::Include | FeeItems::Exclude) => self
                .menu_item_ids
                .is_empty()
                .then_some(FeeViolation::EmptyItemList),
            // `item_scope` reads `Unspecified` as `All`, which needs no list.
            Some(FeeItems::All | FeeItems::Unspecified) => None,
        };
        [charge, tax, items].into_iter().flatten().collect()
    }
}

/// Whether `rate` is from 0 to 1 inclusive, that is from 0 % to 100 %, compared in integers.
///
/// A [`Ratio`]'s denominator may be negative, so the bounds follow its sign: `n / d` is within
/// `0..=1` exactly when `0 <= n <= d` for a positive `d`, and when `d <= n <= 0` for a negative
/// one.
fn is_zero_to_one_hundred_percent(rate: Ratio) -> bool {
    let numerator = rate.numerator();
    let denominator = rate.denominator().get();
    if denominator > 0 {
        (0..=denominator).contains(&numerator)
    } else {
        (denominator..=0).contains(&numerator)
    }
}

/// What is wrong with a fee rule's shape, as [`PublishedFee::violations`] finds it.
///
/// Each message names the field to correct.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FeeViolation {
    /// The kind is absent, `FEE_KIND_UNSPECIFIED`, or a token this build does not know, so the rule
    /// would apply to nothing.
    #[error("kind is absent or not one this release knows")]
    UnknownKind,
    /// A [`FeeKind::Percent`] rule with no `rate`.
    #[error("a FEE_KIND_PERCENT fee needs a rate")]
    MissingRate,
    /// A `rate` below 0 % or above 100 %.
    #[error("rate must be from 0 to 100 percent")]
    RateOutOfRange,
    /// A [`FeeKind::AmountPerBill`] or [`FeeKind::AmountPerUnit`] rule with no `amount`.
    #[error("a FEE_KIND_AMOUNT_PER_BILL or FEE_KIND_AMOUNT_PER_UNIT fee needs an amount")]
    MissingAmount,
    /// A negative `amount`: a fee adds to a bill, and a reduction is a discount or a campaign.
    #[error("amount must not be negative")]
    NegativeAmount,
    /// [`FeeTax::TaxClass`] with no `tax_class_id`.
    #[error("a FEE_TAX_TAX_CLASS fee needs a tax_class_id")]
    MissingTaxClass,
    /// [`FeeItems::Include`] or [`FeeItems::Exclude`] with no `menu_item_ids`.
    #[error("a FEE_ITEMS_INCLUDE or FEE_ITEMS_EXCLUDE fee needs an item in menu_item_ids")]
    EmptyItemList,
    /// An `item_scope` token this build does not know, so the rule would count no line.
    #[error("item_scope is not one this release knows")]
    UnknownItemScope,
}

/// The `fees` config node: every fee rule a store's bills may apply.
///
/// A list rather than a map for the same round-trips-in-a-diff reason
/// [`crate::campaign::PublishedCampaigns`] and [`crate::reason_codes::PublishedReasonCodes`] are
/// lists. Empty is the safe default and what a store with no node has: no rule, so no fee, which is
/// what a bill charged before the node existed.
///
/// No `deny_unknown_fields`, for the same reason as [`PublishedFee`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedFees {
    /// Every rule, in the order the node lists them, including paused ones and ones of a kind this
    /// build does not know. [`active_fees`](Self::active_fees) gives the ones that may apply.
    #[serde(default)]
    pub fees: Vec<PublishedFee>,
}

impl PublishedFees {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "fees";

    /// The rules that may apply to a bill: active, and of a kind this build knows.
    ///
    /// A paused rule applies to nothing, and so does a rule of an unknown kind
    /// ([`PublishedFee::kind()`]), so neither is offered. Which channels and lines each one covers
    /// is still the rule's to say, through [`PublishedFee::applies_on`] and
    /// [`PublishedFee::counts_item`].
    pub fn active_fees(&self) -> impl Iterator<Item = &PublishedFee> {
        self.fees
            .iter()
            .filter(|fee| fee.active && fee.kind().is_some())
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroI64;
    use std::collections::BTreeMap;

    use super::{FeeCode, FeeItems, FeeKind, FeeTax, FeeViolation, PublishedFee, PublishedFees};
    use crate::enums::SalesChannel;
    use crate::ids::{FeeId, MenuItemId, TaxClassId};
    use crate::money::{CurrencyCode, Money, Ratio};
    use crate::text::DisplayName;
    use crate::ulid::Ulid;
    use crate::wire_enum::{Open, WireEnum};

    /// Every channel a sale is made on: all of them but `UNSPECIFIED`, including any added later.
    fn known_channels() -> impl Iterator<Item = SalesChannel> {
        SalesChannel::ALL
            .iter()
            .copied()
            .filter(|channel| *channel != SalesChannel::Unspecified)
    }

    fn item(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }

    fn vnd(amount: i64) -> Money {
        Money::new(CurrencyCode::VND, amount)
    }

    fn ratio(numerator: i64, denominator: i64) -> Ratio {
        Ratio::new(
            numerator,
            NonZeroI64::new(denominator).expect("a test ratio has a non-zero denominator"),
        )
    }

    /// A rule as the cloud would publish it: an id, a code and a name, plus `fields`.
    ///
    /// Parsed from text, as the edge parses a node: a currency code borrows from its input, so a
    /// `Money` does not deserialise from a `serde_json::Value`.
    fn parse(fields: &serde_json::Value) -> PublishedFee {
        let mut document = serde_json::json!({
            "fee_id": FeeId::new(Ulid::from_u128(1)).to_string(),
            "code": "SERVICE",
            "display_name": "Service charge",
        });
        if let (Some(rule), Some(extra)) = (document.as_object_mut(), fields.as_object()) {
            for (key, value) in extra {
                rule.insert(key.clone(), value.clone());
            }
        }
        serde_json::from_str(&document.to_string()).expect("the rule parses")
    }

    /// A 5 % service charge with every field set to something other than its default.
    fn service_charge() -> PublishedFee {
        PublishedFee {
            fee_id: FeeId::new(Ulid::from_u128(1)),
            code: FeeCode::new("SERVICE"),
            display_name: DisplayName::new("Service charge"),
            display_name_translations: BTreeMap::from([(
                "vi".to_owned(),
                DisplayName::new("Phí phục vụ"),
            )]),
            kind: Open::from_known(FeeKind::Percent),
            rate: Some(Ratio::percent(5).expect("a rate")),
            amount: None,
            channels: vec![Open::from_known(SalesChannel::DineIn)],
            item_scope: Open::from_known(FeeItems::Exclude),
            menu_item_ids: vec![item(7)],
            base_discounted: false,
            base_tax_inclusive: true,
            tax: Open::from_known(FeeTax::TaxClass),
            tax_class_id: Some(TaxClassId::new(Ulid::from_u128(3))),
            waivable: true,
            active: false,
        }
    }

    /// A packaging fee per box on takeaway and delivery, for two items, not taxed.
    fn packaging() -> PublishedFee {
        PublishedFee {
            fee_id: FeeId::new(Ulid::from_u128(2)),
            code: FeeCode::new("PACKAGING"),
            display_name: DisplayName::new("Packaging"),
            display_name_translations: BTreeMap::new(),
            kind: Open::from_known(FeeKind::AmountPerUnit),
            rate: None,
            amount: Some(vnd(3_000)),
            channels: vec![
                Open::from_known(SalesChannel::Takeaway),
                Open::from_known(SalesChannel::Delivery),
            ],
            item_scope: Open::from_known(FeeItems::Include),
            menu_item_ids: vec![item(7), item(8)],
            base_discounted: true,
            base_tax_inclusive: false,
            tax: Open::from_known(FeeTax::NotTaxable),
            tax_class_id: None,
            waivable: false,
            active: true,
        }
    }

    #[test]
    fn the_node_round_trips_through_json() {
        let node = PublishedFees {
            fees: vec![service_charge(), packaging()],
        };
        let json = serde_json::to_string(&node).expect("serialise");
        let back: PublishedFees = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, node);
        assert_eq!(PublishedFees::NODE, "fees");
    }

    #[test]
    fn the_tokens_are_the_published_vocabulary() {
        // The tokens are the contract a stored rule and an older edge rely on, so every one is
        // pinned here.
        fn tokens<E: WireEnum>() -> Vec<&'static str> {
            E::ALL.iter().copied().map(E::as_wire).collect()
        }
        assert_eq!(
            tokens::<FeeKind>(),
            [
                "FEE_KIND_UNSPECIFIED",
                "FEE_KIND_PERCENT",
                "FEE_KIND_AMOUNT_PER_BILL",
                "FEE_KIND_AMOUNT_PER_UNIT",
            ]
        );
        assert_eq!(
            tokens::<FeeTax>(),
            [
                "FEE_TAX_UNSPECIFIED",
                "FEE_TAX_NOT_TAXABLE",
                "FEE_TAX_FOLLOW_LINES",
                "FEE_TAX_TAX_CLASS",
            ]
        );
        assert_eq!(
            tokens::<FeeItems>(),
            [
                "FEE_ITEMS_UNSPECIFIED",
                "FEE_ITEMS_ALL",
                "FEE_ITEMS_INCLUDE",
                "FEE_ITEMS_EXCLUDE",
            ]
        );
        for kind in FeeKind::ALL {
            assert_eq!(FeeKind::from_wire(kind.as_wire()), Some(*kind));
        }
        for treatment in FeeTax::ALL {
            assert_eq!(FeeTax::from_wire(treatment.as_wire()), Some(*treatment));
        }
        for scope in FeeItems::ALL {
            assert_eq!(FeeItems::from_wire(scope.as_wire()), Some(*scope));
        }
    }

    #[test]
    fn an_empty_node_has_no_fees() {
        // A store with no `fees` node, or one published empty, charges no fee: what every bill did
        // before the node existed.
        let empty: PublishedFees = serde_json::from_str("{}").expect("an empty node parses");
        assert!(empty.fees.is_empty());
        assert_eq!(empty, PublishedFees::default());
        assert_eq!(empty.active_fees().count(), 0);

        let published_empty: PublishedFees =
            serde_json::from_str(r#"{ "fees": [] }"#).expect("an empty list parses");
        assert_eq!(published_empty, empty);
    }

    #[test]
    fn an_absent_field_reads_as_the_confirmed_default() {
        let fee = parse(&serde_json::json!({
            "kind": "FEE_KIND_PERCENT",
            "rate": { "numerator": 5, "denominator": 100 },
        }));

        // The defaults the owner confirmed on 2026-10-01.
        assert!(fee.base_discounted, "after discounts by default");
        assert!(!fee.base_tax_inclusive, "net of tax by default");
        assert_eq!(fee.tax(), FeeTax::FollowLines);
        assert!(!fee.waivable, "not waivable by default");
        assert!(fee.active, "a published rule is active");
        // Every channel and every item.
        assert_eq!(fee.item_scope(), Some(FeeItems::All));
        assert!(fee.counts_item(item(42)));
        for channel in known_channels() {
            assert!(fee.applies_on(channel), "applies on {channel}");
        }
        assert!(fee.display_name_translations.is_empty());
        assert_eq!(fee.tax_class_id, None);
        assert_eq!(fee.amount, None);
        assert_eq!(fee.kind(), Some(FeeKind::Percent));
        assert!(fee.violations().is_empty(), "{:?}", fee.violations());

        // An explicit `*_UNSPECIFIED` reads as an absent field does.
        let explicit = parse(&serde_json::json!({
            "kind": "FEE_KIND_PERCENT",
            "rate": { "numerator": 5, "denominator": 100 },
            "item_scope": "FEE_ITEMS_UNSPECIFIED",
            "tax": "FEE_TAX_UNSPECIFIED",
        }));
        assert_eq!(explicit, fee);

        // And an empty or `false` default is left off the wire, where absence already says it.
        let value = serde_json::to_value(&fee).expect("serialise");
        let mut keys: Vec<&str> = value
            .as_object()
            .map(|rule| rule.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "active",
                "base_discounted",
                "code",
                "display_name",
                "fee_id",
                "item_scope",
                "kind",
                "rate",
                "tax",
            ],
            "{value}"
        );
    }

    #[test]
    fn a_field_this_release_does_not_know_is_ignored() {
        // A newer cloud adds a condition; an older store still applies the rule and the node.
        let fee = parse(&serde_json::json!({
            "kind": "FEE_KIND_AMOUNT_PER_BILL",
            "amount": { "currency_code": "VND", "amount_minor": 15_000 },
            "minimum_party_size": 6,
        }));
        assert_eq!(fee.kind(), Some(FeeKind::AmountPerBill));
        assert_eq!(fee.amount, Some(vnd(15_000)));

        let document = serde_json::json!({
            "fees": [serde_json::to_value(&fee).expect("serialise")],
            "cover_charges": [],
        });
        let node: PublishedFees = serde_json::from_str(&document.to_string())
            .expect("an unknown field on the node is ignored too");
        assert_eq!(node.fees, vec![fee]);
    }

    #[test]
    fn an_unknown_token_degrades_and_the_accessors_refuse_to_guess() {
        let fee = parse(&serde_json::json!({
            "kind": "FEE_KIND_TOP_UP_TO",
            "amount": { "currency_code": "VND", "amount_minor": 500_000 },
            "channels": ["SALES_CHANNEL_KIOSK"],
            "item_scope": "FEE_ITEMS_CATEGORY",
            "tax": "FEE_TAX_REDUCED",
        }));

        // An unknown kind is no kind: the rule applies to nothing, and no node offers it.
        assert!(fee.kind.is_unrecognised());
        assert_eq!(fee.kind(), None);
        let node = PublishedFees {
            fees: vec![fee.clone()],
        };
        assert_eq!(node.active_fees().count(), 0);
        // An unknown scope counts no item, an unknown channel matches no channel…
        assert_eq!(fee.item_scope(), None);
        assert!(!fee.counts_item(item(7)));
        for channel in known_channels() {
            assert!(
                !fee.applies_on(channel),
                "an unknown token matched {channel}"
            );
        }
        // …and an unknown tax treatment is the one that reads as its default.
        assert_eq!(fee.tax(), FeeTax::FollowLines);
        // So the shape check refuses the unknown kind and the unknown scope, and not the tax.
        assert_eq!(
            fee.violations(),
            [FeeViolation::UnknownKind, FeeViolation::UnknownItemScope]
        );

        // Every token survives a round trip, so the store re-serialises what it cannot read.
        let json = serde_json::to_string(&fee).expect("serialise");
        for token in [
            "FEE_KIND_TOP_UP_TO",
            "SALES_CHANNEL_KIOSK",
            "FEE_ITEMS_CATEGORY",
            "FEE_TAX_REDUCED",
        ] {
            assert!(json.contains(token), "{token} was lost: {json}");
        }
        let back: PublishedFee = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, fee);
    }

    #[test]
    fn an_empty_channel_list_means_every_channel() {
        let every = parse(&serde_json::json!({ "kind": "FEE_KIND_AMOUNT_PER_BILL" }));
        for channel in known_channels() {
            assert!(
                every.applies_on(channel),
                "an empty list leaves out {channel}"
            );
        }
        // `UNSPECIFIED` is not a channel a sale is made on.
        assert!(!every.applies_on(SalesChannel::Unspecified));

        let listed = parse(&serde_json::json!({
            "kind": "FEE_KIND_AMOUNT_PER_BILL",
            "channels": ["SALES_CHANNEL_TAKEAWAY", "SALES_CHANNEL_DELIVERY", "SALES_CHANNEL_KIOSK"],
        }));
        assert!(listed.applies_on(SalesChannel::Takeaway));
        assert!(listed.applies_on(SalesChannel::Delivery));
        assert!(!listed.applies_on(SalesChannel::DineIn));
        assert!(!listed.applies_on(SalesChannel::Qr));
        assert!(!listed.applies_on(SalesChannel::Api));
        assert!(!listed.applies_on(SalesChannel::Unspecified));
    }

    #[test]
    fn an_include_or_exclude_list_decides_which_lines_count() {
        let include = parse(&serde_json::json!({
            "kind": "FEE_KIND_AMOUNT_PER_UNIT",
            "item_scope": "FEE_ITEMS_INCLUDE",
            "menu_item_ids": [item(7).to_string(), item(8).to_string()],
        }));
        assert!(include.counts_item(item(7)));
        assert!(include.counts_item(item(8)));
        assert!(!include.counts_item(item(9)));

        let exclude = parse(&serde_json::json!({
            "kind": "FEE_KIND_PERCENT",
            "item_scope": "FEE_ITEMS_EXCLUDE",
            "menu_item_ids": [item(7).to_string()],
        }));
        assert!(!exclude.counts_item(item(7)));
        assert!(exclude.counts_item(item(8)));
        assert!(exclude.counts_item(item(9)));

        // Under ALL a list is ignored rather than read as an include list.
        let all = parse(&serde_json::json!({
            "kind": "FEE_KIND_PERCENT",
            "item_scope": "FEE_ITEMS_ALL",
            "menu_item_ids": [item(7).to_string()],
        }));
        assert!(all.counts_item(item(7)));
        assert!(all.counts_item(item(9)));
    }

    #[test]
    fn a_percentage_needs_a_rate_from_zero_to_one_hundred_percent() {
        let with_rate = |rate: Option<Ratio>| PublishedFee {
            rate,
            ..parse(&serde_json::json!({ "kind": "FEE_KIND_PERCENT" }))
        };

        assert_eq!(with_rate(None).violations(), [FeeViolation::MissingRate]);
        for valid in [
            ratio(0, 100),
            ratio(5, 100),
            ratio(100, 100),
            ratio(825, 10_000),
            ratio(i64::MAX, i64::MAX),
            // A negative denominator flips the bounds: −1 / −2 is 50 %, and 0 / −5 is 0 %.
            ratio(-1, -2),
            ratio(0, -5),
            ratio(-5, -5),
        ] {
            assert!(
                with_rate(Some(valid)).violations().is_empty(),
                "{valid:?} is from 0 to 100 %"
            );
        }
        for invalid in [
            ratio(101, 100),
            ratio(-1, 100),
            ratio(1, -2),
            ratio(-3, -2),
            ratio(i64::MIN, -1),
            ratio(i64::MIN, i64::MAX),
        ] {
            assert_eq!(
                with_rate(Some(invalid)).violations(),
                [FeeViolation::RateOutOfRange],
                "{invalid:?} is outside 0 to 100 %"
            );
        }
    }

    #[test]
    fn an_amount_kind_needs_an_amount_that_is_not_negative() {
        for kind in ["FEE_KIND_AMOUNT_PER_BILL", "FEE_KIND_AMOUNT_PER_UNIT"] {
            let with_amount = |amount: Option<Money>| PublishedFee {
                amount,
                ..parse(&serde_json::json!({ "kind": kind }))
            };
            assert_eq!(
                with_amount(None).violations(),
                [FeeViolation::MissingAmount],
                "{kind}"
            );
            assert_eq!(
                with_amount(Some(vnd(-1))).violations(),
                [FeeViolation::NegativeAmount],
                "{kind}"
            );
            assert!(with_amount(Some(vnd(0))).violations().is_empty(), "{kind}");
            assert!(
                with_amount(Some(vnd(15_000))).violations().is_empty(),
                "{kind}"
            );
        }
        // A rate on an amount kind is ignored, not a substitute for the amount.
        let rate_instead = parse(&serde_json::json!({
            "kind": "FEE_KIND_AMOUNT_PER_BILL",
            "rate": { "numerator": 5, "denominator": 100 },
        }));
        assert_eq!(rate_instead.violations(), [FeeViolation::MissingAmount]);
    }

    #[test]
    fn a_fee_taxed_at_one_class_needs_the_class() {
        let missing = parse(&serde_json::json!({
            "kind": "FEE_KIND_AMOUNT_PER_BILL",
            "amount": { "currency_code": "VND", "amount_minor": 15_000 },
            "tax": "FEE_TAX_TAX_CLASS",
        }));
        assert_eq!(missing.tax(), FeeTax::TaxClass);
        assert_eq!(missing.violations(), [FeeViolation::MissingTaxClass]);

        let named = PublishedFee {
            tax_class_id: Some(TaxClassId::new(Ulid::from_u128(3))),
            ..missing
        };
        assert!(named.violations().is_empty());
    }

    #[test]
    fn an_include_or_exclude_scope_needs_an_item() {
        for scope in ["FEE_ITEMS_INCLUDE", "FEE_ITEMS_EXCLUDE"] {
            let empty = parse(&serde_json::json!({
                "kind": "FEE_KIND_AMOUNT_PER_UNIT",
                "amount": { "currency_code": "VND", "amount_minor": 3_000 },
                "item_scope": scope,
            }));
            assert_eq!(empty.violations(), [FeeViolation::EmptyItemList], "{scope}");

            let listed = PublishedFee {
                menu_item_ids: vec![item(7)],
                ..empty
            };
            assert!(listed.violations().is_empty(), "{scope}");
        }
        // ALL needs no list.
        let all = parse(&serde_json::json!({
            "kind": "FEE_KIND_AMOUNT_PER_UNIT",
            "amount": { "currency_code": "VND", "amount_minor": 3_000 },
            "item_scope": "FEE_ITEMS_ALL",
        }));
        assert!(all.violations().is_empty());
    }

    #[test]
    fn an_unknown_item_scope_is_a_violation_and_an_unknown_tax_treatment_is_not() {
        // A scope this release does not know counts no line, whatever the list says, so the rule
        // is refused rather than published to count nothing.
        for items in [
            serde_json::json!([]),
            serde_json::json!([item(7).to_string()]),
        ] {
            let fee = parse(&serde_json::json!({
                "kind": "FEE_KIND_AMOUNT_PER_UNIT",
                "amount": { "currency_code": "VND", "amount_minor": 3_000 },
                "item_scope": "FEE_ITEMS_CATEGORY",
                "menu_item_ids": items,
            }));
            assert_eq!(
                fee.violations(),
                [FeeViolation::UnknownItemScope],
                "{items}"
            );
        }

        // An unknown tax treatment reads as FOLLOW_LINES, so the rule still charges as authored.
        let fee = parse(&serde_json::json!({
            "kind": "FEE_KIND_AMOUNT_PER_UNIT",
            "amount": { "currency_code": "VND", "amount_minor": 3_000 },
            "tax": "FEE_TAX_REDUCED",
        }));
        assert_eq!(fee.tax(), FeeTax::FollowLines);
        assert!(fee.violations().is_empty(), "{:?}", fee.violations());
    }

    #[test]
    fn an_unknown_kind_is_a_violation() {
        for kind in [
            serde_json::json!({}),
            serde_json::json!({ "kind": "FEE_KIND_UNSPECIFIED" }),
            serde_json::json!({ "kind": "FEE_KIND_TOP_UP_TO" }),
        ] {
            assert_eq!(
                parse(&kind).violations(),
                [FeeViolation::UnknownKind],
                "{kind}"
            );
        }

        // A rule wrong in several ways reports each, in a fixed order.
        let fee = parse(&serde_json::json!({
            "kind": "FEE_KIND_TOP_UP_TO",
            "tax": "FEE_TAX_TAX_CLASS",
            "item_scope": "FEE_ITEMS_INCLUDE",
        }));
        assert_eq!(
            fee.violations(),
            [
                FeeViolation::UnknownKind,
                FeeViolation::MissingTaxClass,
                FeeViolation::EmptyItemList,
            ]
        );
    }

    #[test]
    fn each_violation_names_the_field_to_correct() {
        for (violation, field) in [
            (FeeViolation::UnknownKind, "kind"),
            (FeeViolation::MissingRate, "rate"),
            (FeeViolation::RateOutOfRange, "rate"),
            (FeeViolation::MissingAmount, "amount"),
            (FeeViolation::NegativeAmount, "amount"),
            (FeeViolation::MissingTaxClass, "tax_class_id"),
            (FeeViolation::EmptyItemList, "menu_item_ids"),
            (FeeViolation::UnknownItemScope, "item_scope"),
        ] {
            assert!(
                violation.to_string().contains(field),
                "{violation:?} does not name `{field}`"
            );
        }
    }

    #[test]
    fn only_active_rules_of_a_known_kind_are_offered() {
        let paused = PublishedFee {
            active: false,
            ..packaging()
        };
        let unknown = parse(&serde_json::json!({ "kind": "FEE_KIND_TOP_UP_TO" }));
        let node = PublishedFees {
            fees: vec![paused, packaging(), unknown],
        };
        let offered: Vec<&PublishedFee> = node.active_fees().collect();
        assert_eq!(offered, [&packaging()]);
        assert_eq!(node.fees.len(), 3, "the node keeps every rule");
    }

    #[test]
    fn a_name_falls_back_to_the_display_name() {
        let fee = service_charge();
        assert_eq!(fee.localized_name("vi").as_str(), "Phí phục vụ");
        assert_eq!(fee.localized_name("ja").as_str(), "Service charge");

        let untranslated = packaging();
        assert_eq!(untranslated.localized_name("vi").as_str(), "Packaging");
        let json = serde_json::to_string(&untranslated).expect("serialise");
        assert!(
            !json.contains("display_name_translations"),
            "a rule with no translations does not carry the field: {json}"
        );
    }
}
