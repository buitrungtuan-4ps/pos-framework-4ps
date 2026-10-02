// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `printing` config node: how a store prints the paper it hands a guest
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 2).
//!
//! Every field is a setting in [`crate::settings`], and every default is what the edge did before the
//! field existed: a receipt in the store's display language, printed on every settle. So a document
//! with no `printing` node, or a node without a field, prints exactly as it printed before. Kitchen
//! tickets are not here: each station's language belongs to the `stations` node.

use serde::{Deserialize, Serialize};

use crate::wire_enum;
use crate::wire_enum::Open;

wire_enum! {
    /// The language a receipt, its copy and a pre-bill print in: their fixed labels, and each
    /// item's name where the menu translates it.
    ReceiptLanguage, prefix = "RECEIPT_LANGUAGE";
    /// The store's display language, from the `locale` node: what every receipt printed in before
    /// the setting existed.
    Display = "DISPLAY",
    /// The language of the store's country, from its country pack (ADR-0105), which the cloud
    /// publishes on the store's `locale` node: Vietnamese in Vietnam. What a new store is given, so
    /// its receipts follow its country whatever language its tills show. Until the store's locale
    /// names a country whose language the edge prints labels in, the display language, as
    /// [`Self::Display`].
    Country = "COUNTRY",
    /// Vietnamese.
    Vietnamese = "VI",
    /// English.
    English = "EN",
}

impl ReceiptLanguage {
    /// The language's tag (BCP 47) for a value that names one, or `None` for one the edge resolves
    /// from the store: [`Self::Display`] and [`Self::Country`].
    #[must_use]
    pub const fn tag(self) -> Option<&'static str> {
        match self {
            Self::Vietnamese => Some("vi"),
            Self::English => Some("en"),
            Self::Unspecified | Self::Display | Self::Country => None,
        }
    }
}

/// The `printing` node.
///
/// No `deny_unknown_fields`, as for every published node: an edge on an older release applies a node
/// that carries a field it does not know, rather than refusing the whole document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedPrinting {
    /// The language a receipt prints in. Read it through [`PublishedPrinting::receipt_language`],
    /// which gives an absent or unknown value its default.
    #[serde(default)]
    pub receipt_language: Open<ReceiptLanguage>,
    /// Whether a settle prints the guest's receipt. Read it through
    /// [`PublishedPrinting::receipt_printed_on_settle`]: absent is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_printed_on_settle: Option<bool>,
}

impl PublishedPrinting {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "printing";

    /// The language a receipt prints in.
    ///
    /// An absent value, `RECEIPT_LANGUAGE_UNSPECIFIED`, and a value this release does not know all
    /// read as [`ReceiptLanguage::Display`], the default. A value from a newer release is never
    /// offered to a store that cannot honour it (ADR-0160 decision 5), so the default is what such a
    /// store was running anyway.
    #[must_use]
    pub fn receipt_language(&self) -> ReceiptLanguage {
        match self.receipt_language.known() {
            ReceiptLanguage::Unspecified | ReceiptLanguage::Display => ReceiptLanguage::Display,
            ReceiptLanguage::Country => ReceiptLanguage::Country,
            ReceiptLanguage::Vietnamese => ReceiptLanguage::Vietnamese,
            ReceiptLanguage::English => ReceiptLanguage::English,
        }
    }

    /// Whether a settle prints the guest's receipt: `true` unless the store says otherwise, because
    /// every settle printed one before the setting existed.
    #[must_use]
    pub fn receipt_printed_on_settle(&self) -> bool {
        self.receipt_printed_on_settle.unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::{PublishedPrinting, ReceiptLanguage};
    use crate::wire_enum::{Open, WireEnum};

    #[test]
    fn an_absent_node_or_field_prints_as_before() {
        let node: PublishedPrinting = serde_json::from_str("{}").expect("an empty node parses");
        assert_eq!(node.receipt_language(), ReceiptLanguage::Display);
        assert!(node.receipt_printed_on_settle());
        assert_eq!(PublishedPrinting::default(), node);
    }

    #[test]
    fn a_published_language_and_switch_are_read() {
        let node: PublishedPrinting = serde_json::from_str(
            r#"{ "receipt_language": "RECEIPT_LANGUAGE_COUNTRY", "receipt_printed_on_settle": false }"#,
        )
        .expect("the node parses");
        assert_eq!(node.receipt_language(), ReceiptLanguage::Country);
        assert!(!node.receipt_printed_on_settle());
    }

    #[test]
    fn a_value_from_a_newer_release_reads_as_the_default() {
        let node: PublishedPrinting =
            serde_json::from_str(r#"{ "receipt_language": "RECEIPT_LANGUAGE_JA" }"#)
                .expect("an unknown token still parses");
        assert!(node.receipt_language.is_unrecognised());
        assert_eq!(node.receipt_language(), ReceiptLanguage::Display);
    }

    #[test]
    fn a_field_this_release_does_not_know_is_ignored() {
        let node: PublishedPrinting = serde_json::from_str(
            r#"{ "receipt_language": "RECEIPT_LANGUAGE_VI", "receipt_second_language": "EN" }"#,
        )
        .expect("an unknown field does not refuse the node");
        assert_eq!(node.receipt_language(), ReceiptLanguage::Vietnamese);
    }

    #[test]
    fn only_a_named_language_has_a_tag() {
        assert_eq!(ReceiptLanguage::Vietnamese.tag(), Some("vi"));
        assert_eq!(ReceiptLanguage::English.tag(), Some("en"));
        for resolved_by_the_edge in [
            ReceiptLanguage::Unspecified,
            ReceiptLanguage::Display,
            ReceiptLanguage::Country,
        ] {
            assert_eq!(resolved_by_the_edge.tag(), None);
        }
    }

    #[test]
    fn the_node_round_trips_and_leaves_an_unset_switch_off_the_wire() {
        let node = PublishedPrinting {
            receipt_language: Open::from_known(ReceiptLanguage::English),
            receipt_printed_on_settle: None,
        };
        let text = serde_json::to_string(&node).expect("serialise");
        assert_eq!(text, r#"{"receipt_language":"RECEIPT_LANGUAGE_EN"}"#);
        let back: PublishedPrinting = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back, node);
        assert_eq!(
            ReceiptLanguage::Country.as_wire(),
            "RECEIPT_LANGUAGE_COUNTRY"
        );
    }
}
