// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `qr` config node
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! item 2 and its consequences).
//!
//! Two kinds of field share the node. The console's QR form writes the guardrails around QR ordering
//! (`enabled`, `staff_confirmation_required`, `per_table_limit`, `rate_window_secs`,
//! `business_hours`; ADR-0057, ADR-0116), which the cloud's guest intake and the edge read, and the
//! settings in [`crate::settings`] write `table_order` beside them. The cloud and the edge both read
//! the node through this type, so the two cannot drift apart about it.
//!
//! The guardrails are read field by field, as their readers always read them: a guardrail holding
//! anything other than its JSON type reads as absent, and never takes the node, or another
//! guardrail, down with it. [`PublishedQr::guardrails`] reads them apart from `table_order`, so a
//! malformed setting does not stop them being read either. The type has no `deny_unknown_fields`, as
//! for every published node, so a node carrying a field from a newer release parses.
//!
//! Every default is what the edge did before the field existed, so a document with no `qr` node,
//! or a node without a field, runs a store as it ran before.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// The `qr` node: its setting, and the guardrails the console's QR form writes beside it.
///
/// Each guardrail is `None` where the node does not carry it, or carries something other than its
/// JSON type, and each is left off the wire while absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedQr {
    /// What a guest's order does at a table that already has an open order. Read it through
    /// [`PublishedQr::table_order`], which gives an absent or unknown value its default.
    #[serde(default)]
    pub table_order: Open<TableOrder>,
    /// QR ordering's switch as the node carries it. The switch itself is the store's
    /// `qr_ordering_enabled` flag (ADR-0160 decision 5), and a cloud that knows it writes this equal
    /// to it, so a `false` flag beside a value that is not `false` is the flag from before the
    /// switch.
    #[serde(
        default,
        deserialize_with = "flag",
        skip_serializing_if = "Option::is_none"
    )]
    pub enabled: Option<bool>,
    /// Whether a table-bearing guest order waits for a member of staff before the kitchen sees it.
    /// Absent is `true` to both readers (ADR-0057): a store that never said holds guest orders.
    #[serde(
        default,
        deserialize_with = "flag",
        skip_serializing_if = "Option::is_none"
    )]
    pub staff_confirmation_required: Option<bool>,
    /// How many guest orders one table may submit within the rate window. Absent is the guest
    /// intake's default.
    #[serde(
        default,
        deserialize_with = "count",
        skip_serializing_if = "Option::is_none"
    )]
    pub per_table_limit: Option<u32>,
    /// The rate window, in seconds. Absent is the guest intake's default.
    #[serde(
        default,
        deserialize_with = "seconds",
        skip_serializing_if = "Option::is_none"
    )]
    pub rate_window_secs: Option<u64>,
    /// The hours the store takes QR orders. Absent, or without a whole open and close hour, is
    /// always open.
    #[serde(
        default,
        deserialize_with = "hours",
        skip_serializing_if = "Option::is_none"
    )]
    pub business_hours: Option<QrBusinessHours>,
}

/// The hours a store takes QR orders, as the `qr` node carries them.
///
/// Read only through the node ([`PublishedQr`]), field by field: an open or close hour that is not a
/// whole number from 0 to 255 leaves the store with no hours, which is always open, and an offset
/// that is not a whole number reads as absent. The guest intake reads an hour past 23 as no hours
/// too, as the console refuses to write one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct QrBusinessHours {
    /// The hour orders open, in the store's offset from UTC.
    pub open_hour: u8,
    /// The hour orders close. An open hour later than the close hour wraps past midnight, and one
    /// equal to it is always open.
    pub close_hour: u8,
    /// The store's offset from UTC, in minutes. Absent reads as UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tz_offset_minutes: Option<i64>,
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

    /// The guardrails a `qr` node carries, read apart from its setting: a `table_order` that does
    /// not parse is left out rather than taking them down, and a node that is not an object carries
    /// none. The `table_order` of what comes back is the default; read the setting through the
    /// node's own parse.
    #[must_use]
    pub fn guardrails(node: &Value) -> Option<Self> {
        let mut fields = node.as_object()?.clone();
        fields.remove("table_order");
        serde_json::from_value(Value::Object(fields)).ok()
    }
}

/// A switch as the node carries it: `true` or `false`, and `None` for anything else.
fn flag<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Value::deserialize(deserializer)?.as_bool())
}

/// A count as the node carries it: a whole number that fits a `u32`, and `None` for anything else.
fn count<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Value::deserialize(deserializer)?
        .as_u64()
        .and_then(|count| u32::try_from(count).ok()))
}

/// A number of seconds as the node carries it: a non-negative whole number, and `None` for anything
/// else.
fn seconds<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Value::deserialize(deserializer)?.as_u64())
}

/// The business hours as the node carries them, read as [`QrBusinessHours`] says.
fn hours<'de, D>(deserializer: D) -> Result<Option<QrBusinessHours>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let hour = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|hour| u8::try_from(hour).ok())
    };
    Ok(match (hour("open_hour"), hour("close_hour")) {
        (Some(open_hour), Some(close_hour)) => Some(QrBusinessHours {
            open_hour,
            close_hour,
            tz_offset_minutes: value.get("tz_offset_minutes").and_then(Value::as_i64),
        }),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::{PublishedQr, QrBusinessHours, TableOrder};
    use crate::wire_enum::{Open, WireEnum};
    use serde_json::json;

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
            ..PublishedQr::default()
        };
        let text = serde_json::to_string(&node).expect("serialise");
        assert_eq!(text, r#"{"table_order":"TABLE_ORDER_JOIN"}"#);
        let back: PublishedQr = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back, node);
        assert_eq!(TableOrder::Separate.as_wire(), "TABLE_ORDER_SEPARATE");
    }

    /// The node as the console's QR form and the settings write it, guardrails and setting together.
    fn written() -> serde_json::Value {
        json!({
            "enabled": true,
            "staff_confirmation_required": false,
            "per_table_limit": 5,
            "rate_window_secs": 300,
            "business_hours": { "open_hour": 10, "close_hour": 22, "tz_offset_minutes": 420 },
            "table_order": "TABLE_ORDER_JOIN"
        })
    }

    #[test]
    fn a_written_node_round_trips_through_the_type() {
        let node: PublishedQr = serde_json::from_value(written()).expect("the node parses");
        assert_eq!(node.table_order(), TableOrder::Join);
        assert_eq!(node.enabled, Some(true));
        assert_eq!(node.staff_confirmation_required, Some(false));
        assert_eq!(node.per_table_limit, Some(5));
        assert_eq!(node.rate_window_secs, Some(300));
        assert_eq!(
            node.business_hours,
            Some(QrBusinessHours {
                open_hour: 10,
                close_hour: 22,
                tz_offset_minutes: Some(420),
            })
        );
        assert_eq!(serde_json::to_value(&node).expect("serialise"), written());
    }

    #[test]
    fn an_absent_guardrail_stays_absent_and_an_unknown_field_is_ignored() {
        let node: PublishedQr =
            serde_json::from_value(json!({ "staff_confirmation_required": true, "later": 1 }))
                .expect("the node parses");
        assert_eq!(
            node,
            PublishedQr {
                staff_confirmation_required: Some(true),
                ..PublishedQr::default()
            }
        );
        assert_eq!(
            serde_json::to_value(&node).expect("serialise"),
            json!({ "table_order": "TABLE_ORDER_UNSPECIFIED", "staff_confirmation_required": true })
        );
    }

    #[test]
    fn a_guardrail_of_the_wrong_kind_reads_as_absent_and_takes_nothing_else_down() {
        let node: PublishedQr = serde_json::from_value(json!({
            "enabled": "yes",
            "staff_confirmation_required": 0,
            "per_table_limit": 5_000_000_000_u64,
            "rate_window_secs": -1,
            "business_hours": { "open_hour": "10", "close_hour": 22 },
            "table_order": "TABLE_ORDER_JOIN"
        }))
        .expect("no guardrail refuses the node");
        assert_eq!(
            node,
            PublishedQr {
                table_order: Open::from_known(TableOrder::Join),
                ..PublishedQr::default()
            }
        );
        for (hours, read) in [
            (
                json!({ "open_hour": 10, "close_hour": 22 }),
                Some((10, 22, None)),
            ),
            (
                json!({ "open_hour": 24, "close_hour": 2, "tz_offset_minutes": "x" }),
                Some((24, 2, None)),
            ),
            (json!({ "open_hour": 256, "close_hour": 2 }), None),
            (json!({ "open_hour": 10.0, "close_hour": 2 }), None),
            (json!({ "close_hour": 2 }), None),
            (json!([10, 22]), None),
        ] {
            let node: PublishedQr = serde_json::from_value(json!({ "business_hours": hours }))
                .expect("the node parses");
            assert_eq!(
                node.business_hours.map(|hours| (
                    hours.open_hour,
                    hours.close_hour,
                    hours.tz_offset_minutes
                )),
                read,
                "{hours}"
            );
        }
    }

    #[test]
    fn the_guardrails_are_read_whatever_the_setting_beside_them_holds() {
        let broken =
            json!({ "staff_confirmation_required": false, "enabled": true, "table_order": 7 });
        assert!(serde_json::from_value::<PublishedQr>(broken.clone()).is_err());
        let guardrails = PublishedQr::guardrails(&broken).expect("an object");
        assert_eq!(guardrails.staff_confirmation_required, Some(false));
        assert_eq!(guardrails.enabled, Some(true));
        assert_eq!(guardrails.table_order(), TableOrder::Separate);

        for not_an_object in [json!([true, false]), json!(true), json!(null)] {
            assert_eq!(
                PublishedQr::guardrails(&not_an_object),
                None,
                "{not_an_object}"
            );
        }
    }
}
