// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `tender_keys` config node: the keys the pay screen offers
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 2).
//!
//! Every field is a setting in [`crate::settings`], and every default is what the pay screen offered
//! before the field existed: tip keys of five, ten and fifteen percent. So a document with no
//! `tender_keys` node, or a node without a field, runs a store exactly as it ran before.
//!
//! A node of its own rather than fields on `tender`, whose `accepted` list restricts the payment
//! methods a store takes ([`crate::channels::PublishedTender`]). An edge from before these settings
//! ignores a node it does not know, so they can never be read as a restriction there.

use core::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

/// The whole percentages of the bill a tip key may be, both included. `0` hides the key.
pub const TIP_PERCENT: RangeInclusive<i64> = 0..=100;

/// The tip keys when nothing sets them: five, ten and fifteen percent of the bill, the keys the pay
/// screen offered before the settings existed.
pub const DEFAULT_TIP_PERCENTS: [u8; 3] = [5, 10, 15];

/// The `tender_keys` node.
///
/// Each field is the number as it travels: `None` when the node does not carry it, or carries
/// something that is not a whole number, so a value the edge cannot read costs the node none of its
/// other fields. Read a field through its accessor, which applies the bounds and the default. No
/// `deny_unknown_fields`, as for every published node: an edge on an older release applies a node
/// that carries a field it does not know, rather than refusing the whole document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedTenderKeys {
    /// The first tip key, a whole percentage of the bill. Read it through
    /// [`PublishedTenderKeys::first_tip_percent`].
    #[serde(
        default,
        deserialize_with = "whole_number",
        skip_serializing_if = "Option::is_none"
    )]
    pub first_tip_percent: Option<i64>,
    /// The second tip key. Read it through [`PublishedTenderKeys::second_tip_percent`].
    #[serde(
        default,
        deserialize_with = "whole_number",
        skip_serializing_if = "Option::is_none"
    )]
    pub second_tip_percent: Option<i64>,
    /// The third tip key. Read it through [`PublishedTenderKeys::third_tip_percent`].
    #[serde(
        default,
        deserialize_with = "whole_number",
        skip_serializing_if = "Option::is_none"
    )]
    pub third_tip_percent: Option<i64>,
}

impl PublishedTenderKeys {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "tender_keys";

    /// The first tip key: the node's number when it is within [`TIP_PERCENT`], and `5` otherwise.
    /// `0` hides the key.
    #[must_use]
    pub fn first_tip_percent(&self) -> u8 {
        let [first, _, _] = DEFAULT_TIP_PERCENTS;
        tip_percent(self.first_tip_percent, first)
    }

    /// The second tip key: the node's number when it is within [`TIP_PERCENT`], and `10` otherwise.
    #[must_use]
    pub fn second_tip_percent(&self) -> u8 {
        let [_, second, _] = DEFAULT_TIP_PERCENTS;
        tip_percent(self.second_tip_percent, second)
    }

    /// The third tip key: the node's number when it is within [`TIP_PERCENT`], and `15` otherwise.
    #[must_use]
    pub fn third_tip_percent(&self) -> u8 {
        let [_, _, third] = DEFAULT_TIP_PERCENTS;
        tip_percent(self.third_tip_percent, third)
    }

    /// The tip keys the pay screen offers, in the order the settings name them: each one that is not
    /// `0`, and not the same percentage as a key before it. Empty when every key is `0`, and the pay
    /// screen then shows no tip row.
    #[must_use]
    pub fn tip_percents(&self) -> Vec<u8> {
        let mut keys = Vec::with_capacity(DEFAULT_TIP_PERCENTS.len());
        for percent in [
            self.first_tip_percent(),
            self.second_tip_percent(),
            self.third_tip_percent(),
        ] {
            if percent > 0 && !keys.contains(&percent) {
                keys.push(percent);
            }
        }
        keys
    }
}

/// `value` when it is within [`TIP_PERCENT`], and `default` otherwise.
fn tip_percent(value: Option<i64>, default: u8) -> u8 {
    value
        .filter(|percent| TIP_PERCENT.contains(percent))
        .and_then(|percent| u8::try_from(percent).ok())
        .unwrap_or(default)
}

/// A whole number as the node carries it, and `None` for anything else — a string, a fraction,
/// `null` — so the field reads as its default.
fn whole_number<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?.as_i64())
}

#[cfg(test)]
mod tests {
    use super::PublishedTenderKeys;

    fn node(text: &str) -> PublishedTenderKeys {
        serde_json::from_str(text).expect("the node parses")
    }

    #[test]
    fn a_node_that_sets_no_tip_key_offers_five_ten_and_fifteen_percent() {
        for unset in [node("{}"), PublishedTenderKeys::default()] {
            assert_eq!(
                (
                    unset.first_tip_percent(),
                    unset.second_tip_percent(),
                    unset.third_tip_percent()
                ),
                (5, 10, 15)
            );
            assert_eq!(unset.tip_percents(), vec![5, 10, 15]);
        }
        assert_eq!(
            serde_json::to_string(&PublishedTenderKeys::default()).expect("serialise"),
            "{}",
            "a node that sets nothing is written as nothing"
        );
    }

    /// The keys are offered in the order they are set, without a key at `0` or one that repeats an
    /// earlier key's percentage; and none at all when every key is `0`.
    #[test]
    fn the_keys_are_offered_in_order_once_each_and_a_zero_hides_one() {
        let set = node(
            r#"{ "first_tip_percent": 10, "second_tip_percent": 0, "third_tip_percent": 20 }"#,
        );
        assert_eq!(set.tip_percents(), vec![10, 20]);
        assert_eq!(
            node(r#"{ "first_tip_percent": 20, "third_tip_percent": 100 }"#).tip_percents(),
            vec![20, 10, 100],
            "the order the settings name them, not ascending"
        );
        assert_eq!(
            node(
                r#"{ "first_tip_percent": 10, "second_tip_percent": 10, "third_tip_percent": 15 }"#
            )
            .tip_percents(),
            vec![10, 15],
            "a percentage an earlier key offers is offered once"
        );
        let none =
            node(r#"{ "first_tip_percent": 0, "second_tip_percent": 0, "third_tip_percent": 0 }"#);
        assert!(none.tip_percents().is_empty());
    }

    /// A value outside 0 to 100, or one that is not a whole number, reads as that key's default
    /// without costing the other keys theirs.
    #[test]
    fn a_key_that_cannot_be_read_is_its_default_and_costs_the_others_nothing() {
        for garbage in [
            r#"{ "first_tip_percent": 101, "second_tip_percent": 0 }"#,
            r#"{ "first_tip_percent": -1, "second_tip_percent": 0 }"#,
            r#"{ "first_tip_percent": "12", "second_tip_percent": 0 }"#,
            r#"{ "first_tip_percent": 12.5, "second_tip_percent": 0 }"#,
            r#"{ "first_tip_percent": null, "second_tip_percent": 0 }"#,
            r#"{ "first_tip_percent": [12], "second_tip_percent": 0 }"#,
        ] {
            let read = node(garbage);
            assert_eq!(read.first_tip_percent(), 5, "{garbage}");
            assert_eq!(read.tip_percents(), vec![5, 15], "{garbage}");
        }
    }
}
