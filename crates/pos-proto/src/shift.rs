// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `shift` config node: how a store runs its shifts
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 2).
//!
//! Every field is a setting in [`crate::settings`], and every default is what the edge did before the
//! field existed. So a document with no `shift` node, or a node without a field, runs a store exactly
//! as it ran before, which is what lets an upgrade change nothing until someone sets a value.

use serde::{Deserialize, Serialize};

use crate::wire_enum;
use crate::wire_enum::Open;

wire_enum! {
    /// Whether a store sells while no shift is open.
    NoShiftSelling, prefix = "NO_SHIFT_SELLING";
    /// Sell with or without an open shift: the behaviour before the setting existed. A sale made
    /// while no shift is open belongs to no shift, and its cash is in no shift's expected drawer.
    Allow = "ALLOW",
    /// Refuse to seat a table, start a counter order or take a payment until a shift is open, so
    /// that every sale and every payment belongs to a shift.
    Refuse = "REFUSE",
}

/// The `shift` node.
///
/// No `deny_unknown_fields`, as for every published node: an edge on an older release applies a node
/// that carries a field it does not know, rather than refusing the whole document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedShift {
    /// Whether the store sells while no shift is open. Read it through
    /// [`PublishedShift::no_shift_selling`], which gives an absent or unknown value its default.
    #[serde(default)]
    pub no_shift_selling: Open<NoShiftSelling>,
}

impl PublishedShift {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "shift";

    /// Whether the store sells while no shift is open.
    ///
    /// An absent value, `NO_SHIFT_SELLING_UNSPECIFIED`, and a value this release does not know all
    /// read as [`NoShiftSelling::Allow`], the default. A value from a newer release is never offered
    /// to a store that cannot honour it (ADR-0160 decision 5), so the default is what such a store
    /// was running anyway.
    #[must_use]
    pub fn no_shift_selling(&self) -> NoShiftSelling {
        match self.no_shift_selling.known() {
            NoShiftSelling::Unspecified | NoShiftSelling::Allow => NoShiftSelling::Allow,
            NoShiftSelling::Refuse => NoShiftSelling::Refuse,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NoShiftSelling, PublishedShift};
    use crate::wire_enum::{Open, WireEnum};

    #[test]
    fn an_absent_node_field_sells_as_before() {
        let node: PublishedShift = serde_json::from_str("{}").expect("an empty node parses");
        assert_eq!(node.no_shift_selling(), NoShiftSelling::Allow);
        assert_eq!(
            PublishedShift::default().no_shift_selling(),
            NoShiftSelling::Allow
        );
    }

    #[test]
    fn a_published_refusal_is_read() {
        let node: PublishedShift =
            serde_json::from_str(r#"{ "no_shift_selling": "NO_SHIFT_SELLING_REFUSE" }"#)
                .expect("the node parses");
        assert_eq!(node.no_shift_selling(), NoShiftSelling::Refuse);
    }

    #[test]
    fn a_value_from_a_newer_release_reads_as_the_default() {
        let node: PublishedShift =
            serde_json::from_str(r#"{ "no_shift_selling": "NO_SHIFT_SELLING_LATER" }"#)
                .expect("an unknown token still parses");
        assert!(node.no_shift_selling.is_unrecognised());
        assert_eq!(node.no_shift_selling(), NoShiftSelling::Allow);
    }

    #[test]
    fn a_field_this_release_does_not_know_is_ignored() {
        let node: PublishedShift = serde_json::from_str(
            r#"{ "no_shift_selling": "NO_SHIFT_SELLING_REFUSE", "opening_float_minor": 500000 }"#,
        )
        .expect("an unknown field does not refuse the node");
        assert_eq!(node.no_shift_selling(), NoShiftSelling::Refuse);
    }

    #[test]
    fn the_node_round_trips() {
        let node = PublishedShift {
            no_shift_selling: Open::from_known(NoShiftSelling::Refuse),
        };
        let text = serde_json::to_string(&node).expect("serialise");
        assert_eq!(text, r#"{"no_shift_selling":"NO_SHIFT_SELLING_REFUSE"}"#);
        let back: PublishedShift = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back, node);
        assert_eq!(NoShiftSelling::Refuse.as_wire(), "NO_SHIFT_SELLING_REFUSE");
    }
}
