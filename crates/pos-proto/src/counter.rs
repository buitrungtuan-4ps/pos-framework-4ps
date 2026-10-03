// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `counter` config node: the channel an order the counter opens for a walk-in guest
//! takes ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 2, [ADR-0146](../../../docs/adr/0146-a-counter-store-starts-its-own-orders.md)).
//!
//! The channel decides the price book a walk-in is sold from and the tax it is charged. In Japan a
//! meal eaten in is taxed at 10% and one taken away at 8%, so a café whose guests eat at its
//! counter has to open their orders dine-in, or ask each guest. The counter opened every walk-in
//! for takeaway before the setting existed, and that is its default, so a document with no `counter`
//! node, or a node without the field, runs a store exactly as it ran before.
//!
//! A node of its own rather than a field on `channels`, whose `enabled` list is the channels a store
//! accepts ([`crate::channels::PublishedChannels`]). An edge from before the setting reads a
//! `channels` node without that list as a store that accepts no channel, and would refuse every
//! order; it ignores a node it does not know.

use serde::{Deserialize, Serialize};

use crate::enums::SalesChannel;
use crate::wire_enum;
use crate::wire_enum::{Open, is_absent};

wire_enum! {
    /// The channel an order the counter opens for a walk-in guest takes.
    WalkInChannel, prefix = "WALK_IN_CHANNEL";
    /// Every walk-in is taken away: what the counter did before the setting existed.
    Takeaway = "TAKEAWAY",
    /// Every walk-in is eaten in.
    DineIn = "DINE_IN",
    /// The cashier asks each guest, and the order opens eaten in or taken away as they answer.
    Ask = "ASK",
}

impl WalkInChannel {
    /// The channel a walk-in opens on when the till names none: dine-in for [`Self::DineIn`], and
    /// takeaway otherwise.
    ///
    /// Under [`Self::Ask`] the till names the guest's answer, so only a till from before the setting
    /// names none, and it gets the takeaway order it always opened. `WALK_IN_CHANNEL_UNSPECIFIED` is
    /// takeaway, the default.
    #[must_use]
    pub const fn sales_channel(self) -> SalesChannel {
        match self {
            Self::DineIn => SalesChannel::DineIn,
            Self::Unspecified | Self::Takeaway | Self::Ask => SalesChannel::Takeaway,
        }
    }
}

/// The `counter` node.
///
/// No `deny_unknown_fields`, as for every published node: an edge on an older release applies a
/// node that carries a field it does not know, rather than refusing the whole document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedCounter {
    /// The channel a walk-in takes. Read it through [`PublishedCounter::walk_in_channel`], which
    /// gives an absent or unknown value its default. A token this release does not know is kept and
    /// written back as it came; a value that is not a token at all, such as a number or `null`,
    /// reads as absent, so it costs the node none of its other fields. Left off the wire while
    /// absent.
    #[serde(default, deserialize_with = "token", skip_serializing_if = "is_absent")]
    pub walk_in_channel: Open<WalkInChannel>,
}

impl PublishedCounter {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "counter";

    /// The channel a walk-in takes.
    ///
    /// An absent value, `WALK_IN_CHANNEL_UNSPECIFIED`, and a value this release does not know all
    /// read as [`WalkInChannel::Takeaway`], the default and what every counter did before the
    /// setting existed. A value from a newer release is never offered to a store that cannot honour
    /// it (ADR-0160 decision 5), so the default is what such a store was running anyway.
    #[must_use]
    pub fn walk_in_channel(&self) -> WalkInChannel {
        match self.walk_in_channel.known() {
            WalkInChannel::Unspecified | WalkInChannel::Takeaway => WalkInChannel::Takeaway,
            WalkInChannel::DineIn => WalkInChannel::DineIn,
            WalkInChannel::Ask => WalkInChannel::Ask,
        }
    }
}

/// A walk-in channel as the node carries it: a token, kept as it came where this release does not
/// know it, and nothing for a value that is not a token, so the field reads as its default.
fn token<'de, D>(deserializer: D) -> Result<Open<WalkInChannel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?
        .as_str()
        .map_or_else(Open::default, Open::parse))
}

#[cfg(test)]
mod tests {
    use super::{PublishedCounter, WalkInChannel};
    use crate::enums::SalesChannel;
    use crate::wire_enum::WireEnum;

    fn node(text: &str) -> PublishedCounter {
        serde_json::from_str(text).expect("the node parses")
    }

    /// A node that sets nothing, and one carrying a value that is not a token, open every walk-in
    /// for takeaway, as the counter did before the setting.
    #[test]
    fn a_walk_in_is_taken_away_until_the_store_sets_otherwise() {
        for unset in [
            "{}",
            r#"{ "walk_in_channel": "WALK_IN_CHANNEL_UNSPECIFIED" }"#,
            r#"{ "walk_in_channel": "WALK_IN_CHANNEL_TAKEAWAY" }"#,
            r#"{ "walk_in_channel": 2 }"#,
            r#"{ "walk_in_channel": null }"#,
            r#"{ "walk_in_channel": ["WALK_IN_CHANNEL_DINE_IN"] }"#,
        ] {
            let read = node(unset);
            assert_eq!(read.walk_in_channel(), WalkInChannel::Takeaway, "{unset}");
            assert_eq!(
                read.walk_in_channel().sales_channel(),
                SalesChannel::Takeaway,
                "{unset}"
            );
        }
        assert_eq!(
            PublishedCounter::default().walk_in_channel(),
            WalkInChannel::Takeaway
        );
        assert_eq!(
            serde_json::to_string(&PublishedCounter::default()).expect("serialise"),
            "{}",
            "a node that sets nothing is written as nothing"
        );
    }

    /// A store whose walk-ins are eaten in opens them dine-in. One that asks opens what the till
    /// names, and takeaway for a till that names nothing.
    #[test]
    fn a_store_that_eats_in_opens_dine_in_and_one_that_asks_opens_takeaway_unless_told() {
        let dine_in = node(r#"{ "walk_in_channel": "WALK_IN_CHANNEL_DINE_IN" }"#);
        assert_eq!(dine_in.walk_in_channel(), WalkInChannel::DineIn);
        assert_eq!(
            dine_in.walk_in_channel().sales_channel(),
            SalesChannel::DineIn
        );
        let ask = node(r#"{ "walk_in_channel": "WALK_IN_CHANNEL_ASK" }"#);
        assert_eq!(ask.walk_in_channel(), WalkInChannel::Ask);
        assert_eq!(
            ask.walk_in_channel().sales_channel(),
            SalesChannel::Takeaway
        );
        assert_eq!(WalkInChannel::Ask.as_wire(), "WALK_IN_CHANNEL_ASK");
        assert_eq!(WalkInChannel::DineIn.as_wire(), "WALK_IN_CHANNEL_DINE_IN");
    }

    #[test]
    fn a_channel_from_a_newer_release_reads_as_takeaway_and_goes_back_out_as_it_came() {
        let newer = r#"{"walk_in_channel":"WALK_IN_CHANNEL_DRIVE_THROUGH"}"#;
        let read = node(newer);
        assert!(read.walk_in_channel.is_unrecognised());
        assert_eq!(read.walk_in_channel(), WalkInChannel::Takeaway);
        assert_eq!(serde_json::to_string(&read).expect("serialise"), newer);
    }
}
