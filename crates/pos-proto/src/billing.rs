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

use serde::{Deserialize, Serialize};

use crate::wire_enum;
use crate::wire_enum::{Open, is_absent};

wire_enum! {
    /// How long after it was paid a bill may still be voided at the till.
    SettledVoidWindow, prefix = "SETTLED_VOID_WINDOW";
    /// While the store is still trading on the bill's own business date.
    BusinessDate = "BUSINESS_DATE",
    /// Only while the shift that took the money is open.
    Shift = "SHIFT",
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
}

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
}

/// A window as the node carries it: a token, kept as it came where this release does not know it,
/// and nothing for a value that is not a token, so the field reads as its default.
fn token<'de, D>(deserializer: D) -> Result<Open<SettledVoidWindow>, D::Error>
where
    D: serde::Deserializer<'de>,
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
