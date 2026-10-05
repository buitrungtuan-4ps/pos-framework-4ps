// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `retention` config node: how many days a store's edge keeps an event it has synced
//! ([ADR-0145](../../../docs/adr/0145-the-edge-keeps-events-until-synced-and-n-days-old.md)).
//!
//! The cloud's `PUT /admin/config/retention` writes it, and the edge reads it. Both read it through
//! this type and its bounds, so the two cannot disagree about what a store may keep.
//!
//! Read field by field, as the edge always read it: a value outside [`EVENT_LOG_DAYS`], or one that
//! is not a whole number, reads as absent, and an absent value leaves the edge on the figure it had,
//! so a malformed publish never shortens a store's log.

use core::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

/// The days a store's edge may keep a synced event, both bounds included. Thirty at least, so a typo
/// cannot have a store forget last month; ten years at most, so a stray zero cannot keep a log
/// forever. The cloud refuses a publish outside it, and the edge ignores one that arrives anyway.
pub const EVENT_LOG_DAYS: RangeInclusive<u16> = 30..=3650;

/// The `retention` node.
///
/// No `deny_unknown_fields`, as for every published node: an edge on an older release applies a node
/// that carries a field it does not know, rather than refusing the whole document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedRetention {
    /// How many days the edge keeps a synced event, as the node carries it: a whole number, and
    /// `None` for anything else. Read it through [`PublishedRetention::event_log_days`].
    #[serde(
        default,
        deserialize_with = "whole_days",
        skip_serializing_if = "Option::is_none"
    )]
    pub event_log_days: Option<u64>,
}

impl PublishedRetention {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "retention";

    /// The node `value` carries, or `None` when it is not an object and so carries no field.
    ///
    /// The readers before this type asked an object for its field and found nothing in an array or
    /// a scalar. A typed parse alone would read an array positionally, so this is the reader for a
    /// node taken from a document.
    #[must_use]
    pub fn read(value: &serde_json::Value) -> Option<Self> {
        if !value.is_object() {
            return None;
        }
        serde_json::from_value(value.clone()).ok()
    }

    /// How many days the edge keeps a synced event: the node's number when it is within
    /// [`EVENT_LOG_DAYS`], and `None` otherwise, which leaves the edge on the figure it had.
    #[must_use]
    pub fn event_log_days(&self) -> Option<u16> {
        self.event_log_days
            .and_then(|days| u16::try_from(days).ok())
            .filter(|days| EVENT_LOG_DAYS.contains(days))
    }
}

/// A day count as the node carries it, read as the edge read it before this type: a non-negative
/// whole number, and `None` for anything else — a string, a fraction, a negative number, `null`.
fn whole_days<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?.as_u64())
}

#[cfg(test)]
mod tests {
    use super::{EVENT_LOG_DAYS, PublishedRetention};
    use serde_json::json;

    #[test]
    fn the_bounds_keep_a_month_at_least_and_ten_years_at_most() {
        assert_eq!(EVENT_LOG_DAYS, 30..=3650);
    }

    #[test]
    fn the_node_round_trips_and_an_absent_value_stays_off_the_wire() {
        let node = json!({ "event_log_days": 45 });
        let read = PublishedRetention::read(&node).expect("an object");
        assert_eq!(read.event_log_days(), Some(45));
        assert_eq!(serde_json::to_value(read).expect("serialise"), node);

        let empty = PublishedRetention::read(&json!({})).expect("an object");
        assert_eq!(empty, PublishedRetention::default());
        assert_eq!(empty.event_log_days(), None);
        assert_eq!(serde_json::to_value(empty).expect("serialise"), json!({}));
    }

    #[test]
    fn a_value_outside_the_bounds_or_not_a_whole_number_reads_as_absent() {
        for (published, read) in [
            (json!(30), Some(30)),
            (json!(3650), Some(3650)),
            (json!(29), None),
            (json!(3651), None),
            (json!(70_000), None),
            (json!(-1), None),
            (json!(45.0), None),
            (json!("45"), None),
            (json!(null), None),
        ] {
            let node = PublishedRetention::read(&json!({ "event_log_days": published }))
                .expect("a malformed field does not take the node down");
            assert_eq!(node.event_log_days(), read, "{published}");
        }
    }

    #[test]
    fn an_unknown_field_is_ignored_and_a_node_that_is_not_an_object_carries_nothing() {
        let node = PublishedRetention::read(&json!({ "event_log_days": 45, "later": true }))
            .expect("an object");
        assert_eq!(node.event_log_days(), Some(45));
        for not_an_object in [json!([45]), json!(45), json!("45"), json!(null)] {
            assert_eq!(
                PublishedRetention::read(&not_an_object),
                None,
                "{not_an_object}"
            );
        }
    }
}
