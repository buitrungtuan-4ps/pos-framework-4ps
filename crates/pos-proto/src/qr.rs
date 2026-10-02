// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `qr` config node, typed for the settings it carries
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! item 2 and its consequences).
//!
//! The node is older than this type: the console's QR guardrails write it (`enabled`,
//! `staff_confirmation_required`, `per_table_limit`, `rate_window_secs`, `business_hours`), and the
//! cloud's guest intake and the edge each read the fields they need from it where they need them.
//! This type carries only the fields that are settings in [`crate::settings`], so far one, and
//! leaves the rest to those readers. It has no `deny_unknown_fields`, as for every published node,
//! so a node carrying the guardrails parses, and so does one carrying a field from a newer release.
//!
//! Every default is what the edge did before the field existed, so a document with no `qr` node,
//! or a node without a field, runs a store as it ran before.

use serde::{Deserialize, Serialize};

use crate::wire_enum;
use crate::wire_enum::Open;

wire_enum! {
    /// What a guest's QR order does at a table that already has an open order.
    TableOrder, prefix = "TABLE_ORDER";
    /// The guest's order is its own order on the table: the behaviour before the setting existed.
    Separate = "SEPARATE",
    /// The guest's lines join the table's open order, once a member of staff confirms them where
    /// the store asks for that, so the table has one order and one bill. A table whose order
    /// cannot take a line, because its bill is open or paid, gets the guest's own order as with
    /// [`Self::Separate`].
    Join = "JOIN",
}

/// The `qr` node, as far as the register reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedQr {
    /// What a guest's order does at a table that already has an open order. Read it through
    /// [`PublishedQr::table_order`], which gives an absent or unknown value its default.
    #[serde(default)]
    pub table_order: Open<TableOrder>,
}

impl PublishedQr {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "qr";

    /// What a guest's order does at a table that already has an open order.
    ///
    /// An absent value, `TABLE_ORDER_UNSPECIFIED`, and a value this release does not know all read
    /// as [`TableOrder::Separate`], the default. A value from a newer release is never offered to a
    /// store that cannot honour it (ADR-0160 decision 5), so the default is what such a store was
    /// running anyway.
    #[must_use]
    pub fn table_order(&self) -> TableOrder {
        match self.table_order.known() {
            TableOrder::Unspecified | TableOrder::Separate => TableOrder::Separate,
            TableOrder::Join => TableOrder::Join,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PublishedQr, TableOrder};
    use crate::wire_enum::{Open, WireEnum};

    #[test]
    fn an_absent_field_keeps_a_guest_order_separate_as_before() {
        let node: PublishedQr = serde_json::from_str("{}").expect("an empty node parses");
        assert_eq!(node.table_order(), TableOrder::Separate);
        assert_eq!(PublishedQr::default().table_order(), TableOrder::Separate);
    }

    #[test]
    fn a_published_join_is_read_beside_the_guardrails() {
        let node: PublishedQr = serde_json::from_str(
            r#"{
                "enabled": true,
                "staff_confirmation_required": true,
                "per_table_limit": 10,
                "rate_window_secs": 60,
                "table_order": "TABLE_ORDER_JOIN"
            }"#,
        )
        .expect("the guardrails do not refuse the node");
        assert_eq!(node.table_order(), TableOrder::Join);
    }

    #[test]
    fn a_value_from_a_newer_release_reads_as_the_default() {
        let node: PublishedQr = serde_json::from_str(r#"{ "table_order": "TABLE_ORDER_LATER" }"#)
            .expect("an unknown token still parses");
        assert!(node.table_order.is_unrecognised());
        assert_eq!(node.table_order(), TableOrder::Separate);
    }

    #[test]
    fn the_node_round_trips() {
        let node = PublishedQr {
            table_order: Open::from_known(TableOrder::Join),
        };
        let text = serde_json::to_string(&node).expect("serialise");
        assert_eq!(text, r#"{"table_order":"TABLE_ORDER_JOIN"}"#);
        let back: PublishedQr = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back, node);
        assert_eq!(TableOrder::Separate.as_wire(), "TABLE_ORDER_SEPARATE");
    }
}
