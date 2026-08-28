// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Channel and tender enablement policy (Track M7, [ADR-0080](../../../docs/adr/0080-channels-and-payments.md)).
//!
//! The wire `channels` and `tender` nodes ([`pos_proto::channels`]) list the sales channels and
//! payment methods a store has enabled. This module turns those wire lists into the domain sets the
//! edge enforces, and answers the one question each guards: *is this channel accepted here* and *is
//! this tender accepted here*.
//!
//! An [`Open`](pos_proto::wire_enum::Open) token that is unspecified or unrecognised is dropped rather
//! than enabled — a channel or method a newer cloud named that this build does not understand is not
//! silently switched on. The wire node keeps the raw token for a faithful round-trip; the domain set
//! carries only values this build can act on.

use std::collections::BTreeSet;

use pos_proto::channels::{PublishedChannels, PublishedTender};
use pos_proto::enums::{PaymentMethod, SalesChannel};

/// The set of sales channels a store accepts, from its published `channels` node.
///
/// Unspecified/unrecognised tokens are dropped. An empty set means the node enabled nothing this build
/// recognises; the caller decides what an *absent* node means (the edge treats absent as "no
/// restriction", present as authoritative — see the module docs in `pos_proto::channels`).
#[must_use]
pub fn enabled_channels(node: &PublishedChannels) -> BTreeSet<SalesChannel> {
    node.enabled()
        .iter()
        .filter(|channel| !channel.is_unspecified() && !channel.is_unrecognised())
        .map(pos_proto::wire_enum::Open::known)
        .collect()
}

/// The set of payment methods a store accepts, from its published `tender` node. Same drop-unknown
/// rule as [`enabled_channels`].
#[must_use]
pub fn accepted_tender(node: &PublishedTender) -> BTreeSet<PaymentMethod> {
    node.accepted()
        .iter()
        .filter(|method| !method.is_unspecified() && !method.is_unrecognised())
        .map(pos_proto::wire_enum::Open::known)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{accepted_tender, enabled_channels};
    use pos_proto::channels::{PublishedChannels, PublishedTender};
    use pos_proto::enums::{PaymentMethod, SalesChannel};
    use pos_proto::wire_enum::Open;

    #[test]
    fn enabled_channels_collects_the_known_tokens() {
        let node = PublishedChannels::new(vec![
            Open::from_known(SalesChannel::DineIn),
            Open::from_known(SalesChannel::Delivery),
        ]);
        let set = enabled_channels(&node);
        assert!(set.contains(&SalesChannel::DineIn));
        assert!(set.contains(&SalesChannel::Delivery));
        assert!(!set.contains(&SalesChannel::Qr));
    }

    #[test]
    fn an_unrecognised_channel_is_dropped_not_enabled() {
        // A channel a newer cloud named is tolerated on the wire but never silently switched on.
        let node = PublishedChannels::new(vec![
            Open::from_known(SalesChannel::Takeaway),
            Open::parse("SALES_CHANNEL_KIOSK"),
            Open::parse("SALES_CHANNEL_UNSPECIFIED"),
        ]);
        let set = enabled_channels(&node);
        assert_eq!(set.len(), 1, "only the one known, specified channel");
        assert!(set.contains(&SalesChannel::Takeaway));
    }

    #[test]
    fn accepted_tender_collects_the_known_methods() {
        let node = PublishedTender::new(vec![
            Open::from_known(PaymentMethod::Cash),
            Open::from_known(PaymentMethod::Card),
        ]);
        let set = accepted_tender(&node);
        assert!(set.contains(&PaymentMethod::Cash));
        assert!(set.contains(&PaymentMethod::Card));
        assert!(!set.contains(&PaymentMethod::Voucher));
    }
}
