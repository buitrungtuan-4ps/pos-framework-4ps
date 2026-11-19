// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A weighed item and where its weight came from, and the `weighing` node
//! ([ADR-0223](../../../docs/adr/0223-a-weighed-item-is-priced-by-its-weight.md)).
//!
//! An item sold by weight is priced per kilogram and its line quantity is its net weight in grams,
//! which is thousandths of a kilogram, so [`crate::quantity::Quantity`] carries it as it is and the
//! total is rounded once by the price's own rule. Every such line records where its weight came
//! from. Kilograms only, a line in grams and the source on every line are fixed rules; whether a
//! weight may be typed by hand and the least weight a line may carry are a store's settings
//! (ADR-0160), the first seeded by its country pack, which states the law.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::wire_enum;

wire_enum! {
    /// How an item is sold.
    SoldBy, prefix = "SOLD_BY";
    /// By the piece, at its price for one: every item before weighed items existed.
    Unit = "UNIT",
    /// By the kilogram: its price is per kilogram and its line counts net grams.
    Weight = "WEIGHT",
}

wire_enum! {
    /// Where a weighed line's weight came from (decision 2), recorded on the line for audit.
    WeightSource, prefix = "WEIGHT_SOURCE";
    /// A label printed by a labelling scale, read as a variable-measure code.
    Label = "LABEL",
    /// The scale beside the till, read by the edge through the `Scale` port.
    Scale = "SCALE",
    /// Typed by hand, only where the store's `weighing.manual_entry` allows it.
    Manual = "MANUAL",
}

/// The values `min_grams` may take.
pub const MIN_GRAMS: RangeInclusive<i64> = 1..=1_000;

/// What `min_grams` is where the store sets none: any weight above nothing.
pub const DEFAULT_MIN_GRAMS: i64 = 1;

/// The most grams one weighed line carries: what a five-digit weight label can hold.
pub const MAX_GRAMS: u32 = 99_999;

/// The `weighing` node: whether a weight may be typed by hand, and the least a line may weigh.
///
/// No `deny_unknown_fields`, as for every published node. A value of the wrong kind, or a number
/// outside [`MIN_GRAMS`], reads as absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct PublishedWeighing {
    /// Whether a weight may be typed by hand. Read it through [`Self::manual_entry`].
    #[serde(
        default,
        deserialize_with = "flag",
        skip_serializing_if = "Option::is_none"
    )]
    pub manual_entry: Option<bool>,
    /// The least net weight a weighed line may carry, in grams. Read it through
    /// [`Self::min_grams`].
    #[serde(
        default,
        deserialize_with = "whole",
        skip_serializing_if = "Option::is_none"
    )]
    pub min_grams: Option<i64>,
}

impl PublishedWeighing {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "weighing";

    /// Whether a weight may be typed by hand: only where the node turns it on. Off by default,
    /// because where trade law requires a certified scale a typed weight is not one (decision 3).
    #[must_use]
    pub fn manual_entry(&self) -> bool {
        self.manual_entry.unwrap_or(false)
    }

    /// The least net weight a weighed line may carry: the node's number within [`MIN_GRAMS`], and
    /// [`DEFAULT_MIN_GRAMS`] otherwise.
    #[must_use]
    pub fn min_grams(&self) -> u32 {
        self.min_grams
            .filter(|grams| MIN_GRAMS.contains(grams))
            .and_then(|grams| u32::try_from(grams).ok())
            .unwrap_or(1)
    }
}

fn flag<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?.as_bool())
}

fn whole<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?.as_i64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_entry_is_off_and_the_least_weight_is_a_gram_unless_the_node_says_otherwise() {
        for (text, manual, least) in [
            ("{}", false, 1),
            (r#"{ "manual_entry": true, "min_grams": 50 }"#, true, 50),
            (r#"{ "manual_entry": "yes", "min_grams": 0 }"#, false, 1),
            (r#"{ "min_grams": 1001 }"#, false, 1),
            (r#"{ "min_grams": "50" }"#, false, 1),
        ] {
            let node: PublishedWeighing = serde_json::from_str(text).unwrap_or_default();
            assert_eq!(node.manual_entry(), manual, "{text}");
            assert_eq!(node.min_grams(), least, "{text}");
        }
    }
}
