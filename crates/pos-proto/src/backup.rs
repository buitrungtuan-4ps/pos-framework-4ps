// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `backup` config node: how often a store ships a sealed archive of its database
//! off the box ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md),
//! [ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 6).
//!
//! The interval was the box's own: `backup_interval_hours` in its `config.toml`. It is a setting
//! now, and its default is the file's, a day, so a document with no `backup` node, or a node
//! without the field, archives exactly as before. A node of its own, because no edge before the
//! setting reads one: an older release ignores it and keeps its file's interval.
//!
//! # No value switches archiving off
//!
//! The interval is an hour at the least, as no value switches the PIN lockout off
//! ([`crate::session`]): one mistake in the console must not stop the backups of a fleet. A box
//! whose operator has arranged something else, for a metered link, still switches archiving off in
//! its own file, and that wins.

use core::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

/// The hours between two archives a store may set, both bounds included. One at the least, because
/// no value switches archiving off. A week at the most, because the interval is the most trading a
/// store can lose if its disk dies.
pub const INTERVAL_HOURS: RangeInclusive<i64> = 1..=168;

/// The hours between two archives when nothing sets them: a day, the edge's interval before the
/// setting existed.
pub const DEFAULT_INTERVAL_HOURS: u32 = 24;

/// The `backup` node.
///
/// No `deny_unknown_fields`, as for every published node: an edge on an older release applies a node
/// that carries a field it does not know, rather than refusing the whole document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedBackup {
    /// The hours between two archives. Read it through [`PublishedBackup::interval_hours`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_hours: Option<i64>,
}

impl PublishedBackup {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "backup";

    /// The hours between two archives, or `None` when the node sets no value within
    /// [`INTERVAL_HOURS`].
    ///
    /// `None` rather than the default, as for the sign-in idle timeout
    /// ([`crate::session::PublishedSession::sign_in_idle_timeout_minutes`]), because a box may still
    /// carry an interval in its local file, deprecated by ADR-0160 decision 6. The edge falls back
    /// to that, and to [`DEFAULT_INTERVAL_HOURS`] when the file sets none.
    #[must_use]
    pub fn interval_hours(&self) -> Option<u32> {
        self.interval_hours
            .filter(|hours| INTERVAL_HOURS.contains(hours))
            .and_then(|hours| u32::try_from(hours).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::{INTERVAL_HOURS, PublishedBackup};

    fn node(text: &str) -> PublishedBackup {
        serde_json::from_str(text).expect("the node parses")
    }

    #[test]
    fn an_absent_node_or_field_sets_no_interval() {
        assert_eq!(node("{}").interval_hours(), None);
        assert_eq!(PublishedBackup::default(), node("{}"));
        assert_eq!(
            serde_json::to_string(&PublishedBackup::default()).expect("serialise"),
            "{}",
            "left off the wire until set"
        );
    }

    #[test]
    fn an_interval_within_its_bounds_is_read_and_one_outside_them_is_none() {
        for (published, read) in [
            (1, Some(1)),
            (6, Some(6)),
            (24, Some(24)),
            (168, Some(168)),
            (0, None),
            (169, None),
            (-6, None),
            (i64::MAX, None),
        ] {
            let set = node(&format!(r#"{{ "interval_hours": {published} }}"#));
            assert_eq!(set.interval_hours(), read, "{published}");
        }
    }

    #[test]
    fn no_value_switches_archiving_off() {
        assert!(*INTERVAL_HOURS.start() > 0);
    }
}
