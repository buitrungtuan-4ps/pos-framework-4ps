// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `tabs` config node: whether a bar tab may carry a guest's label
//! ([ADR-0221](../../../docs/adr/0221-a-bar-tab-is-an-order-without-a-table-that-stays-open.md)).
//!
//! A label is what staff type to find a tab again, usually the guest's name, which is personal data
//! (Decree 13/2023, GDPR). It is off unless a store turns it on, and turning it on is written only
//! with a recorded confirmation of its notice (ADR-0169), which names the lawful basis and the DPIA
//! entry the store owes. No template turns it on. A label stays in the edge's subject store, never
//! in an event, a log, a ticket or the cloud.

use serde::{Deserialize, Serialize};

use crate::wire_enum;
use crate::wire_enum::{Open, is_absent};

/// The most characters a label may have: a bound, so the store's subject store stays small.
pub const LABEL_MAX_CHARS: usize = 32;

wire_enum! {
    /// Whether a tab may carry a guest's label.
    GuestLabel, prefix = "GUEST_LABEL";
    /// A tab has its number only: the default, and what every store did before the setting.
    Off = "OFF",
    /// Staff may type a label beside a tab's number, kept on the store only.
    On = "ON",
}

/// The `tabs` node.
///
/// No `deny_unknown_fields`, as for every published node: an edge on an older release applies a
/// node that carries a field it does not know, rather than refusing the whole document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct PublishedTabs {
    /// Whether a tab may carry a label. Read it through [`PublishedTabs::guest_label`]. A value
    /// that is not a token reads as absent. Left off the wire while absent.
    #[serde(default, deserialize_with = "token", skip_serializing_if = "is_absent")]
    pub guest_label: Open<GuestLabel>,
}

impl PublishedTabs {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "tabs";

    /// Whether a tab may carry a label: [`GuestLabel::On`] only where the node says so, and
    /// [`GuestLabel::Off`] for an absent, unspecified or unknown value, which records nothing.
    #[must_use]
    pub fn guest_label(&self) -> GuestLabel {
        match self.guest_label.known() {
            GuestLabel::On => GuestLabel::On,
            GuestLabel::Unspecified | GuestLabel::Off => GuestLabel::Off,
        }
    }
}

/// A label setting as the node carries it: a token, kept as it came where this release does not
/// know it, and nothing for a value that is not a token.
fn token<'de, D>(deserializer: D) -> Result<Open<GuestLabel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?
        .as_str()
        .map_or_else(Open::default, Open::parse))
}

#[cfg(test)]
mod tests {
    use super::{GuestLabel, PublishedTabs};

    #[test]
    fn a_label_is_off_unless_the_node_turns_it_on() {
        for (text, read) in [
            ("{}", GuestLabel::Off),
            (r#"{ "guest_label": "GUEST_LABEL_OFF" }"#, GuestLabel::Off),
            (r#"{ "guest_label": "GUEST_LABEL_LATER" }"#, GuestLabel::Off),
            (r#"{ "guest_label": true }"#, GuestLabel::Off),
            (r#"{ "guest_label": "GUEST_LABEL_ON" }"#, GuestLabel::On),
        ] {
            let node: PublishedTabs = serde_json::from_str(text).unwrap_or_default();
            assert_eq!(node.guest_label(), read, "{text}");
        }
    }
}
