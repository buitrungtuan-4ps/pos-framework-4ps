// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `billing` config node: how long after it was paid a bill may still be voided at
//! the till ([ADR-0184](../../../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)
//! decision 7, [ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)).
//!
//! A void after payment stays on the sale's own business date, so no rollup has to restate a
//! closed day. The default, [`SettledVoidWindow::BusinessDate`], is that and nothing narrower; a
//! store may narrow it to the shift that took the money. A document with no `billing` node, or a
//! node without the field, runs the default.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::wire_enum;
use crate::wire_enum::{Open, WireEnum, is_absent};

wire_enum! {
    /// How long after it was paid a bill may still be voided at the till.
    SettledVoidWindow, prefix = "SETTLED_VOID_WINDOW";
    /// While the store is still trading on the bill's own business date.
    BusinessDate = "BUSINESS_DATE",
    /// Only while the shift that took the money is open.
    Shift = "SHIFT",
}

wire_enum! {
    /// Who must agree before a parked card is confirmed as paid by hand
    /// ([ADR-0243](../../../docs/adr/0243-a-hand-confirmed-card-needs-a-manager.md)).
    CardConfirmApproval, prefix = "CARD_CONFIRM_APPROVAL";
    /// A manager, as a void needs. The default.
    Manager = "MANAGER",
    /// Nobody but the person confirming.
    None = "NONE",
}

/// The `billing` node.
///
/// No `deny_unknown_fields`, as for every published node: an edge on an older release applies a
/// node that carries a field it does not know, rather than refusing the whole document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct PublishedBilling {
    /// How long after it was paid a bill may still be voided. Read it through
    /// [`PublishedBilling::settled_void_window`], which gives an absent or unknown value its
    /// default. A token this release does not know is kept and written back as it came; a value
    /// that is not a token at all reads as absent. Left off the wire while absent.
    #[serde(default, deserialize_with = "token", skip_serializing_if = "is_absent")]
    pub settled_void_window: Open<SettledVoidWindow>,
    /// Whether a bill is paid only once a check showing what it now comes to has been printed
    /// ([ADR-0218](../../../docs/adr/0218-a-printed-check-is-an-event-every-till-sees.md)). Read it
    /// through [`PublishedBilling::check_before_payment`]: absent, or not `true` or `false`, is off.
    #[serde(
        default,
        deserialize_with = "flag",
        skip_serializing_if = "Option::is_none"
    )]
    pub check_before_payment: Option<bool>,
    /// Whether the till offers taking a bill in parts, one payment at a time
    /// ([ADR-0220](../../../docs/adr/0220-a-bill-takes-its-payments-one-at-a-time.md)). Read it
    /// through [`PublishedBilling::part_payments`]: absent, or not `true` or `false`, is on.
    #[serde(
        default,
        deserialize_with = "flag",
        skip_serializing_if = "Option::is_none"
    )]
    pub part_payments: Option<bool>,
    /// The most a line's new unit price may be on a price override, as a multiple of the price it
    /// was added at
    /// ([ADR-0219](../../../docs/adr/0219-a-comp-and-a-price-override-each-need-a-reason-and-an-approver.md)).
    /// Read it through [`PublishedBilling::price_override_max_multiple`]: absent, or not a whole
    /// number within [`PRICE_OVERRIDE_MAX_MULTIPLE`], is [`DEFAULT_PRICE_OVERRIDE_MAX_MULTIPLE`].
    #[serde(
        default,
        deserialize_with = "whole",
        skip_serializing_if = "Option::is_none"
    )]
    pub price_override_max_multiple: Option<i64>,
    /// Who must agree before a parked card is confirmed as paid by hand (ADR-0243). Read it
    /// through [`PublishedBilling::card_confirm_approval`]: anything but `NONE` is a manager.
    #[serde(default, deserialize_with = "token", skip_serializing_if = "is_absent")]
    pub card_confirm_approval: Open<CardConfirmApproval>,
}

/// The multiples a store may publish as its price override's ceiling (ADR-0219).
pub const PRICE_OVERRIDE_MAX_MULTIPLE: RangeInclusive<i64> = 1..=100;

/// The ceiling until a store publishes its own: a new unit price at most a hundred times the price
/// the line was added at (ADR-0219, accepted by the owner on 2026-11-18).
pub const DEFAULT_PRICE_OVERRIDE_MAX_MULTIPLE: i64 = 100;

impl PublishedBilling {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "billing";

    /// How long after it was paid a bill may still be voided.
    ///
    /// An absent value, `SETTLED_VOID_WINDOW_UNSPECIFIED`, and a value this release does not know
    /// all read as [`SettledVoidWindow::BusinessDate`], the default.
    #[must_use]
    pub fn settled_void_window(&self) -> SettledVoidWindow {
        match self.settled_void_window.known() {
            SettledVoidWindow::Unspecified | SettledVoidWindow::BusinessDate => {
                SettledVoidWindow::BusinessDate
            }
            SettledVoidWindow::Shift => SettledVoidWindow::Shift,
        }
    }

    /// Whether a bill is paid only once a fresh check has been printed for it (ADR-0218): off
    /// unless the store turns it on, which is what every store did before the setting existed.
    #[must_use]
    pub fn check_before_payment(&self) -> bool {
        self.check_before_payment.unwrap_or(false)
    }

    /// Who must agree before a parked card is confirmed by hand: a manager unless the store says
    /// [`CardConfirmApproval::None`] (ADR-0243), the safe default for money.
    #[must_use]
    pub fn card_confirm_approval(&self) -> CardConfirmApproval {
        match self.card_confirm_approval.known() {
            CardConfirmApproval::None => CardConfirmApproval::None,
            CardConfirmApproval::Unspecified | CardConfirmApproval::Manager => {
                CardConfirmApproval::Manager
            }
        }
    }

    /// Whether a bill may be paid in parts, one payment at a time (ADR-0220): on unless the store
    /// turns it off, as a counter that is paid at once may.
    #[must_use]
    pub fn part_payments(&self) -> bool {
        self.part_payments.unwrap_or(true)
    }

    /// The most a line's new unit price may be, as a multiple of the price it was added at
    /// (ADR-0219): the node's number within [`PRICE_OVERRIDE_MAX_MULTIPLE`], and
    /// [`DEFAULT_PRICE_OVERRIDE_MAX_MULTIPLE`] otherwise.
    #[must_use]
    pub fn price_override_max_multiple(&self) -> i64 {
        self.price_override_max_multiple
            .filter(|multiple| PRICE_OVERRIDE_MAX_MULTIPLE.contains(multiple))
            .unwrap_or(DEFAULT_PRICE_OVERRIDE_MAX_MULTIPLE)
    }
}

/// A whole number as the node carries it, or nothing for a value that is not one, so the field
/// reads as its default rather than refusing the node.
fn whole<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?.as_i64())
}

/// An on-or-off value as the node carries it, or nothing for a value that is not one, so the field
/// reads as its default rather than refusing the node.
fn flag<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?.as_bool())
}

/// A window as the node carries it: a token, kept as it came where this release does not know it,
/// and nothing for a value that is not a token, so the field reads as its default.
fn token<'de, D, T>(deserializer: D) -> Result<Open<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: WireEnum,
{
    Ok(serde_json::Value::deserialize(deserializer)?
        .as_str()
        .map_or_else(Open::default, Open::parse))
}

#[cfg(test)]
mod tests {
    use super::{PublishedBilling, SettledVoidWindow};

    fn node(text: &str) -> PublishedBilling {
        serde_json::from_str(text).expect("the node parses")
    }

    /// A node that sets nothing, or sets something this release cannot read, keeps a paid bill
    /// voidable for the rest of its business date.
    #[test]
    fn a_paid_bill_is_voidable_on_its_business_date_until_the_store_sets_otherwise() {
        for unset in [
            "{}",
            r#"{ "settled_void_window": "SETTLED_VOID_WINDOW_UNSPECIFIED" }"#,
            r#"{ "settled_void_window": "SETTLED_VOID_WINDOW_BUSINESS_DATE" }"#,
            r#"{ "settled_void_window": "SETTLED_VOID_WINDOW_WEEK" }"#,
            r#"{ "settled_void_window": 2 }"#,
            r#"{ "settled_void_window": null }"#,
        ] {
            assert_eq!(
                node(unset).settled_void_window(),
                SettledVoidWindow::BusinessDate,
                "{unset}"
            );
        }
        assert_eq!(
            serde_json::to_string(&PublishedBilling::default()).expect("serialise"),
            "{}",
            "a node that sets nothing is written as nothing"
        );
    }

    /// ADR-0218: no store must print a check before payment until it says so, and a value that is
    /// not on or off reads as off rather than refusing the node.
    #[test]
    fn a_check_before_payment_is_off_until_a_store_turns_it_on() {
        for off in [
            "{}",
            r#"{ "check_before_payment": false }"#,
            r#"{ "check_before_payment": "yes" }"#,
            r#"{ "check_before_payment": null }"#,
        ] {
            assert!(!node(off).check_before_payment(), "{off}");
        }
        let on = node(r#"{ "check_before_payment": true }"#);
        assert!(on.check_before_payment());
        assert_eq!(
            serde_json::to_value(&on).expect("serialise"),
            serde_json::json!({ "check_before_payment": true })
        );
    }

    /// ADR-0220: a bill may be paid in parts until a store says otherwise.
    #[test]
    fn part_payments_are_on_until_a_store_turns_them_off() {
        for on in [
            "{}",
            r#"{ "part_payments": true }"#,
            r#"{ "part_payments": 1 }"#,
        ] {
            assert!(node(on).part_payments(), "{on}");
        }
        assert!(!node(r#"{ "part_payments": false }"#).part_payments());
    }

    /// ADR-0219: a hundred times the price until a store says otherwise, and a value outside one to
    /// a hundred, or not a whole number, reads as the default rather than refusing the node.
    #[test]
    fn a_price_override_is_bounded_at_a_hundred_times_until_a_store_narrows_it() {
        for unset in [
            "{}",
            r#"{ "price_override_max_multiple": 0 }"#,
            r#"{ "price_override_max_multiple": 101 }"#,
            r#"{ "price_override_max_multiple": "3" }"#,
            r#"{ "price_override_max_multiple": 2.5 }"#,
        ] {
            assert_eq!(node(unset).price_override_max_multiple(), 100, "{unset}");
        }
        assert_eq!(
            node(r#"{ "price_override_max_multiple": 3 }"#).price_override_max_multiple(),
            3
        );
    }

    #[test]
    fn a_store_may_narrow_the_window_to_the_shift() {
        let shift = node(r#"{ "settled_void_window": "SETTLED_VOID_WINDOW_SHIFT" }"#);
        assert_eq!(shift.settled_void_window(), SettledVoidWindow::Shift);
        assert_eq!(
            serde_json::to_value(&shift).expect("serialise"),
            serde_json::json!({ "settled_void_window": "SETTLED_VOID_WINDOW_SHIFT" })
        );
    }

    #[test]
    fn a_token_from_a_newer_release_is_written_back_as_it_came() {
        let newer = node(r#"{ "settled_void_window": "SETTLED_VOID_WINDOW_WEEK" }"#);
        assert_eq!(
            serde_json::to_value(&newer).expect("serialise"),
            serde_json::json!({ "settled_void_window": "SETTLED_VOID_WINDOW_WEEK" })
        );
    }
}
