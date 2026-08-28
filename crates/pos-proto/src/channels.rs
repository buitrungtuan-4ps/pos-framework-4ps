// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `channels` and `tender` config nodes ([ADR-0080](../../../docs/adr/0080-channels-and-payments.md), M7):
//! which sales channels a store accepts orders on, and which payment methods it takes.
//!
//! Both are small per-store settings documents in the same author-in-cloud → publish node → edge
//! applies shape as `tax`/`locale`. Each is a list of [`Open`]-wrapped enum tokens so a value a newer
//! cloud names round-trips rather than failing the whole node, and so the diff a publish shows is
//! line-stable.
//!
//! **Never-blank, opt-in semantics.** An *absent* node means "no restriction" — the store behaves
//! exactly as it did before M7 (a channel is implicitly enabled by the menu carrying it; any known
//! tender is accepted). A *present* node is authoritative: only the channels/methods it lists are
//! enabled. This keeps a store that has never published one trading unchanged, while letting an
//! operator turn a channel or a tender off explicitly. The domain sets are built in
//! `pos_core::channels`, the only place that maps these wire lists to a policy the edge enforces.

use serde::{Deserialize, Serialize};

use crate::enums::{PaymentMethod, SalesChannel};
use crate::wire_enum::Open;

/// The `channels` config node: the sales channels a store accepts orders on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedChannels {
    /// The enabled channels. An empty list published means the store accepts no channel; an *absent*
    /// node (never published) means no restriction — see the module docs.
    #[serde(default)]
    enabled: Vec<Open<SalesChannel>>,
}

impl PublishedChannels {
    /// A node enabling exactly `channels`.
    #[must_use]
    pub fn new(channels: Vec<Open<SalesChannel>>) -> Self {
        Self { enabled: channels }
    }

    /// The enabled channels, in the order the node lists them.
    #[must_use]
    pub fn enabled(&self) -> &[Open<SalesChannel>] {
        &self.enabled
    }

    /// Whether the node lists no channel at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.enabled.is_empty()
    }
}

/// The `tender` config node: the payment methods a store accepts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedTender {
    /// The accepted payment methods. An empty list published means the store takes no tender; an
    /// *absent* node means no restriction — any known method is accepted, as before M7.
    #[serde(default)]
    accepted: Vec<Open<PaymentMethod>>,
}

impl PublishedTender {
    /// A node accepting exactly `methods`.
    #[must_use]
    pub fn new(methods: Vec<Open<PaymentMethod>>) -> Self {
        Self { accepted: methods }
    }

    /// The accepted payment methods, in the order the node lists them.
    #[must_use]
    pub fn accepted(&self) -> &[Open<PaymentMethod>] {
        &self.accepted
    }

    /// Whether the node lists no method at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.accepted.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{PublishedChannels, PublishedTender};
    use crate::enums::{PaymentMethod, SalesChannel};
    use crate::wire_enum::Open;

    #[test]
    fn a_channels_node_round_trips_through_json() {
        let node = PublishedChannels::new(vec![
            Open::from_known(SalesChannel::DineIn),
            Open::from_known(SalesChannel::Takeaway),
        ]);
        let json = serde_json::to_string(&node).expect("serialize");
        let back: PublishedChannels = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(node, back);
        assert_eq!(back.enabled().len(), 2);
    }

    #[test]
    fn a_tender_node_round_trips_and_defaults_empty() {
        let node = PublishedTender::new(vec![Open::from_known(PaymentMethod::Cash)]);
        let json = serde_json::to_string(&node).expect("serialize");
        let back: PublishedTender = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(node, back);
        assert!(PublishedTender::default().is_empty());
        assert!(PublishedChannels::default().is_empty());
    }

    #[test]
    fn an_unknown_channel_token_survives_a_round_trip() {
        // The forward-compat property: a channel a newer cloud named round-trips byte-for-byte rather
        // than failing the node, exactly as `Open` guarantees.
        let json = r#"{"enabled":["SALES_CHANNEL_KIOSK"]}"#;
        let node: PublishedChannels = serde_json::from_str(json).expect("deserialize");
        assert_eq!(serde_json::to_string(&node).expect("serialize"), json);
        assert!(node.enabled().first().expect("one").is_unrecognised());
    }
}
