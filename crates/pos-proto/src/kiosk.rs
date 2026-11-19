// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `kiosk` node
//! ([ADR-0224](../../../docs/adr/0224-self-order-is-a-guest-page-and-a-kiosk.md), Configuration):
//! how long an untouched kiosk order waits before the kiosk voids it.
//!
//! A store's preference, so a setting (ADR-0160). What a kiosk may do, that a guest's details are
//! never asked for and that leaving the kiosk takes a PIN are fixed rules, and are not here.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

/// The values `abandon_seconds` may take.
pub const ABANDON_SECONDS: RangeInclusive<i64> = 30..=900;

/// What `abandon_seconds` is where the store sets none (ADR-0224 decision 5).
pub const DEFAULT_ABANDON_SECONDS: i64 = 120;

/// The `kiosk` node.
///
/// No `deny_unknown_fields`, as for every published node. A value of the wrong kind, or a number
/// outside [`ABANDON_SECONDS`], reads as absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct PublishedKiosk {
    /// How long an untouched kiosk order waits before it is voided, in seconds. Read it through
    /// [`Self::abandon_seconds`].
    #[serde(
        default,
        deserialize_with = "whole",
        skip_serializing_if = "Option::is_none"
    )]
    pub abandon_seconds: Option<i64>,
}

impl PublishedKiosk {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "kiosk";

    /// How long an untouched kiosk order waits: the node's number within [`ABANDON_SECONDS`], and
    /// [`DEFAULT_ABANDON_SECONDS`] otherwise.
    #[must_use]
    pub fn abandon_seconds(&self) -> u32 {
        self.abandon_seconds
            .filter(|seconds| ABANDON_SECONDS.contains(seconds))
            .and_then(|seconds| u32::try_from(seconds).ok())
            .unwrap_or(120)
    }
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
    fn an_untouched_kiosk_order_waits_two_minutes_unless_the_node_says_otherwise() {
        for (text, seconds) in [
            ("{}", 120),
            (r#"{ "abandon_seconds": 30 }"#, 30),
            (r#"{ "abandon_seconds": 900 }"#, 900),
            (r#"{ "abandon_seconds": 29 }"#, 120),
            (r#"{ "abandon_seconds": 901 }"#, 120),
            (r#"{ "abandon_seconds": "60" }"#, 120),
        ] {
            let node: PublishedKiosk = serde_json::from_str(text).unwrap_or_default();
            assert_eq!(node.abandon_seconds(), seconds, "{text}");
        }
        assert_eq!(
            i64::from(PublishedKiosk::default().abandon_seconds()),
            DEFAULT_ABANDON_SECONDS
        );
    }
}
