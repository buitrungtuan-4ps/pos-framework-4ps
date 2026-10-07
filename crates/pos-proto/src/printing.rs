// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `printing` config node: how a store prints the paper it hands a guest
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 2).
//!
//! Every field is a setting in [`crate::settings`], and every default is what the edge did before the
//! field existed: a receipt in the store's display language alone, printed on every settle, with
//! each tax line's named components under its rate. So a document with no `printing` node, or a
//! node without a field, prints exactly as it printed before.
//! Kitchen tickets are not here: each station's language belongs to the `stations` node, as a
//! [`ReceiptLanguage`] of its own ([`crate::floor::KitchenStation::ticket_language`]).
//!
//! The size a line is drawn at when the printer cannot draw it in its own characters is here too,
//! for every piece of paper the store prints: it was the box's own `font_size_dots` in its
//! `config.toml`, and ADR-0160 decision 6 makes it a setting.

use core::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::wire_enum;
use crate::wire_enum::{Open, is_absent};

/// The printer dots per em a line the printer cannot draw in its own characters may be drawn at,
/// both bounds included. Sixteen at the least, below which a Vietnamese tone mark no longer reads on
/// a 203 dpi head. Forty-eight at the most, twice the default, so a dish name still fits a 58 mm
/// paper in a few words a line.
pub const FONT_SIZE_DOTS: RangeInclusive<i64> = 16..=48;

/// The size when nothing sets one: 24 dots per em, a comfortable receipt body at the 203 dpi every
/// common thermal printer runs at, which the edge drew at before the setting existed.
pub const DEFAULT_FONT_SIZE_DOTS: u16 = 24;

wire_enum! {
    /// The language a receipt, its copy and a pre-bill print in, and a kitchen station's tickets:
    /// their fixed labels, and each item's name where the menu translates it. The `printing` node's
    /// `receipt_language` chooses it for the paper a guest is handed, and each station's
    /// `ticket_language` on the `stations` node for the paper its cooks read.
    ReceiptLanguage, prefix = "RECEIPT_LANGUAGE";
    /// The store's display language, from the `locale` node: what every receipt and every kitchen
    /// ticket printed in before the setting existed.
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

wire_enum! {
    /// A second language a receipt, its copy and a pre-bill print in, after the first: a bilingual
    /// receipt. Each label prints in the receipt's language and then in this one, and each item,
    /// modifier and fee prints its name in this one too where the menu or the fee's rule translates
    /// it.
    ReceiptSecondLanguage, prefix = "RECEIPT_SECOND_LANGUAGE";
    /// No second language: a receipt prints in one, as every receipt did before the setting existed.
    None = "NONE",
    /// Vietnamese.
    Vietnamese = "VI",
    /// English.
    English = "EN",
}

impl ReceiptSecondLanguage {
    /// The language's tag (BCP 47), or `None` for no second language.
    #[must_use]
    pub const fn tag(self) -> Option<&'static str> {
        match self {
            Self::Vietnamese => Some("vi"),
            Self::English => Some("en"),
            Self::Unspecified | Self::None => None,
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
    /// A second language the receipt prints in. Read it through
    /// [`PublishedPrinting::receipt_second_language`], which reads an absent or unknown value as
    /// none. Left off the wire while absent, so a node that does not set it is written as before.
    #[serde(default, skip_serializing_if = "is_absent")]
    pub receipt_second_language: Open<ReceiptSecondLanguage>,
    /// How large a line the printer cannot draw in its own characters is drawn, in printer dots per
    /// em. Read it through [`PublishedPrinting::font_size_dots`]. Left off the wire while absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size_dots: Option<i64>,
    /// Whether a receipt, its copy and a pre-bill print each tax line's named components. Read it
    /// through [`PublishedPrinting::receipt_tax_components`]: absent is `true`. Left off the wire
    /// while absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_tax_components: Option<bool>,
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

    /// Whether a receipt, its copy and a pre-bill print each tax line's named components under its
    /// rate, where the store's `tax` node names them
    /// ([ADR-0168](../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md)
    /// decision 4): `true` unless the store says otherwise, because every receipt printed them
    /// before the setting existed. Off, each prints the tax line alone; the settle records the
    /// components either way.
    #[must_use]
    pub fn receipt_tax_components(&self) -> bool {
        self.receipt_tax_components.unwrap_or(true)
    }

    /// The second language a receipt prints in.
    ///
    /// An absent value, `RECEIPT_SECOND_LANGUAGE_UNSPECIFIED`, and a value this release does not
    /// know all read as [`ReceiptSecondLanguage::None`], so a receipt prints in one language until
    /// a store chooses a second, as for [`Self::receipt_language`].
    #[must_use]
    pub fn receipt_second_language(&self) -> ReceiptSecondLanguage {
        match self.receipt_second_language.known() {
            ReceiptSecondLanguage::Unspecified | ReceiptSecondLanguage::None => {
                ReceiptSecondLanguage::None
            }
            ReceiptSecondLanguage::Vietnamese => ReceiptSecondLanguage::Vietnamese,
            ReceiptSecondLanguage::English => ReceiptSecondLanguage::English,
        }
    }

    /// The printer dots per em a line the printer cannot draw in its own characters is drawn at,
    /// or `None` when the node sets no value within [`FONT_SIZE_DOTS`].
    ///
    /// `None` rather than the default, as for the sign-in idle timeout
    /// ([`crate::session::PublishedSession::sign_in_idle_timeout_minutes`]), because a box may still
    /// carry a size in its local file, deprecated by ADR-0160 decision 6. The edge falls back to
    /// that, and to [`DEFAULT_FONT_SIZE_DOTS`] when the file sets none. A line the printer's own
    /// character set covers prints in the printer's font, which this does not change, and
    /// double-size text is drawn at twice it.
    #[must_use]
    pub fn font_size_dots(&self) -> Option<u16> {
        self.font_size_dots
            .filter(|dots| FONT_SIZE_DOTS.contains(dots))
            .and_then(|dots| u16::try_from(dots).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::{FONT_SIZE_DOTS, PublishedPrinting, ReceiptLanguage, ReceiptSecondLanguage};
    use crate::wire_enum::{Open, WireEnum};

    #[test]
    fn an_absent_node_or_field_prints_as_before() {
        let node: PublishedPrinting = serde_json::from_str("{}").expect("an empty node parses");
        assert_eq!(node.receipt_language(), ReceiptLanguage::Display);
        assert!(node.receipt_printed_on_settle());
        assert!(
            node.receipt_tax_components(),
            "each tax line's components print"
        );
        assert_eq!(node.receipt_second_language(), ReceiptSecondLanguage::None);
        assert_eq!(node.font_size_dots(), None, "the box's own size");
        assert_eq!(PublishedPrinting::default(), node);
    }

    #[test]
    fn a_store_that_turns_tax_components_off_says_so_and_one_that_does_not_writes_nothing() {
        // ADR-0168 decision 4: on until a store turns it off, and left off the wire until then, so
        // a node written before the setting is written the same.
        let off: PublishedPrinting = serde_json::from_str(r#"{ "receipt_tax_components": false }"#)
            .expect("the node parses");
        assert!(!off.receipt_tax_components());
        assert_eq!(
            serde_json::to_string(&off).expect("serialise"),
            r#"{"receipt_language":"RECEIPT_LANGUAGE_UNSPECIFIED","receipt_tax_components":false}"#
        );
        assert!(
            !serde_json::to_string(&PublishedPrinting::default())
                .expect("serialise")
                .contains("receipt_tax_components")
        );
    }

    #[test]
    fn a_font_size_within_its_bounds_is_read_and_one_outside_them_is_none() {
        for (published, read) in [
            (16, Some(16)),
            (24, Some(24)),
            (32, Some(32)),
            (48, Some(48)),
            (15, None),
            (49, None),
            (0, None),
            (-24, None),
        ] {
            let node: PublishedPrinting =
                serde_json::from_str(&format!(r#"{{ "font_size_dots": {published} }}"#))
                    .expect("the node parses");
            assert_eq!(node.font_size_dots(), read, "{published}");
        }
        assert!(*FONT_SIZE_DOTS.start() > 0, "no size draws nothing");
    }

    #[test]
    fn an_absent_unspecified_or_unknown_second_language_is_none() {
        for absent in [
            "{}",
            r#"{ "receipt_second_language": "RECEIPT_SECOND_LANGUAGE_UNSPECIFIED" }"#,
            r#"{ "receipt_second_language": "RECEIPT_SECOND_LANGUAGE_JA" }"#,
        ] {
            let node: PublishedPrinting = serde_json::from_str(absent).expect("the node parses");
            assert_eq!(
                node.receipt_second_language(),
                ReceiptSecondLanguage::None,
                "{absent}"
            );
        }
        let english: PublishedPrinting =
            serde_json::from_str(r#"{ "receipt_second_language": "RECEIPT_SECOND_LANGUAGE_EN" }"#)
                .expect("the node parses");
        assert_eq!(
            english.receipt_second_language(),
            ReceiptSecondLanguage::English
        );
        assert_eq!(english.receipt_second_language().tag(), Some("en"));
        assert_eq!(ReceiptSecondLanguage::None.tag(), None);
    }

    #[test]
    fn a_second_language_is_left_off_the_wire_until_set_and_a_newer_one_goes_back_as_it_came() {
        let unset = serde_json::to_string(&PublishedPrinting::default()).expect("serialise");
        assert_eq!(
            unset,
            r#"{"receipt_language":"RECEIPT_LANGUAGE_UNSPECIFIED"}"#
        );
        let newer = r#"{"receipt_language":"RECEIPT_LANGUAGE_VI","receipt_second_language":"RECEIPT_SECOND_LANGUAGE_JA"}"#;
        let node: PublishedPrinting = serde_json::from_str(newer).expect("the node parses");
        assert_eq!(serde_json::to_string(&node).expect("serialise"), newer);
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
            r#"{ "receipt_language": "RECEIPT_LANGUAGE_VI", "receipt_logo": "4PS" }"#,
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
            receipt_second_language: Open::default(),
            font_size_dots: None,
            receipt_tax_components: None,
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
